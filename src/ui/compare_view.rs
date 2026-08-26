//! Split-screen dual-stream compare TUI (`--compare URL1 URL2`).

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{backend::CrosstermBackend, Frame, Terminal};
use tokio::sync::mpsc::{self, Receiver};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::engine::export::{build_curl, build_har, ExportCapture};
use crate::engine::redact::redact_url;
use crate::engine::ManifestPoller;
use crate::models::{
    AbrVariant, CdnStats, HealthReport, LatencyState, PlaylistMeta, SegmentMetrics, StreamEvent,
    StreamStatus, VirtualBuffer, DIAGNOSTIC_DIR, EVENT_CHANNEL_CAPACITY,
};
use crate::ui::app::SessionOpts;

const FRAME_PERIOD: Duration = Duration::from_millis(33);
const TOAST_SECS: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    Left,
    Right,
}

#[derive(Debug)]
struct PaneState {
    label: String,
    url: String,
    status: StreamStatus,
    latency: LatencyState,
    playlist: Option<PlaylistMeta>,
    variants: Vec<AbrVariant>,
    last_segment: Option<SegmentMetrics>,
    health: HealthReport,
    cdn: CdnStats,
    buffer: VirtualBuffer,
    log_tail: Vec<String>,
    log_scroll: u16,
}

impl PaneState {
    fn new(label: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            url: url.into(),
            status: StreamStatus::live("Starting…"),
            latency: LatencyState::Unknown,
            playlist: None,
            variants: Vec::new(),
            last_segment: None,
            health: HealthReport::perfect(),
            cdn: CdnStats::default(),
            buffer: VirtualBuffer::default(),
            log_tail: Vec::new(),
            log_scroll: 0,
        }
    }

    fn apply(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Status(s) => self.status = s,
            StreamEvent::Variants(v) if !v.is_empty() => self.variants = v,
            StreamEvent::PlaylistMeta(m) => self.playlist = Some(m),
            StreamEvent::Segment(s) => self.last_segment = Some(s),
            StreamEvent::Latency(l) => self.latency = l,
            StreamEvent::Health(h) => self.health = h,
            StreamEvent::CdnStats(c) => self.cdn = c,
            StreamEvent::Buffer(b) => self.buffer = b,
            StreamEvent::Log { message, .. } => {
                self.log_tail.push(message);
                if self.log_tail.len() > 80 {
                    let n = self.log_tail.len() - 80;
                    self.log_tail.drain(0..n);
                }
            }
            StreamEvent::Error(m) => {
                self.log_tail.push(m);
            }
            _ => {}
        }
    }

    fn seq(&self) -> Option<u64> {
        self.last_segment
            .as_ref()
            .map(|s| s.media_sequence)
            .or_else(|| self.playlist.as_ref().map(|p| p.media_sequence))
    }

    fn latency_ms(&self) -> Option<u64> {
        match self.latency {
            LatencyState::Measured(ms) | LatencyState::Estimated(ms) => Some(ms),
            LatencyState::Unknown => self.last_segment.as_ref().and_then(|s| s.latency_ms),
        }
    }

    fn bitrate_kbps(&self) -> Option<u64> {
        self.last_segment
            .as_ref()
            .and_then(|s| s.download_kbps)
            .or_else(|| {
                self.variants
                    .iter()
                    .find(|v| v.selected)
                    .or_else(|| self.variants.first())
                    .map(|v| v.bandwidth / 1000)
            })
    }

    fn export_capture(&self, session: &SessionOpts) -> ExportCapture {
        ExportCapture {
            manifest_url: self
                .playlist
                .as_ref()
                .map(|p| p.url.clone())
                .unwrap_or_else(|| self.url.clone()),
            segment_url: self.last_segment.as_ref().map(|s| s.uri.clone()),
            probe_headers: session.probe_headers,
            headers: session.headers.clone(),
            user_agent: session.user_agent.clone(),
            last_http_status: self.last_segment.as_ref().map(|s| s.http_status),
            last_ttfb_ms: self.last_segment.as_ref().map(|s| s.ttfb_ms),
            last_size_bytes: self.last_segment.as_ref().map(|s| s.size_bytes),
        }
    }
}

pub struct CompareApp {
    left: PaneState,
    right: PaneState,
    left_rx: Receiver<StreamEvent>,
    right_rx: Receiver<StreamEvent>,
    left_poller: JoinHandle<()>,
    right_poller: JoinHandle<()>,
    should_quit: bool,
    paused: bool,
    show_detail: bool,
    log_focus: bool,
    focus: FocusPane,
    session: SessionOpts,
    toast: Option<(String, Instant)>,
}

impl CompareApp {
    pub async fn run(url1: String, url2: String, session: SessionOpts) -> Result<()> {
        let (l_tx, l_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (r_tx, r_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

        let left_poller = ManifestPoller::new(
            url1.clone(),
            session.headers.clone(),
            session.user_agent.clone(),
            session.interval_ms,
            session.probe_headers,
            session.probe_drm,
            l_tx,
        )
        .wrap_err("failed to start primary poller")?;
        let right_poller = ManifestPoller::new(
            url2.clone(),
            session.headers.clone(),
            session.user_agent.clone(),
            session.interval_ms,
            session.probe_headers,
            session.probe_drm,
            r_tx,
        )
        .wrap_err("failed to start backup poller")?;

        let mut app = Self {
            left: PaneState::new("Primary / Origin", url1),
            right: PaneState::new("Backup / CDN", url2),
            left_rx: l_rx,
            right_rx: r_rx,
            left_poller: tokio::spawn(async move { left_poller.run().await }),
            right_poller: tokio::spawn(async move { right_poller.run().await }),
            should_quit: false,
            paused: false,
            show_detail: false,
            log_focus: false,
            focus: FocusPane::Left,
            session,
            toast: None,
        };

        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let mut events = EventStream::new();
        let mut frames = interval(FRAME_PERIOD);
        frames.set_missed_tick_behavior(MissedTickBehavior::Skip);

        while !app.should_quit {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => app.should_quit = true,
                Some(ev) = app.left_rx.recv() => {
                    if !app.paused {
                        app.left.apply(ev);
                    }
                }
                Some(ev) = app.right_rx.recv() => {
                    if !app.paused {
                        app.right.apply(ev);
                    }
                }
                maybe = events.next() => {
                    match maybe {
                        Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                            app.handle_key(key.code, key.modifiers)?;
                        }
                        Some(Ok(Event::Resize(_, _))) => {
                            terminal.draw(|f| draw_compare(f, &app))?;
                        }
                        Some(Err(_)) | None => app.should_quit = true,
                        _ => {}
                    }
                }
                _ = frames.tick() => {
                    if let Some((_, until)) = app.toast {
                        if Instant::now() >= until {
                            app.toast = None;
                        }
                    }
                    terminal.draw(|f| draw_compare(f, &app))?;
                }
            }
        }

        app.left_poller.abort();
        app.right_poller.abort();
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        Ok(())
    }

    fn focused_mut(&mut self) -> &mut PaneState {
        match self.focus {
            FocusPane::Left => &mut self.left,
            FocusPane::Right => &mut self.right,
        }
    }

    fn focused(&self) -> &PaneState {
        match self.focus {
            FocusPane::Left => &self.left,
            FocusPane::Right => &self.right,
        }
    }

    fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Result<()> {
        if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            self.should_quit = true;
            return Ok(());
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char(' ') => {
                self.paused = !self.paused;
                self.toast = Some((
                    if self.paused {
                        "paused".into()
                    } else {
                        "resumed".into()
                    },
                    Instant::now() + Duration::from_secs(TOAST_SECS),
                ));
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.show_detail = !self.show_detail;
            }
            KeyCode::Char('l') | KeyCode::Char('L') => {
                self.log_focus = !self.log_focus;
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    FocusPane::Left => FocusPane::Right,
                    FocusPane::Right => FocusPane::Left,
                };
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                let cmd = build_curl(&self.focused().export_capture(&self.session));
                match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(cmd.clone())) {
                    Ok(()) => {
                        self.toast = Some((
                            "curl copied (redacted)".into(),
                            Instant::now() + Duration::from_secs(TOAST_SECS),
                        ));
                    }
                    Err(_) => {
                        self.focused_mut().log_tail.push(format!("curl: {cmd}"));
                        self.toast = Some((
                            "clipboard failed — see log".into(),
                            Instant::now() + Duration::from_secs(TOAST_SECS),
                        ));
                    }
                }
            }
            KeyCode::Char('h') | KeyCode::Char('H') | KeyCode::Char('e') | KeyCode::Char('E') => {
                self.export_har()?;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let pane = self.focused_mut();
                pane.log_scroll = pane.log_scroll.saturating_add(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let pane = self.focused_mut();
                pane.log_scroll = pane.log_scroll.saturating_sub(1);
            }
            KeyCode::Char('?') => {
                self.toast = Some((
                    "Space pause · d detail · l log · c curl · h HAR · Tab focus · q quit".into(),
                    Instant::now() + Duration::from_secs(5),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn export_har(&mut self) -> Result<()> {
        fs::create_dir_all(DIAGNOSTIC_DIR)?;
        let side = match self.focus {
            FocusPane::Left => "A",
            FocusPane::Right => "B",
        };
        let stamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let path = PathBuf::from(DIAGNOSTIC_DIR).join(format!("compare_{side}_{stamp}.har"));
        let cap = self.focused().export_capture(&self.session);
        let doc = build_har(&cap);
        fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
        self.toast = Some((
            format!("HAR {}", path.display()),
            Instant::now() + Duration::from_secs(TOAST_SECS),
        ));
        Ok(())
    }
}

fn draw_compare(frame: &mut Frame, app: &CompareApp) {
    let area = frame.area();
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    draw_pane(
        frame,
        &app.left,
        cols[0],
        Color::LightGreen,
        app.focus == FocusPane::Left,
        app.show_detail,
        app.log_focus,
        app.paused,
    );
    draw_pane(
        frame,
        &app.right,
        cols[1],
        Color::LightCyan,
        app.focus == FocusPane::Right,
        app.show_detail,
        app.log_focus,
        app.paused,
    );

    let delta = diff_line(&app.left, &app.right, app.paused);
    let footer = Rect {
        x: area.x,
        y: area.y.saturating_add(area.height.saturating_sub(1)),
        width: area.width,
        height: 1,
    };
    let footer_text = if let Some((msg, _)) = &app.toast {
        format!(" {delta}  |  {msg} ")
    } else {
        format!(" {delta}  |  Space pause  d detail  l log  c curl  h HAR  Tab focus  q quit ")
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            footer_text,
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightYellow)
                .add_modifier(Modifier::BOLD),
        ))),
        footer,
    );
}

fn diff_line(left: &PaneState, right: &PaneState, paused: bool) -> String {
    let seq = match (left.seq(), right.seq()) {
        (Some(a), Some(b)) => {
            let d = b as i64 - a as i64;
            format!("Δ Seq: {d:+}")
        }
        _ => "Δ Seq: —".into(),
    };
    let lat = match (left.latency_ms(), right.latency_ms()) {
        (Some(a), Some(b)) => {
            let d = (b as f64 - a as f64) / 1000.0;
            format!("Δ Latency: {d:+.1}s")
        }
        _ => "Δ Latency: —".into(),
    };
    let br = match (left.bitrate_kbps(), right.bitrate_kbps()) {
        (Some(a), Some(b)) => {
            let d = b as i64 - a as i64;
            format!("Δ Bitrate: {d:+} kbps ({a} vs {b})")
        }
        _ => "Δ Bitrate: —".into(),
    };
    let cache = format!(
        "Cache: {} vs {}",
        left.last_segment
            .as_ref()
            .map(|s| s.cdn.badge())
            .unwrap_or_else(|| "—".into()),
        right
            .last_segment
            .as_ref()
            .map(|s| s.cdn.badge())
            .unwrap_or_else(|| "—".into())
    );
    let pause = if paused { " PAUSED" } else { "" };
    format!("{seq}  |  {lat}  |  {br}  |  {cache}{pause}")
}

#[allow(clippy::too_many_arguments)]
fn draw_pane(
    frame: &mut Frame,
    pane: &PaneState,
    area: Rect,
    accent: Color,
    focused: bool,
    show_detail: bool,
    log_focus: bool,
    paused: bool,
) {
    let title = format!(
        " {}{}{} ",
        pane.label,
        if focused { " ●" } else { "" },
        if paused { " ⏸" } else { "" }
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(if focused { Color::Yellow } else { accent }));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (top_h, mid_h) = if log_focus {
        (3u16, 2u16)
    } else if show_detail {
        (9, 4)
    } else {
        (7, 4)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top_h),
            Constraint::Length(mid_h),
            Constraint::Min(3),
        ])
        .split(inner);

    let seq = pane
        .seq()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "—".into());
    let lat = pane.latency.display();
    let shi = format!("{} ({})", pane.health.score, pane.health.label);
    let cdn = pane
        .last_segment
        .as_ref()
        .map(|s| s.cdn.badge())
        .unwrap_or_else(|| {
            pane.cdn
                .hit_ratio_pct()
                .map(|p| format!("hit {p:.0}%"))
                .unwrap_or_else(|| "—".into())
        });
    let net = pane
        .last_segment
        .as_ref()
        .and_then(|s| s.network.as_ref())
        .map(|n| n.display_line())
        .unwrap_or_else(|| "DNS/TCP/TLS/TTFB: —".into());
    let br = pane
        .bitrate_kbps()
        .map(|b| format!("{b} kbps"))
        .unwrap_or_else(|| "—".into());

    let mut status_lines = vec![
        Line::from(truncate_url(
            &redact_url(&pane.url),
            (area.width as usize).saturating_sub(4),
        )),
        Line::from(format!(
            "Status : {} — {}",
            status_tag(&pane.status),
            pane.status.message
        )),
        Line::from(format!("SHI    : {shi}")),
        Line::from(format!(
            "Seq    : {seq}  |  Latency: {lat}  |  Bitrate: {br}"
        )),
        Line::from(format!("CDN    : {cdn}")),
        Line::from(format!("Buffer : {}", pane.buffer.display())),
    ];
    if show_detail {
        status_lines.push(Line::from(Span::styled(
            net,
            Style::default().fg(Color::Cyan),
        )));
        if let Some(seg) = &pane.last_segment {
            status_lines.push(Line::from(format!(
                "Seg    : TTFB {}ms HTTP {} {}",
                seg.ttfb_ms,
                seg.http_status,
                truncate_url(&redact_url(&seg.uri), 40)
            )));
        }
    } else if !log_focus {
        status_lines.push(Line::from(Span::styled(
            net,
            Style::default().fg(Color::Cyan),
        )));
    }
    frame.render_widget(Paragraph::new(status_lines), chunks[0]);

    let abr = if pane.variants.is_empty() {
        "Single / no ABR ladder".into()
    } else {
        pane.variants
            .iter()
            .take(3)
            .map(|v| {
                format!(
                    "{} {} {}",
                    if v.selected { "●" } else { "○" },
                    v.resolution_label(),
                    v.fps_label()
                )
            })
            .collect::<Vec<_>>()
            .join("  |  ")
    };
    frame.render_widget(
        Paragraph::new(abr).block(Block::default().title(" ABR ")),
        chunks[1],
    );

    let visible = chunks[2].height as usize;
    let scroll = pane.log_scroll as usize;
    let logs: Vec<Line> = pane
        .log_tail
        .iter()
        .rev()
        .skip(scroll)
        .take(visible)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|m| Line::from(truncate_url(m, (area.width as usize).saturating_sub(4))))
        .collect();
    frame.render_widget(
        Paragraph::new(logs).block(Block::default().title(" Log (j/k) ")),
        chunks[2],
    );
}

fn status_tag(s: &StreamStatus) -> &'static str {
    use crate::models::StreamStatusKind;
    match s.kind {
        StreamStatusKind::Live => "LIVE",
        StreamStatusKind::Error => "ERROR",
        StreamStatusKind::Degraded => "DEGRADED",
    }
}

fn truncate_url(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let t: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{t}…")
}

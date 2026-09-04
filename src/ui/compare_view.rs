//! Split-screen dual-stream compare TUI (`--compare URL1 URL2`).

use std::collections::VecDeque;
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
const PAUSE_RING_CAP: usize = 256;

/// Bounded pause buffer: keep last N events while UI is paused, replay on resume.
#[derive(Debug, Default)]
pub struct PauseRingBuffer {
    buf: VecDeque<StreamEvent>,
    cap: usize,
}

impl PauseRingBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap),
            cap: cap.max(1),
        }
    }

    pub fn push(&mut self, event: StreamEvent) {
        if self.buf.len() >= self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(event);
    }

    pub fn drain(&mut self) -> Vec<StreamEvent> {
        self.buf.drain(..).collect()
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

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
    ad_active: bool,
    ad_mismatch_total: u32,
    finding_count: u32,
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
            ad_active: false,
            ad_mismatch_total: 0,
            finding_count: 0,
        }
    }

    fn push_log(&mut self, line: String) {
        self.log_tail.push(line);
        if self.log_tail.len() > 80 {
            let n = self.log_tail.len() - 80;
            self.log_tail.drain(0..n);
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
                self.push_log(message);
            }
            StreamEvent::AdBreak(ad) => {
                self.ad_active = ad.active && !ad.kind.contains("CUE-IN");
                self.push_log(format!("[AD] {}", ad.summary));
            }
            StreamEvent::AdMarkerMismatch(m) => {
                self.ad_mismatch_total = self.ad_mismatch_total.saturating_add(1);
                self.push_log(format!("[MISMATCH] {}: {}", m.rule, m.message));
            }
            StreamEvent::Finding(f) => {
                self.finding_count = self.finding_count.saturating_add(1);
                self.push_log(format!("[{}] {}", f.category.tag(), f.message));
            }
            StreamEvent::Error(m) => {
                self.push_log(m);
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
                .map_or_else(|| self.url.clone(), |p| p.url.clone()),
            segment_url: self.last_segment.as_ref().map(|s| s.uri.clone()),
            probe_headers: session.probe_headers,
            headers: session.headers.clone(),
            user_agent: session.user_agent.clone(),
            last_http_status: self.last_segment.as_ref().map(|s| s.http_status),
            last_ttfb_ms: self.last_segment.as_ref().map(|s| s.ttfb_ms),
            last_size_bytes: self.last_segment.as_ref().map(|s| s.size_bytes),
            ..Default::default()
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
    left_pause_buf: PauseRingBuffer,
    right_pause_buf: PauseRingBuffer,
    show_detail: bool,
    log_focus: bool,
    focus: FocusPane,
    session: SessionOpts,
    toast: Option<(String, Instant)>,
    log_filter: Option<String>,
}

impl CompareApp {
    pub async fn run(url1: String, url2: String, session: SessionOpts) -> Result<()> {
        let (l_tx, l_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (r_tx, r_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

        let diag = crate::engine::poller::DiagnosticOpts {
            tr101290: session.tr101290,
            probe_sei: session.probe_sei,
        };
        let left_poller = crate::engine::session_poller::apply_session_doh(
            ManifestPoller::new(
                url1.as_str(),
                &session.headers,
                session.user_agent.as_deref(),
                session.interval_ms,
                session.probe_headers,
                session.probe_drm,
                l_tx,
            )
            .wrap_err("failed to start primary poller")?
            .with_diagnostics(&diag),
            &session,
        )?;
        let right_poller = crate::engine::session_poller::apply_session_doh(
            ManifestPoller::new(
                url2.as_str(),
                &session.headers,
                session.user_agent.as_deref(),
                session.interval_ms,
                session.probe_headers,
                session.probe_drm,
                r_tx,
            )
            .wrap_err("failed to start backup poller")?
            .with_diagnostics(&diag),
            &session,
        )?;

        let mut app = Self {
            left: PaneState::new("Primary / Origin", url1),
            right: PaneState::new("Backup / CDN", url2),
            left_rx: l_rx,
            right_rx: r_rx,
            left_poller: tokio::spawn(async move { left_poller.run().await }),
            right_poller: tokio::spawn(async move { right_poller.run().await }),
            should_quit: false,
            paused: false,
            left_pause_buf: PauseRingBuffer::new(PAUSE_RING_CAP),
            right_pause_buf: PauseRingBuffer::new(PAUSE_RING_CAP),
            show_detail: false,
            log_focus: false,
            focus: FocusPane::Left,
            session,
            toast: None,
            log_filter: None,
        };

        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        crate::ui::app::mark_terminal_active();
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let mut events = EventStream::new();
        let mut frames = interval(FRAME_PERIOD);
        frames.set_missed_tick_behavior(MissedTickBehavior::Skip);

        while !app.should_quit {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => app.should_quit = true,
                Some(ev) = app.left_rx.recv() => {
                    if app.paused {
                        app.left_pause_buf.push(ev);
                    } else {
                        app.left.apply(ev);
                    }
                }
                Some(ev) = app.right_rx.recv() => {
                    if app.paused {
                        app.right_pause_buf.push(ev);
                    } else {
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
                if self.paused {
                    // Resume: replay buffered events so UI catches up to last state.
                    for ev in self.left_pause_buf.drain() {
                        self.left.apply(ev);
                    }
                    for ev in self.right_pause_buf.drain() {
                        self.right.apply(ev);
                    }
                    self.paused = false;
                    self.toast = Some((
                        "resumed".into(),
                        Instant::now() + Duration::from_secs(TOAST_SECS),
                    ));
                } else {
                    self.paused = true;
                    self.toast = Some((
                        "paused (buffering)".into(),
                        Instant::now() + Duration::from_secs(TOAST_SECS),
                    ));
                }
            }
            KeyCode::Char('d' | 'D') => {
                self.show_detail = !self.show_detail;
            }
            KeyCode::Char('l' | 'L') => {
                self.log_focus = !self.log_focus;
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    FocusPane::Left => FocusPane::Right,
                    FocusPane::Right => FocusPane::Left,
                };
            }
            KeyCode::Char('c' | 'C') => {
                let cmd = build_curl(&self.focused().export_capture(&self.session));
                if matches!(
                    arboard::Clipboard::new().and_then(|mut cb| cb.set_text(cmd.clone())),
                    Ok(())
                ) {
                    self.toast = Some((
                        "curl copied (redacted)".into(),
                        Instant::now() + Duration::from_secs(TOAST_SECS),
                    ));
                } else {
                    self.focused_mut().log_tail.push(format!("curl: {cmd}"));
                    self.toast = Some((
                        "clipboard failed - see log".into(),
                        Instant::now() + Duration::from_secs(TOAST_SECS),
                    ));
                }
            }
            KeyCode::Char('h' | 'H' | 'e' | 'E') => {
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
                    "Space pause · d detail · l log · f filter · c curl · h HAR · Tab focus · q quit".into(),
                    Instant::now() + Duration::from_secs(5),
                ));
            }
            KeyCode::Char('f') => {
                self.log_filter = match self.log_filter.as_deref() {
                    None => Some("404".into()),
                    Some("404") => Some("SCTE".into()),
                    Some("SCTE") => Some("MISMATCH".into()),
                    Some("MISMATCH") => Some("DRIFT".into()),
                    _ => None,
                };
            }
            KeyCode::Char('F') => self.log_filter = None,
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
        app.log_filter.as_deref(),
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
        app.log_filter.as_deref(),
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
    if let (Some(l), Some(r)) = (&left.last_segment, &right.last_segment) {
        let drift = crate::engine::cdn_telemetry::compare_cdn_drift(l, r);
        let mut base =
            crate::engine::cdn_telemetry::format_compare_drift(&left.cdn, &right.cdn, &drift);
        if paused {
            base.push_str(" PAUSED");
        }
        return base;
    }
    let seq = match (left.seq(), right.seq()) {
        (Some(a), Some(b)) => {
            let d = b as i64 - a as i64;
            format!("Δ Seq: {d:+}")
        }
        _ => "Δ Seq: -".into(),
    };
    let lat = match (left.latency_ms(), right.latency_ms()) {
        (Some(a), Some(b)) => {
            let d = (b as f64 - a as f64) / 1000.0;
            format!("Δ Latency: {d:+.1}s")
        }
        _ => "Δ Latency: -".into(),
    };
    let br = match (left.bitrate_kbps(), right.bitrate_kbps()) {
        (Some(a), Some(b)) => {
            let d = b as i64 - a as i64;
            format!("Δ Bitrate: {d:+} kbps ({a} vs {b})")
        }
        _ => "Δ Bitrate: -".into(),
    };
    let cache = format!(
        "Cache: {} vs {}",
        left.last_segment
            .as_ref()
            .map_or_else(|| "-".into(), |s| s.cdn.badge()),
        right
            .last_segment
            .as_ref()
            .map_or_else(|| "-".into(), |s| s.cdn.badge())
    );
    let pause = if paused { " PAUSED" } else { "" };
    let ad = if left.ad_mismatch_total == right.ad_mismatch_total {
        String::new()
    } else {
        format!(
            "  |  Δ AdMismatch: {}",
            right.ad_mismatch_total as i64 - left.ad_mismatch_total as i64
        )
    };
    format!("{seq}  |  {lat}  |  {br}  |  {cache}{ad}{pause}")
}

#[allow(clippy::fn_params_excessive_bools)]
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
    log_filter: Option<&str>,
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

    let seq = pane.seq().map_or_else(|| "-".into(), |s| s.to_string());
    let lat = pane.latency.display();
    let shi = format!("{} ({})", pane.health.score, pane.health.label);
    let cdn = pane.last_segment.as_ref().map_or_else(
        || {
            pane.cdn
                .hit_ratio_pct()
                .map_or_else(|| "-".into(), |p| format!("hit {p:.0}%"))
        },
        |s| {
            let b = s.cdn.badge();
            let d = s.cdn.edge_detail();
            if d.is_empty() {
                b
            } else {
                format!("{b} {d}")
            }
        },
    );
    let net = pane
        .last_segment
        .as_ref()
        .and_then(|s| s.network.as_ref())
        .map_or_else(
            || "DNS/TCP/TLS/TTFB: -".into(),
            super::super::models::stream::NetworkTiming::display_line,
        );
    let br = pane
        .bitrate_kbps()
        .map_or_else(|| "-".into(), |b| format!("{b} kbps"));

    let mut status_lines = vec![
        Line::from(truncate_url(
            &redact_url(&pane.url),
            (area.width as usize).saturating_sub(4),
        )),
        Line::from(format!(
            "Status : {} - {}",
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
    let filtered: Vec<&String> = log_filter.map_or_else(
        || pane.log_tail.iter().collect(),
        |pat| {
            let needle = pat.to_ascii_lowercase();
            pane.log_tail
                .iter()
                .filter(|m| m.to_ascii_lowercase().contains(&needle))
                .collect()
        },
    );
    let log_title = log_filter.map_or_else(
        || " Log (j/k) ".into(),
        |pat| format!(" Log [filter: {pat}] (j/k) "),
    );
    let logs: Vec<Line> = filtered
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
        Paragraph::new(logs).block(Block::default().title(log_title)),
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
    format!("{t}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{HealthReport, StreamStatus};

    #[test]
    fn pause_ring_keeps_last_n_and_drains() {
        let mut ring = PauseRingBuffer::new(3);
        ring.push(StreamEvent::Status(StreamStatus::live("a")));
        ring.push(StreamEvent::Status(StreamStatus::live("b")));
        ring.push(StreamEvent::Status(StreamStatus::live("c")));
        ring.push(StreamEvent::Status(StreamStatus::live("d")));
        assert_eq!(ring.len(), 3);
        let drained = ring.drain();
        assert_eq!(drained.len(), 3);
        assert!(ring.is_empty());
        match &drained[2] {
            StreamEvent::Status(s) => assert!(s.message.contains('d')),
            _ => panic!("expected status"),
        }
    }

    #[test]
    fn pane_applies_health_after_buffer_replay() {
        let mut pane = PaneState::new("t", "https://ex/m.m3u8");
        let mut ring = PauseRingBuffer::new(256);
        ring.push(StreamEvent::Health(HealthReport {
            score: 42,
            label: "degraded".into(),
            deductions: vec![],
        }));
        for ev in ring.drain() {
            pane.apply(ev);
        }
        assert_eq!(pane.health.score, 42);
    }
}

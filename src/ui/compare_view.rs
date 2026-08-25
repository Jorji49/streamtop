//! Split-screen dual-stream compare TUI (`--compare URL1 URL2`).

use std::io;
use std::time::Duration;

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
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::engine::ManifestPoller;
use crate::models::{
    AbrVariant, CdnStats, HealthReport, LatencyState, PlaylistMeta, SegmentMetrics, StreamEvent,
    StreamStatus, VirtualBuffer,
};
use crate::ui::app::SessionOpts;

const FRAME_PERIOD: Duration = Duration::from_millis(33);

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
                if self.log_tail.len() > 40 {
                    let n = self.log_tail.len() - 40;
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
}

pub struct CompareApp {
    left: PaneState,
    right: PaneState,
    left_rx: UnboundedReceiver<StreamEvent>,
    right_rx: UnboundedReceiver<StreamEvent>,
    left_poller: JoinHandle<()>,
    right_poller: JoinHandle<()>,
    should_quit: bool,
}

impl CompareApp {
    pub async fn run(url1: String, url2: String, session: SessionOpts) -> Result<()> {
        let (l_tx, l_rx) = mpsc::unbounded_channel();
        let (r_tx, r_rx) = mpsc::unbounded_channel();

        let left_poller = ManifestPoller::new(
            url1.clone(),
            session.headers.clone(),
            session.user_agent.clone(),
            session.interval_ms,
            session.probe_headers,
            l_tx,
        )
        .wrap_err("failed to start primary poller")?;
        let right_poller = ManifestPoller::new(
            url2.clone(),
            session.headers.clone(),
            session.user_agent.clone(),
            session.interval_ms,
            session.probe_headers,
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
                Some(ev) = app.left_rx.recv() => app.left.apply(ev),
                Some(ev) = app.right_rx.recv() => app.right.apply(ev),
                maybe = events.next() => {
                    if let Some(Ok(Event::Key(key))) = maybe {
                        if key.kind == KeyEventKind::Press {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    app.should_quit = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ = frames.tick() => {
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
}

fn draw_compare(frame: &mut Frame, app: &CompareApp) {
    let area = frame.area();
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    draw_pane(frame, &app.left, cols[0], Color::LightGreen);
    draw_pane(frame, &app.right, cols[1], Color::LightCyan);

    let delta = diff_line(&app.left, &app.right);
    let footer = Rect {
        x: area.x,
        y: area.y.saturating_add(area.height.saturating_sub(1)),
        width: area.width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {delta}  |  q/Esc quit "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightYellow)
                .add_modifier(Modifier::BOLD),
        ))),
        footer,
    );
}

fn diff_line(left: &PaneState, right: &PaneState) -> String {
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
    format!("{seq}  |  {lat}  |  {cache}")
}

fn draw_pane(frame: &mut Frame, pane: &PaneState, area: Rect, accent: Color) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", pane.label))
        .border_style(Style::default().fg(accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(4),
            Constraint::Min(3),
        ])
        .split(inner);

    let seq = pane.seq().map(|s| s.to_string()).unwrap_or_else(|| "—".into());
    let lat = pane.latency.display();
    let shi = format!("{} ({})", pane.health.score, pane.health.label);
    let cdn = pane
        .last_segment
        .as_ref()
        .map(|s| s.cdn.badge())
        .unwrap_or_else(|| pane.cdn.hit_ratio_pct().map(|p| format!("hit {p:.0}%")).unwrap_or_else(|| "—".into()));
    let net = pane
        .last_segment
        .as_ref()
        .and_then(|s| s.network.as_ref())
        .map(|n| n.display_line())
        .unwrap_or_else(|| "DNS/TCP/TLS/TTFB: —".into());

    let status = Paragraph::new(vec![
        Line::from(truncate_url(&pane.url, (area.width as usize).saturating_sub(4))),
        Line::from(format!("Status : {} — {}", status_tag(&pane.status), pane.status.message)),
        Line::from(format!("SHI    : {shi}")),
        Line::from(format!("Seq    : {seq}  |  Latency: {lat}")),
        Line::from(format!("CDN    : {cdn}")),
        Line::from(format!("Buffer : {}", pane.buffer.display())),
        Line::from(Span::styled(net, Style::default().fg(Color::Cyan))),
    ]);
    frame.render_widget(status, chunks[0]);

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

    let logs: Vec<Line> = pane
        .log_tail
        .iter()
        .rev()
        .take(chunks[2].height as usize)
        .rev()
        .map(|m| Line::from(truncate_url(m, (area.width as usize).saturating_sub(4))))
        .collect();
    frame.render_widget(
        Paragraph::new(logs).block(Block::default().title(" Log ")),
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

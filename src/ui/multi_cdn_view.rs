//! Multi-CDN skew matrix TUI (`--multi-cdn URL1,URL2,...`).

use std::collections::HashMap;
use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, Table};
use ratatui::{backend::CrosstermBackend, Frame, Terminal};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::engine::multi_cdn::{compute_skew_from_snapshots, MultiCdnTarget};
use crate::engine::session_poller::apply_session_doh;
use crate::engine::ManifestPoller;
use crate::models::{
    DiagnosticReasonCode, MultiCdnEdgeSnapshot, MultiCdnSkewReport, StreamEvent,
    EVENT_CHANNEL_CAPACITY,
};
use crate::ui::app::{restore_terminal_global, SessionOpts};

const FRAME_MS: Duration = Duration::from_millis(33);

#[derive(Debug, Default)]
struct EdgeLive {
    media_sequence: Option<u64>,
    pdt_offset_ms: Option<i64>,
    ttfb_ms: Option<u64>,
    cdn_hits: u64,
    cdn_misses: u64,
}

pub struct MultiCdnApp {
    targets: Vec<MultiCdnTarget>,
    session: SessionOpts,
    max_skew_ms: i64,
    edges: HashMap<String, EdgeLive>,
    report: MultiCdnSkewReport,
    should_quit: bool,
    handles: Vec<JoinHandle<()>>,
    receivers: Vec<(String, mpsc::Receiver<StreamEvent>)>,
}

impl MultiCdnApp {
    pub fn new(targets: Vec<MultiCdnTarget>, session: SessionOpts, max_skew_ms: i64) -> Self {
        let mut edges = HashMap::new();
        for t in &targets {
            edges.insert(t.label.clone(), EdgeLive::default());
        }
        Self {
            targets,
            session,
            max_skew_ms,
            edges,
            report: MultiCdnSkewReport::default(),
            should_quit: false,
            handles: Vec::new(),
            receivers: Vec::new(),
        }
    }

    pub async fn run(mut self) -> Result<()> {
        for target in &self.targets {
            let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
            let mut poller = ManifestPoller::new(
                &target.url,
                &self.session.headers,
                self.session.user_agent.as_deref(),
                self.session.interval_ms,
                self.session.probe_headers,
                self.session.probe_drm,
                tx,
            )?;
            poller = apply_session_doh(poller, &self.session)?;
            let handle = tokio::spawn(async move {
                poller.run().await;
            });
            self.handles.push(handle);
            self.receivers.push((target.label.clone(), rx));
        }

        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        let mut events = EventStream::new();
        let mut frames = interval(FRAME_MS);
        frames.set_missed_tick_behavior(MissedTickBehavior::Skip);

        while !self.should_quit {
            self.drain_events();
            self.refresh_report();
            terminal.draw(|f| draw_matrix(f, &self))?;
            tokio::select! {
                _ = tokio::signal::ctrl_c() => self.should_quit = true,
                maybe = events.next() => {
                    if let Some(Ok(Event::Key(k))) = maybe {
                        if k.kind == KeyEventKind::Press && matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                            self.should_quit = true;
                        }
                    }
                }
                _ = frames.tick() => {}
            }
        }

        for h in self.handles.drain(..) {
            h.abort();
        }
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        restore_terminal_global();
        Ok(())
    }

    fn drain_events(&mut self) {
        for (label, rx) in &mut self.receivers {
            while let Ok(ev) = rx.try_recv() {
                if let Some(edge) = self.edges.get_mut(label) {
                    match ev {
                        StreamEvent::PlaylistMeta(m) => {
                            edge.media_sequence = Some(m.media_sequence);
                        }
                        StreamEvent::Segment(s) => {
                            edge.media_sequence = Some(s.media_sequence);
                            edge.ttfb_ms = Some(s.ttfb_ms);
                        }
                        StreamEvent::CdnStats(c) => {
                            edge.cdn_hits = c.hits;
                            edge.cdn_misses = c.misses;
                        }
                        StreamEvent::Latency(crate::models::LatencyState::Measured(ms)) => {
                            edge.pdt_offset_ms = Some(ms as i64);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn refresh_report(&mut self) {
        let snapshots: Vec<MultiCdnEdgeSnapshot> = self
            .targets
            .iter()
            .map(|t| {
                let e = self.edges.get(&t.label);
                MultiCdnEdgeSnapshot {
                    label: t.label.clone(),
                    url: t.url.clone(),
                    media_sequence: e.and_then(|x| x.media_sequence),
                    pdt_offset_ms: e.and_then(|x| x.pdt_offset_ms),
                    segment_delay_ms: e.and_then(|x| x.ttfb_ms),
                    cdn_hits: e.map_or(0, |x| x.cdn_hits),
                    cdn_misses: e.map_or(0, |x| x.cdn_misses),
                    ttfb_ms: e.and_then(|x| x.ttfb_ms),
                    http_version: None,
                }
            })
            .collect();
        self.report = compute_skew_from_snapshots(&snapshots);
    }

    pub fn skew_exceeds_threshold(&self) -> bool {
        self.report.max_skew_ms > self.max_skew_ms
    }

    pub fn reason_code(&self) -> Option<&'static str> {
        if self.skew_exceeds_threshold() {
            Some(DiagnosticReasonCode::ErrCdnSyncSkew.as_str())
        } else {
            None
        }
    }
}

fn draw_matrix(frame: &mut Frame, app: &MultiCdnApp) {
    let area = frame.area();
    let header = Line::from(vec![
        Span::styled(
            " Multi-CDN skew matrix ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  max_skew={}ms threshold={}ms",
            app.report.max_skew_ms, app.max_skew_ms
        )),
    ]);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(header).block(Block::default().borders(Borders::ALL)),
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(4)])
            .split(area)[0],
    );
    let table_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area)[1];
    let rows: Vec<Row> = app
        .report
        .edges
        .iter()
        .map(|e| {
            let hit_pct = if e.cdn_hits + e.cdn_misses > 0 {
                (e.cdn_hits as f64 / (e.cdn_hits + e.cdn_misses) as f64) * 100.0
            } else {
                0.0
            };
            Row::new(vec![
                Cell::from(e.label.as_str()),
                Cell::from(e.media_sequence.map_or_else(|| "-".into(), |s| s.to_string())),
                Cell::from(
                    e.pdt_offset_ms
                        .map_or_else(|| "-".into(), |ms| format!("{ms}ms")),
                ),
                Cell::from(e.ttfb_ms.map_or_else(|| "-".into(), |ms| format!("{ms}ms"))),
                Cell::from(format!("{hit_pct:.0}%")),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(8),
        ],
    )
    .header(Row::new(vec!["Edge", "Seq", "PDT", "TTFB", "Hit%"]).style(Style::default().fg(Color::Cyan)))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" CDN edges "),
    );
    frame.render_widget(table, table_area);
}

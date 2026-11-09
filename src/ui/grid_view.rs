//! Multi-stream grid TUI.
//!
//! Tiles show live status, bitrate, segment latency, and alarm counts. Arrow keys move the
//! selection. Enter opens the existing single-stream dashboard; leaving that dashboard returns
//! to the grid. An 80x24 terminal shows a small page and scrolls instead of shrinking every tile.

use std::io::Stdout;
use std::time::Duration;

use color_eyre::eyre::{eyre, Result, WrapErr};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc::{self, Sender};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::engine::agent::{
    session_for_agent_stream, AgentConfigFile, AgentGlobals, MAX_AGENT_STREAMS,
};
use crate::engine::evidence::{
    alarm_from_event, label_from_url, AlarmKind, AlarmState, EvidenceHandle,
};
use crate::engine::poller::DiagnosticOpts;
use crate::engine::session_poller::apply_session_doh;
use crate::engine::ManifestPoller;
use crate::models::{
    AbrVariant, DiagSeverity, LatencyState, SegmentMetrics, StreamEvent, StreamStatus,
    StreamStatusKind, EVENT_CHANNEL_CAPACITY,
};
use crate::ui::app::{restore_terminal, setup_terminal, App, SessionOpts};

const FRAME_PERIOD: Duration = Duration::from_millis(33);
const MIN_TILE_W: u16 = 36;
const MIN_TILE_H: u16 = 6;
const MAX_COLS: usize = 4;

#[derive(Debug, Clone)]
pub struct GridTarget {
    pub id: String,
    pub url: String,
    pub session: SessionOpts,
}

pub fn targets_from_urls(urls: &[String], session: &SessionOpts) -> Result<Vec<GridTarget>> {
    if urls.is_empty() {
        return Err(eyre!(
            "the grid needs at least one stream URL (or --agent CONFIG --grid)"
        ));
    }
    if urls.len() > MAX_AGENT_STREAMS {
        return Err(eyre!(
            "grid has {} URLs; max {MAX_AGENT_STREAMS}",
            urls.len()
        ));
    }
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(urls.len());
    for url in urls {
        let base = label_from_url(url);
        let n = counts.entry(base.clone()).or_insert(0);
        *n += 1;
        let id = if *n == 1 { base } else { format!("{base}-{n}") };
        out.push(GridTarget {
            session: label_session(session, &id),
            id,
            url: url.clone(),
        });
    }
    Ok(out)
}

pub fn targets_from_agent(
    cfg: &AgentConfigFile,
    evidence: Option<EvidenceHandle>,
) -> Vec<GridTarget> {
    let globals = AgentGlobals {
        allow_insecure_webhooks: cfg.allow_insecure_webhooks,
        allow_insecure_otel: cfg.allow_insecure_otel,
        otel_endpoint: cfg.otel_endpoint.clone(),
        evidence,
    };
    let mut out = Vec::with_capacity(cfg.streams.len());
    for stream in &cfg.streams {
        let id = stream.id.clone();
        let url = stream.url.clone();
        let session = session_for_agent_stream(stream, &globals);
        out.push(GridTarget { id, url, session });
    }
    out
}

fn label_session(session: &SessionOpts, id: &str) -> SessionOpts {
    let mut session = session.clone();
    if let Some(handle) = session.evidence.as_mut() {
        handle.label = Some(id.to_string());
    }
    session
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridGeometry {
    pub cols: usize,
    pub rows: usize,
    pub page_size: usize,
    pub scroll: usize,
    pub list: bool,
    pub header: u16,
    pub footer: u16,
}

pub fn grid_geometry(width: u16, height: u16, count: usize, selected: usize) -> GridGeometry {
    let count = count.max(1);
    let (header, footer) = if height >= 6 { (1u16, 1u16) } else { (0, 0) };
    let inner_h = height.saturating_sub(header.saturating_add(footer)).max(1);
    let max_cols = (width / MIN_TILE_W) as usize;
    let list = max_cols == 0 || inner_h < MIN_TILE_H;
    let (cols, rows) = if list {
        let rows = (inner_h as usize).max(1).min(count);
        (1, rows)
    } else {
        let cols = max_cols.clamp(1, MAX_COLS).min(count);
        let rows_fit = ((inner_h / MIN_TILE_H) as usize).max(1);
        let rows_needed = count.div_ceil(cols);
        (cols, rows_fit.min(rows_needed).max(1))
    };
    let page_size = cols.saturating_mul(rows).max(1);
    let selected = selected.min(count - 1);
    let scroll = (selected / page_size) * page_size;
    GridGeometry {
        cols,
        rows,
        page_size,
        scroll,
        list,
        header,
        footer,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridNav {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

pub fn move_selection(selected: usize, count: usize, cols: usize, nav: GridNav) -> usize {
    if count == 0 {
        return 0;
    }
    let cols = cols.max(1);
    let selected = selected.min(count - 1);
    match nav {
        GridNav::Left => {
            if selected.is_multiple_of(cols) {
                selected
            } else {
                selected - 1
            }
        }
        GridNav::Right => {
            let end_of_row = selected % cols + 1 == cols;
            if end_of_row || selected + 1 >= count {
                selected
            } else {
                selected + 1
            }
        }
        GridNav::Up => selected.saturating_sub(cols),
        GridNav::Down => {
            let next = selected + cols;
            if next < count {
                next
            } else {
                selected
            }
        }
        GridNav::Home => 0,
        GridNav::End => count - 1,
    }
}

pub fn move_page(selected: usize, count: usize, page_size: usize, forward: bool) -> usize {
    if count == 0 {
        return 0;
    }
    let page = page_size.max(1);
    let selected = selected.min(count - 1);
    if forward {
        selected.saturating_add(page).min(count - 1)
    } else {
        selected.saturating_sub(page)
    }
}

#[derive(Debug, Clone)]
pub struct TileState {
    pub id: String,
    pub url: String,
    pub status: StreamStatus,
    pub latency: LatencyState,
    pub variants: Vec<AbrVariant>,
    pub last_segment: Option<SegmentMetrics>,
    pub error_count: u32,
    pub alarm_count: u32,
    pub cc_errors: u32,
    pub last_alarm: Option<String>,
    pub last_kind: Option<AlarmKind>,
    alarm_state: AlarmState,
}

impl TileState {
    fn new(id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            url: url.into(),
            status: StreamStatus::live("Starting…"),
            latency: LatencyState::Unknown,
            variants: Vec::new(),
            last_segment: None,
            error_count: 0,
            alarm_count: 0,
            cc_errors: 0,
            last_alarm: None,
            last_kind: None,
            alarm_state: AlarmState::default(),
        }
    }

    fn apply(&mut self, event: StreamEvent) {
        if let Some(alarm) = alarm_from_event(&event, &mut self.alarm_state) {
            self.alarm_count = self.alarm_count.saturating_add(1);
            self.last_kind = Some(alarm.kind);
            self.last_alarm = Some(fit(&alarm.message, 80));
        }
        match event {
            StreamEvent::Status(status) => self.status = status,
            StreamEvent::Variants(v) if !v.is_empty() => self.variants = v,
            StreamEvent::Segment(seg) => self.last_segment = Some(seg),
            StreamEvent::Latency(latency) => self.latency = latency,
            StreamEvent::Finding(f) => {
                if f.severity == DiagSeverity::Error {
                    self.error_count = self.error_count.saturating_add(1);
                }
            }
            StreamEvent::Error(_) => {
                self.error_count = self.error_count.saturating_add(1);
                self.status =
                    StreamStatus::error(self.last_alarm.clone().unwrap_or_else(|| "error".into()));
            }
            StreamEvent::Tr101290(report) => self.cc_errors = report.cc_errors,
            _ => {}
        }
    }

    fn bitrate_label(&self) -> String {
        if let Some(kbps) = self.last_segment.as_ref().and_then(|s| s.download_kbps) {
            return format!("{kbps} kbps");
        }
        if let Some(v) = self
            .variants
            .iter()
            .find(|v| v.selected)
            .or_else(|| self.variants.first())
        {
            if v.bandwidth > 0 {
                return format!("{} kbps", v.bandwidth / 1000);
            }
        }
        if self.last_segment.as_ref().is_some_and(|s| s.probed) {
            return "probe".into();
        }
        "-".into()
    }

    fn latency_label(&self) -> String {
        let (ms, estimated) = match self.latency {
            LatencyState::Measured(ms) => (Some(ms), false),
            LatencyState::Estimated(ms) => (Some(ms), true),
            LatencyState::Unknown => (self.last_segment.as_ref().and_then(|s| s.latency_ms), false),
        };
        match ms {
            Some(ms) if estimated => format!("~{:.1}s", ms as f64 / 1000.0),
            Some(ms) => format!("{:.1}s", ms as f64 / 1000.0),
            None => "-".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopOut {
    Quit,
    Drill(usize),
}

struct TileTask {
    poller: JoinHandle<()>,
    forward: JoinHandle<()>,
}

struct GridMsg {
    index: usize,
    event: StreamEvent,
}

pub struct GridModel {
    tiles: Vec<TileState>,
    selected: usize,
    width: u16,
    height: u16,
    help: bool,
    toast: Option<String>,
}

impl GridModel {
    fn from_targets(targets: &[GridTarget]) -> Self {
        Self {
            tiles: targets
                .iter()
                .map(|t| TileState::new(t.id.clone(), t.url.clone()))
                .collect(),
            selected: 0,
            width: 80,
            height: 24,
            help: false,
            toast: None,
        }
    }

    fn geometry(&self) -> GridGeometry {
        grid_geometry(self.width, self.height, self.tiles.len(), self.selected)
    }
}

pub struct GridApp {
    targets: Vec<GridTarget>,
    model: GridModel,
    tasks: Vec<Option<TileTask>>,
    tx: Sender<GridMsg>,
    rx: mpsc::Receiver<GridMsg>,
}

impl GridApp {
    pub async fn run(targets: Vec<GridTarget>) -> Result<()> {
        if targets.is_empty() {
            return Err(eyre!("the grid has no streams"));
        }
        let (tx, rx) = mpsc::channel(1024);
        let model = GridModel::from_targets(&targets);
        let mut app = Self {
            tasks: Vec::new(),
            model,
            targets,
            tx,
            rx,
        };
        for index in 0..app.targets.len() {
            app.spawn_tile(index)?;
        }
        let mut terminal = setup_terminal()?;
        let size = terminal.size().unwrap_or(ratatui::layout::Size {
            width: 80,
            height: 24,
        });
        app.model.width = size.width;
        app.model.height = size.height;
        let result = app.loop_terminal(&mut terminal).await;
        app.abort_all();
        restore_terminal(&mut terminal);
        result
    }

    async fn loop_terminal(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> Result<()> {
        loop {
            match self.event_loop(terminal).await? {
                LoopOut::Quit => return Ok(()),
                LoopOut::Drill(index) => {
                    self.stop_tile(index);
                    let target = self.targets[index].clone();
                    restore_terminal(terminal);
                    let opened = App::run_diagnostics(
                        target.id.clone(),
                        target.url.clone(),
                        target.session.clone(),
                    )
                    .await;
                    *terminal = setup_terminal()?;
                    self.spawn_tile(index)?;
                    if let Err(err) = opened {
                        self.model.toast = Some(format!("could not open stream: {err}"));
                    }
                }
            }
        }
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> Result<LoopOut> {
        let mut events = EventStream::new();
        let mut frames = interval(FRAME_PERIOD);
        frames.set_missed_tick_behavior(MissedTickBehavior::Skip);
        terminal.draw(|frame| draw(frame, &mut self.model))?;
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return Ok(LoopOut::Quit),
                Some(msg) = self.rx.recv() => {
                    if let Some(tile) = self.model.tiles.get_mut(msg.index) {
                        tile.apply(msg.event);
                    }
                }
                maybe = events.next() => {
                    match maybe {
                        Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press
                            || key.kind == KeyEventKind::Repeat =>
                        {
                            if let Some(out) = self.handle_key(key.code, key.modifiers) {
                                return Ok(out);
                            }
                        }
                        Some(Ok(Event::Resize(w, h))) => {
                            self.model.width = w;
                            self.model.height = h;
                            terminal.draw(|frame| draw(frame, &mut self.model))?;
                        }
                        Some(Err(_)) | None => return Ok(LoopOut::Quit),
                        _ => {}
                    }
                }
                _ = frames.tick() => {
                    terminal.draw(|frame| draw(frame, &mut self.model))?;
                }
            }
        }
    }

    fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<LoopOut> {
        if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            return Some(LoopOut::Quit);
        }
        if self.model.help && !matches!(code, KeyCode::Char('?' | 'q' | 'Q') | KeyCode::Esc) {
            self.model.help = false;
        }
        let geo = self.model.geometry();
        let count = self.model.tiles.len();
        match code {
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => Some(LoopOut::Quit),
            KeyCode::Char('?') => {
                self.model.help = !self.model.help;
                None
            }
            KeyCode::Enter => Some(LoopOut::Drill(
                self.model.selected.min(count.saturating_sub(1)),
            )),
            KeyCode::Left | KeyCode::Char('h') => {
                self.model.selected =
                    move_selection(self.model.selected, count, geo.cols, GridNav::Left);
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.model.selected =
                    move_selection(self.model.selected, count, geo.cols, GridNav::Right);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.model.selected =
                    move_selection(self.model.selected, count, geo.cols, GridNav::Up);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.model.selected =
                    move_selection(self.model.selected, count, geo.cols, GridNav::Down);
                None
            }
            KeyCode::PageUp | KeyCode::Char('[') => {
                self.model.selected = move_page(self.model.selected, count, geo.page_size, false);
                None
            }
            KeyCode::PageDown | KeyCode::Char(']') => {
                self.model.selected = move_page(self.model.selected, count, geo.page_size, true);
                None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.model.selected = 0;
                None
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.model.selected = count.saturating_sub(1);
                None
            }
            _ => None,
        }
    }

    fn spawn_tile(&mut self, index: usize) -> Result<()> {
        self.stop_tile(index);
        let target = &self.targets[index];
        let (poller, forward) = spawn_poller(target, self.tx.clone(), index)?;
        if self.tasks.len() <= index {
            self.tasks.resize_with(index + 1, || None);
        }
        self.tasks[index] = Some(TileTask { poller, forward });
        Ok(())
    }

    fn stop_tile(&mut self, index: usize) {
        if let Some(slot) = self.tasks.get_mut(index) {
            if let Some(task) = slot.take() {
                task.poller.abort();
                task.forward.abort();
            }
        }
    }

    fn abort_all(&mut self) {
        for task in self.tasks.iter_mut().flatten() {
            task.poller.abort();
            task.forward.abort();
        }
    }
}

fn spawn_poller(
    target: &GridTarget,
    tx: Sender<GridMsg>,
    index: usize,
) -> Result<(JoinHandle<()>, JoinHandle<()>)> {
    let (poll_tx, mut poll_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let session = label_session(&target.session, &target.id);
    let mut poller = ManifestPoller::new(
        target.url.as_str(),
        &session.headers,
        session.user_agent.as_deref(),
        session.interval_ms,
        session.probe_headers,
        session.probe_drm,
        poll_tx,
    )
    .wrap_err_with(|| format!("failed to start {}", target.id))?
    .with_diagnostics(&DiagnosticOpts {
        tr101290: session.tr101290,
        probe_sei: session.probe_sei,
    });
    if let Some(ck) = &session.clearkey {
        if let Ok(spec) = crate::engine::drm_probe::ClearKeySpec::parse(ck) {
            poller = poller.with_clearkey(Some(spec));
        }
    }
    if let Some(hook) = session.webhook_url.clone() {
        let alerts = crate::engine::webhook::AlertKind::parse_list(&session.alert_on)
            .wrap_err_with(|| format!("{} has an invalid alert_on list", target.id))?;
        crate::engine::webhook::validate_webhook_url(&hook, session.allow_insecure_webhooks)?;
        let (hook_tx, hook_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        poller = poller.with_webhook_tx(hook_tx);
        crate::engine::webhook::spawn_webhook_listener(
            crate::engine::webhook::WebhookConfig {
                url: hook,
                alerts,
                allow_insecure: session.allow_insecure_webhooks,
            },
            hook_rx,
            format!("{}:{}", target.id, target.url),
        );
    }
    poller = apply_session_doh(poller, &session)?;
    let poller_task = tokio::spawn(async move {
        poller.run().await;
    });
    let forward = tokio::spawn(async move {
        while let Some(event) = poll_rx.recv().await {
            if tx.send(GridMsg { index, event }).await.is_err() {
                break;
            }
        }
    });
    Ok((poller_task, forward))
}

fn draw(frame: &mut Frame, model: &mut GridModel) {
    let area = frame.area();
    model.width = area.width;
    model.height = area.height;
    if area.width == 0 || area.height == 0 || model.tiles.is_empty() {
        return;
    }
    let geo = model.geometry();
    let body = Rect {
        x: area.x,
        y: area.y.saturating_add(geo.header),
        width: area.width,
        height: area
            .height
            .saturating_sub(geo.header.saturating_add(geo.footer)),
    };
    if geo.header > 0 {
        draw_header(
            frame,
            model,
            geo,
            Rect {
                height: geo.header,
                ..area
            },
        );
    }
    let visible = geo
        .page_size
        .min(model.tiles.len().saturating_sub(geo.scroll));
    for slot in 0..visible {
        let index = geo.scroll + slot;
        let rect = tile_rect(body, geo.cols, geo.rows, slot);
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        if geo.list || rect.height < MIN_TILE_H {
            draw_list_row(frame, &model.tiles[index], rect, index == model.selected);
        } else {
            draw_tile(frame, &model.tiles[index], rect, index == model.selected);
        }
    }
    if geo.footer > 0 {
        let footer = Rect {
            x: area.x,
            y: area
                .y
                .saturating_add(area.height.saturating_sub(geo.footer)),
            width: area.width,
            height: geo.footer,
        };
        draw_footer(frame, model, footer);
    }
    if model.help {
        draw_help(frame, area);
    }
}

fn draw_header(frame: &mut Frame, model: &GridModel, geo: GridGeometry, area: Rect) {
    let id = model
        .tiles
        .get(model.selected)
        .map_or("-", |t| t.id.as_str());
    let alarms: u32 = model.tiles.iter().map(|t| t.alarm_count).sum();
    let page = geo.scroll / geo.page_size + 1;
    let pages = model.tiles.len().div_ceil(geo.page_size).max(1);
    let text = format!(
        " streams {}  {}/{}  page {page}/{pages}  alarms {alarms}  {id}",
        model.tiles.len(),
        model.selected + 1,
        model.tiles.len()
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            fit(&text, area.width as usize),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        area,
    );
}

fn draw_footer(frame: &mut Frame, model: &GridModel, area: Rect) {
    let text = model.toast.as_ref().map_or_else(
        || " j/k/h/l move   enter open   [ ] page   ? help   q quit ".to_string(),
        |toast| format!(" {toast} "),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            fit(&text, area.width as usize),
            Style::default().fg(Color::Black).bg(Color::LightYellow),
        )),
        area,
    );
}

fn draw_list_row(frame: &mut Frame, tile: &TileState, area: Rect, selected: bool) {
    let mark = if selected { ">" } else { " " };
    let line = format!(
        "{mark}{} {} {} {} err {}",
        fit(&tile.id, 16),
        tile.status.kind.label(),
        tile.bitrate_label(),
        tile.latency_label(),
        tile.error_count
    );
    let style = if selected {
        Style::default()
            .fg(Color::Black)
            .bg(status_color(tile))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(status_color(tile))
    };
    frame.render_widget(
        Paragraph::new(Span::styled(fit(&line, area.width as usize), style)),
        area,
    );
}

fn draw_tile(frame: &mut Frame, tile: &TileState, area: Rect, selected: bool) {
    let border = if selected {
        Color::Yellow
    } else if tile.last_kind == Some(AlarmKind::Stall)
        || tile.status.kind == StreamStatusKind::Error
    {
        Color::Red
    } else if tile.alarm_count > 0 {
        Color::Magenta
    } else {
        Color::DarkGray
    };
    let label = format!(
        " {} {} ",
        fit(&tile.id, area.width.saturating_sub(12) as usize),
        tile.status.kind.label()
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(fit(&label, area.width as usize))
        .title_style(
            Style::default()
                .fg(status_color(tile))
                .add_modifier(Modifier::BOLD),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let seq = tile
        .last_segment
        .as_ref()
        .map_or_else(|| "-".to_string(), |seg| seg.media_sequence.to_string());
    let http = tile
        .last_segment
        .as_ref()
        .map_or_else(|| "-".to_string(), |seg| seg.http_status.to_string());
    let alarm = tile.last_alarm.as_deref().unwrap_or("ok");
    let lines = [
        format!("{}  {}", tile.bitrate_label(), tile.latency_label()),
        format!("seq {seq}  http {http}"),
        format!(
            "err {}  cc {}  alarms {}",
            tile.error_count, tile.cc_errors, tile.alarm_count
        ),
        alarm.to_string(),
    ];
    let text: Vec<Line> = lines
        .iter()
        .take(inner.height as usize)
        .map(|line| Line::from(fit(line, inner.width as usize)))
        .collect();
    frame.render_widget(Paragraph::new(text), inner);
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let w = area.width.clamp(20, 60);
    let h = area.height.clamp(4, 12);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let modal = Rect::new(x, y, w, h);
    frame.render_widget(Clear, modal);
    let body = [
        " Grid",
        " arrows or hjkl   move",
        " enter            open the stream",
        " q or esc there   return here",
        " [ ]              page",
        " q or esc         quit",
    ];
    let lines: Vec<Line> = body
        .iter()
        .take(h.saturating_sub(2) as usize)
        .map(|line| Line::from(*line))
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" help "),
        ),
        modal,
    );
}

fn tile_rect(area: Rect, cols: usize, rows: usize, slot: usize) -> Rect {
    let cols = u16::try_from(cols.max(1)).unwrap_or(1);
    let rows = u16::try_from(rows.max(1)).unwrap_or(1);
    let col = (slot as u16) % cols;
    let row = (slot as u16) / cols;
    let tw = area.width / cols;
    let th = area.height / rows;
    let extra_w = area.width % cols;
    let extra_h = area.height % rows;
    let width = if col + 1 == cols { tw + extra_w } else { tw };
    let height = if row + 1 == rows { th + extra_h } else { th };
    Rect {
        x: area.x.saturating_add(col.saturating_mul(tw)),
        y: area.y.saturating_add(row.saturating_mul(th)),
        width,
        height,
    }
}

fn status_color(tile: &TileState) -> Color {
    match tile.status.kind {
        StreamStatusKind::Live => Color::Green,
        StreamStatusKind::Vod => Color::Cyan,
        StreamStatusKind::Degraded => Color::Yellow,
        StreamStatusKind::Error => Color::Red,
    }
}

fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 1 {
        return "…".into();
    }
    let mut out: String = text.chars().take(width - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CdnEdgeInfo, ContainerKind, SegmentMetrics};
    use ratatui::backend::TestBackend;

    fn urls(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("https://cdn.example/live/{i}.m3u8"))
            .collect()
    }

    fn session() -> SessionOpts {
        SessionOpts {
            headers: Vec::new(),
            user_agent: None,
            interval_ms: None,
            probe_headers: true,
            probe_drm: false,
            clearkey: None,
            export_incident: None,
            webhook_url: None,
            alert_on: "stall".into(),
            allow_insecure_webhooks: false,
            allow_insecure_otel: false,
            otel_endpoint: None,
            tr101290: false,
            probe_sei: false,
            doh_provider: None,
            evidence_settings: crate::engine::evidence::EvidenceSettings::default(),
            evidence: None,
        }
    }

    fn sample_segment() -> SegmentMetrics {
        SegmentMetrics {
            media_sequence: 42,
            duration_secs: 2.0,
            size_bytes: 1_000_000,
            transferred_bytes: 1_000_000,
            ttfb_ms: 40,
            download_ms: 200,
            dl_to_dur_ratio: Some(0.1),
            download_kbps: Some(4500),
            latency_ms: Some(2100),
            uri: "https://cdn.example/42.ts".into(),
            cdn: CdnEdgeInfo::default(),
            probed: false,
            container: ContainerKind::Ts,
            http_status: 200,
            network: None,
            wire: None,
        }
    }

    #[test]
    fn eighty_by_twenty_four_pages_instead_of_fitting_everything() {
        let geo = grid_geometry(80, 24, 12, 0);
        assert!(geo.cols >= 2, "cols {}", geo.cols);
        assert!(geo.page_size < 12, "page {}", geo.page_size);
        assert!(!geo.list);
        assert_eq!(geo.scroll, 0);
        let later = grid_geometry(80, 24, 12, geo.page_size);
        assert_eq!(later.scroll, geo.page_size);
        assert!(later.scroll + later.page_size <= 12 || later.scroll < 12);
    }

    #[test]
    fn narrow_or_short_terminals_show_fewer_tiles() {
        // 80x8 keeps one row of tiles and scrolls the rest.
        let short = grid_geometry(80, 8, 10, 0);
        assert!(short.page_size < 10, "page {}", short.page_size);
        assert!(short.page_size <= 2, "page {}", short.page_size);
        // Narrower than one tile: a one-column list.
        let narrow = grid_geometry(30, 24, 5, 3);
        assert!(narrow.list);
        assert_eq!(narrow.cols, 1);
        assert_eq!(narrow.scroll, 0);
        // 10x3 still pages instead of drawing every stream.
        let tiny = grid_geometry(10, 3, 4, 2);
        assert!(tiny.list);
        assert!(tiny.page_size < 4, "page {}", tiny.page_size);
        assert!(tiny.scroll <= 2);
    }

    #[test]
    fn wide_terminal_uses_more_columns() {
        let geo = grid_geometry(200, 50, 20, 0);
        assert_eq!(geo.cols, MAX_COLS);
        assert!(geo.page_size > 6);
    }

    #[test]
    fn selection_moves_by_cell_and_stops_at_the_edge() {
        assert_eq!(move_selection(0, 12, 2, GridNav::Right), 1);
        assert_eq!(move_selection(1, 12, 2, GridNav::Right), 1);
        assert_eq!(move_selection(1, 12, 2, GridNav::Down), 3);
        assert_eq!(move_selection(0, 12, 2, GridNav::Left), 0);
        assert_eq!(move_selection(0, 12, 2, GridNav::Up), 0);
        assert_eq!(move_selection(10, 12, 2, GridNav::Down), 10);
        assert_eq!(move_page(0, 12, 6, true), 6);
        assert_eq!(move_page(6, 12, 6, false), 0);
        assert_eq!(move_page(10, 12, 6, true), 11);
    }

    #[test]
    fn duplicate_url_labels_get_a_suffix() {
        let targets = targets_from_urls(
            &[
                "https://a.example/live.m3u8".into(),
                "https://b.example/live.m3u8".into(),
            ],
            &session(),
        )
        .unwrap();
        assert_eq!(targets[0].id, "live_m3u8");
        assert_eq!(targets[1].id, "live_m3u8-2");
        assert!(targets_from_urls(&[], &session()).is_err());
        assert!(targets_from_urls(&urls(MAX_AGENT_STREAMS + 1), &session()).is_err());
    }

    #[test]
    fn tile_tracks_bitrate_latency_and_alarms() {
        let mut tile = TileState::new("primary", "https://cdn.example/live.m3u8");
        tile.apply(StreamEvent::Segment(sample_segment()));
        tile.apply(StreamEvent::Latency(LatencyState::Measured(2100)));
        assert_eq!(tile.bitrate_label(), "4500 kbps");
        assert_eq!(tile.latency_label(), "2.1s");
        tile.apply(StreamEvent::Finding(crate::models::DiagnosticFinding {
            category: crate::models::DiagCategory::Stalling,
            severity: DiagSeverity::Warn,
            rule: "RTF_STALL".into(),
            message: "buffer drain".into(),
            reason: None,
        }));
        assert_eq!(tile.alarm_count, 1);
        assert_eq!(tile.last_kind, Some(AlarmKind::Stall));
        assert_eq!(tile.error_count, 0);
        tile.apply(StreamEvent::Error("HTTP 500".into()));
        assert!(tile.error_count >= 1);
        assert_eq!(tile.status.kind, StreamStatusKind::Error);
    }

    #[test]
    fn draw_eighty_by_twenty_four_shows_a_page_not_every_tile() {
        let targets = targets_from_urls(&urls(12), &session()).unwrap();
        let mut model = GridModel::from_targets(&targets);
        model.tiles[0].apply(StreamEvent::Segment(sample_segment()));
        model.tiles[0].status = StreamStatus::live("edge");
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut model)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            text.contains("4500 kbps") || text.contains("LIVE"),
            "{text}"
        );
        assert!(text.contains(&targets[0].id), "{text}");
        let geo = grid_geometry(80, 24, 12, 0);
        assert!(
            !text.contains(&targets[geo.page_size].id),
            "off-page tile {} was drawn:\n{text}",
            targets[geo.page_size].id
        );
    }

    #[test]
    fn draw_short_terminal_does_not_panic() {
        let targets = targets_from_urls(&urls(6), &session()).unwrap();
        let mut model = GridModel::from_targets(&targets);
        for (w, h) in [(80, 24), (40, 10), (20, 5), (80, 8)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| draw(frame, &mut model)).unwrap();
        }
    }
}

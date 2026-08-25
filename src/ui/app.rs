use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use color_eyre::eyre::{eyre, Result, WrapErr};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::engine::ManifestPoller;
use crate::models::{
    format_dvr_window, AbrHealth, AbrVariant, AdBreakInfo, CdnStats, ChannelEntry, DiagCategory,
    DiagnosticFinding, DiagnosticSummary, HealthReport, LatencyState, LogEntry, LogLevel,
    PlaylistMeta, RingBuffer, SegmentMetrics, StreamEvent, StreamSnapshot, StreamStatus,
    VirtualBuffer, DEEP_WIRE_PROBE_BYTES, DIAGNOSTIC_DIR, HISTORY_CAPACITY, LOG_CAPACITY,
};
use crate::ui::channel_picker::{ChannelPicker, PickerAction};
use crate::ui::layout;

const FRAME_PERIOD: Duration = Duration::from_millis(33);
const TOAST_SECS: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMode {
    Picker,
    Diagnostic,
}

#[derive(Debug, Clone)]
pub struct SessionOpts {
    pub headers: Vec<String>,
    pub user_agent: Option<String>,
    pub interval_ms: Option<u64>,
    pub probe_headers: bool,
    pub webhook_url: Option<String>,
    pub alert_on: String,
}

pub struct App {
    pub source_url: String,
    pub active_url: String,
    pub channel_name: Option<String>,
    pub status: StreamStatus,
    pub latency: LatencyState,
    pub playlist: Option<PlaylistMeta>,
    pub variants: Vec<AbrVariant>,
    pub last_segment: Option<SegmentMetrics>,
    pub health: HealthReport,
    pub cdn: CdnStats,
    pub abr_health: AbrHealth,
    pub active_ad: Option<AdBreakInfo>,
    pub buffer: VirtualBuffer,
    pub probe_mode: bool,
    pub findings: Vec<DiagnosticFinding>,
    pub latency_history: RingBuffer,
    pub ttfb_history: RingBuffer,
    pub bitrate_history: RingBuffer,
    pub transfer_history: RingBuffer,
    pub log: Vec<LogEntry>,
    pub log_scroll: u16,
    pub should_quit: bool,
    pub mode: UiMode,
    pub overlay: bool,
    pub show_help: bool,
    /// Toast message (curl copied, export path, …).
    pub toast: Option<(String, Instant)>,
    pub picker: Option<ChannelPicker>,
    pub session: SessionOpts,
    rx: UnboundedReceiver<StreamEvent>,
    tx: UnboundedSender<StreamEvent>,
    poller: Option<JoinHandle<()>>,
}

impl App {
    /// IPTV / catalog: Channel Picker; poller starts after channel select.
    pub async fn run_picker(
        origin: String,
        channels: Vec<ChannelEntry>,
        session: SessionOpts,
    ) -> Result<()> {
        if channels.is_empty() {
            return Err(eyre!("channel list is empty"));
        }
        let app = Self::new(origin, None, channels, session)?;
        debug_assert!(app.mode == UiMode::Picker);
        debug_assert!(app.poller.is_none());
        app.run().await
    }

    /// Single HLS/DASH URL → diagnostics dashboard + poller.
    pub async fn run_diagnostics(origin: String, url: String, session: SessionOpts) -> Result<()> {
        let app = Self::new(origin, Some(url), Vec::new(), session)?;
        debug_assert!(app.mode == UiMode::Diagnostic);
        app.run().await
    }

    pub fn new(
        source_label: String,
        start_url: Option<String>,
        channels: Vec<ChannelEntry>,
        session: SessionOpts,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let picker = if channels.is_empty() {
            None
        } else {
            Some(ChannelPicker::new(channels))
        };

        let start_diagnostics = start_url.is_some() && picker.is_none();

        let mut app = Self {
            active_url: start_url.clone().unwrap_or_else(|| source_label.clone()),
            source_url: source_label,
            channel_name: None,
            status: StreamStatus::live(if start_diagnostics {
                "Starting…"
            } else {
                "Channel picker"
            }),
            latency: LatencyState::Unknown,
            playlist: None,
            variants: Vec::new(),
            last_segment: None,
            health: HealthReport::perfect(),
            cdn: CdnStats::default(),
            abr_health: AbrHealth::default(),
            active_ad: None,
            buffer: VirtualBuffer::default(),
            probe_mode: session.probe_headers,
            findings: Vec::new(),
            latency_history: RingBuffer::new(HISTORY_CAPACITY),
            ttfb_history: RingBuffer::new(HISTORY_CAPACITY),
            bitrate_history: RingBuffer::new(HISTORY_CAPACITY),
            transfer_history: RingBuffer::new(HISTORY_CAPACITY),
            log: Vec::new(),
            log_scroll: 0,
            should_quit: false,
            mode: if start_diagnostics {
                UiMode::Diagnostic
            } else {
                UiMode::Picker
            },
            overlay: false,
            show_help: false,
            toast: None,
            picker,
            session,
            rx,
            tx,
            poller: None,
        };

        if start_diagnostics {
            if let Some(url) = start_url {
                app.spawn_poller(url)?;
            }
        }

        Ok(app)
    }

    pub fn has_catalog(&self) -> bool {
        self.picker
            .as_ref()
            .map(|p| !p.channels.is_empty())
            .unwrap_or(false)
    }

    fn spawn_poller(&mut self, url: String) -> Result<()> {
        if let Some(handle) = self.poller.take() {
            handle.abort();
        }
        let (tx, rx) = mpsc::unbounded_channel();
        self.tx = tx.clone();
        self.rx = rx;
        self.source_url = url.clone();
        self.active_url = url.clone();

        let mut poller = ManifestPoller::new(
            url.clone(),
            self.session.headers.clone(),
            self.session.user_agent.clone(),
            self.session.interval_ms,
            self.session.probe_headers,
            tx,
        )
        .wrap_err("failed to start poller")?;

        if let Some(hook_url) = self.session.webhook_url.clone() {
            if let Ok(alerts) =
                crate::engine::webhook::AlertKind::parse_list(&self.session.alert_on)
            {
                let (hook_tx, hook_rx) = mpsc::unbounded_channel();
                poller = poller.with_webhook_tx(hook_tx);
                crate::engine::webhook::spawn_webhook_listener(
                    crate::engine::webhook::WebhookConfig {
                        url: hook_url,
                        alerts,
                    },
                    hook_rx,
                    url,
                );
            }
        }

        self.poller = Some(tokio::spawn(async move {
            poller.run().await;
        }));
        Ok(())
    }

    fn switch_channel(&mut self, index: usize) -> Result<()> {
        let (url, name) = {
            let picker = self
                .picker
                .as_ref()
                .ok_or_else(|| eyre!("no channel catalog"))?;
            let ch = picker
                .channels
                .get(index)
                .ok_or_else(|| eyre!("invalid channel index"))?;
            (ch.url.clone(), ch.name.clone())
        };
        self.reset_metrics_silent();
        self.channel_name = Some(name.clone());
        self.spawn_poller(url)?;
        self.mode = UiMode::Diagnostic;
        self.overlay = false;
        self.push_log(
            LogLevel::Info,
            DiagCategory::Info,
            format!("Switched channel: {name}"),
        );
        Ok(())
    }

    pub async fn run(mut self) -> Result<()> {
        let mut terminal = setup_terminal()?;
        let result = self.event_loop(&mut terminal).await;
        if let Some(handle) = self.poller.take() {
            handle.abort();
        }
        let _ = restore_terminal(&mut terminal);
        result
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> Result<()> {
        let mut events = EventStream::new();
        let mut frames = interval(FRAME_PERIOD);
        frames.set_missed_tick_behavior(MissedTickBehavior::Skip);

        terminal.draw(|frame| self.draw_ui(frame))?;

        while !self.should_quit {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    self.should_quit = true;
                }
                maybe_event = self.rx.recv() => {
                    match maybe_event {
                        Some(event) => self.apply_event(event),
                        None => {
                            if self.mode == UiMode::Diagnostic && self.poller.is_some() {
                                self.push_log(LogLevel::Warn, DiagCategory::Info, "Poller channel closed");
                                self.status = StreamStatus::error("Poller stopped");
                            }
                        }
                    }
                }
                maybe_key = events.next() => {
                    match maybe_key {
                        Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press
                            || key.kind == KeyEventKind::Repeat =>
                        {
                            self.handle_key(key)?;
                        }
                        Some(Ok(Event::Resize(_, _))) => {}
                        Some(Err(err)) => {
                            self.push_log(LogLevel::Error, DiagCategory::Info, format!("Keyboard: {err}"));
                        }
                        None => self.should_quit = true,
                        _ => {}
                    }
                }
                _ = frames.tick() => {
                    terminal.draw(|frame| self.draw_ui(frame))?;
                }
            }
        }

        Ok(())
    }

    fn draw_ui(&mut self, frame: &mut ratatui::Frame) {
        if let Some((_, until)) = &self.toast {
            if Instant::now() > *until {
                self.toast = None;
            }
        }

        if self.mode == UiMode::Picker {
            if let Some(picker) = self.picker.as_mut() {
                picker.draw(frame, frame.area(), false);
            }
            if self.show_help {
                layout::draw_help(frame, frame.area(), true);
            }
            return;
        }
        layout::draw(frame, self);
        if self.overlay {
            if let Some(picker) = self.picker.as_mut() {
                picker.draw(frame, frame.area(), true);
            }
        }
        if self.show_help {
            layout::draw_help(frame, frame.area(), false);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Ok(());
        }

        if self.show_help {
            self.show_help = false;
            return Ok(());
        }

        let picker_active = self.mode == UiMode::Picker || self.overlay;
        if picker_active {
            if key.code == KeyCode::Char('?') {
                self.show_help = true;
                return Ok(());
            }
            if let Some(picker) = self.picker.as_mut() {
                match picker.handle_key(key) {
                    PickerAction::None => {}
                    PickerAction::Quit => self.should_quit = true,
                    PickerAction::Cancel => {
                        if self.overlay {
                            self.overlay = false;
                        } else {
                            self.should_quit = true;
                        }
                    }
                    PickerAction::Select(i) => self.switch_channel(i)?,
                }
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.should_quit = true;
            }
            KeyCode::Esc => {
                if self.has_catalog() && self.mode == UiMode::Diagnostic {
                    self.return_to_picker();
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Tab => {
                if self.has_catalog() {
                    self.overlay = true;
                }
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.copy_curl_to_clipboard()?;
            }
            KeyCode::Char('?') => {
                self.show_help = true;
            }
            KeyCode::Char(' ') => self.export_diagnostic()?,
            KeyCode::Char('r') | KeyCode::Char('R') => self.reset_metrics(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.log_scroll = self.log_scroll.saturating_add(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.log_scroll = self.log_scroll.saturating_sub(1);
            }
            KeyCode::PageUp => {
                self.log_scroll = self.log_scroll.saturating_add(5);
            }
            KeyCode::PageDown => {
                self.log_scroll = self.log_scroll.saturating_sub(5);
            }
            _ => {}
        }
        Ok(())
    }

    fn copy_curl_to_clipboard(&mut self) -> Result<()> {
        let cmd = self.build_curl_command();
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(cmd.clone())) {
            Ok(()) => {
                self.toast = Some((
                    "[Copied cURL to clipboard!]".into(),
                    Instant::now() + Duration::from_secs(TOAST_SECS),
                ));
                self.push_log(
                    LogLevel::Info,
                    DiagCategory::Info,
                    "curl command copied to clipboard",
                );
            }
            Err(err) => {
                self.push_log(
                    LogLevel::Warn,
                    DiagCategory::Info,
                    format!("clipboard unavailable ({err}); curl:\n{cmd}"),
                );
                self.toast = Some((
                    "clipboard failed — see log".into(),
                    Instant::now() + Duration::from_secs(TOAST_SECS),
                ));
            }
        }
        Ok(())
    }

    pub fn build_curl_command(&self) -> String {
        let url = self
            .last_segment
            .as_ref()
            .map(|s| s.uri.as_str())
            .unwrap_or(self.active_url.as_str());
        let mut parts = vec!["curl -sS -L".to_string()];
        if self.probe_mode || self.session.probe_headers {
            parts.push(format!("-H \"Range: bytes=0-{DEEP_WIRE_PROBE_BYTES}\""));
        }
        for h in &self.session.headers {
            let escaped = h.replace('"', "\\\"");
            parts.push(format!("-H \"{escaped}\""));
        }
        if let Some(ua) = &self.session.user_agent {
            parts.push(format!("-A \"{ua}\""));
        }
        parts.push(format!("\"{url}\""));
        parts.join(" ")
    }

    fn return_to_picker(&mut self) {
        if let Some(handle) = self.poller.take() {
            handle.abort();
        }
        self.overlay = false;
        self.mode = UiMode::Picker;
        self.status = StreamStatus::live("Channel picker");
    }

    fn reset_metrics_silent(&mut self) {
        self.latency_history.clear();
        self.ttfb_history.clear();
        self.bitrate_history.clear();
        self.transfer_history.clear();
        self.findings.clear();
        self.buffer = VirtualBuffer::default();
        self.log_scroll = 0;
        self.last_segment = None;
        self.playlist = None;
        self.variants.clear();
        self.active_ad = None;
        self.cdn = CdnStats::default();
        self.health = HealthReport::perfect();
        self.latency = LatencyState::Unknown;
        self.log.clear();
    }

    fn reset_metrics(&mut self) {
        self.latency_history.clear();
        self.ttfb_history.clear();
        self.bitrate_history.clear();
        self.transfer_history.clear();
        self.findings.clear();
        self.buffer = VirtualBuffer::default();
        self.log_scroll = 0;
        self.push_log(
            LogLevel::Info,
            DiagCategory::Info,
            "Metrics / diagnostics history reset",
        );
    }

    fn apply_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Status(status) => self.status = status,
            StreamEvent::Variants(variants) => {
                if !variants.is_empty() {
                    self.variants = variants;
                }
            }
            StreamEvent::PlaylistMeta(meta) => {
                self.active_url = meta.url.clone();
                self.playlist = Some(meta);
            }
            StreamEvent::Segment(metrics) => {
                self.ttfb_history.push(metrics.ttfb_ms);
                self.transfer_history.push(metrics.download_ms.max(1));
                if let Some(kbps) = metrics.download_kbps {
                    self.bitrate_history.push(kbps);
                }
                if metrics.probed {
                    self.probe_mode = true;
                }
                self.last_segment = Some(metrics);
            }
            StreamEvent::Latency(state) => {
                if let LatencyState::Measured(ms) = state {
                    self.latency_history.push(ms);
                }
                self.latency = state;
            }
            StreamEvent::Health(h) => self.health = h,
            StreamEvent::CdnStats(c) => self.cdn = c,
            StreamEvent::AbrHealth(a) => self.abr_health = a,
            StreamEvent::AdBreak(ad) => {
                if ad.kind.contains("CUE-IN") {
                    self.active_ad = None;
                } else {
                    self.active_ad = Some(ad);
                }
            }
            StreamEvent::Buffer(b) => self.buffer = b,
            StreamEvent::ProbeMode(on) => self.probe_mode = on,
            StreamEvent::Finding(f) => {
                self.findings.push(f);
                if self.findings.len() > 100 {
                    let drain = self.findings.len() - 100;
                    self.findings.drain(0..drain);
                }
            }
            StreamEvent::WireProbe(_) => {}
            StreamEvent::Log {
                level,
                category,
                message,
            } => self.push_log(level, category, message),
            StreamEvent::Error(message) => {
                self.push_log(LogLevel::Error, DiagCategory::Info, message);
            }
        }
    }

    fn push_log(&mut self, level: LogLevel, category: DiagCategory, message: impl Into<String>) {
        self.log.push(LogEntry::make(level, category, message));
        if self.log.len() > LOG_CAPACITY {
            let drain = self.log.len() - LOG_CAPACITY;
            self.log.drain(0..drain);
        }
    }

    fn export_diagnostic(&mut self) -> Result<()> {
        let dvr = self
            .playlist
            .as_ref()
            .map(|p| format_dvr_window(p.window_segments, p.window_secs))
            .unwrap_or_else(|| "n/a".into());
        let status = match self.status.kind {
            crate::models::StreamStatusKind::Live => "LIVE",
            crate::models::StreamStatusKind::Error => "ERROR",
            crate::models::StreamStatusKind::Degraded => "DEGRADED",
        };

        let health = self.health.clone();
        let now = chrono::Utc::now();
        let channel = self.channel_name.clone().or_else(|| {
            url::Url::parse(&self.active_url)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.to_string()))
        });

        let title = match &channel {
            Some(name) => format!(
                "{name} — diagnostic @ {}",
                now.format("%Y-%m-%d %H:%M:%S UTC")
            ),
            None => format!(
                "streamtop diagnostic @ {}",
                now.format("%Y-%m-%d %H:%M:%S UTC")
            ),
        };

        let timeline: Vec<String> = self.log.iter().map(LogEntry::timeline_line).collect();

        let snapshot = StreamSnapshot {
            title,
            summary: DiagnosticSummary {
                channel: channel.clone(),
                captured_at: now,
                source_url: self.source_url.clone(),
                active_url: self.active_url.clone(),
                status: status.into(),
                health_score: health.score,
                health_label: health.label.clone(),
                latency: self.latency.display(),
                cdn: self
                    .cdn
                    .last
                    .as_ref()
                    .map(|c| c.badge())
                    .unwrap_or_else(|| "UNKNOWN".into()),
                dvr_window: dvr,
                buffer: self.buffer.display(),
                ll_hls: self
                    .playlist
                    .as_ref()
                    .map(|p| p.ll_hls.is_ll_hls)
                    .unwrap_or(false),
            },
            timeline,
            health,
            cdn: self.cdn.clone(),
            abr_health: self.abr_health.clone(),
            active_ad: self.active_ad.clone(),
            playlist: self.playlist.clone(),
            abr_profiles: self.variants.clone(),
            last_segment: self.last_segment.clone(),
            findings: self.findings.clone(),
            event_log: self.log.clone(),
        };

        let path = diagnostic_export_path(channel.as_deref(), &self.active_url, now);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&snapshot)
            .wrap_err("failed to serialize diagnostic report")?;
        std::fs::write(&path, json)
            .wrap_err_with(|| format!("failed to write {}", path.display()))?;

        let saved = path.display().to_string();
        self.toast = Some((
            format!("Saved {saved}"),
            Instant::now() + Duration::from_secs(TOAST_SECS),
        ));
        self.push_log(
            LogLevel::Info,
            DiagCategory::Info,
            format!("Diagnostic report saved: {saved}"),
        );
        Ok(())
    }

    pub fn uses_pdt(&self) -> bool {
        self.playlist.as_ref().map(|p| p.has_pdt).unwrap_or(false) || self.latency.is_measured()
    }
}

/// `diagnostics/<slug>_<YYYYMMDD_HHMMSS>.json`
fn diagnostic_export_path(
    channel: Option<&str>,
    url: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> PathBuf {
    let slug = channel
        .map(slugify_label)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(slugify_label))
        })
        .unwrap_or_else(|| "stream".into());
    let stamp = now.format("%Y%m%d_%H%M%S");
    PathBuf::from(DIAGNOSTIC_DIR).join(format!("{slug}_{stamp}.json"))
}

fn slugify_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for ch in raw.chars() {
        let c = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else if ch.is_whitespace() || matches!(ch, '-' | '_' | '.' | '/') {
            '-'
        } else {
            continue;
        };
        if c == '-' {
            if prev_dash || out.is_empty() {
                continue;
            }
            prev_dash = true;
            out.push('-');
        } else {
            prev_dash = false;
            out.push(c);
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 48 {
        out.truncate(48);
        while out.ends_with('-') {
            out.pop();
        }
    }
    out
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().wrap_err("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).wrap_err("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).wrap_err("failed to create terminal")?;
    terminal.hide_cursor().ok();
    Ok(terminal)
}

pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
    Ok(())
}

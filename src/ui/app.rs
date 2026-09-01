use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use color_eyre::eyre::{eyre, Result, WrapErr};
use crossterm::cursor::Show;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::engine::poller::DiagnosticOpts;
use crate::engine::quick_play::{launch_quick_play, QuickPlayResult};
use crate::engine::ManifestPoller;
use crate::models::{
    format_dvr_window, AbrHealth, AbrVariant, AdBreakInfo, CdnStats, ChannelEntry, DiagCategory,
    DiagSeverity, DiagnosticFinding, DiagnosticSummary, DlDurHud, G2gMetrics, HealthReport,
    LatencyState, LogEntry, LogLevel, MultiCdnSkewReport, NetworkTiming, PlaylistMeta, RingBuffer,
    SegmentMetrics, SeiProbeResult, StreamEvent, StreamSnapshot, StreamStatus, SyntheticQoeSnapshot,
    Tr101290Report, VirtualBuffer, DIAGNOSTIC_DIR, EVENT_CHANNEL_CAPACITY, HISTORY_CAPACITY, LOG_CAPACITY,
};
use crate::ui::channel_picker::{ChannelPicker, PickerAction};
use crate::ui::layout::{self, DiagnosticPanel};
use crate::ui::render_cache::UiRenderCache;

const FRAME_PERIOD: Duration = Duration::from_millis(33);
const TOAST_SECS: u64 = 2;
const MANIFEST_HISTORY_CAP: usize = 10;

fn push_manifest_history(history: &mut Vec<PlaylistMeta>, meta: PlaylistMeta) {
    if history.last().map(|p| (p.media_sequence, p.url.as_str()))
        == Some((meta.media_sequence, meta.url.as_str()))
    {
        return;
    }
    if history.len() >= MANIFEST_HISTORY_CAP {
        history.remove(0);
    }
    history.push(meta);
}

static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Mark raw-mode / alternate-screen active (headless modes skip restore).
pub fn mark_terminal_active() {
    TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
}

/// Restore terminal only when TUI was started.
pub fn restore_terminal_global() {
    if TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, Show);
    }
}

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
    /// Optional DRM license / ClearKey / LA_URL TTFB probe (`--probe-drm`).
    pub probe_drm: bool,
    /// Staging ClearKey KID:KEY (`--clearkey`).
    pub clearkey: Option<String>,
    /// Optional incident export path (`--export-incident`).
    pub export_incident: Option<String>,
    pub webhook_url: Option<String>,
    pub alert_on: String,
    /// Bypass webhook SSRF checks (local tests only).
    pub allow_insecure_webhooks: bool,
    /// Bypass OTLP destination checks (local tests only).
    pub allow_insecure_otel: bool,
    /// OTLP trace export endpoint (e.g. http://127.0.0.1:4318).
    pub otel_endpoint: Option<String>,
    /// ETSI TR 101 290 P1/P2 MPEG-TS compliance (`--tr101290`).
    pub tr101290: bool,
    /// SEI/HDR/caption wire probe (`--probe-sei`).
    pub probe_sei: bool,
    /// Synthetic player QoE simulator (`--simulate-player`).
    pub simulate_player: bool,
    pub throttle_kbps: Option<u64>,
    pub simulated_rtt_ms: Option<u64>,
    /// Optional DNS-over-HTTPS provider (`--doh-provider`).
    pub doh_provider: Option<String>,
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
    pub dl_dur_hud: DlDurHud,
    pub health: HealthReport,
    pub cdn: CdnStats,
    pub abr_health: AbrHealth,
    pub active_ad: Option<AdBreakInfo>,
    pub buffer: VirtualBuffer,
    pub g2g: G2gMetrics,
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
    pub diagnostic_panel: DiagnosticPanel,
    pub tr101290: Tr101290Report,
    pub sei_probe: SeiProbeResult,
    pub synthetic_qoe: SyntheticQoeSnapshot,
    pub show_help: bool,
    /// Active regex filter for event log (`/` modal).
    pub log_filter: Option<String>,
    pub log_filter_regex: Option<regex::Regex>,
    pub log_filter_edit: bool,
    pub log_filter_draft: String,
    pub manifest_history: Vec<PlaylistMeta>,
    pub http_log: Vec<crate::models::HttpTransaction>,
    /// Toast message (curl copied, export path, …).
    pub toast: Option<(String, Instant)>,
    pub picker: Option<ChannelPicker>,
    pub session: SessionOpts,
    pub render_cache: UiRenderCache,
    pub transport: NetworkTiming,
    pub multi_cdn: Option<MultiCdnSkewReport>,
    rx: Receiver<StreamEvent>,
    tx: Sender<StreamEvent>,
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
        debug_assert_eq!(app.mode, UiMode::Picker);
        debug_assert!(app.poller.is_none());
        app.run().await
    }

    /// Single HLS/DASH URL → diagnostics dashboard + poller.
    pub async fn run_diagnostics(origin: String, url: String, session: SessionOpts) -> Result<()> {
        let app = Self::new(origin, Some(url), Vec::new(), session)?;
        debug_assert_eq!(app.mode, UiMode::Diagnostic);
        app.run().await
    }

    pub fn new(
        source_label: String,
        start_url: Option<String>,
        channels: Vec<ChannelEntry>,
        session: SessionOpts,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
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
            dl_dur_hud: DlDurHud::default(),
            health: HealthReport::perfect(),
            cdn: CdnStats::default(),
            abr_health: AbrHealth::default(),
            active_ad: None,
            buffer: VirtualBuffer::default(),
            g2g: G2gMetrics::default(),
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
            diagnostic_panel: DiagnosticPanel::None,
            tr101290: Tr101290Report::default(),
            sei_probe: SeiProbeResult::default(),
            synthetic_qoe: SyntheticQoeSnapshot::default(),
            show_help: false,
            log_filter: None,
            log_filter_regex: None,
            log_filter_edit: false,
            log_filter_draft: String::new(),
            manifest_history: Vec::new(),
            http_log: Vec::new(),
            toast: None,
            picker,
            session,
            render_cache: UiRenderCache::default(),
            transport: NetworkTiming::default(),
            multi_cdn: None,
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
        self.picker.as_ref().is_some_and(|p| !p.channels.is_empty())
    }

    fn spawn_poller(&mut self, url: String) -> Result<()> {
        if let Some(handle) = self.poller.take() {
            handle.abort();
        }
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        self.tx = tx.clone();
        self.rx = rx;
        self.source_url.clone_from(&url);
        self.active_url.clone_from(&url);

        let mut poller = ManifestPoller::new(
            url.as_str(),
            &self.session.headers,
            self.session.user_agent.as_deref(),
            self.session.interval_ms,
            self.session.probe_headers,
            self.session.probe_drm,
            tx,
        )
        .wrap_err("failed to start poller")?
        .with_diagnostics(&DiagnosticOpts {
            tr101290: self.session.tr101290,
            probe_sei: self.session.probe_sei,
            simulate_player: self.session.simulate_player,
            throttle_kbps: self.session.throttle_kbps,
            simulated_rtt_ms: self.session.simulated_rtt_ms,
        });
        if let Some(ref ck) = self.session.clearkey {
            if let Ok(spec) = crate::engine::drm_probe::ClearKeySpec::parse(ck) {
                poller = poller.with_clearkey(Some(spec));
            }
        }
        poller = crate::engine::session_poller::apply_session_doh(poller, &self.session)?;

        if let Some(hook_url) = self.session.webhook_url.clone() {
            let alerts = crate::engine::webhook::AlertKind::parse_list(&self.session.alert_on)?;
            let (hook_tx, hook_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
            poller = poller.with_webhook_tx(hook_tx);
            crate::engine::webhook::spawn_webhook_listener(
                crate::engine::webhook::WebhookConfig {
                    url: hook_url,
                    alerts,
                    allow_insecure: self.session.allow_insecure_webhooks,
                },
                hook_rx,
                url,
            );
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
        restore_terminal(&mut terminal);
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
                        Some(Ok(Event::Resize(_, _))) => {
                            self.render_cache.mark_dirty();
                            terminal.draw(|frame| self.draw_ui(frame))?;
                        }
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
        let url_width = frame.area().width.saturating_sub(14) as usize;
        UiRenderCache::take_and_rebuild(self, url_width);
        layout::draw(frame, self);
        if self.diagnostic_panel != DiagnosticPanel::None {
            layout::draw_diagnostic_panel(frame, frame.area(), self);
        }
        if self.overlay {
            if let Some(picker) = self.picker.as_mut() {
                picker.draw(frame, frame.area(), true);
            }
        }
        if self.show_help {
            layout::draw_help(frame, frame.area(), false);
        }
        if self.log_filter_edit {
            layout::draw_regex_modal(frame, frame.area(), self);
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

        if self.log_filter_edit {
            match key.code {
                KeyCode::Esc => {
                    self.log_filter_edit = false;
                    self.log_filter_draft.clear();
                    self.log_filter = None;
                    self.log_filter_regex = None;
                    self.log_scroll = 0;
                }
                KeyCode::Enter => {
                    let draft = self.log_filter_draft.trim().to_string();
                    if draft.is_empty() {
                        self.log_filter = None;
                        self.log_filter_regex = None;
                    } else if let Ok(re) = regex::Regex::new(&draft) {
                        self.log_filter = Some(draft);
                        self.log_filter_regex = Some(re);
                    } else {
                        self.log_filter_edit = false;
                        self.log_filter_draft.clear();
                        self.push_log(
                            LogLevel::Warn,
                            DiagCategory::Info,
                            "Invalid regex filter; cleared",
                        );
                        self.log_scroll = 0;
                        return Ok(());
                    }
                    self.log_filter_edit = false;
                    self.log_filter_draft.clear();
                    self.log_scroll = 0;
                }
                KeyCode::Backspace => {
                    self.log_filter_draft.pop();
                }
                KeyCode::Char(c) => {
                    self.log_filter_draft.push(c);
                }
                _ => {}
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q' | 'Q') => {
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
            KeyCode::Char('c' | 'C') => {
                self.copy_curl_to_clipboard();
            }
            KeyCode::Char('p' | 'P') => {
                self.quick_play();
            }
            KeyCode::Char('?') => {
                self.show_help = true;
            }
            KeyCode::Char(' ') => self.export_diagnostic()?,
            KeyCode::Char('e' | 'E') => self.export_incident()?,
            KeyCode::Char('f') => {
                self.log_filter = match self.log_filter.as_deref() {
                    None => Some("404".into()),
                    Some("404") => Some("SCTE".into()),
                    Some("SCTE") => Some("DRIFT".into()),
                    Some("DRIFT") => Some("CC_ERROR".into()),
                    _ => None,
                };
            }
            KeyCode::Char('F') => {
                self.log_filter = None;
                self.log_filter_regex = None;
            }
            KeyCode::Char('/') => {
                self.log_filter_edit = true;
                self.log_filter_draft = self.log_filter.clone().unwrap_or_default();
            }
            KeyCode::Char('r' | 'R') => self.reset_metrics(),
            KeyCode::Char('t' | 'T') => {
                self.diagnostic_panel = if self.diagnostic_panel == DiagnosticPanel::Tr101290 {
                    DiagnosticPanel::None
                } else {
                    DiagnosticPanel::Tr101290
                };
            }
            KeyCode::Char('s' | 'S') => {
                self.diagnostic_panel = if self.diagnostic_panel == DiagnosticPanel::Sei {
                    DiagnosticPanel::None
                } else {
                    DiagnosticPanel::Sei
                };
            }
            KeyCode::Char('y' | 'Y') => {
                self.diagnostic_panel = if self.diagnostic_panel == DiagnosticPanel::Qoe {
                    DiagnosticPanel::None
                } else {
                    DiagnosticPanel::Qoe
                };
            }
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

    fn copy_curl_to_clipboard(&mut self) {
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
                    "clipboard failed - see log".into(),
                    Instant::now() + Duration::from_secs(TOAST_SECS),
                ));
            }
        }
    }

    fn quick_play(&mut self) {
        let url = if self.active_url.is_empty() {
            self.source_url.as_str()
        } else {
            self.active_url.as_str()
        };
        if url.is_empty() {
            self.push_log(
                LogLevel::Warn,
                DiagCategory::Info,
                "[WARN] Quick Play failed: no active stream URL",
            );
            return;
        }
        match launch_quick_play(
            url,
            &self.session.headers,
            self.session.user_agent.as_deref(),
        ) {
            QuickPlayResult::Started { player } => {
                self.push_log(
                    LogLevel::Info,
                    DiagCategory::Info,
                    format!("Quick Play started with {player}"),
                );
                self.toast = Some((
                    format!("playing via {player}"),
                    Instant::now() + Duration::from_secs(TOAST_SECS),
                ));
            }
            QuickPlayResult::NotFound => {
                self.push_log(
                    LogLevel::Warn,
                    DiagCategory::Info,
                    "[WARN] Quick Play failed: Neither mpv nor ffplay found in system PATH",
                );
                self.toast = Some((
                    "mpv/ffplay not found".into(),
                    Instant::now() + Duration::from_secs(TOAST_SECS),
                ));
            }
            QuickPlayResult::SpawnFailed { player, error } => {
                self.push_log(
                    LogLevel::Warn,
                    DiagCategory::Info,
                    format!("[WARN] Quick Play failed: could not start {player}: {error}"),
                );
            }
        }
    }

    pub fn build_curl_command(&self) -> String {
        use crate::engine::export::{build_curl, ExportCapture};
        build_curl(&ExportCapture {
            manifest_url: self.active_url.clone(),
            segment_url: self.last_segment.as_ref().map(|s| s.uri.clone()),
            probe_headers: self.probe_mode || self.session.probe_headers,
            headers: self.session.headers.clone(),
            user_agent: self.session.user_agent.clone(),
            last_http_status: self.last_segment.as_ref().map(|s| s.http_status),
            last_ttfb_ms: self.last_segment.as_ref().map(|s| s.ttfb_ms),
            last_size_bytes: self.last_segment.as_ref().map(|s| s.size_bytes),
            ..Default::default()
        })
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
        self.dl_dur_hud.clear();
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
                self.active_url.clone_from(&meta.url);
                push_manifest_history(&mut self.manifest_history, meta.clone());
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
                self.dl_dur_hud.update_from_segment(&metrics);
                self.http_log.push(crate::models::HttpTransaction {
                    method: "GET".into(),
                    url: metrics.uri.clone(),
                    status: metrics.http_status,
                    ttfb_ms: metrics.ttfb_ms,
                    bytes: metrics.transferred_bytes,
                    cdn_provider: metrics.cdn.provider.clone(),
                });
                if self.http_log.len() > 100 {
                    self.http_log.remove(0);
                }
                self.last_segment = Some(metrics);
            }
            StreamEvent::LlHlsPart(p) => {
                if let Some(ratio) = p.part_dl_duration_ratio {
                    self.push_log(
                        LogLevel::Info,
                        DiagCategory::LlHls,
                        format!("part seq={} rtf={ratio:.2}", p.part_sequence),
                    );
                }
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
            StreamEvent::AdMarkerMismatch(m) => {
                self.push_log(
                    LogLevel::Warn,
                    DiagCategory::Ad,
                    format!("[MISMATCH] {}: {}", m.rule, m.message),
                );
                self.findings.push(crate::models::DiagnosticFinding {
                    category: DiagCategory::Ad,
                    severity: crate::models::DiagSeverity::Error,
                    rule: m.rule.clone(),
                    message: m.message,
                    reason: None,
                });
            }
            StreamEvent::InbandAdEvent(ev) => {
                let msg = ev.scte35_summary.clone().unwrap_or_else(|| {
                    format!("emsg id={} scheme={}", ev.emsg.id, ev.emsg.scheme_id_uri)
                });
                self.push_log(LogLevel::Info, DiagCategory::Ad, format!("[INBAND] {msg}"));
            }
            StreamEvent::Buffer(b) => self.buffer = b,
            StreamEvent::G2g(g) => self.g2g = g,
            StreamEvent::ProbeMode(on) => self.probe_mode = on,
            StreamEvent::Finding(f) => {
                if let Some(reason) = &f.reason {
                    let level = match f.severity {
                        DiagSeverity::Info => LogLevel::Info,
                        DiagSeverity::Warn => LogLevel::Warn,
                        DiagSeverity::Error => LogLevel::Error,
                    };
                    self.push_log(
                        level,
                        f.category,
                        format!("[{reason}] {}", f.message),
                    );
                }
                self.findings.push(f);
                if self.findings.len() > 100 {
                    let drain = self.findings.len() - 100;
                    self.findings.drain(0..drain);
                }
            }
            StreamEvent::WireProbe(_) => {}
            StreamEvent::Tr101290(r) => self.tr101290 = r,
            StreamEvent::SeiProbe(s) => self.sei_probe = s,
            StreamEvent::SyntheticQoe(q) => self.synthetic_qoe = q,
            StreamEvent::Log {
                level,
                category,
                message,
            } => self.push_log(level, category, message),
            StreamEvent::Error(message) => {
                self.push_log(LogLevel::Error, DiagCategory::Info, message);
            }
            StreamEvent::MultiCdnSkew(report) => {
                self.multi_cdn = Some(report);
                self.render_cache.mark_dirty();
            }
            StreamEvent::Transport(t) => {
                self.transport = t;
                self.render_cache.mark_dirty();
            }
        }
        self.render_cache.mark_dirty();
    }

    fn push_log(&mut self, level: LogLevel, category: DiagCategory, message: impl Into<String>) {
        self.log.push(LogEntry::make(level, category, message));
        if self.log.len() > LOG_CAPACITY {
            let drain = self.log.len() - LOG_CAPACITY;
            self.log.drain(0..drain);
        }
    }

    fn export_diagnostic(&mut self) -> Result<()> {
        let dvr = self.playlist.as_ref().map_or_else(
            || "n/a".into(),
            |p| format_dvr_window(p.window_segments, p.window_secs),
        );
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
                .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        });

        let title = channel.as_ref().map_or_else(
            || {
                format!(
                    "streamtop diagnostic @ {}",
                    now.format("%Y-%m-%d %H:%M:%S UTC")
                )
            },
            |name| {
                format!(
                    "{name} - diagnostic @ {}",
                    now.format("%Y-%m-%d %H:%M:%S UTC")
                )
            },
        );

        let timeline: Vec<String> = self
            .log
            .iter()
            .map(|e| crate::engine::redact::redact_text(&e.timeline_line()))
            .collect();

        let snapshot = StreamSnapshot {
            title,
            summary: DiagnosticSummary {
                channel: channel.clone(),
                captured_at: now,
                source_url: crate::engine::redact::redact_url(&self.source_url),
                active_url: crate::engine::redact::redact_url(&self.active_url),
                status: status.into(),
                health_score: health.score,
                health_label: health.label.clone(),
                latency: self.latency.display(),
                cdn: self.cdn.last.as_ref().map_or_else(
                    || "UNKNOWN".into(),
                    super::super::models::stream::CdnEdgeInfo::badge,
                ),
                dvr_window: dvr,
                buffer: self.buffer.display(),
                ll_hls: self.playlist.as_ref().is_some_and(|p| p.ll_hls.is_ll_hls),
                dropped_events: crate::engine::channel_stats::channel_dropped_total(),
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
        let json = crate::engine::redact::redact_text(&json);
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

    fn export_incident(&mut self) -> Result<()> {
        let now = chrono::Utc::now();
        let channel = self.channel_name.clone();
        let status = match self.status.kind {
            crate::models::StreamStatusKind::Live => "LIVE",
            crate::models::StreamStatusKind::Error => "ERROR",
            crate::models::StreamStatusKind::Degraded => "DEGRADED",
        };
        let health = self.health.clone();
        let title = format!(
            "streamtop incident @ {}",
            now.format("%Y-%m-%d %H:%M:%S UTC")
        );
        let timeline: Vec<String> = self
            .log
            .iter()
            .map(|e| crate::engine::redact::redact_text(&e.timeline_line()))
            .collect();
        let snapshot = StreamSnapshot {
            title,
            summary: DiagnosticSummary {
                channel,
                captured_at: now,
                source_url: crate::engine::redact::redact_url(&self.source_url),
                active_url: crate::engine::redact::redact_url(&self.active_url),
                status: status.into(),
                health_score: health.score,
                health_label: health.label.clone(),
                latency: self.latency.display(),
                cdn: self.cdn.last.as_ref().map_or_else(
                    || "UNKNOWN".into(),
                    super::super::models::stream::CdnEdgeInfo::badge,
                ),
                dvr_window: self.playlist.as_ref().map_or_else(
                    || "n/a".into(),
                    |p| format_dvr_window(p.window_segments, p.window_secs),
                ),
                buffer: self.buffer.display(),
                ll_hls: self.playlist.as_ref().is_some_and(|p| p.ll_hls.is_ll_hls),
                dropped_events: crate::engine::channel_stats::channel_dropped_total(),
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
        let report = crate::engine::incident::build_incident_report(
            snapshot,
            &self.manifest_history,
            &self.http_log,
            &self.session.headers,
            self.session.user_agent.as_deref(),
        );
        let path = self.session.export_incident.as_ref().map_or_else(
            || crate::engine::incident::incident_export_path(None, now),
            |p| {
                if p.is_empty() {
                    crate::engine::incident::incident_export_path(None, now)
                } else {
                    std::path::PathBuf::from(p)
                }
            },
        );
        crate::engine::incident::write_incident_report(&path, &report)?;
        let saved = path.display().to_string();
        self.toast = Some((
            format!("Incident {saved}"),
            Instant::now() + Duration::from_secs(TOAST_SECS),
        ));
        self.push_log(
            LogLevel::Info,
            DiagCategory::Info,
            format!("Incident report saved: {saved}"),
        );
        Ok(())
    }

    pub fn uses_pdt(&self) -> bool {
        self.playlist.as_ref().is_some_and(|p| p.has_pdt) || self.latency.is_measured()
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
    mark_terminal_active();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).wrap_err("failed to create terminal")?;
    terminal.hide_cursor().ok();
    Ok(terminal)
}

pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

#[cfg(test)]
impl App {
    pub(crate) fn minimal_for_render_cache_test() -> Self {
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            source_url: "https://example.com/live.m3u8".into(),
            active_url: "https://example.com/live.m3u8".into(),
            channel_name: None,
            status: StreamStatus::live("test"),
            latency: LatencyState::Unknown,
            playlist: None,
            variants: Vec::new(),
            last_segment: None,
            dl_dur_hud: DlDurHud::default(),
            health: HealthReport::perfect(),
            cdn: CdnStats::default(),
            abr_health: AbrHealth::default(),
            active_ad: None,
            buffer: VirtualBuffer::default(),
            g2g: G2gMetrics::default(),
            probe_mode: false,
            findings: Vec::new(),
            latency_history: RingBuffer::new(HISTORY_CAPACITY),
            ttfb_history: RingBuffer::new(HISTORY_CAPACITY),
            bitrate_history: RingBuffer::new(HISTORY_CAPACITY),
            transfer_history: RingBuffer::new(HISTORY_CAPACITY),
            log: Vec::new(),
            log_scroll: 0,
            should_quit: false,
            mode: UiMode::Diagnostic,
            overlay: false,
            diagnostic_panel: DiagnosticPanel::None,
            tr101290: Tr101290Report::default(),
            sei_probe: SeiProbeResult::default(),
            synthetic_qoe: SyntheticQoeSnapshot::default(),
            show_help: false,
            log_filter: None,
            log_filter_regex: None,
            log_filter_edit: false,
            log_filter_draft: String::new(),
            manifest_history: Vec::new(),
            http_log: Vec::new(),
            toast: None,
            picker: None,
            session: SessionOpts {
                headers: vec![],
                user_agent: None,
                interval_ms: None,
                probe_headers: false,
                probe_drm: false,
                clearkey: None,
                export_incident: None,
                webhook_url: None,
                alert_on: String::new(),
                allow_insecure_webhooks: false,
                allow_insecure_otel: false,
                otel_endpoint: None,
                tr101290: false,
                probe_sei: false,
                simulate_player: false,
                throttle_kbps: None,
                simulated_rtt_ms: None,
                doh_provider: None,
            },
            render_cache: UiRenderCache::default(),
            transport: NetworkTiming::default(),
            multi_cdn: None,
            rx,
            tx,
            poller: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn regex_filter_compiles_valid_pattern() {
        let re = regex::Regex::new(r"(?i)scte").unwrap();
        assert!(re.is_match("SCTE-35 cue"));
    }

    #[test]
    fn regex_filter_rejects_invalid_pattern() {
        let bad = format!("({}", "unclosed");
        assert!(regex::Regex::new(&bad).is_err());
    }
}

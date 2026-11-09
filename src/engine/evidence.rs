//! Opt-in evidence bundles written when a stall, continuity-counter error, or other alarm fires.
//!
//! Bundles are rate-limited and size-capped. Sensitive headers (`Authorization`, `Cookie`, and
//! similar) and secret-bearing URLs are redacted before anything is written.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use color_eyre::eyre::{eyre, Result};
use http::HeaderMap;
use serde::Serialize;

use crate::engine::agent::sanitize_stream_id;
use crate::engine::redact::{is_sensitive_header, redact_text, redact_url, REDACTED};
use crate::models::{
    DiagCategory, DiagSeverity, DiagnosticFinding, DiagnosticReasonCode, StreamEvent, StreamStatus,
    StreamStatusKind, STALL_TTFB_MS,
};

pub const EVIDENCE_SCHEMA: &str = "streamtop.evidence.v1";
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
pub const DEFAULT_MAX_SEGMENT_BYTES: u64 = 256 * 1024;
pub const DEFAULT_MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_BUNDLE_BYTES: u64 = 2 * 1024 * 1024;
pub const DEFAULT_MIN_INTERVAL_SECS: u64 = 30;
pub const DEFAULT_MAX_DUMPS: u32 = 32;
pub const DEFAULT_MAX_MANIFESTS: usize = 4;
pub const DEFAULT_MAX_SEGMENT_METAS: usize = 8;
/// Hard ceiling so a config typo cannot fill the disk.
pub const HARD_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
pub const HARD_MAX_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
pub const HARD_MAX_DUMPS: u32 = 500;
pub const HARD_MAX_SEGMENTS: u32 = 8;

#[derive(Debug, Clone, Default)]
pub struct EvidenceSettings {
    pub dir: Option<PathBuf>,
    pub segments: Option<u32>,
    pub max_total_bytes: Option<u64>,
    pub max_segment_bytes: Option<u64>,
    pub min_interval_secs: Option<u64>,
    pub max_dumps: Option<u32>,
}

impl EvidenceSettings {
    pub fn merge_override(&mut self, over: &Self) {
        if over.dir.is_some() {
            self.dir.clone_from(&over.dir);
        }
        if over.segments.is_some() {
            self.segments = over.segments;
        }
        if over.max_total_bytes.is_some() {
            self.max_total_bytes = over.max_total_bytes;
        }
        if over.max_segment_bytes.is_some() {
            self.max_segment_bytes = over.max_segment_bytes;
        }
        if over.min_interval_secs.is_some() {
            self.min_interval_secs = over.min_interval_secs;
        }
        if over.max_dumps.is_some() {
            self.max_dumps = over.max_dumps;
        }
    }

    /// `Ok(None)` when evidence is off. Errors when limits are set without a directory,
    /// or a limit is outside the hard ceiling.
    pub fn build(&self) -> Result<Option<EvidenceConfig>> {
        if self.dir.is_none() {
            let extras = self.segments.is_some()
                || self.max_total_bytes.is_some()
                || self.max_segment_bytes.is_some()
                || self.min_interval_secs.is_some()
                || self.max_dumps.is_some();
            if extras {
                return Err(eyre!(
                    "evidence limits require --evidence-dir or evidence_dir in the config"
                ));
            }
            return Ok(None);
        }
        let dir = self.dir.clone().unwrap_or_default();
        if dir.as_os_str().is_empty() {
            return Err(eyre!("evidence directory is empty"));
        }
        let segments = self.segments.unwrap_or(0);
        if segments > HARD_MAX_SEGMENTS {
            return Err(eyre!(
                "evidence segments must be 0..={HARD_MAX_SEGMENTS} (payloads are opt-in)"
            ));
        }
        let max_total_bytes = self.max_total_bytes.unwrap_or(DEFAULT_MAX_TOTAL_BYTES);
        if max_total_bytes == 0 || max_total_bytes > HARD_MAX_TOTAL_BYTES {
            return Err(eyre!(
                "evidence max bytes must be 1..={HARD_MAX_TOTAL_BYTES} ({HARD_MAX_TOTAL_BYTES} is 1 GiB)"
            ));
        }
        let max_segment_bytes = self
            .max_segment_bytes
            .unwrap_or(DEFAULT_MAX_SEGMENT_BYTES)
            .min(max_total_bytes);
        if self.max_segment_bytes.is_some()
            && (max_segment_bytes == 0 || max_segment_bytes > HARD_MAX_SEGMENT_BYTES)
        {
            return Err(eyre!(
                "evidence segment cap must be 1..={HARD_MAX_SEGMENT_BYTES}"
            ));
        }
        let min_interval_secs = self.min_interval_secs.unwrap_or(DEFAULT_MIN_INTERVAL_SECS);
        if min_interval_secs == 0 {
            return Err(eyre!(
                "evidence min interval must be at least 1 second (the byte and dump caps still apply)"
            ));
        }
        let max_dumps = self.max_dumps.unwrap_or(DEFAULT_MAX_DUMPS);
        if max_dumps == 0 || max_dumps > HARD_MAX_DUMPS {
            return Err(eyre!("evidence max dumps must be 1..={HARD_MAX_DUMPS}"));
        }
        let max_bundle_bytes = DEFAULT_MAX_BUNDLE_BYTES.min(max_total_bytes);
        Ok(Some(EvidenceConfig {
            dir,
            segments,
            max_total_bytes,
            max_segment_bytes,
            max_manifest_bytes: DEFAULT_MAX_MANIFEST_BYTES,
            max_bundle_bytes,
            max_manifests: DEFAULT_MAX_MANIFESTS,
            max_segment_metas: DEFAULT_MAX_SEGMENT_METAS,
            min_interval: Duration::from_secs(min_interval_secs),
            max_dumps,
        }))
    }
}

#[derive(Debug, Clone)]
pub struct EvidenceConfig {
    pub dir: PathBuf,
    pub segments: u32,
    pub max_total_bytes: u64,
    pub max_segment_bytes: u64,
    pub max_manifest_bytes: usize,
    pub max_bundle_bytes: u64,
    pub max_manifests: usize,
    pub max_segment_metas: usize,
    pub min_interval: Duration,
    pub max_dumps: u32,
}

impl EvidenceConfig {
    pub fn enabled(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            segments: 0,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_segment_bytes: DEFAULT_MAX_SEGMENT_BYTES,
            max_manifest_bytes: DEFAULT_MAX_MANIFEST_BYTES,
            max_bundle_bytes: DEFAULT_MAX_BUNDLE_BYTES,
            max_manifests: DEFAULT_MAX_MANIFESTS,
            max_segment_metas: DEFAULT_MAX_SEGMENT_METAS,
            min_interval: Duration::from_secs(DEFAULT_MIN_INTERVAL_SECS),
            max_dumps: DEFAULT_MAX_DUMPS,
        }
    }
}

#[derive(Debug)]
pub struct EvidenceBudget {
    total_bytes: u64,
    dumps: u32,
    max_total_bytes: u64,
    max_dumps: u32,
}

impl EvidenceBudget {
    pub fn new(max_total_bytes: u64, max_dumps: u32) -> Self {
        Self {
            total_bytes: 0,
            dumps: 0,
            max_total_bytes,
            max_dumps,
        }
    }

    fn try_reserve(&mut self, bytes: u64) -> bool {
        if self.dumps >= self.max_dumps {
            return false;
        }
        if self.total_bytes.saturating_add(bytes) > self.max_total_bytes {
            return false;
        }
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        self.dumps = self.dumps.saturating_add(1);
        true
    }

    fn rollback(&mut self, bytes: u64) {
        self.total_bytes = self.total_bytes.saturating_sub(bytes);
        self.dumps = self.dumps.saturating_sub(1);
    }
}

/// Shared config and process-wide byte/dump budget.
#[derive(Clone)]
pub struct EvidenceHandle {
    pub config: EvidenceConfig,
    pub budget: Arc<Mutex<EvidenceBudget>>,
    pub label: Option<String>,
}

impl EvidenceHandle {
    pub fn new(config: EvidenceConfig) -> Self {
        let budget = EvidenceBudget::new(config.max_total_bytes, config.max_dumps);
        Self {
            budget: Arc::new(Mutex::new(budget)),
            config,
            label: None,
        }
    }

    pub fn from_settings(settings: &EvidenceSettings) -> Result<Option<Self>> {
        Ok(settings.build()?.map(Self::new))
    }
}

impl std::fmt::Debug for EvidenceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvidenceHandle")
            .field("dir", &self.config.dir)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlarmKind {
    Stall,
    Continuity,
    Other,
}

impl AlarmKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stall => "stall",
            Self::Continuity => "continuity",
            Self::Other => "alarm",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alarm {
    pub kind: AlarmKind,
    pub rule: String,
    pub message: String,
}

impl Alarm {
    pub fn summary(&self) -> String {
        format!("{}: {}", self.kind.as_str(), self.message)
    }
}

#[derive(Debug, Clone, Default)]
pub struct AlarmState {
    pub last_cc: u32,
}

/// Classify one poller event. Continuity alarms fire when the TR 101 290 counter increases.
pub fn alarm_from_event(event: &StreamEvent, state: &mut AlarmState) -> Option<Alarm> {
    match event {
        StreamEvent::Finding(f) => finding_alarm(f),
        StreamEvent::Tr101290(report) => {
            if report.cc_errors > state.last_cc {
                let delta = report.cc_errors - state.last_cc;
                state.last_cc = report.cc_errors;
                return Some(Alarm {
                    kind: AlarmKind::Continuity,
                    rule: "P1_CC".into(),
                    message: format!(
                        "{delta} new continuity-counter errors (total {})",
                        report.cc_errors
                    ),
                });
            }
            None
        }
        StreamEvent::Segment(seg) => {
            if seg.http_status >= 500 {
                return Some(Alarm {
                    kind: AlarmKind::Other,
                    rule: "HTTP_5XX".into(),
                    message: format!("segment HTTP {}", seg.http_status),
                });
            }
            if seg.ttfb_ms >= STALL_TTFB_MS {
                return Some(Alarm {
                    kind: AlarmKind::Stall,
                    rule: "TTFB_STALL".into(),
                    message: format!("segment TTFB {} ms", seg.ttfb_ms),
                });
            }
            None
        }
        StreamEvent::Buffer(buf) if buf.stall_risk_pct >= 80 => Some(Alarm {
            kind: AlarmKind::Stall,
            rule: "BUFFER_STALL".into(),
            message: format!("virtual buffer stall risk {}%", buf.stall_risk_pct),
        }),
        StreamEvent::Health(health) => {
            let stall = health
                .deductions
                .iter()
                .any(|d| d.to_ascii_lowercase().contains("stall"));
            if stall {
                Some(Alarm {
                    kind: AlarmKind::Stall,
                    rule: "HEALTH_STALL".into(),
                    message: format!("health {} ({})", health.score, health.deductions.join("; ")),
                })
            } else {
                None
            }
        }
        StreamEvent::Status(status) => status_alarm(status),
        StreamEvent::Error(message) => Some(Alarm {
            kind: AlarmKind::Other,
            rule: "STREAM_ERROR".into(),
            message: message.clone(),
        }),
        StreamEvent::AdMarkerMismatch(m) => Some(Alarm {
            kind: AlarmKind::Other,
            rule: m.rule.clone(),
            message: m.message.clone(),
        }),
        _ => None,
    }
}

fn finding_alarm(f: &DiagnosticFinding) -> Option<Alarm> {
    if is_continuity_finding(f) {
        return Some(Alarm {
            kind: AlarmKind::Continuity,
            rule: f.rule.clone(),
            message: f.message.clone(),
        });
    }
    if is_stall_finding(f) {
        return Some(Alarm {
            kind: AlarmKind::Stall,
            rule: f.rule.clone(),
            message: f.message.clone(),
        });
    }
    if f.severity == DiagSeverity::Error {
        return Some(Alarm {
            kind: AlarmKind::Other,
            rule: f.rule.clone(),
            message: f.message.clone(),
        });
    }
    None
}

fn is_continuity_finding(f: &DiagnosticFinding) -> bool {
    f.rule == "P1_CC"
        || f.reason.as_deref() == Some(DiagnosticReasonCode::ErrTr101290CcError.as_str())
        || f.message
            .to_ascii_lowercase()
            .contains("continuity counter")
}

fn is_stall_finding(f: &DiagnosticFinding) -> bool {
    f.category == DiagCategory::Stalling
        || f.rule.to_ascii_uppercase().contains("STALL")
        || f.reason.as_deref() == Some(DiagnosticReasonCode::ErrRtfStallRisk.as_str())
        || f.reason.as_deref() == Some(DiagnosticReasonCode::ErrPartRtfStall.as_str())
}

fn status_alarm(status: &StreamStatus) -> Option<Alarm> {
    let lower = status.message.to_ascii_lowercase();
    if lower.contains("stall") {
        return Some(Alarm {
            kind: AlarmKind::Stall,
            rule: "STATUS".into(),
            message: status.message.clone(),
        });
    }
    if status.kind == StreamStatusKind::Error {
        return Some(Alarm {
            kind: AlarmKind::Other,
            rule: "STATUS".into(),
            message: status.message.clone(),
        });
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeKind {
    Manifest,
    Segment,
}

impl ExchangeKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Segment => "segment",
        }
    }
}

#[derive(Debug, Clone)]
struct StoredExchange {
    kind: ExchangeKind,
    url: String,
    status: u16,
    ttfb_ms: u64,
    download_ms: u64,
    bytes: u64,
    truncated: bool,
    captured_at: String,
    request_headers: Vec<(String, String)>,
    response_headers: Vec<(String, String)>,
    body: Vec<u8>,
    body_ext: String,
}

struct RecorderInner {
    manifests: Vec<StoredExchange>,
    segments: Vec<StoredExchange>,
    alarm_state: AlarmState,
    last_dump: HashMap<AlarmKind, Instant>,
    warned_cap: bool,
}

/// Per-stream ring of recent playlists and segment requests.
pub struct EvidenceRecorder {
    handle: EvidenceHandle,
    label: String,
    url: String,
    inner: Mutex<RecorderInner>,
}

impl EvidenceRecorder {
    pub fn new(handle: EvidenceHandle, url: String) -> Self {
        let label = handle
            .label
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| label_from_url(&url));
        Self {
            handle,
            label: sanitize_stream_id(&label),
            url,
            inner: Mutex::new(RecorderInner {
                manifests: Vec::new(),
                segments: Vec::new(),
                alarm_state: AlarmState::default(),
                last_dump: HashMap::new(),
                warned_cap: false,
            }),
        }
    }

    pub fn saves_segment_bytes(&self) -> bool {
        self.handle.config.segments > 0
    }

    pub fn record(&self, note: &ExchangeNote<'_>) {
        let stored = store_note(note, &self.handle.config);
        let mut guard = lock_inner(&self.inner);
        match stored.kind {
            ExchangeKind::Manifest => {
                push_cap(
                    &mut guard.manifests,
                    stored,
                    self.handle.config.max_manifests,
                );
            }
            ExchangeKind::Segment => {
                push_cap(
                    &mut guard.segments,
                    stored,
                    self.handle.config.max_segment_metas,
                );
            }
        }
    }

    pub fn on_event(&self, event: &StreamEvent) {
        let alarm = {
            let mut guard = lock_inner(&self.inner);
            alarm_from_event(event, &mut guard.alarm_state)
        };
        let Some(alarm) = alarm else {
            return;
        };
        self.dump(&alarm);
    }

    fn dump(&self, alarm: &Alarm) {
        let (manifests, segments) = {
            let mut guard = lock_inner(&self.inner);
            if !cooled_down(&guard, alarm.kind, self.handle.config.min_interval) {
                return;
            }
            guard.last_dump.insert(alarm.kind, Instant::now());
            (guard.manifests.clone(), guard.segments.clone())
        };
        let files = plan_bundle(
            &self.handle.config,
            &self.label,
            &redact_url(&self.url),
            alarm,
            &manifests,
            &segments,
        );
        let total: u64 = files.iter().map(|f| f.bytes.len() as u64).sum();
        if !reserve_budget(&self.handle.budget, total) {
            self.warn_cap(alarm);
            lock_inner(&self.inner).last_dump.remove(&alarm.kind);
            return;
        }
        let dir = unique_dir(
            &self.handle.config.dir,
            &format!(
                "{}_{}_{}",
                Utc::now().format("%Y%m%dT%H%M%SZ"),
                alarm.kind.as_str(),
                self.label
            ),
        );
        if let Err(err) = write_bundle(&dir, &files) {
            rollback_budget(&self.handle.budget, total);
            lock_inner(&self.inner).last_dump.remove(&alarm.kind);
            eprintln!(
                "streamtop evidence: could not write {}: {err}",
                dir.display()
            );
            return;
        }
        eprintln!(
            "streamtop evidence: wrote {} ({total} bytes, {})",
            dir.display(),
            alarm.kind.as_str()
        );
    }

    fn warn_cap(&self, alarm: &Alarm) {
        {
            let mut guard = lock_inner(&self.inner);
            if guard.warned_cap {
                return;
            }
            guard.warned_cap = true;
        }
        eprintln!(
            "streamtop evidence: skipped {} for {}; byte cap {} or dump cap {} reached",
            alarm.kind.as_str(),
            self.label,
            self.handle.config.max_total_bytes,
            self.handle.config.max_dumps
        );
    }
}

fn cooled_down(inner: &RecorderInner, kind: AlarmKind, interval: Duration) -> bool {
    inner
        .last_dump
        .get(&kind)
        .is_none_or(|prev| prev.elapsed() >= interval)
}

fn lock_inner(inner: &Mutex<RecorderInner>) -> std::sync::MutexGuard<'_, RecorderInner> {
    inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_budget(budget: &Mutex<EvidenceBudget>) -> std::sync::MutexGuard<'_, EvidenceBudget> {
    budget
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn reserve_budget(budget: &Mutex<EvidenceBudget>, bytes: u64) -> bool {
    lock_budget(budget).try_reserve(bytes)
}

fn rollback_budget(budget: &Mutex<EvidenceBudget>, bytes: u64) {
    lock_budget(budget).rollback(bytes);
}

pub struct ExchangeNote<'a> {
    pub kind: ExchangeKind,
    pub url: &'a str,
    pub status: u16,
    pub ttfb_ms: u64,
    pub download_ms: u64,
    pub bytes: u64,
    pub request_headers: &'a [(String, String)],
    pub response_headers: &'a [(String, String)],
    pub body: &'a [u8],
}

pub fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let text = value.to_str().unwrap_or("[binary]");
            (name.as_str().to_string(), text.to_string())
        })
        .collect()
}

fn store_note(note: &ExchangeNote<'_>, cfg: &EvidenceConfig) -> StoredExchange {
    let (cap, keep_body) = match note.kind {
        ExchangeKind::Manifest => (cfg.max_manifest_bytes, true),
        ExchangeKind::Segment => (cfg.max_segment_bytes as usize, cfg.segments > 0),
    };
    let truncated = keep_body && note.body.len() > cap;
    let slice = if keep_body {
        &note.body[..note.body.len().min(cap)]
    } else {
        &[]
    };
    let body = if note.kind == ExchangeKind::Manifest {
        std::str::from_utf8(slice)
            .map_or_else(|_| slice.to_vec(), |text| redact_text(text).into_bytes())
    } else {
        slice.to_vec()
    };
    let body_ext = body_extension(note.kind, &body);
    StoredExchange {
        kind: note.kind,
        url: redact_url(note.url),
        status: note.status,
        ttfb_ms: note.ttfb_ms,
        download_ms: note.download_ms,
        bytes: note.bytes,
        truncated,
        captured_at: Utc::now().to_rfc3339(),
        request_headers: redact_pairs(note.request_headers),
        response_headers: redact_pairs(note.response_headers),
        body,
        body_ext,
    }
}

fn redact_pairs(pairs: &[(String, String)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| {
            if is_sensitive_header(name) {
                (name.clone(), REDACTED.to_string())
            } else {
                (name.clone(), value.clone())
            }
        })
        .collect()
}

fn body_extension(kind: ExchangeKind, body: &[u8]) -> String {
    if kind == ExchangeKind::Segment {
        return "bin".into();
    }
    let head = String::from_utf8_lossy(&body[..body.len().min(256)]);
    if head.contains("#EXTM3U") || head.contains("#EXT-X-") {
        "m3u8".into()
    } else if head.contains("<MPD") || head.contains("<mpd") {
        "mpd".into()
    } else {
        "txt".into()
    }
}

fn push_cap(buf: &mut Vec<StoredExchange>, item: StoredExchange, cap: usize) {
    if cap == 0 {
        return;
    }
    if buf.len() >= cap {
        buf.remove(0);
    }
    buf.push(item);
}

pub fn label_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let tail = path
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("stream");
    let clean = sanitize_stream_id(tail);
    if clean.len() > 32 {
        clean[..32].to_string()
    } else {
        clean
    }
}

struct PlannedFile {
    rel: String,
    bytes: Vec<u8>,
}

fn plan_bundle(
    cfg: &EvidenceConfig,
    label: &str,
    url: &str,
    alarm: &Alarm,
    manifests: &[StoredExchange],
    segments: &[StoredExchange],
) -> Vec<PlannedFile> {
    let mut manifests: Vec<StoredExchange> = manifests.to_vec();
    let mut segments: Vec<StoredExchange> = segments.to_vec();
    let keep = cfg.segments as usize;
    if segments.len() > keep {
        let drop_n = segments.len() - keep;
        for seg in segments.iter_mut().take(drop_n) {
            seg.body.clear();
        }
    }
    if keep == 0 {
        for seg in &mut segments {
            seg.body.clear();
        }
    }
    loop {
        let files = render_files(label, url, alarm, &manifests, &segments);
        let size: u64 = files.iter().map(|f| f.bytes.len() as u64).sum();
        if size <= cfg.max_bundle_bytes {
            return files;
        }
        if let Some(seg) = segments.iter_mut().find(|s| !s.body.is_empty()) {
            seg.body.clear();
            seg.truncated = true;
            continue;
        }
        if manifests.len() > 1 {
            manifests.remove(0);
            continue;
        }
        if let Some(man) = manifests.first_mut() {
            if man.body.len() > 1024 {
                man.body.truncate(1024);
                man.truncated = true;
                continue;
            }
        }
        if !segments.is_empty() {
            segments.remove(0);
            continue;
        }
        return files;
    }
}

fn render_files(
    label: &str,
    url: &str,
    alarm: &Alarm,
    manifests: &[StoredExchange],
    segments: &[StoredExchange],
) -> Vec<PlannedFile> {
    let mut files = Vec::new();
    let mut manifest_paths = Vec::new();
    let mut segment_paths = Vec::new();
    for (i, item) in manifests.iter().enumerate() {
        let n = i + 1;
        let meta_rel = format!("manifests/{n:03}.meta.json");
        let body_rel = if item.body.is_empty() {
            None
        } else {
            Some(format!("manifests/{n:03}.{}", item.body_ext))
        };
        if let Some(rel) = &body_rel {
            files.push(PlannedFile {
                rel: rel.clone(),
                bytes: item.body.clone(),
            });
        }
        files.push(PlannedFile {
            rel: meta_rel.clone(),
            bytes: meta_json(item, body_rel.as_deref()),
        });
        manifest_paths.push(meta_rel);
    }
    for (i, item) in segments.iter().enumerate() {
        let n = i + 1;
        let meta_rel = format!("segments/{n:03}.meta.json");
        let body_rel = if item.body.is_empty() {
            None
        } else {
            Some(format!("segments/{n:03}.{}", item.body_ext))
        };
        if let Some(rel) = &body_rel {
            files.push(PlannedFile {
                rel: rel.clone(),
                bytes: item.body.clone(),
            });
        }
        files.push(PlannedFile {
            rel: meta_rel.clone(),
            bytes: meta_json(item, body_rel.as_deref()),
        });
        segment_paths.push(meta_rel);
    }
    let index = BundleIndex {
        schema: EVIDENCE_SCHEMA,
        stream_id: label,
        url,
        alarm_kind: alarm.kind.as_str(),
        rule: &alarm.rule,
        message: &alarm.message,
        captured_at: Utc::now().to_rfc3339(),
        manifests: manifest_paths,
        segments: segment_paths,
    };
    let mut index_bytes = serde_json::to_vec_pretty(&index).unwrap_or_else(|_| b"{}".to_vec());
    index_bytes.push(b'\n');
    files.push(PlannedFile {
        rel: "bundle.json".into(),
        bytes: index_bytes,
    });
    files
}

fn meta_json(item: &StoredExchange, body_rel: Option<&str>) -> Vec<u8> {
    let meta = ExchangeMeta {
        kind: item.kind.as_str(),
        method: "GET",
        url: &item.url,
        status: item.status,
        ttfb_ms: item.ttfb_ms,
        download_ms: item.download_ms,
        bytes: item.bytes,
        truncated: item.truncated,
        captured_at: &item.captured_at,
        request_headers: header_json(&item.request_headers),
        response_headers: header_json(&item.response_headers),
        body_file: body_rel,
    };
    let mut bytes = serde_json::to_vec_pretty(&meta).unwrap_or_else(|_| b"{}".to_vec());
    bytes.push(b'\n');
    bytes
}

fn header_json(pairs: &[(String, String)]) -> Vec<HeaderJson<'_>> {
    pairs
        .iter()
        .map(|(name, value)| HeaderJson { name, value })
        .collect()
}

#[derive(Serialize)]
struct HeaderJson<'a> {
    name: &'a str,
    value: &'a str,
}

#[derive(Serialize)]
struct ExchangeMeta<'a> {
    kind: &'a str,
    method: &'static str,
    url: &'a str,
    status: u16,
    ttfb_ms: u64,
    download_ms: u64,
    bytes: u64,
    truncated: bool,
    captured_at: &'a str,
    request_headers: Vec<HeaderJson<'a>>,
    response_headers: Vec<HeaderJson<'a>>,
    body_file: Option<&'a str>,
}

#[derive(Serialize)]
struct BundleIndex<'a> {
    schema: &'a str,
    stream_id: &'a str,
    url: &'a str,
    alarm_kind: &'a str,
    rule: &'a str,
    message: &'a str,
    captured_at: String,
    manifests: Vec<String>,
    segments: Vec<String>,
}

fn unique_dir(root: &Path, stem: &str) -> PathBuf {
    let mut path = root.join(stem);
    let mut n = 1u32;
    while path.exists() && n < 1000 {
        path = root.join(format!("{stem}_{n}"));
        n += 1;
    }
    path
}

fn write_bundle(dir: &Path, files: &[PlannedFile]) -> Result<()> {
    fs::create_dir_all(dir)?;
    for file in files {
        let path = dir.join(&file.rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, &file.bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        AdMarkerMismatch, CdnEdgeInfo, ContainerKind, HealthReport, SegmentMetrics, Tr101290Report,
        VirtualBuffer,
    };

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "streamtop-evidence-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_millis()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn recorder(dir: &Path, mut cfg: EvidenceConfig) -> EvidenceRecorder {
        cfg.dir = dir.to_path_buf();
        let handle = EvidenceHandle::new(cfg);
        EvidenceRecorder::new(
            handle,
            "https://cdn.example/live.m3u8?token=supersecret".into(),
        )
    }

    fn note_manifest<'a>(url: &'a str, body: &'a str) -> ExchangeNote<'a> {
        ExchangeNote {
            kind: ExchangeKind::Manifest,
            url,
            status: 200,
            ttfb_ms: 12,
            download_ms: 20,
            bytes: body.len() as u64,
            request_headers: &[],
            response_headers: &[],
            body: body.as_bytes(),
        }
    }

    fn stall_event() -> StreamEvent {
        StreamEvent::Finding(DiagnosticFinding {
            category: DiagCategory::Stalling,
            severity: DiagSeverity::Warn,
            rule: "RTF_STALL".into(),
            message: "buffer drain".into(),
            reason: Some(DiagnosticReasonCode::ErrRtfStallRisk.as_str().into()),
        })
    }

    fn segment(status: u16, ttfb: u64) -> SegmentMetrics {
        SegmentMetrics {
            media_sequence: 1,
            duration_secs: 2.0,
            size_bytes: 1000,
            transferred_bytes: 1000,
            ttfb_ms: ttfb,
            download_ms: ttfb,
            dl_to_dur_ratio: None,
            download_kbps: Some(4500),
            latency_ms: Some(2100),
            uri: "https://cdn.example/1.ts".into(),
            cdn: CdnEdgeInfo::default(),
            probed: false,
            container: ContainerKind::Ts,
            http_status: status,
            network: None,
            wire: None,
        }
    }

    fn dirs(root: &Path) -> Vec<PathBuf> {
        fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    }

    fn read_tree(root: &Path) -> String {
        let mut out = String::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_none_or(|ext| ext != "bin") {
                    out.push_str(&fs::read_to_string(&path).unwrap_or_default());
                    out.push('\n');
                }
            }
        }
        out
    }

    #[test]
    fn stall_and_continuity_and_other_classify() {
        let mut state = AlarmState::default();
        let stall = alarm_from_event(&stall_event(), &mut state).unwrap();
        assert_eq!(stall.kind, AlarmKind::Stall);
        assert!(alarm_from_event(
            &StreamEvent::Log {
                level: crate::models::LogLevel::Info,
                category: DiagCategory::Info,
                message: "polling".into(),
            },
            &mut state
        )
        .is_none());
        assert!(alarm_from_event(
            &StreamEvent::Status(StreamStatus::live("Polling")),
            &mut state
        )
        .is_none());

        let report = Tr101290Report {
            cc_errors: 2,
            ..Tr101290Report::default()
        };
        let cc = alarm_from_event(&StreamEvent::Tr101290(report.clone()), &mut state).unwrap();
        assert_eq!(cc.kind, AlarmKind::Continuity);
        assert!(alarm_from_event(&StreamEvent::Tr101290(report), &mut state).is_none());

        let http = alarm_from_event(&StreamEvent::Segment(segment(503, 10)), &mut state).unwrap();
        assert_eq!(http.kind, AlarmKind::Other);
        let ttfb = alarm_from_event(&StreamEvent::Segment(segment(200, 3000)), &mut state).unwrap();
        assert_eq!(ttfb.kind, AlarmKind::Stall);

        let mut health = HealthReport::perfect();
        assert!(alarm_from_event(&StreamEvent::Health(health.clone()), &mut state).is_none());
        health.deductions.push("origin stall".into());
        assert_eq!(
            alarm_from_event(&StreamEvent::Health(health), &mut state)
                .unwrap()
                .kind,
            AlarmKind::Stall
        );

        let buf = VirtualBuffer {
            stall_risk_pct: 90,
            ..VirtualBuffer::default()
        };
        assert_eq!(
            alarm_from_event(&StreamEvent::Buffer(buf), &mut state)
                .unwrap()
                .kind,
            AlarmKind::Stall
        );

        let mismatch = alarm_from_event(
            &StreamEvent::AdMarkerMismatch(AdMarkerMismatch {
                rule: "DAI".into(),
                message: "cue drift".into(),
                manifest_kind: "CUE-OUT".into(),
                planned_duration_secs: None,
                wire_pts_ms: None,
                drift_ms: None,
            }),
            &mut state,
        )
        .unwrap();
        assert_eq!(mismatch.kind, AlarmKind::Other);
    }

    #[test]
    fn settings_reject_limits_without_dir_and_huge_caps() {
        let mut settings = EvidenceSettings {
            segments: Some(2),
            ..EvidenceSettings::default()
        };
        assert!(settings.build().is_err());
        settings.dir = Some(PathBuf::from("evidence"));
        settings.segments = Some(99);
        assert!(settings.build().is_err());
        settings.segments = Some(2);
        settings.max_total_bytes = Some(HARD_MAX_TOTAL_BYTES + 1);
        assert!(settings.build().is_err());
        settings.max_total_bytes = Some(1024);
        settings.min_interval_secs = Some(0);
        assert!(settings.build().is_err());
        settings.min_interval_secs = Some(5);
        let cfg = settings.build().unwrap().unwrap();
        assert_eq!(cfg.segments, 2);
        assert_eq!(cfg.min_interval, Duration::from_secs(5));
    }

    #[test]
    fn bundle_redacts_headers_and_urls_and_saves_manifest() {
        let dir = temp_dir("redact");
        let mut cfg = EvidenceConfig::enabled(&dir);
        cfg.min_interval = Duration::from_secs(1);
        cfg.segments = 1;
        cfg.max_segment_bytes = 4;
        let rec = recorder(&dir, cfg);
        let req = vec![
            (
                "Authorization".into(),
                "Bearer supersecret-evidence-token".into(),
            ),
            ("X-Custom".into(), "ok".into()),
        ];
        let resp = vec![
            (
                "Set-Cookie".into(),
                "session=supersecret-evidence-token".into(),
            ),
            (
                "Content-Type".into(),
                "application/vnd.apple.mpegurl".into(),
            ),
        ];
        let playlist = "#EXTM3U\nhttps://cdn.example/a.ts?token=supersecret-evidence-token\nseg.ts?token=supersecret-evidence-token\n";
        rec.record(&ExchangeNote {
            kind: ExchangeKind::Manifest,
            url: "https://user:supersecret-evidence-token@cdn.example/live.m3u8?token=abc",
            status: 200,
            ttfb_ms: 5,
            download_ms: 9,
            bytes: playlist.len() as u64,
            request_headers: &req,
            response_headers: &resp,
            body: playlist.as_bytes(),
        });
        rec.record(&ExchangeNote {
            kind: ExchangeKind::Segment,
            url: "https://cdn.example/seg.ts?token=supersecret-evidence-token",
            status: 206,
            ttfb_ms: 4,
            download_ms: 8,
            bytes: 100,
            request_headers: &req,
            response_headers: &resp,
            body: b"0123456789abcdefghijklmnopqrstuvwxyz",
        });
        rec.on_event(&stall_event());
        let written = dirs(&dir);
        assert_eq!(written.len(), 1, "expected one bundle dir");
        let text = read_tree(&written[0]);
        assert!(!text.contains("supersecret-evidence-token"), "{text}");
        assert!(text.contains(REDACTED), "{text}");
        assert!(text.contains("X-Custom"), "{text}");
        assert!(text.contains("stall"), "{text}");
        assert!(text.contains(EVIDENCE_SCHEMA), "{text}");
        let bin = fs::read(written[0].join("segments/001.bin")).unwrap();
        assert_eq!(bin, b"0123");
        let meta: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(written[0].join("segments/001.meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["truncated"], true);
        assert_eq!(meta["status"], 206);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rate_limit_is_per_alarm_kind_and_dump_cap_stops_writes() {
        let dir = temp_dir("rate");
        let mut cfg = EvidenceConfig::enabled(&dir);
        cfg.min_interval = Duration::from_secs(3600);
        cfg.max_dumps = 2;
        let rec = recorder(&dir, cfg);
        rec.on_event(&stall_event());
        rec.on_event(&stall_event());
        let report = Tr101290Report {
            cc_errors: 1,
            ..Tr101290Report::default()
        };
        rec.on_event(&StreamEvent::Tr101290(report));
        let written = dirs(&dir);
        assert_eq!(
            written.len(),
            2,
            "stall once plus continuity, second stall suppressed"
        );
        rec.on_event(&StreamEvent::Error("HTTP 500".into()));
        assert_eq!(dirs(&dir).len(), 2, "dump cap reached");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn byte_cap_writes_nothing_when_the_bundle_cannot_fit() {
        let dir = temp_dir("bytes");
        let mut cfg = EvidenceConfig::enabled(&dir);
        cfg.min_interval = Duration::from_secs(1);
        cfg.max_total_bytes = 1;
        cfg.max_bundle_bytes = 1;
        cfg.max_dumps = 4;
        let rec = recorder(&dir, cfg);
        rec.record(&note_manifest(
            "https://cdn.example/live.m3u8",
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n",
        ));
        rec.on_event(&stall_event());
        assert_eq!(dirs(&dir), Vec::<PathBuf>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_body_is_truncated_to_the_cap() {
        let dir = temp_dir("trunc");
        let mut cfg = EvidenceConfig::enabled(&dir);
        cfg.min_interval = Duration::from_secs(1);
        cfg.max_manifest_bytes = 16;
        let rec = recorder(&dir, cfg);
        rec.record(&note_manifest(
            "https://cdn.example/live.m3u8",
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nseg.ts\n",
        ));
        rec.on_event(&stall_event());
        let written = dirs(&dir);
        assert_eq!(written.len(), 1);
        let playlist = fs::read(written[0].join("manifests/001.m3u8")).unwrap();
        assert!(playlist.len() <= 16, "{}", playlist.len());
        let meta: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(written[0].join("manifests/001.meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["truncated"], true);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn label_from_url_uses_the_last_path_segment() {
        assert_eq!(
            label_from_url("https://cdn.example/live/master.m3u8?token=abc"),
            "master_m3u8"
        );
    }
}

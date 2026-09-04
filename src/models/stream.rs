use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const HISTORY_CAPACITY: usize = 60;
pub const LOG_CAPACITY: usize = 200;
pub const DIAGNOSTIC_DIR: &str = "diagnostics";
pub const DIAGNOSTIC_PATH: &str = "streamtop_diagnostic.json";
pub const HLS_LIVE_EDGE_SEGMENTS: u64 = 3;
pub const TARGET_DURATION_SLACK_SECS: f32 = 0.5;
pub const STALL_MULTIPLIER: f64 = 1.5;
pub const TTFB_SPIKE_MS: u64 = 500;
pub const BUFFER_STALL_THRESHOLD_SECS: f64 = 4.0;
pub const RANGE_PROBE_BYTES: u64 = 2048;
/// Bytes fetched for wire probe (SPS/PPS / moov / PAT-PMT).
pub const DEEP_WIRE_PROBE_BYTES: u64 = 65535;
/// Bounded poller->UI/webhook queue; full = drop (prefer latest via try_send).
pub const EVENT_CHANNEL_CAPACITY: usize = 512;
pub const AUDIT_REPORT_JSON: &str = "audit_report.json";
pub const AUDIT_REPORT_CSV: &str = "audit_report.csv";
pub const AUDIT_CONCURRENCY: usize = 25;
pub const AUDIT_CONNECT_TIMEOUT_SECS: u64 = 3;
pub const AUDIT_REQUEST_TIMEOUT_SECS: u64 = 5;
/// Manifest/segment probe connect timeout.
pub const PROBE_CONNECT_TIMEOUT_SECS: u64 = 3;
/// Per-request read timeout for manifest and segment probes.
pub const PROBE_READ_TIMEOUT_SECS: u64 = 4;
pub const STALL_TTFB_MS: u64 = 2500;
/// Download-to-duration ratio below this is normal buffer fill.
pub const DL_TO_DUR_NORMAL_MAX: f32 = 0.70;
/// Download-to-duration ratio above this depletes the virtual buffer.
pub const DL_TO_DUR_ELEVATED_MAX: f32 = 1.00;
/// Maximum manifest / metadata download size (decompression-bomb guard).
pub const MAX_MANIFEST_BYTES: usize = 10 * 1024 * 1024;
/// Maximum full segment download when not range-probing.
pub const MAX_SEGMENT_BYTES: usize = 32 * 1024 * 1024;
/// Nested HLS master -> variant -> sub-playlist depth cap.
pub const MAX_PLAYLIST_DEPTH: u32 = 8;
/// Maximum binary SCTE-35 section size accepted by the decoder.
pub const MAX_SCTE35_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelEntry {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvg_id: Option<String>,
}

impl ChannelEntry {
    pub fn group_label(&self) -> &str {
        self.group.as_deref().unwrap_or("Ungrouped")
    }

    pub fn url_summary(&self, max: usize) -> String {
        let u = self.url.as_str();
        if max == 0 {
            return String::new();
        }
        if u.chars().count() <= max {
            return u.to_string();
        }
        let trimmed: String = u.chars().take(max.saturating_sub(1)).collect();
        format!("{trimmed}…")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditVerdict {
    Live,
    Error,
    Stall,
}

impl AuditVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "LIVE",
            Self::Error => "ERROR",
            Self::Stall => "STALL",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRow {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub url: String,
    pub verdict: AuditVerdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    pub cdn: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<u64>,
    pub bitrate_profiles: Vec<u64>,
    pub has_pdt: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    pub captured_at: DateTime<Utc>,
    pub source: String,
    pub total: usize,
    pub live: usize,
    pub errors: usize,
    pub stalls: usize,
    pub channels: Vec<AuditRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbrVariant {
    pub bandwidth: u64,
    pub resolution: Option<String>,
    pub codecs: Option<String>,
    /// Declared video frame rate from HLS FRAME-RATE or DASH @frameRate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<f64>,
    pub uri: String,
    pub selected: bool,
    /// True when resolution / FPS / codecs were filled from bitstream probe.
    #[serde(default)]
    pub from_wire: bool,
    /// Manifest vs wire mismatch warning for this profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mismatch: Option<String>,
}

impl AbrVariant {
    pub fn fps_label(&self) -> String {
        let base = match self.frame_rate {
            Some(f) if f > 0.0 => {
                if (f - f.round()).abs() < 0.05 {
                    format!("{:.0}", f.round())
                } else {
                    format!("{f:.2}")
                }
            }
            _ => "-".into(),
        };
        if self.from_wire && self.frame_rate.is_some() {
            format!("{base}[wire]")
        } else {
            base
        }
    }

    pub fn resolution_label(&self) -> String {
        let base = self.resolution.clone().unwrap_or_else(|| "-".into());
        if self.from_wire && self.resolution.is_some() {
            format!("{base}[wire]")
        } else {
            base
        }
    }
}

/// Bitstream parameters from fMP4 / MPEG-TS.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WireProbeInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_channels: Option<u8>,
    /// First sample in moof/traf/trun is a sync / IDR frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_sample: Option<bool>,
    /// IDR / sync keyframes observed in the probed byte range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframe_count: Option<u32>,
    /// Presentation time (seconds) of the first keyframe in this segment, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframe_pts_sec: Option<f64>,
    /// Mean interval between consecutive keyframes across recent segments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gop_duration_sec: Option<f64>,
    /// True when GOP interval is stable across at least three keyframe samples.
    #[serde(default)]
    pub is_fixed_cadence: bool,
    /// ISO-BMFF / MPEG-TS timing diagnostics from the wire probe window.
    #[serde(default, skip_serializing_if = "WireTimingInfo::is_empty")]
    pub timing: WireTimingInfo,
    #[serde(default)]
    pub adts_sync_valid: bool,
    #[serde(default)]
    pub audio_silent_suspect: bool,
    #[serde(default)]
    pub container: ContainerKind,
    /// PSSH boxes discovered in the wire probe window.
    #[serde(default, skip_serializing_if = "PsshProbeInfo::is_empty")]
    pub pssh: PsshProbeInfo,
    /// DASH inband `emsg` boxes seen in the probe window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inband_emsg: Vec<InbandEmsgInfo>,
}

/// ISO BMFF `emsg` inband event (DASH EventMessage).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InbandEmsgInfo {
    pub version: u8,
    pub scheme_id_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub timescale: u32,
    pub presentation_time_delta: u64,
    pub event_duration: u64,
    pub id: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_data: Vec<u8>,
}

impl InbandEmsgInfo {
    pub fn is_scte_related(&self) -> bool {
        let s = self.scheme_id_uri.to_ascii_lowercase();
        s.contains("scte") || s.contains("splice") || s.contains("ad")
    }
}

/// Wire inband ad marker from `emsg` + optional decoded SCTE-35 summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InbandAdEvent {
    pub emsg: InbandEmsgInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scte35_summary: Option<String>,
}

/// fMP4 / MPEG-TS timing signals extracted from the probe buffer.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WireTimingInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidx_reference_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidx_timescale: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidx_earliest_presentation_time: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidx_first_subsegment_duration_ticks: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moof_base_decode_time: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moof_timescale: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trun_sample_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trun_total_duration_ticks: Option<u64>,
    #[serde(default)]
    pub pts_discontinuity: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pts_gap_ms: Option<f64>,
    #[serde(default)]
    pub pts_rollover_suspect: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_continuity_errors: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcr_pts_drift_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_duration_sec: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_duration_deviation_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prft_ntp_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prft_media_time_ticks: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glass_to_glass_ms: Option<i64>,
}

impl WireTimingInfo {
    pub fn is_empty(&self) -> bool {
        self.sidx_reference_count.is_none()
            && self.sidx_timescale.is_none()
            && self.sidx_earliest_presentation_time.is_none()
            && self.sidx_first_subsegment_duration_ticks.is_none()
            && self.moof_base_decode_time.is_none()
            && self.moof_timescale.is_none()
            && self.trun_sample_count.is_none()
            && self.trun_total_duration_ticks.is_none()
            && !self.pts_discontinuity
            && self.pts_gap_ms.is_none()
            && !self.pts_rollover_suspect
            && self.ts_continuity_errors.is_none()
            && self.pcr_pts_drift_ms.is_none()
            && self.wire_duration_sec.is_none()
            && self.target_duration_deviation_pct.is_none()
            && self.prft_ntp_unix_ms.is_none()
            && self.prft_media_time_ticks.is_none()
            && self.glass_to_glass_ms.is_none()
    }

    pub fn timing_label(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.pts_discontinuity {
            parts.push("PTS gap".into());
        }
        if self.pts_rollover_suspect {
            parts.push("PTS rollover?".into());
        }
        if let Some(n) = self.ts_continuity_errors.filter(|&v| v > 0) {
            parts.push(format!("CC err {n}"));
        }
        if let Some(ms) = self
            .pcr_pts_drift_ms
            .filter(|v| v.is_finite() && v.abs() > 50.0)
        {
            parts.push(format!("PCR drift {ms:.0}ms"));
        }
        if let Some(pct) = self
            .target_duration_deviation_pct
            .filter(|v| v.is_finite() && v.abs() > 15.0)
        {
            parts.push(format!("dur Δ {pct:.0}%"));
        }
        if let Some(ms) = self.glass_to_glass_ms.filter(|v| v.abs() > 500) {
            parts.push(format!("G2G {ms}ms"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }

    pub fn timing_badge(&self) -> Option<&'static str> {
        if self.pts_discontinuity || self.pts_rollover_suspect {
            Some("PTS!")
        } else if self.ts_continuity_errors.is_some_and(|n| n > 0) {
            Some("CC!")
        } else if self
            .target_duration_deviation_pct
            .is_some_and(|p| p.abs() > 15.0)
        {
            Some("DUR~")
        } else {
            None
        }
    }
}

impl WireProbeInfo {
    pub fn resolution_label(&self) -> Option<String> {
        match (self.width, self.height) {
            (Some(w), Some(h)) => Some(format!("{w}x{h}")),
            _ => None,
        }
    }

    pub fn gop_label(&self) -> Option<String> {
        if let Some(d) = self.gop_duration_sec.filter(|v| v.is_finite() && *v > 0.0) {
            let cadence = if self.is_fixed_cadence {
                "Fixed"
            } else {
                "Variable"
            };
            return Some(format!("{d:.2}s ({cadence})"));
        }
        let sync = self.sync_sample?;
        let base = if sync {
            "Keyframe (sync/IDR)"
        } else {
            "Delta (non-sync)"
        };
        Some(match self.keyframe_count {
            Some(n) if n > 0 => format!("{base} · {n} IDR in probe"),
            _ => base.into(),
        })
    }

    pub fn gop_badge(&self) -> Option<&'static str> {
        if self.gop_duration_sec.is_some() {
            return Some(if self.is_fixed_cadence { "GOP" } else { "GOP~" });
        }
        self.sync_sample
            .map(|sync| if sync { "IDR" } else { "Delta" })
    }

    pub fn audio_label(&self) -> Option<String> {
        if self.audio_codec.is_none()
            && self.audio_sample_rate.is_none()
            && self.audio_channels.is_none()
        {
            return None;
        }
        Some(format!(
            "{} · {} · {}",
            self.audio_codec.as_deref().unwrap_or("-"),
            self.audio_sample_rate
                .map_or_else(|| "- Hz".into(), |r| format!("{r} Hz")),
            self.audio_channels
                .map_or_else(|| "- ch".into(), |c| format!("{c} ch"))
        ))
    }

    pub fn audio_badge(&self) -> Option<String> {
        if self.audio_codec.is_none()
            && self.audio_sample_rate.is_none()
            && self.audio_channels.is_none()
        {
            return None;
        }
        let codec = self.audio_codec.as_deref().unwrap_or("audio");
        let sr = self.audio_sample_rate.map_or_else(
            || "-".into(),
            |r| {
                if r >= 1000 {
                    format!("{}k", r / 1000)
                } else {
                    format!("{r}")
                }
            },
        );
        let ch = self
            .audio_channels
            .map_or_else(|| "-".into(), |c| c.to_string());
        Some(format!("{codec}·{sr}·{ch}ch"))
    }
}

/// Negotiated HTTP version (ALPN / wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HttpVersion {
    #[default]
    H1,
    #[serde(rename = "h2")]
    H2,
    #[serde(rename = "h3")]
    H3,
}

impl HttpVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::H1 => "h1.1",
            Self::H2 => "h2",
            Self::H3 => "h3",
        }
    }

    pub fn as_metric_label(self) -> &'static str {
        self.as_str()
    }

    pub fn from_reqwest(version: reqwest::Version) -> Self {
        match version {
            reqwest::Version::HTTP_2 => Self::H2,
            reqwest::Version::HTTP_3 => Self::H3,
            _ => Self::H1,
        }
    }
}

/// QUIC-specific transport telemetry when HTTP/3 is active.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct QuicTelemetry {
    /// QUIC handshake duration (0-RTT or 1-RTT).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handshake_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_0rtt: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_resets: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packet_loss_pct: Option<f32>,
}

/// Socket-level timing breakdown for a single HTTP fetch.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[allow(clippy::struct_field_names)] // OpenMetrics: dns_ms, tcp_ms, tls_ms, ttfb_ms
pub struct NetworkTiming {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_ms: Option<u64>,
    /// Time until first response header byte.
    pub ttfb_ms: u64,
    /// Response body transfer after TTFB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_ms: Option<u64>,
    /// DoH JSON lookup latency when `--doh-provider` is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doh_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_version: Option<HttpVersion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quic: Option<QuicTelemetry>,
}

impl NetworkTiming {
    pub fn display_line(&self) -> String {
        let fmt = |v: Option<u64>| v.map_or_else(|| "-".into(), |ms| format!("{ms}ms"));
        let ver = self
            .http_version
            .map_or_else(|| "-".into(), |v| v.as_str().to_string());
        let xfer = self
            .transfer_ms
            .map_or_else(|| "-".into(), |ms| format!("{ms}ms"));
        format!(
            "DNS: {} | TCP: {} | TLS: {} | TTFB: {}ms | Xfer: {} | HTTP: {ver}{}",
            fmt(self.dns_ms),
            fmt(self.tcp_ms),
            fmt(self.tls_ms),
            self.ttfb_ms,
            xfer,
            self.doh_ms
                .map_or_else(String::new, |ms| format!(" | DoH: {ms}ms"))
        )
    }

    #[must_use]
    pub fn with_transfer(mut self, download_ms: u64) -> Self {
        self.transfer_ms = Some(download_ms.saturating_sub(self.ttfb_ms));
        self
    }
}

/// Multi-CDN edge snapshot for skew matrix.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MultiCdnEdgeSnapshot {
    pub label: String,
    pub url: String,
    pub media_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdt_offset_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_delay_ms: Option<u64>,
    pub cdn_hits: u64,
    pub cdn_misses: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_version: Option<HttpVersion>,
}

/// Aggregated multi-CDN skew report.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MultiCdnSkewReport {
    pub edges: Vec<MultiCdnEdgeSnapshot>,
    pub max_skew_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub propagation_latency_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_desync: Option<String>,
}

/// Parse DASH/HLS frame-rate strings (`25`, `30`, `30000/1001`).
pub fn parse_frame_rate(raw: &str) -> Option<f64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some((n, d)) = s.split_once('/') {
        let num: f64 = n.trim().parse().ok()?;
        let den: f64 = d.trim().parse().ok()?;
        if den <= 0.0 {
            return None;
        }
        let fps = num / den;
        return (fps > 0.0 && fps.is_finite()).then_some(fps);
    }
    let fps: f64 = s.parse().ok()?;
    (fps > 0.0 && fps.is_finite()).then_some(fps)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ContainerKind {
    Ts,
    Fmp4,
    #[default]
    Unknown,
}

impl ContainerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ts => "MPEG-TS",
            Self::Fmp4 => "fMP4/CMAF",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DlToDurState {
    #[default]
    Normal,
    Elevated,
    Draining,
}

/// Stack-backed RTF label for the TUI render loop (no per-frame heap allocs).
#[derive(Debug, Clone, Copy, Default)]
pub struct DlDurHud {
    bytes: [u8; 32],
    len: u8,
    pub state: DlToDurState,
}

impl DlDurHud {
    pub fn clear(&mut self) {
        self.len = 0;
        self.state = DlToDurState::Normal;
    }

    pub fn update_from_segment(&mut self, seg: &SegmentMetrics) {
        self.clear();
        let Some(ratio) = seg.dl_to_dur_ratio else {
            return;
        };
        self.state = classify_dl_to_dur(ratio);
        self.len = write_dl_dur_label(&mut self.bytes, ratio, self.state);
    }

    pub fn as_str(&self) -> &str {
        if self.len == 0 {
            return "-";
        }
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("-")
    }

    pub fn is_visible(&self) -> bool {
        self.len > 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentMetrics {
    pub media_sequence: u64,
    pub duration_secs: f32,
    /// Declared / full object size when known (Content-Range total); else transferred.
    pub size_bytes: u64,
    /// Bytes actually received on the wire for this sample.
    pub transferred_bytes: u64,
    pub ttfb_ms: u64,
    pub download_ms: u64,
    /// `download_secs / duration_secs`; `None` when duration is zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dl_to_dur_ratio: Option<f32>,
    /// Throughput from transferred bytes; `None` in range-probe mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_kbps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    pub uri: String,
    pub cdn: CdnEdgeInfo,
    pub probed: bool,
    pub container: ContainerKind,
    /// HTTP status of the segment/probe response (200/206 on success).
    #[serde(default)]
    pub http_status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<WireProbeInfo>,
}

impl SegmentMetrics {
    pub fn compute_dl_to_dur_ratio(download_ms: u64, duration_secs: f32) -> Option<f32> {
        if duration_secs > 0.0 {
            Some((download_ms as f32 / 1000.0) / duration_secs)
        } else {
            None
        }
    }

    pub fn dl_to_dur_state(&self) -> Option<DlToDurState> {
        self.dl_to_dur_ratio.map(classify_dl_to_dur)
    }

    pub fn rate_label(&self) -> String {
        if self.probed {
            let kb = self.transferred_bytes as f64 / 1024.0;
            format!("Probe: {kb:.1} KB in {} ms", self.download_ms)
        } else if let Some(kbps) = self.download_kbps {
            format!("{kbps} kbps")
        } else {
            "-".into()
        }
    }
}

pub fn classify_dl_to_dur(ratio: f32) -> DlToDurState {
    if ratio < DL_TO_DUR_NORMAL_MAX {
        DlToDurState::Normal
    } else if ratio <= DL_TO_DUR_ELEVATED_MAX {
        DlToDurState::Elevated
    } else {
        DlToDurState::Draining
    }
}

fn write_dl_dur_label(out: &mut [u8], ratio: f32, state: DlToDurState) -> u8 {
    use std::fmt::Write as _;
    struct StackBuf<'a> {
        buf: &'a mut [u8],
        len: usize,
    }
    impl std::fmt::Write for StackBuf<'_> {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            let take = s.len().min(self.buf.len().saturating_sub(self.len));
            self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
            self.len += take;
            Ok(())
        }
    }
    let mut w = StackBuf { buf: out, len: 0 };
    let _ = write!(w, "RTF: {ratio:.2}x");
    if state == DlToDurState::Draining {
        let _ = write!(w, " [STALL RISK]");
    }
    w.len.min(u8::MAX as usize) as u8
}

#[cfg(test)]
mod dl_to_dur_tests {
    use super::*;

    #[test]
    fn dl_to_dur_ratio_standard_segment() {
        let ratio = SegmentMetrics::compute_dl_to_dur_ratio(180, 2.0);
        assert!(ratio.is_some());
        assert!((ratio.unwrap() - 0.09).abs() < 0.001);
    }

    #[test]
    fn dl_to_dur_ratio_zero_duration_is_none() {
        assert!(SegmentMetrics::compute_dl_to_dur_ratio(500, 0.0).is_none());
    }

    #[test]
    fn dl_to_dur_threshold_classification() {
        assert_eq!(classify_dl_to_dur(0.18), DlToDurState::Normal);
        assert_eq!(classify_dl_to_dur(0.69), DlToDurState::Normal);
        assert_eq!(classify_dl_to_dur(0.70), DlToDurState::Elevated);
        assert_eq!(classify_dl_to_dur(0.85), DlToDurState::Elevated);
        assert_eq!(classify_dl_to_dur(1.00), DlToDurState::Elevated);
        assert_eq!(classify_dl_to_dur(1.01), DlToDurState::Draining);
        assert_eq!(classify_dl_to_dur(2.5), DlToDurState::Draining);
    }

    #[test]
    fn dl_dur_hud_formats_without_heap() {
        let seg = SegmentMetrics {
            media_sequence: 1,
            duration_secs: 2.0,
            size_bytes: 1000,
            transferred_bytes: 1000,
            ttfb_ms: 40,
            download_ms: 360,
            dl_to_dur_ratio: Some(0.18),
            download_kbps: Some(500),
            latency_ms: None,
            uri: String::new(),
            cdn: CdnEdgeInfo::default(),
            probed: false,
            container: ContainerKind::Ts,
            http_status: 200,
            network: None,
            wire: None,
        };
        let mut hud = DlDurHud::default();
        hud.update_from_segment(&seg);
        assert_eq!(hud.as_str(), "RTF: 0.18x");
        assert_eq!(hud.state, DlToDurState::Normal);
    }
}

/// How many trailing playlist segments to scan for DAI / SCTE tags.
pub const AD_SCAN_LIVE_EDGE_SEGMENTS: usize = 5;

/// Media-sequence advance tolerance before treating as a gap.
pub const MEDIA_SEQ_GAP_TOLERANCE: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamStatusKind {
    Live,
    Error,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamStatus {
    pub kind: StreamStatusKind,
    pub message: String,
}

impl StreamStatus {
    pub fn live(message: impl Into<String>) -> Self {
        Self {
            kind: StreamStatusKind::Live,
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            kind: StreamStatusKind::Error,
            message: message.into(),
        }
    }

    pub fn degraded(message: impl Into<String>) -> Self {
        Self {
            kind: StreamStatusKind::Degraded,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LatencyState {
    Measured(u64),
    Estimated(u64),
    Unknown,
}

impl LatencyState {
    pub fn is_estimated(&self) -> bool {
        matches!(self, Self::Estimated(_))
    }

    pub fn is_measured(&self) -> bool {
        matches!(self, Self::Measured(_))
    }

    pub fn display(&self) -> String {
        match self {
            Self::Unknown => "-".into(),
            Self::Estimated(ms) => format!("estimated ~{:.2}s", *ms as f64 / 1000.0),
            Self::Measured(ms) => format!("{:.3}s", *ms as f64 / 1000.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagCategory {
    Rfc,
    Stalling,
    Cdn,
    Ad,
    Abr,
    Segment,
    Buffer,
    LlHls,
    AvSync,
    Drm,
    Info,
}

impl DiagCategory {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Rfc => "RFC",
            Self::Stalling => "ORIGIN",
            Self::Cdn => "CDN",
            Self::Ad => "AD",
            Self::Abr => "ABR",
            Self::Segment => "SEGMENT",
            Self::Buffer => "BUFFER",
            Self::LlHls => "LL-HLS",
            Self::AvSync => "A/V",
            Self::Drm => "DRM",
            Self::Info => "INFO",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagSeverity {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts: DateTime<Utc>,
    pub time: String,
    pub level: LogLevel,
    pub tag: String,
    pub category: DiagCategory,
    pub message: String,
}

impl LogEntry {
    pub fn make(level: LogLevel, category: DiagCategory, message: impl Into<String>) -> Self {
        let ts = Utc::now();
        Self {
            time: ts.format("%H:%M:%S%.3f").to_string(),
            ts,
            level,
            tag: category.tag().to_string(),
            category,
            message: message.into(),
        }
    }

    /// Timeline line: `HH:MM:SS.mmm  [TAG] message`.
    pub fn timeline_line(&self) -> String {
        format!("{}  [{:<8}] {}", self.time, self.tag, self.message)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticFinding {
    pub category: DiagCategory,
    pub severity: DiagSeverity,
    pub rule: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl DiagnosticFinding {
    pub fn with_reason_code(
        category: DiagCategory,
        severity: DiagSeverity,
        rule: impl Into<String>,
        message: impl Into<String>,
        code: crate::models::DiagnosticReasonCode,
    ) -> Self {
        Self {
            category,
            severity,
            rule: rule.into(),
            message: message.into(),
            reason: Some(code.as_str().to_string()),
        }
    }
}

/// Summary schema spec violation row (`spec_violations` in summary v4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecViolation {
    pub severity: String,
    pub rule: String,
    pub message: String,
    pub standard: String,
}

impl SpecViolation {
    pub fn from_finding(f: &DiagnosticFinding) -> Self {
        let severity = match f.severity {
            DiagSeverity::Error => "ERROR",
            DiagSeverity::Warn => "WARNING",
            DiagSeverity::Info => "INFO",
        };
        let standard = if f.rule.starts_with("DASH_") {
            "DASH"
        } else {
            match f.category {
                DiagCategory::Rfc | DiagCategory::LlHls => "HLS",
                DiagCategory::Cdn => "CDN",
                DiagCategory::Ad => "DAI",
                _ => "GENERAL",
            }
        };
        Self {
            severity: severity.into(),
            rule: f.rule.clone(),
            message: f.message.clone(),
            standard: standard.into(),
        }
    }
}

/// Manifest vs wire SCTE-35 alignment failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdMarkerMismatch {
    pub rule: String,
    pub message: String,
    pub manifest_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planned_duration_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_pts_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift_ms: Option<i64>,
}

/// Sanitized HTTP transaction for incident bundles.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HttpTransaction {
    pub method: String,
    pub url: String,
    pub status: u16,
    pub ttfb_ms: u64,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdn_provider: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CacheVerdict {
    Hit,
    Miss,
    #[default]
    Unknown,
}

impl CacheVerdict {
    /// Short TUI badge for Hit / Miss / Unknown (used by `CdnEdgeInfo::badge`).
    pub fn badge(self) -> &'static str {
        match self {
            Self::Hit => "HIT (Edge)",
            Self::Miss => "MISS (Origin)",
            Self::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamProtocol {
    Hls,
    Dash,
}

impl StreamProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hls => "HLS",
            Self::Dash => "DASH",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CdnEdgeInfo {
    pub verdict: CacheVerdict,
    /// Detected CDN / cache provider (Akamai, Cloudflare, CloudFront, Fastly, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pop: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub served_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cf_ray: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub akamai_cache_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_cache_hits: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_timing_edge_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_timing_origin_ms: Option<u64>,
}

impl CdnEdgeInfo {
    pub fn badge(&self) -> String {
        let edge = match self.verdict {
            CacheVerdict::Unknown => self.guess_edge_badge(),
            other => other.badge(),
        };
        self.provider
            .as_ref()
            .map_or_else(|| edge.to_string(), |p| format!("{p} · {edge}"))
    }

    fn guess_edge_badge(&self) -> &'static str {
        if self.age.is_some_and(|a| a > 0) {
            "HIT? (Age)"
        } else if self.served_by.as_deref().is_some_and(|s| {
            let u = s.to_ascii_uppercase();
            u.contains("CACHE") || u.contains("EDGE") || u.contains("VARNISH")
        }) || self.via.is_some()
            || self.pop.is_some()
        {
            "EDGE?"
        } else {
            "ORIGIN?"
        }
    }

    /// Compact POP / Age / Server-Timing for status line.
    pub fn edge_detail(&self) -> String {
        let mut parts = Vec::new();
        if let Some(p) = &self.pop {
            parts.push(format!("pop={p}"));
        }
        if let Some(a) = self.age {
            parts.push(format!("age={a}s"));
        }
        if let Some(ms) = self.server_timing_edge_ms {
            parts.push(format!("edge={ms}ms"));
        }
        if let Some(ms) = self.server_timing_origin_ms {
            parts.push(format!("origin={ms}ms"));
        }
        parts.join(" ")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CdnStats {
    pub hits: u64,
    pub misses: u64,
    pub unknown: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<CdnEdgeInfo>,
}

impl CdnStats {
    pub fn record(&mut self, info: &CdnEdgeInfo) {
        match info.verdict {
            CacheVerdict::Hit => self.hits = self.hits.saturating_add(1),
            CacheVerdict::Miss => self.misses = self.misses.saturating_add(1),
            CacheVerdict::Unknown => self.unknown = self.unknown.saturating_add(1),
        }
        self.last = Some(info.clone());
    }

    pub fn hit_ratio_pct(&self) -> Option<f64> {
        let known = self.hits.saturating_add(self.misses);
        if known == 0 {
            None
        } else {
            Some((self.hits as f64 / known as f64) * 100.0)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdBreakInfo {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planned_duration_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_secs: Option<f64>,
    pub summary: String,
    pub active: bool,
    /// Decoded binary SCTE-35 summary line when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scte35_binary: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct VirtualBuffer {
    pub buffer_secs: f64,
    pub stall_risk_pct: u8,
    /// Rebuffer probability from download-vs-duration simulation (0-100%).
    pub rebuffer_probability_pct: u8,
    /// Composite stall risk index (stall + rebuffer, capped at 100).
    pub stall_risk_index: u8,
    pub ladder_switches: u32,
    pub ping_pong_detected: bool,
}

impl VirtualBuffer {
    /// Drain by wall-clock elapsed, then credit segment duration.
    pub fn on_new_segment(&mut self, duration_secs: f32, elapsed_wall_secs: f64) {
        self.buffer_secs = (self.buffer_secs - elapsed_wall_secs.max(0.0)
            + f64::from(duration_secs))
        .clamp(0.0, 120.0);
        self.recompute_stall_risk();
        self.stall_risk_index = self.stall_risk_pct;
    }

    /// Drain buffer by wall-clock time between polls.
    pub fn drain_elapsed(&mut self, elapsed_wall_secs: f64) {
        if elapsed_wall_secs <= 0.0 {
            return;
        }
        self.buffer_secs = (self.buffer_secs - elapsed_wall_secs).clamp(0.0, 120.0);
        self.recompute_stall_risk();
    }

    pub fn recompute_stall_risk(&mut self) {
        self.stall_risk_pct = if self.buffer_secs >= BUFFER_STALL_THRESHOLD_SECS {
            0
        } else {
            (((BUFFER_STALL_THRESHOLD_SECS - self.buffer_secs) / BUFFER_STALL_THRESHOLD_SECS)
                * 100.0)
                .round()
                .clamp(0.0, 100.0) as u8
        };
    }

    pub fn display(&self) -> String {
        let abr = if self.ping_pong_detected {
            " | ABR ping-pong".to_string()
        } else if self.ladder_switches > 0 {
            format!(" | switches={}", self.ladder_switches)
        } else {
            String::new()
        };
        format!(
            "Buffer: {:.1}s | Stall: {}% | Rebuf: {}%{abr}",
            self.buffer_secs, self.stall_risk_pct, self.rebuffer_probability_pct
        )
    }
}

/// Glass-to-glass pipeline latency breakdown.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[allow(clippy::struct_field_names)] // G2G pipeline: ingestion_lag_ms, edge_propagation_ms, g2g_total_ms
pub struct G2gMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingestion_lag_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_propagation_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub g2g_total_ms: Option<i64>,
}

impl G2gMetrics {
    pub fn is_empty(&self) -> bool {
        self.ingestion_lag_ms.is_none()
            && self.edge_propagation_ms.is_none()
            && self.g2g_total_ms.is_none()
    }

    pub fn display(&self) -> String {
        let ingest = self
            .ingestion_lag_ms
            .map_or_else(|| "ingest -".into(), |v| format!("ingest {v}ms"));
        let edge = self
            .edge_propagation_ms
            .map_or_else(|| "edge -".into(), |v| format!("edge {v}ms"));
        let total = self
            .g2g_total_ms
            .map_or_else(|| "G2G -".into(), |v| format!("G2G {v}ms"));
        format!("{total} | {ingest} | {edge}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PsshEntry {
    pub system_id: String,
    pub drm_system: String,
    pub version: u8,
    pub key_ids: Vec<String>,
    pub data_len: u32,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption_scheme: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PsshProbeInfo {
    pub entries: Vec<PsshEntry>,
}

impl PsshProbeInfo {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DrmInfo {
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_format: Option<String>,
    pub badge: String,
    /// Absolute or relative URI from `#EXT-X-KEY:URI=…` (license / key server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_uri: Option<String>,
    /// `#EXT-X-KEY` IV attribute when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_iv: Option<String>,
    /// RTT / TTFB to the key/license URI when probed (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_ttfb_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_error: Option<String>,
    /// `GET` range probe or `POST` ClearKey JSON license request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_method: Option<String>,
    /// Parsed PSSH entries from manifest or fMP4 wire probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pssh: Option<PsshProbeInfo>,
}

/// Subtitle timing vs video PTS correlation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubtitleSyncInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle_drift_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    pub cue_count: u32,
    pub desync_warning: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MediaRenditions {
    pub audio: Vec<String>,
    pub subtitles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlHlsInfo {
    pub is_ll_hls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_target_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_part_duration_secs: Option<f64>,
    /// Last PART index within the current partial segment (1-based).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_part_sequence: Option<u32>,
    /// Last PART duration in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_part_duration_ms: Option<u64>,
    /// Part / PRELOAD-HINT Range-probe transfer rate (kbps).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_part_transfer_kbps: Option<f64>,
    pub part_count: u32,
    pub has_preload_hint: bool,
    pub can_block_reload: bool,
    /// True when PRELOAD-HINT / PART was Range-probed this cycle.
    pub preload_hint_fetched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preload_hint_uri: Option<String>,
    /// Absolute byte offset from `#EXT-X-BYTERANGE` / PRELOAD-HINT BYTERANGE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preload_byterange_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preload_byterange_length: Option<u64>,
}

impl LlHlsInfo {
    /// Part latency for the status bar (ms), preferring measured PART duration.
    pub fn part_latency_ms(&self) -> Option<u64> {
        if let Some(ms) = self.last_part_duration_ms {
            return Some(ms);
        }
        self.last_part_duration_secs
            .or(self.part_target_secs)
            .map(|s| (s * 1000.0).round() as u64)
    }

    /// LL-HLS status badge for the header.
    pub fn header_badge(&self) -> Option<String> {
        if !self.is_ll_hls {
            return None;
        }
        let latency = self
            .part_latency_ms()
            .map_or_else(|| "-".into(), |ms| format!("{ms}ms"));
        let seq = self.last_part_sequence.map_or_else(
            || format!("parts={}", self.part_count),
            |s| format!("seq={s}"),
        );
        let rate = self.last_part_transfer_kbps.map_or_else(
            || {
                if self.preload_hint_fetched {
                    "probed".into()
                } else if self.has_preload_hint {
                    "hint".into()
                } else {
                    "-".into()
                }
            },
            |k| {
                if k >= 1000.0 {
                    format!("{:.2} Mbps", k / 1000.0)
                } else {
                    format!("{k:.0} kbps")
                }
            },
        );
        Some(format!("[LL-HLS] part {latency} | {seq} | {rate}"))
    }

    /// LL-HLS poll sleep from part duration (clamped 200-330 ms).
    pub fn poll_interval_ms(&self) -> u64 {
        let secs = self
            .last_part_duration_secs
            .or(self.part_target_secs)
            .unwrap_or(0.33);
        let ms = (secs * 1000.0).round() as u64;
        ms.clamp(200, 330)
    }
}

/// Per-part LL-HLS wire metrics from PRELOAD-HINT / `#EXT-X-PART` probe.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlHlsPartMetrics {
    pub part_sequence: u32,
    pub ttfb_ms: u64,
    pub download_ms: u64,
    pub part_duration_secs: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_dl_duration_ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_kbps: Option<f64>,
}

impl LlHlsPartMetrics {
    pub fn compute_part_rtf(download_ms: u64, part_duration_secs: f64) -> Option<f32> {
        if part_duration_secs > 0.0 {
            Some((download_ms as f32 / 1000.0) / part_duration_secs as f32)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod ll_hls_part_tests {
    use super::LlHlsPartMetrics;

    #[test]
    fn part_rtf_ratio_for_partial_segment() {
        let ratio = LlHlsPartMetrics::compute_part_rtf(330, 0.33).expect("ratio");
        assert!((ratio - 1.0).abs() < 0.01);
        let stall = LlHlsPartMetrics::compute_part_rtf(500, 0.33).expect("stall");
        assert!(stall > 1.0);
    }
}

/// Low-latency DASH / CMAF chunking signals from MPD and segment fetches.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlDashInfo {
    pub is_ll_dash: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_target_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability_time_offset_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utc_timing_scheme: Option<String>,
    pub chunked_transfer: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_drift_ms: Option<i64>,
}

impl LlDashInfo {
    pub fn header_badge(&self) -> Option<String> {
        if !self.is_ll_dash {
            return None;
        }
        let target = self
            .latency_target_ms
            .map_or_else(|| "-".into(), |ms| format!("{ms}ms"));
        let ato = self
            .availability_time_offset_secs
            .map_or_else(|| "-".into(), |s| format!("ato={s:.3}s"));
        let cte = if self.chunked_transfer { "CTE" } else { "-" };
        let drift = self
            .production_drift_ms
            .map_or_else(|| "-".into(), |d| format!("drift={d}ms"));
        Some(format!(
            "[LL-DASH] target {target} | {ato} | {cte} | {drift}"
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistMeta {
    pub media_sequence: u64,
    pub target_duration: u64,
    pub url: String,
    pub window_segments: u32,
    pub window_secs: f64,
    pub has_pdt: bool,
    pub has_master_playlist: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_interval_ms: Option<u64>,
    pub ll_hls: LlHlsInfo,
    #[serde(default)]
    pub ll_dash: LlDashInfo,
    #[serde(default)]
    pub drm: DrmInfo,
    #[serde(default)]
    pub renditions: MediaRenditions,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Tr101290Check {
    pub priority: u8,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Tr101290Report {
    pub p1_violations: u32,
    pub p2_violations: u32,
    pub sync_errors: u32,
    pub cc_errors: u32,
    pub pat_timeout: bool,
    pub pmt_timeout: bool,
    pub pcr_gap_ms: Option<f64>,
    pub pcr_jitter_ms: Option<f64>,
    pub pts_discontinuities: u32,
    pub unreferenced_pids: u32,
    #[serde(default)]
    pub checks: Vec<Tr101290Check>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SeiProbeResult {
    pub cea608_present: bool,
    pub cea708_present: bool,
    pub hdr10_present: bool,
    pub hlg_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cll: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fall: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption_language: Option<String>,
    pub nal_units_scanned: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub score: u8,
    pub label: String,
    pub deductions: Vec<String>,
}

impl HealthReport {
    pub fn perfect() -> Self {
        Self {
            score: 100,
            label: "Excellent".into(),
            deductions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AbrHealth {
    pub warnings: Vec<String>,
    pub score_penalty: u8,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum StreamEvent {
    Status(StreamStatus),
    Variants(Vec<AbrVariant>),
    PlaylistMeta(PlaylistMeta),
    Segment(SegmentMetrics),
    LlHlsPart(LlHlsPartMetrics),
    Latency(LatencyState),
    Health(HealthReport),
    CdnStats(CdnStats),
    AbrHealth(AbrHealth),
    AdBreak(AdBreakInfo),
    AdMarkerMismatch(AdMarkerMismatch),
    InbandAdEvent(InbandAdEvent),
    Buffer(VirtualBuffer),
    G2g(G2gMetrics),
    ProbeMode(bool),
    Finding(DiagnosticFinding),
    WireProbe(WireProbeInfo),
    Tr101290(Tr101290Report),
    SeiProbe(SeiProbeResult),
    Log {
        level: LogLevel,
        category: DiagCategory,
        message: String,
    },
    Error(String),
    /// Multi-CDN skew matrix update (`--multi-cdn`).
    MultiCdnSkew(MultiCdnSkewReport),
    /// Active transport / ALPN snapshot for TUI status bar.
    Transport(NetworkTiming),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    pub captured_at: DateTime<Utc>,
    pub source_url: String,
    pub active_url: String,
    pub status: String,
    pub health_score: u8,
    pub health_label: String,
    pub latency: String,
    pub cdn: String,
    pub dvr_window: String,
    pub buffer: String,
    pub ll_hls: bool,
    #[serde(default)]
    pub dropped_events: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamSnapshot {
    pub title: String,
    pub summary: DiagnosticSummary,
    /// Readable log lines (`HH:MM:SS.mmm  [TAG] message`).
    pub timeline: Vec<String>,
    pub health: HealthReport,
    pub cdn: CdnStats,
    pub abr_health: AbrHealth,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_ad: Option<AdBreakInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist: Option<PlaylistMeta>,
    pub abr_profiles: Vec<AbrVariant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_segment: Option<SegmentMetrics>,
    pub findings: Vec<DiagnosticFinding>,
    pub event_log: Vec<LogEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct RingBuffer {
    inner: VecDeque<u64>,
    capacity: usize,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, value: u64) {
        if self.inner.len() >= self.capacity {
            self.inner.pop_front();
        }
        self.inner.push_back(value);
    }

    pub fn clear(&mut self) {
        self.inner.clear();
    }

    pub fn to_vec(&self) -> Vec<u64> {
        self.inner.iter().copied().collect()
    }
}

pub fn format_dvr_window(segments: u32, window_secs: f64) -> String {
    let human = format_duration_human(window_secs);
    if segments == 0 {
        if window_secs > 0.0 {
            format!("Window: ~{human} DVR")
        } else {
            "Window: -".into()
        }
    } else {
        format!("Window: {segments} seg (~{human} DVR)")
    }
}

pub fn format_duration_human(secs: f64) -> String {
    if secs >= 3600.0 {
        format!("{:.1} hours", secs / 3600.0)
    } else if secs >= 60.0 {
        format!("{:.1} min", secs / 60.0)
    } else {
        format!("{secs:.1}s")
    }
}

/// Truncate long URLs with a mid ellipsis.
pub fn format_url_mid_ellipsis(url: &str, max: usize) -> String {
    let chars: Vec<char> = url.chars().collect();
    if max < 8 || chars.len() <= max {
        return url.to_string();
    }
    let keep = max.saturating_sub(3);
    let head = keep / 2;
    let tail = keep - head;
    let left: String = chars.iter().take(head).collect();
    let right: String = chars.iter().skip(chars.len() - tail).collect();
    format!("{left}...{right}")
}

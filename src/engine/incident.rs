//! Incident bundle export for NOC triage.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use color_eyre::eyre::Result;
use serde::Serialize;

use crate::engine::export::{build_curl, ExportCapture};
use crate::engine::redact::{redact_text, redact_url};
use crate::models::{
    DiagnosticFinding, HttpTransaction, PlaylistMeta, SpecViolation, StreamSnapshot,
};

pub const INCIDENT_DIR: &str = "diagnostics";

/// Structured incident report (`incident_report_<TIMESTAMP>.json`).
#[derive(Debug, Serialize)]
pub struct IncidentReport {
    pub schema: &'static str,
    pub captured_at: DateTime<Utc>,
    pub url: String,
    pub spec_violations: Vec<SpecViolation>,
    pub manifest_snapshots: Vec<ManifestSnapshot>,
    pub http_transactions: Vec<HttpTransaction>,
    pub curl_commands: Vec<String>,
    pub summary: StreamSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestSnapshot {
    pub captured_at: DateTime<Utc>,
    pub media_sequence: u64,
    pub target_duration: u64,
    pub url: String,
}

impl ManifestSnapshot {
    pub fn from_playlist(p: &PlaylistMeta) -> Self {
        Self {
            captured_at: Utc::now(),
            media_sequence: p.media_sequence,
            target_duration: p.target_duration,
            url: redact_url(&p.url),
        }
    }
}

pub fn findings_to_spec_violations(findings: &[DiagnosticFinding]) -> Vec<SpecViolation> {
    findings.iter().map(SpecViolation::from_finding).collect()
}

pub fn build_incident_report(
    snapshot: StreamSnapshot,
    manifest_history: &[PlaylistMeta],
    http_log: &[HttpTransaction],
    headers: &[String],
    user_agent: Option<&str>,
) -> IncidentReport {
    let spec_violations = findings_to_spec_violations(&snapshot.findings);
    let manifest_snapshots: Vec<ManifestSnapshot> = manifest_history
        .iter()
        .map(ManifestSnapshot::from_playlist)
        .collect();

    let mut curl_commands = Vec::new();
    if let Some(seg) = &snapshot.last_segment {
        curl_commands.push(build_curl(&ExportCapture {
            manifest_url: snapshot
                .playlist
                .as_ref()
                .map_or_else(|| snapshot.summary.active_url.clone(), |p| p.url.clone()),
            segment_url: Some(seg.uri.clone()),
            probe_headers: seg.probed,
            headers: headers.to_vec(),
            user_agent: user_agent.map(String::from),
            last_http_status: Some(seg.http_status),
            last_ttfb_ms: Some(seg.ttfb_ms),
            last_size_bytes: Some(seg.transferred_bytes),
        }));
    } else if let Some(pl) = &snapshot.playlist {
        curl_commands.push(build_curl(&ExportCapture {
            manifest_url: pl.url.clone(),
            segment_url: None,
            probe_headers: false,
            headers: headers.to_vec(),
            user_agent: user_agent.map(String::from),
            last_http_status: None,
            last_ttfb_ms: None,
            last_size_bytes: None,
        }));
    }

    let http_transactions: Vec<HttpTransaction> = http_log
        .iter()
        .rev()
        .take(100)
        .cloned()
        .map(|mut t| {
            t.url = redact_url(&t.url);
            t
        })
        .collect();

    IncidentReport {
        schema: "streamtop.incident.v1",
        captured_at: snapshot.summary.captured_at,
        url: snapshot.summary.active_url.clone(),
        spec_violations,
        manifest_snapshots,
        http_transactions,
        curl_commands,
        summary: snapshot,
    }
}

pub fn incident_export_path(base: Option<&Path>, now: DateTime<Utc>) -> PathBuf {
    let stamp = now.format("%Y%m%d_%H%M%S");
    base.map_or_else(
        || PathBuf::from(format!("{INCIDENT_DIR}/incident_report_{stamp}.json")),
        std::path::Path::to_path_buf,
    )
}

pub fn write_incident_report(path: &Path, report: &IncidentReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(report)?;
    std::fs::write(path, redact_text(&json))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DiagCategory, DiagSeverity};

    #[test]
    fn spec_violation_mapping() {
        let f = DiagnosticFinding {
            category: DiagCategory::Rfc,
            severity: DiagSeverity::Error,
            rule: "TARGET_DURATION".into(),
            message: "too long".into(),
        };
        let v = SpecViolation::from_finding(&f);
        assert_eq!(v.severity, "ERROR");
        assert_eq!(v.standard, "HLS");
    }

    #[test]
    fn incident_path_default() {
        let p = incident_export_path(None, Utc::now());
        assert!(p.to_string_lossy().contains("incident_report_"));
    }
}

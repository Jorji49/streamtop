//! curl / HAR export for debugging and incident reports.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::Utc;
use color_eyre::eyre::{eyre, Result, WrapErr};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::engine::redact::{
    is_sensitive_header, redact_header_line, redact_headers, redact_url, redact_user_agent,
    REDACTED,
};
use crate::engine::ManifestPoller;
use crate::models::{
    StreamEvent, DEEP_WIRE_PROBE_BYTES, EVENT_CHANNEL_CAPACITY, RANGE_PROBE_BYTES,
};
use crate::ui::app::SessionOpts;

#[derive(Debug, Clone, Default)]
pub struct ExportCapture {
    pub manifest_url: String,
    pub segment_url: Option<String>,
    pub probe_headers: bool,
    pub headers: Vec<String>,
    pub user_agent: Option<String>,
    pub last_http_status: Option<u16>,
    pub last_ttfb_ms: Option<u64>,
    pub last_size_bytes: Option<u64>,
}

/// Build a reproducible curl for the last segment (or manifest). Secrets are redacted.
pub fn build_curl(cap: &ExportCapture) -> String {
    let url = redact_url(
        cap.segment_url
            .as_deref()
            .unwrap_or(cap.manifest_url.as_str()),
    );
    let mut parts = vec!["curl -sS -L".to_string()];
    if cap.probe_headers {
        parts.push(format!("-H \"Range: bytes=0-{DEEP_WIRE_PROBE_BYTES}\""));
    }
    for h in redact_headers(&cap.headers) {
        let escaped = h.replace('"', "\\\"");
        parts.push(format!("-H \"{escaped}\""));
    }
    if let Some(ua) = redact_user_agent(cap.user_agent.as_deref()) {
        let escaped = ua.replace('"', "\\\"");
        parts.push(format!("-A \"{escaped}\""));
    }
    parts.push(format!("\"{url}\""));
    parts.join(" ")
}

/// HAR 1.2 document with manifest + optional segment entries (metadata only; no bodies).
pub fn build_har(cap: &ExportCapture) -> serde_json::Value {
    let started = Utc::now().to_rfc3339();
    let mut entries = Vec::new();

    entries.push(har_entry(
        &cap.manifest_url,
        None,
        None,
        None,
        &cap.headers,
        cap.user_agent.as_deref(),
    ));

    if let Some(seg) = &cap.segment_url {
        let range = if cap.probe_headers {
            Some(format!("bytes=0-{DEEP_WIRE_PROBE_BYTES}"))
        } else {
            None
        };
        entries.push(har_entry(
            seg,
            range.as_deref(),
            cap.last_http_status,
            cap.last_ttfb_ms,
            &cap.headers,
            cap.user_agent.as_deref(),
        ));
    }

    json!({
        "log": {
            "version": "1.2",
            "creator": {
                "name": "streamtop",
                "version": env!("CARGO_PKG_VERSION")
            },
            "comment": format!(
                "Exported {started}; range-probe={} (2KB audit / {}B wire probe); secrets redacted",
                cap.probe_headers, DEEP_WIRE_PROBE_BYTES
            ),
            "entries": entries
        }
    })
}

fn har_entry(
    url: &str,
    range: Option<&str>,
    status: Option<u16>,
    ttfb_ms: Option<u64>,
    headers: &[String],
    user_agent: Option<&str>,
) -> serde_json::Value {
    let mut req_headers = Vec::new();
    if let Some(r) = range {
        req_headers.push(json!({"name": "Range", "value": r}));
    }
    for h in headers {
        if let Some((k, v)) = h.split_once(':') {
            let name = k.trim();
            let value = if is_sensitive_header(name) {
                REDACTED
            } else {
                v.trim()
            };
            req_headers.push(json!({"name": name, "value": value}));
        } else {
            let _ = redact_header_line(h);
        }
    }
    if let Some(ua) = redact_user_agent(user_agent) {
        req_headers.push(json!({"name": "User-Agent", "value": ua}));
    }

    let wait = ttfb_ms.unwrap_or(0) as f64;
    json!({
        "startedDateTime": Utc::now().to_rfc3339(),
        "time": wait,
        "request": {
            "method": "GET",
            "url": redact_url(url),
            "httpVersion": "HTTP/1.1",
            "cookies": [],
            "headers": req_headers,
            "queryString": [],
            "headersSize": -1,
            "bodySize": 0
        },
        "response": {
            "status": status.unwrap_or(0),
            "statusText": "",
            "httpVersion": "HTTP/1.1",
            "cookies": [],
            "headers": [],
            "content": { "size": 0, "mimeType": "application/octet-stream" },
            "redirectURL": "",
            "headersSize": -1,
            "bodySize": 0
        },
        "cache": {},
        "timings": {
            "blocked": -1,
            "dns": -1,
            "connect": -1,
            "send": 0,
            "wait": wait,
            "receive": 0,
            "ssl": -1
        }
    })
}

pub fn write_har(path: impl AsRef<Path>, cap: &ExportCapture) -> Result<()> {
    let doc = build_har(cap);
    let text = serde_json::to_string_pretty(&doc)?;
    fs::write(path.as_ref(), text).wrap_err("write HAR")?;
    Ok(())
}

/// Poll briefly, capture last segment, then return export snapshot.
pub async fn capture_for_export(
    url: String,
    session: SessionOpts,
    timeout_secs: u64,
) -> Result<ExportCapture> {
    let (tx, mut rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let poller = ManifestPoller::new(
        url.clone(),
        session.headers.clone(),
        session.user_agent.clone(),
        session.interval_ms,
        session.probe_headers,
        session.probe_drm,
        tx,
    )?;
    let handle = tokio::spawn(async move {
        poller.run().await;
    });

    let mut cap = ExportCapture {
        manifest_url: url,
        probe_headers: session.probe_headers,
        headers: session.headers,
        user_agent: session.user_agent,
        ..Default::default()
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, rx.recv()).await {
            Ok(Some(StreamEvent::Segment(s))) => {
                cap.segment_url = Some(s.uri);
                if s.http_status > 0 {
                    cap.last_http_status = Some(s.http_status);
                }
                cap.last_ttfb_ms = Some(s.ttfb_ms);
                cap.last_size_bytes = Some(s.size_bytes);
            }
            Ok(Some(StreamEvent::PlaylistMeta(m))) => {
                if !m.url.is_empty() {
                    cap.manifest_url = m.url;
                }
            }
            Ok(None) => break,
            Err(_) => break,
            _ => {}
        }
    }
    handle.abort();

    if cap.segment_url.is_none() && cap.manifest_url.is_empty() {
        return Err(eyre!("export capture produced no URLs"));
    }
    let _ = RANGE_PROBE_BYTES; // documented in HAR comment via DEEP_WIRE
    Ok(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_includes_range_when_probe() {
        let cap = ExportCapture {
            manifest_url: "https://ex/master.m3u8".into(),
            segment_url: Some("https://ex/seg.ts".into()),
            probe_headers: true,
            headers: vec!["X-Test: 1".into()],
            user_agent: Some("streamtop-test".into()),
            ..Default::default()
        };
        let cmd = build_curl(&cap);
        assert!(cmd.contains("Range: bytes=0-"));
        assert!(cmd.contains("https://ex/seg.ts"));
        assert!(cmd.contains("X-Test: 1"));
    }

    #[test]
    fn curl_redacts_auth_and_token() {
        let cap = ExportCapture {
            manifest_url: "https://ex/m.m3u8?token=secret".into(),
            segment_url: None,
            headers: vec!["Authorization: Bearer abc".into()],
            ..Default::default()
        };
        let cmd = build_curl(&cap);
        assert!(!cmd.contains("Bearer abc"));
        assert!(!cmd.contains("token=secret"));
        assert!(cmd.contains(REDACTED));
    }

    #[test]
    fn har_has_two_entries_with_segment() {
        let cap = ExportCapture {
            manifest_url: "https://ex/master.m3u8".into(),
            segment_url: Some("https://ex/seg.m4s".into()),
            probe_headers: true,
            last_http_status: Some(206),
            last_ttfb_ms: Some(42),
            ..Default::default()
        };
        let har = build_har(&cap);
        let entries = har["log"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(har["log"]["creator"]["name"], "streamtop");
    }

    #[test]
    fn har_redacts_sensitive_headers() {
        let cap = ExportCapture {
            manifest_url: "https://ex/m.m3u8?key=abc".into(),
            headers: vec!["Cookie: sid=1".into()],
            ..Default::default()
        };
        let har = build_har(&cap);
        let s = har.to_string();
        assert!(!s.contains("sid=1"));
        assert!(!s.contains("key=abc"));
        assert!(s.contains(REDACTED));
    }
}

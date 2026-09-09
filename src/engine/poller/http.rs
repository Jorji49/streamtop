use std::time::Duration;

use color_eyre::eyre::{eyre, Result, WrapErr};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Client;
use url::Url;

use crate::engine::doh::resolve_doh;
use crate::engine::playlist_parser::local_path_from_url;
use crate::models::{
    DiagCategory, DiagSeverity, DiagnosticFinding, DiagnosticReasonCode, NetworkTiming, StreamEvent,
    MAX_MANIFEST_BYTES, MAX_PLAYLIST_DEPTH,
};

use super::ManifestPoller;

const DEFAULT_UA: &str = concat!("streamtop/", env!("CARGO_PKG_VERSION"));

impl ManifestPoller {
    pub(super) fn traceparent(&self) -> Option<String> {
        self.otel.as_ref().map(|o| o.traceparent())
    }

    pub(super) async fn apply_doh_timing(&self, url: &str, network: &mut NetworkTiming) {
        let Some(ref provider) = self.doh_provider else {
            return;
        };
        if let Ok(cache) = self.doh_cache.lock() {
            if let Some(ms) = *cache {
                network.doh_ms = Some(ms);
                return;
            }
        }
        let host = Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));
        let Some(host) = host else {
            return;
        };
        match resolve_doh(&self.client, &host, provider).await {
            Ok(result) => {
                network.doh_ms = Some(result.doh_ms);
                if let Ok(mut cache) = self.doh_cache.lock() {
                    *cache = Some(result.doh_ms);
                }
                if let Some(m) = &self.metrics {
                    if let Ok(mut snap) = m.write() {
                        snap.dns_doh_duration_secs = result.doh_ms as f64 / 1000.0;
                    }
                }
            }
            Err(err) => {
                let already = self.doh_failed.lock().is_ok_and(|f| *f);
                if !already {
                    if let Ok(mut failed) = self.doh_failed.lock() {
                        *failed = true;
                    }
                    self.send_event(StreamEvent::Finding(DiagnosticFinding::with_reason_code(
                        DiagCategory::Info,
                        DiagSeverity::Warn,
                        "DOH_RESOLVE",
                        format!("DoH failed for {host}: {err}"),
                        DiagnosticReasonCode::ErrDohResolutionFailed,
                    )));
                }
            }
        }
    }
    pub(super) async fn fetch_manifest(&self, url: &str) -> Result<(Vec<u8>, Option<String>)> {
        if let Some(path) = local_path_from_url(url) {
            let body = tokio::fs::read(&path)
                .await
                .wrap_err_with(|| format!("failed to read {}", path.display()))?;
            return Ok((body, None));
        }
        let response = with_probe_read_timeout(async {
            self.client
                .get(url)
                .send()
                .await
                .wrap_err_with(|| format!("GET failed: {url}"))
        })
        .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(eyre!("HTTP {status} - {url}"));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(std::string::ToString::to_string);
        let body = read_response_bytes_limited(response, MAX_MANIFEST_BYTES)
            .await
            .wrap_err("failed to read body")?;
        Ok((body, content_type))
    }

    pub(super) async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        Ok(self.fetch_manifest(url).await?.0)
    }

    pub(super) async fn fetch_bytes_with_depth(&self, url: &str, depth: u32) -> Result<Vec<u8>> {
        if depth > MAX_PLAYLIST_DEPTH {
            return Err(eyre!(
                "playlist nesting exceeds MAX_PLAYLIST_DEPTH ({MAX_PLAYLIST_DEPTH})"
            ));
        }
        let _ = depth;
        self.fetch_bytes(url).await
    }
}

pub fn build_http_client(headers: &[String], user_agent: Option<&str>) -> Result<Client> {
    use crate::models::{PROBE_CONNECT_TIMEOUT_SECS, PROBE_READ_TIMEOUT_SECS};
    build_http_client_with_timeouts(
        headers,
        user_agent,
        PROBE_CONNECT_TIMEOUT_SECS,
        PROBE_READ_TIMEOUT_SECS,
    )
}

pub(super) async fn with_probe_read_timeout<F, T>(future: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    use crate::models::PROBE_READ_TIMEOUT_SECS;
    tokio::time::timeout(Duration::from_secs(PROBE_READ_TIMEOUT_SECS), future)
        .await
        .unwrap_or_else(|_| Err(eyre!("probe read timeout after {PROBE_READ_TIMEOUT_SECS}s")))
}

pub(super) async fn read_response_bytes_limited(response: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.wrap_err("response stream read error")?;
        if buf.len().saturating_add(chunk.len()) > max {
            return Err(eyre!("response exceeds {max} byte limit"));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Audit HTTP client with short connect/total timeouts.
pub fn build_audit_http_client(headers: &[String], user_agent: Option<&str>) -> Result<Client> {
    use crate::models::{AUDIT_CONNECT_TIMEOUT_SECS, AUDIT_REQUEST_TIMEOUT_SECS};
    build_http_client_with_timeouts(
        headers,
        user_agent,
        AUDIT_CONNECT_TIMEOUT_SECS,
        AUDIT_REQUEST_TIMEOUT_SECS,
    )
}

fn build_http_client_with_timeouts(
    headers: &[String],
    user_agent: Option<&str>,
    connect_secs: u64,
    timeout_secs: u64,
) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(user_agent.unwrap_or(DEFAULT_UA))
        .gzip(true)
        .brotli(true)
        .redirect(crate::engine::redirect::redirect_policy())
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(connect_secs))
        .pool_idle_timeout(Duration::from_secs(90));

    let header_map = parse_headers(headers)?;
    if !header_map.is_empty() {
        builder = builder.default_headers(header_map);
    }

    builder.build().wrap_err("failed to build HTTP client")
}

fn parse_headers(raw: &[String]) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for entry in raw {
        let (name, value) = entry
            .split_once(':')
            .ok_or_else(|| eyre!("invalid header (expected KEY: VALUE): {entry}"))?;
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .wrap_err_with(|| format!("invalid header name: {name}"))?;
        let value = HeaderValue::from_str(value.trim())
            .wrap_err_with(|| format!("invalid header value: {value}"))?;
        map.insert(name, value);
    }
    Ok(map)
}

pub(super) fn resolve_url(base: &Url, href: &str) -> Result<Url> {
    if let Ok(absolute) = Url::parse(href) {
        return Ok(absolute);
    }
    base.join(href)
        .wrap_err_with(|| format!("failed to join URL: base={base} href={href}"))
}

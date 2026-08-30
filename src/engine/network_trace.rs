//! Socket-level DNS / TCP / TLS / TTFB breakdown for HTTP(S) GETs.

use std::fmt::Write as _;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use color_eyre::eyre::{eyre, Result, WrapErr};
use http::HeaderMap;
use rustls::pki_types::ServerName;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::Url;

use crate::engine::ip_pin::{pick_connect_addr, resolve_pinned_addrs, validate_outbound_url};
use crate::models::NetworkTiming;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct TracedResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub timing: NetworkTiming,
    pub download_ms: u64,
    pub chunked_transfer: bool,
}

/// Perform a timed HTTP/1.1 GET with optional Range header and W3C trace context.
pub async fn traced_get(
    url: &str,
    extra_headers: &[(String, String)],
    range: Option<&str>,
    max_body: Option<usize>,
    traceparent: Option<&str>,
) -> Result<TracedResponse> {
    let parsed = Url::parse(url).wrap_err("invalid URL")?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(eyre!("unsupported scheme: {scheme}"));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| eyre!("URL missing host"))?
        .to_string();
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| eyre!("URL missing port"))?;
    let path = if parsed.query().is_some() {
        format!("{}?{}", parsed.path(), parsed.query().unwrap_or(""))
    } else {
        parsed.path().to_string()
    };
    let path = if path.is_empty() { "/".into() } else { path };

    let dns_start = Instant::now();
    let addrs = tokio::net::lookup_host((host.as_str(), port))
        .await
        .wrap_err("DNS lookup failed")?
        .collect::<Vec<SocketAddr>>();
    let dns_ms = dns_start.elapsed().as_millis() as u64;
    if addrs.is_empty() {
        return Err(eyre!("DNS returned no addresses for {host}"));
    }
    let addr = pick_connect_addr(&addrs);

    let tcp_start = Instant::now();
    let tcp = tokio::time::timeout(DEFAULT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| eyre!("TCP connect timeout"))?
        .wrap_err("TCP connect failed")?;
    let tcp_ms = tcp_start.elapsed().as_millis() as u64;
    tcp.set_nodelay(true).ok();

    let mut tls_ms = None;
    let total_start = Instant::now();

    let mut request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: */*\r\nUser-Agent: streamtop/{}\r\n",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(r) = range {
        let _ = write!(request, "Range: {r}\r\n");
    }
    if let Some(tp) = traceparent {
        let _ = write!(request, "traceparent: {tp}\r\n");
    }
    for (k, v) in extra_headers {
        if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("connection") {
            continue;
        }
        let _ = write!(request, "{k}: {v}\r\n");
    }
    request.push_str("\r\n");

    let (status, headers, body, ttfb_ms) = if scheme == "https" {
        let tls_start = Instant::now();
        let connector = build_tls_connector();
        let server_name =
            ServerName::try_from(host.clone()).map_err(|_| eyre!("invalid TLS server name"))?;
        let mut tls = tokio::time::timeout(DEFAULT_TIMEOUT, connector.connect(server_name, tcp))
            .await
            .map_err(|_| eyre!("TLS handshake timeout"))?
            .wrap_err("TLS handshake failed")?;
        tls_ms = Some(tls_start.elapsed().as_millis() as u64);

        let write_start = Instant::now();
        tls.write_all(request.as_bytes()).await?;
        tls.flush().await?;
        let (st, hdrs, body_bytes, header_done) = read_http_response(&mut tls, max_body).await?;
        let ttfb = write_start.elapsed().as_millis() as u64;
        let ttfb_ms = header_done.unwrap_or(ttfb);
        (st, hdrs, body_bytes, ttfb_ms)
    } else {
        let mut stream = tcp;
        let write_start = Instant::now();
        stream.write_all(request.as_bytes()).await?;
        stream.flush().await?;
        let (st, hdrs, body_bytes, header_done) = read_http_response(&mut stream, max_body).await?;
        let ttfb_ms = header_done.unwrap_or_else(|| write_start.elapsed().as_millis() as u64);
        (st, hdrs, body_bytes, ttfb_ms)
    };

    let download_ms = total_start.elapsed().as_millis() as u64;
    let chunked_transfer = headers_indicate_chunked(&headers);
    Ok(TracedResponse {
        status,
        headers,
        body,
        timing: NetworkTiming {
            dns_ms: Some(dns_ms),
            tcp_ms: Some(tcp_ms),
            tls_ms,
            ttfb_ms,
        },
        download_ms: download_ms.max(1),
        chunked_transfer,
    })
}

fn build_tls_connector() -> TlsConnector {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

async fn read_http_response<S: AsyncReadExt + Unpin>(
    stream: &mut S,
    max_body: Option<usize>,
) -> Result<(u16, HeaderMap, Vec<u8>, Option<u64>)> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 4096];
    let header_start = Instant::now();
    let mut header_ms = None;
    let header_end;
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Err(eyre!("connection closed before HTTP headers"));
        }
        if header_ms.is_none() {
            header_ms = Some(header_start.elapsed().as_millis() as u64);
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            header_end = pos;
            break;
        }
        if buf.len() > 1024 * 1024 {
            return Err(eyre!("HTTP headers too large"));
        }
    }

    let header_bytes = &buf[..header_end];
    let (status, headers) = parse_headers(header_bytes)?;
    let mut body = buf[header_end..].to_vec();

    let content_length = headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok());

    let limit = max_body
        .or(content_length)
        .unwrap_or(crate::models::MAX_SEGMENT_BYTES);
    while body.len() < limit {
        let want = (limit - body.len()).min(tmp.len());
        match stream.read(&mut tmp[..want]).await {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        if let Some(max) = max_body {
            if body.len() >= max {
                body.truncate(max);
                break;
            }
        }
    }

    Ok((status, headers, body, header_ms))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn parse_headers(header_block: &[u8]) -> Result<(u16, HeaderMap)> {
    let text = std::str::from_utf8(header_block).wrap_err("headers not utf-8")?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or_else(|| eyre!("empty response"))?;
    let mut parts = status_line.split_whitespace();
    let _http = parts.next();
    let code: u16 = parts
        .next()
        .ok_or_else(|| eyre!("bad status line"))?
        .parse()
        .wrap_err("bad status code")?;
    let mut headers = HeaderMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            if let (Ok(name), Ok(val)) = (
                http::HeaderName::try_from(k.trim()),
                http::HeaderValue::try_from(v.trim()),
            ) {
                headers.append(name, val);
            }
        }
    }
    Ok((code, headers))
}

/// Convert reqwest-style header list ("Key: Value") into pairs.
pub fn parse_header_pairs(headers: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for h in headers {
        if let Some((k, v)) = h.split_once(':') {
            out.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    out
}

/// Fallback timing when only wall-clock TTFB is known.
pub fn timing_from_ttfb(ttfb_ms: u64) -> NetworkTiming {
    NetworkTiming {
        dns_ms: None,
        tcp_ms: None,
        tls_ms: None,
        ttfb_ms,
    }
}

pub fn headers_indicate_chunked(headers: &HeaderMap) -> bool {
    headers
        .get("transfer-encoding")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
}

pub fn reqwest_headers_chunked(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get("transfer-encoding")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
}

/// IP-pinned POST with SSRF validation (webhooks / DRM license endpoints).
pub async fn pinned_post_json(
    url: &str,
    body: &Value,
    allow_insecure: bool,
    timeout: Duration,
) -> Result<u16> {
    validate_outbound_url(url, allow_insecure)?;
    let parsed = Url::parse(url).wrap_err("invalid URL")?;
    let host = parsed
        .host_str()
        .ok_or_else(|| eyre!("URL missing host"))?
        .to_string();
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| eyre!("URL missing port"))?;
    let addrs = resolve_pinned_addrs(&host, port, allow_insecure)?;
    let addr = pick_connect_addr(&addrs);
    let path = if parsed.query().is_some() {
        format!("{}?{}", parsed.path(), parsed.query().unwrap_or(""))
    } else {
        parsed.path().to_string()
    };
    let path = if path.is_empty() { "/".into() } else { path };
    let payload = body.to_string();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\nUser-Agent: streamtop/{}\r\n\r\n{payload}",
        payload.len(),
        env!("CARGO_PKG_VERSION")
    );
    let scheme = parsed.scheme();
    let (status, _headers, _body, _) = if scheme == "https" {
        let connector = build_tls_connector();
        let server_name =
            ServerName::try_from(host.clone()).map_err(|_| eyre!("invalid TLS server name"))?;
        let tcp = tokio::time::timeout(timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| eyre!("TCP connect timeout"))?
            .wrap_err("TCP connect failed")?;
        let mut tls = tokio::time::timeout(timeout, connector.connect(server_name, tcp))
            .await
            .map_err(|_| eyre!("TLS handshake timeout"))?
            .wrap_err("TLS handshake failed")?;
        tls.write_all(request.as_bytes()).await?;
        tls.flush().await?;
        read_http_response(&mut tls, Some(65536)).await?
    } else {
        let mut tcp = tokio::time::timeout(timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| eyre!("TCP connect timeout"))?
            .wrap_err("TCP connect failed")?;
        tcp.write_all(request.as_bytes()).await?;
        tcp.flush().await?;
        read_http_response(&mut tcp, Some(65536)).await?
    };
    Ok(status)
}

/// IP-pinned GET with optional Range header (DRM license probes).
pub async fn pinned_get_range(
    url: &str,
    range: Option<&str>,
    allow_insecure: bool,
    timeout: Duration,
    max_body: usize,
) -> Result<(u16, u64)> {
    validate_outbound_url(url, allow_insecure)?;
    let parsed = Url::parse(url).wrap_err("invalid URL")?;
    let host = parsed
        .host_str()
        .ok_or_else(|| eyre!("URL missing host"))?
        .to_string();
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| eyre!("URL missing port"))?;
    let addrs = resolve_pinned_addrs(&host, port, allow_insecure)?;
    let addr = pick_connect_addr(&addrs);
    let path = if parsed.query().is_some() {
        format!("{}?{}", parsed.path(), parsed.query().unwrap_or(""))
    } else {
        parsed.path().to_string()
    };
    let path = if path.is_empty() { "/".into() } else { path };
    let mut request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: */*\r\nUser-Agent: streamtop/{}\r\n",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(r) = range {
        let _ = write!(request, "Range: {r}\r\n");
    }
    request.push_str("\r\n");

    let started = Instant::now();
    let scheme = parsed.scheme();
    let (status, _headers, _body, _) = if scheme == "https" {
        let connector = build_tls_connector();
        let server_name =
            ServerName::try_from(host.clone()).map_err(|_| eyre!("invalid TLS server name"))?;
        let tcp = tokio::time::timeout(timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| eyre!("TCP connect timeout"))?
            .wrap_err("TCP connect failed")?;
        let mut tls = tokio::time::timeout(timeout, connector.connect(server_name, tcp))
            .await
            .map_err(|_| eyre!("TLS handshake timeout"))?
            .wrap_err("TLS handshake failed")?;
        tls.write_all(request.as_bytes()).await?;
        tls.flush().await?;
        read_http_response(&mut tls, Some(max_body)).await?
    } else {
        let mut tcp = tokio::time::timeout(timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| eyre!("TCP connect timeout"))?
            .wrap_err("TCP connect failed")?;
        tcp.write_all(request.as_bytes()).await?;
        tcp.flush().await?;
        read_http_response(&mut tcp, Some(max_body)).await?
    };
    Ok((status, started.elapsed().as_millis() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::NetworkTiming;

    #[test]
    fn display_line_formats() {
        let t = NetworkTiming {
            dns_ms: Some(4),
            tcp_ms: Some(18),
            tls_ms: Some(22),
            ttfb_ms: 45,
        };
        let s = t.display_line();
        assert!(s.contains("DNS: 4ms"));
        assert!(s.contains("TCP: 18ms"));
        assert!(s.contains("TLS: 22ms"));
        assert!(s.contains("TTFB: 45ms"));
    }

    #[test]
    fn find_header_end_works() {
        let buf = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\nBODY";
        assert_eq!(find_header_end(buf), Some(buf.len() - 4));
    }
}

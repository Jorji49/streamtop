//! In-memory Tokio HTTP server serving synthetic HLS/DASH for hermetic tests.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum MockScenario {
    Normal,
    /// Delay every HTTP response (manifest stall simulation).
    Stall {
        delay_ms: u64,
    },
    DriftDuration,
    Segment404,
    /// Invalid fMP4 init bytes for wire-probe error paths.
    CorruptFmp4,
    /// Manifest carries malformed SCTE-35 base64.
    CorruptScte35,
    /// Response includes synthetic clock-skew header.
    ClockSkew,
    Scte35Ad,
    /// Abort TCP connection mid-body after partial payload.
    TcpResetMidDownload,
    /// Send Content-Length larger than body (truncated payload).
    TruncatedPayload,
    /// Chunked transfer with jittery delivery delays.
    JitterChunked,
    /// LL-HLS parts listed out of order in media playlist.
    OutOfOrderLlHls,
    /// WebVTT subtitle track with intentional PTS drift.
    SubtitleDrift,
    /// fMP4 segment with corrupted PSSH box.
    CorruptPssh,
}

#[derive(Debug, Clone)]
pub struct MockStreamServer {
    pub base_url: String,
    #[allow(dead_code)]
    pub scenario: MockScenario,
    shutdown: Arc<oneshot::Sender<()>>,
}

static SEGMENT_SEQ: AtomicU64 = AtomicU64::new(1);

impl MockStreamServer {
    pub async fn start_hls() -> Self {
        Self::start_with(MockScenario::Normal).await
    }

    pub async fn start_with(scenario: MockScenario) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{addr}");
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let shutdown = Arc::new(shutdown_tx);
        let shutdown_poll = Arc::clone(&shutdown);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let Ok((mut stream, _)) = accept else { continue };
                        let scenario = scenario;
                        tokio::spawn(async move {
                            let mut buf = vec![0u8; 8192];
                            let Ok(n) = stream.read(&mut buf).await else { return };
                            let req = String::from_utf8_lossy(&buf[..n]);
                            let path = req
                                .lines()
                                .next()
                                .unwrap_or("")
                                .split_whitespace()
                                .nth(1)
                                .unwrap_or("/");
                            if let MockScenario::Stall { delay_ms } = scenario {
                                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                            }
                            if matches!(scenario, MockScenario::JitterChunked) && path.ends_with(".ts") {
                                serve_jitter_chunked(&mut stream, path, scenario).await;
                                return;
                            }
                            if matches!(scenario, MockScenario::TcpResetMidDownload) && path.ends_with(".ts") {
                                serve_tcp_reset(&mut stream, path, scenario).await;
                                return;
                            }
                            let (status, body, ctype, extra) = route_hls(path, scenario);
                            if matches!(scenario, MockScenario::TruncatedPayload) && path.ends_with(".ts") {
                                let response = format!(
                                    "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close{extra}\r\n\r\n",
                                    body.len().saturating_add(512)
                                );
                                let _ = stream.write_all(response.as_bytes()).await;
                                let _ = stream.write_all(body.as_bytes()).await;
                                return;
                            }
                            let response = format!(
                                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close{extra}\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                            let _ = stream.write_all(body.as_bytes()).await;
                        });
                    }
                }
            }
            let _ = shutdown_poll;
        });
        Self {
            base_url,
            scenario,
            shutdown,
        }
    }
}

impl Drop for MockStreamServer {
    fn drop(&mut self) {
        if let Ok(tx) = Arc::try_unwrap(self.shutdown.clone()) {
            let _ = tx.send(());
        }
    }
}

async fn serve_tcp_reset(stream: &mut tokio::net::TcpStream, path: &str, scenario: MockScenario) {
    let (_, body, ctype, extra) = route_hls(path, scenario);
    let partial = &body[..body.len().min(64)];
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close{extra}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(partial.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn serve_jitter_chunked(
    stream: &mut tokio::net::TcpStream,
    path: &str,
    scenario: MockScenario,
) {
    let (_, body, ctype, extra) = route_hls(path, scenario);
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nTransfer-Encoding: chunked\r\nConnection: close{extra}\r\n\r\n"
    );
    let _ = stream.write_all(headers.as_bytes()).await;
    for chunk in body.as_bytes().chunks(47) {
        tokio::time::sleep(Duration::from_millis(15)).await;
        let hex = format!("{:x}\r\n", chunk.len());
        let _ = stream.write_all(hex.as_bytes()).await;
        let _ = stream.write_all(chunk).await;
        let _ = stream.write_all(b"\r\n").await;
    }
    let _ = stream.write_all(b"0\r\n\r\n").await;
}

fn route_hls(
    path: &str,
    scenario: MockScenario,
) -> (&'static str, String, &'static str, &'static str) {
    let extra = match scenario {
        MockScenario::ClockSkew => "\r\nX-Streamtop-Clock-Skew-Ms: 2500",
        _ => "",
    };
    if path.ends_with(".vtt") {
        let body = if matches!(scenario, MockScenario::SubtitleDrift) {
            "WEBVTT\n\n00:00:05.000 --> 00:00:08.000\nDrifted cue\n".into()
        } else {
            "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nOK\n".into()
        };
        return ("200 OK", body, "text/vtt", extra);
    }
    if path.ends_with(".m3u8") {
        let body = match scenario {
            MockScenario::DriftDuration => {
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nseg.ts\n#EXTINF:3.5,\nseg2.ts\n".into()
            }
            MockScenario::Scte35Ad => {
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-CUE-OUT:30\n#EXTINF:2.0,\nseg.ts\n".into()
            }
            MockScenario::CorruptScte35 => {
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-SCTE35:!!!not-valid-base64!!!\n#EXTINF:2.0,\nseg.ts\n".into()
            }
            MockScenario::OutOfOrderLlHls => {
                "#EXTM3U\n#EXT-X-VERSION:9\n#EXT-X-TARGETDURATION:2\n#EXT-X-PART-TARGET:0.5\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-PART:DURATION=0.5,URI=\"part1.m4s\"\n#EXT-X-PART:DURATION=0.5,URI=\"part0.m4s\"\n#EXTINF:2.0,\nseg.ts\n".into()
            }
            MockScenario::SubtitleDrift => {
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nseg.ts\n#EXTINF:2.0,\nsub.vtt\n".into()
            }
            _ => "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nseg.ts\n".into(),
        };
        return ("200 OK", body, "application/vnd.apple.mpegurl", extra);
    }
    if path.ends_with(".ts") || path.ends_with(".m4s") {
        if matches!(scenario, MockScenario::Segment404) {
            return ("404 Not Found", String::new(), "text/plain", extra);
        }
        if matches!(scenario, MockScenario::CorruptFmp4) {
            return ("200 OK", "not-a-valid-box".into(), "video/mp4", extra);
        }
        if matches!(scenario, MockScenario::CorruptPssh) {
            return ("200 OK", corrupt_pssh_segment(), "video/mp4", extra);
        }
        let seq = SEGMENT_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut pkt = vec![0u8; 188];
        pkt[0] = 0x47;
        pkt[3] = 0x10 | ((seq % 16) as u8);
        return (
            "200 OK",
            String::from_utf8(pkt).unwrap_or_default(),
            "video/mp2t",
            extra,
        );
    }
    ("404 Not Found", String::new(), "text/plain", extra)
}

pub fn corrupt_pssh_segment() -> String {
    let mut bytes = vec![0u8; 32];
    bytes[0..4].copy_from_slice(&32u32.to_be_bytes());
    bytes[4..8].copy_from_slice(b"pssh");
    bytes[8] = 1;
    String::from_utf8(bytes).unwrap_or_else(|_| "bad".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_hls_serves_manifest() {
        let server = MockStreamServer::start_hls().await;
        let url = format!("{}/live.m3u8", server.base_url);
        let client = reqwest::Client::new();
        let body = client
            .get(&url)
            .send()
            .await
            .expect("get")
            .text()
            .await
            .expect("text");
        assert!(body.contains("#EXTM3U"));
    }

    #[tokio::test]
    async fn mock_stall_delays_response() {
        let server = MockStreamServer::start_with(MockScenario::Stall { delay_ms: 200 }).await;
        let url = format!("{}/live.m3u8", server.base_url);
        let started = std::time::Instant::now();
        let _ = reqwest::Client::new().get(&url).send().await;
        assert!(started.elapsed() >= Duration::from_millis(180));
    }

    #[tokio::test]
    async fn mock_subtitle_drift_serves_vtt() {
        let server = MockStreamServer::start_with(MockScenario::SubtitleDrift).await;
        let url = format!("{}/sub.vtt", server.base_url);
        let body = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("get")
            .text()
            .await
            .expect("text");
        assert!(body.contains("00:00:05.000"));
    }

    #[tokio::test]
    async fn mock_corrupt_pssh_segment() {
        let server = MockStreamServer::start_with(MockScenario::CorruptPssh).await;
        let url = format!("{}/init.m4s", server.base_url);
        let bytes = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("get")
            .bytes()
            .await
            .expect("bytes");
        assert!(bytes.windows(4).any(|w| w == b"pssh"));
    }

    #[tokio::test]
    async fn mock_out_of_order_ll_hls_manifest() {
        let server = MockStreamServer::start_with(MockScenario::OutOfOrderLlHls).await;
        let url = format!("{}/ll.m3u8", server.base_url);
        let body = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("get")
            .text()
            .await
            .expect("text");
        assert!(body.contains("#EXT-X-PART"));
        let p0 = body.find("part0").unwrap_or(0);
        let p1 = body.find("part1").unwrap_or(0);
        assert!(p1 < p0, "parts intentionally out of presentation order");
    }
}

//! In-memory Tokio HTTP server serving synthetic HLS for hermetic tests.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy)]
pub enum MockScenario {
    Normal,
    Stall { delay_ms: u64 },
    OutOfOrderLlHls,
    SubtitleDrift,
    CorruptPssh,
}

#[derive(Debug, Clone)]
pub struct MockStreamServer {
    pub base_url: String,
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
                            let (status, body, ctype) = route_hls(path, scenario);
                            let response = format!(
                                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
        Self { base_url, shutdown }
    }
}

impl Drop for MockStreamServer {
    fn drop(&mut self) {
        if let Ok(tx) = Arc::try_unwrap(self.shutdown.clone()) {
            let _ = tx.send(());
        }
    }
}

fn route_hls(path: &str, scenario: MockScenario) -> (&'static str, String, &'static str) {
    if path.ends_with(".vtt") {
        let body = if matches!(scenario, MockScenario::SubtitleDrift) {
            "WEBVTT\n\n00:00:05.000 --> 00:00:08.000\nDrifted cue\n".into()
        } else {
            "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nOK\n".into()
        };
        return ("200 OK", body, "text/vtt");
    }
    if path.ends_with(".m3u8") {
        let body = match scenario {
            MockScenario::OutOfOrderLlHls => {
                "#EXTM3U\n#EXT-X-VERSION:9\n#EXT-X-TARGETDURATION:2\n#EXT-X-PART-TARGET:0.5\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-PART:DURATION=0.5,URI=\"part1.m4s\"\n#EXT-X-PART:DURATION=0.5,URI=\"part0.m4s\"\n#EXTINF:2.0,\nseg.ts\n".into()
            }
            MockScenario::SubtitleDrift => {
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nseg.ts\n#EXTINF:2.0,\nsub.vtt\n".into()
            }
            _ => "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nseg.ts\n".into(),
        };
        return ("200 OK", body, "application/vnd.apple.mpegurl");
    }
    if path.ends_with(".ts") || path.ends_with(".m4s") {
        if matches!(scenario, MockScenario::CorruptPssh) {
            return ("200 OK", corrupt_pssh_segment(), "video/mp4");
        }
        let seq = SEGMENT_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut pkt = vec![0u8; 188];
        pkt[0] = 0x47;
        pkt[3] = 0x10 | ((seq % 16) as u8);
        return (
            "200 OK",
            String::from_utf8(pkt).unwrap_or_default(),
            "video/mp2t",
        );
    }
    ("404 Not Found", String::new(), "text/plain")
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

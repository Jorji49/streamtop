//! In-memory Tokio HTTP server serving synthetic HLS/DASH/TS for hermetic tests.

mod fixtures;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

pub use fixtures::{sei_caption_hdr_ts, tr101290_broken_ts};

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum MockScenario {
    Normal,
    Stall { delay_ms: u64 },
    OutOfOrderLlHls,
    SubtitleDrift,
    CorruptPssh,
    Tr101290,
    SeiCaptions,
    LlHlsFmp4,
    DashLive,
    Aes128Hls,
    CdnRedirect,
    MediaSeqGap,
}

#[derive(Debug, Clone)]
pub struct MockStreamServer {
    pub base_url: String,
    shutdown: Arc<oneshot::Sender<()>>,
}

static SEGMENT_SEQ: AtomicU64 = AtomicU64::new(1);
static MEDIA_SEQ_GAP_HITS: AtomicU64 = AtomicU64::new(0);

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
                            let mut buf = vec![0u8; 16384];
                            let Ok(n) = stream.read(&mut buf).await else { return };
                            let req = String::from_utf8_lossy(&buf[..n]);
                            if let MockScenario::Stall { delay_ms } = scenario {
                                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                            }
                            let range = parse_range_header(&req);
                            let path = req
                                .lines()
                                .next()
                                .unwrap_or("")
                                .split_whitespace()
                                .nth(1)
                                .unwrap_or("/");
                            let (status, body, ctype, location) = route(path, scenario);
                            if let Some((start, end)) = range {
                                let start = start.min(body.len());
                                let end = end.min(body.len().saturating_sub(1));
                                if start <= end && !body.is_empty() {
                                    let slice = &body[start..=end];
                                    let header = format!(
                                        "HTTP/1.1 206 Partial Content\r\nContent-Type: {ctype}\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        body.len(),
                                        slice.len()
                                    );
                                    let _ = stream.write_all(header.as_bytes()).await;
                                    let _ = stream.write_all(slice).await;
                                    return;
                                }
                                let msg = http_response("416 Range Not Satisfiable", b"", ctype, None);
                                let _ = stream.write_all(&msg).await;
                                return;
                            }
                            let msg = http_response(status, &body, ctype, location);
                            let _ = stream.write_all(&msg).await;
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

fn http_response(
    status: &str,
    body: &[u8],
    ctype: &str,
    location: Option<&str>,
) -> Vec<u8> {
    let loc = location
        .map(|href| format!("Location: {href}\r\n"))
        .unwrap_or_default();
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\n{loc}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn parse_range_header(req: &str) -> Option<(usize, usize)> {
    for line in req.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(val) = lower.strip_prefix("range: bytes=") {
            let part = val.trim();
            if let Some((start, end)) = part.split_once('-') {
                let s: usize = start.parse().ok()?;
                let e: usize = end.parse().ok()?;
                return Some((s, e));
            }
        }
    }
    None
}

#[allow(clippy::too_many_lines)]
fn route(
    path: &str,
    scenario: MockScenario,
) -> (&'static str, Vec<u8>, &'static str, Option<&'static str>) {
    let path_obj = std::path::Path::new(path);
    if path_obj
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("vtt"))
    {
        let body = if matches!(scenario, MockScenario::SubtitleDrift) {
            b"WEBVTT\n\n00:00:05.000 --> 00:00:08.000\nDrifted cue\n".to_vec()
        } else {
            b"WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nOK\n".to_vec()
        };
        return ("200 OK", body, "text/vtt", None);
    }

    if matches!(scenario, MockScenario::CdnRedirect)
        && (path.contains("edge.m3u8") || path.ends_with("/cdn/live.m3u8"))
    {
        return (
            "302 Found",
            Vec::new(),
            "text/plain",
            Some("/origin/live.m3u8"),
        );
    }

    if path.contains("key.bin") || path.ends_with("/aes.key") {
        return ("200 OK", vec![0u8; 16], "application/octet-stream", None);
    }

    if (path_obj
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mpd"))
        || path.contains("live.mpd"))
        && matches!(scenario, MockScenario::DashLive)
    {
        return (
            "200 OK",
            load_fixture_bytes("dash_live.mpd"),
            "application/dash+xml",
            None,
        );
    }

    if path_obj
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u8"))
    {
        let body = match scenario {
            MockScenario::LlHlsFmp4 if path.contains("master") => ll_hls_master_playlist(),
            MockScenario::OutOfOrderLlHls | MockScenario::LlHlsFmp4
                if path.contains("ll/") || path.contains("360") || path.contains("720") =>
            {
                ll_hls_media_playlist()
            }
            MockScenario::OutOfOrderLlHls => {
                b"#EXTM3U\n#EXT-X-VERSION:9\n#EXT-X-TARGETDURATION:2\n#EXT-X-PART-TARGET:0.5\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-PART:DURATION=0.5,URI=\"part1.m4s\"\n#EXT-X-PART:DURATION=0.5,URI=\"part0.m4s\"\n#EXTINF:2.0,\nseg.ts\n".to_vec()
            }
            MockScenario::SubtitleDrift => {
                b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nseg.ts\n#EXTINF:2.0,\nsub.vtt\n".to_vec()
            }
            MockScenario::Tr101290 | MockScenario::CdnRedirect => {
                b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nseg.ts\n".to_vec()
            }
            MockScenario::SeiCaptions => {
                b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nsei.ts\n".to_vec()
            }
            MockScenario::DashLive => {
                b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nseg.m4s\n".to_vec()
            }
            MockScenario::Aes128Hls => aes128_media_playlist(),
            MockScenario::MediaSeqGap => media_seq_gap_playlist(),
            _ => b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2.0,\nseg.ts\n".to_vec(),
        };
        return ("200 OK", body, "application/vnd.apple.mpegurl", None);
    }

    if std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
    {
        if matches!(scenario, MockScenario::Tr101290) {
            return ("200 OK", fixtures::tr101290_broken_ts(), "video/mp2t", None);
        }
        if matches!(scenario, MockScenario::SeiCaptions) || path.contains("sei") {
            return ("200 OK", fixtures::sei_caption_hdr_ts(), "video/mp2t", None);
        }
        let seq = SEGMENT_SEQ.fetch_add(1, Ordering::Relaxed);
        return (
            "200 OK",
            fixtures::minimal_ts_packet(seq),
            "video/mp2t",
            None,
        );
    }

    if std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("m4s"))
        || std::path::Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
    {
        if matches!(scenario, MockScenario::CorruptPssh) {
            return (
                "200 OK",
                corrupt_pssh_segment_bytes(),
                "video/mp4",
                None,
            );
        }
        if matches!(
            scenario,
            MockScenario::SeiCaptions | MockScenario::LlHlsFmp4 | MockScenario::DashLive
        ) {
            return ("200 OK", fixtures::sei_fmp4_m4s(), "video/mp4", None);
        }
    }

    ("404 Not Found", Vec::new(), "text/plain", None)
}

fn aes128_media_playlist() -> Vec<u8> {
    b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x00000000000000000000000000000001\n#EXTINF:2.0,\nseg.ts\n".to_vec()
}

fn media_seq_gap_playlist() -> Vec<u8> {
    let hit = MEDIA_SEQ_GAP_HITS.fetch_add(1, Ordering::SeqCst);
    let seq = if hit == 0 { 1 } else { 10 };
    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:{seq}\n#EXTINF:2.0,\nseg.ts\n"
    )
    .into_bytes()
}

fn ll_hls_master_playlist() -> Vec<u8> {
    b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401f,mp4a.40.2\"\nll/360.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\"\nll/720.m3u8\n".to_vec()
}

fn ll_hls_media_playlist() -> Vec<u8> {
    b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK=0.5\n#EXT-X-PART-INF:PART-TARGET=0.5\n#EXT-X-MAP:URI=\"init.m4s\"\n#EXT-X-PART:DURATION=0.5,URI=\"part0.m4s\"\n#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part1.m4s\"\n#EXTINF:2.0,\nseg.m4s\n".to_vec()
}

fn load_fixture_bytes(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    fs::read(path).unwrap_or_default()
}

pub fn corrupt_pssh_segment() -> String {
    String::from_utf8(corrupt_pssh_segment_bytes()).unwrap_or_else(|_| "bad".into())
}

fn corrupt_pssh_segment_bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 32];
    bytes[0..4].copy_from_slice(&32u32.to_be_bytes());
    bytes[4..8].copy_from_slice(b"pssh");
    bytes[8] = 1;
    bytes
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
    }

    #[tokio::test]
    async fn mock_tr101290_serves_broken_ts() {
        let server = MockStreamServer::start_with(MockScenario::Tr101290).await;
        let url = format!("{}/live.m3u8", server.base_url);
        let _ = reqwest::Client::new().get(&url).send().await;
        let seg = format!("{}/seg.ts", server.base_url);
        let bytes = reqwest::Client::new()
            .get(&seg)
            .send()
            .await
            .expect("get")
            .bytes()
            .await
            .expect("bytes");
        assert!(bytes.len() >= 188);
    }

    #[tokio::test]
    async fn mock_range_probe_returns_206() {
        let server = MockStreamServer::start_with(MockScenario::Tr101290).await;
        let url = format!("{}/seg.ts", server.base_url);
        let client = reqwest::Client::new();
        let resp = client
            .get(&url)
            .header("Range", "bytes=0-187")
            .send()
            .await
            .expect("get");
        assert_eq!(resp.status().as_u16(), 206);
    }

    #[tokio::test]
    async fn mock_aes128_playlist_and_key() {
        let server = MockStreamServer::start_with(MockScenario::Aes128Hls).await;
        let client = reqwest::Client::new();
        let pl = client
            .get(format!("{}/live.m3u8", server.base_url))
            .send()
            .await
            .expect("get")
            .text()
            .await
            .expect("text");
        assert!(pl.contains("#EXT-X-KEY:METHOD=AES-128"));
        assert!(pl.contains("URI=\"key.bin\""));
        let key = client
            .get(format!("{}/key.bin", server.base_url))
            .send()
            .await
            .expect("key")
            .bytes()
            .await
            .expect("bytes");
        assert_eq!(key.len(), 16);
    }

    #[tokio::test]
    async fn mock_cdn_redirect_hop() {
        let server = MockStreamServer::start_with(MockScenario::CdnRedirect).await;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        let resp = client
            .get(format!("{}/edge.m3u8", server.base_url))
            .send()
            .await
            .expect("get");
        assert_eq!(resp.status().as_u16(), 302);
        let loc = resp
            .headers()
            .get("location")
            .expect("location")
            .to_str()
            .expect("str");
        assert!(loc.contains("origin/live.m3u8"));
    }

    #[tokio::test]
    async fn mock_media_seq_gap_jumps() {
        MEDIA_SEQ_GAP_HITS.store(0, Ordering::SeqCst);
        let server = MockStreamServer::start_with(MockScenario::MediaSeqGap).await;
        let client = reqwest::Client::new();
        let url = format!("{}/live.m3u8", server.base_url);
        let first = client
            .get(&url)
            .send()
            .await
            .expect("get")
            .text()
            .await
            .expect("text");
        let second = client
            .get(&url)
            .send()
            .await
            .expect("get")
            .text()
            .await
            .expect("text");
        assert!(first.contains("#EXT-X-MEDIA-SEQUENCE:1"));
        assert!(second.contains("#EXT-X-MEDIA-SEQUENCE:10"));
    }
}

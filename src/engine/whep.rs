//! WHEP (WebRTC HTTP Egress Protocol) signaling probe without media decode.

use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr};
use reqwest::Client;
use serde::Serialize;
use url::Url;

const MIN_SDP_OFFER: &str = "v=0\r\n\
o=- 0 0 IN IP4 127.0.0.1\r\n\
s=streamtop-whep-probe\r\n\
t=0 0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=rtpmap:96 H264/90000\r\n\
a=recvonly\r\n";

#[derive(Debug, Clone, Serialize, Default)]
pub struct WhepProbeReport {
    pub endpoint: String,
    pub http_status: u16,
    pub signaling_ttfb_ms: u64,
    pub download_ms: u64,
    pub ready: bool,
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
    pub stream_ids: Vec<String>,
    pub ice_candidates: u32,
    pub location: Option<String>,
    pub error: Option<String>,
}

pub fn is_whep_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return false;
    }
    let Some(segments) = parsed.path_segments() else {
        return false;
    };
    let segments: Vec<&str> = segments.filter(|part| !part.is_empty()).collect();
    if segments.is_empty() {
        return false;
    }
    let playlist = segments.iter().any(|part| is_playlist_segment(part));
    if playlist {
        return false;
    }
    segments
        .iter()
        .any(|part| part.eq_ignore_ascii_case("whep"))
}

fn push_mline_payload_types(rest: &str, out: &mut Vec<String>) {
    // m=<media> <port> <proto> <fmt>...
    // `rest` is the line with the "m=audio" / "m=video" prefix removed.
    for pt in rest.split_whitespace().skip(2) {
        if !pt.is_empty() {
            out.push(pt.to_string());
        }
    }
}

fn is_playlist_segment(part: &str) -> bool {
    let Some((_, ext)) = part.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "m3u8" | "m3u" | "mpd" | "ts"
    )
}

/// POST SDP Offer to WHEP endpoint; inspect SDP Answer metadata only.
pub async fn probe_whep(
    client: &Client,
    endpoint: &str,
    timeout: Duration,
) -> Result<WhepProbeReport> {
    let started = Instant::now();
    let response = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/sdp")
        .header(reqwest::header::ACCEPT, "application/sdp")
        .timeout(timeout)
        .body(MIN_SDP_OFFER)
        .send()
        .await
        .wrap_err_with(|| format!("WHEP POST failed: {endpoint}"))?;
    let http_status = response.status().as_u16();
    let signaling_ttfb_ms = started.elapsed().as_millis() as u64;
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = response.text().await.unwrap_or_default();
    let download_ms = started.elapsed().as_millis() as u64;
    let answer_ok = matches!(http_status, 200 | 201) && !body.is_empty();
    let mut report = WhepProbeReport {
        endpoint: endpoint.to_string(),
        http_status,
        signaling_ttfb_ms,
        download_ms,
        ready: answer_ok,
        location,
        ..WhepProbeReport::default()
    };
    if report.ready {
        parse_sdp_answer(&body, &mut report);
    } else {
        report.error = Some(format!("WHEP HTTP {http_status}"));
    }
    Ok(report)
}

fn parse_sdp_answer(sdp: &str, report: &mut WhepProbeReport) {
    for line in sdp.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("a=rtpmap:") {
            let codec = rest.split_whitespace().next().unwrap_or(rest);
            if t.contains("audio") || rest.contains("opus") || rest.contains("AAC") {
                report.audio_codecs.push(codec.to_string());
            } else {
                report.video_codecs.push(codec.to_string());
            }
        } else if t.starts_with("a=candidate:") {
            report.ice_candidates = report.ice_candidates.saturating_add(1);
        } else if let Some(msid) = t.strip_prefix("a=msid:") {
            let id = msid.split_whitespace().next().unwrap_or(msid);
            if !id.is_empty() {
                report.stream_ids.push(id.to_string());
            }
        } else if let Some(m) = t.strip_prefix("m=audio") {
            push_mline_payload_types(m, &mut report.audio_codecs);
        } else if let Some(m) = t.strip_prefix("m=video") {
            push_mline_payload_types(m, &mut report.video_codecs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_whep_urls() {
        assert!(is_whep_url("https://cdn.example/whep/feed1"));
        assert!(is_whep_url("https://cdn.example/live/whep"));
        assert!(!is_whep_url("https://cdn.example/live.m3u8"));
        assert!(!is_whep_url("https://cdn.example/whep/live.m3u8"));
        assert!(!is_whep_url("https://cdn.example/live.m3u8?next=/whep"));
        assert!(!is_whep_url("https://cdn.example/notwhep"));
    }

    #[test]
    fn sdp_answer_parses_codecs_candidates_and_stream_ids() {
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 H264/90000\r\na=msid:stream1 track1\r\na=candidate:1\r\n";
        let mut report = WhepProbeReport::default();
        parse_sdp_answer(sdp, &mut report);
        assert_eq!(report.video_codecs, ["96", "96"]);
        assert_eq!(report.ice_candidates, 1);
        assert_eq!(report.stream_ids, vec!["stream1"]);
    }
}

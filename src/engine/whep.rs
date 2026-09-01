//! WHEP (WebRTC HTTP Egress Protocol) signaling probe without media decode.

use std::time::Instant;

use color_eyre::eyre::{Result, WrapErr};
use reqwest::Client;
use serde::Serialize;

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
    let lower = url.to_ascii_lowercase();
    lower.contains("/whep") || lower.ends_with("whep")
}

/// POST SDP Offer to WHEP endpoint; inspect SDP Answer metadata only.
pub async fn probe_whep(client: &Client, endpoint: &str) -> Result<WhepProbeReport> {
    let started = Instant::now();
    let response = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/sdp")
        .header(reqwest::header::ACCEPT, "application/sdp")
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
            if let Some(pt) = m.split_whitespace().nth(3) {
                report.audio_codecs.push(pt.to_string());
            }
        } else if let Some(m) = t.strip_prefix("m=video") {
            if let Some(pt) = m.split_whitespace().nth(3) {
                report.video_codecs.push(pt.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_whep_urls() {
        assert!(is_whep_url("https://cdn.example/whep/feed1"));
        assert!(!is_whep_url("https://cdn.example/live.m3u8"));
    }

    #[test]
    fn sdp_answer_parses_codecs_candidates_and_stream_ids() {
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 H264/90000\r\na=msid:stream1 track1\r\na=candidate:1\r\n";
        let mut report = WhepProbeReport::default();
        parse_sdp_answer(sdp, &mut report);
        assert!(!report.video_codecs.is_empty());
        assert_eq!(report.ice_candidates, 1);
        assert_eq!(report.stream_ids, vec!["stream1"]);
    }
}

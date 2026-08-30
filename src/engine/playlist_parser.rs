//! IPTV lineups and JSON/YAML channel catalogs.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{eyre, Result, WrapErr};
use serde::Deserialize;
use url::Url;

use crate::engine::dash::looks_like_dash;
use crate::models::ChannelEntry;

/// Result of detecting and parsing an input URL/file body.
#[derive(Debug, Clone)]
pub enum ParsedInput {
    /// HLS master/media or DASH MPD - open diagnostics for this URL.
    SingleStream { origin: String, url: String },
    /// Channel lineup → Channel Picker.
    IptvChannels {
        origin: String,
        channels: Vec<ChannelEntry>,
    },
}

#[derive(Debug, Deserialize)]
struct CatalogItem {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    stream: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    logo: Option<String>,
    #[serde(default)]
    tvg_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogRoot {
    #[serde(default)]
    channels: Vec<CatalogItem>,
}

pub fn looks_like_remote_url(input: &str) -> bool {
    let t = input.trim();
    t.starts_with("http://") || t.starts_with("https://")
}

pub fn path_to_file_url(path: &Path) -> Result<String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().wrap_err("cwd")?.join(path)
    };
    Url::from_file_path(&abs)
        .map(|u| u.to_string())
        .map_err(|()| eyre!("cannot convert path to file URL: {}", abs.display()))
}

/// Classify input bytes (IPTV / HLS / DASH / catalog).
pub fn detect_and_parse(
    origin: &str,
    body: &[u8],
    content_type: Option<&str>,
) -> Result<ParsedInput> {
    classify_source(origin, body, content_type)
}

pub fn classify_source(
    origin: &str,
    body: &[u8],
    content_type: Option<&str>,
) -> Result<ParsedInput> {
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    let lower_origin = origin.to_ascii_lowercase();
    let path_hint = origin_path_hint(origin);

    if trimmed.starts_with('#') || trimmed.contains("#EXT") {
        return classify_ext_playlist(origin, trimmed);
    }

    if looks_like_dash(origin, body, content_type)
        || trimmed.contains("<MPD")
        || trimmed.contains("<mpd")
    {
        return Ok(ParsedInput::SingleStream {
            origin: origin.to_string(),
            url: origin.to_string(),
        });
    }

    if is_json_hint(&lower_origin, path_hint.as_deref(), trimmed) {
        let channels = parse_json_catalog(trimmed, origin)?;
        return Ok(ParsedInput::IptvChannels {
            origin: origin.to_string(),
            channels,
        });
    }

    if is_yaml_hint(&lower_origin, path_hint.as_deref(), trimmed) {
        let channels = parse_yaml_catalog(trimmed, origin)?;
        return Ok(ParsedInput::IptvChannels {
            origin: origin.to_string(),
            channels,
        });
    }

    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        let channels = parse_json_catalog(trimmed, origin)?;
        return Ok(ParsedInput::IptvChannels {
            origin: origin.to_string(),
            channels,
        });
    }

    Ok(ParsedInput::SingleStream {
        origin: origin.to_string(),
        url: origin.to_string(),
    })
}

fn classify_ext_playlist(origin: &str, text: &str) -> Result<ParsedInput> {
    if is_iptv_channel_list(text) {
        let channels = parse_m3u_channels(text, origin);
        if channels.is_empty() {
            return Err(eyre!("M3U lineup contains no playable channel URLs"));
        }
        return Ok(ParsedInput::IptvChannels {
            origin: origin.to_string(),
            channels,
        });
    }

    if text.contains("#EXT-X-STREAM-INF") {
        return Ok(ParsedInput::SingleStream {
            origin: origin.to_string(),
            url: origin.to_string(),
        });
    }

    if is_hls_media_playlist(text) {
        return Ok(ParsedInput::SingleStream {
            origin: origin.to_string(),
            url: origin.to_string(),
        });
    }

    if text.contains("#EXTINF") {
        let channels = parse_m3u_channels(text, origin);
        if !channels.is_empty() {
            return Ok(ParsedInput::IptvChannels {
                origin: origin.to_string(),
                channels,
            });
        }
    }

    Ok(ParsedInput::SingleStream {
        origin: origin.to_string(),
        url: origin.to_string(),
    })
}

/// True when body looks like an IPTV channel list (not HLS media).
pub fn is_iptv_channel_list(text: &str) -> bool {
    if !text.contains("#EXTINF") {
        return false;
    }
    if text.contains("#EXT-X-STREAM-INF") {
        return false;
    }
    if text.contains("#EXT-X-TARGETDURATION") {
        return false;
    }
    if text.contains("#EXT-X-MEDIA-SEQUENCE")
        || text.contains("#EXT-X-MAP:")
        || text.contains("#EXT-X-PART:")
        || text.contains("#EXT-X-SERVER-CONTROL")
        || text.contains("#EXT-X-PLAYLIST-TYPE")
    {
        return false;
    }
    let _has_iptv_attrs = text.contains("tvg-name")
        || text.contains("tvg-id")
        || text.contains("group-title")
        || text.contains("tvg-logo");
    true
}

fn is_hls_media_playlist(text: &str) -> bool {
    text.contains("#EXT-X-TARGETDURATION")
        || text.contains("#EXT-X-MEDIA-SEQUENCE")
        || text.contains("#EXT-X-MAP:")
        || text.contains("#EXT-X-PART:")
        || text.contains("#EXT-X-SERVER-CONTROL")
        || text.contains("#EXT-X-PLAYLIST-TYPE")
}

fn origin_path_hint(origin: &str) -> Option<String> {
    if let Ok(u) = Url::parse(origin) {
        return u
            .path_segments()
            .and_then(|mut s| s.next_back())
            .map(str::to_ascii_lowercase);
    }
    Path::new(origin)
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
}

fn is_json_hint(origin: &str, file: Option<&str>, body: &str) -> bool {
    origin.contains(".json")
        || file.is_some_and(|f| {
            std::path::Path::new(f)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        || ((body.starts_with('{') || body.starts_with('[')) && !body.contains("#EXT"))
}

fn is_yaml_hint(origin: &str, file: Option<&str>, body: &str) -> bool {
    origin.contains(".yaml")
        || origin.contains(".yml")
        || file.is_some_and(|f| {
            std::path::Path::new(f).extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
            })
        })
        || body.starts_with("---")
}

/// Parse IPTV M3U into channel entries.
pub fn parse_m3u_channels(text: &str, base: &str) -> Vec<ChannelEntry> {
    let base_url = Url::parse(base).ok();
    let mut channels = Vec::new();
    #[allow(clippy::type_complexity)]
    let mut pending: Option<(String, Option<String>, Option<String>, Option<String>)> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("#EXTM3U") {
            continue;
        }
        if line.starts_with("#EXTINF") {
            pending = Some(parse_extinf(line));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let url = resolve_channel_url(line, base_url.as_ref());
        if url.is_empty() {
            continue;
        }
        let (name, group, logo, tvg_id) = pending
            .take()
            .unwrap_or_else(|| (format!("Channel {}", channels.len() + 1), None, None, None));
        channels.push(ChannelEntry {
            name,
            url,
            group,
            logo,
            tvg_id,
        });
    }

    channels
}

fn parse_extinf(line: &str) -> (String, Option<String>, Option<String>, Option<String>) {
    let rest = line.strip_prefix("#EXTINF:").unwrap_or(line);
    let (attrs_part, display) = match rest.rsplit_once(',') {
        Some((a, d)) => (a, d.trim()),
        None => (rest, ""),
    };

    let tvg_name = attr(attrs_part, "tvg-name");
    let group = attr(attrs_part, "group-title");
    let logo = attr(attrs_part, "tvg-logo");
    let tvg_id = attr(attrs_part, "tvg-id");

    let name = tvg_name
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if display.is_empty() {
                None
            } else {
                Some(display.to_string())
            }
        })
        .unwrap_or_else(|| "Untitled".into());

    (name, group, logo, tvg_id)
}

fn attr(hay: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let idx = hay.find(&needle)?;
    let after = &hay[idx + needle.len()..];
    if let Some(rest) = after.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }
    if let Some(rest) = after.strip_prefix('\'') {
        let end = rest.find('\'')?;
        return Some(rest[..end].to_string());
    }
    let end = after
        .find(|c: char| c.is_whitespace() || c == ',')
        .unwrap_or(after.len());
    Some(after[..end].to_string())
}

fn parse_json_catalog(text: &str, base: &str) -> Result<Vec<ChannelEntry>> {
    if let Ok(items) = serde_json::from_str::<Vec<CatalogItem>>(text) {
        return items_to_channels(items, base);
    }
    if let Ok(root) = serde_json::from_str::<CatalogRoot>(text) {
        if !root.channels.is_empty() {
            return items_to_channels(root.channels, base);
        }
    }
    if let Ok(item) = serde_json::from_str::<CatalogItem>(text) {
        return items_to_channels(vec![item], base);
    }
    Err(eyre!("JSON channel catalog parse error"))
}

fn parse_yaml_catalog(text: &str, base: &str) -> Result<Vec<ChannelEntry>> {
    if let Ok(items) = serde_yaml::from_str::<Vec<CatalogItem>>(text) {
        return items_to_channels(items, base);
    }
    if let Ok(root) = serde_yaml::from_str::<CatalogRoot>(text) {
        if !root.channels.is_empty() {
            return items_to_channels(root.channels, base);
        }
    }
    Err(eyre!("YAML channel catalog parse error"))
}

fn items_to_channels(items: Vec<CatalogItem>, base: &str) -> Result<Vec<ChannelEntry>> {
    let base_url = Url::parse(base).ok();
    let mut channels = Vec::new();
    for item in items {
        let url_raw = item
            .url
            .or(item.stream)
            .or(item.uri)
            .ok_or_else(|| eyre!("catalog item missing url"))?;
        let url = resolve_channel_url(&url_raw, base_url.as_ref());
        let name = item
            .name
            .or(item.title)
            .unwrap_or_else(|| format!("Channel {}", channels.len() + 1));
        channels.push(ChannelEntry {
            name,
            url,
            group: item.group.or(item.category),
            logo: item.logo,
            tvg_id: item.tvg_id,
        });
    }
    if channels.is_empty() {
        return Err(eyre!("channel catalog is empty"));
    }
    Ok(channels)
}

fn resolve_channel_url(href: &str, base: Option<&Url>) -> String {
    let href = href.trim();
    if let Ok(u) = Url::parse(href) {
        return u.to_string();
    }
    if let Some(base) = base {
        if let Ok(u) = base.join(href) {
            return u.to_string();
        }
    }
    href.to_string()
}

pub fn local_path_from_url(url: &str) -> Option<PathBuf> {
    let u = Url::parse(url).ok()?;
    if u.scheme() != "file" {
        return None;
    }
    u.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_iptv_m3u() {
        let body = r#"#EXTM3U
#EXTINF:-1 tvg-name="beIN Sports 1" group-title="Spor" tvg-logo="http://logo/bein.png",beIN Sports 1
https://edge.example/bein.m3u8
#EXTINF:-1 group-title="Haber",TRT Haber
https://edge.example/trt.m3u8
"#;
        let src = detect_and_parse("file:///lineup.m3u", body.as_bytes(), None).unwrap();
        assert!(matches!(src, ParsedInput::IptvChannels { .. }));
        if let ParsedInput::IptvChannels { channels, .. } = src {
            assert_eq!(channels.len(), 2);
            assert_eq!(channels[0].name, "beIN Sports 1");
            assert_eq!(channels[0].group.as_deref(), Some("Spor"));
            assert_eq!(channels[1].name, "TRT Haber");
        }
    }

    #[test]
    fn iptv_without_targetduration_never_single_stream() {
        let body = b"#EXTM3U\n#EXTINF:-1,TRT 1\nhttps://tv-trt1.example/master.m3u8\n";
        assert!(is_iptv_channel_list(&String::from_utf8_lossy(body)));
        let src = detect_and_parse(
            "https://raw.githubusercontent.com/iptv-org/iptv/master/streams/tr.m3u",
            body,
            Some("application/vnd.apple.mpegurl"),
        )
        .unwrap();
        assert!(matches!(src, ParsedInput::IptvChannels { .. }));
    }

    #[test]
    fn iptv_with_tvg_attrs() {
        let body =
            b"#EXTM3U\n#EXTINF:-1 tvg-id=\"trt1.tr\" tvg-name=\"TRT 1\",TRT 1\nhttps://a/b.m3u8\n";
        assert!(is_iptv_channel_list(&String::from_utf8_lossy(body)));
        let src = detect_and_parse("https://x/tr.m3u", body, None).unwrap();
        assert!(matches!(src, ParsedInput::IptvChannels { .. }));
        if let ParsedInput::IptvChannels { channels, .. } = src {
            assert_eq!(channels.len(), 1);
        }
    }

    #[test]
    fn hls_master_is_single() {
        let body = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000\nmedia.m3u8\n";
        let src = detect_and_parse("https://ex.com/master.m3u8", body, None).unwrap();
        assert!(matches!(src, ParsedInput::SingleStream { .. }));
    }

    #[test]
    fn hls_media_with_extinf_is_single_not_iptv() {
        let body =
            b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:10\n#EXTINF:6.0,\nseg.ts\n";
        assert!(!is_iptv_channel_list(&String::from_utf8_lossy(body)));
        let src = detect_and_parse("https://ex.com/media.m3u8", body, None).unwrap();
        assert!(matches!(src, ParsedInput::SingleStream { .. }));
    }

    #[test]
    fn json_catalog() {
        let body = r#"[{"name":"beIN Sports 1","url":"https://ex.com/a.m3u8","group":"Spor"}]"#;
        let src = detect_and_parse("channels.json", body.as_bytes(), None).unwrap();
        assert!(matches!(src, ParsedInput::IptvChannels { .. }));
        if let ParsedInput::IptvChannels { channels, .. } = src {
            assert_eq!(channels[0].name, "beIN Sports 1");
        }
    }
}

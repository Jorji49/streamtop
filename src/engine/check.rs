//! `streamtop check`: batch reachability check for IPTV playlists and channel catalogs.

use std::fmt::Write as _;

use color_eyre::eyre::Result;

use crate::engine::audit::{render_csv, render_json};
use crate::models::{AuditReport, AuditVerdict, ChannelEntry};

/// Output format for `streamtop check`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum CheckFormat {
    /// Colored table on stdout.
    #[default]
    Table,
    /// JSON report (URLs redacted).
    Json,
    /// CSV report (URLs redacted).
    Csv,
}

/// Render a machine-readable report. Returns `None` for [`CheckFormat::Table`],
/// which is printed by [`crate::engine::audit::print_matrix`].
pub fn render_report(report: &AuditReport, format: CheckFormat) -> Result<Option<String>> {
    match format {
        CheckFormat::Table => Ok(None),
        CheckFormat::Json => render_json(report).map(Some),
        CheckFormat::Csv => {
            let mut buf = Vec::new();
            render_csv(report, &mut buf)?;
            Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
        }
    }
}

/// True when the run should exit non-zero: any dead entry, or any stall when `strict`.
pub fn should_fail(report: &AuditReport, strict: bool) -> bool {
    report.errors > 0 || (strict && report.stalls > 0)
}

/// Build an M3U playlist with the entries that are not dead (LIVE and VOD, plus STALL unless `strict`).
///
/// `channels` must be the list that produced `report`, in the same order.
/// URLs are written unredacted because the output is meant to be played.
pub fn alive_m3u(channels: &[ChannelEntry], report: &AuditReport, strict: bool) -> String {
    let mut out = String::from("#EXTM3U\n");
    for (ch, row) in channels.iter().zip(&report.channels) {
        let keep = match row.verdict {
            AuditVerdict::Live | AuditVerdict::Vod => true,
            AuditVerdict::Stall => !strict,
            AuditVerdict::Error => false,
        };
        if !keep {
            continue;
        }
        out.push_str("#EXTINF:-1");
        if let Some(id) = &ch.tvg_id {
            let _ = write!(out, " tvg-id=\"{}\"", attr(id));
        }
        if let Some(logo) = &ch.logo {
            let _ = write!(out, " tvg-logo=\"{}\"", attr(logo));
        }
        if let Some(group) = &ch.group {
            let _ = write!(out, " group-title=\"{}\"", attr(group));
        }
        let _ = writeln!(out, ",{}", ch.name.replace(['\r', '\n'], " "));
        let _ = writeln!(out, "{}", ch.url.trim());
    }
    out
}

fn attr(s: &str) -> String {
    s.replace(['"', '\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AuditRow;
    use chrono::Utc;

    fn ch(name: &str, url: &str) -> ChannelEntry {
        ChannelEntry {
            name: name.into(),
            url: url.into(),
            group: Some("News".into()),
            logo: None,
            tvg_id: Some("id1".into()),
        }
    }

    fn row(c: &ChannelEntry, verdict: AuditVerdict) -> AuditRow {
        AuditRow {
            name: c.name.clone(),
            group: c.group.clone(),
            url: c.url.clone(),
            verdict,
            http_status: Some(200),
            protocol: Some("HLS".into()),
            cdn: "-".into(),
            ttfb_ms: Some(10),
            bitrate_profiles: vec![],
            has_pdt: false,
            error: None,
        }
    }

    fn report(channels: &[ChannelEntry], verdicts: &[AuditVerdict]) -> AuditReport {
        let rows: Vec<AuditRow> = channels
            .iter()
            .zip(verdicts)
            .map(|(c, v)| row(c, *v))
            .collect();
        let n = |v| verdicts.iter().filter(|x| **x == v).count();
        AuditReport {
            captured_at: Utc::now(),
            source: "list.m3u".into(),
            total: rows.len(),
            live: n(AuditVerdict::Live),
            vod: n(AuditVerdict::Vod),
            errors: n(AuditVerdict::Error),
            stalls: n(AuditVerdict::Stall),
            channels: rows,
        }
    }

    #[test]
    fn alive_playlist_skips_dead_and_keeps_attrs() {
        let chans = vec![
            ch("A", "http://a/x.m3u8?token=t"),
            ch("B", "http://b/x.m3u8"),
            ch("C", "http://c/x.m3u8"),
        ];
        let rep = report(
            &chans,
            &[AuditVerdict::Live, AuditVerdict::Error, AuditVerdict::Stall],
        );
        let m3u = alive_m3u(&chans, &rep, false);
        assert!(m3u.starts_with("#EXTM3U\n"));
        assert!(m3u.contains("tvg-id=\"id1\" group-title=\"News\",A\nhttp://a/x.m3u8?token=t\n"));
        assert!(!m3u.contains("http://b/"));
        assert!(m3u.contains("http://c/"));
        assert!(!alive_m3u(&chans, &rep, true).contains("http://c/"));
    }

    #[test]
    fn fail_rules() {
        let chans = vec![ch("A", "http://a"), ch("B", "http://b")];
        let ok = report(&chans, &[AuditVerdict::Live, AuditVerdict::Stall]);
        assert!(!should_fail(&ok, false));
        assert!(should_fail(&ok, true));
        let dead = report(&chans, &[AuditVerdict::Live, AuditVerdict::Error]);
        assert!(should_fail(&dead, false));
    }

    #[test]
    fn csv_and_json_redact_tokens() {
        let chans = vec![ch("A", "http://a/x.m3u8?token=sekrit")];
        let rep = report(&chans, &[AuditVerdict::Live]);
        let csv = render_report(&rep, CheckFormat::Csv).unwrap().unwrap();
        assert!(csv.starts_with("name,group,url,verdict"));
        assert!(!csv.contains("sekrit"));
        let json = render_report(&rep, CheckFormat::Json).unwrap().unwrap();
        assert!(!json.contains("sekrit"));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["live"], 1);
        assert!(render_report(&rep, CheckFormat::Table).unwrap().is_none());
    }

    #[test]
    fn vod_entries_are_alive() {
        let chans = vec![ch("a", "https://cdn/a.m3u8"), ch("b", "https://cdn/b.m3u8")];
        let rep = report(&chans, &[AuditVerdict::Vod, AuditVerdict::Error]);
        assert_eq!(rep.vod, 1);
        let m3u = alive_m3u(&chans, &rep, true);
        assert!(m3u.contains("https://cdn/a.m3u8"), "{m3u}");
        assert!(!m3u.contains("https://cdn/b.m3u8"), "{m3u}");
    }
}

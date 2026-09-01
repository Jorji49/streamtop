//! GitHub Actions step summary markdown generator.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::Result;

use crate::engine::budget::BudgetReport;
use crate::models::{AbrHealth, HealthReport, Tr101290Report};

#[derive(Debug, Clone)]
pub struct GithubSummaryInput<'a> {
    pub url: &'a str,
    pub health: &'a HealthReport,
    pub abr: &'a AbrHealth,
    pub budget: Option<&'a BudgetReport>,
    pub rtf: Option<f32>,
    pub part_rtf: Option<f32>,
    pub tr101290: Option<&'a Tr101290Report>,
    pub doh_ms: Option<u64>,
}

pub fn resolve_github_summary_path(cli: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = cli {
        return Some(path.to_path_buf());
    }
    env::var("GITHUB_STEP_SUMMARY")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

pub fn maybe_write_github_summary(
    input: &GithubSummaryInput<'_>,
    cli_path: Option<&Path>,
) -> Result<()> {
    let Some(path) = resolve_github_summary_path(cli_path) else {
        return Ok(());
    };
    write_github_step_summary(&path, input)
}

pub fn write_github_step_summary(path: &Path, input: &GithubSummaryInput<'_>) -> Result<()> {
    let mut out = String::new();
    out.push_str("## streamtop probe summary\n\n");
    let _ = writeln!(out, "**URL:** `{url}`\n", url = input.url);
    out.push_str("| Metric | Value |\n|--------|-------|\n");
    let _ = writeln!(
        out,
        "| SHI | {} ({}) |",
        input.health.score, input.health.label
    );
    if let Some(rtf) = input.rtf {
        let _ = writeln!(out, "| RTF (segment) | {rtf:.3} |");
    }
    if let Some(part) = input.part_rtf {
        let _ = writeln!(out, "| Part RTF | {part:.3} |");
    }
    if let Some(ms) = input.doh_ms {
        let _ = writeln!(out, "| DoH latency | {ms}ms |");
    }
    if let Some(tr) = input.tr101290 {
        let _ = writeln!(out, "| TR 101 290 P1 | {} |", tr.p1_violations);
        let _ = writeln!(out, "| TR 101 290 P2 | {} |", tr.p2_violations);
    }

    if !input.abr.warnings.is_empty() {
        out.push_str("\n### ABR variant ladder\n\n");
        out.push_str("| Warning |\n|---------|\n");
        for w in &input.abr.warnings {
            let _ = writeln!(out, "| {w} |");
        }
    }

    if let Some(b) = input.budget {
        out.push_str("\n### Budget\n\n");
        let _ = writeln!(out, "Verdict: **{}**\n", b.verdict);
        if b.breaches.is_empty() {
            out.push_str("No threshold breaches.\n");
        } else {
            out.push_str("| Reason | Message |\n|--------|--------|\n");
            for br in &b.breaches {
                let _ = writeln!(out, "| `{}` | {} |", br.reason, br.message);
            }
        }
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::HealthReport;

    #[test]
    fn github_summary_includes_shi_and_rtf() {
        let health = HealthReport::perfect();
        let abr = AbrHealth::default();
        let input = GithubSummaryInput {
            url: "https://example.test/live.m3u8",
            health: &health,
            abr: &abr,
            budget: None,
            rtf: Some(0.42),
            part_rtf: Some(0.88),
            tr101290: None,
            doh_ms: Some(12),
        };
        let dir = std::env::temp_dir().join("streamtop_gh_summary_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("summary.md");
        write_github_step_summary(&path, &input).expect("write");
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("SHI"));
        assert!(text.contains("Part RTF"));
        assert!(text.contains("DoH latency"));
        let _ = fs::remove_dir_all(&dir);
    }
}

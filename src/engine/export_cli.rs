//! Unified `--export FORMAT[:FILE]` parsing.

use std::path::PathBuf;

use color_eyre::eyre::{eyre, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    ReportHtml,
    ReportJson,
    Curl,
    Har,
    Incident,
    Grafana,
    Sarif,
}

impl ExportFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReportHtml => "report-html",
            Self::ReportJson => "report-json",
            Self::Curl => "curl",
            Self::Har => "har",
            Self::Incident => "incident",
            Self::Grafana => "grafana",
            Self::Sarif => "sarif",
        }
    }

    fn from_token(token: &str) -> Result<Self> {
        match token {
            "report-html" | "report_html" | "html" => Ok(Self::ReportHtml),
            "report-json" | "report_json" | "json-report" => Ok(Self::ReportJson),
            "curl" => Ok(Self::Curl),
            "har" => Ok(Self::Har),
            "incident" => Ok(Self::Incident),
            "grafana" => Ok(Self::Grafana),
            "sarif" => Ok(Self::Sarif),
            other => Err(eyre!(
                "unknown export format {other:?}; expected report-html, report-json, curl, har, incident, grafana, sarif"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExport {
    pub format: ExportFormat,
    pub path: Option<PathBuf>,
}

/// Parse `--export format` or `--export format:path`.
pub fn parse_export_spec(raw: &str) -> Result<ResolvedExport> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(eyre!("--export value is empty"));
    }
    let (fmt, path) = if let Some((fmt, rest)) = trimmed.split_once(':') {
        let p = rest.trim();
        if p.is_empty() {
            (fmt.trim(), None)
        } else {
            (fmt.trim(), Some(PathBuf::from(p)))
        }
    } else {
        (trimmed, None)
    };
    Ok(ResolvedExport {
        format: ExportFormat::from_token(fmt)?,
        path,
    })
}

#[derive(Debug, Default)]
pub struct ExportPlan {
    pub exports: Vec<ResolvedExport>,
}

impl ExportPlan {
    pub fn push(&mut self, item: ResolvedExport) {
        if !self
            .exports
            .iter()
            .any(|e| e.format == item.format && e.path == item.path)
        {
            self.exports.push(item);
        }
    }

    pub fn wants_any(&self) -> bool {
        !self.exports.is_empty()
    }

    pub fn wants_curl_or_har(&self) -> bool {
        self.exports
            .iter()
            .any(|e| matches!(e.format, ExportFormat::Curl | ExportFormat::Har))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_export_with_optional_path() {
        let spec = parse_export_spec("har:out.har").expect("har");
        assert_eq!(spec.format, ExportFormat::Har);
        assert_eq!(spec.path, Some(PathBuf::from("out.har")));

        let spec = parse_export_spec("curl").expect("curl");
        assert_eq!(spec.format, ExportFormat::Curl);
        assert!(spec.path.is_none());
    }

    #[test]
    fn parse_sarif_and_grafana_aliases() {
        assert_eq!(
            parse_export_spec("sarif:findings.sarif")
                .expect("sarif")
                .format,
            ExportFormat::Sarif
        );
        assert_eq!(
            parse_export_spec("report-json").expect("json").format,
            ExportFormat::ReportJson
        );
    }
}

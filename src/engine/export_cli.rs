//! Unified `--export FORMAT[:FILE]` parsing (v1.4.0).

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
    pub deprecated_via: Option<&'static str>,
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
        deprecated_via: None,
    })
}

#[derive(Debug, Default)]
pub struct ExportPlan {
    pub exports: Vec<ResolvedExport>,
}

impl ExportPlan {
    #[must_use]
    pub fn merge_legacy(
        mut self,
        export_report: Option<&PathBuf>,
        export_curl: bool,
        export_har: Option<&PathBuf>,
        export_incident: Option<&str>,
        export_grafana: bool,
    ) -> Self {
        if export_grafana {
            self.push_deprecated(ExportFormat::Grafana, None, "--export-grafana");
        }
        if export_curl {
            self.push_deprecated(ExportFormat::Curl, None, "--export-curl");
        }
        if let Some(path) = export_har {
            self.push_deprecated(ExportFormat::Har, Some(path.clone()), "--export-har");
        }
        if let Some(path) = export_report {
            let fmt = if path.extension().is_some_and(|e| e == "json") {
                ExportFormat::ReportJson
            } else {
                ExportFormat::ReportHtml
            };
            self.push_deprecated(fmt, Some(path.clone()), "--export-report");
        }
        if let Some(path) = export_incident {
            let p = if path.is_empty() {
                None
            } else {
                Some(PathBuf::from(path))
            };
            self.push_deprecated(ExportFormat::Incident, p, "--export-incident");
        }
        self
    }

    pub fn push(&mut self, item: ResolvedExport) {
        if !self.exports.iter().any(|e| e.format == item.format && e.path == item.path) {
            self.exports.push(item);
        }
    }

    fn push_deprecated(
        &mut self,
        format: ExportFormat,
        path: Option<PathBuf>,
        flag: &'static str,
    ) {
        self.push(ResolvedExport {
            format,
            path,
            deprecated_via: Some(flag),
        });
    }

    pub fn wants_any(&self) -> bool {
        !self.exports.is_empty()
    }

    pub fn wants_curl_or_har(&self) -> bool {
        self.exports.iter().any(|e| {
            matches!(e.format, ExportFormat::Curl | ExportFormat::Har)
        })
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

    #[test]
    fn legacy_merge_deduplicates() {
        let plan = ExportPlan::default().merge_legacy(None, true, None, None, false);
        assert_eq!(plan.exports.len(), 1);
        assert_eq!(plan.exports[0].format, ExportFormat::Curl);
        assert_eq!(plan.exports[0].deprecated_via, Some("--export-curl"));
    }
}

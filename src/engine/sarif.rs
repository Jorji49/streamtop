//! OASIS SARIF v2.1.0 export for GitHub Code Scanning.

use color_eyre::eyre::Result;
use serde::Serialize;

use crate::models::{DiagnosticFinding, SpecViolation};

const SARIF_SCHEMA: &str =
    "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json";

#[derive(Debug, Serialize)]
pub struct SarifLog {
    pub schema: &'static str,
    pub version: &'static str,
    pub runs: Vec<SarifRun>,
}

#[derive(Debug, Serialize)]
pub struct SarifRun {
    pub tool: SarifTool,
    pub results: Vec<SarifResult>,
}

#[derive(Debug, Serialize)]
pub struct SarifTool {
    pub driver: SarifDriver,
}

#[derive(Debug, Serialize)]
pub struct SarifDriver {
    pub name: &'static str,
    pub version: &'static str,
    pub information_uri: &'static str,
}

#[derive(Debug, Serialize)]
pub struct SarifResult {
    #[serde(rename = "ruleId")]
    pub rule_id: String,
    pub level: &'static str,
    pub message: SarifMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<SarifProperties>,
}

#[derive(Debug, Serialize)]
pub struct SarifMessage {
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct SarifProperties {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub standard: String,
}

pub fn build_sarif(findings: &[DiagnosticFinding], violations: &[SpecViolation]) -> SarifLog {
    let mut results = Vec::new();
    for f in findings {
        results.push(finding_to_result(f));
    }
    for v in violations {
        results.push(SarifResult {
            rule_id: v.rule.clone(),
            level: sarif_level(&v.severity),
            message: SarifMessage {
                text: v.message.clone(),
            },
            properties: Some(SarifProperties {
                reason: None,
                standard: v.standard.clone(),
            }),
        });
    }
    SarifLog {
        schema: SARIF_SCHEMA,
        version: "2.1.0",
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: "streamtop",
                    version: env!("CARGO_PKG_VERSION"),
                    information_uri: "https://github.com/Jorji49/streamtop",
                },
            },
            results,
        }],
    }
}

fn finding_to_result(f: &DiagnosticFinding) -> SarifResult {
    SarifResult {
        rule_id: f.reason.clone().unwrap_or_else(|| f.rule.clone()),
        level: match f.severity {
            crate::models::DiagSeverity::Error => "error",
            crate::models::DiagSeverity::Warn => "warning",
            crate::models::DiagSeverity::Info => "note",
        },
        message: SarifMessage {
            text: f.message.clone(),
        },
        properties: Some(SarifProperties {
            reason: f.reason.clone(),
            standard: "STREAM".into(),
        }),
    }
}

fn sarif_level(severity: &str) -> &'static str {
    match severity {
        "ERROR" => "error",
        "WARNING" => "warning",
        _ => "note",
    }
}

pub fn render_sarif_json(
    findings: &[DiagnosticFinding],
    violations: &[SpecViolation],
) -> Result<String> {
    let log = build_sarif(findings, violations);
    Ok(serde_json::to_string_pretty(&log)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DiagCategory, DiagSeverity};

    #[test]
    fn sarif_schema_fields_present() {
        let f = DiagnosticFinding {
            category: DiagCategory::Cdn,
            severity: DiagSeverity::Warn,
            rule: "CACHE_MISS".into(),
            message: "seq=1 MISS".into(),
            reason: Some("ERR_CDN_CACHE_MISS".into()),
        };
        let json = render_sarif_json(&[f], &[]).expect("sarif");
        assert!(json.contains("\"version\": \"2.1.0\""));
        assert!(json.contains("ERR_CDN_CACHE_MISS"));
        assert!(json.contains("\"ruleId\": \"ERR_CDN_CACHE_MISS\""));
    }
}

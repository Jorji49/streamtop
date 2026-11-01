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
    // Spec violations are usually derived from the findings. Report each
    // problem once and take the standard from the matching violation.
    let same =
        |f: &DiagnosticFinding, v: &SpecViolation| f.rule == v.rule && f.message == v.message;
    let mut results = Vec::new();
    for f in findings {
        let mut r = finding_to_result(f);
        if let Some(v) = violations.iter().find(|v| same(f, v)) {
            if let Some(p) = r.properties.as_mut() {
                p.standard.clone_from(&v.standard);
            }
        }
        results.push(r);
    }
    for v in violations
        .iter()
        .filter(|v| !findings.iter().any(|f| same(f, v)))
    {
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

    #[test]
    fn derived_violations_are_not_reported_twice() {
        let f = DiagnosticFinding {
            category: DiagCategory::Cdn,
            severity: DiagSeverity::Warn,
            rule: "CACHE_MISS".into(),
            message: "seq=1 MISS".into(),
            reason: Some("ERR_CDN_CACHE_MISS".into()),
        };
        let v = SpecViolation::from_finding(&f);
        let log = build_sarif(std::slice::from_ref(&f), &[v]);
        assert_eq!(log.runs[0].results.len(), 1);
        let props = log.runs[0].results[0].properties.as_ref().expect("props");
        assert_eq!(props.standard, "CDN");
    }
}

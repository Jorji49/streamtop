//! CI stream budget mode: threshold assertions with structured JSON output.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use serde::Serialize;
use tokio::time::timeout;

use crate::engine::poller::ManifestPoller;
use crate::models::{DiagnosticReasonCode, DiagCategory, StreamEvent, EVENT_CHANNEL_CAPACITY};
use crate::ui::app::SessionOpts;

#[derive(Debug, Clone, Default)]
pub struct BudgetOpts {
    pub max_rtf: Option<f32>,
    pub max_ttfb_ms: Option<u64>,
    pub max_cc_errors: Option<u32>,
    pub max_drift_ms: Option<i64>,
    pub duration_secs: u64,
}

impl BudgetOpts {
    pub fn active(&self) -> bool {
        self.max_rtf.is_some()
            || self.max_ttfb_ms.is_some()
            || self.max_cc_errors.is_some()
            || self.max_drift_ms.is_some()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BudgetReport {
    pub ok: bool,
    pub verdict: &'static str,
    pub url: String,
    pub duration_secs: u64,
    pub breaches: Vec<BudgetBreach>,
    pub last_rtf: Option<f32>,
    pub last_ttfb_ms: Option<u64>,
    pub cc_errors: u32,
    pub subtitle_drift_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BudgetBreach {
    pub reason: &'static str,
    pub message: String,
}

pub async fn run_budget(
    url: String,
    session: SessionOpts,
    budget: BudgetOpts,
    github_summary: Option<&Path>,
) -> Result<ExitCode> {
    let dur = budget.duration_secs.max(1);
    let (tx, mut rx) = tokio::sync::mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let poller = crate::engine::session_poller::apply_session_doh(
        ManifestPoller::new(
            url.as_str(),
            &session.headers,
            session.user_agent.as_deref(),
            session.interval_ms,
            session.probe_headers,
            session.probe_drm,
            tx,
        )?
        .with_diagnostics(&crate::engine::poller::DiagnosticOpts {
            tr101290: session.tr101290,
            probe_sei: session.probe_sei,
            simulate_player: session.simulate_player,
            throttle_kbps: session.throttle_kbps,
            simulated_rtt_ms: session.simulated_rtt_ms,
        }),
        &session,
    )?;

    let handle = tokio::spawn(async move {
        let () = poller.run().await;
    });

    let deadline = Instant::now() + Duration::from_secs(dur);
    let mut last_rtf: Option<f32> = None;
    let mut last_ttfb: Option<u64> = None;
    let mut cc_errors = 0u32;
    let mut subtitle_drift_ms: Option<i64> = None;
    let mut breaches: Vec<BudgetBreach> = Vec::new();
    let mut health = crate::models::HealthReport::perfect();
    let mut abr_health = crate::models::AbrHealth::default();
    let mut tr101290: Option<crate::models::Tr101290Report> = None;
    let mut last_doh_ms: Option<u64> = None;
    let mut last_part_rtf: Option<f32> = None;

    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, rx.recv()).await {
            Ok(Some(ev)) => match ev {
                StreamEvent::Health(h) => health = h,
                StreamEvent::AbrHealth(a) => abr_health = a,
                StreamEvent::LlHlsPart(p) => last_part_rtf = p.part_dl_duration_ratio,
                StreamEvent::Segment(s) => {
                    last_ttfb = Some(s.ttfb_ms);
                    last_rtf = s.dl_to_dur_ratio;
                    if let Some(net) = &s.network {
                        last_doh_ms = net.doh_ms.or(last_doh_ms);
                    }
                    if let (Some(max), Some(rtf)) = (budget.max_rtf, s.dl_to_dur_ratio) {
                        if rtf > max {
                            breaches.push(BudgetBreach {
                                reason: DiagnosticReasonCode::ErrBudgetRtfExceeded.as_str(),
                                message: format!("RTF {rtf:.3} > budget {max:.3}"),
                            });
                        }
                    }
                    if let Some(max) = budget.max_ttfb_ms {
                        if s.ttfb_ms > max {
                            breaches.push(BudgetBreach {
                                reason: DiagnosticReasonCode::ErrBudgetTtfbExceeded.as_str(),
                                message: format!("TTFB {}ms > budget {max}ms", s.ttfb_ms),
                            });
                        }
                    }
                }
                StreamEvent::Tr101290(r) => {
                    tr101290 = Some(r.clone());
                    cc_errors = r.cc_errors;
                    if let Some(max) = budget.max_cc_errors {
                        if r.cc_errors > max {
                            breaches.push(BudgetBreach {
                                reason: DiagnosticReasonCode::ErrBudgetCcExceeded.as_str(),
                                message: format!("CC errors {} > budget {max}", r.cc_errors),
                            });
                        }
                    }
                }
                StreamEvent::Log {
                    category: DiagCategory::AvSync,
                    message,
                    ..
                } => {
                    if let Some(drift) = crate::engine::summary::parse_subtitle_drift_ms(&message) {
                        subtitle_drift_ms = Some(drift);
                        if let Some(max) = budget.max_drift_ms {
                            if drift.abs() > max {
                                breaches.push(BudgetBreach {
                                    reason: DiagnosticReasonCode::ErrBudgetDriftExceeded.as_str(),
                                    message: format!("drift {drift}ms exceeds budget {max}ms"),
                                });
                            }
                        }
                    }
                }
                _ => {}
            },
            Ok(None) | Err(_) => break,
        }
        if !breaches.is_empty() {
            break;
        }
    }

    handle.abort();

    let ok = breaches.is_empty();
    let report = BudgetReport {
        ok,
        verdict: if ok { "PASS" } else { "FAIL" },
        url,
        duration_secs: dur,
        breaches,
        last_rtf,
        last_ttfb_ms: last_ttfb,
        cc_errors,
        subtitle_drift_ms,
    };
    println!("{}", serde_json::to_string(&report)?);

    let gh_input = crate::engine::github_summary::GithubSummaryInput {
        url: &report.url,
        health: &health,
        abr: &abr_health,
        budget: Some(&report),
        rtf: report.last_rtf,
        part_rtf: last_part_rtf,
        tr101290: tr101290.as_ref(),
        doh_ms: last_doh_ms,
    };
    crate::engine::github_summary::maybe_write_github_summary(&gh_input, github_summary)?;

    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

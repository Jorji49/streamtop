//! Every `DiagnosticReasonCode` variant must map to a stable string label.

use streamtop::models::DiagnosticReasonCode;

const ALL_CODES: &[DiagnosticReasonCode] = &[
    DiagnosticReasonCode::ErrHttpRedirectLoop,
    DiagnosticReasonCode::ErrRtfStallRisk,
    DiagnosticReasonCode::ErrTr101290SyncLoss,
    DiagnosticReasonCode::ErrTr101290CcError,
    DiagnosticReasonCode::ErrTr101290PcrJitter,
    DiagnosticReasonCode::ErrTr101290Tei,
    DiagnosticReasonCode::ErrCdnCacheMiss,
    DiagnosticReasonCode::ErrAesKeyFetchFailed,
    DiagnosticReasonCode::ErrTcpIoReset,
    DiagnosticReasonCode::ErrPatPmtTimeout,
    DiagnosticReasonCode::ErrPartRtfStall,
    DiagnosticReasonCode::ErrAbrVariantMisalignment,
    DiagnosticReasonCode::ErrBudgetRtfExceeded,
    DiagnosticReasonCode::ErrBudgetTtfbExceeded,
    DiagnosticReasonCode::ErrBudgetCcExceeded,
    DiagnosticReasonCode::ErrBudgetDriftExceeded,
    DiagnosticReasonCode::ErrDohResolutionFailed,
    DiagnosticReasonCode::ErrCdnSyncSkew,
];

#[test]
fn reason_code_labels_are_err_prefixed() {
    for code in ALL_CODES {
        let label = code.as_str();
        assert!(
            label.starts_with("ERR_"),
            "expected ERR_ prefix for {label:?}"
        );
        assert!(
            label
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()),
            "expected SCREAMING_SNAKE for {label}"
        );
    }
}

#[test]
fn tr101290_rule_mapping_covers_p1_p2() {
    assert_eq!(
        DiagnosticReasonCode::from_tr101290_rule("P1_SYNC"),
        Some(DiagnosticReasonCode::ErrTr101290SyncLoss)
    );
    assert_eq!(
        DiagnosticReasonCode::from_tr101290_rule("P2_TEI"),
        Some(DiagnosticReasonCode::ErrTr101290Tei)
    );
    assert_eq!(
        DiagnosticReasonCode::from_tr101290_rule("P1_PAT_PMT_TIMEOUT"),
        Some(DiagnosticReasonCode::ErrPatPmtTimeout)
    );
}

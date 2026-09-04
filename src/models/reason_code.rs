use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiagnosticReasonCode {
    ErrHttpRedirectLoop,
    ErrRtfStallRisk,
    ErrTr101290SyncLoss,
    ErrTr101290CcError,
    ErrTr101290PcrJitter,
    ErrTr101290Tei,
    ErrCdnCacheMiss,
    ErrAesKeyFetchFailed,
    ErrTcpIoReset,
    ErrPatPmtTimeout,
    ErrPartRtfStall,
    ErrAbrVariantMisalignment,
    ErrBudgetRtfExceeded,
    ErrBudgetTtfbExceeded,
    ErrBudgetCcExceeded,
    ErrBudgetDriftExceeded,
    ErrDohResolutionFailed,
    ErrCdnSyncSkew,
}

impl DiagnosticReasonCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ErrHttpRedirectLoop => "ERR_HTTP_REDIRECT_LOOP",
            Self::ErrRtfStallRisk => "ERR_RTF_STALL_RISK",
            Self::ErrTr101290SyncLoss => "ERR_TR101290_SYNC_LOSS",
            Self::ErrTr101290CcError => "ERR_TR101290_CC_ERROR",
            Self::ErrTr101290PcrJitter => "ERR_TR101290_PCR_JITTER",
            Self::ErrTr101290Tei => "ERR_TR101290_TEI",
            Self::ErrCdnCacheMiss => "ERR_CDN_CACHE_MISS",
            Self::ErrAesKeyFetchFailed => "ERR_AES_KEY_FETCH_FAILED",
            Self::ErrTcpIoReset => "ERR_TCP_IO_RESET",
            Self::ErrPatPmtTimeout => "ERR_TR101290_PAT_PMT_TIMEOUT",
            Self::ErrPartRtfStall => "ERR_PART_RTF_STALL",
            Self::ErrAbrVariantMisalignment => "ERR_ABR_VARIANT_MISALIGNMENT",
            Self::ErrBudgetRtfExceeded => "ERR_BUDGET_RTF_EXCEEDED",
            Self::ErrBudgetTtfbExceeded => "ERR_BUDGET_TTFB_EXCEEDED",
            Self::ErrBudgetCcExceeded => "ERR_BUDGET_CC_EXCEEDED",
            Self::ErrBudgetDriftExceeded => "ERR_BUDGET_DRIFT_EXCEEDED",
            Self::ErrDohResolutionFailed => "ERR_DOH_RESOLUTION_FAILED",
            Self::ErrCdnSyncSkew => "ERR_CDN_SYNC_SKEW",
        }
    }

    pub fn from_tr101290_rule(rule: &str) -> Option<Self> {
        match rule {
            "P1_SYNC" => Some(Self::ErrTr101290SyncLoss),
            "P1_CC" => Some(Self::ErrTr101290CcError),
            "P1_PAT" | "P1_PMT" | "P1_PAT_PMT_TIMEOUT" => Some(Self::ErrPatPmtTimeout),
            "P2_PCR_JITTER" | "P2_PCR_GAP" => Some(Self::ErrTr101290PcrJitter),
            "P2_TEI" => Some(Self::ErrTr101290Tei),
            _ => None,
        }
    }
}

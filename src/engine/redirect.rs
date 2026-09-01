//! Redirect policy with cycle detection and hop limits.

use reqwest::redirect::{Attempt, Policy};

use crate::models::DiagnosticReasonCode;

/// Maximum redirect hops before `ERR_HTTP_REDIRECT_LOOP`.
pub const MAX_REDIRECT_HOPS: usize = 10;

/// Build redirect policy: preserve default headers (auth tokens) and detect loops.
pub fn redirect_policy() -> Policy {
    Policy::custom(|attempt: Attempt| {
        let hops = attempt.previous().len();
        if hops >= MAX_REDIRECT_HOPS {
            return attempt.error(RedirectLoopError::LimitExceeded);
        }
        let current = attempt.url().as_str();
        for prev in attempt.previous() {
            if prev.as_str() == current {
                return attempt.error(RedirectLoopError::CycleDetected);
            }
        }
        attempt.follow()
    })
}

#[derive(Debug, Clone, Copy)]
pub enum RedirectLoopError {
    LimitExceeded,
    CycleDetected,
}

impl RedirectLoopError {
    pub fn reason_code() -> DiagnosticReasonCode {
        DiagnosticReasonCode::ErrHttpRedirectLoop
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::LimitExceeded => "redirect hop limit exceeded",
            Self::CycleDetected => "redirect cycle detected",
        }
    }
}

impl std::fmt::Display for RedirectLoopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for RedirectLoopError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_reason_code() {
        assert_eq!(
            RedirectLoopError::reason_code().as_str(),
            "ERR_HTTP_REDIRECT_LOOP"
        );
    }
}

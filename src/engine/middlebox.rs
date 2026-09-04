//! Passive transport anomaly hints from timing (read-only heuristics).

use crate::models::DiagnosticReasonCode;

#[derive(Debug, Clone, Copy, Default)]
pub struct MiddleboxHints {
    pub tcp_reset_suspected: bool,
}

impl MiddleboxHints {
    pub fn reason_code(self) -> Option<&'static str> {
        if self.tcp_reset_suspected {
            Some(DiagnosticReasonCode::ErrTcpIoReset.as_str())
        } else {
            None
        }
    }
}

/// Heuristic: TCP connect succeeded but transfer ended before any bytes with I/O error class.
pub fn classify_transport_failure(
    connected: bool,
    bytes_received: u64,
    io_error: bool,
) -> MiddleboxHints {
    MiddleboxHints {
        tcp_reset_suspected: connected && bytes_received == 0 && io_error,
    }
}

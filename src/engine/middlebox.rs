//! Passive middlebox / DPI anomaly hints from transport timing (read-only heuristics).

use crate::models::DiagnosticReasonCode;

#[derive(Debug, Clone, Copy, Default)]
pub struct MiddleboxHints {
    pub tcp_reset_suspected: bool,
    pub dns_slow_vs_throughput: bool,
}

impl MiddleboxHints {
    pub fn reason_code(self) -> Option<&'static str> {
        if self.tcp_reset_suspected {
            Some(DiagnosticReasonCode::ErrDpiTcpReset.as_str())
        } else {
            None
        }
    }
}

/// Heuristic: TCP connect succeeded but transfer ended before any bytes with I/O error class.
pub fn classify_transport_failure(connected: bool, bytes_received: u64, io_error: bool) -> MiddleboxHints {
    MiddleboxHints {
        tcp_reset_suspected: connected && bytes_received == 0 && io_error,
        dns_slow_vs_throughput: false,
    }
}

/// Compare DNS lookup ms vs segment throughput kbps for shaping hints.
pub fn dns_shaping_hint(dns_ms: u64, throughput_kbps: Option<u64>) -> bool {
    if dns_ms < 500 {
        return false;
    }
    throughput_kbps.is_some_and(|k| k < 200)
}

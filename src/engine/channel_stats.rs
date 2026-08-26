//! Global counters for bounded channel backpressure visibility.

use std::sync::atomic::{AtomicU64, Ordering};

static CHANNEL_DROPPED: AtomicU64 = AtomicU64::new(0);

/// Record one dropped event from a full `try_send` queue.
pub fn record_channel_drop() {
    CHANNEL_DROPPED.fetch_add(1, Ordering::Relaxed);
}

/// Total poller/UI/webhook events dropped since process start.
pub fn channel_dropped_total() -> u64 {
    CHANNEL_DROPPED.load(Ordering::Relaxed)
}

#[cfg(test)]
pub fn reset_for_test() {
    CHANNEL_DROPPED.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn drop_counter_increments_on_saturation() {
        reset_for_test();
        let (tx, _rx) = mpsc::channel(1);
        assert!(tx.try_send(()).is_ok());
        assert!(tx.try_send(()).is_err());
        record_channel_drop();
        assert_eq!(channel_dropped_total(), 1);
    }
}

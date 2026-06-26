//! Lightweight, per-session latency and throughput instrumentation.
//!
//! The receive/inject loop in [`crate::connection`] is single-threaded async,
//! so a [`SessionMetrics`] value lives entirely inside that loop and needs no
//! locking. When metrics are disabled every record call is a cheap early
//! return, so there is no measurable overhead in production.
//!
//! ## What is measured
//!
//! - **inject latency** (`inject_*_us`): time from "event decoded off the wire"
//!   to "`InputEventSink::handle` returned". Measured with a local monotonic
//!   [`Instant`], so it is fully accurate. This is the part of end-to-end
//!   latency we directly control on the receiving host.
//! - **end-to-end latency** (`e2e_*_us`): `now_wall - event.timestamp_us`, i.e.
//!   capture (on the sender) to just-before-inject (on the receiver). This
//!   spans two machines, so the **absolute** value is only trustworthy when
//!   both clocks are NTP-synchronised; the **distribution/jitter** is useful
//!   even with a constant offset. Synthetic events (`timestamp_us == 0`) and
//!   samples that would be negative (clock skew) are skipped and counted in
//!   `e2e_skipped`.
//! - **throughput**: inbound events + bytes and outbound messages + bytes per
//!   window, reported as per-second rates.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::info;

/// Environment variable controlling the metrics flush interval, in seconds.
/// Unset uses [`DEFAULT_INTERVAL_SECS`]; `0` disables metrics entirely.
const INTERVAL_ENV: &str = "FLOWKEY_METRICS_INTERVAL_SECS";
const DEFAULT_INTERVAL_SECS: u64 = 10;

/// Safety cap on per-window samples to bound memory under pathological rates.
/// At the coalesced input rate (~250 events/s) a 10s window holds ~2.5k
/// samples, far below this; the cap only guards against runaway input.
const MAX_SAMPLES: usize = 100_000;

#[derive(Debug)]
pub struct SessionMetrics {
    interval: Duration,
    enabled: bool,
    window_start: Instant,

    inject_us: Vec<u32>,
    // Signed raw (now - capture) deltas in microseconds. Signed because the
    // sender's clock may run ahead of ours; we subtract the per-window minimum
    // at flush time to cancel the clock offset.
    e2e_raw_us: Vec<i64>,

    inbound_events: u64,
    inbound_bytes: u64,
    outbound_messages: u64,
    outbound_bytes: u64,
    e2e_skipped: u64,
}

impl SessionMetrics {
    /// Build metrics for a session, reading the flush interval from the
    /// environment. A `0` interval disables collection.
    pub fn from_env() -> Self {
        let interval_secs = std::env::var(INTERVAL_ENV)
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_INTERVAL_SECS);
        Self::new(interval_secs)
    }

    pub fn new(interval_secs: u64) -> Self {
        Self {
            // The select! ticker requires a non-zero period even when disabled;
            // the `enabled` flag is what actually gates flushing and recording.
            interval: Duration::from_secs(interval_secs.max(1)),
            enabled: interval_secs > 0,
            window_start: Instant::now(),
            inject_us: Vec::new(),
            e2e_raw_us: Vec::new(),
            inbound_events: 0,
            inbound_bytes: 0,
            outbound_messages: 0,
            outbound_bytes: 0,
            e2e_skipped: 0,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Ticker period for the flush arm of the session `select!`. Always
    /// non-zero so `tokio::time::interval` never panics.
    pub fn tick_interval(&self) -> Duration {
        self.interval
    }

    /// Record one decoded inbound message and its on-wire size in bytes.
    pub fn record_inbound(&mut self, wire_bytes: usize) {
        if !self.enabled {
            return;
        }
        self.inbound_events = self.inbound_events.saturating_add(1);
        self.inbound_bytes = self.inbound_bytes.saturating_add(wire_bytes as u64);
    }

    /// Record one written outbound message and its on-wire size in bytes.
    pub fn record_outbound(&mut self, wire_bytes: usize) {
        if !self.enabled {
            return;
        }
        self.outbound_messages = self.outbound_messages.saturating_add(1);
        self.outbound_bytes = self.outbound_bytes.saturating_add(wire_bytes as u64);
    }

    /// Record how long injecting a single event took on this host.
    pub fn record_inject(&mut self, elapsed: Duration) {
        if !self.enabled {
            return;
        }
        push_capped(&mut self.inject_us, duration_to_micros_u32(elapsed));
    }

    /// Record end-to-end latency from a captured event's wall-clock timestamp.
    /// Skips synthetic (`0`) and clock-skewed (negative) samples.
    pub fn record_e2e(&mut self, capture_timestamp_us: u64) {
        if !self.enabled {
            return;
        }
        if capture_timestamp_us == 0 {
            // Synthetic event (e.g. hotkey-injected) carries no capture time.
            self.e2e_skipped = self.e2e_skipped.saturating_add(1);
            return;
        }
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        // Keep the signed delta even when negative (sender clock ahead of ours).
        // The per-window minimum is subtracted at flush time, cancelling any
        // constant clock offset and leaving the real latency spread — so this
        // works without NTP-synced clocks. (Previously negatives were dropped,
        // which discarded almost every sample when the controller ran ahead.)
        if self.e2e_raw_us.len() < MAX_SAMPLES {
            self.e2e_raw_us
                .push(now_us - capture_timestamp_us as i64);
        }
    }

    /// If a full window has elapsed, emit one summary log line and reset the
    /// window. No-op when disabled or the window is not yet complete.
    pub fn maybe_flush(&mut self, peer_id: &str) {
        if !self.enabled {
            return;
        }
        let elapsed = self.window_start.elapsed();
        if elapsed < self.interval {
            return;
        }
        self.flush(peer_id, elapsed);
    }

    /// Force a final summary, e.g. when a session ends. No-op when disabled or
    /// nothing was recorded this window.
    pub fn flush_final(&mut self, peer_id: &str) {
        if !self.enabled {
            return;
        }
        if self.inbound_events == 0 && self.outbound_messages == 0 {
            return;
        }
        let elapsed = self.window_start.elapsed();
        self.flush(peer_id, elapsed);
    }

    fn flush(&mut self, peer_id: &str, elapsed: Duration) {
        let secs = elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
        let inject = Percentiles::from_samples(&mut self.inject_us);
        // e2e_* are offset-corrected: each window subtracts its own minimum
        // delta, so they measure latency *spread* above the best case and are
        // immune to a constant clock offset between hosts. e2e_offset_us is that
        // subtracted baseline (raw now-capture min); it conflates the true
        // best-case latency with the inter-host clock skew, so only its changes
        // are meaningful, not its absolute value.
        let (e2e, e2e_offset_us) = percentiles_offset_corrected(&mut self.e2e_raw_us);

        info!(
            peer = %peer_id,
            window_s = format_args!("{:.1}", secs),
            in_eps = format_args!("{:.0}", self.inbound_events as f64 / secs),
            in_kbps = format_args!("{:.1}", self.inbound_bytes as f64 / secs / 1024.0),
            out_mps = format_args!("{:.0}", self.outbound_messages as f64 / secs),
            out_kbps = format_args!("{:.1}", self.outbound_bytes as f64 / secs / 1024.0),
            inject_p50_us = inject.p50,
            inject_p99_us = inject.p99,
            inject_max_us = inject.max,
            inject_n = inject.count,
            e2e_p50_us = e2e.p50,
            e2e_p99_us = e2e.p99,
            e2e_max_us = e2e.max,
            e2e_n = e2e.count,
            e2e_offset_us = e2e_offset_us,
            e2e_skipped = self.e2e_skipped,
            "session metrics (inject_* are local/exact; e2e_* are offset-corrected jitter, clock-skew safe)"
        );

        self.reset();
    }

    fn reset(&mut self) {
        self.window_start = Instant::now();
        self.inject_us.clear();
        self.e2e_raw_us.clear();
        self.inbound_events = 0;
        self.inbound_bytes = 0;
        self.outbound_messages = 0;
        self.outbound_bytes = 0;
        self.e2e_skipped = 0;
    }
}

#[derive(Debug, Default)]
struct Percentiles {
    p50: u32,
    p99: u32,
    max: u32,
    count: usize,
}

impl Percentiles {
    /// Computes p50/p99/max by sorting in place. Cheap at window scale and
    /// avoids any external dependency.
    fn from_samples(samples: &mut [u32]) -> Self {
        let count = samples.len();
        if count == 0 {
            return Self::default();
        }
        samples.sort_unstable();
        Self {
            p50: percentile(samples, 50),
            p99: percentile(samples, 99),
            max: samples[count - 1],
            count,
        }
    }
}

/// Computes offset-corrected percentiles from signed raw (now - capture)
/// deltas: subtract the minimum so the result measures latency spread above the
/// best observed case, independent of any constant clock offset between hosts.
/// Returns the percentiles and the subtracted baseline (the raw minimum).
fn percentiles_offset_corrected(samples: &mut [i64]) -> (Percentiles, i64) {
    let Some(&offset) = samples.iter().min() else {
        return (Percentiles::default(), 0);
    };
    let mut corrected: Vec<u32> = samples
        .iter()
        .map(|&d| (d - offset).clamp(0, u32::MAX as i64) as u32)
        .collect();
    (Percentiles::from_samples(&mut corrected), offset)
}

/// Nearest-rank percentile over a pre-sorted slice. `p` is in `0..=100`.
fn percentile(sorted: &[u32], p: u8) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p as usize * sorted.len()).div_ceil(100); // ceil(p/100 * n)
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn push_capped(buf: &mut Vec<u32>, value: u32) {
    if buf.len() < MAX_SAMPLES {
        buf.push(value);
    }
}

fn duration_to_micros_u32(d: Duration) -> u32 {
    d.as_micros().min(u32::MAX as u128) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_nearest_rank() {
        let mut s: Vec<u32> = (1..=100).collect();
        s.sort_unstable();
        assert_eq!(percentile(&s, 50), 50);
        assert_eq!(percentile(&s, 99), 99);
        assert_eq!(percentile(&s, 100), 100);
    }

    #[test]
    fn percentiles_from_samples_handles_empty_and_single() {
        let empty = Percentiles::from_samples(&mut []);
        assert_eq!((empty.p50, empty.p99, empty.max, empty.count), (0, 0, 0, 0));

        let single = Percentiles::from_samples(&mut [42]);
        assert_eq!((single.p50, single.p99, single.max, single.count), (42, 42, 42, 1));
    }

    #[test]
    fn disabled_metrics_record_nothing() {
        let mut m = SessionMetrics::new(0);
        assert!(!m.is_enabled());
        m.record_inbound(100);
        m.record_inject(Duration::from_micros(500));
        m.record_e2e(1);
        assert!(m.inject_us.is_empty());
        assert_eq!(m.inbound_events, 0);
    }

    #[test]
    fn e2e_skips_only_synthetic_samples() {
        let mut m = SessionMetrics::new(10);
        m.record_e2e(0); // synthetic => skipped
        m.record_e2e(1); // real capture (clock-skewed) => kept, not skipped
        assert_eq!(m.e2e_skipped, 1);
        assert_eq!(m.e2e_raw_us.len(), 1);
    }

    #[test]
    fn e2e_offset_correction_cancels_constant_clock_skew() {
        // Three samples with a large constant offset (e.g. sender clock ahead):
        // the corrected percentiles should reflect only the spread (0, 5, 10).
        let mut raw = vec![-1_000_000_i64, -1_000_000 + 5_000, -1_000_000 + 10_000];
        let (p, offset) = percentiles_offset_corrected(&mut raw);
        assert_eq!(offset, -1_000_000);
        assert_eq!(p.max, 10_000);
        assert_eq!(p.count, 3);
    }

    #[test]
    fn samples_are_capped() {
        let mut buf = Vec::new();
        for _ in 0..(MAX_SAMPLES + 10) {
            push_capped(&mut buf, 1);
        }
        assert_eq!(buf.len(), MAX_SAMPLES);
    }
}

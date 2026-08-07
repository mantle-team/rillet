//! Command-channel metrics every service collects for itself.
//!
//! Each service counts, per command, how many were enqueued and how many
//! were processed; the difference is the queue's exact depth at any moment.
//! The service loop also records depth samples into a ring buffer as it
//! processes commands, and the aggregate statistics compute their
//! time-windowed means and maxima from those samples. An idle service
//! records no samples and has depth zero.
//!
//! Handles expose all of it through `command_stats()` and
//! `aggregate_stats()`; nothing is declared or wired.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Default sample interval in milliseconds.
pub const DEFAULT_SAMPLE_INTERVAL_MS: u64 = 100;

/// Default number of commands between samples.
pub const DEFAULT_SAMPLE_COMMAND_INTERVAL: u64 = 100;

/// Default sample buffer capacity, sized to span the one-minute
/// aggregation window at the default sample interval.
pub const DEFAULT_SAMPLE_BUFFER_SIZE: usize = 600;

/// A service's command counters and depth-sample history.
///
/// `N` is the number of command variants.
pub struct CommandMetrics<const N: usize> {
    /// Per-command enqueued counters.
    enqueued: [AtomicU64; N],
    /// Per-command processed counters.
    processed: [AtomicU64; N],
    /// Aggregate sample history.
    samples: Mutex<SampleRing>,
}

impl<const N: usize> CommandMetrics<N> {
    /// Creates new metrics with zeroed counters and empty sample buffer.
    pub fn new() -> Self {
        Self {
            enqueued: std::array::from_fn(|_| AtomicU64::new(0)),
            processed: std::array::from_fn(|_| AtomicU64::new(0)),
            samples: Mutex::new(SampleRing::new()),
        }
    }

    /// Increments the enqueued counter for a command.
    #[inline]
    pub fn inc_enqueued(&self, cmd_index: usize) {
        self.enqueued[cmd_index].fetch_add(1, Ordering::Relaxed);
    }

    /// Increments the processed counter for a command.
    #[inline]
    pub fn inc_processed(&self, cmd_index: usize) {
        self.processed[cmd_index].fetch_add(1, Ordering::Relaxed);
    }

    /// Returns current depth for a specific command.
    pub fn command_depth(&self, cmd_index: usize) -> u64 {
        let enqueued = self.enqueued[cmd_index].load(Ordering::Relaxed);
        let processed = self.processed[cmd_index].load(Ordering::Relaxed);
        enqueued.saturating_sub(processed)
    }

    /// Returns total enqueued count for a specific command.
    pub fn command_enqueued(&self, cmd_index: usize) -> u64 {
        self.enqueued[cmd_index].load(Ordering::Relaxed)
    }

    /// Returns total processed count for a specific command.
    pub fn command_processed(&self, cmd_index: usize) -> u64 {
        self.processed[cmd_index].load(Ordering::Relaxed)
    }

    /// Returns aggregate depth across all commands.
    pub fn total_depth(&self) -> u64 {
        let total_enqueued: u64 = self
            .enqueued
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum();
        let total_processed: u64 = self
            .processed
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum();
        total_enqueued.saturating_sub(total_processed)
    }

    /// Returns aggregate enqueued count.
    pub fn total_enqueued(&self) -> u64 {
        self.enqueued
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum()
    }

    /// Returns aggregate processed count.
    pub fn total_processed(&self) -> u64 {
        self.processed
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum()
    }

    /// Records a sample of the current aggregate depth.
    pub fn record_sample(&self) {
        let depth = self.total_depth();
        self.samples.lock().unwrap().push(depth);
    }

    /// Computes aggregate statistics from samples.
    pub fn aggregate_stats(&self) -> AggregateStats {
        let samples = self.samples.lock().unwrap();
        let now = Instant::now();

        let depth = self.total_depth();
        let total_enqueued = self.total_enqueued();
        let total_processed = self.total_processed();

        let (mean_1s, max_1s, count_1s) =
            samples.stats_since(now.checked_sub(Duration::from_secs(1)));
        let (mean_10s, max_10s, _) = samples.stats_since(now.checked_sub(Duration::from_secs(10)));
        let (mean_1m, max_1m, _) = samples.stats_since(now.checked_sub(Duration::from_secs(60)));

        let throughput_1s = count_1s as f64;

        AggregateStats {
            depth,
            total_enqueued,
            total_processed,
            mean_depth_1s: mean_1s,
            mean_depth_10s: mean_10s,
            mean_depth_1m: mean_1m,
            max_depth_1s: max_1s,
            max_depth_10s: max_10s,
            max_depth_1m: max_1m,
            throughput_1s,
        }
    }
}

impl<const N: usize> Default for CommandMetrics<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for a single command type.
#[derive(Debug, Clone)]
pub struct CommandStats {
    /// Command name (e.g., "send_message").
    pub name: &'static str,
    /// Current queue depth for this command.
    pub depth: u64,
    /// Total commands enqueued.
    pub total_enqueued: u64,
    /// Total commands processed.
    pub total_processed: u64,
}

/// Aggregate statistics computed from samples.
#[derive(Debug, Clone)]
pub struct AggregateStats {
    /// Current aggregate depth (exact).
    pub depth: u64,
    /// Total commands enqueued across all types.
    pub total_enqueued: u64,
    /// Total commands processed across all types.
    pub total_processed: u64,

    /// Mean depth over last 1 second.
    pub mean_depth_1s: f64,
    /// Mean depth over last 10 seconds.
    pub mean_depth_10s: f64,
    /// Mean depth over last 1 minute.
    pub mean_depth_1m: f64,

    /// Max depth over last 1 second.
    pub max_depth_1s: u64,
    /// Max depth over last 10 seconds.
    pub max_depth_10s: u64,
    /// Max depth over last 1 minute.
    pub max_depth_1m: u64,

    /// Number of samples recorded in the last second, not commands
    /// processed.
    pub throughput_1s: f64,
}

/// Fixed-size ring buffer for depth samples.
pub struct SampleRing {
    buffer: Box<[(Instant, u64)]>,
    head: usize,
    len: usize,
}

impl SampleRing {
    /// Creates a new sample ring with default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_SAMPLE_BUFFER_SIZE)
    }

    /// Creates a new sample ring with specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: vec![(Instant::now(), 0); capacity].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    /// Pushes a new sample, overwriting oldest if full.
    pub fn push(&mut self, depth: u64) {
        let now = Instant::now();
        self.buffer[self.head] = (now, depth);
        self.head = (self.head + 1) % self.buffer.len();
        if self.len < self.buffer.len() {
            self.len += 1;
        }
    }

    /// Returns (mean, max, count) for samples since the given instant, or
    /// over every sample when the window has no lower bound.
    pub fn stats_since(&self, since: Option<Instant>) -> (f64, u64, usize) {
        let mut sum = 0u64;
        let mut max = 0u64;
        let mut count = 0usize;

        for i in 0..self.len {
            let idx = (self.head + self.buffer.len() - 1 - i) % self.buffer.len();
            let (ts, depth) = self.buffer[idx];
            if since.is_some_and(|since| ts < since) {
                break;
            }
            sum += depth;
            max = max.max(depth);
            count += 1;
        }

        let mean = if count > 0 {
            sum as f64 / count as f64
        } else {
            0.0
        };

        (mean, max, count)
    }

    /// Returns an iterator over samples from newest to oldest.
    pub fn iter(&self) -> impl Iterator<Item = (Instant, u64)> + '_ {
        (0..self.len).map(move |i| {
            let idx = (self.head + self.buffer.len() - 1 - i) % self.buffer.len();
            self.buffer[idx]
        })
    }
}

impl Default for SampleRing {
    fn default() -> Self {
        Self::new()
    }
}

/// A service loop's decision state for when to take the next sample.
pub struct SamplingState {
    /// Last time a sample was recorded.
    pub last_sample_time: Instant,
    /// Commands processed since last sample.
    pub commands_since_sample: u64,
    /// Sample after this many commands.
    pub command_interval: u64,
    /// Sample after this duration.
    pub time_interval: Duration,
}

impl SamplingState {
    /// Creates new sampling state with default intervals.
    pub fn new() -> Self {
        Self {
            last_sample_time: Instant::now(),
            commands_since_sample: 0,
            command_interval: DEFAULT_SAMPLE_COMMAND_INTERVAL,
            time_interval: Duration::from_millis(DEFAULT_SAMPLE_INTERVAL_MS),
        }
    }

    /// Returns true once enough commands or enough time has passed since
    /// the last sample.
    #[inline]
    pub fn should_sample(&self) -> bool {
        self.commands_since_sample >= self.command_interval
            || self.last_sample_time.elapsed() >= self.time_interval
    }

    /// Resets state after taking a sample.
    #[inline]
    pub fn reset(&mut self) {
        self.commands_since_sample = 0;
        self.last_sample_time = Instant::now();
    }

    /// Increments command count.
    #[inline]
    pub fn inc_command(&mut self) {
        self.commands_since_sample += 1;
    }
}

impl Default for SamplingState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_command_metrics_basic() {
        let metrics: CommandMetrics<3> = CommandMetrics::new();

        // Enqueue some commands
        metrics.inc_enqueued(0);
        metrics.inc_enqueued(0);
        metrics.inc_enqueued(1);

        assert_eq!(metrics.command_depth(0), 2);
        assert_eq!(metrics.command_depth(1), 1);
        assert_eq!(metrics.command_depth(2), 0);
        assert_eq!(metrics.total_depth(), 3);

        // Process some
        metrics.inc_processed(0);

        assert_eq!(metrics.command_depth(0), 1);
        assert_eq!(metrics.total_depth(), 2);
    }

    #[test]
    fn test_sample_ring() {
        let mut ring = SampleRing::with_capacity(5);

        ring.push(10);
        thread::sleep(Duration::from_millis(10));
        ring.push(20);
        thread::sleep(Duration::from_millis(10));
        ring.push(30);

        let (mean, max, count) =
            ring.stats_since(Instant::now().checked_sub(Duration::from_secs(1)));
        assert_eq!(count, 3);
        assert_eq!(max, 30);
        assert!((mean - 20.0).abs() < 0.001);
    }

    #[test]
    fn unbounded_window_covers_every_sample() {
        let mut ring = SampleRing::with_capacity(5);

        ring.push(10);
        ring.push(20);

        let (mean, max, count) = ring.stats_since(None);
        assert_eq!(count, 2);
        assert_eq!(max, 20);
        assert!((mean - 15.0).abs() < 0.001);
    }

    #[test]
    fn test_sample_ring_overflow() {
        let mut ring = SampleRing::with_capacity(3);

        ring.push(1);
        ring.push(2);
        ring.push(3);
        ring.push(4); // overwrites 1

        let samples: Vec<_> = ring.iter().map(|(_, d)| d).collect();
        assert_eq!(samples, vec![4, 3, 2]);
    }

    #[test]
    fn test_sampling_state() {
        let mut state = SamplingState::new();
        state.command_interval = 10;
        state.time_interval = Duration::from_secs(1);

        // Not enough commands yet
        for _ in 0..9 {
            state.inc_command();
            assert!(!state.should_sample());
        }

        // 10th command triggers
        state.inc_command();
        assert!(state.should_sample());

        state.reset();
        assert!(!state.should_sample());
    }
}

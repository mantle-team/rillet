//! Metrics: the observability every service carries for free.
//!
//! Each handle exposes per-command counters and sampled queue-depth
//! statistics; nothing needs to be declared or wired. The command queue's
//! size is set here explicitly: a full queue panics rather than dropping,
//! so it is sized for the worst legitimate burst.
//!
//! Run with: cargo run --example 07_metrics

use std::time::Duration;

/// A worker taking two kinds of jobs.
#[rillet::service(command_capacity = 64)]
struct Worker {
    #[rillet(get, default)]
    done: u32,
}

#[rillet::handlers]
impl Worker {
    #[rillet(command)]
    fn quick_job(&mut self) {
        self.done += 1;
    }

    #[rillet(command)]
    fn slow_job(&mut self) {
        std::thread::sleep(Duration::from_millis(2));
        self.done += 1;
    }
}

fn main() {
    let worker = Worker::new().spawn();

    for _ in 0..40 {
        worker.quick_job();
    }
    for _ in 0..10 {
        worker.slow_job();
    }

    // Poll until every job has finished.
    while worker.done() < 50 {
        std::thread::sleep(Duration::from_millis(5));
    }

    println!("per-command:");
    for stat in worker.command_stats() {
        println!(
            "  {:>9}: enqueued {:>3}, processed {:>3}, depth {}",
            stat.name, stat.total_enqueued, stat.total_processed, stat.depth
        );
    }

    let agg = worker.aggregate_stats();
    println!(
        "aggregate: {} processed, depth {}, max depth (10s window) {}",
        agg.total_processed, agg.depth, agg.max_depth_10s
    );

    worker.cancel();
}

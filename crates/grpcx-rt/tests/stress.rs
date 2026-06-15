//! Multi-thread runtime stress harness (v0.6).
//!
//! A fan-in storm: `P` producer tasks, spread across workers, push `per`
//! values each into one collector, interleaving nested spawns,
//! `yield_now`, and short `sleep`s — exercising cross-worker channels,
//! work-stealing, the timer wheel, and the cooperative budget all at
//! once. Each round asserts **message conservation** (exact count) and a
//! deterministic **sum** (so a lost, duplicated, or corrupted message
//! fails), and the `Runtime` is reused across rounds to shake out
//! steady-state scheduler bugs.
//!
//! - `stress_fan_in_short` runs a few rounds in seconds — part of the
//!   per-platform gate.
//! - `stress_soak` is `#[ignore]`d and time-boxed via
//!   `GRPCX_STRESS_SECS` (+ `_WORKERS` / `_PRODUCERS` / `_PER`) for the
//!   v0.6 24h soak (no deadlock / livelock / starvation). A hang shows
//!   as the test never returning; correctness shows as the per-round
//!   asserts.

use std::time::{Duration, Instant};

use grpcx_rt::{Runtime, channel, sleep, yield_now};

/// Sum of all values a fan-in round pushes: `val = p * per + i` over
/// `p in 0..producers`, `i in 0..per`. Wrapping, matching the producers.
fn expected_sum(producers: u64, per: u64) -> u64 {
    let mut sum = 0u64;
    for p in 0..producers {
        for i in 0..per {
            sum = sum.wrapping_add(p.wrapping_mul(per).wrapping_add(i));
        }
    }
    sum
}

/// Run one fan-in round on `rt`; returns the collected sum. Asserts the
/// message count is exactly `producers * per`.
fn fan_in_round(rt: &Runtime, producers: u64, per: u64) -> u64 {
    let h = rt.handle();
    rt.block_on(async move {
        let (tx, rx) = channel::<u64>(256);
        for p in 0..producers {
            let tx = tx.clone();
            let h_nested = h.clone();
            h.spawn(async move {
                // Keep nested-spawn results live so the work isn't
                // optimized away; not sent into the channel.
                let mut nested = 0u64;
                for i in 0..per {
                    if i % 256 == 0 {
                        let j = h_nested.spawn(async move { i.wrapping_mul(2) });
                        nested = nested.wrapping_add(j.await);
                    }
                    if i % 128 == 0 {
                        yield_now().await;
                    }
                    if i % 512 == 0 {
                        sleep(Duration::from_millis(1)).await;
                    }
                    let val = p.wrapping_mul(per).wrapping_add(i);
                    while tx.try_send(val).is_err() {
                        yield_now().await;
                    }
                }
                std::hint::black_box(nested);
            });
        }
        drop(tx); // last original sender gone ⇒ recv → None once producers finish

        let mut total = 0u64;
        let mut count = 0u64;
        while let Some(v) = rx.recv().await {
            total = total.wrapping_add(v);
            count += 1;
        }
        assert_eq!(count, producers * per, "message conservation");
        total
    })
}

#[test]
fn stress_fan_in_short() {
    let rt = Runtime::new(4).expect("runtime");
    const ROUNDS: u64 = 3;
    const PRODUCERS: u64 = 32;
    const PER: u64 = 1_000;
    let expected = expected_sum(PRODUCERS, PER);
    for round in 0..ROUNDS {
        let total = fan_in_round(&rt, PRODUCERS, PER);
        assert_eq!(total, expected, "sum mismatch on round {round}");
    }
}

#[test]
#[ignore = "long-running soak; time-boxed via GRPCX_STRESS_SECS"]
fn stress_soak() {
    fn env_u64(key: &str, default: u64) -> u64 {
        std::env::var(key)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    }

    let secs = env_u64("GRPCX_STRESS_SECS", 20);
    let workers = env_u64("GRPCX_STRESS_WORKERS", 4) as usize;
    let producers = env_u64("GRPCX_STRESS_PRODUCERS", 48);
    let per = env_u64("GRPCX_STRESS_PER", 2_000);

    let rt = Runtime::new(workers).expect("runtime");
    let expected = expected_sum(producers, per);
    let deadline = Instant::now() + Duration::from_secs(secs);

    let mut rounds = 0u64;
    let mut msgs = 0u64;
    while Instant::now() < deadline {
        let total = fan_in_round(&rt, producers, per);
        assert_eq!(total, expected, "sum mismatch on round {rounds}");
        rounds += 1;
        msgs = msgs.wrapping_add(producers * per);
    }

    let rate = msgs / secs.max(1);
    eprintln!(
        "[soak] {workers}w {producers}p×{per} — {rounds} rounds, {msgs} msgs in ~{secs}s ({rate} msg/s)"
    );
    assert!(rounds > 0, "soak completed zero rounds");
}

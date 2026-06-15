//! Head-to-head bench: `grpcx-rt` vs `tokio` on identical workloads.
//!
//! Two top-level groups — `current_thread` (single-thread `block_on`)
//! and `multi_thread` (work-stealing `Runtime::new(N)`). Each bench
//! lines up the most natural API on each side; the goal is
//! aggregate-non-loss, not micro-optimised cherry-picks.
//!
//! Fairness invariants:
//!
//! - Same iteration counts on both sides (`N` / `P` / `K` constants
//!   are shared).
//! - Runtime construction is hoisted **out** of the `iter` loop in
//!   the `multi_thread` benches (a new pool per iter would measure
//!   thread spawn, not scheduling). The `current_thread` benches
//!   construct in-iter on both sides because that's the realistic
//!   pattern for `block_on`.
//! - Both sides allocate their channels / spawn their tasks fresh
//!   per iter so the steady-state is the runtime, not the workload
//!   data structure.
//! - No `criterion::black_box` on tokio side except where also
//!   applied on the grpcx-rt side.

use core::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};

// ====================================================================
// current_thread — single-thread `block_on`.
// ====================================================================

fn bench_block_on_ready(c: &mut Criterion) {
    let mut group = c.benchmark_group("current_thread/block_on_ready");

    group.bench_function("grpcx_rt", |b| {
        b.iter(|| grpcx_rt::block_on(async { 1u64 }));
    });

    group.bench_function("tokio", |b| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio current_thread");
        b.iter(|| rt.block_on(async { 1u64 }));
    });

    group.finish();
}

fn bench_spawn_and_await(c: &mut Criterion) {
    let mut group = c.benchmark_group("current_thread/spawn_and_await");

    group.bench_function("grpcx_rt", |b| {
        b.iter(|| {
            grpcx_rt::block_on(async {
                let h = grpcx_rt::spawn(async { 42u64 });
                h.await
            })
        });
    });

    group.bench_function("tokio", |b| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio current_thread");
        b.iter(|| {
            rt.block_on(async {
                let h = tokio::task::spawn(async { 42u64 });
                h.await.expect("join")
            })
        });
    });

    group.finish();
}

fn bench_spawn_many_join_all(c: &mut Criterion) {
    const N: u64 = 100;
    let mut group = c.benchmark_group("current_thread/spawn_many_join_all_100");

    group.bench_function("grpcx_rt", |b| {
        b.iter(|| {
            grpcx_rt::block_on(async {
                let mut handles = Vec::with_capacity(N as usize);
                for i in 0..N {
                    handles.push(grpcx_rt::spawn(async move { i }));
                }
                let mut sum = 0u64;
                for h in handles {
                    sum = sum.wrapping_add(h.await);
                }
                sum
            })
        });
    });

    group.bench_function("tokio", |b| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio current_thread");
        b.iter(|| {
            rt.block_on(async {
                let mut handles = Vec::with_capacity(N as usize);
                for i in 0..N {
                    handles.push(tokio::task::spawn(async move { i }));
                }
                let mut sum = 0u64;
                for h in handles {
                    sum = sum.wrapping_add(h.await.expect("join"));
                }
                sum
            })
        });
    });

    group.finish();
}

fn bench_mpsc_ping_pong_ct(c: &mut Criterion) {
    const N: u64 = 1_000;
    let mut group = c.benchmark_group("current_thread/mpsc_ping_pong_1k");

    group.bench_function("grpcx_rt", |b| {
        b.iter(|| {
            grpcx_rt::block_on(async {
                let (tx, rx) = grpcx_rt::channel::<u64>(64);
                grpcx_rt::spawn(async move {
                    for i in 0..N {
                        while tx.try_send(i).is_err() {
                            grpcx_rt::yield_now().await;
                        }
                    }
                });
                let mut sum = 0u64;
                for _ in 0..N {
                    sum = sum.wrapping_add(rx.recv().await.expect("v"));
                }
                sum
            })
        });
    });

    group.bench_function("tokio", |b| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio current_thread");
        b.iter(|| {
            rt.block_on(async {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<u64>(64);
                tokio::task::spawn(async move {
                    for i in 0..N {
                        tx.send(i).await.expect("send");
                    }
                });
                let mut sum = 0u64;
                for _ in 0..N {
                    sum = sum.wrapping_add(rx.recv().await.expect("v"));
                }
                sum
            })
        });
    });

    group.finish();
}

// ====================================================================
// multi_thread — work-stealing `Runtime::new(N)`.
// ====================================================================

const WORKERS: usize = 4;

fn bench_spawn_storm(c: &mut Criterion) {
    const N: u64 = 1_000;
    let mut group = c.benchmark_group("multi_thread/spawn_storm_1k");

    {
        let rt = grpcx_rt::Runtime::new(WORKERS).expect("grpcx Runtime");
        group.bench_function("grpcx_rt", |b| {
            b.iter(|| {
                let h = rt.handle();
                rt.block_on(async move {
                    let mut handles = Vec::with_capacity(N as usize);
                    for i in 0..N {
                        handles.push(h.spawn(async move { i }));
                    }
                    let mut sum = 0u64;
                    for h in handles {
                        sum = sum.wrapping_add(h.await);
                    }
                    sum
                })
            });
        });
    }

    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .build()
            .expect("tokio multi_thread");
        group.bench_function("tokio", |b| {
            b.iter(|| {
                rt.block_on(async {
                    let mut handles = Vec::with_capacity(N as usize);
                    for i in 0..N {
                        handles.push(tokio::task::spawn(async move { i }));
                    }
                    let mut sum = 0u64;
                    for h in handles {
                        sum = sum.wrapping_add(h.await.expect("join"));
                    }
                    sum
                })
            });
        });
    }

    group.finish();
}

fn bench_work_steal_fan_out(c: &mut Criterion) {
    const N: u64 = 1_000;
    let mut group = c.benchmark_group("multi_thread/work_steal_fan_out_1k");

    {
        let rt = grpcx_rt::Runtime::new(WORKERS).expect("grpcx Runtime");
        group.bench_function("grpcx_rt", |b| {
            b.iter(|| {
                let h = rt.handle();
                rt.block_on(async move {
                    let counter = Arc::new(AtomicU64::new(0));
                    let mut handles = Vec::with_capacity(N as usize);
                    for i in 0..N {
                        let counter = Arc::clone(&counter);
                        handles.push(h.spawn(async move {
                            counter.fetch_add(i, Ordering::Relaxed);
                        }));
                    }
                    for h in handles {
                        h.await;
                    }
                    counter.load(Ordering::Relaxed)
                })
            });
        });
    }

    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .build()
            .expect("tokio multi_thread");
        group.bench_function("tokio", |b| {
            b.iter(|| {
                rt.block_on(async {
                    let counter = Arc::new(AtomicU64::new(0));
                    let mut handles = Vec::with_capacity(N as usize);
                    for i in 0..N {
                        let counter = Arc::clone(&counter);
                        handles.push(tokio::task::spawn(async move {
                            counter.fetch_add(i, Ordering::Relaxed);
                        }));
                    }
                    for h in handles {
                        h.await.expect("join");
                    }
                    counter.load(Ordering::Relaxed)
                })
            });
        });
    }

    group.finish();
}

fn bench_channel_fan_in(c: &mut Criterion) {
    const P: u64 = 8;
    const K: u64 = 200;
    let mut group = c.benchmark_group("multi_thread/channel_fan_in_8x200");

    {
        let rt = grpcx_rt::Runtime::new(WORKERS).expect("grpcx Runtime");
        group.bench_function("grpcx_rt", |b| {
            b.iter(|| {
                let h = rt.handle();
                rt.block_on(async move {
                    let (tx, rx) = grpcx_rt::channel::<u64>(64);
                    for p in 0..P {
                        let tx = tx.clone();
                        h.spawn(async move {
                            for i in 0..K {
                                let val = p.wrapping_mul(K).wrapping_add(i);
                                while tx.try_send(val).is_err() {
                                    grpcx_rt::yield_now().await;
                                }
                            }
                        });
                    }
                    drop(tx);
                    let mut sum = 0u64;
                    while let Some(v) = rx.recv().await {
                        sum = sum.wrapping_add(v);
                    }
                    sum
                })
            });
        });
    }

    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .build()
            .expect("tokio multi_thread");
        group.bench_function("tokio", |b| {
            b.iter(|| {
                rt.block_on(async {
                    let (tx, mut rx) = tokio::sync::mpsc::channel::<u64>(64);
                    for p in 0..P {
                        let tx = tx.clone();
                        tokio::task::spawn(async move {
                            for i in 0..K {
                                let val = p.wrapping_mul(K).wrapping_add(i);
                                tx.send(val).await.expect("send");
                            }
                        });
                    }
                    drop(tx);
                    let mut sum = 0u64;
                    while let Some(v) = rx.recv().await {
                        sum = sum.wrapping_add(v);
                    }
                    sum
                })
            });
        });
    }

    group.finish();
}

fn bench_sleep_concurrent(c: &mut Criterion) {
    const N: u64 = 32;
    let mut group = c.benchmark_group("multi_thread/sleep_concurrent_32x1ms");

    {
        let rt = grpcx_rt::Runtime::new(WORKERS).expect("grpcx Runtime");
        group.bench_function("grpcx_rt", |b| {
            b.iter(|| {
                let h = rt.handle();
                rt.block_on(async move {
                    let mut handles = Vec::with_capacity(N as usize);
                    for _ in 0..N {
                        handles.push(
                            h.spawn(async { grpcx_rt::sleep(Duration::from_millis(1)).await }),
                        );
                    }
                    for h in handles {
                        h.await;
                    }
                })
            });
        });
    }

    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .enable_time()
            .build()
            .expect("tokio multi_thread");
        group.bench_function("tokio", |b| {
            b.iter(|| {
                rt.block_on(async {
                    let mut handles = Vec::with_capacity(N as usize);
                    for _ in 0..N {
                        handles.push(tokio::task::spawn(async {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }));
                    }
                    for h in handles {
                        h.await.expect("join");
                    }
                })
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_block_on_ready,
    bench_spawn_and_await,
    bench_spawn_many_join_all,
    bench_mpsc_ping_pong_ct,
    bench_spawn_storm,
    bench_work_steal_fan_out,
    bench_channel_fan_in,
    bench_sleep_concurrent,
);
criterion_main!(benches);

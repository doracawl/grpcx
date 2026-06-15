//! Multi-thread runtime (v0.5) integration tests — functional coverage
//! (spawn/join, fan-out/in, cross-worker channels, sleep, readable) plus
//! a medium stress run and a cooperative-budget fairness check. Driven
//! entirely through the public API so the same suite runs on every
//! platform.

use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use grpcx_rt::{Runtime, channel, readable, sleep, timeout, yield_now};

#[test]
fn block_on_returns_value() {
    let rt = Runtime::new(2).expect("runtime");
    assert_eq!(rt.block_on(async { 42 }), 42);
}

#[test]
fn spawn_many_and_join_sum() {
    let rt = Runtime::new(4).expect("runtime");
    let h = rt.handle();
    const N: u64 = 1_000;
    let total = rt.block_on(async move {
        let mut handles = Vec::with_capacity(N as usize);
        for i in 0..N {
            handles.push(h.spawn(async move { i }));
        }
        let mut sum = 0u64;
        for jh in handles {
            sum += jh.await;
        }
        sum
    });
    assert_eq!(total, (0..N).sum());
}

#[test]
fn nested_spawn_via_handle() {
    let rt = Runtime::new(3).expect("runtime");
    let h = rt.handle();
    let v = rt.block_on(async move {
        let h2 = h.clone();
        let outer = h.spawn(async move {
            let inner = h2.spawn(async { 10u32 });
            inner.await + 5
        });
        outer.await
    });
    assert_eq!(v, 15);
}

#[test]
fn channel_fan_out_in_across_workers() {
    let rt = Runtime::new(4).expect("runtime");
    let h = rt.handle();
    const P: u64 = 8;
    const PER: u64 = 100;
    let (total, count) = rt.block_on(async move {
        let (tx, rx) = channel::<u64>(64);
        for _ in 0..P {
            let tx = tx.clone();
            h.spawn(async move {
                for i in 0..PER {
                    while tx.try_send(i).is_err() {
                        yield_now().await;
                    }
                }
            });
        }
        drop(tx); // last sender gone once producers finish ⇒ recv → None
        let mut total = 0u64;
        let mut count = 0u64;
        while let Some(v) = rx.recv().await {
            total += v;
            count += 1;
        }
        (total, count)
    });
    assert_eq!(count, P * PER);
    assert_eq!(total, P * (0..PER).sum::<u64>());
}

#[test]
fn channel_ping_pong_cross_worker() {
    let rt = Runtime::new(2).expect("runtime");
    let h = rt.handle();
    const ROUNDS: u32 = 20;
    let (final_v, b_count) = rt.block_on(async move {
        let (to_b, from_a) = channel::<u32>(1);
        let (to_a, from_b) = channel::<u32>(1);
        let b = h.spawn(async move {
            // Echo v+1 for each received value until the sender closes.
            let mut count = 0u32;
            while let Some(v) = from_a.recv().await {
                count += 1;
                if to_a.try_send(v + 1).is_err() {
                    break;
                }
            }
            count
        });
        let mut v = 0u32;
        for _ in 0..ROUNDS {
            to_b.try_send(v).expect("send to b");
            v = from_b.recv().await.expect("reply from b");
        }
        drop(to_b); // close so b exits its loop
        let b_count = b.await;
        (v, b_count)
    });
    assert_eq!(final_v, ROUNDS); // each round adds 1
    assert_eq!(b_count, ROUNDS);
}

#[test]
fn sleep_over_mt_waits() {
    let rt = Runtime::new(2).expect("runtime");
    let start = Instant::now();
    rt.block_on(async {
        sleep(Duration::from_millis(30)).await;
    });
    assert!(start.elapsed() >= Duration::from_millis(30));
}

#[test]
fn concurrent_sleeps_complete() {
    let rt = Runtime::new(3).expect("runtime");
    let h = rt.handle();
    let order = rt.block_on(async move {
        let a = h.spawn(async {
            sleep(Duration::from_millis(50)).await;
            50u32
        });
        let b = h.spawn(async {
            sleep(Duration::from_millis(10)).await;
            10u32
        });
        // Await the slower first by value; both must still fire.
        let av = a.await;
        let bv = b.await;
        (av, bv)
    });
    assert_eq!(order, (50, 10));
}

#[test]
fn readable_over_mt() {
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(b"x").expect("write");
    drop(writer); // byte stays buffered in the pipe
    let fd = reader.as_raw_fd();

    let rt = Runtime::new(2).expect("runtime");
    rt.block_on(async move {
        readable(fd).await;
        drop(reader); // keep the fd valid across the await
    });
}

/// A `readable` that never fires (nothing written to the pipe), wrapped
/// in a `timeout` that wins: the still-pending `Readable` is re-polled
/// (waker refresh) and then dropped, exercising the MT runtime's
/// readiness refresh + one-shot deregister-on-cancel path.
#[test]
fn readable_cancelled_when_dropped_pending() {
    let (reader, _writer) = std::io::pipe().expect("pipe"); // never readable
    let fd = reader.as_raw_fd();
    let rt = Runtime::new(2).expect("runtime");
    rt.block_on(async move {
        let r = timeout(Duration::from_millis(20), readable(fd)).await;
        assert!(r.is_err(), "timeout should fire before the fd is readable");
        drop(reader);
    });
}

#[test]
fn medium_stress_tens_of_thousands_of_tasks() {
    let rt = Runtime::new(4).expect("runtime");
    let h = rt.handle();
    const K: u64 = 50_000;
    let total = rt.block_on(async move {
        let mut handles = Vec::with_capacity(K as usize);
        for i in 0..K {
            handles.push(h.spawn(async move { i }));
        }
        let mut sum = 0u64;
        for jh in handles {
            sum += jh.await;
        }
        sum
    });
    assert_eq!(total, (0..K).sum());
}

/// On a single worker, a task that keeps receiving ready values would
/// monopolize the thread and never let a peer run — unless the
/// cooperative budget forces a periodic yield. The peer's flag must flip
/// before the greedy loop finishes.
#[test]
fn cooperative_budget_prevents_starvation() {
    let rt = Runtime::new(1).expect("runtime");
    let h = rt.handle();
    let ran_peer = rt.block_on(async move {
        let (tx, rx) = channel::<u32>(64);
        for i in 0..64 {
            tx.try_send(i).expect("prefill");
        }
        let flag = Arc::new(AtomicBool::new(false));
        let flag_for_peer = Arc::clone(&flag);
        let _peer = h.spawn(async move {
            flag_for_peer.store(true, Ordering::SeqCst);
        });

        // Greedy: keep the buffer non-empty and receive in a tight loop.
        // Without the budget this single worker would never schedule the
        // peer until this loop ends; with it, a yield lands mid-loop.
        let mut got = 0u32;
        while got < 2_000 {
            let _ = tx.try_send(got); // refill so data is always ready
            let _ = rx.recv().await;
            got += 1;
            if flag.load(Ordering::SeqCst) {
                break; // peer ran — budget worked
            }
        }
        flag.load(Ordering::SeqCst)
    });
    assert!(ran_peer, "cooperative budget should let a peer task run");
}

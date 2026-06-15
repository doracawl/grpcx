//! Fuzz-only internals (unstable, `#[doc(hidden)]`). Mirrors
//! grpcx-io's `__fuzz_helpers`: the fuzz targets live in the `fuzz/`
//! subcrate and can only see the public surface, so the byte-driven op
//! loops over the crate-private `no_std` cores (the Chase-Lev `deque`,
//! the timer `Wheel`) live here, where they have access — and assert
//! the structures' invariants so a fuzzer-found violation surfaces as a
//! panic. NOT a stable API; may change or vanish without notice.

extern crate std;

use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::AtomicBool;
use core::task::{RawWaker, RawWakerVTable, Waker};

use crate::channel::{TryRecvError, TrySendError, channel};
use crate::deque::{Deque, Steal};
use crate::executor::{block_on, spawn};
use crate::timer::{Entry, Wheel};

/// A waker whose vtable ignores the (null) data pointer — lets the
/// timer wheel be driven without a runtime.
fn noop_waker() -> Waker {
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(core::ptr::null(), &VT)
    }
    fn noop(_: *const ()) {}
    static VT: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: the vtable's functions ignore the (null) data pointer.
    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VT)) }
}

/// Drive the lock-free Chase-Lev deque single-threaded with a byte
/// script of push / pop / steal, checking **conservation**: every value
/// is pushed under a unique id and must be removed exactly once (a
/// double-take from the speculative-read path, a lost value, or a
/// phantom value all trip an assert). Single-threaded by design — the
/// concurrent paths are covered by the soak; this pins the sequential
/// logic, bounds math, and the `unsafe` move-out discipline.
pub fn drive_deque(data: &[u8]) {
    let d: Deque<u64> = Deque::new(64);
    let mut live: BTreeSet<u64> = BTreeSet::new();
    let mut next_id: u64 = 0;

    for &byte in data.iter().take(4096) {
        match byte & 0b11 {
            // Bias toward push (2 of 4 cases) so the deque actually fills.
            0 | 1 => {
                if d.push(next_id).is_ok() {
                    assert!(live.insert(next_id), "deque: duplicate live id {next_id}");
                }
                next_id = next_id.wrapping_add(1);
            }
            2 => {
                if let Some(v) = d.pop() {
                    assert!(live.remove(&v), "deque: pop of bogus/duplicate value {v}");
                }
            }
            3 => match d.steal() {
                Steal::Data(v) => {
                    assert!(live.remove(&v), "deque: steal of bogus/duplicate value {v}");
                }
                Steal::Empty | Steal::Retry => {}
            },
            _ => unreachable!(),
        }
    }

    // Drain: everything still live must come out exactly once, nothing else.
    while let Some(v) = d.pop() {
        assert!(live.remove(&v), "deque: drain of bogus/duplicate value {v}");
    }
    assert!(live.is_empty(), "deque: {} value(s) lost", live.len());
}

/// Drive the hierarchical timing wheel with a byte script of insert /
/// advance, checking the wheel's two invariants: a fired timer's
/// deadline is `<= now` (**never early**), and after advancing past all
/// deadlines **every** inserted timer has fired (**never stuck**).
pub fn drive_timer(data: &[u8]) {
    let mut wheel = Wheel::new();
    let mut now: u64 = 0;
    let mut pending: u64 = 0;

    for chunk in data.chunks(2).take(2048) {
        let byte = chunk[0];
        let arg = u64::from(*chunk.get(1).unwrap_or(&0));
        if byte & 1 == 0 {
            // Insert at a strictly-future deadline (+1 avoids when < now).
            let when = now.saturating_add(arg + 1);
            wheel.insert(Entry {
                when,
                waker: noop_waker(),
                done: Arc::new(AtomicBool::new(false)),
            });
            pending += 1;
        } else {
            now = now.saturating_add(arg);
            let mut fired: Vec<Entry> = Vec::new();
            wheel.advance(now, &mut fired);
            for e in &fired {
                assert!(
                    e.when <= now,
                    "timer: fired early — when={} now={now}",
                    e.when
                );
            }
            pending = pending
                .checked_sub(fired.len() as u64)
                .expect("timer: fired more timers than were inserted");
        }
    }

    // Advance past every possible deadline; all pending timers must fire.
    now = now.saturating_add(u64::from(u32::MAX) + 1);
    let mut fired: Vec<Entry> = Vec::new();
    wheel.advance(now, &mut fired);
    pending = pending
        .checked_sub(fired.len() as u64)
        .expect("timer: fired more timers than were inserted");
    assert_eq!(pending, 0, "timer: {pending} timer(s) never fired");
}

/// Drive the bounded `mpsc` channel with a byte script of try_send /
/// try_recv / clone-sender / drop-sender / drop-receiver, checking
/// **conservation**: every value try_send'd successfully must be
/// received exactly once, or be observable as live in the buffer when
/// the receiver drops. Single-threaded; the cross-thread wake path is
/// covered by the stress soak.
pub fn drive_channel(data: &[u8]) {
    // Cap derived from the first byte (1..=16) so the byte script can
    // explore both tiny and small ring buffers.
    let cap = data.first().copied().unwrap_or(4) as usize % 16 + 1;
    let (mut tx_opt, mut rx_opt) = {
        let (tx, rx) = channel::<u64>(cap);
        (Some(tx), Some(rx))
    };
    let mut extra_senders: Vec<crate::channel::Sender<u64>> = Vec::new();
    let mut live: BTreeSet<u64> = BTreeSet::new();
    let mut next_id: u64 = 0;

    for &byte in data.iter().skip(1).take(4096) {
        match byte & 0b111 {
            // Bias toward send (3 / 8) so the ring actually fills.
            0..=2 => {
                let id = next_id;
                next_id = next_id.wrapping_add(1);
                if let Some(tx) = tx_opt.as_ref().or_else(|| extra_senders.first()) {
                    match tx.try_send(id) {
                        Ok(()) => {
                            assert!(live.insert(id), "channel: duplicate live id {id}");
                        }
                        Err(TrySendError::Full(v)) => {
                            assert_eq!(v, id, "channel: Full returned wrong value");
                        }
                        Err(TrySendError::Closed(v)) => {
                            assert_eq!(v, id, "channel: Closed returned wrong value");
                            assert!(
                                rx_opt.is_none(),
                                "channel: Closed reported while receiver alive"
                            );
                        }
                    }
                }
            }
            3..=4 => {
                if let Some(rx) = rx_opt.as_ref() {
                    match rx.try_recv() {
                        Ok(v) => {
                            assert!(live.remove(&v), "channel: recv of bogus/dup value {v}");
                        }
                        Err(TryRecvError::Empty) => {
                            // OK: buffer empty.
                        }
                        Err(TryRecvError::Disconnected) => {
                            assert!(
                                tx_opt.is_none() && extra_senders.is_empty(),
                                "channel: Disconnected reported while senders alive"
                            );
                        }
                    }
                }
            }
            5 => {
                // Clone the original sender (cap extras at 4).
                if let Some(tx) = tx_opt.as_ref()
                    && extra_senders.len() < 4
                {
                    extra_senders.push(tx.clone());
                }
            }
            6 => {
                // Drop one extra sender, or — if none — the original.
                if extra_senders.pop().is_none() {
                    tx_opt = None;
                }
            }
            7 => {
                // Drop the receiver. Future try_send must surface Closed.
                rx_opt = None;
            }
            _ => unreachable!(),
        }
    }

    // Drop all senders so any remaining recv observes the disconnect.
    drop(tx_opt.take());
    extra_senders.clear();

    if let Some(rx) = rx_opt.as_ref() {
        // Drain — every still-live id must come out exactly once; then
        // Disconnected.
        loop {
            match rx.try_recv() {
                Ok(v) => {
                    assert!(live.remove(&v), "channel: drain of bogus/dup value {v}");
                }
                Err(TryRecvError::Empty) => {
                    panic!("channel: Empty after all senders dropped (should be Disconnected)");
                }
                Err(TryRecvError::Disconnected) => break,
            }
        }
        assert!(
            live.is_empty(),
            "channel: {} value(s) lost (senders dropped, receiver drained)",
            live.len()
        );
    }
}

/// Drive a spawn-storm on the single-thread `block_on` executor with a
/// byte script: `N ≤ 8` producers each push their `per ≤ 32` values
/// through a shared `mpsc` channel sized to fit the full storm (so
/// `try_send` never returns `Full`); the root awaits a deterministic
/// sum + count to catch lost / duplicated / corrupted messages, and
/// then `await`s every `JoinHandle` to confirm `JoinShared`'s Empty →
/// Ready transition.
///
/// Exercises the slab pool (`Inner::tasks`), the per-task wake
/// state-machine (`make_waker` + `vt_wake_by_ref`), the channel
/// `recv` await path with cross-task wake, and the `JoinHandle`
/// surface. The MT runtime + cooperative-budget paths are covered by
/// the stress soak; this harness is single-thread by design so
/// producers cannot starve the root via a busy-spin on a full
/// channel (the `current_thread` executor drains its run queue
/// without fairness).
pub fn drive_runtime(data: &[u8]) {
    let n = (data.first().copied().unwrap_or(2) as u64 % 8) + 1;
    let per = (data.get(1).copied().unwrap_or(4) as u64 % 32) + 1;
    // Cap >= n * per guarantees `try_send` never returns `Full` — the
    // single-thread `block_on` drains its run queue without fairness,
    // so a producer that spun `yield_now` on a full channel would
    // starve the root receiver.
    let cap = (n * per) as usize;

    let mut expected_sum = 0u64;
    for p in 0..n {
        for i in 0..per {
            expected_sum = expected_sum.wrapping_add(p.wrapping_mul(per).wrapping_add(i));
        }
    }

    let total = block_on(async move {
        let (tx, rx) = channel::<u64>(cap);
        let mut handles = Vec::with_capacity(n as usize);
        for p in 0..n {
            let tx = tx.clone();
            handles.push(spawn(async move {
                for i in 0..per {
                    let val = p.wrapping_mul(per).wrapping_add(i);
                    // Cap guarantees space — Err here is a real bug.
                    tx.try_send(val)
                        .expect("runtime: try_send unexpectedly failed");
                }
                p
            }));
        }
        drop(tx); // last original sender goes when producers finish

        let mut total = 0u64;
        let mut count = 0u64;
        while let Some(v) = rx.recv().await {
            total = total.wrapping_add(v);
            count += 1;
        }
        let mut join_sum = 0u64;
        for h in handles {
            join_sum = join_sum.wrapping_add(h.await);
        }
        assert_eq!(count, n * per, "runtime: message conservation");
        // 0 + 1 + ... + (n-1) — confirms every handle delivered.
        assert_eq!(join_sum, (0..n).sum::<u64>(), "runtime: join_sum mismatch");
        total
    });
    assert_eq!(total, expected_sum, "runtime: sum mismatch");
}

#[cfg(test)]
mod tests {
    use super::{drive_channel, drive_deque, drive_runtime, drive_timer};
    use alloc::vec::Vec;

    /// A spread of byte values hits every op branch (push/pop/steal,
    /// insert/advance) plus the drain + conservation asserts — so the
    /// fuzz drivers are validated under plain `cargo test`, no nightly
    /// required, and counted by coverage.
    fn spread() -> Vec<u8> {
        (0u16..512).map(|i| i as u8).collect()
    }

    #[test]
    fn drive_deque_smoke() {
        drive_deque(&spread());
        drive_deque(&[]);
    }

    #[test]
    fn drive_timer_smoke() {
        drive_timer(&spread());
        drive_timer(&[]);
    }

    #[test]
    fn drive_channel_smoke() {
        drive_channel(&spread());
        drive_channel(&[]);
        // A spread with no `drop receiver` byte (no `b & 7 == 7`) so the
        // post-loop drain path (lines: rx alive → loop try_recv → Ok /
        // Disconnected) is exercised; this also hits the "drop last
        // sender" branch via the implicit drop of `tx_opt` at end.
        let no_drop_rx: Vec<u8> = spread().into_iter().filter(|b| b & 7 != 7).collect();
        drive_channel(&no_drop_rx);
    }

    #[test]
    fn drive_runtime_smoke() {
        drive_runtime(&spread());
        drive_runtime(&[]);
    }
}

//! Lock-free Chase-Lev work-stealing deque (design intent #4).
//!
//! Single owner, many thieves. The owner pushes/pops at the *bottom*;
//! thieves steal from the *top*. Fixed capacity (rounded up to a power
//! of two); a full `push` returns the value so the caller can overflow
//! to a global injector — matching tokio's "fixed local ring + global
//! queue" shape.
//!
//! The memory orderings follow Lê, Pop, Cohen & Zappa Nardelli,
//! *"Correct and Efficient Work-Stealing for Weak Memory Models"*
//! (PPoPP 2013) — the model-checked formulation that is sound on weak
//! architectures (ARM/AArch64), not just x86-TSO. `top`/`bottom` are
//! monotonically increasing `usize` counters; the 64-bit width makes
//! ABA on the `top` CAS unreachable in practice. Slot reads are
//! speculative (`ptr::read`): a thief reads the slot *before* the CAS
//! and `mem::forget`s the copy if the CAS loses, so a value is moved
//! out exactly once across the one winning `pop`/`steal`.
//!
//! `no_std` (core atomics + `alloc`).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering, fence};

/// Outcome of a [`Deque::steal`].
pub(crate) enum Steal<T> {
    /// The deque was observed empty.
    Empty,
    /// Lost a race with the owner or another thief; caller may retry.
    Retry,
    /// Successfully stole a value.
    Data(T),
}

pub(crate) struct Deque<T> {
    /// Next index a thief will steal from (front).
    top: AtomicUsize,
    /// Next index the owner will push into (back).
    bottom: AtomicUsize,
    buffer: Box<[UnsafeCell<MaybeUninit<T>>]>,
    mask: usize,
}

// SAFETY: the Chase-Lev protocol guarantees each slot is moved out by
// exactly one of pop/steal, so concurrent access through the shared
// `&Deque` never aliases a live value. `T: Send` is required because
// values cross threads (owner → thief).
unsafe impl<T: Send> Sync for Deque<T> {}
unsafe impl<T: Send> Send for Deque<T> {}

impl<T> Deque<T> {
    /// Create a deque whose capacity is `capacity` rounded up to a
    /// power of two (minimum 32).
    pub(crate) fn new(capacity: usize) -> Self {
        let cap = capacity.max(32).next_power_of_two();
        let buffer: Vec<UnsafeCell<MaybeUninit<T>>> = (0..cap)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect();
        Self {
            top: AtomicUsize::new(0),
            bottom: AtomicUsize::new(0),
            buffer: buffer.into_boxed_slice(),
            mask: cap - 1,
        }
    }

    fn cap(&self) -> usize {
        self.mask + 1
    }

    /// Raw slot pointer for index `i` (already reduced mod capacity by
    /// the caller via `& self.mask`).
    #[inline]
    fn slot(&self, i: usize) -> *mut T {
        // SAFETY: `i & mask` is in bounds; `MaybeUninit<T>` and `T`
        // share layout, so the cast yields a valid `*mut T` slot.
        unsafe { (*self.buffer[i & self.mask].get()).as_mut_ptr() }
    }

    /// Push to the bottom (owner only). Returns `Err(value)` if full.
    pub(crate) fn push(&self, value: T) -> Result<(), T> {
        let b = self.bottom.load(Ordering::Relaxed);
        let t = self.top.load(Ordering::Acquire);
        if b.wrapping_sub(t) >= self.cap() {
            return Err(value); // full — caller overflows elsewhere
        }
        // SAFETY: the slot is free (the full-check guarantees the value
        // previously here, if any, was already taken). Owner-only write.
        unsafe { self.slot(b).write(value) };
        // Publish the slot write before the bottom increment so a thief
        // that observes the new `bottom` also observes the value.
        fence(Ordering::Release);
        self.bottom.store(b.wrapping_add(1), Ordering::Relaxed);
        Ok(())
    }

    /// Pop from the bottom (owner only).
    pub(crate) fn pop(&self) -> Option<T> {
        let b = self.bottom.load(Ordering::Relaxed);
        let t = self.top.load(Ordering::Relaxed);
        if b.wrapping_sub(t) == 0 {
            return None; // empty
        }
        let b = b.wrapping_sub(1);
        self.bottom.store(b, Ordering::Relaxed);
        // Order the bottom store before the top load (Dekker-style).
        fence(Ordering::SeqCst);
        let t = self.top.load(Ordering::Relaxed);
        let len = b.wrapping_sub(t) as isize;
        if len < 0 {
            // A thief took the last element between our store and load.
            self.bottom.store(b.wrapping_add(1), Ordering::Relaxed);
            return None;
        }
        // SAFETY: bottom element is owned by us; thieves only take from
        // `top` (< b here). Speculative read; committed below.
        let value = unsafe { self.slot(b).read() };
        if len > 0 {
            return Some(value); // more than one element — uncontended
        }
        // len == 0: last element, racing a thief on `top`.
        let won = self
            .top
            .compare_exchange(t, t.wrapping_add(1), Ordering::SeqCst, Ordering::Relaxed)
            .is_ok();
        self.bottom.store(b.wrapping_add(1), Ordering::Relaxed);
        if won {
            Some(value)
        } else {
            core::mem::forget(value); // thief won; we never had it
            None
        }
    }

    /// Steal from the top (any thread).
    pub(crate) fn steal(&self) -> Steal<T> {
        let t = self.top.load(Ordering::Acquire);
        // Order the top load before the bottom load.
        fence(Ordering::SeqCst);
        let b = self.bottom.load(Ordering::Acquire);
        if b.wrapping_sub(t) as isize <= 0 {
            return Steal::Empty;
        }
        // SAFETY: speculative read of the top slot; if the CAS below
        // loses we `forget` it, so it is moved out only on success.
        let value = unsafe { self.slot(t).read() };
        if self
            .top
            .compare_exchange(t, t.wrapping_add(1), Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            Steal::Data(value)
        } else {
            core::mem::forget(value); // lost the race; do not drop
            Steal::Retry
        }
    }
}

impl<T> Drop for Deque<T> {
    fn drop(&mut self) {
        // No concurrency at drop; drain and drop any remaining values.
        while self.pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use alloc::sync::Arc;
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::thread;

    #[test]
    fn push_pop_lifo_single_thread() {
        let d: Deque<u32> = Deque::new(32);
        assert!(d.pop().is_none());
        for i in 0..10 {
            d.push(i).unwrap();
        }
        // Owner pop is LIFO from the bottom.
        for i in (0..10).rev() {
            assert_eq!(d.pop(), Some(i));
        }
        assert!(d.pop().is_none());
    }

    #[test]
    fn push_full_returns_value() {
        let d: Deque<u32> = Deque::new(32); // cap 32
        for i in 0..32 {
            d.push(i).unwrap();
        }
        assert!(matches!(d.push(999), Err(999)));
    }

    #[test]
    fn steal_is_fifo_from_top() {
        let d: Deque<u32> = Deque::new(32);
        for i in 0..5 {
            d.push(i).unwrap();
        }
        // Steal takes from the top (oldest first).
        for i in 0..5 {
            match d.steal() {
                Steal::Data(v) => assert_eq!(v, i),
                other => panic!(
                    "expected Data({i}), got {:?}",
                    core::mem::discriminant(&other)
                ),
            }
        }
        assert!(matches!(d.steal(), Steal::Empty));
    }

    /// The core correctness property: under one owner pushing/popping
    /// and many thieves stealing, every pushed item is consumed exactly
    /// once — no loss, no duplication. Run on weak-memory (aarch64) and
    /// TSO (x86_64) to shake out ordering bugs.
    #[test]
    fn concurrent_steal_exactly_once() {
        const N: u32 = 200_000;
        let d: Arc<Deque<u32>> = Arc::new(Deque::new(256));
        let consumed: Arc<Mutex<HashSet<u32>>> = Arc::new(Mutex::new(HashSet::new()));

        let n_thieves = 4;
        let mut thieves = Vec::new();
        let stop = Arc::new(core::sync::atomic::AtomicBool::new(false));
        for _ in 0..n_thieves {
            let d = Arc::clone(&d);
            let consumed = Arc::clone(&consumed);
            let stop = Arc::clone(&stop);
            thieves.push(thread::spawn(move || {
                let mut local = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    match d.steal() {
                        Steal::Data(v) => local.push(v),
                        Steal::Empty | Steal::Retry => thread::yield_now(),
                    }
                }
                // Drain whatever is left after stop.
                while let Steal::Data(v) = d.steal() {
                    local.push(v);
                }
                let mut set = consumed.lock().unwrap();
                for v in local {
                    assert!(set.insert(v), "duplicate consume of {v}");
                }
            }));
        }

        // Owner: push 0..N, occasionally popping (which also consumes).
        let mut owner_consumed = Vec::new();
        let mut i = 0;
        while i < N {
            // Push a small burst.
            for _ in 0..16 {
                if i >= N {
                    break;
                }
                while d.push(i).is_err() {
                    // Local full — pop one to make room (consume it).
                    if let Some(v) = d.pop() {
                        owner_consumed.push(v);
                    }
                }
                i += 1;
            }
            // Occasionally pop from the bottom too.
            if let Some(v) = d.pop() {
                owner_consumed.push(v);
            }
        }
        // Owner drains the rest it can reach.
        while let Some(v) = d.pop() {
            owner_consumed.push(v);
        }
        stop.store(true, Ordering::Relaxed);
        for h in thieves {
            h.join().unwrap();
        }

        let mut set = consumed.lock().unwrap();
        for v in owner_consumed {
            assert!(set.insert(v), "duplicate consume (owner) of {v}");
        }
        // Every value 0..N consumed exactly once.
        assert_eq!(
            set.len(),
            N as usize,
            "lost items: got {} of {N}",
            set.len()
        );
        for v in 0..N {
            assert!(set.contains(&v), "missing item {v}");
        }
    }

    /// Stress with `Arc<T>` payloads to confirm no double-drop / leak
    /// of non-Copy values across the speculative-read path.
    #[test]
    fn concurrent_steal_arc_no_double_drop() {
        const N: usize = 50_000;
        let d: Arc<Deque<Arc<usize>>> = Arc::new(Deque::new(128));
        let live = Arc::new(core::sync::atomic::AtomicUsize::new(0));

        let stop = Arc::new(core::sync::atomic::AtomicBool::new(false));
        let mut thieves = Vec::new();
        for _ in 0..3 {
            let d = Arc::clone(&d);
            let live = Arc::clone(&live);
            let stop = Arc::clone(&stop);
            thieves.push(thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if let Steal::Data(v) = d.steal() {
                        live.fetch_sub(1, Ordering::Relaxed);
                        drop(v);
                    } else {
                        thread::yield_now();
                    }
                }
                while let Steal::Data(v) = d.steal() {
                    live.fetch_sub(1, Ordering::Relaxed);
                    drop(v);
                }
            }));
        }
        for i in 0..N {
            let item = Arc::new(i);
            live.fetch_add(1, Ordering::Relaxed);
            while d.push(Arc::clone(&item)).is_err() {
                if let Some(v) = d.pop() {
                    live.fetch_sub(1, Ordering::Relaxed);
                    drop(v);
                }
            }
            drop(item);
        }
        while let Some(v) = d.pop() {
            live.fetch_sub(1, Ordering::Relaxed);
            drop(v);
        }
        stop.store(true, Ordering::Relaxed);
        for h in thieves {
            h.join().unwrap();
        }
        // Every pushed Arc was consumed exactly once.
        assert_eq!(live.load(Ordering::Relaxed), 0);
    }
}

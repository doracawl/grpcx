//! Hierarchical timing wheel (design intent #3) — `no_std` core.
//!
//! Six levels of 64 slots each; a slot at level `L` spans `64^L` ticks,
//! so the wheel covers `64^6 ≈ 6.9e10` ticks before the top level
//! saturates. Insertion is O(1): a timer's level is the highest 6-bit
//! group in which its deadline differs from the current `elapsed`
//! cursor, and its slot is the corresponding 6 bits of the deadline.
//! Firing is amortized O(1) via cascade — when the cursor reaches a
//! higher level's slot boundary, that slot's timers are re-inserted and
//! sift down toward level 0, where they fire.
//!
//! This module is tick-agnostic: it knows nothing of wall-clock time.
//! The executor maps a monotonic `Instant` to ticks (1 tick = 1 ms) and
//! drives [`Wheel::advance`]. Timer cancellation is implicit — a dropped
//! waiter leaves a dead entry that fires harmlessly later (a spurious
//! wake of an already-finished task is a no-op); explicit removal is a
//! deferred optimization, not needed for correctness.

use alloc::vec::Vec;
use core::sync::atomic::AtomicBool;
use core::task::Waker;

const SLOTS: u64 = 64;
const LEVELS: usize = 6;
const SLOT_BITS: u32 = 6; // log2(SLOTS)
const SLOT_MASK: u64 = SLOTS - 1;

/// Ticks spanned by one slot at `level` (`64^level`).
const fn slot_range(level: usize) -> u64 {
    1u64 << (SLOT_BITS * level as u32)
}

/// The level a `deadline` belongs to given the current `elapsed` cursor:
/// the highest 6-bit group in which the two differ.
fn level_for(elapsed: u64, deadline: u64) -> usize {
    let masked = elapsed ^ deadline;
    if masked == 0 {
        return 0;
    }
    let significant = 63 - masked.leading_zeros();
    ((significant / SLOT_BITS) as usize).min(LEVELS - 1)
}

/// One scheduled timer.
pub(crate) struct Entry {
    /// Deadline in ticks. Kept for re-bucketing during cascade.
    pub when: u64,
    /// Woken when the timer fires.
    pub waker: Waker,
    /// Shared with the waiter; set true by the executor on fire so the
    /// waiter can distinguish a real fire from a spurious poll.
    pub done: alloc::sync::Arc<AtomicBool>,
}

struct Level {
    /// Bit `i` set iff `slots[i]` is non-empty.
    occupied: u64,
    slots: [Vec<Entry>; SLOTS as usize],
}

impl Level {
    fn new() -> Self {
        Self {
            occupied: 0,
            slots: core::array::from_fn(|_| Vec::new()),
        }
    }
}

/// The expiration the wheel will next process: a slot whose entries
/// fire (level 0) or cascade down (higher levels) when the cursor
/// reaches `deadline`.
struct Expiration {
    level: usize,
    slot: usize,
    deadline: u64,
}

/// A hierarchical timing wheel keyed in abstract ticks.
pub(crate) struct Wheel {
    /// Ticks fully processed; timers with `when <= elapsed` have fired.
    elapsed: u64,
    levels: [Level; LEVELS],
}

impl Wheel {
    pub(crate) fn new() -> Self {
        Self {
            elapsed: 0,
            levels: core::array::from_fn(|_| Level::new()),
        }
    }

    /// Current cursor in ticks.
    #[cfg(test)]
    pub(crate) fn elapsed(&self) -> u64 {
        self.elapsed
    }

    /// Schedule `entry` to fire at `entry.when` ticks.
    pub(crate) fn insert(&mut self, entry: Entry) {
        let when = entry.when.max(self.elapsed);
        let level = level_for(self.elapsed, when);
        let slot = ((when >> (SLOT_BITS * level as u32)) & SLOT_MASK) as usize;
        self.levels[level].slots[slot].push(entry);
        self.levels[level].occupied |= 1 << slot;
    }

    /// The earliest tick at which the wheel must next do work (fire or
    /// cascade), or `None` when no timers are scheduled.
    pub(crate) fn next_deadline(&self) -> Option<u64> {
        self.next_expiration().map(|e| e.deadline)
    }

    /// True when no timers are scheduled.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.levels.iter().all(|l| l.occupied == 0)
    }

    fn next_expiration(&self) -> Option<Expiration> {
        // The lowest occupied level holds the nearest deadline: a timer
        // sits at level L only because its deadline shares all higher
        // bit-groups with `elapsed`, so level 0's window is strictly
        // nearer than level 1's, and so on.
        for level in 0..LEVELS {
            let lv = &self.levels[level];
            if lv.occupied == 0 {
                continue;
            }
            let sr = slot_range(level);
            let now_slot = ((self.elapsed / sr) % SLOTS) as u32;
            let dist = u64::from(lv.occupied.rotate_right(now_slot).trailing_zeros());
            let aligned = self.elapsed - (self.elapsed % sr);
            let deadline = aligned + dist * sr;
            let slot = ((u64::from(now_slot) + dist) % SLOTS) as usize;
            return Some(Expiration {
                level,
                slot,
                deadline,
            });
        }
        None
    }

    /// Advance the cursor to `now`, pushing every fired timer's entry
    /// into `out`. Cascades higher-level slots down as their boundaries
    /// are crossed.
    pub(crate) fn advance(&mut self, now: u64, out: &mut Vec<Entry>) {
        while let Some(exp) = self.next_expiration() {
            if exp.deadline > now {
                break;
            }
            self.elapsed = exp.deadline;
            let entries = core::mem::take(&mut self.levels[exp.level].slots[exp.slot]);
            self.levels[exp.level].occupied &= !(1 << exp.slot);
            if exp.level == 0 {
                out.extend(entries);
            } else {
                for entry in entries {
                    self.insert(entry); // cascade down toward level 0
                }
            }
        }
        if self.elapsed < now {
            self.elapsed = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::task::{RawWaker, RawWakerVTable, Waker};

    // A no-op waker so the wheel can be unit-tested with synthetic ticks
    // (no executor, no real time).
    fn noop_waker() -> Waker {
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(core::ptr::null(), &VT)
        }
        fn noop(_: *const ()) {}
        static VT: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        // SAFETY: the vtable's functions ignore the (null) data pointer.
        unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VT)) }
    }

    fn entry(when: u64) -> (Entry, Arc<AtomicBool>) {
        let done = Arc::new(AtomicBool::new(false));
        (
            Entry {
                when,
                waker: noop_waker(),
                done: Arc::clone(&done),
            },
            done,
        )
    }

    /// Advance to `now` and return the `when`s that fired, in order.
    fn fire_to(wheel: &mut Wheel, now: u64) -> Vec<u64> {
        let mut out = Vec::new();
        wheel.advance(now, &mut out);
        out.into_iter().map(|e| e.when).collect()
    }

    #[test]
    fn fires_at_deadline_not_before() {
        let mut w = Wheel::new();
        let (e, _done) = entry(5);
        w.insert(e);
        assert!(fire_to(&mut w, 4).is_empty());
        assert_eq!(fire_to(&mut w, 5), [5]);
        assert!(w.is_empty());
    }

    #[test]
    fn fires_in_deadline_order_same_level() {
        let mut w = Wheel::new();
        for when in [3, 1, 2] {
            w.insert(entry(when).0);
        }
        assert_eq!(fire_to(&mut w, 10), [1, 2, 3]);
    }

    #[test]
    fn cascades_across_levels() {
        // 100 lands in level 1 (slot 1), must cascade down to level 0
        // and fire exactly at tick 100.
        let mut w = Wheel::new();
        w.insert(entry(100).0);
        assert!(fire_to(&mut w, 63).is_empty());
        assert!(fire_to(&mut w, 99).is_empty());
        assert_eq!(fire_to(&mut w, 100), [100]);
        assert!(w.is_empty());
    }

    #[test]
    fn deep_cascade_high_level() {
        // 64^2 + 64 + 7 spans level 2; exercises multi-step cascade.
        let when = 64 * 64 + 64 + 7;
        let mut w = Wheel::new();
        w.insert(entry(when).0);
        assert!(fire_to(&mut w, when - 1).is_empty());
        assert_eq!(fire_to(&mut w, when), [when]);
    }

    #[test]
    fn mixed_deadlines_fire_in_order_across_advances() {
        let mut w = Wheel::new();
        for when in [5, 70, 4096, 130, 64] {
            w.insert(entry(when).0);
        }
        let mut fired = Vec::new();
        for now in [10, 65, 140, 5000] {
            fired.extend(fire_to(&mut w, now));
        }
        assert_eq!(fired, [5, 64, 70, 130, 4096]);
        assert!(w.is_empty());
    }

    #[test]
    fn next_deadline_tracks_earliest() {
        let mut w = Wheel::new();
        assert_eq!(w.next_deadline(), None);
        w.insert(entry(500).0);
        w.insert(entry(20).0);
        // 20 is in level 0; its slot boundary is the earliest work.
        assert_eq!(w.next_deadline(), Some(20));
        let _ = fire_to(&mut w, 20);
        // 500 is in level 1 (slot 7); next work is its slot boundary 448.
        assert_eq!(w.next_deadline(), Some(448));
    }

    #[test]
    fn advance_past_empty_time_is_cheap() {
        // No timers: advancing far just moves the cursor, no work, no panic.
        let mut w = Wheel::new();
        assert!(fire_to(&mut w, 1_000_000).is_empty());
        assert_eq!(w.elapsed(), 1_000_000);
    }
}

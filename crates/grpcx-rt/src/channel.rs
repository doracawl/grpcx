//! Channels (v0.4) — bounded `mpsc` + `oneshot`, fixed-size ring.
//!
//! Both are backed by a const-capacity ring buffer (design intent #2):
//! a full `mpsc` rejects via [`Sender::try_send`] returning the value in
//! `Err(TrySendError::Full(_))` — no allocation, no blocking the
//! allocator, no async-send backpressure. The receive side is async.
//!
//! std-backed (`Mutex` for the shared state). A cross-thread `Sender`
//! wakes the receiver task's waker, which — being a `grpcx-rt` waker —
//! unparks the runtime's blocked `Poll::poll` via `grpcx_io::Waker`
//! (see the executor module). Same-thread sends just enqueue the task.

extern crate std;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::task::{Context, Poll, Waker};
use std::sync::Mutex;

// ============================================================
// Fixed-capacity ring buffer (no async, just storage).
// ============================================================

struct RingBuf<T> {
    slots: alloc::boxed::Box<[Option<T>]>,
    head: usize, // next pop
    tail: usize, // next push
    len: usize,
}

impl<T> RingBuf<T> {
    fn with_capacity(capacity: usize) -> Self {
        let cap = capacity.max(1);
        let slots: Vec<Option<T>> = (0..cap).map(|_| None).collect();
        Self {
            slots: slots.into_boxed_slice(),
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    fn cap(&self) -> usize {
        self.slots.len()
    }

    fn is_full(&self) -> bool {
        self.len == self.cap()
    }

    /// Push to the back; returns the value back if full.
    fn push(&mut self, value: T) -> Result<(), T> {
        if self.is_full() {
            return Err(value);
        }
        self.slots[self.tail] = Some(value);
        self.tail = (self.tail + 1) % self.cap();
        self.len += 1;
        Ok(())
    }

    /// Pop from the front; `None` when empty.
    fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let value = self.slots[self.head].take();
        self.head = (self.head + 1) % self.cap();
        self.len -= 1;
        value
    }
}

// ============================================================
// mpsc — multi-producer, single-consumer, bounded.
// ============================================================

/// Error from [`Sender::try_send`].
#[derive(Debug)]
pub enum TrySendError<T> {
    /// The ring buffer is full; the value is returned.
    Full(T),
    /// The receiver has been dropped; the value is returned.
    Closed(T),
}

/// Error from [`Receiver::try_recv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRecvError {
    /// No value is currently buffered.
    Empty,
    /// The channel is empty and every sender has been dropped.
    Disconnected,
}

struct Chan<T> {
    buf: Mutex<RingBuf<T>>,
    recv_waker: Mutex<Option<Waker>>,
    senders: AtomicUsize,
    receiver_alive: AtomicBool,
}

impl<T> Chan<T> {
    fn try_recv(&self) -> Result<T, TryRecvError> {
        if let Some(value) = self.buf.lock().unwrap().pop() {
            return Ok(value);
        }
        if self.senders.load(Ordering::Acquire) == 0 {
            // Race-close: between our first `pop` (drop the lock) and
            // this `senders.load`, a still-live sender could have run
            // `try_send` (push under lock) and then `Drop` (the
            // `fetch_sub(AcqRel)` that took `senders` to 0). Our
            // `Acquire` load synchronizes with that last `fetch_sub`,
            // which itself synchronized with every prior sender's
            // `fetch_sub`, which on each sender happened-after that
            // sender's `try_send`. A second `lock().pop()` here picks
            // up any value that landed in that narrow window; without
            // it the receiver would return `Disconnected` while a
            // pushed value sat in the ring forever (the v0.6 soak
            // caught this — `REFACTOR-v2-v0.6-coverage-fuzz-mem.md`).
            if let Some(value) = self.buf.lock().unwrap().pop() {
                return Ok(value);
            }
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }

    fn wake_receiver(&self) {
        if let Some(waker) = self.recv_waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

/// Create a bounded `mpsc` channel with a fixed-size ring buffer of
/// `capacity` slots (minimum 1).
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let chan = Arc::new(Chan {
        buf: Mutex::new(RingBuf::with_capacity(capacity)),
        recv_waker: Mutex::new(None),
        senders: AtomicUsize::new(1),
        receiver_alive: AtomicBool::new(true),
    });
    (
        Sender {
            chan: Arc::clone(&chan),
        },
        Receiver { chan },
    )
}

/// The sending half of a fixed-size ring-buffer channel.
pub struct Sender<T> {
    chan: Arc<Chan<T>>,
}

impl<T> Sender<T> {
    /// Attempt to send `value` without blocking.
    ///
    /// On a full ring buffer or a dropped receiver, `value` is handed
    /// back in `Err` — no allocation, no blocking (design intent #2).
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        if !self.chan.receiver_alive.load(Ordering::Acquire) {
            return Err(TrySendError::Closed(value));
        }
        if let Err(value) = self.chan.buf.lock().unwrap().push(value) {
            return Err(TrySendError::Full(value));
        }
        self.chan.wake_receiver();
        Ok(())
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.chan.senders.fetch_add(1, Ordering::Relaxed);
        Self {
            chan: Arc::clone(&self.chan),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if self.chan.senders.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Last sender gone — wake the receiver so a parked `recv`
            // observes the disconnect.
            self.chan.wake_receiver();
        }
    }
}

/// The receiving half of a fixed-size ring-buffer channel. Single
/// consumer (not `Clone`).
pub struct Receiver<T> {
    chan: Arc<Chan<T>>,
}

impl<T> Receiver<T> {
    /// Attempt to receive without blocking.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.chan.try_recv()
    }

    /// Receive the next value, awaiting one if the buffer is empty.
    /// Resolves to `None` once the buffer is empty and all senders are
    /// gone.
    pub fn recv(&self) -> Recv<T> {
        Recv {
            chan: Arc::clone(&self.chan),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.chan.receiver_alive.store(false, Ordering::Release);
    }
}

/// Future returned by [`Receiver::recv`].
pub struct Recv<T> {
    chan: Arc<Chan<T>>,
}

impl<T> Future for Recv<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        // Cooperative budget: a task that keeps receiving ready values
        // would otherwise monopolize its worker. When the per-run budget
        // is spent, yield once so the worker can service its peers. A
        // no-op outside a multi-thread runtime (budget unset).
        if crate::runtime::budget_exhausted() {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        let chan = &self.chan;
        match chan.try_recv() {
            Ok(value) => return Poll::Ready(Some(value)),
            Err(TryRecvError::Disconnected) => return Poll::Ready(None),
            Err(TryRecvError::Empty) => {}
        }
        // Register, then re-check: a sender may have pushed (and tried
        // to wake an absent waker) between the check above and now.
        *chan.recv_waker.lock().unwrap() = Some(cx.waker().clone());
        match chan.try_recv() {
            Ok(value) => Poll::Ready(Some(value)),
            Err(TryRecvError::Disconnected) => Poll::Ready(None),
            Err(TryRecvError::Empty) => Poll::Pending,
        }
    }
}

// ============================================================
// oneshot — single value, single producer + single consumer.
// ============================================================

/// One-shot single-value channel: a producer sends at most one value,
/// a consumer awaits it.
pub mod oneshot {
    use super::{Arc, Context, Future, Mutex, Pin, Poll, Waker};

    /// The receiver was awaited but the sender dropped without sending.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Canceled;

    enum State<T> {
        Empty(Option<Waker>),
        Sent(T),
        SenderGone,
        Taken,
    }

    struct Shared<T> {
        state: Mutex<State<T>>,
        recv_alive: core::sync::atomic::AtomicBool,
    }

    /// Create a one-shot channel.
    pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::Empty(None)),
            recv_alive: core::sync::atomic::AtomicBool::new(true),
        });
        (
            Sender {
                shared: Arc::clone(&shared),
                sent: false,
            },
            Receiver { shared },
        )
    }

    /// The sending half. Consumed by [`send`](Self::send).
    pub struct Sender<T> {
        shared: Arc<Shared<T>>,
        sent: bool,
    }

    impl<T> Sender<T> {
        /// Send `value`. Returns `Err(value)` if the receiver is gone.
        pub fn send(mut self, value: T) -> Result<(), T> {
            self.sent = true; // suppress the drop-time SenderGone marker
            if !self
                .shared
                .recv_alive
                .load(core::sync::atomic::Ordering::Acquire)
            {
                return Err(value);
            }
            let mut state = self.shared.state.lock().unwrap();
            match &mut *state {
                State::Empty(waker) => {
                    let waker = waker.take();
                    *state = State::Sent(value);
                    drop(state);
                    if let Some(waker) = waker {
                        waker.wake();
                    }
                    Ok(())
                }
                _ => Err(value),
            }
        }
    }

    impl<T> Drop for Sender<T> {
        fn drop(&mut self) {
            if self.sent {
                return;
            }
            let mut state = self.shared.state.lock().unwrap();
            if let State::Empty(waker) = &mut *state {
                let waker = waker.take();
                *state = State::SenderGone;
                drop(state);
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
        }
    }

    /// The receiving half. Awaiting it yields the sent value, or
    /// [`Canceled`] if the sender dropped first.
    pub struct Receiver<T> {
        shared: Arc<Shared<T>>,
    }

    impl<T> Future for Receiver<T> {
        type Output = Result<T, Canceled>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<T, Canceled>> {
            let mut state = self.shared.state.lock().unwrap();
            match core::mem::replace(&mut *state, State::Taken) {
                State::Sent(value) => Poll::Ready(Ok(value)),
                State::SenderGone => Poll::Ready(Err(Canceled)),
                State::Empty(_) => {
                    *state = State::Empty(Some(cx.waker().clone()));
                    Poll::Pending
                }
                State::Taken => panic!("grpcx-rt: oneshot Receiver polled after completion"),
            }
        }
    }

    impl<T> Drop for Receiver<T> {
        fn drop(&mut self) {
            self.shared
                .recv_alive
                .store(false, core::sync::atomic::Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::{TryRecvError, TrySendError, channel, oneshot};
    use crate::{block_on, spawn};
    use std::vec::Vec;

    #[test]
    fn mpsc_same_thread_fifo() {
        let got = block_on(async {
            let (tx, rx) = channel::<i32>(4);
            spawn(async move {
                for i in 0..3 {
                    tx.try_send(i).expect("send");
                }
            });
            let mut got = Vec::new();
            for _ in 0..3 {
                got.push(rx.recv().await.expect("value"));
            }
            got
        });
        assert_eq!(got, [0, 1, 2]);
    }

    #[test]
    fn mpsc_full_returns_value() {
        let (tx, _rx) = channel::<i32>(2);
        tx.try_send(1).expect("1");
        tx.try_send(2).expect("2");
        match tx.try_send(3) {
            Err(TrySendError::Full(3)) => {}
            other => panic!("expected Full(3), got {other:?}"),
        }
    }

    #[test]
    fn mpsc_try_recv_states() {
        let (tx, rx) = channel::<i32>(2);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        tx.try_send(5).expect("send");
        assert_eq!(rx.try_recv(), Ok(5));
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        drop(tx);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[test]
    fn mpsc_closed_recv_yields_none() {
        let out = block_on(async {
            let (tx, rx) = channel::<i32>(2);
            drop(tx);
            rx.recv().await
        });
        assert_eq!(out, None);
    }

    #[test]
    fn mpsc_send_to_full_then_closed() {
        let (tx, rx) = channel::<i32>(1);
        tx.try_send(1).expect("send");
        drop(rx);
        match tx.try_send(2) {
            Err(TrySendError::Closed(2)) => {}
            other => panic!("expected Closed(2), got {other:?}"),
        }
    }

    #[test]
    fn mpsc_cross_thread_wake() {
        // Sender on another thread; the receiver task parks inside
        // block_on's blocked poll and must be unparked via grpcx_io::Waker.
        let got = block_on(async {
            let (tx, rx) = channel::<i32>(8);
            let producer = std::thread::spawn(move || {
                for i in 0..5 {
                    while tx.try_send(i).is_err() {
                        std::thread::yield_now();
                    }
                }
            });
            let mut got = Vec::new();
            for _ in 0..5 {
                got.push(rx.recv().await.expect("value"));
            }
            producer.join().expect("join");
            got
        });
        assert_eq!(got, [0, 1, 2, 3, 4]);
    }

    /// Regression — the race that the v0.6 24h soak first surfaced
    /// (`REFACTOR-v2-v0.6-coverage-fuzz-mem.md` § T3): the receiver's
    /// `try_recv` would observe `senders == 0` between its own `pop`
    /// unlock and the senders-load, leaving any value pushed in that
    /// window stranded in the ring forever and returning `Disconnected`
    /// to the caller. The fix is the second `pop` after observing
    /// `senders == 0` in `Chan::try_recv`. This test reproduces the
    /// shape (many rounds of N senders each pushing one value then
    /// dropping near-simultaneously, receiver hot-loop on `try_recv`)
    /// in a tight loop. Without the fix, 10k rounds × 4 senders
    /// reliably lost values on lx64; with the fix, zero loss.
    #[test]
    fn mpsc_no_disconnected_with_pending_value_race() {
        use std::sync::Arc;
        use std::sync::Barrier;

        const ROUNDS: usize = 10_000;
        const SENDERS: usize = 4;

        let mut lost = 0u64;
        for round in 0..ROUNDS {
            let (tx, rx) = channel::<u64>(SENDERS);
            let barrier = Arc::new(Barrier::new(SENDERS + 1));
            let mut hs = Vec::with_capacity(SENDERS);
            for s in 0..SENDERS {
                let tx = tx.clone();
                let barrier = Arc::clone(&barrier);
                let base = (round * SENDERS + s) as u64;
                hs.push(std::thread::spawn(move || {
                    barrier.wait();
                    // Tight push-then-drop: the bug only fires when the
                    // last sender's `Drop::fetch_sub` lands between the
                    // receiver's `pop`-unlock and its `senders.load`.
                    tx.try_send(base).expect("try_send");
                    drop(tx);
                }));
            }
            drop(tx); // the original; only the N clones remain
            barrier.wait();

            // Receiver hot-loop: drain until Disconnected, count values.
            let mut got = 0u64;
            loop {
                match rx.try_recv() {
                    Ok(_) => got += 1,
                    Err(TryRecvError::Empty) => std::thread::yield_now(),
                    Err(TryRecvError::Disconnected) => break,
                }
            }
            for h in hs {
                h.join().expect("join");
            }
            if got < SENDERS as u64 {
                lost += SENDERS as u64 - got;
            }
        }
        assert_eq!(
            lost, 0,
            "mpsc lost {lost} values across {ROUNDS} rounds × {SENDERS} senders"
        );
    }

    #[test]
    fn oneshot_send_then_recv() {
        let out = block_on(async {
            let (tx, rx) = oneshot::channel::<i32>();
            tx.send(42).expect("send");
            rx.await
        });
        assert_eq!(out, Ok(42));
    }

    #[test]
    fn oneshot_sender_dropped_is_canceled() {
        let out = block_on(async {
            let (tx, rx) = oneshot::channel::<i32>();
            drop(tx);
            rx.await
        });
        assert_eq!(out, Err(oneshot::Canceled));
    }

    #[test]
    fn oneshot_cross_thread() {
        let out = block_on(async {
            let (tx, rx) = oneshot::channel::<i32>();
            std::thread::spawn(move || {
                std::thread::sleep(core::time::Duration::from_millis(15));
                let _ = tx.send(7);
            });
            rx.await
        });
        assert_eq!(out, Ok(7));
    }
}

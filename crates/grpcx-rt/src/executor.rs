//! Single-thread `current_thread` executor + [`block_on`] + reactor
//! (`Waker` ↔ `grpcx-io::Poll`) integration + cross-thread unpark.
//!
//! # std posture
//!
//! This module is **std-backed** (`Mutex` for a sound cross-thread
//! waker, `thread_local!` for the current-runtime handle). The
//! `no_std` aspiration (design intent #5) holds for the channel /
//! timer data-structure cores, not for the executor itself.
//!
//! # Wakeups (v0.4)
//!
//! Same-thread wakes (the reactor delivering an event, or one task
//! waking another / the root during a poll) just enqueue work — the
//! driver is not parked. A **cross-thread** wake (a channel `Sender` on
//! another thread; later the multi-thread executor) additionally fires
//! the runtime's `grpcx_io::Waker` unparker (registered at
//! `UNPARK_TOKEN`) to interrupt a blocked `Poll::poll`. The wake site
//! distinguishes the two by comparing the current thread to the runtime
//! thread; a spurious cross-thread unpark while not parked is harmless
//! (drained on the next `reset`).
//!
//! # Reactor integration model — one-shot
//!
//! `grpcx-io`'s backends differ in trigger semantics: Linux `epoll` is
//! level-triggered (no `EPOLLET`), while `kqueue` uses `EV_CLEAR`
//! (edge-triggered). To present one behavior across both, the runtime
//! registers each readiness interest **one-shot**: when the reactor
//! reports an event, the runtime wakes the waiter *and* deregisters the
//! fd. A future that still needs readiness re-registers on its next
//! poll. This avoids epoll's level-triggered re-fire spin without
//! relying on kqueue-only edge semantics.

extern crate std;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use core::time::Duration;
use std::sync::Mutex;
use std::thread::ThreadId;
use std::time::Instant;

use grpcx_io::{
    Events, Interest, IoResult, Poll as Reactor, RawFd, Registry, SourceFd, Token, Waker as IoWaker,
};

use crate::timer::{Entry as TimerEntry, Wheel};

/// Tick granularity of the timer wheel: 1 tick = 1 millisecond.
const TICK_MS: u64 = 1;

/// Reserved reactor token for the cross-thread unparker (`grpcx_io::Waker`).
/// Out of the io-waiter allocator's range, which counts up from 0.
const UNPARK_TOKEN: usize = usize::MAX;

// ============================================================
// Waker — hand-rolled RawWakerVTable (design intent #7).
//
// The waker's data is an `Arc<WakerData>` holding the wake target
// (root flag or a slab task id) plus the shared scheduler. `Arc` +
// the `Send + Sync` `Sched` make the waker sound to clone / send,
// satisfying the `Waker` contract even though v0.2 only ever wakes
// on the runtime thread.
// ============================================================

#[derive(Clone, Copy)]
enum WakeTarget {
    Root,
    Task(usize),
}

struct WakerData {
    target: WakeTarget,
    sched: Arc<Sched>,
}

impl WakerData {
    fn wake(&self) {
        match self.target {
            WakeTarget::Root => self.sched.root_woken.store(true, Ordering::Release),
            WakeTarget::Task(id) => self.sched.run.lock().unwrap().push_back(id),
        }
        // A wake from another thread must interrupt a blocked
        // `Poll::poll`; a same-thread wake (fired while the driver is
        // polling a task on the runtime thread) need not — the driver
        // is not parked and will see the enqueued work directly. A
        // spurious unpark (cross-thread wake while not parked) is
        // harmless: the next `reset` drains it.
        if std::thread::current().id() != self.sched.runtime_thread {
            let _ = self.sched.unparker.wake();
        }
    }
}

static VTABLE: RawWakerVTable = RawWakerVTable::new(vt_clone, vt_wake, vt_wake_by_ref, vt_drop);

fn make_waker(target: WakeTarget, sched: Arc<Sched>) -> Waker {
    let data = Arc::new(WakerData { target, sched });
    let ptr = Arc::into_raw(data).cast::<()>();
    // SAFETY: `ptr` is a freshly `Arc::into_raw`'d `WakerData` and
    // `VTABLE`'s functions reconstitute it with the matching type.
    unsafe { Waker::from_raw(RawWaker::new(ptr, &VTABLE)) }
}

unsafe fn vt_clone(ptr: *const ()) -> RawWaker {
    // SAFETY: `ptr` was produced by `Arc::into_raw::<WakerData>`.
    let arc = unsafe { Arc::from_raw(ptr.cast::<WakerData>()) };
    let cloned = Arc::clone(&arc);
    core::mem::forget(arc); // keep the original refcount intact
    RawWaker::new(Arc::into_raw(cloned).cast::<()>(), &VTABLE)
}

unsafe fn vt_wake(ptr: *const ()) {
    // SAFETY: `ptr` was produced by `Arc::into_raw::<WakerData>`;
    // consume it (wake-by-value drops one refcount).
    let arc = unsafe { Arc::from_raw(ptr.cast::<WakerData>()) };
    arc.wake();
}

unsafe fn vt_wake_by_ref(ptr: *const ()) {
    // SAFETY: `ptr` was produced by `Arc::into_raw::<WakerData>`;
    // do not drop the refcount (wake-by-ref).
    let arc = unsafe { Arc::from_raw(ptr.cast::<WakerData>()) };
    arc.wake();
    core::mem::forget(arc);
}

unsafe fn vt_drop(ptr: *const ()) {
    // SAFETY: `ptr` was produced by `Arc::into_raw::<WakerData>`.
    drop(unsafe { Arc::from_raw(ptr.cast::<WakerData>()) });
}

// ============================================================
// Scheduler state — shared with wakers (hence `Arc` + interior sync).
// ============================================================

struct Sched {
    /// Task ids woken and awaiting a poll.
    run: Mutex<VecDeque<usize>>,
    /// Set when the root future's waker fires.
    root_woken: AtomicBool,
    /// Cross-thread unparker — interrupts a blocked `Poll::poll`.
    unparker: IoWaker,
    /// The thread `block_on` runs on; wakes from other threads unpark.
    runtime_thread: ThreadId,
}

impl Sched {
    fn new(unparker: IoWaker, runtime_thread: ThreadId) -> Self {
        Self {
            run: Mutex::new(VecDeque::new()),
            root_woken: AtomicBool::new(false),
            unparker,
            runtime_thread,
        }
    }
}

// ============================================================
// Task slab — `current_thread` tasks live here and are reused.
// ============================================================

type BoxedTask = Pin<Box<dyn Future<Output = ()>>>;

enum Slot {
    Vacant,
    Occupied(BoxedTask),
    /// Taken out for polling; restored or removed afterwards.
    Polling,
}

struct Slab {
    slots: Vec<Slot>,
    free: Vec<usize>,
}

impl Slab {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    fn insert(&mut self, task: BoxedTask) -> usize {
        if let Some(id) = self.free.pop() {
            self.slots[id] = Slot::Occupied(task);
            id
        } else {
            self.slots.push(Slot::Occupied(task));
            self.slots.len() - 1
        }
    }

    fn take(&mut self, id: usize) -> Option<BoxedTask> {
        let slot = self.slots.get_mut(id)?;
        if matches!(slot, Slot::Occupied(_)) {
            match core::mem::replace(slot, Slot::Polling) {
                Slot::Occupied(task) => Some(task),
                _ => None,
            }
        } else {
            None
        }
    }

    fn restore(&mut self, id: usize, task: BoxedTask) {
        self.slots[id] = Slot::Occupied(task);
    }

    fn remove(&mut self, id: usize) {
        self.slots[id] = Slot::Vacant;
        self.free.push(id);
    }
}

// ============================================================
// IO waiters — token → (fd, waker) for one-shot readiness.
// ============================================================

struct IoEntry {
    fd: RawFd,
    waker: Waker,
}

struct IoWaiters {
    next: usize,
    map: BTreeMap<usize, IoEntry>,
}

impl IoWaiters {
    fn new() -> Self {
        Self {
            next: 0,
            map: BTreeMap::new(),
        }
    }

    fn alloc(&mut self) -> usize {
        let token = self.next;
        self.next = self.next.wrapping_add(1);
        token
    }
}

// ============================================================
// Runtime inner — one per `block_on`, reachable via `CURRENT`.
// ============================================================

struct Inner {
    reactor: RefCell<Reactor>,
    registry: Registry,
    tasks: RefCell<Slab>,
    io: RefCell<IoWaiters>,
    timer: RefCell<Wheel>,
    sched: Arc<Sched>,
    start: Instant,
}

impl Inner {
    fn new() -> IoResult<Self> {
        let reactor = Reactor::new()?;
        let registry = reactor.registry().clone();
        let unparker = IoWaker::new(&registry, Token(UNPARK_TOKEN))?;
        let sched = Arc::new(Sched::new(unparker, std::thread::current().id()));
        Ok(Self {
            reactor: RefCell::new(reactor),
            registry,
            tasks: RefCell::new(Slab::new()),
            io: RefCell::new(IoWaiters::new()),
            timer: RefCell::new(Wheel::new()),
            sched,
            start: Instant::now(),
        })
    }

    /// Monotonic ticks elapsed since the runtime started (1 tick = 1 ms).
    fn now_tick(&self) -> u64 {
        (self.start.elapsed().as_millis() as u64) / TICK_MS
    }
}

std::thread_local! {
    static CURRENT: RefCell<Option<Rc<Inner>>> = const { RefCell::new(None) };
}

struct CurrentGuard;

impl Drop for CurrentGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = None);
    }
}

fn set_current(inner: Rc<Inner>) -> CurrentGuard {
    CURRENT.with(|c| {
        let mut slot = c.borrow_mut();
        assert!(
            slot.is_none(),
            "grpcx-rt: nested block_on is not supported in v0.2"
        );
        *slot = Some(inner);
    });
    CurrentGuard
}

fn current() -> Rc<Inner> {
    CURRENT.with(|c| {
        c.borrow()
            .as_ref()
            .expect("grpcx-rt: spawn / readable called outside block_on")
            .clone()
    })
}

// ============================================================
// block_on — the single-thread driver.
// ============================================================

/// Run `future` to completion on the current thread, returning its
/// output.
///
/// Driven by `grpcx-io`'s `Poll` (design intent #8): when neither the
/// root future nor any spawned task can make progress, the driver
/// blocks in `Poll::poll` until the reactor reports IO readiness — no
/// busy-loop, no condvar.
///
/// # Panics
///
/// Panics if called re-entrantly (nested `block_on`), if the reactor
/// fails to initialize, or if the root future is left pending with no
/// runnable task and no pending IO (an unsatisfiable wait — in v0.2
/// this also covers an attempted cross-thread wakeup; see the module
/// docs).
pub fn block_on<F: Future>(future: F) -> F::Output {
    let inner = Rc::new(Inner::new().expect("grpcx-rt: reactor init failed"));
    let _guard = set_current(Rc::clone(&inner));

    let mut fut = Box::pin(future);
    let root_waker = make_waker(WakeTarget::Root, Arc::clone(&inner.sched));
    inner.sched.root_woken.store(true, Ordering::Release);
    let mut events = Events::with_capacity(256);

    loop {
        if inner.sched.root_woken.swap(false, Ordering::AcqRel) {
            let mut cx = Context::from_waker(&root_waker);
            if let Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
                return value;
            }
        }

        // Drain woken spawned tasks.
        loop {
            let next = inner.sched.run.lock().unwrap().pop_front();
            let Some(id) = next else { break };
            poll_task(&inner, id);
        }

        // Fire any due timers, waking their waiters.
        fire_due_timers(&inner);

        // The root or a task may have been woken (by the task drain
        // above or a timer that just fired).
        if inner.sched.root_woken.load(Ordering::Acquire)
            || !inner.sched.run.lock().unwrap().is_empty()
        {
            continue;
        }

        // Nothing runnable. Block on the reactor until IO readiness, a
        // cross-thread unpark, or the next timer deadline. The unparker
        // is always registered, so `poll` always has something to wait
        // on; a future awaiting a cross-thread wake (e.g. a channel send
        // from another thread) legitimately blocks here until it comes —
        // like tokio, the runtime can block indefinitely on such a wait.
        let now = inner.now_tick();
        let next_timer = inner.timer.borrow().next_deadline();
        let timeout = next_timer
            .map(|deadline| Duration::from_millis(deadline.saturating_sub(now) * TICK_MS));

        inner
            .reactor
            .borrow_mut()
            .poll(&mut events, timeout)
            .expect("grpcx-rt: reactor poll failed");

        // Collect fired tokens first to avoid overlapping borrows.
        let mut fired: Vec<usize> = Vec::new();
        for event in &events {
            fired.push(event.token().0);
        }
        for token in fired {
            if token == UNPARK_TOKEN {
                // Cross-thread unpark — drain and re-arm; the woken work
                // is already on the run queue / root flag.
                let _ = inner.sched.unparker.reset();
                continue;
            }
            let entry = inner.io.borrow_mut().map.remove(&token);
            if let Some(entry) = entry {
                // One-shot: deregister so a level-triggered backend does
                // not re-fire before the waiter re-registers.
                let mut source = SourceFd::new(entry.fd);
                let _ = inner.registry.deregister(&mut source);
                entry.waker.wake();
            }
        }
    }
}

fn poll_task(inner: &Rc<Inner>, id: usize) {
    let Some(mut task) = inner.tasks.borrow_mut().take(id) else {
        return; // already completed or not a live task
    };
    let waker = make_waker(WakeTarget::Task(id), Arc::clone(&inner.sched));
    let mut cx = Context::from_waker(&waker);
    match task.as_mut().poll(&mut cx) {
        Poll::Ready(()) => inner.tasks.borrow_mut().remove(id),
        Poll::Pending => inner.tasks.borrow_mut().restore(id, task),
    }
}

/// Advance the timer wheel to the current tick, marking each fired
/// timer done and waking its waiter.
fn fire_due_timers(inner: &Rc<Inner>) {
    let now = inner.now_tick();
    let mut fired: Vec<TimerEntry> = Vec::new();
    inner.timer.borrow_mut().advance(now, &mut fired);
    for entry in fired {
        entry.done.store(true, Ordering::Release);
        entry.waker.wake();
    }
}

// ============================================================
// spawn + Spawn + JoinHandle.
// ============================================================

/// Spawns futures onto an executor.
///
/// The single seam shared by the single-thread (`current_thread`,
/// v0.2) and the future multi-thread work-stealing (`multi_thread`,
/// v0.5) executors.
pub trait Spawn {
    /// Spawn `future` onto this executor, returning a handle to its
    /// eventual output.
    fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static;
}

/// A cheap handle to the current-thread runtime, usable as a [`Spawn`]
/// implementor. Obtain one inside a [`block_on`] context.
pub struct Handle;

impl Spawn for Handle {
    fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        spawn(future)
    }
}

/// Spawn `future` onto the current-thread runtime, returning a
/// [`JoinHandle`] for its output.
///
/// # Panics
///
/// Panics if called outside a [`block_on`] context.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let join = Arc::new(JoinShared::new());
    let join_for_task = Arc::clone(&join);
    let inner = current();

    let id = inner.tasks.borrow_mut().insert(Box::pin(async move {
        let output = future.await;
        join_for_task.complete(output);
    }));
    // Schedule the task's first poll.
    inner.sched.run.lock().unwrap().push_back(id);

    JoinHandle { join }
}

enum JoinState<T> {
    Empty(Option<Waker>),
    Ready(T),
    Done,
}

pub(crate) struct JoinShared<T> {
    state: Mutex<JoinState<T>>,
}

impl<T> JoinShared<T> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(JoinState::Empty(None)),
        }
    }

    pub(crate) fn complete(&self, value: T) {
        let mut state = self.state.lock().unwrap();
        let waker = match &mut *state {
            JoinState::Empty(slot) => slot.take(),
            _ => None,
        };
        *state = JoinState::Ready(value);
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn poll(&self, cx: &Context<'_>) -> Poll<T> {
        let mut state = self.state.lock().unwrap();
        match core::mem::replace(&mut *state, JoinState::Done) {
            JoinState::Ready(value) => Poll::Ready(value),
            JoinState::Empty(_) => {
                *state = JoinState::Empty(Some(cx.waker().clone()));
                Poll::Pending
            }
            JoinState::Done => panic!("grpcx-rt: JoinHandle polled after completion"),
        }
    }
}

/// A handle to a spawned task. Awaiting it yields the task's output.
pub struct JoinHandle<T> {
    join: Arc<JoinShared<T>>,
}

impl<T> JoinHandle<T> {
    /// Wrap a shared join slot in a handle. Used by both executors'
    /// `spawn` to hand the caller a future for the task's output.
    pub(crate) fn new(join: Arc<JoinShared<T>>) -> Self {
        Self { join }
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        self.join.poll(cx)
    }
}

// ============================================================
// Reactor readiness primitive (v0.2 minimal IO surface).
//
// The general AsyncRead / AsyncWrite traits land in v0.6; v0.2 exposes
// just enough to await fd readiness and exercise the Waker ↔ Poll loop.
// ============================================================

/// Return a future that completes when `fd` becomes readable.
///
/// Registers `fd` with the current runtime's reactor one-shot (see
/// module docs). The fd must stay valid until the future completes or
/// is dropped.
///
/// # Panics
///
/// The returned future panics on first poll if awaited outside a
/// [`block_on`] context.
pub fn readable(fd: RawFd) -> Readable {
    Readable {
        fd,
        token: None,
        mt: false,
    }
}

/// Future returned by [`readable`]. Resolves once the fd is readable.
pub struct Readable {
    fd: RawFd,
    token: Option<usize>,
    /// True once registered on a multi-thread runtime (vs current-thread).
    mt: bool,
}

impl Future for Readable {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();

        if let Some(token) = this.token {
            // Already registered. Still mapped → not fired yet (refresh
            // the waker and stay pending); gone → the runtime fired and
            // deregistered it one-shot → ready. Route to whichever
            // runtime registered it.
            if this.mt {
                return match crate::runtime::refresh_readable_mt(token, cx.waker().clone()) {
                    Some(true) => Poll::Pending,
                    _ => {
                        this.token = None;
                        Poll::Ready(())
                    }
                };
            }
            let inner = current();
            let mut io = inner.io.borrow_mut();
            if let Some(entry) = io.map.get_mut(&token) {
                entry.waker = cx.waker().clone();
                return Poll::Pending;
            }
            this.token = None;
            return Poll::Ready(());
        }

        // First poll: register the fd. Prefer a multi-thread runtime's
        // shared reactor; otherwise the current-thread runtime.
        if let Some(token) = crate::runtime::register_readable_mt(this.fd, cx.waker().clone()) {
            this.mt = true;
            this.token = Some(token);
            return Poll::Pending;
        }

        let inner = current();
        let token = inner.io.borrow_mut().alloc();
        let mut source = SourceFd::new(this.fd);
        inner
            .registry
            .register(&mut source, Token(token), Interest::READABLE)
            .expect("grpcx-rt: reactor register failed");
        inner.io.borrow_mut().map.insert(
            token,
            IoEntry {
                fd: this.fd,
                waker: cx.waker().clone(),
            },
        );
        this.token = Some(token);
        Poll::Pending
    }
}

impl Drop for Readable {
    fn drop(&mut self) {
        let Some(token) = self.token else { return };
        if self.mt {
            crate::runtime::deregister_readable_mt(token, self.fd);
            return;
        }
        // Deregister if still pending. Ignore failures during teardown
        // and any case where the runtime is already gone.
        let _ = CURRENT.try_with(|c| {
            if let Some(inner) = c.borrow().as_ref() {
                let inner = Rc::clone(inner);
                if inner.io.borrow_mut().map.remove(&token).is_some() {
                    let mut source = SourceFd::new(self.fd);
                    let _ = inner.registry.deregister(&mut source);
                }
            }
        });
    }
}

// ============================================================
// Time — Sleep / Timer / Timeout / Interval (v0.3).
//
// Granularity is 1 ms (`TICK_MS`), built on the hierarchical wheel in
// `crate::timer`. A `Sleep` computes its absolute deadline lazily on
// first poll (anchoring to poll time) and shares an `Arc<AtomicBool>`
// with its wheel entry; the driver sets the flag on fire.
// ============================================================

/// Convert a duration to wheel ticks, rounding **up** so a sleep never
/// wakes early.
fn duration_to_ticks(dur: Duration) -> u64 {
    let tick_nanos = u128::from(TICK_MS) * 1_000_000;
    let ticks = dur.as_nanos().div_ceil(tick_nanos);
    u64::try_from(ticks).unwrap_or(u64::MAX)
}

enum SleepState {
    Init(Duration),
    Pending(Arc<AtomicBool>),
    Done,
}

/// A future that completes after a duration elapses (1 ms
/// granularity). Construct via [`sleep`] or [`Timer::after`].
pub struct Sleep {
    state: SleepState,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        match &this.state {
            SleepState::Done => Poll::Ready(()),
            SleepState::Pending(done) => {
                if done.load(Ordering::Acquire) {
                    this.state = SleepState::Done;
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }
            SleepState::Init(dur) => {
                let done = Arc::new(AtomicBool::new(false));
                let ticks = duration_to_ticks(*dur);
                // Dispatch: on a multi-thread runtime worker the timer
                // lives in the shared `Wheel` behind the driver thread;
                // otherwise fall back to the current-thread runtime.
                if !crate::runtime::register_sleep_mt(ticks, cx.waker().clone(), Arc::clone(&done))
                {
                    let inner = current();
                    let deadline = inner.now_tick().saturating_add(ticks);
                    inner.timer.borrow_mut().insert(TimerEntry {
                        when: deadline,
                        waker: cx.waker().clone(),
                        done: Arc::clone(&done),
                    });
                }
                this.state = SleepState::Pending(done);
                Poll::Pending
            }
        }
    }
}

/// Return a [`Sleep`] future that completes after `duration`.
///
/// # Panics
///
/// The returned future panics on first poll if awaited outside a
/// [`block_on`] context.
pub fn sleep(duration: Duration) -> Sleep {
    Sleep {
        state: SleepState::Init(duration),
    }
}

/// Handle to the timer wheel bound to the current runtime. Hands out
/// time-based futures.
pub struct Timer;

impl Timer {
    /// Return a [`Sleep`] that fires `duration` from when it is first
    /// polled.
    pub fn after(&self, duration: Duration) -> Sleep {
        sleep(duration)
    }
}

/// Error returned by [`timeout`] when the deadline elapses before the
/// wrapped future completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl core::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("future timed out")
    }
}

impl core::error::Error for Elapsed {}

/// Future returned by [`timeout`]: resolves with the wrapped future's
/// output, or [`Elapsed`] if `duration` passes first.
pub struct Timeout<F> {
    future: F,
    sleep: Sleep,
}

/// Wrap `future` with a timeout. Resolves to `Ok` if it completes
/// within `duration`, else `Err(`[`Elapsed`]`)`.
pub fn timeout<F: Future>(duration: Duration, future: F) -> Timeout<F> {
    Timeout {
        future,
        sleep: sleep(duration),
    }
}

impl<F: Future> Future for Timeout<F> {
    type Output = Result<F::Output, Elapsed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: structural pinning — `future` and `sleep` are never
        // moved out of `self`; each is re-pinned in place below.
        let this = unsafe { self.get_unchecked_mut() };
        // SAFETY: `this.future` is pinned because `self` is.
        let fut = unsafe { Pin::new_unchecked(&mut this.future) };
        if let Poll::Ready(value) = fut.poll(cx) {
            return Poll::Ready(Ok(value));
        }
        // `Sleep` is `Unpin`, so a safe re-pin suffices.
        match Pin::new(&mut this.sleep).poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(Elapsed)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A periodic timer yielding a tick every `period`.
///
/// Ticks are **relative**: each [`tick`](Self::tick) sleeps `period`
/// from when it is awaited, so a slow consumer shifts subsequent ticks
/// later. Absolute-deadline anchoring is a deferred refinement.
pub struct Interval {
    period: Duration,
    sleep: Option<Sleep>,
}

/// Create an [`Interval`] that ticks every `period`.
pub fn interval(period: Duration) -> Interval {
    Interval {
        period,
        sleep: None,
    }
}

impl Interval {
    /// Wait for the next tick.
    pub async fn tick(&mut self) {
        core::future::poll_fn(|cx| self.poll_tick(cx)).await;
    }

    fn poll_tick(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let sleep = self.sleep.get_or_insert_with(|| sleep(self.period));
        match Pin::new(sleep).poll(cx) {
            Poll::Ready(()) => {
                self.sleep = None; // arm the next tick on the following call
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::future::Future;
    use core::pin::Pin;
    use core::task::{Context, Poll};
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::time::Instant;

    /// Pending exactly once (waking itself), then Ready — exercises the
    /// waker → reschedule loop without any IO.
    struct YieldOnce(bool);

    impl Future for YieldOnce {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    #[test]
    fn block_on_ready_value() {
        assert_eq!(block_on(async { 42 }), 42);
    }

    #[test]
    fn block_on_yield_then_ready() {
        let out = block_on(async {
            YieldOnce(false).await;
            7
        });
        assert_eq!(out, 7);
    }

    #[test]
    fn spawn_and_join() {
        let sum = block_on(async {
            let a = spawn(async { 2 });
            let b = spawn(async { 3 });
            a.await + b.await
        });
        assert_eq!(sum, 5);
    }

    #[test]
    fn spawn_via_handle_trait() {
        let v = block_on(async {
            let h = Handle;
            let j = h.spawn(async {
                YieldOnce(false).await;
                99
            });
            j.await
        });
        assert_eq!(v, 99);
    }

    #[test]
    fn nested_spawn() {
        let v = block_on(async {
            let outer = spawn(async {
                let inner = spawn(async { 10 });
                inner.await + 5
            });
            outer.await
        });
        assert_eq!(v, 15);
    }

    #[test]
    fn reactor_readable_wakes_future() {
        // A pre-written byte makes the read end readable; block_on must
        // drive the reactor, observe readiness, wake the Readable
        // future, and complete. Exercises the Waker ↔ Poll loop.
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        writer.write_all(b"x").expect("write");
        let fd = reader.as_raw_fd();

        block_on(async {
            readable(fd).await;
        });

        // reader / writer kept alive across block_on; drop here.
        drop(reader);
        drop(writer);
    }

    #[test]
    fn sleep_waits_at_least_the_duration() {
        let start = Instant::now();
        block_on(async {
            sleep(Duration::from_millis(20)).await;
        });
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn multiple_sleeps_complete_in_order() {
        let order = block_on(async {
            let mut order = alloc::vec::Vec::new();
            let a = spawn(async {
                sleep(Duration::from_millis(40)).await;
                40
            });
            let b = spawn(async {
                sleep(Duration::from_millis(10)).await;
                10
            });
            // Await the shorter first by value, but record completion order
            // via the values; b (10ms) should be ready no later than a.
            order.push(b.await);
            order.push(a.await);
            order
        });
        assert_eq!(order, [10, 40]);
    }

    #[test]
    fn sleep_across_wheel_levels() {
        // 70ms > 64 ticks → lands in level 1, must cascade down to fire.
        let start = Instant::now();
        block_on(async {
            sleep(Duration::from_millis(70)).await;
        });
        assert!(start.elapsed() >= Duration::from_millis(70));
    }

    #[test]
    fn timeout_returns_ok_when_future_completes_first() {
        let result = block_on(async {
            timeout(Duration::from_millis(100), async {
                sleep(Duration::from_millis(5)).await;
                42
            })
            .await
        });
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn timeout_elapses_when_future_is_slow() {
        let result = block_on(async {
            timeout(Duration::from_millis(10), async {
                sleep(Duration::from_millis(200)).await;
                99
            })
            .await
        });
        assert_eq!(result, Err(Elapsed));
    }

    #[test]
    fn interval_ticks_repeatedly() {
        let start = Instant::now();
        block_on(async {
            let mut iv = interval(Duration::from_millis(10));
            for _ in 0..3 {
                iv.tick().await;
            }
        });
        // Three 10ms ticks ≈ ≥ 30ms (relative scheduling).
        assert!(start.elapsed() >= Duration::from_millis(30));
    }

    #[test]
    fn timer_after_sleeps() {
        let start = Instant::now();
        block_on(async {
            Timer.after(Duration::from_millis(15)).await;
        });
        assert!(start.elapsed() >= Duration::from_millis(15));
    }

    #[test]
    fn elapsed_display_and_error_impls() {
        // Display is on the cold error-print path; exercise it here so
        // `cargo llvm-cov` sees the trivial format-string body.
        let s = std::format!("{}", Elapsed);
        assert_eq!(s, "future timed out");
        // `dyn core::error::Error` upcast (line under `impl Error for
        // Elapsed`) — confirms the trait impl compiles + lives.
        let _e: &dyn core::error::Error = &Elapsed;
    }

    /// A `readable` that never fires, wrapped in a `timeout` that wins:
    /// the still-pending `Readable` is re-polled (waker refresh) then
    /// dropped, exercising the current-thread deregister-on-cancel path.
    #[test]
    fn readable_cancelled_when_dropped_pending() {
        let (reader, _writer) = std::io::pipe().expect("pipe"); // never readable
        let fd = reader.as_raw_fd();
        block_on(async move {
            let r = timeout(Duration::from_millis(20), readable(fd)).await;
            assert!(r.is_err(), "timeout should fire before fd is readable");
            drop(reader);
        });
    }
}

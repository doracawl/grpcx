//! Multi-thread work-stealing runtime (v0.5, design intent #4).
//!
//! A fixed pool of worker threads, each owning a lock-free Chase-Lev
//! [`Deque`](crate::deque::Deque) of runnable tasks, plus a global
//! `Mutex<VecDeque>` injector for overflow and for tasks scheduled from
//! off-worker threads. Workers run their local deque LIFO, steal FIFO
//! from peers when idle, then drain the injector, then park on a
//! `Condvar` until woken. A dedicated **driver thread** owns the reactor
//! and the timer wheel: it fires due timers and turns IO readiness into
//! task wake-ups, and is itself interrupted from its blocking
//! `Poll::poll` by a [`grpcx_io::Waker`] when a worker registers fresh
//! timer/IO interest.
//!
//! # std posture
//!
//! Like the `current_thread` executor, this module is **std-backed**
//! (`thread`, `Mutex`, `Condvar`, `thread_local!`). The `no_std`
//! aspiration holds for the data-structure cores it builds on — the
//! Chase-Lev deque and the timer wheel — not for the runtime driver.
//!
//! # Task lifecycle
//!
//! A [`Task`] is shared as `Arc<Task>`; its `AtomicU8` state machine
//! (`IDLE`/`QUEUED`/`RUNNING`/`RUNNING_NOTIFIED`) keeps a task from being
//! enqueued twice and folds a wake that lands mid-poll into a single
//! re-schedule after the poll returns `Pending`. The task's [`Waker`] is
//! a hand-rolled `RawWakerVTable` over `Arc<Task>` (distinct from the
//! current-thread executor's `Arc<WakerData>`): waking schedules the
//! task onto the current worker's deque (or the injector) and notifies a
//! parked peer.
//!
//! # Leaf integration
//!
//! `sleep` / `readable` (defined in the `executor` module) dispatch to
//! this runtime's shared timer/reactor via [`register_sleep_mt`] /
//! [`register_readable_mt`] when polled on a worker thread, falling back
//! to the current-thread runtime otherwise. A cooperative budget
//! ([`budget_exhausted`], [`yield_now`]) keeps a task that stays ready
//! from starving its worker's peers.

extern crate std;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use core::time::Duration;
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::Instant;

use grpcx_io::{
    Events, Interest, IoResult, Poll as Reactor, RawFd, Registry, SourceFd, Token, Waker as IoWaker,
};

use crate::deque::{Deque, Steal};
use crate::executor::{JoinHandle, JoinShared, Spawn};
use crate::timer::{Entry as TimerEntry, Wheel};

/// Tick granularity of the timer wheel: 1 tick = 1 millisecond.
const TICK_MS: u64 = 1;

/// Reserved reactor token for the driver's unparker (`grpcx_io::Waker`),
/// out of the io-waiter allocator's range (which counts up from 0).
const UNPARK_TOKEN: usize = usize::MAX;

/// Per-worker local deque capacity (rounded up to a power of two by the
/// deque). Matches tokio's local-queue order of magnitude; overflow
/// spills to the injector.
const LOCAL_CAP: usize = 256;

/// Polls a leaf future may make before the cooperative budget forces a
/// yield. Reset at the start of each task run.
const BUDGET_INIT: u32 = 128;

// Task states (`AtomicU8`).
const IDLE: u8 = 0;
const QUEUED: u8 = 1;
const RUNNING: u8 = 2;
const RUNNING_NOTIFIED: u8 = 3;

// ============================================================
// Task — an `Arc`-shared unit of work with a wake state machine.
// ============================================================

struct Task {
    future: Mutex<Option<Pin<Box<dyn Future<Output = ()> + Send>>>>,
    state: AtomicU8,
    shared: Arc<Shared>,
}

impl Task {
    fn new(shared: Arc<Shared>, future: Pin<Box<dyn Future<Output = ()> + Send>>) -> Self {
        Self {
            future: Mutex::new(Some(future)),
            state: AtomicU8::new(IDLE),
            shared,
        }
    }
}

// ============================================================
// Task waker — hand-rolled RawWakerVTable over `Arc<Task>`.
// ============================================================

static TASK_VTABLE: RawWakerVTable =
    RawWakerVTable::new(tw_clone, tw_wake, tw_wake_by_ref, tw_drop);

fn make_task_waker(task: Arc<Task>) -> Waker {
    let ptr = Arc::into_raw(task).cast::<()>();
    // SAFETY: `ptr` is a freshly `Arc::into_raw`'d `Task`, and the
    // vtable's functions reconstitute it with the matching type.
    unsafe { Waker::from_raw(RawWaker::new(ptr, &TASK_VTABLE)) }
}

unsafe fn tw_clone(ptr: *const ()) -> RawWaker {
    // SAFETY: `ptr` was produced by `Arc::into_raw::<Task>`.
    let arc = unsafe { Arc::from_raw(ptr.cast::<Task>()) };
    let cloned = Arc::clone(&arc);
    core::mem::forget(arc); // keep the original refcount intact
    RawWaker::new(Arc::into_raw(cloned).cast::<()>(), &TASK_VTABLE)
}

unsafe fn tw_wake(ptr: *const ()) {
    // SAFETY: `ptr` was produced by `Arc::into_raw::<Task>`; consume it
    // (wake-by-value drops one refcount).
    let arc = unsafe { Arc::from_raw(ptr.cast::<Task>()) };
    schedule(arc);
}

unsafe fn tw_wake_by_ref(ptr: *const ()) {
    // SAFETY: `ptr` was produced by `Arc::into_raw::<Task>`; do not drop
    // the refcount (wake-by-ref).
    let arc = unsafe { Arc::from_raw(ptr.cast::<Task>()) };
    schedule(Arc::clone(&arc));
    core::mem::forget(arc);
}

unsafe fn tw_drop(ptr: *const ()) {
    // SAFETY: `ptr` was produced by `Arc::into_raw::<Task>`.
    drop(unsafe { Arc::from_raw(ptr.cast::<Task>()) });
}

// ============================================================
// Scheduling — push to a queue + notify a worker, gated by state.
// ============================================================

/// Wake `task`: move it into a run queue iff it is idle, or mark a
/// mid-poll wake so the running poll re-schedules itself afterward.
fn schedule(task: Arc<Task>) {
    loop {
        match task.state.load(Ordering::Acquire) {
            IDLE => {
                if task
                    .state
                    .compare_exchange(IDLE, QUEUED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    enqueue(&task);
                    task.shared.notify_one();
                    return;
                }
            }
            RUNNING => {
                // Folded into the running poll; it will re-enqueue.
                if task
                    .state
                    .compare_exchange(
                        RUNNING,
                        RUNNING_NOTIFIED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return;
                }
            }
            // QUEUED (already runnable) or RUNNING_NOTIFIED (already
            // flagged) — nothing to do.
            _ => return,
        }
    }
}

/// Enqueue an already-`QUEUED` task: onto the current worker's local
/// deque when scheduled from within its own runtime, else the injector.
fn enqueue(task: &Arc<Task>) {
    let pushed = MT_CURRENT.with(|c| {
        let cur = c.borrow();
        match cur.as_ref() {
            Some(cur) if Arc::ptr_eq(&cur.shared, &task.shared) => {
                cur.local.push(Arc::clone(task)).is_ok()
            }
            _ => false,
        }
    });
    if !pushed {
        task.shared
            .injector
            .lock()
            .unwrap()
            .push_back(Arc::clone(task));
    }
}

/// Mark a fresh or re-woken task `QUEUED`, enqueue it, and notify a
/// worker.
fn spawn_task(task: Arc<Task>) {
    task.state.store(QUEUED, Ordering::Release);
    enqueue(&task);
    task.shared.notify_one();
}

/// Run one task: take its future, poll it, and either retire it (Ready)
/// or restore it and re-schedule if it was woken mid-poll (Pending).
fn run_task(task: Arc<Task>) {
    BUDGET.with(|b| b.set(BUDGET_INIT));
    // Taken from a queue with state `QUEUED`; only this worker holds it
    // now and `schedule` is a no-op while `QUEUED`, so the store is safe.
    task.state.store(RUNNING, Ordering::Release);

    let mut fut = match task.future.lock().unwrap().take() {
        Some(f) => f,
        None => return, // already retired (defensive)
    };
    let waker = make_task_waker(Arc::clone(&task));
    let mut cx = Context::from_waker(&waker);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(()) => {
            // Retire: drop the future, never schedule again. `future`
            // stays `None`; the `Arc<Task>` frees once all wakers drop.
            drop(fut);
        }
        Poll::Pending => {
            *task.future.lock().unwrap() = Some(fut);
            // Try RUNNING → IDLE. If a wake landed mid-poll the state is
            // RUNNING_NOTIFIED, so re-schedule to poll again — but to the
            // *back* (injector), not this worker's LIFO local deque: a
            // local push would be popped again immediately, re-running
            // the same task and starving its peers (this is exactly the
            // yield_now / hot-recv fairness path). Going through the
            // injector lets already-queued tasks run first.
            if task
                .state
                .compare_exchange(RUNNING, IDLE, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                task.state.store(QUEUED, Ordering::Release);
                task.shared
                    .injector
                    .lock()
                    .unwrap()
                    .push_back(Arc::clone(&task));
                task.shared.notify_one();
            }
        }
    }
}

// ============================================================
// Shared runtime state — held by Arc across workers + driver.
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

struct Shared {
    /// Overflow + off-worker queue.
    injector: Mutex<VecDeque<Arc<Task>>>,
    /// Each worker's deque, for stealing.
    stealers: Vec<Arc<Deque<Arc<Task>>>>,
    /// Worker park state: `true` once a wake is pending.
    park_notified: Mutex<bool>,
    park_cv: Condvar,
    /// Interrupts the driver's blocked `Poll::poll` to recompute timeout.
    driver_unparker: IoWaker,
    /// Reactor registry, shared with workers for IO registration.
    registry: Registry,
    /// Token → (fd, waker) for one-shot IO readiness.
    io: Mutex<IoWaiters>,
    /// Timer wheel, advanced by the driver.
    timer: Mutex<Wheel>,
    /// Monotonic anchor for tick computation.
    start: Instant,
    /// Set on shutdown; workers and driver exit their loops.
    shutdown: AtomicBool,
}

impl Shared {
    fn now_tick(&self) -> u64 {
        (self.start.elapsed().as_millis() as u64) / TICK_MS
    }

    /// Wake one parked worker (if any).
    fn notify_one(&self) {
        let mut notified = self.park_notified.lock().unwrap();
        *notified = true;
        drop(notified);
        self.park_cv.notify_one();
    }

    /// Park the calling worker until a wake is pending or shutdown. The
    /// `notified` flag closes the lost-wakeup window: a `notify_one`
    /// between a failed scan and the `wait` is observed here instead of
    /// being missed.
    fn park_worker(&self) {
        let mut notified = self.park_notified.lock().unwrap();
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        if *notified {
            *notified = false;
            return; // a wake raced our scan — rescan
        }
        loop {
            notified = self.park_cv.wait(notified).unwrap();
            if self.shutdown.load(Ordering::Acquire) {
                return;
            }
            if *notified {
                *notified = false;
                return;
            }
        }
    }

    /// Spawn `future` as a new task on this runtime.
    fn spawn<F>(self: &Arc<Self>, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let join = Arc::new(JoinShared::new());
        let join_for_task = Arc::clone(&join);
        let task = Arc::new(Task::new(
            Arc::clone(self),
            Box::pin(async move {
                let output = future.await;
                join_for_task.complete(output);
            }),
        ));
        spawn_task(task);
        JoinHandle::new(join)
    }
}

// ============================================================
// Per-worker thread-local context (for enqueue + leaf dispatch).
// ============================================================

struct MtCurrent {
    shared: Arc<Shared>,
    local: Arc<Deque<Arc<Task>>>,
}

std::thread_local! {
    static MT_CURRENT: RefCell<Option<MtCurrent>> = const { RefCell::new(None) };
    /// Cooperative budget for the running task (0 ⇒ not on a worker).
    static BUDGET: Cell<u32> = const { Cell::new(0) };
}

struct MtCurrentGuard;

impl Drop for MtCurrentGuard {
    fn drop(&mut self) {
        MT_CURRENT.with(|c| *c.borrow_mut() = None);
    }
}

fn set_mt_current(shared: Arc<Shared>, local: Arc<Deque<Arc<Task>>>) -> MtCurrentGuard {
    MT_CURRENT.with(|c| *c.borrow_mut() = Some(MtCurrent { shared, local }));
    MtCurrentGuard
}

// ============================================================
// Worker + driver loops.
// ============================================================

fn run_worker(shared: Arc<Shared>, local: Arc<Deque<Arc<Task>>>, index: usize) {
    let _guard = set_mt_current(Arc::clone(&shared), Arc::clone(&local));
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }
        if let Some(task) = find_task(&shared, &local, index) {
            // We found work and there may be more — wake a peer to help
            // drain it (cascading wake-up keeps the pool subscribed).
            shared.notify_one();
            run_task(task);
            continue;
        }
        shared.park_worker();
    }
}

/// Find the next task: local deque, then steal from peers, then the
/// injector.
fn find_task(shared: &Arc<Shared>, local: &Deque<Arc<Task>>, index: usize) -> Option<Arc<Task>> {
    if let Some(task) = local.pop() {
        return Some(task);
    }
    let n = shared.stealers.len();
    for off in 1..=n {
        let victim = (index + off) % n;
        if victim == index {
            continue;
        }
        loop {
            match shared.stealers[victim].steal() {
                Steal::Data(task) => return Some(task),
                Steal::Retry => continue,
                Steal::Empty => break,
            }
        }
    }
    shared.injector.lock().unwrap().pop_front()
}

fn run_driver(shared: Arc<Shared>, mut reactor: Reactor) {
    let mut events = Events::with_capacity(256);
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }

        // Fire due timers, waking their waiters.
        let now = shared.now_tick();
        let mut fired: Vec<TimerEntry> = Vec::new();
        shared.timer.lock().unwrap().advance(now, &mut fired);
        for entry in fired {
            entry.done.store(true, Ordering::Release);
            entry.waker.wake();
        }

        // Block until IO readiness, the next timer deadline, or an
        // unpark (a worker registered fresh timer/IO interest).
        let now = shared.now_tick();
        let next_timer = shared.timer.lock().unwrap().next_deadline();
        let timeout = next_timer
            .map(|deadline| Duration::from_millis(deadline.saturating_sub(now) * TICK_MS));

        reactor
            .poll(&mut events, timeout)
            .expect("grpcx-rt: reactor poll failed");

        // Collect fired tokens first to avoid holding the io lock across
        // the iteration.
        let mut tokens: Vec<usize> = Vec::new();
        for event in &events {
            tokens.push(event.token().0);
        }
        for token in tokens {
            if token == UNPARK_TOKEN {
                let _ = shared.driver_unparker.reset();
                continue;
            }
            let entry = shared.io.lock().unwrap().map.remove(&token);
            if let Some(entry) = entry {
                // One-shot: deregister so a level-triggered backend does
                // not re-fire before the waiter re-registers.
                let mut source = SourceFd::new(entry.fd);
                let _ = shared.registry.deregister(&mut source);
                entry.waker.wake();
            }
        }
    }
}

// ============================================================
// Leaf dispatch — called from `executor`'s Sleep / Readable.
// ============================================================

/// Register a timer on the current worker's runtime. Returns `true` if a
/// multi-thread runtime handled it; `false` to fall back to the
/// current-thread runtime.
pub(crate) fn register_sleep_mt(ticks: u64, waker: Waker, done: Arc<AtomicBool>) -> bool {
    MT_CURRENT.with(|c| {
        let cur = c.borrow();
        let Some(cur) = cur.as_ref() else {
            return false;
        };
        let deadline = cur.shared.now_tick().saturating_add(ticks);
        cur.shared.timer.lock().unwrap().insert(TimerEntry {
            when: deadline,
            waker,
            done,
        });
        // Make the driver recompute its timeout to include this deadline.
        let _ = cur.shared.driver_unparker.wake();
        true
    })
}

/// Register `fd` for one-shot readability on the current worker's
/// runtime. Returns the token, or `None` to fall back to the
/// current-thread runtime.
pub(crate) fn register_readable_mt(fd: RawFd, waker: Waker) -> Option<usize> {
    MT_CURRENT.with(|c| {
        let cur = c.borrow();
        let cur = cur.as_ref()?;
        let token = cur.shared.io.lock().unwrap().alloc();
        let mut source = SourceFd::new(fd);
        cur.shared
            .registry
            .register(&mut source, Token(token), Interest::READABLE)
            .expect("grpcx-rt: reactor register failed");
        cur.shared
            .io
            .lock()
            .unwrap()
            .map
            .insert(token, IoEntry { fd, waker });
        // Interrupt the driver so it re-polls with the new fd registered.
        let _ = cur.shared.driver_unparker.wake();
        Some(token)
    })
}

/// Refresh the waker for a pending MT readiness registration. Returns
/// `Some(true)` if still pending, `Some(false)` if it already fired
/// (one-shot deregistered), `None` if no MT runtime is current.
pub(crate) fn refresh_readable_mt(token: usize, waker: Waker) -> Option<bool> {
    MT_CURRENT.with(|c| {
        let cur = c.borrow();
        let cur = cur.as_ref()?;
        let mut io = cur.shared.io.lock().unwrap();
        match io.map.get_mut(&token) {
            Some(entry) => {
                entry.waker = waker;
                Some(true)
            }
            None => Some(false),
        }
    })
}

/// Deregister a still-pending MT readiness registration (on drop).
pub(crate) fn deregister_readable_mt(token: usize, fd: RawFd) {
    let _ = MT_CURRENT.try_with(|c| {
        let cur = c.borrow();
        if let Some(cur) = cur.as_ref()
            && cur.shared.io.lock().unwrap().map.remove(&token).is_some()
        {
            let mut source = SourceFd::new(fd);
            let _ = cur.shared.registry.deregister(&mut source);
        }
    });
}

/// Consume one unit of cooperative budget; return `true` when it is just
/// exhausted (the caller should yield once). A no-op — always `false` —
/// when not running on a multi-thread worker (budget unset / 0).
pub(crate) fn budget_exhausted() -> bool {
    BUDGET.with(|b| {
        let v = b.get();
        if v == 0 {
            return false;
        }
        let next = v - 1;
        b.set(next);
        next == 0
    })
}

// ============================================================
// yield_now.
// ============================================================

/// Yield control back to the runtime once, letting the worker service
/// other tasks before this one is polled again.
pub fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

/// Future returned by [`yield_now`]. Pending exactly once (self-waking),
/// then Ready.
pub struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

// ============================================================
// Runtime + RtHandle — the public surface.
// ============================================================

/// A multi-thread, work-stealing runtime. Spawns a pool of worker
/// threads plus a driver thread on construction; joins them on drop.
pub struct Runtime {
    shared: Arc<Shared>,
    workers: Vec<thread::JoinHandle<()>>,
    driver: Option<thread::JoinHandle<()>>,
}

impl Runtime {
    /// Create a runtime with `workers` worker threads (minimum 1) plus a
    /// dedicated driver thread for the reactor and timers.
    ///
    /// # Errors
    ///
    /// Returns an error if the reactor or its unparker fail to
    /// initialize.
    pub fn new(workers: usize) -> IoResult<Self> {
        let workers = workers.max(1);
        let reactor = Reactor::new()?;
        let registry = reactor.registry().clone();
        let driver_unparker = IoWaker::new(&registry, Token(UNPARK_TOKEN))?;

        let deques: Vec<Arc<Deque<Arc<Task>>>> = (0..workers)
            .map(|_| Arc::new(Deque::new(LOCAL_CAP)))
            .collect();

        let shared = Arc::new(Shared {
            injector: Mutex::new(VecDeque::new()),
            stealers: deques.clone(),
            park_notified: Mutex::new(false),
            park_cv: Condvar::new(),
            driver_unparker,
            registry,
            io: Mutex::new(IoWaiters::new()),
            timer: Mutex::new(Wheel::new()),
            start: Instant::now(),
            shutdown: AtomicBool::new(false),
        });

        let driver = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("grpcx-rt-driver".into())
                .spawn(move || run_driver(shared, reactor))
                .expect("grpcx-rt: spawn driver thread")
        };

        let mut worker_handles = Vec::with_capacity(workers);
        for (index, local) in deques.into_iter().enumerate() {
            let shared = Arc::clone(&shared);
            let handle = thread::Builder::new()
                .name(format!("grpcx-rt-worker-{index}"))
                .spawn(move || run_worker(shared, local, index))
                .expect("grpcx-rt: spawn worker thread");
            worker_handles.push(handle);
        }

        Ok(Self {
            shared,
            workers: worker_handles,
            driver: Some(driver),
        })
    }

    /// Spawn `future` onto the runtime, returning a [`JoinHandle`] for
    /// its output.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.shared.spawn(future)
    }

    /// Spawn `future` as the root task and block the calling thread
    /// until it completes, returning its output.
    ///
    /// The calling thread is not a worker: it parks on a `Condvar` while
    /// the pool drives the root task, so this composes with any number
    /// of nested `spawn`s without occupying a worker slot.
    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let slot: Arc<(Mutex<Option<F::Output>>, Condvar)> =
            Arc::new((Mutex::new(None), Condvar::new()));
        let slot_for_task = Arc::clone(&slot);
        let _join = self.spawn(async move {
            let output = future.await;
            let (lock, cv) = &*slot_for_task;
            *lock.lock().unwrap() = Some(output);
            cv.notify_one();
        });

        let (lock, cv) = &*slot;
        let mut guard = lock.lock().unwrap();
        while guard.is_none() {
            guard = cv.wait(guard).unwrap();
        }
        guard.take().unwrap()
    }

    /// A cheap, cloneable handle to this runtime, usable as a [`Spawn`]
    /// implementor and sendable to other threads.
    pub fn handle(&self) -> RtHandle {
        RtHandle {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        // Wake every parked worker so it observes shutdown. Taking the
        // park lock orders this `notify_all` after any worker's
        // shutdown-check-then-wait, so none is left parked.
        {
            let _guard = self.shared.park_notified.lock().unwrap();
            self.shared.park_cv.notify_all();
        }
        // Wake the driver out of its blocking poll.
        let _ = self.shared.driver_unparker.wake();

        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

/// A cloneable handle to a [`Runtime`], implementing [`Spawn`]. Unlike
/// the current-thread `Handle`, this carries an `Arc` to the runtime and
/// is `Send`, so it can be moved into spawned tasks or other threads.
#[derive(Clone)]
pub struct RtHandle {
    shared: Arc<Shared>,
}

impl RtHandle {
    /// Spawn `future` onto the runtime, returning a [`JoinHandle`].
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.shared.spawn(future)
    }
}

impl Spawn for RtHandle {
    fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.shared.spawn(future)
    }
}

#[cfg(test)]
mod sizes {
    use super::*;

    // Public handles are a single `Arc` pointer; a stray new field would
    // break these. `size_of::<usize>()` keeps them target-width-agnostic.
    const PTR: usize = core::mem::size_of::<usize>();
    const _: () = assert!(core::mem::size_of::<RtHandle>() == PTR);
    const _: () = assert!(core::mem::size_of::<crate::JoinHandle<()>>() == PTR);
    const _: () = assert!(core::mem::size_of::<crate::Sender<()>>() == PTR);
    const _: () = assert!(core::mem::size_of::<crate::Receiver<()>>() == PTR);

    #[test]
    fn report_internal_sizes() {
        // Heap-payload footprints. Platform-dependent (std `Mutex` /
        // `Condvar` sizes differ across OSes), so recorded in BUDGETS.md
        // from the dev host rather than const-asserted.
        std::eprintln!("size_of::<Task>()   = {}", core::mem::size_of::<Task>());
        std::eprintln!("size_of::<Shared>() = {}", core::mem::size_of::<Shared>());
    }
}

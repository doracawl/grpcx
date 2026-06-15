//! Conformance for the cross-thread [`Waker`] (mio::Waker parity).
//!
//! Runs on whichever host executes it: Linux (eventfd), macOS / iOS,
//! FreeBSD (kqueue EVFILT_USER). Cross-compiled targets are checked but
//! these run only where a backend exists.

#![cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use grpcx_io::{Events, Poll, Token, Waker};

fn fired_tokens(events: &Events) -> Vec<usize> {
    events.into_iter().map(|e| e.token().0).collect()
}

#[test]
fn waker_wakes_poll_same_thread() {
    let mut poll = Poll::new().expect("poll");
    let waker = Waker::new(poll.registry(), Token(99)).expect("waker");

    waker.wake().expect("wake");

    let mut events = Events::with_capacity(8);
    poll.poll(&mut events, Some(Duration::from_secs(1)))
        .expect("poll");
    assert!(fired_tokens(&events).contains(&99));
    waker.reset().expect("reset");
}

#[test]
fn waker_wakes_blocked_poll_from_another_thread() {
    let mut poll = Poll::new().expect("poll");
    let waker = Arc::new(Waker::new(poll.registry(), Token(7)).expect("waker"));

    let bg = {
        let waker = Arc::clone(&waker);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            waker.wake().expect("wake from thread");
        })
    };

    // A generous timeout: a correct cross-thread wake interrupts the
    // blocked poll in ~20 ms; only a broken waker waits this out.
    let mut events = Events::with_capacity(8);
    poll.poll(&mut events, Some(Duration::from_secs(5)))
        .expect("poll");
    assert!(fired_tokens(&events).contains(&7));

    waker.reset().expect("reset");
    bg.join().expect("join");
}

#[test]
fn waker_reset_then_wake_again() {
    let mut poll = Poll::new().expect("poll");
    let waker = Waker::new(poll.registry(), Token(3)).expect("waker");

    for _ in 0..3 {
        waker.wake().expect("wake");
        let mut events = Events::with_capacity(8);
        poll.poll(&mut events, Some(Duration::from_secs(1)))
            .expect("poll");
        assert!(fired_tokens(&events).contains(&3));
        waker.reset().expect("reset");
    }
}

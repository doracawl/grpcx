//! Smallest possible binary that exercises the L1 public API.
//!
//! Used by `cargo bloat --release --example min_poll` to track
//! `grpcx-io`'s contribution to a downstream binary's size — the
//! BINSIZE tier in `BUDGETS.md`. Doing one `Poll::new` + one
//! `poll(timeout = ZERO)` is enough to force-link every Selector
//! constructor + the `Events` translation path; LTO removes
//! everything else.

use grpcx_io::{Events, IoResult, Poll};
use std::time::Duration;

fn main() -> IoResult<()> {
    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(64);
    poll.poll(&mut events, Some(Duration::ZERO))?;
    // Print "ok" so the binary has at least one observable side effect
    // and the optimizer doesn't simplify the whole program to a
    // single return.
    println!("ok");
    Ok(())
}

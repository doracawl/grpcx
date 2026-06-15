//! Canonical binary-size target (BUDGETS.md, BINSIZE tier): the minimal
//! program that exercises the multi-thread runtime — construct a
//! `Runtime`, `block_on` a single spawned task. Measured per minor
//! version with `cargo bloat --release --example min_spawn`.

use grpcx_rt::Runtime;

fn main() {
    let rt = Runtime::new(2).expect("runtime");
    let handle = rt.handle();
    let answer = rt.block_on(async move { handle.spawn(async { 21 + 21 }).await });
    println!("{answer}");
}

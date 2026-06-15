//! Shared loopback datagram-budget probe for the bench /
//! profiling bins: derive the QUIC 1-RTT plaintext budget
//! from the live `net.inet.udp.maxdgram` sysctl instead of
//! hardcoding it, so a raised sysctl flows straight into a
//! re-run (the 64 KiB-lane packet-count experiment).

/// QUIC header + AEAD tag + frame-header headroom inside
/// one datagram.
const WIRE_OVERHEAD: usize = 300;

/// The loopback plaintext budget: `maxdgram` minus wire
/// overhead, capped at what a u16 UDP length can carry.
/// Linux has no `maxdgram` sysctl — the kernel takes any
/// datagram up to the UDP maximum (65 507 payload), so the
/// probe returns the cap directly. Other non-Darwin (or
/// probe failure) falls back to the Darwin default sysctl
/// value so numbers stay comparable.
pub fn loopback_datagram_budget() -> usize {
    // Experiment lever: pin the budget regardless of
    // platform probing (e.g. 1350 to reproduce the
    // internet-default many-datagram shape on loopback).
    if let Some(n) = std::env::var("GRPCX_BENCH_BUDGET")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        return n;
    }
    if cfg!(target_os = "linux") {
        return 65000;
    }
    let maxdgram = std::process::Command::new("sysctl")
        .args(["-n", "net.inet.udp.maxdgram"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(9216);
    maxdgram.saturating_sub(WIRE_OVERHEAD).min(65000)
}

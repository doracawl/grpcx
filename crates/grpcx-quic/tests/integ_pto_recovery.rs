//! Reliability phase 2 acceptance: timer-driven recovery
//! on an intra-process loopback with a deterministic
//! synthetic clock (every pump stamped; no wall-clock
//! reads). Covers the three loss shapes packet-threshold
//! detection (phase 1) cannot see:
//!
//! - a flight TAIL (no later packet to trigger the
//!   threshold) → PTO retransmit;
//! - a lost ACK (receiver owes nothing) → PTO probe makes
//!   the peer re-ACK its full received ranges;
//! - handshake-plane loss (no ACK machinery there at all)
//!   → blind full-flight CRYPTO retransmit until the
//!   handshake is confirmed via HANDSHAKE_DONE.
//!
//! Plus RTT-estimator KATs and the NewReno window shape.

use grpcx_crypto::TlsConfig;
use grpcx_quic::QuicConnection;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};

const SERVER_NAME: &str = "loopback.example";

/// One synthetic park slice — the cadence at which a real
/// drive loop re-pumps an idle connection.
const STEP_US: u64 = 500;

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).expect("CertificateParams");
    let kp = KeyPair::generate_for(&PKCS_ED25519).expect("kp");
    let cert = params.self_signed(&kp).expect("self_signed");
    (cert.der().to_vec(), kp.serialize_der())
}

fn fresh_pair() -> (QuicConnection, QuicConnection) {
    let (cert, key) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert], &key).expect("server cfg");
    server_cfg.set_alpn(&[b"hq"]);
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert).expect("root");
    client_cfg.set_alpn(&[b"hq"]);
    let client =
        QuicConnection::new_client(&client_cfg, SERVER_NAME, b"c".to_vec()).expect("client");
    let server = QuicConnection::new_server(&server_cfg, b"s".to_vec()).expect("server");
    (client, server)
}

fn established_pair() -> (QuicConnection, QuicConnection) {
    let (mut client, mut server) = fresh_pair();
    let mut clock = 0u64;
    drive_timed(
        &mut client,
        &mut server,
        &mut clock,
        64,
        |b| b,
        |b| b,
        |c, s| c.is_established() && s.is_established(),
    );
    (client, server)
}

/// Timed lockstep driver: each round pumps both ends at the
/// shared synthetic clock, advances it one STEP, and runs
/// the direction mangles (client→server / server→client).
/// Returns once `done` holds AND both directions are quiet;
/// panics if `max_rounds` elapse first.
fn drive_timed<FCS, FSC>(
    client: &mut QuicConnection,
    server: &mut QuicConnection,
    clock: &mut u64,
    max_rounds: usize,
    mut mangle_cs: FCS,
    mut mangle_sc: FSC,
    mut done: impl FnMut(&QuicConnection, &QuicConnection) -> bool,
) where
    FCS: FnMut(Vec<Vec<u8>>) -> Vec<Vec<u8>>,
    FSC: FnMut(Vec<Vec<u8>>) -> Vec<Vec<u8>>,
{
    let mut to_server: Vec<Vec<u8>> = Vec::new();
    let mut to_client: Vec<Vec<u8>> = Vec::new();
    for _ in 0..max_rounds {
        let mut next_server: Vec<Vec<u8>> = Vec::new();
        let mut next_client: Vec<Vec<u8>> = Vec::new();
        for dg in &to_client {
            next_server.extend(client.pump_at(Some(dg), false, *clock).expect("client"));
        }
        next_server.extend(client.pump_at(None, true, *clock).expect("client idle"));
        for dg in &to_server {
            next_client.extend(server.pump_at(Some(dg), false, *clock).expect("server"));
        }
        next_client.extend(server.pump_at(None, true, *clock).expect("server idle"));
        to_server = mangle_cs(next_server);
        to_client = mangle_sc(next_client);
        *clock += STEP_US;
        if to_server.is_empty() && to_client.is_empty() && done(client, server) {
            return;
        }
    }
    panic!("drive_timed did not converge");
}

/// Advance the clock far past every timer with idle pumps
/// and assert the pair is QUIET — no retransmit storm, no
/// armed handshake timer left behind.
fn assert_quiescent(client: &mut QuicConnection, server: &mut QuicConnection, clock: &mut u64) {
    for _ in 0..4 {
        *clock += 100_000;
        let c = client.pump_at(None, true, *clock).expect("client idle");
        let s = server.pump_at(None, true, *clock).expect("server idle");
        assert!(
            c.is_empty() && s.is_empty(),
            "expected quiescence, got {} client / {} server datagrams",
            c.len(),
            s.len()
        );
    }
}

fn is_short_header(dg: &[u8]) -> bool {
    !dg.is_empty() && dg[0] & 0x80 == 0
}

fn is_handshake_long(dg: &[u8]) -> bool {
    !dg.is_empty() && dg[0] & 0x80 != 0 && ((dg[0] >> 4) & 0x03) == 0b10
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

#[test]
fn tail_loss_recovered_by_pto() {
    let (mut client, mut server) = established_pair();
    let mut clock = 1_000_000u64;
    let sid = client.open_bi();
    let data = payload(4 * 1024);
    client.write_stream(sid, &data, true).expect("write");

    // Drop the LAST short packet of the first burst — the
    // FIN-bearing tail. No later packet exists to trigger
    // the packet threshold; only the PTO can recover.
    let mut dropped = false;
    drive_timed(
        &mut client,
        &mut server,
        &mut clock,
        200,
        |mut burst| {
            if !dropped && burst.iter().any(|d| is_short_header(d)) {
                let last_short = burst.iter().rposition(|d| is_short_header(d)).expect("any");
                burst.remove(last_short);
                dropped = true;
            }
            burst
        },
        |b| b,
        |_c, s| {
            let mut probe = Vec::new();
            s.peek_stream(sid, &mut probe).1
        },
    );
    assert!(dropped, "the tail packet must actually have been dropped");

    let mut got = Vec::new();
    let (n, fin) = server.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(n, data.len());
    assert_eq!(got, data, "PTO-recovered byte stream must be exact");
    assert!(
        client.smoothed_rtt_us().is_some(),
        "ACK round trips must have produced an RTT sample"
    );
    assert_quiescent(&mut client, &mut server, &mut clock);
}

#[test]
fn lost_ack_recovered_by_probe() {
    let (mut client, mut server) = established_pair();
    let mut clock = 1_000_000u64;
    let sid = client.open_bi();
    let data = payload(100);
    client.write_stream(sid, &data, true).expect("write");

    // The data arrives, but the server's (pure-ACK) reply
    // drops. The server owes nothing further; the client's
    // PTO must probe so the server re-ACKs its ranges.
    let mut dropped = false;
    drive_timed(
        &mut client,
        &mut server,
        &mut clock,
        200,
        |b| b,
        |burst| {
            if !dropped && burst.iter().any(|d| is_short_header(d)) {
                dropped = true;
                return burst.into_iter().filter(|d| !is_short_header(d)).collect();
            }
            burst
        },
        |c, s| {
            let mut probe = Vec::new();
            // Done only once the CLIENT has nothing armed —
            // i.e. the re-ACK actually came back.
            s.peek_stream(sid, &mut probe).1 && c.next_wakeup_us().is_none()
        },
    );
    assert!(dropped, "the ACK must actually have been dropped");

    let mut got = Vec::new();
    let (_, fin) = server.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(got, data);
    assert_quiescent(&mut client, &mut server, &mut clock);
}

#[test]
fn lost_server_handshake_flight_recovered() {
    let (mut client, mut server) = fresh_pair();
    let mut clock = 0u64;

    // Eat the server's entire first flight (Initial +
    // Handshake datagrams). Both sides' handshake timers
    // must converge: the client re-offers its ClientHello,
    // the server re-sends its flight.
    let mut eaten = 0usize;
    drive_timed(
        &mut client,
        &mut server,
        &mut clock,
        400,
        |b| b,
        |burst| {
            if eaten < 2 {
                let keep: Vec<_> = burst
                    .into_iter()
                    .filter(|d| {
                        if eaten < 2 && !is_short_header(d) {
                            eaten += 1;
                            return false;
                        }
                        true
                    })
                    .collect();
                return keep;
            }
            burst
        },
        |c, s| c.is_established() && s.is_established(),
    );
    assert_eq!(eaten, 2, "the server flight must actually have been eaten");

    // Liveness after recovery: a data round trip.
    let sid = client.open_bi();
    let data = payload(2 * 1024);
    client.write_stream(sid, &data, true).expect("write");
    drive_timed(
        &mut client,
        &mut server,
        &mut clock,
        200,
        |b| b,
        |b| b,
        |_c, s| {
            let mut probe = Vec::new();
            s.peek_stream(sid, &mut probe).1
        },
    );
    let mut got = Vec::new();
    let (_, fin) = server.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(got, data);
    assert_quiescent(&mut client, &mut server, &mut clock);
}

#[test]
fn lost_client_finished_recovered_via_handshake_done() {
    let (mut client, mut server) = fresh_pair();
    let mut clock = 0u64;

    // Drop the client's first Handshake-level packet (its
    // Finished). The client believes itself established;
    // the server is stuck. The client must keep its
    // handshake retransmit state armed until the server's
    // HANDSHAKE_DONE arrives — that resend completes the
    // server, whose HANDSHAKE_DONE then confirms the client.
    let mut dropped = false;
    drive_timed(
        &mut client,
        &mut server,
        &mut clock,
        400,
        |burst| {
            if !dropped && burst.iter().any(|d| is_handshake_long(d)) {
                dropped = true;
                return burst
                    .into_iter()
                    .filter(|d| !is_handshake_long(d))
                    .collect();
            }
            burst
        },
        |b| b,
        |c, s| c.is_established() && s.is_established(),
    );
    assert!(
        dropped,
        "the Finished packet must actually have been dropped"
    );

    // Liveness + the confirmation actually landed: with the
    // clock jumping far past every backoff, a confirmed
    // pair emits nothing (an unconfirmed client would keep
    // re-sending its Finished forever).
    let sid = client.open_bi();
    let data = payload(512);
    client.write_stream(sid, &data, true).expect("write");
    drive_timed(
        &mut client,
        &mut server,
        &mut clock,
        200,
        |b| b,
        |b| b,
        |_c, s| {
            let mut probe = Vec::new();
            s.peek_stream(sid, &mut probe).1
        },
    );
    let mut got = Vec::new();
    let (_, fin) = server.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(got, data);
    assert_quiescent(&mut client, &mut server, &mut clock);
}

#[test]
fn rtt_estimator_kats() {
    let (mut client, mut server) = established_pair();
    assert_eq!(
        client.smoothed_rtt_us(),
        None,
        "no ack-eliciting client packet has round-tripped yet"
    );

    // First sample: data out at t=10 000, ACK back at
    // t=12 000 → srtt = 2 000 exactly.
    let sid = client.open_bi();
    client
        .write_stream(sid, &payload(100), false)
        .expect("write");
    let burst = client.pump_at(None, true, 10_000).expect("send");
    let mut acks: Vec<Vec<u8>> = Vec::new();
    for dg in &burst {
        acks.extend(server.pump_at(Some(dg), false, 0).expect("server"));
    }
    acks.extend(server.pump_at(None, true, 0).expect("server idle"));
    assert!(!acks.is_empty(), "server must ACK the data packet");
    for dg in &acks {
        client.pump_at(Some(dg), false, 12_000).expect("client");
    }
    assert_eq!(client.smoothed_rtt_us(), Some(2_000));

    // Second sample of 4 000 → EWMA: 7/8·2000 + 1/8·4000.
    client
        .write_stream(sid, &payload(100), false)
        .expect("write");
    let burst = client.pump_at(None, true, 20_000).expect("send");
    let mut acks: Vec<Vec<u8>> = Vec::new();
    for dg in &burst {
        acks.extend(server.pump_at(Some(dg), false, 0).expect("server"));
    }
    acks.extend(server.pump_at(None, true, 0).expect("server idle"));
    for dg in &acks {
        client.pump_at(Some(dg), false, 24_000).expect("client");
    }
    assert_eq!(client.smoothed_rtt_us(), Some(2_250));
}

#[test]
fn first_burst_is_cwnd_limited() {
    let (mut client, mut server) = established_pair();
    let sid = client.open_bi();
    let initial_cwnd = client.cwnd_bytes();
    assert_eq!(initial_cwnd, 10 * 1350, "RFC 9002 initial window");

    // 64 KiB at the 1350 budget would be a ~50-packet burst
    // unthrottled; the initial window caps it near 10.
    let data = payload(64 * 1024);
    client.write_stream(sid, &data, true).expect("write");
    let burst = client.pump(None).expect("pump");
    let shorts = burst.iter().filter(|d| is_short_header(d)).count();
    assert!(
        (10..=12).contains(&shorts),
        "first burst must be cwnd-limited (~10 packets), got {shorts}"
    );

    // Slow start opens the window as ACKs return; the
    // transfer completes and the window has grown.
    let mut clock = 0u64;
    drive_timed(
        &mut client,
        &mut server,
        &mut clock,
        200,
        |b| b,
        |b| b,
        |_c, s| {
            let mut probe = Vec::new();
            s.peek_stream(sid, &mut probe).1
        },
    );
    let mut got = Vec::new();
    let (_, fin) = server.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(got, data);
    assert!(
        client.cwnd_bytes() > initial_cwnd,
        "slow start must have grown the window (got {})",
        client.cwnd_bytes()
    );
}

#[test]
fn loss_shrinks_window_vs_clean_run() {
    // Two identical 32 KiB transfers; the lossy one loses
    // its 2nd short packet (recovered by packet threshold)
    // and must end with a SMALLER congestion window than
    // the clean control run — NewReno halved it once.
    let run = |lossy: bool| -> usize {
        let (mut client, mut server) = established_pair();
        let sid = client.open_bi();
        let data = payload(32 * 1024);
        client.write_stream(sid, &data, true).expect("write");
        let mut seen = 0u32;
        let mut clock = 0u64;
        drive_timed(
            &mut client,
            &mut server,
            &mut clock,
            400,
            |burst| {
                burst
                    .into_iter()
                    .filter(|d| {
                        if !is_short_header(d) {
                            return true;
                        }
                        seen += 1;
                        !(lossy && seen == 2)
                    })
                    .collect()
            },
            |b| b,
            |_c, s| {
                let mut probe = Vec::new();
                s.peek_stream(sid, &mut probe).1
            },
        );
        let mut got = Vec::new();
        let (_, fin) = server.read_stream(sid, &mut got);
        assert!(fin);
        assert_eq!(got, data);
        client.cwnd_bytes()
    };
    let clean = run(false);
    let lossy = run(true);
    assert!(
        lossy < clean,
        "loss must shrink the window: lossy {lossy} vs clean {clean}"
    );
}

#[test]
fn blackout_collapses_window_persistent_congestion() {
    let (mut client, mut server) = established_pair();
    let mut clock = 1_000_000u64;
    let sid = client.open_bi();
    let data = payload(4 * 1024);
    client.write_stream(sid, &data, true).expect("write");
    let initial_cwnd = client.cwnd_bytes();

    // Total blackout: every datagram in both directions
    // vanishes. PTO probes keep being harvested as losses;
    // once their SEND times span more than 3 × PTO with no
    // forward progress, the window must collapse to the
    // minimum (RFC 9002 §7.6).
    for _ in 0..300 {
        clock += 5_000;
        let _ = client.pump_at(None, true, clock).expect("client idle");
        let _ = server.pump_at(None, true, clock).expect("server idle");
        if client.cwnd_bytes() == 2 * 1350 {
            break;
        }
    }
    assert_eq!(
        client.cwnd_bytes(),
        2 * 1350,
        "blackout must collapse cwnd to the minimum window (was {initial_cwnd})"
    );

    // Link restored. The probe backoff is at its cap
    // (~640 ms) after the blackout — jump the clock past
    // one full interval so the next probe fires on the
    // first restored pump (a real drive loop simply waits
    // it out).
    clock += 700_000;
    // The transfer completes byte-exact and slow start
    // regrows the window from the floor.
    drive_timed(
        &mut client,
        &mut server,
        &mut clock,
        400,
        |b| b,
        |b| b,
        |_c, s| {
            let mut probe = Vec::new();
            s.peek_stream(sid, &mut probe).1
        },
    );
    let mut got = Vec::new();
    let (_, fin) = server.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(got, data, "post-blackout recovery must be byte-exact");
    assert!(
        client.cwnd_bytes() > 2 * 1350,
        "slow start must regrow from the collapsed floor"
    );
    assert_quiescent(&mut client, &mut server, &mut clock);
}

#[test]
fn newreno_fallback_stays_green() {
    use grpcx_quic::CongestionAlgorithm;
    let (mut client, mut server) = established_pair();
    client.set_congestion_algorithm(CongestionAlgorithm::NewReno);
    server.set_congestion_algorithm(CongestionAlgorithm::NewReno);
    let sid = client.open_bi();
    let data = payload(32 * 1024);
    client.write_stream(sid, &data, true).expect("write");

    // Same lossy shape as the threshold gold test — the
    // fallback algorithm must recover identically.
    let mut seen = 0u32;
    let mut clock = 2_000_000u64;
    drive_timed(
        &mut client,
        &mut server,
        &mut clock,
        400,
        |burst| {
            burst
                .into_iter()
                .filter(|d| {
                    if !is_short_header(d) {
                        return true;
                    }
                    seen += 1;
                    seen != 2
                })
                .collect()
        },
        |b| b,
        |_c, s| {
            let mut probe = Vec::new();
            s.peek_stream(sid, &mut probe).1
        },
    );
    let mut got = Vec::new();
    let (_, fin) = server.read_stream(sid, &mut got);
    assert!(fin);
    assert_eq!(got, data);
    assert_quiescent(&mut client, &mut server, &mut clock);
}

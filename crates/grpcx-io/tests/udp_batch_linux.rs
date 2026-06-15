//! Linux batch-datagram syscalls (`sendmmsg` / `recvmmsg`)
//! against real connected UDP socket pairs — the loopback
//! round-trip verifies the syscall numbers and the
//! `mmsghdr` layout (the Darwin precedent).

#![cfg(target_os = "linux")]

use std::net::UdpSocket;
use std::os::fd::AsRawFd;

fn connected_pair() -> (UdpSocket, UdpSocket) {
    let a = UdpSocket::bind("127.0.0.1:0").unwrap();
    let b = UdpSocket::bind("127.0.0.1:0").unwrap();
    a.connect(b.local_addr().unwrap()).unwrap();
    b.connect(a.local_addr().unwrap()).unwrap();
    a.set_nonblocking(true).unwrap();
    b.set_nonblocking(true).unwrap();
    (a, b)
}

#[test]
fn batch_send_then_batch_recv_round_trips() {
    let (a, b) = connected_pair();

    // 48 distinct datagrams in one syscall.
    let payloads: Vec<Vec<u8>> = (0..48u8)
        .map(|i| vec![i; (i as usize % 1100) + 1])
        .collect();
    let refs: Vec<&[u8]> = payloads.iter().map(|p| p.as_slice()).collect();
    let sent = grpcx_io::udp_batch::send_batch(a.as_raw_fd(), &refs).unwrap();
    assert_eq!(sent, 48, "all 48 datagrams accepted in one syscall");

    // Drain them in batches on the other side (deadline
    // driven — loopback delivery can lag under parallel
    // test load).
    let mut got: Vec<Vec<u8>> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while got.len() < 48 && std::time::Instant::now() < deadline {
        let mut storage: Vec<Vec<u8>> = (0..16).map(|_| vec![0u8; 2048]).collect();
        let mut bufs: Vec<&mut [u8]> = storage.iter_mut().map(|b| b.as_mut_slice()).collect();
        let mut lens = [0usize; 16];
        match grpcx_io::udp_batch::recv_batch(b.as_raw_fd(), &mut bufs, &mut lens) {
            Ok(n) => {
                for i in 0..n {
                    got.push(storage[i][..lens[i]].to_vec());
                }
            }
            Err(e) if e.kind() == grpcx_io::IoErrorKind::WouldBlock => {
                std::thread::yield_now();
            }
            Err(e) => panic!("recv_batch failed: {e:?}"),
        }
    }
    assert_eq!(got, payloads, "datagram order + content preserved");
}

#[test]
fn recv_batch_on_empty_socket_is_would_block() {
    let (_a, b) = connected_pair();
    let mut storage = vec![0u8; 2048];
    let mut bufs = [storage.as_mut_slice()];
    let mut lens = [0usize; 1];
    let r = grpcx_io::udp_batch::recv_batch(b.as_raw_fd(), &mut bufs, &mut lens);
    assert!(matches!(
        r,
        Err(e) if e.kind() == grpcx_io::IoErrorKind::WouldBlock
    ));
}

#[test]
fn oversized_batch_is_capped_not_rejected() {
    let (a, b) = connected_pair();
    // 100 datagrams offered; the wrapper caps a single call
    // at 64 — callers loop on the short count.
    let payloads: Vec<Vec<u8>> = (0..100u8).map(|i| vec![i; 64]).collect();
    let refs: Vec<&[u8]> = payloads.iter().map(|p| p.as_slice()).collect();
    let mut off = 0;
    while off < refs.len() {
        let sent = grpcx_io::udp_batch::send_batch(a.as_raw_fd(), &refs[off..]).unwrap();
        assert!(sent >= 1);
        off += sent;
    }

    let mut count = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while count < 100 && std::time::Instant::now() < deadline {
        let mut storage: Vec<Vec<u8>> = (0..32).map(|_| vec![0u8; 256]).collect();
        let mut bufs: Vec<&mut [u8]> = storage.iter_mut().map(|b| b.as_mut_slice()).collect();
        let mut lens = [0usize; 32];
        match grpcx_io::udp_batch::recv_batch(b.as_raw_fd(), &mut bufs, &mut lens) {
            Ok(n) => count += n,
            Err(e) if e.kind() == grpcx_io::IoErrorKind::WouldBlock => std::thread::yield_now(),
            Err(e) => panic!("recv_batch failed: {e:?}"),
        }
    }
    assert_eq!(count, 100);
}

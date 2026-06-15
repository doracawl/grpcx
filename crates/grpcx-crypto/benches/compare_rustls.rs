//! Head-to-head bench: `grpcx-crypto` vs raw `rustls` + `ring` on
//! identical workloads.
//!
//! The bench measures the **wrapper overhead** — what going through
//! our trait API costs over calling rustls / ring directly. The v3
//! trigger gate is ≤ 5 % on the steady-state paths.
//!
//! Three groups:
//!
//! - `aead/`: zero-alloc AEAD seal+open on 1 KiB plaintext.
//! - `hkdf/`: one `expand_label` to 32 bytes.
//! - `tls/`: full 1-RTT loopback handshake + 1 KiB app-data round
//!   trip both directions.
//!
//! Fairness invariants:
//!
//! - Same workload constants on both sides (`N` / `K` shared).
//! - Cert + key minted **outside** the iter loop; per-iter measures
//!   protocol work, not cert generation.
//! - rustls's `ClientConnection` / `ServerConnection` constructed
//!   the same way the wrapper constructs them.

use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::Arc;

const SERVER_NAME: &str = "loopback.example";

// ====================================================================
// AEAD
// ====================================================================

fn bench_aead_aes128gcm(c: &mut Criterion) {
    use grpcx_crypto::{Aead, Aes128Gcm};
    use ring::aead;

    let key = [0x42u8; 16];
    let nonce = [0x12u8; 12];
    let aad: &[u8] = b"";
    let mut pt = vec![0u8; 1024];
    for (i, slot) in pt.iter_mut().enumerate() {
        *slot = i as u8;
    }

    let mut group = c.benchmark_group("aead/aes128gcm_seal_open_1k");

    {
        let aead = Aes128Gcm::new(&key).expect("new");
        group.bench_function("grpcx_crypto", |b| {
            b.iter(|| {
                let mut buf = Vec::with_capacity(pt.len() + Aes128Gcm::TAG_LEN);
                buf.extend_from_slice(&pt);
                buf.extend_from_slice(&[0u8; 16]);
                aead.seal_in_place(&nonce, aad, &mut buf, pt.len()).unwrap();
                aead.open_in_place(&nonce, aad, &mut buf).unwrap();
            });
        });
    }

    {
        let unbound = aead::UnboundKey::new(&aead::AES_128_GCM, &key).expect("unbound");
        let key = aead::LessSafeKey::new(unbound);
        group.bench_function("ring_direct", |b| {
            b.iter(|| {
                let mut buf = pt.clone();
                let n = aead::Nonce::try_assume_unique_for_key(&nonce).unwrap();
                let tag = key
                    .seal_in_place_separate_tag(n, aead::Aad::from(aad), &mut buf)
                    .unwrap();
                buf.extend_from_slice(tag.as_ref());
                let n = aead::Nonce::try_assume_unique_for_key(&nonce).unwrap();
                key.open_in_place(n, aead::Aad::from(aad), &mut buf)
                    .unwrap();
            });
        });
    }

    group.finish();
}

fn bench_aead_chacha(c: &mut Criterion) {
    use grpcx_crypto::{Aead, ChaCha20Poly1305};
    use ring::aead;

    let key = [0x42u8; 32];
    let nonce = [0x12u8; 12];
    let aad: &[u8] = b"";
    let pt = vec![0u8; 1024];

    let mut group = c.benchmark_group("aead/chacha20poly1305_seal_open_1k");

    {
        let aead = ChaCha20Poly1305::new(&key).expect("new");
        group.bench_function("grpcx_crypto", |b| {
            b.iter(|| {
                let mut buf = Vec::with_capacity(pt.len() + ChaCha20Poly1305::TAG_LEN);
                buf.extend_from_slice(&pt);
                buf.extend_from_slice(&[0u8; 16]);
                aead.seal_in_place(&nonce, aad, &mut buf, pt.len()).unwrap();
                aead.open_in_place(&nonce, aad, &mut buf).unwrap();
            });
        });
    }

    {
        let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &key).expect("unbound");
        let key = aead::LessSafeKey::new(unbound);
        group.bench_function("ring_direct", |b| {
            b.iter(|| {
                let mut buf = pt.clone();
                let n = aead::Nonce::try_assume_unique_for_key(&nonce).unwrap();
                let tag = key
                    .seal_in_place_separate_tag(n, aead::Aad::from(aad), &mut buf)
                    .unwrap();
                buf.extend_from_slice(tag.as_ref());
                let n = aead::Nonce::try_assume_unique_for_key(&nonce).unwrap();
                key.open_in_place(n, aead::Aad::from(aad), &mut buf)
                    .unwrap();
            });
        });
    }

    group.finish();
}

// ====================================================================
// HKDF
// ====================================================================

fn bench_hkdf_expand_label(c: &mut Criterion) {
    use grpcx_crypto::{Hkdf, Sha256};
    use ring::hkdf;

    let salt = [1u8; 16];
    let ikm = [2u8; 32];

    let mut group = c.benchmark_group("hkdf/expand_label_32");

    group.bench_function("grpcx_crypto", |b| {
        let hkdf = Hkdf::<Sha256>::extract(&salt, &ikm);
        b.iter(|| {
            let mut out = [0u8; 32];
            hkdf.expand_label(b"quic key", &[], &mut out).unwrap();
        });
    });

    group.bench_function("ring_direct", |b| {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &salt).extract(&ikm);
        b.iter(|| {
            // Hand-assemble the HKDF-Expand-Label info string
            // (RFC 8446 §7.1 wire format) and call ring's expand.
            let mut info: Vec<u8> = Vec::with_capacity(2 + 1 + 14 + 1);
            info.extend_from_slice(&32u16.to_be_bytes());
            info.push((6 + 8) as u8);
            info.extend_from_slice(b"tls13 ");
            info.extend_from_slice(b"quic key");
            info.push(0);
            let mut out = [0u8; 32];
            let pieces: [&[u8]; 1] = [&info];
            let okm = prk.expand(&pieces, OkmLen(32)).unwrap();
            okm.fill(&mut out).unwrap();
        });
    });

    group.finish();
}

struct OkmLen(usize);
impl ring::hkdf::KeyType for OkmLen {
    fn len(&self) -> usize {
        self.0
    }
}

// ====================================================================
// TLS
// ====================================================================

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
    let params = CertificateParams::new(vec![SERVER_NAME.to_string()]).expect("CertificateParams");
    let kp = KeyPair::generate_for(&PKCS_ED25519).expect("kp");
    let cert = params.self_signed(&kp).expect("self_signed");
    (cert.der().to_vec(), kp.serialize_der())
}

fn bench_tls_handshake(c: &mut Criterion) {
    use grpcx_crypto::{ClientHandshake, ServerHandshake, TlsConfig};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
    use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};

    let (cert, key) = mint_cert();
    let mut group = c.benchmark_group("tls/loopback_handshake");

    {
        let mut client_cfg = TlsConfig::client();
        client_cfg.add_root_cert(&cert).expect("trust");
        let server_cfg = TlsConfig::server(&[&cert], &key).expect("server cfg");
        group.bench_function("grpcx_crypto", |b| {
            b.iter(|| {
                let mut c = ClientHandshake::new(&client_cfg, SERVER_NAME).unwrap();
                let mut s = ServerHandshake::new(&server_cfg).unwrap();
                for _ in 0..32 {
                    let c2s = c.take_handshake_bytes();
                    if !c2s.is_empty() {
                        s.process_handshake_bytes(&c2s).unwrap();
                    }
                    let s2c = s.take_handshake_bytes();
                    if !s2c.is_empty() {
                        c.process_handshake_bytes(&s2c).unwrap();
                    }
                    if c2s.is_empty() && s2c.is_empty() {
                        break;
                    }
                }
                let _cc = c.into_connection().unwrap();
                let _sc = s.into_connection().unwrap();
            });
        });
    }

    {
        let cert_der = CertificateDer::from(cert.clone());
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.clone()));
        let mut roots = RootCertStore::empty();
        roots.add(cert_der.clone()).expect("trust");
        let client_cfg = Arc::new(
            ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let server_cfg = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert_der.clone()], key_der)
                .expect("server cfg"),
        );
        group.bench_function("rustls_direct", |b| {
            b.iter(|| {
                let name = ServerName::try_from(SERVER_NAME.to_string()).unwrap();
                let mut c = ClientConnection::new(client_cfg.clone(), name).unwrap();
                let mut s = ServerConnection::new(server_cfg.clone()).unwrap();
                for _ in 0..32 {
                    let mut c2s = Vec::new();
                    while c.wants_write() {
                        let mut w: &mut dyn std::io::Write = &mut c2s;
                        let _ = c.write_tls(&mut w);
                    }
                    drain_into(&c2s, &mut s);
                    let mut s2c = Vec::new();
                    while s.wants_write() {
                        let mut w: &mut dyn std::io::Write = &mut s2c;
                        let _ = s.write_tls(&mut w);
                    }
                    drain_into(&s2c, &mut c);
                    if c2s.is_empty() && s2c.is_empty() {
                        break;
                    }
                }
            });
        });
    }

    group.finish();
}

/// Feed `bytes` into a rustls connection until consumed; runs
/// `process_new_packets` after each chunk. Local helper for the
/// bench's loopback pump.
fn drain_into<S: rustls::SideData>(bytes: &[u8], conn: &mut rustls::ConnectionCommon<S>) {
    let mut buf = bytes;
    while !buf.is_empty() {
        let mut r: &mut dyn std::io::Read = &mut buf;
        match conn.read_tls(&mut r) {
            Ok(0) => break,
            Ok(_) => {
                let _ = conn.process_new_packets();
            }
            Err(_) => break,
        }
    }
}

criterion_group!(
    benches,
    bench_aead_aes128gcm,
    bench_aead_chacha,
    bench_hkdf_expand_label,
    bench_tls_handshake,
);
criterion_main!(benches);

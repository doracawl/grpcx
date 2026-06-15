//! Fuzz-only internals (unstable, `#[doc(hidden)]`). Mirrors L1 /
//! L2's pattern: the fuzz targets live in the `fuzz/` subcrate and
//! can only see the public surface, so the byte-driven op loops
//! live here. NOT a stable API; may change or vanish without
//! notice.

use crate::{
    Aead, Aes128Gcm, Aes256Gcm, ChaCha20Poly1305, ClientHandshake, Hkdf, ServerHandshake, Sha256,
    TlsConfig,
};
use alloc::vec::Vec;

extern crate std;

/// Drive `Aead::open_in_place` with arbitrary bytes per suite.
/// **Invariant**: no input must trigger a panic — only `Err`
/// returns (or `Ok` for a one-in-a-trillion forged tag, which is
/// fine as long as it doesn't panic).
///
/// Byte script: byte 0 picks the suite (3 options); byte 1 is the
/// nonce length offset; the rest is `(nonce || aad_len_u8 || aad
/// || ciphertext_with_tag)`. Both length checks and bad-tag
/// arms are exercised.
pub fn drive_aead_open(data: &[u8]) {
    if data.len() < 4 {
        return;
    }
    let suite = data[0] % 3;
    let nonce_len_offset = (data[1] % 4) as usize; // 0..3 mismatches against 12
    let aad_len = data[2] as usize;
    let rest = &data[3..];
    if rest.len() < aad_len {
        return;
    }
    let aad = &rest[..aad_len];
    let mut io: Vec<u8> = rest[aad_len..].to_vec();
    let nonce_len = 12usize.saturating_sub(nonce_len_offset);
    let mut nonce = alloc::vec![0u8; nonce_len];
    for (i, slot) in nonce.iter_mut().enumerate() {
        *slot = data[i % data.len()];
    }
    let key128 = [0x42u8; 16];
    let key256 = [0x42u8; 32];
    match suite {
        0 => {
            if let Ok(a) = Aes128Gcm::new(&key128) {
                let _ = a.open_in_place(&nonce, aad, &mut io);
            }
        }
        1 => {
            if let Ok(a) = Aes256Gcm::new(&key256) {
                let _ = a.open_in_place(&nonce, aad, &mut io);
            }
        }
        _ => {
            if let Ok(a) = ChaCha20Poly1305::new(&key256) {
                let _ = a.open_in_place(&nonce, aad, &mut io);
            }
        }
    }
}

/// Drive `Hkdf::expand_label` with arbitrary label + context. Must
/// not panic on any input within length-prefix range.
///
/// Byte script: byte 0 = label length (capped at 240); next bytes
/// = label; then byte = context length (capped at 240); then
/// context. Remainder unused.
pub fn drive_hkdf_expand_label(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let label_len = (data[0] as usize).min(240);
    if data.len() < 1 + label_len + 1 {
        return;
    }
    let label = &data[1..1 + label_len];
    let context_len = (data[1 + label_len] as usize).min(240);
    let context_start = 1 + label_len + 1;
    if data.len() < context_start + context_len {
        return;
    }
    let context = &data[context_start..context_start + context_len];
    let hkdf = Hkdf::<Sha256>::extract(&[1u8; 16], &[2u8; 32]);
    let mut out = [0u8; 64];
    let _ = hkdf.expand_label(label, context, &mut out);
}

/// Drive a TLS 1.3 client-side handshake feed with arbitrary
/// bytes. Opens a fresh `ClientHandshake` against a caller-
/// provided self-signed cert + private key, then feeds `data` as
/// inbound bytes. Must not panic; rustls must always surface a
/// typed error.
///
/// Cert + key are passed in (not minted here) so this helper has
/// no dev-only deps — the fuzz target owns the cert generation
/// (typically once at startup) and amortises it across iterations.
pub fn drive_tls_handshake_feed(cert_der: &[u8], data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let mut cfg = TlsConfig::client();
    if cfg.add_root_cert(cert_der).is_err() {
        return;
    }
    let mut client = match ClientHandshake::new(&cfg, "loopback.example") {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = client.take_handshake_bytes();
    let _ = client.process_handshake_bytes(data);
}

/// Drive a TLS 1.3 server-side handshake feed (parallel to
/// `drive_tls_handshake_feed`'s client). Together the two cover
/// rustls's parser on both sides of the connection.
pub fn drive_tls_server_feed(cert_der: &[u8], private_key_pkcs8_der: &[u8], data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let cfg = match TlsConfig::server(&[cert_der], private_key_pkcs8_der) {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut server = match ServerHandshake::new(&cfg) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = server.process_handshake_bytes(data);
    let _ = server.take_handshake_bytes();
}

#[cfg(test)]
mod tests {
    use super::{
        drive_aead_open, drive_hkdf_expand_label, drive_tls_handshake_feed, drive_tls_server_feed,
    };

    /// A spread of byte values hits each driver's branches under
    /// plain `cargo test` (no nightly required) and counts toward
    /// coverage.
    fn spread() -> alloc::vec::Vec<u8> {
        (0u16..512).map(|i| i as u8).collect()
    }

    #[test]
    fn drive_aead_open_smoke() {
        drive_aead_open(&spread());
        drive_aead_open(&[]);
        drive_aead_open(&[0]);
    }

    #[test]
    fn drive_hkdf_expand_label_smoke() {
        drive_hkdf_expand_label(&spread());
        drive_hkdf_expand_label(&[]);
        drive_hkdf_expand_label(&[0]);
    }

    #[test]
    fn drive_tls_handshake_feed_smoke() {
        // Bogus cert bytes — exercises the early-return path in
        // `add_root_cert`. Real cert minting belongs in the fuzz
        // target (rcgen is dev-dep only, not a src/ runtime dep).
        drive_tls_handshake_feed(&[0xff; 4], &spread());
        drive_tls_handshake_feed(&[0xff; 4], &[]);
        drive_tls_handshake_feed(&[0xff; 4], &[0; 100]);
    }

    #[test]
    fn drive_tls_server_feed_smoke() {
        drive_tls_server_feed(&[0xff; 4], &[0xff; 4], &spread());
        drive_tls_server_feed(&[0xff; 4], &[0xff; 4], &[]);
    }
}

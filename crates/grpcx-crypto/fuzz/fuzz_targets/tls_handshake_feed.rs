//! Fuzz rustls's inbound parser through the wrapper's
//! `ClientHandshake::process_handshake_bytes` and
//! `ServerHandshake::process_handshake_bytes`. A single
//! self-signed cert is minted at process start (once via
//! `OnceLock`) and reused across iterations — the rcgen cost is
//! amortised; per-iter cost stays close to the rustls parse work.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

fn loopback_cert() -> &'static (Vec<u8>, Vec<u8>) {
    static CERT: OnceLock<(Vec<u8>, Vec<u8>)> = OnceLock::new();
    CERT.get_or_init(|| {
        use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
        let params =
            CertificateParams::new(vec!["loopback.example".to_string()]).expect("params");
        let kp = KeyPair::generate_for(&PKCS_ED25519).expect("kp");
        let cert = params.self_signed(&kp).expect("self_signed");
        (cert.der().to_vec(), kp.serialize_der())
    })
}

fuzz_target!(|data: &[u8]| {
    let (cert_der, key_der) = loopback_cert();
    // Alternate client + server feeds based on the high bit of byte 0
    // (or fall through if data is empty).
    if data.is_empty() {
        return;
    }
    if data[0] & 0x80 == 0 {
        grpcx_crypto::__fuzz_helpers::drive_tls_handshake_feed(cert_der, &data[1..]);
    } else {
        grpcx_crypto::__fuzz_helpers::drive_tls_server_feed(cert_der, key_der, &data[1..]);
    }
});

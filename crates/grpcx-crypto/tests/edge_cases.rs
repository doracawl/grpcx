//! Edge-case tests added in v0.6 to push coverage ≥ 95 %.
//!
//! These exercise the public-surface arms that v0.2-v0.5's KAT
//! tests didn't reach — bad-length AEAD seal/open, premature
//! `into_connection`, client/server config mismatches, the
//! `Kx::public_key` accessor on P-256 / P-384, etc.

use grpcx_crypto::{
    Aead, AeadError, Aes128Gcm, ClientHandshake, EcdsaP256, Kx, P256, P384, RsaPkcs1, RsaPss,
    ServerHandshake, Sig, SigError, TlsConfig, TlsError,
};

#[test]
fn aead_seal_short_buf_rejects() {
    let aead = Aes128Gcm::new(&[0u8; 16]).expect("new");
    let nonce = [0u8; 12];
    // plaintext_len = 10 but buffer only has 10 bytes (no tag scratch).
    let mut buf = vec![0u8; 10];
    assert_eq!(
        aead.seal_in_place(&nonce, &[], &mut buf, 10),
        Err(AeadError::BadLength)
    );
}

#[test]
fn aead_open_short_buf_rejects() {
    let aead = Aes128Gcm::new(&[0u8; 16]).expect("new");
    let nonce = [0u8; 12];
    // Ciphertext shorter than the tag.
    let mut buf = vec![0u8; 8];
    assert_eq!(
        aead.open_in_place(&nonce, &[], &mut buf),
        Err(AeadError::BadLength)
    );
}

#[test]
fn aead_bad_nonce_len_rejects() {
    let aead = Aes128Gcm::new(&[0u8; 16]).expect("new");
    // 8-byte nonce instead of 12.
    let nonce = [0u8; 8];
    let mut buf = vec![0u8; 32];
    assert_eq!(
        aead.seal_in_place(&nonce, &[], &mut buf, 16),
        Err(AeadError::BadLength)
    );
    assert_eq!(
        aead.open_in_place(&nonce, &[], &mut buf),
        Err(AeadError::BadLength)
    );
}

#[test]
fn p256_public_key_idempotent() {
    let (a, emit) = P256::keypair().expect("keypair");
    assert_eq!(a.public_key(), emit);
    assert_eq!(a.public_key(), emit);
}

#[test]
fn p384_public_key_idempotent() {
    let (a, emit) = P384::keypair().expect("keypair");
    assert_eq!(a.public_key(), emit);
    assert_eq!(a.public_key(), emit);
}

#[test]
fn ecdsa_p256_bad_public_rejects() {
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
    let rng = SystemRandom::new();
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).expect("gen pkcs8");
    let sig = EcdsaP256::sign(pkcs8.as_ref(), b"hi").expect("sign");
    // Wrong-length public key.
    assert_eq!(
        EcdsaP256::verify(&[0u8; 8], b"hi", &sig).err(),
        Some(SigError::BadSignature)
    );
}

#[test]
fn tlsconfig_client_using_handshake_with_server_config_errors() {
    // Use a server cfg in `ClientHandshake::new` → BadConfig.
    let (cert_pem, key_pem) = mint_cert();
    let server_cfg = TlsConfig::server(&[&cert_pem], &key_pem).expect("server cfg");
    let r = ClientHandshake::new(&server_cfg, "loopback.example");
    assert_eq!(r.err(), Some(TlsError::BadConfig));
}

#[test]
fn tlsconfig_server_using_handshake_with_client_config_errors() {
    let client_cfg = TlsConfig::client();
    let r = ServerHandshake::new(&client_cfg);
    assert_eq!(r.err(), Some(TlsError::BadConfig));
}

#[test]
fn tlsconfig_add_root_cert_on_server_errors() {
    let (cert_pem, key_pem) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_pem], &key_pem).expect("server cfg");
    assert_eq!(
        server_cfg.add_root_cert(&cert_pem),
        Err(TlsError::BadConfig)
    );
}

#[test]
fn tlsconfig_add_root_cert_bad_der_errors() {
    let mut client_cfg = TlsConfig::client();
    assert_eq!(client_cfg.add_root_cert(&[0xff; 4]), Err(TlsError::BadCert));
}

#[test]
fn premature_into_connection_errors() {
    let (cert_pem, _key_pem) = mint_cert();
    let mut client_cfg = TlsConfig::client();
    client_cfg.add_root_cert(&cert_pem).expect("trust");
    let client = ClientHandshake::new(&client_cfg, "loopback.example").expect("ch");
    assert_eq!(client.into_connection().err(), Some(TlsError::BadHandshake));
}

#[test]
fn tlsconfig_set_alpn_on_both_sides() {
    let (cert_pem, key_pem) = mint_cert();
    let mut server_cfg = TlsConfig::server(&[&cert_pem], &key_pem).expect("server cfg");
    server_cfg.set_alpn(&[b"h3", b"h2"]);

    let mut client_cfg = TlsConfig::client();
    client_cfg.set_alpn(&[b"h2"]);
    // Just ensure the setters take both shapes without panicking.
    client_cfg.set_alpn(&[]);
}

// RSA suites are documented as v0.4+ deferred. Confirm the
// unimplemented bodies panic with the documented message so any
// accidental "ship without impl" surfaces in tests, not in
// production callsites.
#[test]
#[should_panic(expected = "v0.3 — ring wrapper")]
fn rsa_pkcs1_sign_panics_until_implemented() {
    let _ = RsaPkcs1::sign(&[], b"x");
}

#[test]
#[should_panic(expected = "v0.3 — ring wrapper")]
fn rsa_pkcs1_verify_panics_until_implemented() {
    let _ = RsaPkcs1::verify(&[], b"x", &[]);
}

#[test]
#[should_panic(expected = "v0.3 — ring wrapper")]
fn rsa_pss_sign_panics_until_implemented() {
    let _ = RsaPss::sign(&[], b"x");
}

#[test]
#[should_panic(expected = "v0.3 — ring wrapper")]
fn rsa_pss_verify_panics_until_implemented() {
    let _ = RsaPss::verify(&[], b"x", &[]);
}

fn mint_cert() -> (Vec<u8>, Vec<u8>) {
    use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
    let params =
        CertificateParams::new(vec!["loopback.example".to_string()]).expect("CertificateParams");
    let kp = KeyPair::generate_for(&PKCS_ED25519).expect("kp");
    let cert = params.self_signed(&kp).expect("self_signed");
    (cert.der().to_vec(), kp.serialize_der())
}

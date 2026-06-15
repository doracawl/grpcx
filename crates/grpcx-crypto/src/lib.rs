#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Layout: the trait surface ([`Aead`], [`Kx`], [`Sig`], [`Hash`],
//! [`Hmac`], [`Hkdf`]) and the per-suite / per-curve / per-sig / per-
//! hash concrete types live at the crate root. The trait families
//! that v0.2 implements ([`Hash`], [`Hmac`], [`Hkdf`], [`Aead`]) are
//! sealed — external `impl` of these traits is not part of the
//! stable API; pick one of the shipped suites.
//!
//! The TLS 1.3 connection wrapper ([`TlsConfig`], [`ClientHandshake`],
//! [`ServerHandshake`], [`TlsConnection`], [`TlsError`]) and the QUIC
//! key derivation hooks ([`quic`]) land v0.4 + v0.5; bodies are
//! `unimplemented!()` until then.
//!
//! v0.2 wires the [`Hash`] / [`Hmac`] / [`Hkdf`] / [`Aead`] families
//! over `*ring*`. The trait API shape is **the final self-impl's
//! shape**; the wrapper hides ring behind it. When the self-
//! implementation lands (post external-cryptographer review),
//! swapping implementations is a drop-in. See
//! [`../../REFACTOR-v3-v0.1-bootstrap.md`](../../../REFACTOR-v3-v0.1-bootstrap.md)
//! and [`../../REFACTOR-v3-v0.2-aead-hash-hmac-hkdf.md`](../../../REFACTOR-v3-v0.2-aead-hash-hmac-hkdf.md).

extern crate alloc;

use alloc::vec::Vec;

#[doc(hidden)]
pub mod __fuzz_helpers;

pub mod quic_handshake;

mod sealed {
    /// Private super-trait: prevents external `impl` of [`super::Hash`]
    /// / [`super::Aead`]. The crate's own concrete types are the only
    /// allowed implementors.
    pub trait Sealed {}
}

// ============================================================
// AEAD — authenticated encryption with associated data.
// ============================================================

/// An AEAD suite (AES-GCM, ChaCha20-Poly1305).
///
/// Nonce and tag lengths are per-suite associated constants so the
/// type system prevents misuse (a wrong-length nonce is not
/// expressible). `seal_in_place` / `open_in_place` operate over a
/// mutable buffer that holds the plaintext on entry and the
/// ciphertext + tag on exit (encrypt) or the reverse (decrypt) —
/// zero alloc on the hot path.
///
/// Sealed — external `impl` is not part of the stable API.
pub trait Aead: Sized + sealed::Sealed {
    /// Length of the nonce, in bytes.
    const NONCE_LEN: usize;
    /// Length of the authentication tag, in bytes.
    const TAG_LEN: usize;
    /// Length of the key, in bytes.
    const KEY_LEN: usize;

    /// Initialise a sealing/opening state from a raw key.
    fn new(key: &[u8]) -> Result<Self, AeadError>;

    /// Encrypt `in_out` in place, appending the tag.
    ///
    /// On entry `in_out[..plaintext_len]` is the plaintext; the
    /// caller must have reserved `TAG_LEN` trailing scratch bytes so
    /// the tag has room to land. On success the buffer holds the
    /// ciphertext followed by the tag, and the returned `usize` is
    /// the total written length (`plaintext_len + TAG_LEN`).
    fn seal_in_place(
        &self,
        nonce: &[u8],
        aad: &[u8],
        in_out: &mut [u8],
        plaintext_len: usize,
    ) -> Result<usize, AeadError>;

    /// Decrypt `in_out` in place, verifying and stripping the tag.
    ///
    /// On entry `in_out` is the ciphertext + tag; on success the
    /// leading `in_out.len() - TAG_LEN` bytes are the plaintext and
    /// the returned `usize` is that plaintext length.
    fn open_in_place(
        &self,
        nonce: &[u8],
        aad: &[u8],
        in_out: &mut [u8],
    ) -> Result<usize, AeadError>;
}

/// Failure mode reported by an [`Aead`] op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeadError {
    /// Authentication tag did not verify.
    BadTag,
    /// Nonce / key / buffer length mismatch.
    BadLength,
    /// Suite-specific limit reached (e.g. AES-GCM message-count cap).
    Exhausted,
}

mod aead_impl {
    use super::{Aead, AeadError};
    use ring::aead;

    fn suite<S: Aead + 'static>() -> &'static aead::Algorithm {
        use core::any::TypeId;
        let t = TypeId::of::<S>();
        if t == TypeId::of::<super::Aes128Gcm>() {
            &aead::AES_128_GCM
        } else if t == TypeId::of::<super::Aes256Gcm>() {
            &aead::AES_256_GCM
        } else if t == TypeId::of::<super::ChaCha20Poly1305>() {
            &aead::CHACHA20_POLY1305
        } else {
            unreachable!("sealed Aead: only the three shipped suites are valid")
        }
    }

    /// Shared inner state for the three suites — one keyed
    /// `LessSafeKey` plus the per-suite algorithm reference, the
    /// `seal_in_place_separate_tag` + `open_in_place` plumbing, and
    /// the length checks. Each suite delegates here so the trait
    /// impls are one-liners.
    pub(super) struct KeyedAead {
        alg: &'static aead::Algorithm,
        key: aead::LessSafeKey,
    }

    impl KeyedAead {
        pub(super) fn new<S: Aead + 'static>(raw: &[u8]) -> Result<Self, AeadError> {
            let alg = suite::<S>();
            let unbound = aead::UnboundKey::new(alg, raw).map_err(|_| AeadError::BadLength)?;
            Ok(Self {
                alg,
                key: aead::LessSafeKey::new(unbound),
            })
        }

        pub(super) fn seal(
            &self,
            nonce: &[u8],
            aad: &[u8],
            in_out: &mut [u8],
            plaintext_len: usize,
        ) -> Result<usize, AeadError> {
            let tag_len = self.alg.tag_len();
            if nonce.len() != self.alg.nonce_len() || in_out.len() < plaintext_len + tag_len {
                return Err(AeadError::BadLength);
            }
            let mut nonce_bytes = [0u8; 32];
            nonce_bytes[..nonce.len()].copy_from_slice(nonce);
            let nonce_ref =
                aead::Nonce::try_assume_unique_for_key(&nonce_bytes[..self.alg.nonce_len()])
                    .map_err(|_| AeadError::BadLength)?;
            let aad_ref = aead::Aad::from(aad);
            let (ct, tag_dst) = in_out.split_at_mut(plaintext_len);
            let tag = self
                .key
                .seal_in_place_separate_tag(nonce_ref, aad_ref, ct)
                .map_err(|_| AeadError::Exhausted)?;
            tag_dst[..tag_len].copy_from_slice(tag.as_ref());
            Ok(plaintext_len + tag_len)
        }

        pub(super) fn open(
            &self,
            nonce: &[u8],
            aad: &[u8],
            in_out: &mut [u8],
        ) -> Result<usize, AeadError> {
            let tag_len = self.alg.tag_len();
            if nonce.len() != self.alg.nonce_len() || in_out.len() < tag_len {
                return Err(AeadError::BadLength);
            }
            let mut nonce_bytes = [0u8; 32];
            nonce_bytes[..nonce.len()].copy_from_slice(nonce);
            let nonce_ref =
                aead::Nonce::try_assume_unique_for_key(&nonce_bytes[..self.alg.nonce_len()])
                    .map_err(|_| AeadError::BadLength)?;
            let aad_ref = aead::Aad::from(aad);
            let pt = self
                .key
                .open_in_place(nonce_ref, aad_ref, in_out)
                .map_err(|_| AeadError::BadTag)?;
            Ok(pt.len())
        }
    }
}

impl sealed::Sealed for Aes128Gcm {}
impl sealed::Sealed for Aes256Gcm {}
impl sealed::Sealed for ChaCha20Poly1305 {}
impl sealed::Sealed for Sha256 {}
impl sealed::Sealed for Sha384 {}
impl sealed::Sealed for Sha512 {}
impl sealed::Sealed for X25519 {}
impl sealed::Sealed for P256 {}
impl sealed::Sealed for P384 {}
impl sealed::Sealed for Ed25519 {}
impl sealed::Sealed for EcdsaP256 {}
impl sealed::Sealed for RsaPkcs1 {}
impl sealed::Sealed for RsaPss {}

/// AEAD_AES_128_GCM — RFC 5116, 12-byte nonce, 16-byte tag, 16-byte key.
pub struct Aes128Gcm(aead_impl::KeyedAead);
impl Aead for Aes128Gcm {
    const NONCE_LEN: usize = 12;
    const TAG_LEN: usize = 16;
    const KEY_LEN: usize = 16;
    fn new(key: &[u8]) -> Result<Self, AeadError> {
        Ok(Self(aead_impl::KeyedAead::new::<Self>(key)?))
    }
    fn seal_in_place(
        &self,
        nonce: &[u8],
        aad: &[u8],
        in_out: &mut [u8],
        plaintext_len: usize,
    ) -> Result<usize, AeadError> {
        self.0.seal(nonce, aad, in_out, plaintext_len)
    }
    fn open_in_place(
        &self,
        nonce: &[u8],
        aad: &[u8],
        in_out: &mut [u8],
    ) -> Result<usize, AeadError> {
        self.0.open(nonce, aad, in_out)
    }
}

/// AEAD_AES_256_GCM — RFC 5116, 12-byte nonce, 16-byte tag, 32-byte key.
pub struct Aes256Gcm(aead_impl::KeyedAead);
impl Aead for Aes256Gcm {
    const NONCE_LEN: usize = 12;
    const TAG_LEN: usize = 16;
    const KEY_LEN: usize = 32;
    fn new(key: &[u8]) -> Result<Self, AeadError> {
        Ok(Self(aead_impl::KeyedAead::new::<Self>(key)?))
    }
    fn seal_in_place(
        &self,
        nonce: &[u8],
        aad: &[u8],
        in_out: &mut [u8],
        plaintext_len: usize,
    ) -> Result<usize, AeadError> {
        self.0.seal(nonce, aad, in_out, plaintext_len)
    }
    fn open_in_place(
        &self,
        nonce: &[u8],
        aad: &[u8],
        in_out: &mut [u8],
    ) -> Result<usize, AeadError> {
        self.0.open(nonce, aad, in_out)
    }
}

/// AEAD_CHACHA20_POLY1305 — RFC 8439, 12-byte nonce, 16-byte tag, 32-byte key.
pub struct ChaCha20Poly1305(aead_impl::KeyedAead);
impl Aead for ChaCha20Poly1305 {
    const NONCE_LEN: usize = 12;
    const TAG_LEN: usize = 16;
    const KEY_LEN: usize = 32;
    fn new(key: &[u8]) -> Result<Self, AeadError> {
        Ok(Self(aead_impl::KeyedAead::new::<Self>(key)?))
    }
    fn seal_in_place(
        &self,
        nonce: &[u8],
        aad: &[u8],
        in_out: &mut [u8],
        plaintext_len: usize,
    ) -> Result<usize, AeadError> {
        self.0.seal(nonce, aad, in_out, plaintext_len)
    }
    fn open_in_place(
        &self,
        nonce: &[u8],
        aad: &[u8],
        in_out: &mut [u8],
    ) -> Result<usize, AeadError> {
        self.0.open(nonce, aad, in_out)
    }
}

// ============================================================
// Kx — key exchange (ECDH).
// ============================================================

/// An ECDH curve (X25519, P-256, P-384).
///
/// `keypair` produces an ephemeral private + the encoded public.
/// `agree` consumes the private (`self`) — DH against a peer public
/// once, then drop. Consuming `self` enforces the forward-secrecy
/// "one DH per ephemeral key" contract at compile time.
///
/// Sealed — external `impl` is not part of the stable API.
pub trait Kx: Sized + 'static + sealed::Sealed {
    /// Length of the public key, in bytes.
    const PUBLIC_LEN: usize;
    /// Length of the agreed shared secret, in bytes.
    const SECRET_LEN: usize;
    /// Generate an ephemeral keypair, returning the private state +
    /// the encoded public key.
    fn keypair() -> Result<(Self, Vec<u8>), KxError>;
    /// Return the encoded public key for this private state.
    fn public_key(&self) -> Vec<u8>;
    /// Perform DH against `peer_public`, producing the shared secret.
    /// Consumes `self`: an ephemeral private key is used exactly once.
    fn agree(self, peer_public: &[u8]) -> Result<Vec<u8>, KxError>;
}

/// Failure mode reported by a [`Kx`] op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KxError {
    /// Peer public key was malformed or off-curve.
    BadPublicKey,
    /// Implementation-specific failure (entropy source, etc.).
    Internal,
}

mod kx_impl {
    use super::{Kx, KxError};
    use alloc::vec::Vec;
    use core::any::TypeId;
    use ring::agreement;
    use ring::rand::SystemRandom;

    pub(super) fn alg<K: Kx>() -> &'static agreement::Algorithm {
        let t = TypeId::of::<K>();
        if t == TypeId::of::<super::X25519>() {
            &agreement::X25519
        } else if t == TypeId::of::<super::P256>() {
            &agreement::ECDH_P256
        } else if t == TypeId::of::<super::P384>() {
            &agreement::ECDH_P384
        } else {
            unreachable!("sealed Kx: only X25519/P256/P384 are valid")
        }
    }

    /// Per-curve state — one ephemeral private key + its cached encoded
    /// public. Public is cached because ring derives it from the
    /// private each call; we want `public_key(&self)` to stay cheap and
    /// not allow `agree(self)` to consume the public side prematurely.
    pub(super) struct Ephemeral {
        pub(super) private: agreement::EphemeralPrivateKey,
        pub(super) public: Vec<u8>,
    }

    impl Ephemeral {
        pub(super) fn keypair<K: Kx>() -> Result<Self, KxError> {
            let rng = SystemRandom::new();
            let private = agreement::EphemeralPrivateKey::generate(alg::<K>(), &rng)
                .map_err(|_| KxError::Internal)?;
            let public = private
                .compute_public_key()
                .map_err(|_| KxError::Internal)?;
            Ok(Self {
                private,
                public: public.as_ref().to_vec(),
            })
        }

        pub(super) fn agree<K: Kx>(self, peer_public: &[u8]) -> Result<Vec<u8>, KxError> {
            if peer_public.len() != K::PUBLIC_LEN {
                return Err(KxError::BadPublicKey);
            }
            let peer = agreement::UnparsedPublicKey::new(alg::<K>(), peer_public);
            agreement::agree_ephemeral(self.private, &peer, |secret| secret.to_vec())
                .map_err(|_| KxError::BadPublicKey)
        }
    }
}

/// X25519 — RFC 7748, 32-byte public + 32-byte shared secret.
pub struct X25519(kx_impl::Ephemeral);
impl Kx for X25519 {
    const PUBLIC_LEN: usize = 32;
    const SECRET_LEN: usize = 32;
    fn keypair() -> Result<(Self, Vec<u8>), KxError> {
        let eph = kx_impl::Ephemeral::keypair::<Self>()?;
        let pub_bytes = eph.public.clone();
        Ok((Self(eph), pub_bytes))
    }
    fn public_key(&self) -> Vec<u8> {
        self.0.public.clone()
    }
    fn agree(self, peer_public: &[u8]) -> Result<Vec<u8>, KxError> {
        self.0.agree::<Self>(peer_public)
    }
}

/// NIST P-256 (secp256r1) — 65-byte uncompressed public + 32-byte shared secret.
pub struct P256(kx_impl::Ephemeral);
impl Kx for P256 {
    const PUBLIC_LEN: usize = 65;
    const SECRET_LEN: usize = 32;
    fn keypair() -> Result<(Self, Vec<u8>), KxError> {
        let eph = kx_impl::Ephemeral::keypair::<Self>()?;
        let pub_bytes = eph.public.clone();
        Ok((Self(eph), pub_bytes))
    }
    fn public_key(&self) -> Vec<u8> {
        self.0.public.clone()
    }
    fn agree(self, peer_public: &[u8]) -> Result<Vec<u8>, KxError> {
        self.0.agree::<Self>(peer_public)
    }
}

/// NIST P-384 (secp384r1) — 97-byte uncompressed public + 48-byte shared secret.
pub struct P384(kx_impl::Ephemeral);
impl Kx for P384 {
    const PUBLIC_LEN: usize = 97;
    const SECRET_LEN: usize = 48;
    fn keypair() -> Result<(Self, Vec<u8>), KxError> {
        let eph = kx_impl::Ephemeral::keypair::<Self>()?;
        let pub_bytes = eph.public.clone();
        Ok((Self(eph), pub_bytes))
    }
    fn public_key(&self) -> Vec<u8> {
        self.0.public.clone()
    }
    fn agree(self, peer_public: &[u8]) -> Result<Vec<u8>, KxError> {
        self.0.agree::<Self>(peer_public)
    }
}

// ============================================================
// Sig — digital signatures.
// ============================================================

/// A signature scheme (Ed25519, ECDSA, RSA).
///
/// Sealed — external `impl` is not part of the stable API.
///
/// Key encoding per scheme:
/// - [`Ed25519`]: `private_key` = 32-byte raw seed; `public_key` =
///   32-byte raw; signature = 64-byte raw.
/// - [`EcdsaP256`]: `private_key` = PKCS#8 v1 DER; `public_key` =
///   65-byte uncompressed SEC1 (`0x04` prefix); signature = 64-byte
///   fixed-length `r || s`.
/// - [`RsaPkcs1`] / [`RsaPss`]: deferred to v0.4 — `private_key` will
///   be PKCS#8 DER, `public_key` will be SPKI DER, signature size
///   depends on modulus.
pub trait Sig: sealed::Sealed {
    /// Sign `msg` with the private key bytes.
    fn sign(private_key: &[u8], msg: &[u8]) -> Result<Vec<u8>, SigError>;
    /// Verify `signature` over `msg` with the public key bytes.
    fn verify(public_key: &[u8], msg: &[u8], signature: &[u8]) -> Result<(), SigError>;
}

/// Failure mode reported by a [`Sig`] op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigError {
    /// Signature did not verify.
    BadSignature,
    /// Key was malformed.
    BadKey,
    /// Implementation-specific failure.
    Internal,
}

/// Ed25519 — RFC 8032, EdDSA over Curve25519.
///
/// `private_key` is a 32-byte raw seed; the wrapper derives the
/// signing key on each call. `public_key` is 32-byte raw, signature
/// is 64-byte raw.
pub struct Ed25519;
impl Sig for Ed25519 {
    fn sign(private_key: &[u8], msg: &[u8]) -> Result<Vec<u8>, SigError> {
        if private_key.len() != 32 {
            return Err(SigError::BadKey);
        }
        let kp = ring::signature::Ed25519KeyPair::from_seed_unchecked(private_key)
            .map_err(|_| SigError::BadKey)?;
        Ok(kp.sign(msg).as_ref().to_vec())
    }
    fn verify(public_key: &[u8], msg: &[u8], signature: &[u8]) -> Result<(), SigError> {
        if public_key.len() != 32 {
            return Err(SigError::BadKey);
        }
        let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key);
        key.verify(msg, signature)
            .map_err(|_| SigError::BadSignature)
    }
}

/// ECDSA over NIST P-256 with SHA-256.
///
/// `private_key` is PKCS#8 v1 DER (the format `openssl genpkey -algorithm EC`
/// emits). `public_key` is the 65-byte uncompressed SEC1 encoding
/// (`0x04 || X || Y`). Signature is the 64-byte fixed-length encoding
/// (`r || s`).
pub struct EcdsaP256;
impl Sig for EcdsaP256 {
    fn sign(private_key: &[u8], msg: &[u8]) -> Result<Vec<u8>, SigError> {
        let rng = ring::rand::SystemRandom::new();
        let kp = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            private_key,
            &rng,
        )
        .map_err(|_| SigError::BadKey)?;
        let sig = kp.sign(&rng, msg).map_err(|_| SigError::Internal)?;
        Ok(sig.as_ref().to_vec())
    }
    fn verify(public_key: &[u8], msg: &[u8], signature: &[u8]) -> Result<(), SigError> {
        let key = ring::signature::UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_FIXED,
            public_key,
        );
        key.verify(msg, signature)
            .map_err(|_| SigError::BadSignature)
    }
}

/// RSA-PKCS1-v1_5 with SHA-256 / 384 / 512.
pub struct RsaPkcs1;
impl Sig for RsaPkcs1 {
    fn sign(_private_key: &[u8], _msg: &[u8]) -> Result<Vec<u8>, SigError> {
        unimplemented!("v0.3 — ring wrapper")
    }
    fn verify(_public_key: &[u8], _msg: &[u8], _signature: &[u8]) -> Result<(), SigError> {
        unimplemented!("v0.3 — ring wrapper")
    }
}

/// RSA-PSS with SHA-256 / 384 / 512.
pub struct RsaPss;
impl Sig for RsaPss {
    fn sign(_private_key: &[u8], _msg: &[u8]) -> Result<Vec<u8>, SigError> {
        unimplemented!("v0.3 — ring wrapper")
    }
    fn verify(_public_key: &[u8], _msg: &[u8], _signature: &[u8]) -> Result<(), SigError> {
        unimplemented!("v0.3 — ring wrapper")
    }
}

// ============================================================
// Hash — incremental hashing with a stack-resident output.
// ============================================================

/// A hash function (SHA-256, SHA-384, SHA-512).
///
/// `update` feeds bytes incrementally; `finalize` moves out the
/// fixed-length output. `OUTPUT_LEN` is an associated const so the
/// output is a stack array, not a `Vec`.
///
/// Sealed (`'static + Sized + private`) — external `impl` is not part
/// of the stable API.
pub trait Hash: Sized + 'static + sealed::Sealed {
    /// Length of the hash output, in bytes.
    const OUTPUT_LEN: usize;
    /// Block length used by HMAC, in bytes.
    const BLOCK_LEN: usize;
    /// Initialise a fresh state.
    fn new() -> Self;
    /// Absorb `bytes` into the state.
    fn update(&mut self, bytes: &[u8]);
    /// Consume the state and emit the hash output into `out`.
    /// `out.len()` must equal `OUTPUT_LEN`.
    fn finalize(self, out: &mut [u8]);
}

mod hash_impl {
    use super::Hash;
    use core::any::TypeId;
    use ring::{digest, hkdf, hmac};

    pub(super) fn digest_alg<H: Hash>() -> &'static digest::Algorithm {
        let t = TypeId::of::<H>();
        if t == TypeId::of::<super::Sha256>() {
            &digest::SHA256
        } else if t == TypeId::of::<super::Sha384>() {
            &digest::SHA384
        } else if t == TypeId::of::<super::Sha512>() {
            &digest::SHA512
        } else {
            unreachable!("sealed Hash: only Sha256/Sha384/Sha512 are valid")
        }
    }

    pub(super) fn hmac_alg<H: Hash>() -> hmac::Algorithm {
        let t = TypeId::of::<H>();
        if t == TypeId::of::<super::Sha256>() {
            hmac::HMAC_SHA256
        } else if t == TypeId::of::<super::Sha384>() {
            hmac::HMAC_SHA384
        } else if t == TypeId::of::<super::Sha512>() {
            hmac::HMAC_SHA512
        } else {
            unreachable!("sealed Hash: only Sha256/Sha384/Sha512 are valid")
        }
    }

    pub(super) fn hkdf_alg<H: Hash>() -> hkdf::Algorithm {
        let t = TypeId::of::<H>();
        if t == TypeId::of::<super::Sha256>() {
            hkdf::HKDF_SHA256
        } else if t == TypeId::of::<super::Sha384>() {
            hkdf::HKDF_SHA384
        } else if t == TypeId::of::<super::Sha512>() {
            hkdf::HKDF_SHA512
        } else {
            unreachable!("sealed Hash: only Sha256/Sha384/Sha512 are valid")
        }
    }

    /// Per-hash state — one `digest::Context`. Each public hash type
    /// is a newtype over this, so the trait impls are one-liners.
    pub(super) struct Incremental {
        ctx: digest::Context,
    }

    impl Incremental {
        pub(super) fn new<H: Hash>() -> Self {
            Self {
                ctx: digest::Context::new(digest_alg::<H>()),
            }
        }
        pub(super) fn update(&mut self, bytes: &[u8]) {
            self.ctx.update(bytes);
        }
        pub(super) fn finalize<H: Hash>(self, out: &mut [u8]) {
            assert_eq!(out.len(), H::OUTPUT_LEN, "Hash::finalize: bad out len");
            let d = self.ctx.finish();
            out.copy_from_slice(d.as_ref());
        }
    }
}

/// SHA-256 — FIPS 180-4, 32-byte output, 64-byte block.
pub struct Sha256(hash_impl::Incremental);
impl Hash for Sha256 {
    const OUTPUT_LEN: usize = 32;
    const BLOCK_LEN: usize = 64;
    fn new() -> Self {
        Self(hash_impl::Incremental::new::<Self>())
    }
    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    fn finalize(self, out: &mut [u8]) {
        self.0.finalize::<Self>(out);
    }
}

/// SHA-384 — FIPS 180-4, 48-byte output, 128-byte block.
pub struct Sha384(hash_impl::Incremental);
impl Hash for Sha384 {
    const OUTPUT_LEN: usize = 48;
    const BLOCK_LEN: usize = 128;
    fn new() -> Self {
        Self(hash_impl::Incremental::new::<Self>())
    }
    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    fn finalize(self, out: &mut [u8]) {
        self.0.finalize::<Self>(out);
    }
}

/// SHA-512 — FIPS 180-4, 64-byte output, 128-byte block.
pub struct Sha512(hash_impl::Incremental);
impl Hash for Sha512 {
    const OUTPUT_LEN: usize = 64;
    const BLOCK_LEN: usize = 128;
    fn new() -> Self {
        Self(hash_impl::Incremental::new::<Self>())
    }
    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    fn finalize(self, out: &mut [u8]) {
        self.0.finalize::<Self>(out);
    }
}

// ============================================================
// HMAC + HKDF.
// ============================================================

/// HMAC over a [`Hash`] — RFC 2104. Generic on the hash so each
/// monomorphization is zero-alloc.
pub struct Hmac<H: Hash> {
    ctx: ring::hmac::Context,
    _marker: core::marker::PhantomData<H>,
}

impl<H: Hash> Hmac<H> {
    /// Initialise an HMAC state from a key.
    pub fn new(key: &[u8]) -> Self {
        let alg = hash_impl::hmac_alg::<H>();
        let k = ring::hmac::Key::new(alg, key);
        Self {
            ctx: ring::hmac::Context::with_key(&k),
            _marker: core::marker::PhantomData,
        }
    }
    /// Absorb `bytes`.
    pub fn update(&mut self, bytes: &[u8]) {
        self.ctx.update(bytes);
    }
    /// Emit the MAC into `out` (length == `H::OUTPUT_LEN`).
    pub fn sign(self, out: &mut [u8]) {
        assert_eq!(out.len(), H::OUTPUT_LEN, "Hmac::sign: bad out len");
        let tag = self.ctx.sign();
        out.copy_from_slice(tag.as_ref());
    }
    /// Verify `tag` matches the MAC in constant time.
    pub fn verify(self, tag: &[u8]) -> Result<(), HmacError> {
        if tag.len() != H::OUTPUT_LEN {
            return Err(HmacError::BadLength);
        }
        let computed = self.ctx.sign();
        if ct_eq(computed.as_ref(), tag) {
            Ok(())
        } else {
            Err(HmacError::BadMac)
        }
    }
}

/// Constant-time slice comparison. Standard subtle-style — accumulates
/// the bitwise difference into a single byte so the loop never short-
/// circuits on a mismatched position.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Failure mode reported by [`Hmac::verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HmacError {
    /// MAC did not verify.
    BadMac,
    /// Tag length mismatch.
    BadLength,
}

/// HKDF (Extract-then-Expand) over a [`Hash`] — RFC 5869. Generic on
/// the hash so each monomorphization is zero-alloc on the hot path.
pub struct Hkdf<H: Hash> {
    prk: ring::hkdf::Prk,
    _marker: core::marker::PhantomData<H>,
}

impl<H: Hash> Hkdf<H> {
    /// HKDF-Extract: `PRK = HMAC(salt, ikm)`.
    pub fn extract(salt: &[u8], ikm: &[u8]) -> Self {
        let alg = hash_impl::hkdf_alg::<H>();
        let salt = ring::hkdf::Salt::new(alg, salt);
        Self {
            prk: salt.extract(ikm),
            _marker: core::marker::PhantomData,
        }
    }

    /// Wrap raw PRK bytes as an `Hkdf<H>` instance — for the TLS 1.3
    /// / QUIC pattern where the secret is itself an HKDF output (a
    /// traffic secret derived from the master secret) and must serve
    /// as the PRK input to a further [`Self::expand_label`] without
    /// being re-extracted. Equivalent to ring's
    /// `hkdf::Prk::new_less_safe` — "less safe" because the bytes
    /// have not been validated as an HKDF output by this call.
    pub fn from_prk(secret: &[u8]) -> Self {
        let alg = hash_impl::hkdf_alg::<H>();
        Self {
            prk: ring::hkdf::Prk::new_less_safe(alg, secret),
            _marker: core::marker::PhantomData,
        }
    }

    /// HKDF-Expand: `OKM = T(1) || T(2) || ...` up to `out.len()`.
    pub fn expand(&self, info: &[u8], out: &mut [u8]) -> Result<(), HkdfError> {
        let info_pieces: [&[u8]; 1] = [info];
        let okm = self
            .prk
            .expand(&info_pieces, OkmLen(out.len()))
            .map_err(|_| HkdfError::OutputTooLong)?;
        okm.fill(out).map_err(|_| HkdfError::OutputTooLong)
    }

    /// HKDF-Expand-Label (TLS 1.3 / QUIC) — RFC 8446 §7.1.
    ///
    /// The expansion info is the concatenation of:
    /// `u16 out.len() || u8 label.len() || "tls13 " || label || u8 context.len() || context`.
    pub fn expand_label(
        &self,
        label: &[u8],
        context: &[u8],
        out: &mut [u8],
    ) -> Result<(), HkdfError> {
        const PREFIX: &[u8] = b"tls13 ";
        let total_label_len = PREFIX.len() + label.len();
        if out.len() > u16::MAX as usize
            || total_label_len > u8::MAX as usize
            || context.len() > u8::MAX as usize
        {
            return Err(HkdfError::OutputTooLong);
        }
        let mut info: Vec<u8> = Vec::with_capacity(2 + 1 + total_label_len + 1 + context.len());
        info.extend_from_slice(&(out.len() as u16).to_be_bytes());
        info.push(total_label_len as u8);
        info.extend_from_slice(PREFIX);
        info.extend_from_slice(label);
        info.push(context.len() as u8);
        info.extend_from_slice(context);
        self.expand(&info, out)
    }
}

/// Failure mode reported by an [`Hkdf`] op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HkdfError {
    /// `out` longer than 255 * HashLen (RFC 5869 §2.3) or label /
    /// context out of range for `expand_label`.
    OutputTooLong,
    /// Internal failure.
    Internal,
}

/// Newtype to satisfy ring's `KeyType` for raw-byte-length OKM.
struct OkmLen(usize);
impl ring::hkdf::KeyType for OkmLen {
    fn len(&self) -> usize {
        self.0
    }
}

// ============================================================
// TLS 1.3 connection wrapper.
// ============================================================

/// Configuration for a TLS 1.3 endpoint — cert chain, ALPN, root
/// trust store, etc.
///
/// Inner state is an `Arc`-shared rustls `ClientConfig` / `ServerConfig`.
/// Mutating setters (`set_alpn`, `add_root_cert`) clone-on-write via
/// `Arc::make_mut` so the same config can be reused across many
/// handshakes after setup.
pub struct TlsConfig {
    pub(crate) inner: tls_impl::ConfigInner,
}

impl TlsConfig {
    /// Create a fresh client config with an empty root trust store.
    ///
    /// The caller must either add OS roots (post-v0.4 helper) or feed
    /// trust anchors via [`Self::add_root_cert`] before the handshake;
    /// otherwise every server cert fails verification.
    pub fn client() -> Self {
        Self {
            inner: tls_impl::ConfigInner::new_client(),
        }
    }

    /// Create a fresh server config from a DER-encoded leaf-to-root
    /// chain and a PKCS#8 v1 DER private key. Returns
    /// `TlsError::BadCert` if rustls rejects the chain or
    /// `TlsError::BadConfig` if the private key cannot be parsed.
    pub fn server(cert_chain_der: &[&[u8]], private_key_der: &[u8]) -> Result<Self, TlsError> {
        Ok(Self {
            inner: tls_impl::ConfigInner::new_server(cert_chain_der, private_key_der)?,
        })
    }

    /// Set ALPN protocol identifiers, in preference order. Replaces
    /// any previously-set list.
    pub fn set_alpn(&mut self, protocols: &[&[u8]]) {
        self.inner.set_alpn(protocols);
    }

    /// Add a DER-encoded certificate to the client config's root
    /// trust store. **v3.x-only seam** — the final self-impl will
    /// inherit a different root-management API; pinned here so the
    /// loopback handshake test can trust a self-signed cert.
    pub fn add_root_cert(&mut self, cert_der: &[u8]) -> Result<(), TlsError> {
        self.inner.add_root_cert(cert_der)
    }
}

/// In-progress TLS 1.3 client handshake. Drives the handshake state
/// machine by alternately taking outbound handshake bytes and feeding
/// inbound ones until it transitions into a [`TlsConnection`].
pub struct ClientHandshake {
    inner: tls_impl::ConnInner,
}

impl ClientHandshake {
    /// Begin a handshake against `server_name`.
    pub fn new(config: &TlsConfig, server_name: &str) -> Result<Self, TlsError> {
        Ok(Self {
            inner: tls_impl::ConnInner::new_client(&config.inner, server_name)?,
        })
    }
    /// Drain pending outbound handshake bytes.
    pub fn take_handshake_bytes(&mut self) -> Vec<u8> {
        self.inner.take_wire_bytes()
    }
    /// Feed inbound handshake bytes from the peer.
    pub fn process_handshake_bytes(&mut self, bytes: &[u8]) -> Result<(), TlsError> {
        self.inner.feed_wire_bytes(bytes)
    }
    /// `true` once the handshake has completed —
    /// [`into_connection`](Self::into_connection) will succeed.
    /// (`into_connection` consumes self even on failure, so a
    /// network-driven loop needs this query to know when to
    /// promote.)
    pub fn is_complete(&self) -> bool {
        self.inner.is_complete()
    }
    /// If the handshake has completed, consume self and return the
    /// established connection.
    pub fn into_connection(self) -> Result<TlsConnection, TlsError> {
        self.inner.promote_to_connection()
    }
}

/// In-progress TLS 1.3 server handshake. Same shape as
/// [`ClientHandshake`].
pub struct ServerHandshake {
    inner: tls_impl::ConnInner,
}

impl ServerHandshake {
    /// Begin a server-side handshake.
    pub fn new(config: &TlsConfig) -> Result<Self, TlsError> {
        Ok(Self {
            inner: tls_impl::ConnInner::new_server(&config.inner)?,
        })
    }
    /// Drain pending outbound handshake bytes.
    pub fn take_handshake_bytes(&mut self) -> Vec<u8> {
        self.inner.take_wire_bytes()
    }
    /// Feed inbound handshake bytes from the peer.
    pub fn process_handshake_bytes(&mut self, bytes: &[u8]) -> Result<(), TlsError> {
        self.inner.feed_wire_bytes(bytes)
    }
    /// `true` once the handshake has completed —
    /// [`into_connection`](Self::into_connection) will succeed.
    pub fn is_complete(&self) -> bool {
        self.inner.is_complete()
    }
    /// If the handshake has completed, consume self and return the
    /// established connection.
    pub fn into_connection(self) -> Result<TlsConnection, TlsError> {
        self.inner.promote_to_connection()
    }
}

/// An established TLS 1.3 connection. Encrypts / decrypts
/// application data records over the negotiated cipher suite.
pub struct TlsConnection {
    inner: tls_impl::ConnInner,
}

impl TlsConnection {
    /// Encrypt one application-data record. Returns the wire bytes.
    pub fn encrypt_app_data(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, TlsError> {
        self.inner.encrypt_app_data(plaintext)
    }
    /// Decrypt one application-data record from the wire. Returns
    /// the decrypted plaintext (may be empty if rustls consumed
    /// internal records — e.g. a key-update message — without yielding
    /// application data).
    pub fn decrypt_app_data(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, TlsError> {
        self.inner.decrypt_app_data(ciphertext)
    }
    /// Force a key update (RFC 8446 §4.6.3).
    pub fn key_update(&mut self) -> Result<(), TlsError> {
        self.inner.key_update()
    }
    /// Return the ALPN protocol negotiated for this connection, if
    /// any. Available once the handshake has completed.
    pub fn alpn_protocol(&self) -> Option<Vec<u8>> {
        self.inner.alpn_protocol()
    }
}

/// Failure mode reported by a TLS op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsError {
    /// Record was malformed.
    BadRecord,
    /// Record's MAC did not verify.
    BadMac,
    /// Certificate chain was invalid.
    BadCert,
    /// Handshake message was invalid or out of order.
    BadHandshake,
    /// Configuration was invalid.
    BadConfig,
    /// Implementation-specific failure.
    Internal,
}

mod tls_impl {
    use super::TlsError;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
    use rustls::{
        ClientConfig, ClientConnection, ConnectionCommon, Error as RustlsError, RootCertStore,
        ServerConfig, ServerConnection,
    };
    use std::io::{Read, Write};

    /// `TlsConfig` inner. The client side carries the raw trust store
    /// and ALPN list so we can rebuild a fresh `Arc<ClientConfig>` per
    /// handshake (rustls's `ClientConfig` is immutable post-construction;
    /// rebuilding lets `add_root_cert` work as an ergonomic setter on
    /// the wrapper). The server side is `Arc<ServerConfig>` directly
    /// since ALPN is mutable in place via `Arc::make_mut` and the cert
    /// chain is set at construction.
    pub(super) enum ConfigInner {
        Client {
            roots: RootCertStore,
            alpn: Vec<Vec<u8>>,
        },
        Server(Arc<ServerConfig>),
    }

    impl ConfigInner {
        pub(super) fn new_client() -> Self {
            ConfigInner::Client {
                roots: RootCertStore::empty(),
                alpn: Vec::new(),
            }
        }

        pub(super) fn new_server(
            cert_chain_der: &[&[u8]],
            private_key_der: &[u8],
        ) -> Result<Self, TlsError> {
            let certs: Vec<CertificateDer<'static>> = cert_chain_der
                .iter()
                .map(|der| CertificateDer::from(der.to_vec()))
                .collect();
            let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(private_key_der.to_vec()));
            let config = ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .map_err(map_rustls_error)?;
            Ok(ConfigInner::Server(Arc::new(config)))
        }

        pub(super) fn set_alpn(&mut self, protocols: &[&[u8]]) {
            let alpn: Vec<Vec<u8>> = protocols.iter().map(|p| p.to_vec()).collect();
            match self {
                ConfigInner::Client { alpn: a, .. } => *a = alpn,
                ConfigInner::Server(c) => Arc::make_mut(c).alpn_protocols = alpn,
            }
        }

        pub(super) fn add_root_cert(&mut self, cert_der: &[u8]) -> Result<(), TlsError> {
            match self {
                ConfigInner::Client { roots, .. } => {
                    let cert = CertificateDer::from(cert_der.to_vec());
                    roots.add(cert).map_err(|_| TlsError::BadCert)
                }
                ConfigInner::Server(_) => Err(TlsError::BadConfig),
            }
        }

        /// Build a fresh client config from the stored roots + ALPN.
        /// Called once per handshake on the client side.
        pub(crate) fn build_client_arc(&self) -> Result<Arc<ClientConfig>, TlsError> {
            self.build_client()
        }

        /// Crate-private accessor used by the `quic_handshake` module.
        pub(crate) fn server_arc_crate(&self) -> Result<Arc<ServerConfig>, TlsError> {
            self.server_arc()
        }

        fn build_client(&self) -> Result<Arc<ClientConfig>, TlsError> {
            match self {
                ConfigInner::Client { roots, alpn } => {
                    let mut config = ClientConfig::builder()
                        .with_root_certificates(roots.clone())
                        .with_no_client_auth();
                    if !alpn.is_empty() {
                        config.alpn_protocols = alpn.clone();
                    }
                    Ok(Arc::new(config))
                }
                ConfigInner::Server(_) => Err(TlsError::BadConfig),
            }
        }

        fn server_arc(&self) -> Result<Arc<ServerConfig>, TlsError> {
            match self {
                ConfigInner::Server(c) => Ok(c.clone()),
                ConfigInner::Client { .. } => Err(TlsError::BadConfig),
            }
        }
    }

    /// Underlying rustls connection — boxed so the wrapper is one
    /// pointer wide and the enum discriminant stays cheap.
    pub(super) enum ConnInner {
        Client(Box<ClientConnection>),
        Server(Box<ServerConnection>),
    }

    impl ConnInner {
        pub(super) fn new_client(
            config: &ConfigInner,
            server_name: &str,
        ) -> Result<Self, TlsError> {
            let client = config.build_client()?;
            let name =
                ServerName::try_from(server_name.to_string()).map_err(|_| TlsError::BadConfig)?;
            let conn = ClientConnection::new(client, name).map_err(map_rustls_error)?;
            Ok(ConnInner::Client(Box::new(conn)))
        }

        pub(super) fn new_server(config: &ConfigInner) -> Result<Self, TlsError> {
            let server = config.server_arc()?;
            let conn = ServerConnection::new(server).map_err(map_rustls_error)?;
            Ok(ConnInner::Server(Box::new(conn)))
        }

        /// Drain any pending TLS records the rustls state machine
        /// wants to send (handshake messages and / or app-data writes
        /// queued via `Writer`). Returns the bytes in wire order.
        pub(super) fn take_wire_bytes(&mut self) -> Vec<u8> {
            let mut out: Vec<u8> = Vec::new();
            self.with_conn_mut(|conn| {
                while conn.wants_write() {
                    let _ = conn.write_tls(&mut out);
                }
            });
            out
        }

        /// Feed `bytes` from the wire into the rustls state machine,
        /// then run one `process_new_packets`. Bytes are read in one
        /// call (rustls handles partial records internally on the next
        /// feed).
        pub(super) fn feed_wire_bytes(&mut self, mut bytes: &[u8]) -> Result<(), TlsError> {
            self.with_conn_mut(|conn| {
                while !bytes.is_empty() {
                    let n = conn.read_tls(&mut bytes).map_err(|_| TlsError::BadRecord)?;
                    if n == 0 {
                        break;
                    }
                    conn.process_new_packets().map_err(map_rustls_error)?;
                }
                Ok(())
            })
        }

        pub(super) fn is_complete(&self) -> bool {
            !match self {
                ConnInner::Client(c) => c.is_handshaking(),
                ConnInner::Server(c) => c.is_handshaking(),
            }
        }

        pub(super) fn promote_to_connection(self) -> Result<super::TlsConnection, TlsError> {
            if !self.is_complete() {
                return Err(TlsError::BadHandshake);
            }
            Ok(super::TlsConnection { inner: self })
        }

        pub(super) fn encrypt_app_data(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, TlsError> {
            // Interleave plaintext writes with wire drains:
            // rustls caps its sendable-plaintext buffer
            // (`set_buffer_limit`, default 64 KiB) — a
            // single `write_all` of a >=64 KiB chunk fills
            // the buffer and FAILS unless `write_tls` is
            // drained between writes. Found by the
            // tonic-TLS interop gate (64 KiB unary);
            // small payloads never hit the cap.
            let mut wire: Vec<u8> = Vec::new();
            let mut off = 0;
            while off < plaintext.len() {
                let n = self.with_conn_mut(|conn| {
                    conn.writer()
                        .write(&plaintext[off..])
                        .map_err(|_| TlsError::Internal)
                })?;
                off += n;
                let chunk = self.take_wire_bytes();
                if n == 0 && chunk.is_empty() {
                    // No progress in either direction —
                    // genuinely stuck.
                    return Err(TlsError::Internal);
                }
                wire.extend_from_slice(&chunk);
            }
            wire.extend_from_slice(&self.take_wire_bytes());
            Ok(wire)
        }

        pub(super) fn decrypt_app_data(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, TlsError> {
            // Interleave wire feeds with plaintext drains —
            // the received-plaintext buffer is capped like
            // the send side, and `feed_wire_bytes`'s
            // break-on-zero would silently DROP the
            // unconsumed remainder of a large burst (the
            // receive-side twin of the encrypt cap, same
            // interop-gate finding).
            let mut bytes = ciphertext;
            let mut out: Vec<u8> = Vec::new();
            loop {
                let fed = self.with_conn_mut(|conn| {
                    let mut fed = 0usize;
                    while !bytes.is_empty() {
                        // A full deframer surfaces as a
                        // read_tls Err (not Ok(0)) — soft-
                        // break to the plaintext drain and
                        // resume; the no-progress guard
                        // below catches genuine failures.
                        let Ok(n) = conn.read_tls(&mut bytes) else {
                            break;
                        };
                        if n == 0 {
                            break;
                        }
                        fed += n;
                        conn.process_new_packets().map_err(map_rustls_error)?;
                    }
                    Ok::<usize, TlsError>(fed)
                })?;
                let drained = self.with_conn_mut(|conn| {
                    let mut drained = 0usize;
                    let mut buf = [0u8; 4096];
                    loop {
                        match conn.reader().read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                out.extend_from_slice(&buf[..n]);
                                drained += n;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(_) => return Err(TlsError::Internal),
                        }
                    }
                    Ok::<usize, TlsError>(drained)
                })?;
                if bytes.is_empty() {
                    break;
                }
                if fed == 0 && drained == 0 {
                    // Wire bytes left but no progress.
                    return Err(TlsError::BadRecord);
                }
            }
            Ok(out)
        }

        pub(super) fn key_update(&mut self) -> Result<(), TlsError> {
            self.with_conn_mut(|conn| conn.refresh_traffic_keys().map_err(map_rustls_error))
        }

        pub(super) fn alpn_protocol(&self) -> Option<Vec<u8>> {
            match self {
                ConnInner::Client(c) => c.alpn_protocol().map(|p| p.to_vec()),
                ConnInner::Server(c) => c.alpn_protocol().map(|p| p.to_vec()),
            }
        }

        fn with_conn_mut<R>(&mut self, f: impl FnOnce(&mut dyn ConnAccess) -> R) -> R {
            match self {
                ConnInner::Client(c) => f(&mut ClientAccess(c)),
                ConnInner::Server(c) => f(&mut ServerAccess(c)),
            }
        }
    }

    /// Common operations rustls's `ClientConnection` / `ServerConnection`
    /// share — `Connection` trait isn't quite enough (it gives us
    /// `is_handshaking` / `process_new_packets` / `wants_write` but not
    /// generic-`io` `read_tls` / `write_tls` / `writer` / `reader` /
    /// `refresh_traffic_keys` in one trait), so we surface them through
    /// our own erased trait that the worker closure receives.
    pub(super) trait ConnAccess {
        fn wants_write(&self) -> bool;
        fn write_tls(&mut self, out: &mut Vec<u8>) -> std::io::Result<usize>;
        fn read_tls(&mut self, reader: &mut &[u8]) -> std::io::Result<usize>;
        fn process_new_packets(&mut self) -> Result<rustls::IoState, RustlsError>;
        fn writer(&mut self) -> rustls::Writer<'_>;
        fn reader(&mut self) -> rustls::Reader<'_>;
        fn refresh_traffic_keys(&mut self) -> Result<(), RustlsError>;
    }

    struct ClientAccess<'a>(&'a mut Box<ClientConnection>);
    struct ServerAccess<'a>(&'a mut Box<ServerConnection>);

    macro_rules! impl_conn_access {
        ($ty:ident) => {
            impl ConnAccess for $ty<'_> {
                fn wants_write(&self) -> bool {
                    self.0.wants_write()
                }
                fn write_tls(&mut self, out: &mut Vec<u8>) -> std::io::Result<usize> {
                    let mut w: &mut dyn Write = out;
                    self.0.write_tls(&mut w)
                }
                fn read_tls(&mut self, reader: &mut &[u8]) -> std::io::Result<usize> {
                    let mut r: &mut dyn Read = reader;
                    self.0.read_tls(&mut r)
                }
                fn process_new_packets(&mut self) -> Result<rustls::IoState, RustlsError> {
                    self.0.process_new_packets()
                }
                fn writer(&mut self) -> rustls::Writer<'_> {
                    let conn: &mut ConnectionCommon<_> = &mut self.0;
                    conn.writer()
                }
                fn reader(&mut self) -> rustls::Reader<'_> {
                    let conn: &mut ConnectionCommon<_> = &mut self.0;
                    conn.reader()
                }
                fn refresh_traffic_keys(&mut self) -> Result<(), RustlsError> {
                    self.0.refresh_traffic_keys()
                }
            }
        };
    }
    impl_conn_access!(ClientAccess);
    impl_conn_access!(ServerAccess);

    pub(crate) fn map_rustls_error(err: RustlsError) -> TlsError {
        match err {
            RustlsError::InappropriateMessage { .. }
            | RustlsError::InappropriateHandshakeMessage { .. } => TlsError::BadHandshake,
            RustlsError::AlertReceived(_) => TlsError::BadHandshake,
            RustlsError::DecryptError => TlsError::BadMac,
            RustlsError::InvalidMessage(_) => TlsError::BadRecord,
            RustlsError::InvalidCertificate(_) | RustlsError::PeerIncompatible(_) => {
                TlsError::BadCert
            }
            _ => TlsError::Internal,
        }
    }
}

// ============================================================
// MEM tier — public-type size_of asserts + report.
// ============================================================

#[cfg(test)]
mod sizes {
    use super::{
        Aes128Gcm, ChaCha20Poly1305, ClientHandshake, EcdsaP256, Ed25519, Hkdf, Hmac, P256, P384,
        RsaPkcs1, RsaPss, ServerHandshake, Sha256, Sha384, Sha512, TlsConfig, TlsConnection,
        X25519,
    };
    use core::mem::size_of;

    // Shim / unit / one-pointer types — all zero-cost over their
    // ring backing where applicable.
    const _: () = assert!(size_of::<Sha256>() == size_of::<ring::digest::Context>());
    const _: () = assert!(size_of::<Sha384>() == size_of::<ring::digest::Context>());
    const _: () = assert!(size_of::<Sha512>() == size_of::<ring::digest::Context>());
    const _: () = assert!(size_of::<Ed25519>() == 0);
    const _: () = assert!(size_of::<EcdsaP256>() == 0);
    const _: () = assert!(size_of::<RsaPkcs1>() == 0);
    const _: () = assert!(size_of::<RsaPss>() == 0);

    #[test]
    fn report_internal_sizes() {
        // Captured at v0.6; mirrors grpcx-rt's pattern. Numbers are
        // platform-dependent for std-backed inner state; recorded
        // in BUDGETS.md, not const-asserted.
        std::eprintln!("size_of::<Sha256>()           = {}", size_of::<Sha256>());
        std::eprintln!("size_of::<Sha384>()           = {}", size_of::<Sha384>());
        std::eprintln!("size_of::<Sha512>()           = {}", size_of::<Sha512>());
        std::eprintln!(
            "size_of::<Hmac<Sha256>>()     = {}",
            size_of::<Hmac<Sha256>>()
        );
        std::eprintln!(
            "size_of::<Hkdf<Sha256>>()     = {}",
            size_of::<Hkdf<Sha256>>()
        );
        std::eprintln!("size_of::<Aes128Gcm>()        = {}", size_of::<Aes128Gcm>());
        std::eprintln!(
            "size_of::<ChaCha20Poly1305>() = {}",
            size_of::<ChaCha20Poly1305>()
        );
        std::eprintln!("size_of::<X25519>()           = {}", size_of::<X25519>());
        std::eprintln!("size_of::<P256>()             = {}", size_of::<P256>());
        std::eprintln!("size_of::<P384>()             = {}", size_of::<P384>());
        std::eprintln!("size_of::<TlsConfig>()        = {}", size_of::<TlsConfig>());
        std::eprintln!(
            "size_of::<ClientHandshake>()  = {}",
            size_of::<ClientHandshake>()
        );
        std::eprintln!(
            "size_of::<ServerHandshake>()  = {}",
            size_of::<ServerHandshake>()
        );
        std::eprintln!(
            "size_of::<TlsConnection>()    = {}",
            size_of::<TlsConnection>()
        );
    }
}

// ============================================================
// QUIC key derivation hooks — RFC 9001 §5. v0.5.
// ============================================================

/// QUIC initial-secret + header-protection + packet-key derivation
/// per RFC 9001 §5.
///
/// All ops are thin layers over [`Hkdf::expand_label`]; QUIC uses
/// the same `"tls13 "` HKDF-Expand-Label prefix as TLS 1.3 per
/// RFC 9001 §5.1.
///
/// **v3.x scope**: QUIC v1 only (the only version L4 v3.x targets).
/// SHA-256 only. QUIC v2 / SHA-384 traffic secrets surface as
/// additional entry points in a future release.
pub mod quic {
    use super::{Hash, Hkdf, HkdfError, Hmac, Sha256};
    use alloc::vec::Vec;

    /// QUIC v1 initial salt — RFC 9001 §5.2. 20 bytes, constant.
    pub const INITIAL_SALT_V1: [u8; 20] = [
        0x38, 0x76, 0x2c, 0xf7, 0xf5, 0x59, 0x34, 0xb3, 0x4d, 0x17, 0x9a, 0xe6, 0xa4, 0xc8, 0x0c,
        0xad, 0xcc, 0xbb, 0x7f, 0x0a,
    ];

    /// Derived key material for one QUIC encryption level — packet
    /// key, IV, header-protection key. All three lengths depend on
    /// the negotiated AEAD suite.
    pub struct PacketKeys {
        /// Symmetric AEAD key.
        pub key: Vec<u8>,
        /// Per-packet nonce base (XORed with packet number).
        pub iv: Vec<u8>,
        /// Header-protection mask key.
        pub hp: Vec<u8>,
    }

    /// QUIC initial secret per RFC 9001 §5.2: `HKDF-Extract(initial_salt_v1, client_dst_connection_id)`.
    /// Returns the 32-byte SHA-256 PRK bytes — the caller derives
    /// `{client,server}_initial_secret` from this PRK via
    /// HKDF-Expand-Label with labels `"client in"` / `"server in"`,
    /// e.g. via [`super::Hkdf::from_prk`].
    pub fn initial_secret(connection_id: &[u8]) -> Vec<u8> {
        // HKDF-Extract = HMAC(salt, ikm). Emit the raw HMAC output
        // (32 bytes for SHA-256) so the caller sees the same PRK
        // bytes RFC 9001 § Appendix A.1 quotes.
        let mut hmac = Hmac::<Sha256>::new(&INITIAL_SALT_V1);
        hmac.update(connection_id);
        let mut out = alloc::vec![0u8; <Sha256 as Hash>::OUTPUT_LEN];
        hmac.sign(&mut out);
        out
    }

    /// Derive the header-protection key from a traffic secret —
    /// `HKDF-Expand-Label(traffic_secret, "quic hp", "", key_len)`.
    /// The traffic secret is used directly as the PRK (it is already
    /// an HKDF output; not re-extracted).
    pub fn hp_secret(traffic_secret: &[u8], key_len: usize) -> Result<Vec<u8>, HkdfError> {
        let hkdf = Hkdf::<Sha256>::from_prk(traffic_secret);
        let mut out = alloc::vec![0u8; key_len];
        hkdf.expand_label(b"quic hp", &[], &mut out)?;
        Ok(out)
    }

    /// Derive packet key + IV + header-protection key from a traffic
    /// secret per the chosen AEAD suite. Three HKDF-Expand-Label
    /// calls with labels `"quic key"`, `"quic iv"`, `"quic hp"` and
    /// empty contexts (RFC 9001 §5.1). The traffic secret is used
    /// directly as the PRK (already an HKDF output; not re-extracted).
    pub fn key_iv_hp(
        traffic_secret: &[u8],
        key_len: usize,
        iv_len: usize,
        hp_len: usize,
    ) -> Result<PacketKeys, HkdfError> {
        let hkdf = Hkdf::<Sha256>::from_prk(traffic_secret);
        let mut key = alloc::vec![0u8; key_len];
        let mut iv = alloc::vec![0u8; iv_len];
        let mut hp = alloc::vec![0u8; hp_len];
        hkdf.expand_label(b"quic key", &[], &mut key)?;
        hkdf.expand_label(b"quic iv", &[], &mut iv)?;
        hkdf.expand_label(b"quic hp", &[], &mut hp)?;
        Ok(PacketKeys { key, iv, hp })
    }

    /// QUIC header-protection suite per RFC 9001 §5.4. Maps to the
    /// AEAD cipher suite negotiated for the encryption level.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum HpSuite {
        /// AES-128-ECB on the 16-byte sample (paired with AES-128-GCM).
        Aes128,
        /// AES-256-ECB on the 16-byte sample (paired with AES-256-GCM).
        Aes256,
        /// ChaCha20-block on the 16-byte sample (paired with
        /// ChaCha20-Poly1305).
        ChaCha20,
    }

    /// Failure mode reported by [`header_protect_mask`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum HpError {
        /// Bad HP key length or implementation-internal failure.
        BadKey,
    }

    /// Compute the 5-byte header-protection mask for one QUIC
    /// packet, per RFC 9001 §5.4. The caller XORs `mask[0]` (low
    /// 4 bits for long header, low 5 bits for short header) into
    /// the packet's first byte, and `mask[1..1+pn_len]` into the
    /// packet number bytes.
    ///
    /// `hp_key` is the HP key derived via
    /// [`key_iv_hp`]'s `hp` field. `sample` is the 16 bytes of
    /// (encrypted) packet starting at `pn_offset + 4`.
    pub fn header_protect_mask(
        suite: HpSuite,
        hp_key: &[u8],
        sample: &[u8; 16],
    ) -> Result<[u8; 5], HpError> {
        use ring::aead::quic;
        let alg = match suite {
            HpSuite::Aes128 => &quic::AES_128,
            HpSuite::Aes256 => &quic::AES_256,
            HpSuite::ChaCha20 => &quic::CHACHA20,
        };
        let key = quic::HeaderProtectionKey::new(alg, hp_key).map_err(|_| HpError::BadKey)?;
        key.new_mask(sample).map_err(|_| HpError::BadKey)
    }
}

//! QUIC packet protection — RFC 9001 §5.3 (AEAD) + §5.4 (header
//! protection).
//!
//! v0.3 lands the in-place primitives. The connection state
//! machine in v0.4 will compose them into the full send/receive
//! packet path. Zero allocation on the hot path (design intent
//! #1): the caller owns the packet buffer; we only mutate it.

use crate::Error;
use grpcx_crypto::quic::{HpSuite, header_protect_mask};
use grpcx_crypto::{Aead, Aes128Gcm, Aes256Gcm, ChaCha20Poly1305};

/// QUIC AEAD suite — paired with [`grpcx_crypto::quic::HpSuite`]
/// by the negotiated cipher suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeadSuite {
    /// AES-128-GCM + AES-128-ECB header protection.
    Aes128Gcm,
    /// AES-256-GCM + AES-256-ECB header protection.
    Aes256Gcm,
    /// ChaCha20-Poly1305 + ChaCha20 header protection.
    ChaCha20Poly1305,
}

impl AeadSuite {
    /// Matching header-protection suite.
    pub fn hp_suite(self) -> HpSuite {
        match self {
            AeadSuite::Aes128Gcm => HpSuite::Aes128,
            AeadSuite::Aes256Gcm => HpSuite::Aes256,
            AeadSuite::ChaCha20Poly1305 => HpSuite::ChaCha20,
        }
    }

    /// AEAD tag length (16 bytes for all three v1 suites).
    pub const fn tag_len(self) -> usize {
        16
    }
}

/// Derive the QUIC AEAD nonce per RFC 9001 §5.3: XOR the
/// right-aligned 64-bit packet number into the 12-byte IV.
fn quic_nonce(iv_base: &[u8], packet_number: u64) -> Result<[u8; 12], Error> {
    if iv_base.len() != 12 {
        return Err(Error::BadCrypto);
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(iv_base);
    let pn_be = packet_number.to_be_bytes();
    for i in 0..8 {
        nonce[4 + i] ^= pn_be[i];
    }
    Ok(nonce)
}

/// AEAD-seal the packet payload in place. On entry,
/// `in_out[..plaintext_len]` is the plaintext frames; the caller
/// must have reserved `AeadSuite::tag_len()` trailing scratch
/// bytes so the tag has room to land. `aad` is the unprotected
/// packet header.
///
/// Returns the total written length (`plaintext_len + TAG_LEN`).
pub fn protect_payload(
    suite: AeadSuite,
    key: &[u8],
    iv_base: &[u8],
    packet_number: u64,
    aad: &[u8],
    in_out: &mut [u8],
    plaintext_len: usize,
) -> Result<usize, Error> {
    let nonce = quic_nonce(iv_base, packet_number)?;
    let written = match suite {
        AeadSuite::Aes128Gcm => Aes128Gcm::new(key)
            .map_err(|_| Error::BadCrypto)?
            .seal_in_place(&nonce, aad, in_out, plaintext_len)
            .map_err(|_| Error::BadCrypto)?,
        AeadSuite::Aes256Gcm => Aes256Gcm::new(key)
            .map_err(|_| Error::BadCrypto)?
            .seal_in_place(&nonce, aad, in_out, plaintext_len)
            .map_err(|_| Error::BadCrypto)?,
        AeadSuite::ChaCha20Poly1305 => ChaCha20Poly1305::new(key)
            .map_err(|_| Error::BadCrypto)?
            .seal_in_place(&nonce, aad, in_out, plaintext_len)
            .map_err(|_| Error::BadCrypto)?,
    };
    Ok(written)
}

/// AEAD-open the packet payload in place. On entry, `in_out` is
/// the ciphertext followed by the 16-byte tag (`aad` is the
/// unprotected packet header). Returns the plaintext length.
pub fn unprotect_payload(
    suite: AeadSuite,
    key: &[u8],
    iv_base: &[u8],
    packet_number: u64,
    aad: &[u8],
    in_out: &mut [u8],
) -> Result<usize, Error> {
    let nonce = quic_nonce(iv_base, packet_number)?;
    let pt_len = match suite {
        AeadSuite::Aes128Gcm => Aes128Gcm::new(key)
            .map_err(|_| Error::BadCrypto)?
            .open_in_place(&nonce, aad, in_out)
            .map_err(|_| Error::BadCrypto)?,
        AeadSuite::Aes256Gcm => Aes256Gcm::new(key)
            .map_err(|_| Error::BadCrypto)?
            .open_in_place(&nonce, aad, in_out)
            .map_err(|_| Error::BadCrypto)?,
        AeadSuite::ChaCha20Poly1305 => ChaCha20Poly1305::new(key)
            .map_err(|_| Error::BadCrypto)?
            .open_in_place(&nonce, aad, in_out)
            .map_err(|_| Error::BadCrypto)?,
    };
    Ok(pt_len)
}

/// Sample 16 bytes for header protection — the 16 bytes at
/// `pn_offset + 4` (RFC 9001 §5.4.2). The caller must guarantee
/// the packet bytes past `pn_offset + 4 + 16` exist; we return
/// `BadCrypto` otherwise.
fn sample_hp_input(packet: &[u8], pn_offset: usize) -> Result<&[u8; 16], Error> {
    let start = pn_offset + 4;
    let end = start + 16;
    if packet.len() < end {
        return Err(Error::BadCrypto);
    }
    // SAFETY: bounds checked above; `try_into` validates length.
    <&[u8; 16]>::try_from(&packet[start..end]).map_err(|_| Error::BadCrypto)
}

/// Apply header protection to an outgoing packet. Caller has
/// already AEAD-sealed the payload (so the sample at
/// `pn_offset + 4` is the encrypted bytes). `pn_offset` is the
/// byte offset of the packet number; `pn_len` is its length
/// (1-4). `is_long` selects the first-byte mask width:
/// 4 bits long-header, 5 bits short-header.
pub fn apply_header_protection(
    suite: AeadSuite,
    hp_key: &[u8],
    packet: &mut [u8],
    pn_offset: usize,
    pn_len: usize,
    is_long: bool,
) -> Result<(), Error> {
    if !(1..=4).contains(&pn_len) {
        return Err(Error::BadCrypto);
    }
    if pn_offset + pn_len > packet.len() {
        return Err(Error::BadCrypto);
    }
    let sample = *sample_hp_input(packet, pn_offset)?;
    let mask =
        header_protect_mask(suite.hp_suite(), hp_key, &sample).map_err(|_| Error::BadCrypto)?;
    let first_mask = if is_long { 0x0f } else { 0x1f };
    packet[0] ^= mask[0] & first_mask;
    for i in 0..pn_len {
        packet[pn_offset + i] ^= mask[1 + i];
    }
    Ok(())
}

/// Remove header protection from an incoming packet. Samples
/// the (still-encrypted) bytes at `pn_offset + 4`, unmasks the
/// first byte to recover the true header bits, decodes
/// `pn_len = (first & 0x03) + 1`, then unmasks the PN bytes.
/// Returns the recovered `pn_len`.
pub fn remove_header_protection(
    suite: AeadSuite,
    hp_key: &[u8],
    packet: &mut [u8],
    pn_offset: usize,
    is_long: bool,
) -> Result<u8, Error> {
    let sample = *sample_hp_input(packet, pn_offset)?;
    let mask =
        header_protect_mask(suite.hp_suite(), hp_key, &sample).map_err(|_| Error::BadCrypto)?;
    let first_mask = if is_long { 0x0f } else { 0x1f };
    packet[0] ^= mask[0] & first_mask;
    let pn_len = (packet[0] & 0x03) + 1;
    if pn_offset + pn_len as usize > packet.len() {
        return Err(Error::BadCrypto);
    }
    for i in 0..pn_len as usize {
        packet[pn_offset + i] ^= mask[1 + i];
    }
    Ok(pn_len)
}

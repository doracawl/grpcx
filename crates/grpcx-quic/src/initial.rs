//! Initial-packet build + parse helpers — composes the v0.2-v0.4
//! primitives (header codec, frame codec, packet protection)
//! into a usable pair. The connection state machine in v0.6 will
//! build on these.
//!
//! Initial-level keys are derived per RFC 9001 §5.2: from the
//! client's destination Connection ID, both sides derive the same
//! 32-byte initial_secret, then HKDF-Expand-Label with `"client
//! in"` / `"server in"` to get the per-direction traffic secret,
//! then `quic::key_iv_hp` for the AEAD key + IV + HP key.
//!
//! v0.5 fixes pn_len at 4 bytes and the AEAD suite at AES-128-GCM
//! (RFC 9001 §5.1 mandates AES-128-GCM at the Initial level).

use crate::frame::{Frame, decode_frame, encode_frame};
use crate::packet::{LongHeader, LongHeaderType, PacketHeader, decode_header, encode_header};
use crate::protection::{
    AeadSuite, apply_header_protection, protect_payload, remove_header_protection,
    unprotect_payload,
};
use crate::varint::varint_len;
use crate::{ConnectionId, Error};
use alloc::vec::Vec;
use grpcx_crypto::{Hkdf, Sha256};

/// Which side of the connection this Initial packet originates
/// from. Picks the Initial-secret label fed into HKDF-Expand-Label
/// (RFC 9001 §5.2: `"client in"` vs `"server in"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Sent by the client (server uses these keys to decrypt).
    Client,
    /// Sent by the server (client uses these keys to decrypt).
    Server,
}

const TAG_LEN: usize = 16;
const PN_LEN: u8 = 4;
const KEY_LEN: usize = 16;
const IV_LEN: usize = 12;
const HP_LEN: usize = 16;
const AEAD: AeadSuite = AeadSuite::Aes128Gcm;

/// One side's Initial-level keys derived from the client's
/// destination Connection ID.
struct InitialKeys {
    key: Vec<u8>,
    iv: Vec<u8>,
    hp: Vec<u8>,
}

/// Derive the Initial-level key / IV / HP for one side from the
/// client's destination Connection ID.
fn derive_initial_keys(dcid: &[u8], side: Side) -> Result<InitialKeys, Error> {
    let prk = grpcx_crypto::quic::initial_secret(dcid);
    let hkdf = Hkdf::<Sha256>::from_prk(&prk);
    let mut traffic_secret = [0u8; 32];
    let label: &[u8] = match side {
        Side::Client => b"client in",
        Side::Server => b"server in",
    };
    hkdf.expand_label(label, &[], &mut traffic_secret)
        .map_err(|_| Error::BadCrypto)?;
    let keys = grpcx_crypto::quic::key_iv_hp(&traffic_secret, KEY_LEN, IV_LEN, HP_LEN)
        .map_err(|_| Error::BadCrypto)?;
    Ok(InitialKeys {
        key: keys.key,
        iv: keys.iv,
        hp: keys.hp,
    })
}

/// Build one Initial packet from a CRYPTO frame's bytes. Returns
/// the protected wire bytes (header + AEAD-sealed payload, with
/// header protection applied to the first byte + PN).
///
/// `padded_to` (when `> 0`) pads the encoded **plaintext** payload
/// with PADDING frames so the final protected packet matches the
/// requested wire length. RFC 9000 §14.1 requires the client's
/// first Initial flight to be at least 1200 bytes; this is the
/// hook for that.
pub fn build_initial_packet(
    dcid: &ConnectionId,
    scid: &ConnectionId,
    token: &[u8],
    packet_number: u32,
    crypto_bytes: &[u8],
    side: Side,
    padded_to: usize,
) -> Result<Vec<u8>, Error> {
    // Public API: packet DCID is also the Initial-key seed. This
    // matches the original client → server first-flight case
    // (which is what v0.5's KAT exercises). For the bidirectional
    // case (server → client uses a different on-wire DCID), the
    // connection-state-machine in v0.6 calls
    // [`build_initial_packet_with_initial_dcid`] directly.
    build_initial_packet_with_initial_dcid(
        dcid,
        scid,
        token,
        packet_number,
        crypto_bytes,
        side,
        padded_to,
        &dcid.bytes[..dcid.len as usize],
    )
}

/// Like [`build_initial_packet`], but takes the Initial-key seed
/// (the client's original destination CID, per RFC 9001 §5.2) as
/// an explicit parameter separate from the packet's on-wire DCID.
/// The wrap is needed once the server picks its own CID and the
/// peers re-address each other with different DCIDs — but both
/// sides must still derive Initial keys from the original client
/// CID.
#[allow(clippy::too_many_arguments)]
pub fn build_initial_packet_with_initial_dcid(
    dcid: &ConnectionId,
    scid: &ConnectionId,
    token: &[u8],
    packet_number: u32,
    crypto_bytes: &[u8],
    side: Side,
    padded_to: usize,
    initial_dcid_for_keys: &[u8],
) -> Result<Vec<u8>, Error> {
    let keys = derive_initial_keys(initial_dcid_for_keys, side)?;
    let (key, iv, hp_key) = (keys.key, keys.iv, keys.hp);

    // Build the plaintext payload: a CRYPTO frame at offset 0
    // carrying `crypto_bytes`.
    let mut plaintext: Vec<u8> = Vec::new();
    encode_frame(
        &Frame::Crypto {
            offset: 0,
            data: crypto_bytes.to_vec(),
        },
        &mut plaintext,
    )?;

    // Reserve room for padding.
    let plain_baseline = plaintext.len();
    let pn_overhead = PN_LEN as usize;

    // Compute the header size for a given `payload_len` (which is
    // the varint field that prefixes pn+payload+tag). Pick the
    // smallest viable header length: the length field is a varint
    // of `pn_len + plaintext_len + TAG_LEN`.
    let estimate_header_len = |payload_len_value: u64| -> usize {
        // 1 (first byte) + 4 (version) + 1 (dcid_len) + dcid_len
        // + 1 (scid_len) + scid_len + varint(token.len()) +
        // token.len() + varint(payload_len_value)
        1 + 4
            + 1
            + dcid.len as usize
            + 1
            + scid.len as usize
            + varint_len(token.len() as u64)
            + token.len()
            + varint_len(payload_len_value)
    };

    let mut total_plain = plain_baseline;
    if padded_to > 0 {
        // Iterate to find the right padding amount: the header
        // length depends on the payload length varint, which
        // depends on padding, which depends on header length…
        for _ in 0..4 {
            let payload_len_value = (pn_overhead + total_plain + TAG_LEN) as u64;
            let hdr = estimate_header_len(payload_len_value);
            let wire = hdr + pn_overhead + total_plain + TAG_LEN;
            if wire < padded_to {
                total_plain = padded_to - hdr - pn_overhead - TAG_LEN;
            } else {
                break;
            }
        }
        // Append PADDING frames to grow `plaintext` to `total_plain`.
        while plaintext.len() < total_plain {
            plaintext.push(0x00);
        }
    }

    let plaintext_len = plaintext.len();
    let payload_len_value = (pn_overhead + plaintext_len + TAG_LEN) as u64;

    // Build the unprotected header.
    let mut packet: Vec<u8> =
        Vec::with_capacity(estimate_header_len(payload_len_value) + plaintext_len + TAG_LEN);
    encode_header(
        &PacketHeader::Long(LongHeader {
            type_: LongHeaderType::Initial,
            version: 0x0000_0001,
            dcid: *dcid,
            scid: *scid,
            token: Some(token.to_vec()),
            payload_len: payload_len_value,
            packet_number,
            pn_len: PN_LEN,
        }),
        &mut packet,
    )?;
    let header_len = packet.len();
    let pn_offset = header_len - PN_LEN as usize;

    // Append plaintext + tag scratch and AEAD-seal in place.
    packet.extend_from_slice(&plaintext);
    packet.extend_from_slice(&[0u8; TAG_LEN]);
    let aad = packet[..header_len].to_vec();
    let payload_slice = &mut packet[header_len..];
    let written = protect_payload(
        AEAD,
        &key,
        &iv,
        packet_number as u64,
        &aad,
        payload_slice,
        plaintext_len,
    )?;
    debug_assert_eq!(written, plaintext_len + TAG_LEN);

    // Apply header protection (first byte's low 4 bits + PN).
    apply_header_protection(AEAD, &hp_key, &mut packet, pn_offset, PN_LEN as usize, true)?;

    Ok(packet)
}

/// Parse one Initial packet. `dcid_for_keys` is the client's
/// destination Connection ID used to derive Initial keys (RFC
/// 9001 §5.2 — both sides use the same value, regardless of which
/// direction the packet flows). `side` selects which side's keys
/// to derive (the packet's sender).
///
/// Returns the decoded `LongHeader` (with HP-unmasked first byte
/// and recovered PN) + the decoded frame list.
pub fn parse_initial_packet(
    packet: &mut [u8],
    dcid_for_keys: &[u8],
    side: Side,
) -> Result<(LongHeader, Vec<Frame>), Error> {
    let keys = derive_initial_keys(dcid_for_keys, side)?;
    let (key, iv, hp_key) = (keys.key, keys.iv, keys.hp);

    // The HP-protected fields are the first byte's low 4 bits and
    // the PN. Everything before the PN (version, dcid, scid, token,
    // length varint) is plain and can be decoded directly to find
    // pn_offset.
    //
    // We parse the header tentatively to locate pn_offset, then
    // HP-unmask, then re-read the now-recovered PN from the
    // unmasked bytes.
    let pn_offset = locate_pn_offset(packet)?;

    // HP-unmask: this flips the first byte's low 4 bits and the
    // PN bytes back to their plaintext values.
    let recovered_pn_len =
        remove_header_protection(AEAD, &hp_key, packet, pn_offset, true)? as usize;
    if recovered_pn_len != PN_LEN as usize {
        return Err(Error::BadPacket);
    }

    // Re-decode the header now that the first byte + PN are clean.
    let (header, header_len) = decode_header(packet, 0)?;
    debug_assert_eq!(header_len, pn_offset + PN_LEN as usize);
    let long = match header {
        PacketHeader::Long(h) => h,
        PacketHeader::Short(_) => return Err(Error::BadPacket),
    };
    if long.type_ != LongHeaderType::Initial {
        return Err(Error::BadPacket);
    }

    // AEAD-open the payload in place.
    let aad = packet[..header_len].to_vec();
    let payload_total = long.payload_len as usize - PN_LEN as usize;
    let payload_slice = &mut packet[header_len..header_len + payload_total];
    let plaintext_len = unprotect_payload(
        AEAD,
        &key,
        &iv,
        long.packet_number as u64,
        &aad,
        payload_slice,
    )?;

    // Decode the frames from the plaintext.
    let mut frames: Vec<Frame> = Vec::new();
    let mut cursor: &[u8] = &payload_slice[..plaintext_len];
    while !cursor.is_empty() {
        frames.push(decode_frame(&mut cursor)?);
    }

    Ok((long, frames))
}

/// Walk the unprotected header fields just far enough to compute
/// where the packet-number bytes start. Used by
/// [`parse_initial_packet`] before HP-unmasking the PN.
fn locate_pn_offset(packet: &[u8]) -> Result<usize, Error> {
    use crate::varint::read_varint;
    if packet.is_empty() {
        return Err(Error::BadPacket);
    }
    // first byte (1) + version (4) + dcid_len (1) + dcid + scid_len
    // (1) + scid + token_len_varint + token + payload_len_varint
    let mut idx = 1usize + 4;
    if packet.len() <= idx {
        return Err(Error::BadPacket);
    }
    let dcid_len = packet[idx] as usize;
    idx += 1 + dcid_len;
    if packet.len() <= idx {
        return Err(Error::BadPacket);
    }
    let scid_len = packet[idx] as usize;
    idx += 1 + scid_len;
    if packet.len() <= idx {
        return Err(Error::BadPacket);
    }

    let mut cursor: &[u8] = &packet[idx..];
    let token_len = read_varint(&mut cursor)? as usize;
    if cursor.len() < token_len {
        return Err(Error::BadPacket);
    }
    cursor = &cursor[token_len..];
    let _payload_len = read_varint(&mut cursor)?;
    Ok(packet.len() - cursor.len())
}

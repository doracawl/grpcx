// Crate-private RFC 6455 frame codec. v0.2 fills in the
// `encode_frame` / `decode_frame` entry points exported
// from `lib.rs`.
//
// See `../../REFACTOR-v9-v0.2-frame-codec.md` for design
// notes, the smallest-form enforcement rule, and the
// masking convention (`Frame.payload` is always the
// unmasked form; the codec applies/undoes masking on
// encode/decode).

use crate::{Frame, Opcode, WsError};
use alloc::vec::Vec;

// ============================================================
// Opcode pack / unpack.
// ============================================================

fn opcode_to_u8(op: Opcode) -> u8 {
    match op {
        Opcode::Continuation => 0x0,
        Opcode::Text => 0x1,
        Opcode::Binary => 0x2,
        Opcode::Close => 0x8,
        Opcode::Ping => 0x9,
        Opcode::Pong => 0xA,
    }
}

fn opcode_from_u8(n: u8) -> Result<Opcode, WsError> {
    match n {
        0x0 => Ok(Opcode::Continuation),
        0x1 => Ok(Opcode::Text),
        0x2 => Ok(Opcode::Binary),
        0x8 => Ok(Opcode::Close),
        0x9 => Ok(Opcode::Ping),
        0xA => Ok(Opcode::Pong),
        n => Err(WsError::InvalidOpcode(n)),
    }
}

// ============================================================
// Masking.
// ============================================================

fn apply_mask(payload: &mut [u8], key: [u8; 4]) {
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= key[i & 3];
    }
}

// ============================================================
// Encode.
// ============================================================

pub(crate) fn encode(frame: &Frame, out: &mut Vec<u8>) {
    let mut byte0: u8 = 0;
    if frame.fin {
        byte0 |= 0b1000_0000;
    }
    if frame.rsv1 {
        byte0 |= 0b0100_0000;
    }
    if frame.rsv2 {
        byte0 |= 0b0010_0000;
    }
    if frame.rsv3 {
        byte0 |= 0b0001_0000;
    }
    byte0 |= opcode_to_u8(frame.opcode);
    out.push(byte0);

    let mut byte1: u8 = 0;
    if frame.mask.is_some() {
        byte1 |= 0b1000_0000;
    }
    let len = frame.payload.len();
    if len <= 125 {
        byte1 |= len as u8;
        out.push(byte1);
    } else if len <= 0xFFFF {
        byte1 |= 126;
        out.push(byte1);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        byte1 |= 127;
        out.push(byte1);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }

    if let Some(key) = frame.mask {
        out.extend_from_slice(&key);
        let payload_start = out.len();
        out.extend_from_slice(&frame.payload);
        apply_mask(&mut out[payload_start..], key);
    } else {
        out.extend_from_slice(&frame.payload);
    }
}

// ============================================================
// Decode.
// ============================================================

pub(crate) fn decode(bytes: &[u8]) -> Result<(Frame, usize), WsError> {
    if bytes.len() < 2 {
        return Err(WsError::Truncated);
    }
    let byte0 = bytes[0];
    let byte1 = bytes[1];

    let fin = byte0 & 0b1000_0000 != 0;
    let rsv1 = byte0 & 0b0100_0000 != 0;
    let rsv2 = byte0 & 0b0010_0000 != 0;
    let rsv3 = byte0 & 0b0001_0000 != 0;
    if rsv1 || rsv2 || rsv3 {
        return Err(WsError::ReservedBitSet);
    }
    let opcode = opcode_from_u8(byte0 & 0x0F)?;

    let mask_bit = byte1 & 0b1000_0000 != 0;
    let length_field = byte1 & 0b0111_1111;

    let mut pos: usize = 2;
    let length: usize = match length_field {
        0..=125 => length_field as usize,
        126 => {
            if bytes.len() < pos + 2 {
                return Err(WsError::Truncated);
            }
            let v = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
            pos += 2;
            if v <= 125 {
                // overlong encoding
                return Err(WsError::PayloadTooLarge);
            }
            v
        }
        127 => {
            if bytes.len() < pos + 8 {
                return Err(WsError::Truncated);
            }
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&bytes[pos..pos + 8]);
            let v64 = u64::from_be_bytes(buf);
            pos += 8;
            if v64 & 0x8000_0000_0000_0000 != 0 {
                // MSB must be 0
                return Err(WsError::PayloadTooLarge);
            }
            if v64 <= 0xFFFF {
                // overlong encoding
                return Err(WsError::PayloadTooLarge);
            }
            let v: usize = v64.try_into().map_err(|_| WsError::PayloadTooLarge)?;
            v
        }
        // length_field is masked to 7 bits above, so all
        // remaining values 0..=127 are covered; this arm is
        // unreachable but keeps the match exhaustive.
        _ => return Err(WsError::PayloadTooLarge),
    };

    // Control-frame constraints (RFC 6455 § 5.5): FIN
    // must be 1, payload must be ≤ 125 bytes. Enforced at
    // the decoder; the encoder trusts the caller-provided
    // Frame.
    if opcode.is_control() {
        if !fin {
            return Err(WsError::FragmentedControlFrame);
        }
        if length > 125 {
            return Err(WsError::PayloadTooLarge);
        }
    }

    let mask = if mask_bit {
        if bytes.len() < pos + 4 {
            return Err(WsError::Truncated);
        }
        let key = [bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]];
        pos += 4;
        Some(key)
    } else {
        None
    };

    if bytes.len() < pos + length {
        return Err(WsError::Truncated);
    }
    let mut payload: Vec<u8> = bytes[pos..pos + length].to_vec();
    if let Some(key) = mask {
        apply_mask(&mut payload, key);
    }
    pos += length;

    Ok((
        Frame {
            fin,
            rsv1,
            rsv2,
            rsv3,
            opcode,
            mask,
            payload,
        },
        pos,
    ))
}

// ============================================================
// Unit tests — primitives + smoke. End-to-end KAT lives in
// `tests/kat_frame.rs`.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_round_trip() {
        for op in [
            Opcode::Continuation,
            Opcode::Text,
            Opcode::Binary,
            Opcode::Close,
            Opcode::Ping,
            Opcode::Pong,
        ] {
            assert_eq!(opcode_from_u8(opcode_to_u8(op)).unwrap(), op);
        }
    }

    #[test]
    fn opcode_rejects_reserved() {
        for n in [0x3, 0x4, 0x5, 0x6, 0x7, 0xB, 0xC, 0xD, 0xE, 0xF] {
            assert_eq!(opcode_from_u8(n).unwrap_err(), WsError::InvalidOpcode(n));
        }
    }

    #[test]
    fn apply_mask_xor_round_trip() {
        let key = [0x37, 0xFA, 0x21, 0x3D];
        let original = b"Hello".to_vec();
        let mut buf = original.clone();
        apply_mask(&mut buf, key);
        assert_ne!(buf, original);
        apply_mask(&mut buf, key);
        assert_eq!(buf, original);
    }
}

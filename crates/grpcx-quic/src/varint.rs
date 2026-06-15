//! RFC 9000 §16 variable-length integer encoding.
//!
//! Wire form: the top two bits of the first byte select length
//! (1 / 2 / 4 / 8 bytes); the remaining bits are the integer in
//! big-endian. The encoded representation is **not** unique — the
//! same value can be encoded in any sufficient length. Decoders
//! accept all valid encodings; encoders pick the shortest by
//! default (`write_varint`).

use crate::Error;

/// Maximum value representable as a 62-bit varint
/// (`(1 << 62) - 1`).
pub(crate) const MAX_VARINT: u64 = (1 << 62) - 1;

/// Encode `value` as the shortest valid varint into `out`, returning
/// the number of bytes written. Errors if `value > MAX_VARINT` or
/// `out` is too small.
pub(crate) fn write_varint(value: u64, out: &mut [u8]) -> Result<usize, Error> {
    if value > MAX_VARINT {
        return Err(Error::BadPacket);
    }
    if value < (1 << 6) {
        // 1-byte form: 00xxxxxx
        if out.is_empty() {
            return Err(Error::BadPacket);
        }
        out[0] = value as u8;
        Ok(1)
    } else if value < (1 << 14) {
        // 2-byte form: 01xxxxxx xxxxxxxx
        if out.len() < 2 {
            return Err(Error::BadPacket);
        }
        let v = (value as u16) | (0b01 << 14);
        out[..2].copy_from_slice(&v.to_be_bytes());
        Ok(2)
    } else if value < (1 << 30) {
        // 4-byte form: 10xxxxxx ...
        if out.len() < 4 {
            return Err(Error::BadPacket);
        }
        let v = (value as u32) | (0b10 << 30);
        out[..4].copy_from_slice(&v.to_be_bytes());
        Ok(4)
    } else {
        // 8-byte form: 11xxxxxx ...
        if out.len() < 8 {
            return Err(Error::BadPacket);
        }
        let v = value | (0b11 << 62);
        out[..8].copy_from_slice(&v.to_be_bytes());
        Ok(8)
    }
}

/// Decode a varint from the front of `buf`, advancing `buf` past
/// the consumed bytes. Returns the value or `Error::BadPacket` on
/// a truncated buffer.
pub(crate) fn read_varint(buf: &mut &[u8]) -> Result<u64, Error> {
    if buf.is_empty() {
        return Err(Error::BadPacket);
    }
    let prefix = buf[0] >> 6;
    let len = 1usize << prefix; // 1, 2, 4, or 8
    if buf.len() < len {
        return Err(Error::BadPacket);
    }
    // Right-align the encoded bytes into a u64 and clear the top
    // 2 prefix bits from the high byte.
    let mut bytes = [0u8; 8];
    bytes[8 - len..].copy_from_slice(&buf[..len]);
    bytes[8 - len] &= 0x3f;
    let value = u64::from_be_bytes(bytes);
    *buf = &buf[len..];
    Ok(value)
}

/// Return the number of bytes the shortest varint encoding of
/// `value` will occupy (without writing it). Useful for length
/// prefixing.
pub(crate) const fn varint_len(value: u64) -> usize {
    if value < (1 << 6) {
        1
    } else if value < (1 << 14) {
        2
    } else if value < (1 << 30) {
        4
    } else {
        8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc9000_a1_examples_decode() {
        // 1-byte form: 0x25 → 37
        let mut b: &[u8] = &[0x25];
        assert_eq!(read_varint(&mut b).unwrap(), 37);
        assert!(b.is_empty());

        // 2-byte form (redundant): 0x40 0x25 → 37
        let mut b: &[u8] = &[0x40, 0x25];
        assert_eq!(read_varint(&mut b).unwrap(), 37);

        // 2-byte form: 0x7b 0xbd → 15293
        let mut b: &[u8] = &[0x7b, 0xbd];
        assert_eq!(read_varint(&mut b).unwrap(), 15293);

        // 4-byte form: 0x9d 0x7f 0x3e 0x7d → 494878333
        let mut b: &[u8] = &[0x9d, 0x7f, 0x3e, 0x7d];
        assert_eq!(read_varint(&mut b).unwrap(), 494878333);

        // 8-byte form: 0xc2 0x19 0x7c 0x5e 0xff 0x14 0xe8 0x8c
        //            → 151288809941952652
        let mut b: &[u8] = &[0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c];
        assert_eq!(read_varint(&mut b).unwrap(), 151288809941952652);
    }

    #[test]
    fn shortest_form_round_trip() {
        for v in [
            0u64,
            1,
            63,
            64,
            16383,
            16384,
            (1 << 30) - 1,
            1 << 30,
            MAX_VARINT,
        ] {
            let mut buf = [0u8; 8];
            let n = write_varint(v, &mut buf).expect("encode");
            assert_eq!(n, varint_len(v), "varint_len mismatch for {v}");
            let mut cursor: &[u8] = &buf[..n];
            assert_eq!(read_varint(&mut cursor).unwrap(), v, "round-trip {v}");
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn truncated_buffer_rejected() {
        let mut b: &[u8] = &[0x40]; // 2-byte form, only 1 byte present
        assert_eq!(read_varint(&mut b), Err(Error::BadPacket));

        let mut b: &[u8] = &[];
        assert_eq!(read_varint(&mut b), Err(Error::BadPacket));
    }

    #[test]
    fn overlong_value_rejected_on_encode() {
        let mut buf = [0u8; 8];
        assert_eq!(
            write_varint(MAX_VARINT + 1, &mut buf),
            Err(Error::BadPacket)
        );
    }

    #[test]
    fn small_out_buf_rejected_on_encode() {
        let mut buf = [0u8; 1];
        assert_eq!(write_varint(64, &mut buf), Err(Error::BadPacket));
    }
}

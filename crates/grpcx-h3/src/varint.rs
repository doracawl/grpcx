//! RFC 9000 §16 variable-length integer encoding — duplicated
//! from L4's `grpcx-quic::varint` (`pub(crate)` over there).
//! Re-exporting across the L4 boundary purely to feed L5 would
//! bloat L4's public surface; the duplicated code is ~40 lines
//! of pure-function logic. v1.x consolidation candidate.

use crate::Error;

/// Maximum value representable as a 62-bit varint
/// (`(1 << 62) - 1`).
pub(crate) const MAX_VARINT: u64 = (1 << 62) - 1;

/// Encode `value` as the shortest valid varint into `out`,
/// returning the number of bytes written. Errors if
/// `value > MAX_VARINT` or `out` is too small.
pub(crate) fn write_varint(value: u64, out: &mut [u8]) -> Result<usize, Error> {
    if value > MAX_VARINT {
        return Err(Error::BadFrame);
    }
    if value < (1 << 6) {
        if out.is_empty() {
            return Err(Error::BadFrame);
        }
        out[0] = value as u8;
        Ok(1)
    } else if value < (1 << 14) {
        if out.len() < 2 {
            return Err(Error::BadFrame);
        }
        let v = (value as u16) | (0b01 << 14);
        out[..2].copy_from_slice(&v.to_be_bytes());
        Ok(2)
    } else if value < (1 << 30) {
        if out.len() < 4 {
            return Err(Error::BadFrame);
        }
        let v = (value as u32) | (0b10 << 30);
        out[..4].copy_from_slice(&v.to_be_bytes());
        Ok(4)
    } else {
        if out.len() < 8 {
            return Err(Error::BadFrame);
        }
        let v = value | (0b11 << 62);
        out[..8].copy_from_slice(&v.to_be_bytes());
        Ok(8)
    }
}

/// Decode a varint from the front of `buf`, advancing `buf` past
/// the consumed bytes. Returns the value or `Error::BadFrame` on
/// a truncated buffer.
pub(crate) fn read_varint(buf: &mut &[u8]) -> Result<u64, Error> {
    if buf.is_empty() {
        return Err(Error::BadFrame);
    }
    let prefix = buf[0] >> 6;
    let len = 1usize << prefix; // 1, 2, 4, or 8
    if buf.len() < len {
        return Err(Error::BadFrame);
    }
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
    fn round_trip_boundaries() {
        for v in [
            0u64,
            1,
            63,
            64,
            16_383,
            16_384,
            (1 << 30) - 1,
            1 << 30,
            MAX_VARINT,
        ] {
            let mut buf = [0u8; 8];
            let n = write_varint(v, &mut buf).expect("encode");
            assert_eq!(n, varint_len(v));
            let mut cursor: &[u8] = &buf[..n];
            assert_eq!(read_varint(&mut cursor).unwrap(), v);
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn truncated_rejected() {
        let mut b: &[u8] = &[0x40];
        assert_eq!(read_varint(&mut b), Err(Error::BadFrame));
        let mut b: &[u8] = &[];
        assert_eq!(read_varint(&mut b), Err(Error::BadFrame));
    }

    #[test]
    fn over_max_rejected_on_encode() {
        let mut buf = [0u8; 8];
        assert_eq!(write_varint(MAX_VARINT + 1, &mut buf), Err(Error::BadFrame));
    }

    #[test]
    fn small_out_buf_rejected() {
        let mut buf = [0u8; 1];
        assert_eq!(write_varint(64, &mut buf), Err(Error::BadFrame));
    }
}

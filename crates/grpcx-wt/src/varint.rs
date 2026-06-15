//! RFC 9000 §16 varint codec — duplicated from L4 / L5 (which
//! keep theirs `pub(crate)`). Re-exporting across the L5
//! boundary just to feed L6 would bloat L5's API surface;
//! ~50 lines of pure-function code is the lesser evil.
//! v1.x consolidation candidate.

use crate::Error;

const MAX_VARINT: u64 = (1 << 62) - 1;

pub(crate) fn write_varint(value: u64, out: &mut [u8]) -> Result<usize, Error> {
    if value > MAX_VARINT {
        return Err(Error::BadStreamFrame);
    }
    if value < (1 << 6) {
        if out.is_empty() {
            return Err(Error::BadStreamFrame);
        }
        out[0] = value as u8;
        Ok(1)
    } else if value < (1 << 14) {
        if out.len() < 2 {
            return Err(Error::BadStreamFrame);
        }
        let v = (value as u16) | (0b01 << 14);
        out[..2].copy_from_slice(&v.to_be_bytes());
        Ok(2)
    } else if value < (1 << 30) {
        if out.len() < 4 {
            return Err(Error::BadStreamFrame);
        }
        let v = (value as u32) | (0b10 << 30);
        out[..4].copy_from_slice(&v.to_be_bytes());
        Ok(4)
    } else {
        if out.len() < 8 {
            return Err(Error::BadStreamFrame);
        }
        let v = value | (0b11 << 62);
        out[..8].copy_from_slice(&v.to_be_bytes());
        Ok(8)
    }
}

pub(crate) fn read_varint(buf: &mut &[u8]) -> Result<u64, Error> {
    if buf.is_empty() {
        return Err(Error::BadStreamFrame);
    }
    let prefix = buf[0] >> 6;
    let len = 1usize << prefix;
    if buf.len() < len {
        return Err(Error::BadStreamFrame);
    }
    let mut bytes = [0u8; 8];
    bytes[8 - len..].copy_from_slice(&buf[..len]);
    bytes[8 - len] &= 0x3f;
    let value = u64::from_be_bytes(bytes);
    *buf = &buf[len..];
    Ok(value)
}

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
    use alloc::vec::Vec;

    #[test]
    fn boundary_round_trips() {
        for v in [
            0u64,
            63,
            64,
            16383,
            16384,
            (1 << 30) - 1,
            1 << 30,
            MAX_VARINT,
        ] {
            let mut buf = [0u8; 8];
            let n = write_varint(v, &mut buf).unwrap();
            assert_eq!(n, varint_len(v));
            let mut cursor: &[u8] = &buf[..n];
            assert_eq!(read_varint(&mut cursor).unwrap(), v);
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn empty_rejected() {
        let mut buf: &[u8] = &[];
        assert!(read_varint(&mut buf).is_err());
    }

    #[test]
    fn truncated_rejected() {
        // 2-byte form prefix but only 1 byte present.
        let mut buf: &[u8] = &[0x40];
        assert!(read_varint(&mut buf).is_err());
    }

    #[test]
    fn over_max_on_encode_rejected() {
        let mut buf = [0u8; 8];
        let _: Vec<u8> = Vec::new(); // touch alloc to silence unused-import warning when running this file standalone
        assert!(write_varint(MAX_VARINT + 1, &mut buf).is_err());
    }
}

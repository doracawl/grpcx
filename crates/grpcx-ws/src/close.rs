// Close-code lookup table + close-payload codec.
//
// RFC 6455 § 7.4 lists the named codes the spec defines
// (1000-1011 minus 1004-1006) plus the reserved range
// (1015). Post-RFC IANA codes (1012-1014) are NOT accepted
// at v0.3 — v1.x may soften.
//
// Close payload layout (RFC 6455 § 5.5.1):
//   empty      → no code, no reason
//   2 bytes    → BE u16 code, no reason
//   ≥ 2 bytes  → BE u16 code + UTF-8 reason

use crate::{CloseCode, WsError};
use alloc::string::String;
use alloc::vec::Vec;

// ============================================================
// CloseCode helpers.
// ============================================================

impl CloseCode {
    /// Look `code` up against the RFC 6455 § 7.4 table.
    ///
    /// # Errors
    /// - [`WsError::InvalidCloseCode`] for the explicitly
    ///   reserved codes (1004 / 1005 / 1006 / 1015) and
    ///   for any value outside the spec-defined +
    ///   IANA-registered + application-private ranges.
    pub fn from_u16(code: u16) -> Result<Self, WsError> {
        match code {
            1000 => Ok(CloseCode::Normal),
            1001 => Ok(CloseCode::GoingAway),
            1002 => Ok(CloseCode::ProtocolError),
            1003 => Ok(CloseCode::UnsupportedData),
            1007 => Ok(CloseCode::InvalidPayload),
            1008 => Ok(CloseCode::PolicyViolation),
            1009 => Ok(CloseCode::MessageTooLarge),
            1010 => Ok(CloseCode::ExtensionRequired),
            1011 => Ok(CloseCode::InternalError),
            // 3000-3999: IANA-registered library codes.
            // 4000-4999: application-private codes.
            3000..=4999 => Ok(CloseCode::Custom(code)),
            // Everything else (0-999 reserved, 1004/1005/
            // 1006/1015 explicit-reject, 1012-1014/1016-
            // 2999 RFC-reserved-future, 5000+ out of band).
            _ => Err(WsError::InvalidCloseCode(code)),
        }
    }

    /// Encode `self` back to its 16-bit wire value. Always
    /// produces a code in the spec-valid set.
    pub fn to_u16(&self) -> u16 {
        match self {
            CloseCode::Normal => 1000,
            CloseCode::GoingAway => 1001,
            CloseCode::ProtocolError => 1002,
            CloseCode::UnsupportedData => 1003,
            CloseCode::InvalidPayload => 1007,
            CloseCode::PolicyViolation => 1008,
            CloseCode::MessageTooLarge => 1009,
            CloseCode::ExtensionRequired => 1010,
            CloseCode::InternalError => 1011,
            CloseCode::Custom(c) => *c,
        }
    }

    /// Predicate that mirrors the explicit reject set in
    /// [`CloseCode::from_u16`]: codes the spec marks as
    /// reserved-must-not-send (1004 / 1005 / 1006 / 1015)
    /// plus reserved-future (1012-1014, 1016-2999) plus
    /// out-of-band (0-999, 5000+).
    pub fn is_reserved(code: u16) -> bool {
        Self::from_u16(code).is_err()
    }
}

// ============================================================
// Close-payload codec.
// ============================================================

/// Encode a Close-frame payload per RFC 6455 § 5.5.1.
///
/// - `code = None` → empty `Vec<u8>`. `reason` is ignored
///   in this case (the wire format has no way to carry a
///   reason without a code).
/// - `code = Some(c)` → 2 bytes big-endian + `reason` as
///   UTF-8.
pub fn encode_close_payload(code: Option<CloseCode>, reason: &str) -> Vec<u8> {
    let Some(c) = code else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(2 + reason.len());
    out.extend_from_slice(&c.to_u16().to_be_bytes());
    out.extend_from_slice(reason.as_bytes());
    out
}

/// Decode a Close-frame payload per RFC 6455 § 5.5.1.
///
/// # Errors
/// - [`WsError::Truncated`] — payload is exactly 1 byte
///   (close code is 2 bytes; 1 byte is illegal).
/// - [`WsError::InvalidCloseCode`] — the 2-byte BE code
///   isn't recognised (see [`CloseCode::from_u16`]).
/// - [`WsError::InvalidUtf8`] — the reason bytes don't
///   form valid UTF-8.
pub fn decode_close_payload(bytes: &[u8]) -> Result<(Option<CloseCode>, String), WsError> {
    match bytes.len() {
        0 => Ok((None, String::new())),
        1 => Err(WsError::Truncated),
        _ => {
            let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
            let code = CloseCode::from_u16(raw)?;
            let reason_bytes = &bytes[2..];
            let reason = core::str::from_utf8(reason_bytes)
                .map_err(|_| WsError::InvalidUtf8)?
                .into();
            Ok((Some(code), reason))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_code_named_round_trip() {
        for code in [
            CloseCode::Normal,
            CloseCode::GoingAway,
            CloseCode::ProtocolError,
            CloseCode::UnsupportedData,
            CloseCode::InvalidPayload,
            CloseCode::PolicyViolation,
            CloseCode::MessageTooLarge,
            CloseCode::ExtensionRequired,
            CloseCode::InternalError,
        ] {
            assert_eq!(CloseCode::from_u16(code.to_u16()).unwrap(), code);
        }
    }

    #[test]
    fn close_code_custom_iana_range() {
        assert_eq!(CloseCode::from_u16(3000).unwrap(), CloseCode::Custom(3000));
        assert_eq!(CloseCode::from_u16(3999).unwrap(), CloseCode::Custom(3999));
    }

    #[test]
    fn close_code_custom_application_range() {
        assert_eq!(CloseCode::from_u16(4000).unwrap(), CloseCode::Custom(4000));
        assert_eq!(CloseCode::from_u16(4999).unwrap(), CloseCode::Custom(4999));
    }

    #[test]
    fn close_code_rejects_explicit_reserved() {
        for n in [1004u16, 1005, 1006, 1015] {
            assert_eq!(
                CloseCode::from_u16(n).unwrap_err(),
                WsError::InvalidCloseCode(n)
            );
            assert!(CloseCode::is_reserved(n));
        }
    }

    #[test]
    fn close_code_rejects_out_of_band() {
        for n in [0u16, 999, 1012, 1013, 1014, 2999, 5000, 65535] {
            assert_eq!(
                CloseCode::from_u16(n).unwrap_err(),
                WsError::InvalidCloseCode(n)
            );
        }
    }
}

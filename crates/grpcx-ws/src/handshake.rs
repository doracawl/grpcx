// HTTP/1.1 upgrade handshake — RFC 6455 § 4.2 client request
// + server response. Inline parser + builder; no external
// HTTP dep.
//
// SHA-1 + base64 helpers come from the in-house stones
// `sha1-rfc3174` + `base64-rfc4648` (both 0-runtime-dep,
// no_std + alloc). The `derive_accept_key` /
// `verify_accept_key` helpers + the 4 entry-point functions
// re-export at the crate root.

use crate::{Handshake, WS_MAGIC_GUID, WS_VERSION, WsError};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ============================================================
// Key derivation + verification.
// ============================================================

/// Derive the server `Sec-WebSocket-Accept` value from the
/// client `Sec-WebSocket-Key`. Equals
/// `base64(SHA-1(request_key + WS_MAGIC_GUID))` per
/// RFC 6455 § 4.2.2 step 5.
pub fn derive_accept_key(request_key: &str) -> String {
    let mut concat = String::with_capacity(request_key.len() + WS_MAGIC_GUID.len());
    concat.push_str(request_key);
    concat.push_str(WS_MAGIC_GUID);
    let digest = sha1_rfc3174::sha1(concat.as_bytes());
    base64_rfc4648::encode(&digest)
}

/// Client-side helper: verify that a server-returned
/// `Sec-WebSocket-Accept` matches what
/// [`derive_accept_key`] produces for `request_key`.
///
/// # Errors
/// - [`WsError::BadHandshake`] — accept_key mismatch.
pub fn verify_accept_key(request_key: &str, accept_key: &str) -> Result<(), WsError> {
    if derive_accept_key(request_key) == accept_key {
        Ok(())
    } else {
        Err(WsError::BadHandshake("Sec-WebSocket-Accept mismatch"))
    }
}

// ============================================================
// Builders.
// ============================================================

/// Build the client `GET … HTTP/1.1` upgrade request per
/// RFC 6455 § 4.2.1.
///
/// `handshake.request_key` carries the caller-supplied
/// 16-byte base64-encoded nonce. `accept_key` is ignored
/// (it's a server-side field). `subprotocols` and
/// `extensions` populate the optional headers.
pub fn perform_client_handshake_request(host: &str, path: &str, handshake: &Handshake) -> String {
    let mut out = String::new();
    out.push_str(&format!("GET {path} HTTP/1.1\r\n"));
    out.push_str(&format!("Host: {host}\r\n"));
    out.push_str("Upgrade: websocket\r\n");
    out.push_str("Connection: Upgrade\r\n");
    out.push_str(&format!("Sec-WebSocket-Key: {}\r\n", handshake.request_key));
    out.push_str(&format!("Sec-WebSocket-Version: {WS_VERSION}\r\n"));
    if !handshake.subprotocols.is_empty() {
        out.push_str(&format!(
            "Sec-WebSocket-Protocol: {}\r\n",
            handshake.subprotocols.join(", ")
        ));
    }
    if !handshake.extensions.is_empty() {
        out.push_str(&format!(
            "Sec-WebSocket-Extensions: {}\r\n",
            handshake.extensions.join(", ")
        ));
    }
    out.push_str("\r\n");
    out
}

/// Build the server `HTTP/1.1 101 Switching Protocols`
/// response per RFC 6455 § 4.2.2.
pub fn build_server_handshake_response(handshake: &Handshake) -> String {
    let mut out = String::new();
    out.push_str("HTTP/1.1 101 Switching Protocols\r\n");
    out.push_str("Upgrade: websocket\r\n");
    out.push_str("Connection: Upgrade\r\n");
    out.push_str(&format!(
        "Sec-WebSocket-Accept: {}\r\n",
        handshake.accept_key
    ));
    if !handshake.subprotocols.is_empty() {
        out.push_str(&format!(
            "Sec-WebSocket-Protocol: {}\r\n",
            handshake.subprotocols.join(", ")
        ));
    }
    if !handshake.extensions.is_empty() {
        out.push_str(&format!(
            "Sec-WebSocket-Extensions: {}\r\n",
            handshake.extensions.join(", ")
        ));
    }
    out.push_str("\r\n");
    out
}

// ============================================================
// Parsers.
// ============================================================

/// Parse the client `GET …` upgrade request per RFC 6455
/// § 4.2.1. Returns a [`Handshake`] populated with the
/// `Sec-WebSocket-Key`, offered subprotocols, and offered
/// extensions.
pub fn parse_client_handshake_request(bytes: &[u8]) -> Result<Handshake, WsError> {
    let (start_line, headers) = split_message(bytes)?;
    // Method + path + version.
    let mut parts = start_line.splitn(3, ' ');
    let method = parts
        .next()
        .ok_or(WsError::BadHandshake("missing method"))?;
    let _path = parts.next().ok_or(WsError::BadHandshake("missing path"))?;
    let version = parts
        .next()
        .ok_or(WsError::BadHandshake("missing HTTP version"))?;
    if !method.eq_ignore_ascii_case("GET") {
        return Err(WsError::BadHandshake("client method must be GET"));
    }
    if !version.eq_ignore_ascii_case("HTTP/1.1") {
        return Err(WsError::BadHandshake("HTTP version must be 1.1"));
    }

    require_upgrade_websocket(&headers)?;
    require_connection_upgrade(&headers)?;

    let request_key = header_value(&headers, "Sec-WebSocket-Key")
        .ok_or(WsError::BadHandshake("missing Sec-WebSocket-Key"))?
        .to_string();
    let version_hdr = header_value(&headers, "Sec-WebSocket-Version")
        .ok_or(WsError::BadHandshake("missing Sec-WebSocket-Version"))?;
    if version_hdr.trim() != "13" {
        return Err(WsError::BadHandshake("Sec-WebSocket-Version must be 13"));
    }

    let subprotocols = split_csv_header(&headers, "Sec-WebSocket-Protocol");
    let extensions = split_csv_header(&headers, "Sec-WebSocket-Extensions");

    Ok(Handshake {
        request_key,
        accept_key: String::new(),
        subprotocols,
        extensions,
    })
}

/// Parse the server `HTTP/1.1 101 Switching Protocols`
/// response per RFC 6455 § 4.2.2.
pub fn parse_server_handshake_response(bytes: &[u8]) -> Result<Handshake, WsError> {
    let (start_line, headers) = split_message(bytes)?;
    let mut parts = start_line.splitn(3, ' ');
    let version = parts
        .next()
        .ok_or(WsError::BadHandshake("missing HTTP version"))?;
    let status = parts
        .next()
        .ok_or(WsError::BadHandshake("missing status"))?;
    // status text after 101 is ignored — match real-world
    // server diversity.
    let _reason = parts.next();
    if !version.eq_ignore_ascii_case("HTTP/1.1") {
        return Err(WsError::BadHandshake("HTTP version must be 1.1"));
    }
    if status != "101" {
        return Err(WsError::BadHandshake(
            "server status must be 101 Switching Protocols",
        ));
    }

    require_upgrade_websocket(&headers)?;
    require_connection_upgrade(&headers)?;

    let accept_key = header_value(&headers, "Sec-WebSocket-Accept")
        .ok_or(WsError::BadHandshake("missing Sec-WebSocket-Accept"))?
        .to_string();
    let subprotocols = split_csv_header(&headers, "Sec-WebSocket-Protocol");
    let extensions = split_csv_header(&headers, "Sec-WebSocket-Extensions");

    Ok(Handshake {
        request_key: String::new(),
        accept_key,
        subprotocols,
        extensions,
    })
}

// ============================================================
// Helpers.
// ============================================================

/// Split an HTTP message into its start line + header lines.
/// The body (if any) is ignored. Requires `\r\n\r\n`
/// separator.
fn split_message(bytes: &[u8]) -> Result<(String, Vec<(String, String)>), WsError> {
    // Find `\r\n\r\n` separator.
    let mut head_end = None;
    for i in 0..bytes.len().saturating_sub(3) {
        if &bytes[i..i + 4] == b"\r\n\r\n" {
            head_end = Some(i);
            break;
        }
    }
    let head_end = head_end.ok_or(WsError::BadHandshake(
        "missing \\r\\n\\r\\n header/body separator",
    ))?;
    let head_str = core::str::from_utf8(&bytes[..head_end])
        .map_err(|_| WsError::BadHandshake("headers contain non-UTF-8 bytes"))?;
    let mut lines = head_str.split("\r\n");
    let start_line = lines
        .next()
        .ok_or(WsError::BadHandshake("missing start line"))?
        .to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let colon = line
            .find(':')
            .ok_or(WsError::BadHandshake("header line missing ':'"))?;
        let name = line[..colon].trim().to_string();
        let value = line[colon + 1..].trim().to_string();
        headers.push((name, value));
    }
    Ok((start_line, headers))
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn require_upgrade_websocket(headers: &[(String, String)]) -> Result<(), WsError> {
    let v =
        header_value(headers, "Upgrade").ok_or(WsError::BadHandshake("missing Upgrade header"))?;
    if !v.trim().eq_ignore_ascii_case("websocket") {
        return Err(WsError::BadHandshake("Upgrade header must be websocket"));
    }
    Ok(())
}

fn require_connection_upgrade(headers: &[(String, String)]) -> Result<(), WsError> {
    let v = header_value(headers, "Connection")
        .ok_or(WsError::BadHandshake("missing Connection header"))?;
    let contains_upgrade = v
        .split(',')
        .any(|tok| tok.trim().eq_ignore_ascii_case("Upgrade"));
    if !contains_upgrade {
        return Err(WsError::BadHandshake(
            "Connection header must contain Upgrade token",
        ));
    }
    Ok(())
}

fn split_csv_header(headers: &[(String, String)], name: &str) -> Vec<String> {
    header_value(headers, name)
        .map(|v| {
            v.split(',')
                .map(|tok| tok.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn rfc6455_reference_accept_key() {
        // RFC 6455 § 1.3 canonical test vector.
        assert_eq!(
            derive_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn verify_ok_and_mismatch() {
        let req = "dGhlIHNhbXBsZSBub25jZQ==";
        let good = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
        let bad = "WRONG_VALUE====";
        assert!(verify_accept_key(req, good).is_ok());
        assert!(matches!(
            verify_accept_key(req, bad).unwrap_err(),
            WsError::BadHandshake(_)
        ));
    }

    #[test]
    fn split_message_smoke() {
        let msg = b"GET /chat HTTP/1.1\r\nHost: example.com\r\nUpgrade: websocket\r\n\r\n";
        let (start, headers) = split_message(msg).unwrap();
        assert_eq!(start, "GET /chat HTTP/1.1");
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0], ("Host".to_string(), "example.com".to_string()));
    }

    #[test]
    fn connection_token_list_accepted() {
        let headers = vec![("Connection".to_string(), "keep-alive, Upgrade".to_string())];
        assert!(require_connection_upgrade(&headers).is_ok());
    }
}

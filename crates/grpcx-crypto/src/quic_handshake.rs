//! QUIC-mode TLS handshake driver — wraps `rustls::quic`.
//!
//! Unlike [`crate::TlsConfig`] / [`crate::ClientHandshake`] etc.
//! (which drive a plain TLS 1.3 session for the L3 v1.0 surface),
//! the types here expose **per-encryption-level** key material at
//! Handshake / 1-RTT promotion events. L4 (`grpcx-quic`) needs
//! this to encrypt Handshake-level and 1-RTT packets — RFC 9001
//! §5 maps each QUIC encryption level to a separate set of keys.
//!
//! Initial-level keys are NOT delivered here — they're derived
//! from the client's destination Connection ID per RFC 9001 §5.2
//! (use [`crate::quic::initial_secret`] + [`crate::Hkdf`]).
//! v1.x delivers them; this module owns the post-Initial path.

extern crate std;

use crate::{TlsConfig, TlsError, tls_impl};
use alloc::boxed::Box;
use alloc::vec::Vec;
use rustls::pki_types::ServerName;
use rustls::quic;

/// QUIC protocol version, re-exported from rustls.
pub use rustls::quic::Version as QuicVersion;

/// A QUIC TLS endpoint — wraps a `rustls::quic::ClientConnection`
/// or `ServerConnection`.
pub struct QuicEndpoint {
    inner: ConnInner,
}

enum ConnInner {
    Client(Box<quic::ClientConnection>),
    Server(Box<quic::ServerConnection>),
}

/// Per-encryption-level key promotion delivered by the TLS state
/// machine.
pub enum QuicKeyChange {
    /// Handshake-level keys (after the first peer hello round).
    Handshake(QuicKeys),
    /// 1-RTT keys (after the handshake completes).
    OneRtt(QuicKeys),
}

/// Per-level QUIC keys for one direction pair (local encrypt /
/// remote decrypt) — header + packet keys for both sides.
pub struct QuicKeys {
    inner: quic::Keys,
}

const TAG_LEN: usize = 16;

impl QuicKeys {
    /// AEAD-seal a packet payload in place for the **local** side
    /// (we are sending). `payload` holds plaintext in `[..plaintext_len]`
    /// and `TAG_LEN` scratch bytes in `[plaintext_len..plaintext_len + TAG_LEN]`.
    /// `header` is the unprotected packet header (AAD).
    pub fn local_encrypt(
        &self,
        packet_number: u64,
        header: &[u8],
        payload: &mut [u8],
        plaintext_len: usize,
    ) -> Result<usize, TlsError> {
        if payload.len() < plaintext_len + TAG_LEN {
            return Err(TlsError::Internal);
        }
        let (pt_slot, tag_slot) = payload.split_at_mut(plaintext_len);
        let tag = self
            .inner
            .local
            .packet
            .encrypt_in_place(packet_number, header, pt_slot)
            .map_err(|_| TlsError::Internal)?;
        tag_slot[..TAG_LEN].copy_from_slice(tag.as_ref());
        Ok(plaintext_len + TAG_LEN)
    }

    /// AEAD-open a packet payload in place for the **remote** side
    /// (we are receiving). `packet` is the ciphertext + tag.
    /// Returns the plaintext length.
    pub fn remote_decrypt(
        &self,
        packet_number: u64,
        header: &[u8],
        packet: &mut [u8],
    ) -> Result<usize, TlsError> {
        let plain = self
            .inner
            .remote
            .packet
            .decrypt_in_place(packet_number, header, packet)
            .map_err(|_| TlsError::BadMac)?;
        Ok(plain.len())
    }

    /// Apply header protection for outgoing packets (XOR mask onto
    /// the first byte's bits + PN bytes).
    pub fn local_protect_header(
        &self,
        sample: &[u8],
        first: &mut u8,
        packet_number: &mut [u8],
    ) -> Result<(), TlsError> {
        self.inner
            .local
            .header
            .encrypt_in_place(sample, first, packet_number)
            .map_err(|_| TlsError::Internal)
    }

    /// Remove header protection for incoming packets.
    pub fn remote_unprotect_header(
        &self,
        sample: &[u8],
        first: &mut u8,
        packet_number: &mut [u8],
    ) -> Result<(), TlsError> {
        self.inner
            .remote
            .header
            .decrypt_in_place(sample, first, packet_number)
            .map_err(|_| TlsError::Internal)
    }

    /// HP sample length for the negotiated cipher suite — always
    /// 16 in v1, but exposed for callers that don't want to hard-
    /// code it.
    pub fn sample_len(&self) -> usize {
        self.inner.local.header.sample_len()
    }
}

impl QuicEndpoint {
    /// Construct a client-side QUIC TLS endpoint.
    pub fn new_client(
        tls_config: &TlsConfig,
        version: QuicVersion,
        server_name: &str,
        transport_params: Vec<u8>,
    ) -> Result<Self, TlsError> {
        let client_config = tls_config.inner.build_client_arc()?;
        let name =
            ServerName::try_from(server_name.to_string()).map_err(|_| TlsError::BadConfig)?;
        let conn = quic::ClientConnection::new(client_config, version, name, transport_params)
            .map_err(tls_impl::map_rustls_error)?;
        Ok(Self {
            inner: ConnInner::Client(Box::new(conn)),
        })
    }

    /// Construct a server-side QUIC TLS endpoint.
    pub fn new_server(
        tls_config: &TlsConfig,
        version: QuicVersion,
        transport_params: Vec<u8>,
    ) -> Result<Self, TlsError> {
        let server_config = tls_config.inner.server_arc_crate()?;
        let conn = quic::ServerConnection::new(server_config, version, transport_params)
            .map_err(tls_impl::map_rustls_error)?;
        Ok(Self {
            inner: ConnInner::Server(Box::new(conn)),
        })
    }

    /// Drain pending outbound handshake bytes; if the TLS state
    /// machine produced new keys for the next level, return them.
    pub fn write_hs(&mut self) -> (Vec<u8>, Option<QuicKeyChange>) {
        let mut bytes = Vec::new();
        let kc = match &mut self.inner {
            ConnInner::Client(c) => c.write_hs(&mut bytes),
            ConnInner::Server(c) => c.write_hs(&mut bytes),
        };
        (bytes, kc.map(map_key_change))
    }

    /// Feed inbound handshake bytes (the contents of a peer's
    /// CRYPTO frame) into the TLS state machine.
    pub fn read_hs(&mut self, bytes: &[u8]) -> Result<(), TlsError> {
        match &mut self.inner {
            ConnInner::Client(c) => c.read_hs(bytes),
            ConnInner::Server(c) => c.read_hs(bytes),
        }
        .map_err(tls_impl::map_rustls_error)
    }

    /// Whether the TLS handshake is still in progress.
    pub fn is_handshaking(&self) -> bool {
        match &self.inner {
            ConnInner::Client(c) => c.is_handshaking(),
            ConnInner::Server(c) => c.is_handshaking(),
        }
    }

    /// Negotiated ALPN protocol (after handshake).
    pub fn alpn_protocol(&self) -> Option<Vec<u8>> {
        let p: Option<&[u8]> = match &self.inner {
            ConnInner::Client(c) => c.alpn_protocol(),
            ConnInner::Server(c) => c.alpn_protocol(),
        };
        p.map(|x| x.to_vec())
    }

    /// Peer's QUIC transport parameters (after the handshake feeds
    /// them through).
    pub fn peer_transport_params(&self) -> Option<Vec<u8>> {
        match &self.inner {
            ConnInner::Client(c) => c.quic_transport_parameters().map(|x| x.to_vec()),
            ConnInner::Server(c) => c.quic_transport_parameters().map(|x| x.to_vec()),
        }
    }
}

fn map_key_change(kc: quic::KeyChange) -> QuicKeyChange {
    match kc {
        quic::KeyChange::Handshake { keys } => QuicKeyChange::Handshake(QuicKeys { inner: keys }),
        quic::KeyChange::OneRtt { keys, .. } => QuicKeyChange::OneRtt(QuicKeys { inner: keys }),
    }
}

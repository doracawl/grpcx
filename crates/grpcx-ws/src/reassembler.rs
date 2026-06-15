// Reassembler state machine — turns a stream of `Frame`s
// into reassembled `Message`s per RFC 6455 § 5.4. Control
// frames interleave per § 5.5 without touching the
// fragmentation state.
//
// See `../../REFACTOR-v9-v0.4-reassembly.md` for design notes
// + the full transition table.

use crate::{Frame, Message, MessageKind, Opcode, WsError};
use alloc::vec::Vec;

/// Output of [`Reassembler::feed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReassemblyOutput {
    /// More data fragments expected before the current
    /// message completes.
    Pending,
    /// A complete `Message` — either a single `FIN=1` data
    /// frame or the terminator of a fragmented sequence.
    Message(Message),
    /// A control frame passed through unchanged; caller
    /// handles it (e.g. responds to Ping, decodes Close
    /// payload). The reassembler's fragmentation state is
    /// not affected.
    Control(Frame),
}

/// RFC 6455 § 5.4 message reassembler.
///
/// Feed each decoded [`Frame`] into [`feed`](Self::feed); the
/// output enum signals whether a complete message is
/// available, whether more fragments are expected, or
/// whether the frame was a control frame (caller handles).
#[derive(Debug, Default, Clone)]
pub struct Reassembler {
    pending: Option<Pending>,
}

#[derive(Debug, Clone)]
struct Pending {
    kind: MessageKind,
    payload: Vec<u8>,
}

impl Reassembler {
    /// Construct a fresh reassembler with no fragmentation
    /// in flight.
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume a frame, advance the state machine, and
    /// return a [`ReassemblyOutput`].
    ///
    /// # Errors
    /// - [`WsError::ProtocolError`] — continuation frame
    ///   with no fragmentation in progress, or a new data
    ///   frame while a previous fragmented message is
    ///   still in progress.
    /// - [`WsError::InvalidUtf8`] — emitted Text message
    ///   payload is not valid UTF-8.
    pub fn feed(&mut self, frame: Frame) -> Result<ReassemblyOutput, WsError> {
        if frame.opcode.is_control() {
            return Ok(ReassemblyOutput::Control(frame));
        }
        match (self.pending.as_mut(), frame.opcode) {
            // Empty + complete data frame → emit directly.
            (None, Opcode::Text) if frame.fin => {
                validate_utf8(&frame.payload)?;
                Ok(ReassemblyOutput::Message(Message {
                    kind: MessageKind::Text,
                    payload: frame.payload,
                }))
            }
            (None, Opcode::Binary) if frame.fin => Ok(ReassemblyOutput::Message(Message {
                kind: MessageKind::Binary,
                payload: frame.payload,
            })),
            // Empty + opener fragment → start accumulating.
            (None, Opcode::Text) => {
                self.pending = Some(Pending {
                    kind: MessageKind::Text,
                    payload: frame.payload,
                });
                Ok(ReassemblyOutput::Pending)
            }
            (None, Opcode::Binary) => {
                self.pending = Some(Pending {
                    kind: MessageKind::Binary,
                    payload: frame.payload,
                });
                Ok(ReassemblyOutput::Pending)
            }
            // Empty + continuation → protocol error.
            (None, Opcode::Continuation) => Err(WsError::ProtocolError(
                "continuation frame with no fragmentation in progress",
            )),
            // Fragmenting + continuation → append.
            (Some(pending), Opcode::Continuation) => {
                pending.payload.extend_from_slice(&frame.payload);
                if frame.fin {
                    let pending = self.pending.take().expect("just matched Some");
                    if matches!(pending.kind, MessageKind::Text) {
                        validate_utf8(&pending.payload)?;
                    }
                    Ok(ReassemblyOutput::Message(Message {
                        kind: pending.kind,
                        payload: pending.payload,
                    }))
                } else {
                    Ok(ReassemblyOutput::Pending)
                }
            }
            // Fragmenting + new data frame → protocol error.
            (Some(_), Opcode::Text | Opcode::Binary) => Err(WsError::ProtocolError(
                "new data frame while previous message unfinished",
            )),
            // Control was handled above; this arm is
            // unreachable but keeps the match exhaustive.
            (_, Opcode::Close | Opcode::Ping | Opcode::Pong) => {
                Ok(ReassemblyOutput::Control(frame))
            }
        }
    }
}

fn validate_utf8(bytes: &[u8]) -> Result<(), WsError> {
    core::str::from_utf8(bytes).map_err(|_| WsError::InvalidUtf8)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_frame(fin: bool, payload: &[u8]) -> Frame {
        Frame {
            fin,
            opcode: Opcode::Text,
            payload: payload.to_vec(),
            ..Frame::default()
        }
    }

    fn cont_frame(fin: bool, payload: &[u8]) -> Frame {
        Frame {
            fin,
            opcode: Opcode::Continuation,
            payload: payload.to_vec(),
            ..Frame::default()
        }
    }

    #[test]
    fn default_equals_new() {
        let a = Reassembler::default();
        let b = Reassembler::new();
        assert!(a.pending.is_none());
        assert!(b.pending.is_none());
    }

    #[test]
    fn single_text_frame() {
        let mut r = Reassembler::new();
        let out = r.feed(text_frame(true, b"hello")).unwrap();
        assert_eq!(
            out,
            ReassemblyOutput::Message(Message {
                kind: MessageKind::Text,
                payload: b"hello".to_vec(),
            })
        );
    }

    #[test]
    fn continuation_at_empty_errors() {
        let mut r = Reassembler::new();
        let err = r.feed(cont_frame(true, b"junk")).unwrap_err();
        assert!(matches!(err, WsError::ProtocolError(_)));
    }

    #[test]
    fn text_mid_fragmentation_errors() {
        let mut r = Reassembler::new();
        let _ = r.feed(text_frame(false, b"part-a")).unwrap();
        let err = r.feed(text_frame(true, b"new-message")).unwrap_err();
        assert!(matches!(err, WsError::ProtocolError(_)));
    }

    #[test]
    fn non_utf8_text_rejected() {
        let mut r = Reassembler::new();
        let err = r.feed(text_frame(true, &[0xFF, 0xFE])).unwrap_err();
        assert_eq!(err, WsError::InvalidUtf8);
    }
}

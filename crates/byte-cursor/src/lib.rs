#![doc = include_str!("../README.md")]
#![no_std]

//! Position-tracking cursor over a byte slice. See the
//! README for the full feature matrix and design rationale.

// ============================================================
// Cursor.
// ============================================================

/// A position-tracking cursor over `&[u8]`.
///
/// Lifetime `'src` is the underlying byte slice's
/// lifetime; `take_while` and `remaining` return slices
/// that borrow from it.
#[derive(Debug, Clone)]
pub struct Cursor<'src> {
    bytes: &'src [u8],
    pos: usize,
    line: u32,
    col: u32,
}

impl<'src> Cursor<'src> {
    /// Construct a cursor positioned at byte 0, line 1,
    /// column 1.
    pub fn new(bytes: &'src [u8]) -> Self {
        Cursor {
            bytes,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    /// Convenience: construct from a `&str` (the bytes are
    /// the UTF-8 encoding).
    // `FromStr` is infallible-Result-shaped and erases the
    // borrow lifetime; this constructor is infallible and
    // returns a cursor borrowing the input. Different
    // contract, same verb.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &'src str) -> Self {
        Self::new(s.as_bytes())
    }

    // ---------- Position queries ---------- //

    /// Current byte position (`0..=bytes.len()`).
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// All bytes from the current position to the end.
    pub fn remaining(&self) -> &'src [u8] {
        &self.bytes[self.pos..]
    }

    /// The full underlying byte source, lifetime-bound to
    /// `'src`. Useful for callers that captured a start
    /// position before consuming and want to slice the run
    /// directly (`&cur.source()[start..cur.pos()]`).
    pub fn source(&self) -> &'src [u8] {
        self.bytes
    }

    /// `true` if no more bytes to consume.
    pub fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    /// 1-based line number.
    pub fn line(&self) -> u32 {
        self.line
    }

    /// 1-based column number (resets to 1 after `\n`).
    pub fn col(&self) -> u32 {
        self.col
    }

    /// `(line, col)` at the current position.
    pub fn here(&self) -> (u32, u32) {
        (self.line, self.col)
    }

    // ---------- Byte-level inspection ---------- //

    /// Peek at the current byte without consuming.
    pub fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    /// Peek at `pos + ofs` (0 = same as `peek`).
    pub fn peek_at(&self, ofs: usize) -> Option<u8> {
        self.bytes.get(self.pos + ofs).copied()
    }

    /// Consume and return the current byte; updates
    /// line/col tracking. Returns `None` at end of input.
    pub fn advance(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    /// Consume `expected` if present; return `true` on
    /// match, `false` if peek didn't match or end-of-input.
    pub fn try_consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consume `expected` or return `false` without
    /// advancing. Same as [`try_consume`](Self::try_consume)
    /// but named to read as a precondition assertion at
    /// call sites where mismatch is a parser bug.
    pub fn expect_byte(&mut self, expected: u8) -> bool {
        self.try_consume(expected)
    }

    // ---------- Predicate-driven span extraction ---------- //

    /// Consume bytes while `pred(byte)` is true; return the
    /// span as a borrowed slice.
    pub fn take_while(&mut self, mut pred: impl FnMut(u8) -> bool) -> &'src [u8] {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if pred(b) {
                self.advance();
            } else {
                break;
            }
        }
        &self.bytes[start..self.pos]
    }

    /// Skip bytes while `pred(byte)` is true; return the
    /// count consumed.
    pub fn skip_while(&mut self, mut pred: impl FnMut(u8) -> bool) -> usize {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if pred(b) {
                self.advance();
            } else {
                break;
            }
        }
        self.pos - start
    }

    // ---------- Whitespace shorthands ---------- //

    /// Skip horizontal whitespace (` `, `\t`). Returns
    /// count.
    pub fn skip_ws_horizontal(&mut self) -> usize {
        self.skip_while(|b| b == b' ' || b == b'\t')
    }

    /// Skip ASCII whitespace (` `, `\t`, `\r`, `\n`).
    /// Returns count. Note that `\n` still advances the
    /// line counter.
    pub fn skip_ws(&mut self) -> usize {
        self.skip_while(|b| b == b' ' || b == b'\t' || b == b'\r' || b == b'\n')
    }
}

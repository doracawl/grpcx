// Crate-private recursive-descent parser over the v0.2 token
// stream. Emits the public `Schema` / `Message` / `Field` /
// `Enum` ADTs declared in `lib.rs`.
//
// Grammar accepted: see `../GRAMMAR.md` § "Parser productions".
// Tradeoffs + scope: see `../../REFACTOR-v8-v0.3-parser.md`.
//
// Conventions baked in:
// - Parser emits `FieldType::Message(name)` for ALL named-type
//   references; the v0.4 semantic analyser reclassifies refs
//   that resolve to enums to `FieldType::Enum(name)`.
// - `oneof Foo { … }` members are flattened into the enclosing
//   `Message.fields` as `Label::Singular`; the oneof container
//   is dropped. First-class oneof modelling lands at v1.x.
// - Top-level / message / enum `option` blocks, message-body
//   `reserved` blocks, field-level `[…]` option blocks
//   (including proto2 `[default = …]`), and `service` blocks
//   are parse-and-discarded. `extend` / `group` / `extensions`
//   surface as `ImportError::UnsupportedFeature`.

use crate::lexer::{Keyword, Punct, ScalarType, Spanned, Token};
use crate::{
    Dialect, Edition, Enum, EnumValue, Field, FieldType, ImportError, Label, Message, MethodDef,
    Schema, ServiceDef,
};
use alloc::boxed::Box;
use alloc::string::String;

pub(crate) fn parse(tokens: &[Spanned], dialect: Dialect) -> Result<Schema, ImportError> {
    let mut p = Parser {
        tokens,
        pos: 0,
        dialect,
    };
    p.parse_schema()
}

struct Parser<'t> {
    tokens: &'t [Spanned],
    pos: usize,
    dialect: Dialect,
}

// ============================================================
// Low-level helpers.
// ============================================================

impl<'t> Parser<'t> {
    fn peek(&self) -> Option<&'t Spanned> {
        self.tokens.get(self.pos)
    }

    fn peek_at(&self, ofs: usize) -> Option<&'t Spanned> {
        self.tokens.get(self.pos + ofs)
    }

    fn advance(&mut self) -> Option<&'t Spanned> {
        let t = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(t)
    }

    fn at_eof(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    /// `ImportError::Parse { line, col }` at the current
    /// position (or at the end of the last consumed token on
    /// EOF).
    fn parse_error_here(&self) -> ImportError {
        if let Some(t) = self.peek() {
            ImportError::Parse {
                line: t.line,
                col: t.col,
            }
        } else if let Some(t) = self.tokens.last() {
            ImportError::Parse {
                line: t.line,
                col: t.col,
            }
        } else {
            ImportError::Parse { line: 1, col: 1 }
        }
    }

    fn expect_kw(&mut self, kw: Keyword) -> Result<(), ImportError> {
        match self.peek().map(|s| &s.token) {
            Some(Token::Keyword(k)) if *k == kw => {
                self.advance();
                Ok(())
            }
            _ => Err(self.parse_error_here()),
        }
    }

    fn eat_kw(&mut self, kw: Keyword) -> bool {
        if matches!(self.peek().map(|s| &s.token), Some(Token::Keyword(k)) if *k == kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, p: Punct) -> Result<(), ImportError> {
        match self.peek().map(|s| &s.token) {
            Some(Token::Punct(q)) if *q == p => {
                self.advance();
                Ok(())
            }
            _ => Err(self.parse_error_here()),
        }
    }

    fn eat_punct(&mut self, p: Punct) -> bool {
        if matches!(self.peek().map(|s| &s.token), Some(Token::Punct(q)) if *q == p) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_ident(&mut self) -> Result<String, ImportError> {
        match self.peek().map(|s| &s.token) {
            Some(Token::Ident(name)) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(self.parse_error_here()),
        }
    }

    fn expect_string(&mut self) -> Result<String, ImportError> {
        match self.peek().map(|s| &s.token) {
            Some(Token::StringLit(s)) => {
                let s = s.clone();
                self.advance();
                Ok(s)
            }
            _ => Err(self.parse_error_here()),
        }
    }

    fn expect_int_u64(&mut self) -> Result<u64, ImportError> {
        match self.peek().map(|s| &s.token) {
            Some(Token::IntLit(n)) => {
                let n = *n;
                self.advance();
                Ok(n)
            }
            _ => Err(self.parse_error_here()),
        }
    }
}

// ============================================================
// Top-level recursive descent.
// ============================================================

impl<'t> Parser<'t> {
    fn parse_schema(&mut self) -> Result<Schema, ImportError> {
        // The lexer's syntax-line gate guarantees these four
        // tokens are present and shaped correctly; consume
        // them. For proto3 sources the leading token is the
        // bare ident `syntax` rather than the `edition`
        // keyword — the gate already validated the value.
        match self.dialect {
            Dialect::Edition2023 => self.expect_kw(Keyword::Edition)?,
            Dialect::Proto3 | Dialect::Proto2 => {
                let _ = self.expect_ident()?; // "syntax"
            }
        }
        self.expect_punct(Punct::Equals)?;
        let _ = self.expect_string()?;
        self.expect_punct(Punct::Semicolon)?;

        let mut schema = Schema {
            edition: Edition::Edition2023,
            source_dialect: self.dialect,
            ..Schema::default()
        };

        while !self.at_eof() {
            self.parse_top_level(&mut schema)?;
        }

        Ok(schema)
    }

    fn parse_top_level(&mut self, schema: &mut Schema) -> Result<(), ImportError> {
        let t = self.peek().ok_or_else(|| self.parse_error_here())?;
        match &t.token {
            Token::Punct(Punct::Semicolon) => {
                self.advance();
            }
            Token::Keyword(Keyword::Package) => {
                self.advance();
                let path = self.parse_dotted_ident()?;
                self.expect_punct(Punct::Semicolon)?;
                schema.package = Some(path);
            }
            Token::Keyword(Keyword::Import) => {
                self.advance();
                // Optional "weak" / "public" modifier — bare
                // ident, not a reserved keyword in our token
                // set.
                if let Some(Token::Ident(name)) = self.peek().map(|s| &s.token)
                    && (name == "weak" || name == "public")
                {
                    self.advance();
                }
                let path = self.expect_string()?;
                self.expect_punct(Punct::Semicolon)?;
                schema.imports.push(path);
            }
            Token::Keyword(Keyword::Message) => {
                self.advance();
                let msg = self.parse_message_body()?;
                schema.messages.push(msg);
            }
            Token::Keyword(Keyword::Enum) => {
                self.advance();
                let en = self.parse_enum_body()?;
                schema.enums.push(en);
            }
            Token::Keyword(Keyword::Option) => {
                self.advance();
                self.skip_to_semicolon()?;
            }
            Token::Keyword(Keyword::Service) => {
                self.advance();
                let svc = self.parse_service_body()?;
                schema.services.push(svc);
            }
            Token::Ident(name) if name == "extend" || name == "group" => {
                return Err(ImportError::UnsupportedFeature(
                    "`extend` / `group` declarations are not supported; \
                     editions 2023+ schemas should not need them",
                ));
            }
            _ => return Err(self.parse_error_here()),
        }
        Ok(())
    }
}

// ============================================================
// Services.
// ============================================================

impl<'t> Parser<'t> {
    /// `service Ident { ( rpc … | option …; | ; )* }`
    fn parse_service_body(&mut self) -> Result<ServiceDef, ImportError> {
        let name = self.expect_ident()?;
        self.expect_punct(Punct::LBrace)?;
        let mut svc = ServiceDef {
            name,
            ..ServiceDef::default()
        };
        while !self.eat_punct(Punct::RBrace) {
            if self.at_eof() {
                return Err(self.parse_error_here());
            }
            let t = self.peek().ok_or_else(|| self.parse_error_here())?;
            match &t.token {
                Token::Punct(Punct::Semicolon) => {
                    self.advance();
                }
                Token::Keyword(Keyword::Option) => {
                    self.advance();
                    self.skip_to_semicolon()?;
                }
                Token::Keyword(Keyword::Rpc) => {
                    self.advance();
                    svc.methods.push(self.parse_rpc()?);
                }
                _ => return Err(self.parse_error_here()),
            }
        }
        Ok(svc)
    }

    /// `rpc Name ( [stream] Type ) returns ( [stream] Type ) ( ; | { option…; } )`
    fn parse_rpc(&mut self) -> Result<MethodDef, ImportError> {
        let name = self.expect_ident()?;
        self.expect_punct(Punct::LParen)?;
        let (client_streaming, input) = self.parse_rpc_io_type()?;
        self.expect_punct(Punct::RParen)?;
        // `returns` keyword.
        match self.peek().map(|t| &t.token) {
            Some(Token::Keyword(Keyword::Returns)) => {
                self.advance();
            }
            _ => return Err(self.parse_error_here()),
        }
        self.expect_punct(Punct::LParen)?;
        let (server_streaming, output) = self.parse_rpc_io_type()?;
        self.expect_punct(Punct::RParen)?;
        // Body: bare `;` or an options block `{ … }`.
        if self.eat_punct(Punct::Semicolon) {
            // done
        } else {
            self.expect_punct(Punct::LBrace)?;
            self.skip_balanced_braces()?;
        }
        Ok(MethodDef {
            name,
            input,
            output,
            client_streaming,
            server_streaming,
        })
    }

    /// `[stream] dotted.Type` inside rpc parens. `stream`
    /// is contextual (not a lexer keyword) — an Ident
    /// literally equal to "stream" followed by another
    /// ident is the modifier; otherwise it IS the type
    /// name (protobuf allows a message named `stream`).
    fn parse_rpc_io_type(&mut self) -> Result<(bool, String), ImportError> {
        let first = self.expect_ident()?;
        let streaming =
            first == "stream" && matches!(self.peek().map(|t| &t.token), Some(Token::Ident(_)));
        let mut ty = if streaming {
            self.expect_ident()?
        } else {
            first
        };
        // Dotted path: `.` Ident repeated.
        while self.eat_punct(Punct::Dot) {
            ty.push('.');
            ty.push_str(&self.expect_ident()?);
        }
        Ok((streaming, ty))
    }
}

// ============================================================
// Messages.
// ============================================================

impl<'t> Parser<'t> {
    fn parse_message_body(&mut self) -> Result<Message, ImportError> {
        let name = self.expect_ident()?;
        self.expect_punct(Punct::LBrace)?;
        let mut msg = Message {
            name,
            ..Message::default()
        };
        while !self.eat_punct(Punct::RBrace) {
            if self.at_eof() {
                return Err(self.parse_error_here());
            }
            self.parse_message_member(&mut msg)?;
        }
        Ok(msg)
    }

    fn parse_message_member(&mut self, msg: &mut Message) -> Result<(), ImportError> {
        let t = self.peek().ok_or_else(|| self.parse_error_here())?;
        match &t.token {
            Token::Punct(Punct::Semicolon) => {
                self.advance();
            }
            Token::Keyword(Keyword::Option) => {
                self.advance();
                self.skip_to_semicolon()?;
            }
            Token::Keyword(Keyword::Reserved) => {
                self.advance();
                self.skip_to_semicolon()?;
            }
            Token::Keyword(Keyword::Message) => {
                self.advance();
                let nested = self.parse_message_body()?;
                msg.nested_messages.push(nested);
            }
            Token::Keyword(Keyword::Enum) => {
                self.advance();
                let nested = self.parse_enum_body()?;
                msg.nested_enums.push(nested);
            }
            Token::Keyword(Keyword::Oneof) => {
                self.advance();
                let _oneof_name = self.expect_ident()?;
                self.expect_punct(Punct::LBrace)?;
                while !self.eat_punct(Punct::RBrace) {
                    if self.at_eof() {
                        return Err(self.parse_error_here());
                    }
                    let t = self.peek().ok_or_else(|| self.parse_error_here())?;
                    match &t.token {
                        Token::Punct(Punct::Semicolon) => {
                            self.advance();
                        }
                        Token::Keyword(Keyword::Option) => {
                            self.advance();
                            self.skip_to_semicolon()?;
                        }
                        _ => {
                            // oneof members can't carry the `repeated`
                            // label — disallow at the parse layer by
                            // refusing to recognise it inside the loop.
                            let f = self.parse_field_decl(false)?;
                            msg.fields.push(f);
                        }
                    }
                }
            }
            Token::Ident(name) if name == "extend" || name == "group" => {
                return Err(ImportError::UnsupportedFeature(
                    "`extend` / `group` declarations are not supported; \
                     editions 2023+ schemas should not need them",
                ));
            }
            Token::Ident(name) if name == "extensions" => {
                return Err(ImportError::UnsupportedFeature(
                    "proto2 `extensions` declarations are not supported",
                ));
            }
            Token::Keyword(Keyword::Repeated)
            | Token::Keyword(Keyword::Map)
            | Token::ScalarType(_)
            | Token::Ident(_)
            | Token::Punct(Punct::Dot) => {
                let f = self.parse_field_decl(true)?;
                msg.fields.push(f);
            }
            _ => return Err(self.parse_error_here()),
        }
        Ok(())
    }
}

// ============================================================
// Fields + field types.
// ============================================================

impl<'t> Parser<'t> {
    fn parse_field_decl(&mut self, allow_repeated: bool) -> Result<Field, ImportError> {
        // Leading label idents. proto3: at most an `optional`
        // prefix (stage B presence kernel). proto2 (stage C):
        // every regular message field carries EXACTLY one of
        // optional/required/repeated — `optional` is the same
        // explicit-presence kernel, `required` maps to
        // Label::Singular (always-encoded, like editions
        // LEGACY_REQUIRED; missing-required decode is not
        // enforced — prost doesn't either). map<…> fields and
        // oneof members take no label in both dialects.
        let mut optional = false;
        let mut required = false;
        if allow_repeated {
            match self.peek().map(|t| &t.token) {
                Some(Token::Ident(n))
                    if n == "optional"
                        && matches!(self.dialect, Dialect::Proto3 | Dialect::Proto2) =>
                {
                    optional = true;
                    self.advance();
                }
                Some(Token::Ident(n)) if n == "required" && self.dialect == Dialect::Proto2 => {
                    required = true;
                    self.advance();
                }
                _ => {}
            }
        }
        let label = if allow_repeated && self.eat_kw(Keyword::Repeated) {
            if optional || required {
                return Err(ImportError::UnsupportedFeature(
                    "`optional repeated` / `required repeated`: a field takes at most one label",
                ));
            }
            Label::Repeated
        } else if optional {
            Label::Optional
        } else {
            if self.dialect == Dialect::Proto2
                && allow_repeated
                && !required
                && !matches!(
                    self.peek().map(|t| &t.token),
                    Some(Token::Keyword(Keyword::Map))
                )
            {
                return Err(ImportError::UnsupportedFeature(
                    "proto2 message fields take exactly one optional/required/repeated \
                     label (map fields take none)",
                ));
            }
            Label::Singular
        };
        if (optional || required)
            && matches!(self.peek().map(|t| &t.token), Some(Token::Ident(n)) if n == "group")
        {
            return Err(ImportError::UnsupportedFeature(
                "`group` fields are not supported (proto2 legacy; use a nested message)",
            ));
        }
        let ty = self.parse_field_type()?;
        if (optional || required) && matches!(ty, FieldType::Map { .. }) {
            return Err(ImportError::UnsupportedFeature(
                "`optional map<...>` / `required map<...>`: map fields take no label",
            ));
        }
        let name = self.expect_ident()?;
        self.expect_punct(Punct::Equals)?;
        let n_u64 = self.expect_int_u64()?;
        let number: u32 = n_u64.try_into().map_err(|_| self.parse_error_here())?;
        // Optional `[…]` field-options — skipped via balanced
        // bracket scan; entries themselves are not represented
        // in the v0.3 AST.
        if self.eat_punct(Punct::LBracket) {
            self.skip_balanced_brackets()?;
        }
        self.expect_punct(Punct::Semicolon)?;
        Ok(Field {
            name,
            number,
            ty,
            label,
        })
    }

    fn parse_field_type(&mut self) -> Result<FieldType, ImportError> {
        let t = self.peek().ok_or_else(|| self.parse_error_here())?;
        match &t.token {
            Token::ScalarType(s) => {
                let st = *s;
                self.advance();
                Ok(scalar_to_field_type(st))
            }
            Token::Keyword(Keyword::Map) => {
                self.advance();
                self.expect_punct(Punct::LAngle)?;
                let key = self.parse_field_type()?;
                self.expect_punct(Punct::Comma)?;
                let value = self.parse_field_type()?;
                self.expect_punct(Punct::RAngle)?;
                Ok(FieldType::Map {
                    key: Box::new(key),
                    value: Box::new(value),
                })
            }
            Token::Punct(Punct::Dot) | Token::Ident(_) => {
                let name = self.parse_type_named_ref()?;
                // See module-level convention note: parser emits
                // `Message(name)` for all named refs; v0.4
                // reclassifies enum refs.
                Ok(FieldType::Message(name))
            }
            _ => Err(self.parse_error_here()),
        }
    }

    /// `"."? Ident ("." Ident)*` — consumes dotted-ident
    /// segments only while the token AFTER each `.` is another
    /// ident. That lookahead is what stops the loop before the
    /// field name (the trailing single ident before `=`).
    fn parse_type_named_ref(&mut self) -> Result<String, ImportError> {
        let mut s = String::new();
        if self.eat_punct(Punct::Dot) {
            s.push('.');
        }
        let first = self.expect_ident()?;
        s.push_str(&first);
        while matches!(
            self.peek().map(|t| &t.token),
            Some(Token::Punct(Punct::Dot))
        ) && matches!(self.peek_at(1).map(|t| &t.token), Some(Token::Ident(_)))
        {
            self.advance(); // consume `.`
            s.push('.');
            let part = self.expect_ident()?;
            s.push_str(&part);
        }
        Ok(s)
    }
}

// ============================================================
// Enums.
// ============================================================

impl<'t> Parser<'t> {
    fn parse_enum_body(&mut self) -> Result<Enum, ImportError> {
        let name = self.expect_ident()?;
        self.expect_punct(Punct::LBrace)?;
        let mut en = Enum {
            name,
            ..Enum::default()
        };
        while !self.eat_punct(Punct::RBrace) {
            if self.at_eof() {
                return Err(self.parse_error_here());
            }
            self.parse_enum_member(&mut en)?;
        }
        Ok(en)
    }

    fn parse_enum_member(&mut self, en: &mut Enum) -> Result<(), ImportError> {
        let t = self.peek().ok_or_else(|| self.parse_error_here())?;
        match &t.token {
            Token::Punct(Punct::Semicolon) => {
                self.advance();
            }
            Token::Keyword(Keyword::Option) => {
                self.advance();
                self.skip_to_semicolon()?;
            }
            Token::Keyword(Keyword::Reserved) => {
                self.advance();
                self.skip_to_semicolon()?;
            }
            Token::Ident(_) => {
                let v = self.parse_enum_value()?;
                en.values.push(v);
            }
            _ => return Err(self.parse_error_here()),
        }
        Ok(())
    }

    fn parse_enum_value(&mut self) -> Result<EnumValue, ImportError> {
        let name = self.expect_ident()?;
        self.expect_punct(Punct::Equals)?;
        // Optional sign — `+` is a no-op; `-` negates.
        let mut neg = false;
        if self.eat_punct(Punct::Minus) {
            neg = true;
        } else {
            self.eat_punct(Punct::Plus);
        }
        let n = self.expect_int_u64()?;
        // Range-check against i32 with care: -2147483648 fits
        // even though its absolute value is i32::MIN.unsigned_abs().
        let number = if neg {
            // `-n` must fit in i32 negative range.
            if n > (i32::MIN.unsigned_abs() as u64) {
                return Err(self.parse_error_here());
            }
            if n == i32::MIN.unsigned_abs() as u64 {
                i32::MIN
            } else {
                -(n as i32)
            }
        } else {
            i32::try_from(n).map_err(|_| self.parse_error_here())?
        };
        if self.eat_punct(Punct::LBracket) {
            self.skip_balanced_brackets()?;
        }
        self.expect_punct(Punct::Semicolon)?;
        Ok(EnumValue { name, number })
    }
}

// ============================================================
// Dotted-ident helpers + skippers.
// ============================================================

impl<'t> Parser<'t> {
    fn parse_dotted_ident(&mut self) -> Result<String, ImportError> {
        let mut s = self.expect_ident()?;
        while self.eat_punct(Punct::Dot) {
            s.push('.');
            let p = self.expect_ident()?;
            s.push_str(&p);
        }
        Ok(s)
    }

    /// Consume tokens until the next semicolon at the current
    /// nesting depth (entering nested `{…}` or `[…]` doesn't
    /// trip the semi). Consumes the terminating `;` itself.
    fn skip_to_semicolon(&mut self) -> Result<(), ImportError> {
        let mut depth_brace: u32 = 0;
        let mut depth_bracket: u32 = 0;
        let mut depth_paren: u32 = 0;
        while let Some(t) = self.peek() {
            match &t.token {
                Token::Punct(Punct::LBrace) => {
                    depth_brace += 1;
                    self.advance();
                }
                Token::Punct(Punct::RBrace) => {
                    depth_brace = depth_brace
                        .checked_sub(1)
                        .ok_or_else(|| self.parse_error_here())?;
                    self.advance();
                }
                Token::Punct(Punct::LBracket) => {
                    depth_bracket += 1;
                    self.advance();
                }
                Token::Punct(Punct::RBracket) => {
                    depth_bracket = depth_bracket
                        .checked_sub(1)
                        .ok_or_else(|| self.parse_error_here())?;
                    self.advance();
                }
                Token::Punct(Punct::LParen) => {
                    depth_paren += 1;
                    self.advance();
                }
                Token::Punct(Punct::RParen) => {
                    depth_paren = depth_paren
                        .checked_sub(1)
                        .ok_or_else(|| self.parse_error_here())?;
                    self.advance();
                }
                Token::Punct(Punct::Semicolon)
                    if depth_brace == 0 && depth_bracket == 0 && depth_paren == 0 =>
                {
                    self.advance();
                    return Ok(());
                }
                _ => {
                    self.advance();
                }
            }
        }
        Err(self.parse_error_here())
    }

    /// Called AFTER consuming the opening `[`. Consumes tokens
    /// until the matching `]` (handling nested brackets).
    fn skip_balanced_brackets(&mut self) -> Result<(), ImportError> {
        let mut depth: u32 = 1;
        while let Some(t) = self.peek() {
            match &t.token {
                Token::Punct(Punct::LBracket) => {
                    depth += 1;
                    self.advance();
                }
                Token::Punct(Punct::RBracket) => {
                    depth -= 1;
                    self.advance();
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {
                    self.advance();
                }
            }
        }
        Err(self.parse_error_here())
    }

    /// Called AFTER consuming the opening `{`. Consumes tokens
    /// until the matching `}` (handling nested braces).
    fn skip_balanced_braces(&mut self) -> Result<(), ImportError> {
        let mut depth: u32 = 1;
        while let Some(t) = self.peek() {
            match &t.token {
                Token::Punct(Punct::LBrace) => {
                    depth += 1;
                    self.advance();
                }
                Token::Punct(Punct::RBrace) => {
                    depth -= 1;
                    self.advance();
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {
                    self.advance();
                }
            }
        }
        Err(self.parse_error_here())
    }
}

// ============================================================
// ScalarType → FieldType mapping.
// ============================================================

fn scalar_to_field_type(s: ScalarType) -> FieldType {
    match s {
        ScalarType::Int32 => FieldType::Int32,
        ScalarType::Int64 => FieldType::Int64,
        ScalarType::Uint32 => FieldType::Uint32,
        ScalarType::Uint64 => FieldType::Uint64,
        ScalarType::Sint32 => FieldType::Sint32,
        ScalarType::Sint64 => FieldType::Sint64,
        ScalarType::Fixed32 => FieldType::Fixed32,
        ScalarType::Fixed64 => FieldType::Fixed64,
        ScalarType::Sfixed32 => FieldType::Sfixed32,
        ScalarType::Sfixed64 => FieldType::Sfixed64,
        ScalarType::Float => FieldType::Float,
        ScalarType::Double => FieldType::Double,
        ScalarType::Bool => FieldType::Bool,
        ScalarType::StringTy => FieldType::String_,
        ScalarType::Bytes => FieldType::Bytes,
    }
}

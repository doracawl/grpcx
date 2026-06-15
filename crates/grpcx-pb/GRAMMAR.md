# `grpcx-pb` Grammar — `.proto` editions 2023+

Token-level grammar plus the lexer state-table summary. v0.2
locks this; v0.3+ parser reads against this doc.

## Source charter

Editions 2023 grammar reference:
[`protobuf/docs/reference/editions/editions-grammar.md`](https://protobuf.dev/reference/editions-grammar/).
Where this doc diverges from the upstream reference (typically
to subset for L8's lab-internal scope), the divergence is
called out inline.

## Source encoding

UTF-8. The lexer is byte-driven over ASCII; non-ASCII bytes are
allowed **only** inside string literals and comments. A non-
ASCII byte anywhere else fails with `ImportError::Lex` at the
byte's line / col.

## Trivia

Skipped before every token; not represented in the token
stream.

| Form | Kind |
|---|---|
| ` ` `\t` `\r` `\n` | whitespace |
| `// …` (to end-of-line) | line comment |
| `/* … */` (non-nested) | block comment |

Unterminated block comment → `ImportError::Lex` at the opening
`/*`.

## Token grammar

```
Token  ::= Keyword | ScalarType | Ident | IntLit | FloatLit
         | StringLit | Punct

Keyword     ::= "edition" | "package" | "import" | "option"
              | "message" | "enum"    | "oneof"  | "repeated"
              | "map"     | "service" | "rpc"    | "returns"
              | "reserved"| "to"      | "max"
              | "true"    | "false"

ScalarType  ::= "int32"   | "int64"   | "uint32" | "uint64"
              | "sint32"  | "sint64"
              | "fixed32" | "fixed64" | "sfixed32"|"sfixed64"
              | "float"   | "double"
              | "bool"    | "string"  | "bytes"

Ident       ::= [A-Za-z_] [A-Za-z0-9_]*
              (** not classified as Keyword or ScalarType **)

IntLit      ::= "0" [xX] HexDigit+            // hex
              | "0"                            // bare zero
              | [1-9] [0-9]*                   // decimal

FloatLit    ::= [0-9]+ "." [0-9]* Exponent?
              | "." [0-9]+      Exponent?
              | [0-9]+          Exponent
Exponent    ::= [eE] [+-]? [0-9]+

StringLit   ::= '"' (StringByte | Escape)* '"'
              | "'" (StringByte | Escape)* "'"
StringByte  ::= <any UTF-8 byte except the matching quote, '\\', or '\n'>
Escape      ::= "\\" ("n" | "t" | "r" | "0" | "\\" | "'" | "\"")

Punct       ::= "{" | "}" | "(" | ")" | "[" | "]" | "<" | ">"
              | ";" | "," | "=" | "." | ":" | "-" | "+"
```

### Out-of-scope at v0.2 (lexer-level)

| Construct | Why deferred |
|---|---|
| Octal IntLit (`0o…` or C-style `0[0-7]+`) | Not standard in protobuf editions; rare in practice. |
| `\xHH` / `\uHHHH` / `\UHHHHHHHH` string escapes | No realistic `.proto` schema needs non-ASCII default values. Defer to v0.7. |
| Adjacent-string-literal concatenation | Each `"…"` is a separate `StringLit`; v0.3+ parser concatenates if needed. |
| Raw newlines inside string literals | Reject as `ImportError::Lex` — same as Rust's tokenizer. |
| `+inf` / `-inf` / `nan` float keywords | Treated as `Punct(Plus/Minus) + Ident("inf"/"nan")` for now; v0.4+ semantic analyser can recognise the pair. |

## Syntax-line gate (v0.2 lexer-level)

Editions 2023+ requires the first non-trivia construct to be:

```
edition = "2023" ;
```

Lexer-level enforcement:

1. After skipping leading trivia, inspect the first four
   tokens of the token stream.
2. If the first token is `Token::Ident("syntax")` → the value
   string selects the translation dialect: `"proto3"` →
   `Dialect::Proto3` (1.3, DX stage A), `"proto2"` →
   `Dialect::Proto2` (1.5, DX stage C). The parser translates
   both onto the editions-2023-shaped model; see `Dialect` in
   `lib.rs` for the per-dialect notes.
3. Else if the first token is `Token::Keyword(Edition)`:
   - Expect `Token::Punct(Equals)` next; else
     `ImportError::Lex` at that token's span (or end-of-input).
   - Expect `Token::StringLit(s)` next; if `s != "2023"`,
     `ImportError::UnsupportedFeature("edition must be \"2023\"
     — only editions 2023+ (currently 2023) supported")`.
   - Expect `Token::Punct(Semicolon)` next; else
     `ImportError::Lex`.
4. Else (any other first token, or no tokens at all) →
   `ImportError::Lex` at the offending span (or `{ line: 1,
   col: 1 }` for empty input).

Future-proofing: when editions 2024+ ships, step 3's
`UnsupportedFeature` arm becomes a multi-value check; the
`Edition` ADT in `lib.rs` gains a new variant.

## Token-position invariants

Every `Spanned` carries `line` and `col`, both 1-based,
stamped at the **first byte** of the token. Whitespace and
comments don't bump the column past their last consumed byte;
the next token's column is `col_of_first_byte`. `\n` resets
column to 1 and bumps line by 1.

## Examples (informative)

### Minimal valid `.proto` (editions 2023)

```proto
edition = "2023";
message Empty {}
```

Token stream after `tokenize`:

```text
Keyword(Edition)    line 1 col 1
Punct(Equals)       line 1 col 9
StringLit("2023")   line 1 col 11
Punct(Semicolon)    line 1 col 17
Keyword(Message)    line 2 col 1
Ident("Empty")      line 2 col 9
Punct(LBrace)       line 2 col 15
Punct(RBrace)       line 2 col 16
```

### Accept: proto3 / proto2 input (1.3 / 1.5)

```proto
syntax = "proto3";
message Foo {}
```

`tokenize` → clean token stream with `Dialect::Proto3`
(`"proto2"` → `Dialect::Proto2`); the parser translates the
source onto the editions-2023-shaped model. Historical note:
v0.2–1.2 rejected both here with `UnsupportedFeature`.

## Parser productions (v0.3+)

The parser walks the token stream produced by `tokenize` and
builds the public ADTs declared in `lib.rs`. The grammar
below is the v0.3 accept surface. Parse-and-discard / reject
behaviour is documented in
`../../REFACTOR-v8-v0.3-parser.md` § Scope.

```
Schema       ::= EditionLine TopLevel*
EditionLine  ::= "edition" "=" '"2023"' ";"         // gated at lex

TopLevel     ::= PackageDecl
              | ImportDecl
              | MessageDecl
              | EnumDecl
              | OptionDecl       // parse-and-discard
              | ServiceDecl      // parse-and-discard
              | ";"              // tolerated stray semicolon

PackageDecl  ::= "package" DottedIdent ";"
ImportDecl   ::= "import" ("weak" | "public")? StringLit ";"

MessageDecl  ::= "message" Ident "{" MessageMember* "}"
MessageMember::= FieldDecl
              | "message" Ident "{" MessageMember* "}"
              | "enum"    Ident "{" EnumMember* "}"
              | OneofBlock        // fields flattened into the parent
              | OptionDecl        // parse-and-discard
              | ReservedDecl      // parse-and-discard
              | ";"               // tolerated stray semicolon

OneofBlock   ::= "oneof" Ident "{" (FieldDecl | OptionDecl | ";")* "}"

FieldDecl    ::= "repeated"? FieldType Ident "=" IntLit FieldOptions? ";"
FieldType    ::= ScalarType
              | "map" "<" FieldType "," FieldType ">"
              | NamedRef
NamedRef     ::= "."? Ident ("." Ident)*

FieldOptions ::= "[" <any tokens skipped via balanced brackets> "]"

EnumDecl     ::= "enum" Ident "{" EnumMember* "}"
EnumMember   ::= EnumValue
              | OptionDecl       // parse-and-discard
              | ReservedDecl     // parse-and-discard
              | ";"

EnumValue    ::= Ident "=" SignedIntLit FieldOptions? ";"
SignedIntLit ::= ("-" | "+")? IntLit

ReservedDecl ::= "reserved" <any tokens skipped to ";"> ";"
OptionDecl   ::= "option"   <any tokens skipped to ";"> ";"
ServiceDecl  ::= "service" Ident "{" <any tokens skipped via balanced braces> "}"

DottedIdent  ::= Ident ("." Ident)*
```

### Named-type-ref convention

The parser emits `FieldType::Message(name)` for **every**
named type reference. The semantic analyser (v0.4) flips
references whose name resolves to an `enum` over to
`FieldType::Enum(name)`. Until v0.4 lands, enum-typed fields
appear as `FieldType::Message(name)` in the AST.

### `oneof` collapse convention

`oneof` members are appended to the enclosing
`Message.fields` as `Label::Singular`; the oneof container
itself is not represented. First-class `oneof` modelling is
deferred to v1.x.

### Field-number / enum-value bounds

The parser only checks **type** bounds:
- Field numbers must fit `u32` (lexer-level `IntLit` is `u64`).
- Enum values must fit `i32` (parser applies the optional
  sign).

`.proto` editions 2023 **range** rules
(`1..=536870911 \ 19000..=19999` for field numbers; first
enum value must be 0 for "open" enums) are enforced at the
v0.4 semantic-analysis stage.

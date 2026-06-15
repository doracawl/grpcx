//! `proto!` / `proto_inline!` — `.proto` → typed Rust at
//! compile time, via the in-process `grpcx-pb` compiler.
//! See README.md for the tonic-build comparison.

use proc_macro::TokenStream;

/// Compile a `.proto` file (path relative to
/// `CARGO_MANIFEST_DIR`) and expand the generated items
/// in place.
///
/// ```ignore
/// grpcx_pb_derive::proto!("proto/greeter.proto");
/// ```
#[proc_macro]
pub fn proto(input: TokenStream) -> TokenStream {
    let Some(rel) = parse_str_literal(&input) else {
        return compile_error(
            "proto! expects one string literal: a .proto path relative to CARGO_MANIFEST_DIR",
        );
    };
    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(d) => d,
        Err(_) => return compile_error("CARGO_MANIFEST_DIR not set"),
    };
    let path = std::path::Path::new(&manifest_dir).join(&rel);
    let source = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return compile_error(&format!("proto!: cannot read {}: {e}", path.display()));
        }
    };
    expand(&source, &rel)
}

/// Compile `.proto` source given inline as a string
/// literal.
#[proc_macro]
pub fn proto_inline(input: TokenStream) -> TokenStream {
    let Some(source) = parse_str_literal(&input) else {
        return compile_error("proto_inline! expects one string literal of .proto source");
    };
    expand(&source, "<inline>")
}

fn expand(source: &str, origin: &str) -> TokenStream {
    let schema = match grpcx_pb::import(source) {
        Ok(s) => s,
        Err(e) => return compile_error(&import_error_message(&e, origin)),
    };
    let rust = grpcx_pb::codegen(&schema);
    match rust.parse::<TokenStream>() {
        Ok(ts) => ts,
        Err(e) => compile_error(&format!(
            "proto!: generated code failed to tokenize (grpcx-pb bug — please report): {e}"
        )),
    }
}

/// Render an `ImportError` with the proto origin +
/// line/col when the stage carries them.
fn import_error_message(e: &grpcx_pb::ImportError, origin: &str) -> String {
    use grpcx_pb::ImportError as E;
    match e {
        E::Lex { line, col } => format!("{origin}:{line}:{col}: .proto lex error"),
        E::Parse { line, col } => format!("{origin}:{line}:{col}: .proto parse error"),
        E::Semantic(msg) => format!("{origin}: {msg}"),
        E::UnsupportedFeature(msg) => format!("{origin}: {msg}"),
    }
}

/// Extract the sole string-literal argument without syn:
/// accepts `"…"` and raw `r"…"` / `r#"…"#` forms.
fn parse_str_literal(input: &TokenStream) -> Option<String> {
    let mut iter = input.clone().into_iter();
    let lit = match iter.next()? {
        proc_macro::TokenTree::Literal(l) => l.to_string(),
        _ => return None,
    };
    if iter.next().is_some() {
        return None;
    }
    if let Some(stripped) = lit.strip_prefix('"') {
        // Plain string literal: unescape the common cases.
        let body = stripped.strip_suffix('"')?;
        let mut out = String::with_capacity(body.len());
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    '0' => out.push('\0'),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            } else {
                out.push(c);
            }
        }
        Some(out)
    } else if let Some(after_r) = lit.strip_prefix('r') {
        // Raw literal: r"…" or r#"…"# (any guard depth).
        let hashes = after_r.chars().take_while(|&c| c == '#').count();
        let open = 1 + hashes + 1; // r + hashes + quote
        let close = lit.len() - 1 - hashes;
        (close >= open).then(|| lit[open..close].to_string())
    } else {
        None
    }
}

fn compile_error(msg: &str) -> TokenStream {
    let escaped = msg.replace('\\', "\\\\").replace('"', "\\\"");
    format!("compile_error!(\"{escaped}\");")
        .parse()
        .expect("compile_error tokenizes")
}

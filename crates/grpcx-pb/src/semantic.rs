// Crate-private semantic analyser. Walks the v0.3 parsed
// `Schema` and:
//
// - Builds a flat symbol table of every message + enum, keyed
//   by fully-qualified name (FQN).
// - Resolves every `FieldType::Message(name)` against that
//   table using protobuf editions 2023 walk-up scoping;
//   reclassifies to `FieldType::Enum(fqn)` when the name
//   points at an enum. Rewrites the carried name to its FQN.
// - Range-checks every field number (1..=536_870_911 minus
//   19000..=19999 reserved).
// - Rejects duplicate field names / field numbers within a
//   message and duplicate value names within an enum.
//
// All semantic errors surface as
// `ImportError::Semantic(&'static str)`. v0.4 doesn't carry
// dynamic context; v1.x can expand the variant if the error
// UX needs upgrading.
//
// Out of scope at v0.4 (see `../../REFACTOR-v8-v0.4-semantic.md`):
// enum-first-value-zero / `allow_alias` / cross-file imports
// / map key scalar restriction / reserved field-NAME
// enforcement.

use crate::{Enum, FieldType, ImportError, Message, Schema};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};

pub(crate) fn analyse(schema: &mut Schema) -> Result<(), ImportError> {
    // proto3 constraint: the first value of every enum must
    // be zero (it doubles as the implicit default).
    if schema.source_dialect == crate::Dialect::Proto3 {
        for en in &schema.enums {
            if let Some(first) = en.values.first()
                && first.number != 0
            {
                return Err(ImportError::Semantic(
                    "proto3 requires the first enum value to be zero",
                ));
            }
        }
    }

    let table = build_symbol_table(schema);
    walk_and_resolve(schema, &table)?;
    check_duplicates_and_ranges(schema)?;
    Ok(())
}

// ============================================================
// Symbol table.
// ============================================================

#[derive(Debug, Clone, Copy)]
enum SymbolKind {
    Message,
    Enum,
}

fn build_symbol_table(schema: &Schema) -> BTreeMap<String, SymbolKind> {
    let mut table = BTreeMap::new();
    let prefix = schema.package.clone().unwrap_or_default();
    for m in &schema.messages {
        collect_message(&mut table, &prefix, m);
    }
    for e in &schema.enums {
        collect_enum(&mut table, &prefix, e);
    }
    table
}

fn collect_message(table: &mut BTreeMap<String, SymbolKind>, scope: &str, m: &Message) {
    let fqn = join_scope(scope, &m.name);
    // Last-write-wins for genuine duplicates at the same FQN;
    // `check_duplicates_and_ranges` would already catch true
    // collisions through the duplicate-message-name rules if
    // we modelled them. v0.4 doesn't (duplicate top-level /
    // nested message names is a v1.x semantic check); for now
    // the symbol table holds the LAST declaration's kind,
    // which is harmless for resolution.
    table.insert(fqn.clone(), SymbolKind::Message);
    for nm in &m.nested_messages {
        collect_message(table, &fqn, nm);
    }
    for ne in &m.nested_enums {
        collect_enum(table, &fqn, ne);
    }
}

fn collect_enum(table: &mut BTreeMap<String, SymbolKind>, scope: &str, e: &Enum) {
    let fqn = join_scope(scope, &e.name);
    table.insert(fqn, SymbolKind::Enum);
}

fn join_scope(scope: &str, name: &str) -> String {
    let mut s = String::with_capacity(scope.len() + 1 + name.len());
    if !scope.is_empty() {
        s.push_str(scope);
        s.push('.');
    }
    s.push_str(name);
    s
}

// ============================================================
// Resolution.
// ============================================================

/// Look `name` up against `table`, starting from `scope` and
/// walking up one segment at a time. Returns the resolved FQN
/// and its kind, or `None` if the name doesn't resolve.
///
/// A leading `.` on `name` flags a fully-qualified reference;
/// the FQN is `name[1..]` with no walk-up.
fn resolve(
    table: &BTreeMap<String, SymbolKind>,
    scope: &str,
    name: &str,
) -> Option<(String, SymbolKind)> {
    if let Some(stripped) = name.strip_prefix('.') {
        return table.get(stripped).map(|k| (stripped.to_string(), *k));
    }
    let mut s = scope.to_string();
    loop {
        let candidate = join_scope(&s, name);
        if let Some(k) = table.get(&candidate) {
            return Some((candidate, *k));
        }
        if s.is_empty() {
            return None;
        }
        match s.rfind('.') {
            Some(i) => s.truncate(i),
            None => s.clear(),
        }
    }
}

// ============================================================
// Walk + reclassify + FQN rewrite.
// ============================================================

fn walk_and_resolve(
    schema: &mut Schema,
    table: &BTreeMap<String, SymbolKind>,
) -> Result<(), ImportError> {
    let prefix = schema.package.clone().unwrap_or_default();
    for m in &mut schema.messages {
        resolve_in_message(m, &prefix, table)?;
    }
    Ok(())
}

fn resolve_in_message(
    m: &mut Message,
    scope: &str,
    table: &BTreeMap<String, SymbolKind>,
) -> Result<(), ImportError> {
    let fqn = join_scope(scope, &m.name);
    for f in &mut m.fields {
        resolve_field_type(&mut f.ty, &fqn, table)?;
    }
    for nm in &mut m.nested_messages {
        resolve_in_message(nm, &fqn, table)?;
    }
    Ok(())
}

fn resolve_field_type(
    ty: &mut FieldType,
    scope: &str,
    table: &BTreeMap<String, SymbolKind>,
) -> Result<(), ImportError> {
    if let FieldType::Message(name) = ty {
        // Take the name out so the borrow on `ty` ends before
        // we (potentially) overwrite the whole variant.
        let raw = core::mem::take(name);
        let (fqn, kind) = resolve(table, scope, &raw)
            .ok_or(ImportError::Semantic("unresolved type reference"))?;
        match kind {
            SymbolKind::Message => {
                *ty = FieldType::Message(fqn);
            }
            SymbolKind::Enum => {
                *ty = FieldType::Enum(fqn);
            }
        }
    } else if let FieldType::Map { key, value } = ty {
        resolve_field_type(key, scope, table)?;
        resolve_field_type(value, scope, table)?;
    }
    Ok(())
}

// ============================================================
// Duplicate + range checks.
// ============================================================

const FIELD_NUMBER_MAX: u32 = 536_870_911; // 2^29 - 1
const RESERVED_RANGE_LO: u32 = 19_000;
const RESERVED_RANGE_HI: u32 = 19_999;

fn is_valid_field_number(n: u32) -> bool {
    (1..=FIELD_NUMBER_MAX).contains(&n)
}

fn is_reserved_field_number(n: u32) -> bool {
    (RESERVED_RANGE_LO..=RESERVED_RANGE_HI).contains(&n)
}

fn check_duplicates_and_ranges(schema: &Schema) -> Result<(), ImportError> {
    for m in &schema.messages {
        check_message(m)?;
    }
    for e in &schema.enums {
        check_enum(e)?;
    }
    Ok(())
}

fn check_message(m: &Message) -> Result<(), ImportError> {
    let mut seen_names: BTreeSet<&str> = BTreeSet::new();
    let mut seen_numbers: BTreeSet<u32> = BTreeSet::new();
    for f in &m.fields {
        if !is_valid_field_number(f.number) {
            return Err(ImportError::Semantic(
                "field number out of range (1..=536_870_911)",
            ));
        }
        if is_reserved_field_number(f.number) {
            return Err(ImportError::Semantic(
                "field number in reserved range 19000..=19999",
            ));
        }
        if !seen_names.insert(f.name.as_str()) {
            return Err(ImportError::Semantic("duplicate field name in message"));
        }
        if !seen_numbers.insert(f.number) {
            return Err(ImportError::Semantic("duplicate field number in message"));
        }
    }
    for nm in &m.nested_messages {
        check_message(nm)?;
    }
    for ne in &m.nested_enums {
        check_enum(ne)?;
    }
    Ok(())
}

fn check_enum(e: &Enum) -> Result<(), ImportError> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for v in &e.values {
        if !seen.insert(v.name.as_str()) {
            return Err(ImportError::Semantic("duplicate enum value name in enum"));
        }
    }
    Ok(())
}

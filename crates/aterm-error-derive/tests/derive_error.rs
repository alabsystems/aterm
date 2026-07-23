// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the `#[derive(Error)]` proc-macro.
//!
//! A `proc-macro = true` crate cannot host `#[cfg(test)]` unit tests in its own
//! lib, so these live in `tests/`. The macro emits only `::core` / `::std` paths
//! (never an `aterm_*` runtime path), so this test compiles with zero extra
//! dependencies — it exercises the macro purely by USING it and asserting the
//! generated `Display` / `Error` / `From` impls behave correctly.
//!
//! These deliberately target paths NOT already covered by `aterm-error`'s own
//! `src/tests.rs`: thiserror-style explicit format arguments (`#[error("..", expr)]`),
//! positional `{0}`/`{1}` multi-field tuples, the `#[error(name = "...")]`
//! name-value form, multi-field `#[from]` (other fields default), generics, the
//! all-unit "no source" catch-all, struct positional access, and the `.0.method()`
//! / `.field.method()` shorthand rewrites.

use aterm_error_derive::Error;
use std::error::Error as _;

// ── Explicit format args on enum tuple variants (thiserror-style) ────────────
// Covers `build_explicit_args_arm` + `rewrite_dot_field_refs`: the leading-dot
// `.N` shorthand (`.0`, `.1`) is rewritten to the bound `__fieldN`.

#[derive(Debug, Error)]
enum ExplicitTupleError {
    // bare constant expression arg
    #[error("limit is {}", LIMIT)]
    OverLimit,

    // `.0.method()` shorthand -> calls a method on the first field
    #[error("path: {}", .0.as_str())]
    BadPath(String),

    // two explicit args referencing two positional fields out of order
    #[error("got {} but wanted {}", .1, .0)]
    Mismatch(u32, u32),
}

const LIMIT: u32 = 7;

#[test]
fn explicit_args_unit_variant_uses_constant() {
    assert_eq!(ExplicitTupleError::OverLimit.to_string(), "limit is 7");
}

#[test]
fn explicit_args_dot_method_shorthand_on_tuple_field() {
    let e = ExplicitTupleError::BadPath("/etc".into());
    assert_eq!(e.to_string(), "path: /etc");
}

#[test]
fn explicit_args_out_of_order_positional_fields() {
    // {} #1 -> .1 (second field), {} #2 -> .0 (first field)
    let e = ExplicitTupleError::Mismatch(10, 20);
    assert_eq!(e.to_string(), "got 20 but wanted 10");
}

// ── Explicit format args on enum named variants ──────────────────────────────
// Covers `rewrite_dot_named_refs`: `.field.method()` -> `field.method()`, and a
// bare bound field used directly.

#[derive(Debug, Error)]
enum ExplicitNamedError {
    #[error("name {} len {}", .label.to_uppercase(), .label.len())]
    Labelled { label: String },
}

#[test]
fn explicit_args_dot_named_shorthand() {
    let e = ExplicitNamedError::Labelled { label: "hi".into() };
    assert_eq!(e.to_string(), "name HI len 2");
}

// ── Implicit-capture width / precision parameters (`{val:width$}`) ────────────
// REGRESSION: a field used ONLY as a `$` width/precision parameter must be
// detected as referenced (bound + passed). The old substring heuristic missed
// it, so `write!` failed to compile with "there is no argument named width".

#[derive(Debug, Error)]
enum ParamFmtError {
    #[error("[{val:width$}]")]
    Padded { val: u32, width: usize },
    #[error("{val:.prec$}")]
    Rounded { val: f64, prec: usize },
}

#[test]
fn implicit_width_parameter_field_is_passed() {
    let e = ParamFmtError::Padded { val: 7, width: 5 };
    assert_eq!(e.to_string(), "[    7]");
}

#[test]
fn implicit_precision_parameter_field_is_passed() {
    let e = ParamFmtError::Rounded { val: 1.5, prec: 2 };
    assert_eq!(e.to_string(), "1.50");
}

// ── Escaped braces must NOT be treated as a field reference ───────────────────
// REGRESSION: `{{path}}` is the literal text `{path}`, not a reference. A field
// appearing only inside escaped braces must NOT be bound/passed (the old
// heuristic false-matched the `{path}` substring inside `{{path}}`, causing a
// "named argument never used" compile error).

#[derive(Debug, Error)]
enum EscapedBraceError {
    #[error("literal {{path}} here")]
    EscapedOnly {
        // Genuinely unused for Display (only escaped text mentions it); the point
        // is precisely that the macro must NOT bind/pass it.
        #[allow(dead_code)]
        path: String,
    },
    #[error("show {{path}} then {path}")]
    EscapedAndReal { path: String },
}

#[test]
fn field_only_in_escaped_braces_is_not_referenced() {
    let e = EscapedBraceError::EscapedOnly {
        path: "ignored".into(),
    };
    assert_eq!(e.to_string(), "literal {path} here");
}

#[test]
fn escaped_and_real_brace_uses_only_the_real_reference() {
    let e = EscapedBraceError::EscapedAndReal { path: "/x".into() };
    assert_eq!(e.to_string(), "show {path} then /x");
}

// ── Positional {0}/{1} on a multi-field tuple variant (implicit args path) ───
// Covers `build_format_call` positional rewriting and the "only bind referenced
// fields" filter.

#[derive(Debug, Error)]
enum PositionalError {
    #[error("expected {0} found {1}")]
    Expected(u8, u8),

    // reference {1} only: field 0 must still be patterned (with `_`) and not warn
    #[error("second only: {1}")]
    SecondOnly(u8, u8),
}

#[test]
fn positional_both_fields() {
    assert_eq!(
        PositionalError::Expected(1, 2).to_string(),
        "expected 1 found 2"
    );
}

#[test]
fn positional_only_one_referenced_field() {
    assert_eq!(
        PositionalError::SecondOnly(9, 5).to_string(),
        "second only: 5"
    );
}

// ── Name-value form `#[error(name = "literal")]` ─────────────────────────────
// Covers the `Meta::NameValue` arm of `find_error_attr`.

#[derive(Debug, Error)]
enum NameValueError {
    #[error = "static message"]
    Static,
}

#[test]
fn name_value_error_attr() {
    assert_eq!(NameValueError::Static.to_string(), "static message");
}

// ── Multi-field tuple `#[from]` (other fields get Default) ────────────────────
// Covers the multi-field branch of `build_from_impls`: From sets the #[from]
// field and Default::default() for the rest.

#[derive(Debug, Error)]
enum MultiFieldFrom {
    #[error("io with code {1}: {0}")]
    Io(#[from] std::io::Error, u32),
}

#[test]
fn multi_field_from_defaults_other_fields() {
    let io = std::io::Error::other("boom");
    let e: MultiFieldFrom = io.into();
    // second field is u32::default() == 0
    assert_eq!(e.to_string(), "io with code 0: boom");
    // the #[from] field is reported as the source
    assert_eq!(e.source().unwrap().to_string(), "boom");
}

// ── Named-struct `#[from]` (other named fields get Default) ───────────────────
// Covers the named-fields branch of `build_struct_from_impls`.

#[derive(Debug, Error)]
#[error("load failed (retries={retries}): {cause}")]
struct NamedStructFrom {
    #[from]
    cause: std::io::Error,
    retries: u8,
}

#[test]
fn named_struct_from_defaults_other_fields() {
    let io = std::io::Error::other("eof");
    let e: NamedStructFrom = io.into();
    assert_eq!(e.to_string(), "load failed (retries=0): eof");
    assert_eq!(e.source().unwrap().to_string(), "eof");
}

// ── Generic enum: generics flow through impl/ty generics + where-clause ───────

#[derive(Debug, Error)]
enum GenericError<T: std::fmt::Display + std::fmt::Debug> {
    #[error("wrapped: {0}")]
    Wrapped(T),
}

#[test]
fn generic_enum_display() {
    let e: GenericError<i32> = GenericError::Wrapped(99);
    assert_eq!(e.to_string(), "wrapped: 99");
}

// ── All-unit enum: no source field anywhere -> source() catch-all is None ─────
// Covers the `source_arms.is_empty()` branch (body is plain `Option::None`).

#[derive(Debug, Error)]
enum AllUnitError {
    #[error("a")]
    A,
    #[error("b")]
    B,
}

#[test]
fn all_unit_enum_source_is_none() {
    assert!(AllUnitError::A.source().is_none());
    assert!(AllUnitError::B.source().is_none());
    assert_eq!(AllUnitError::A.to_string(), "a");
    assert_eq!(AllUnitError::B.to_string(), "b");
}

// ── Mixed: a variant with #[source] alongside a variant with none ─────────────
// Covers the "some source arms + unreachable catch-all" branch and that a
// non-source variant returns None through the catch-all.

#[derive(Debug, Error)]
enum MixedSourceError {
    #[error("with cause: {msg}")]
    WithCause {
        msg: String,
        #[source]
        cause: std::io::Error,
    },
    #[error("plain {0}")]
    Plain(u32),
}

#[test]
fn mixed_source_variant_reports_source() {
    let e = MixedSourceError::WithCause {
        msg: "x".into(),
        cause: std::io::Error::other("inner"),
    };
    assert_eq!(e.source().unwrap().to_string(), "inner");
}

#[test]
fn mixed_plain_variant_has_no_source() {
    // hits the `#[allow(unreachable_patterns)] _ => None` catch-all arm
    assert!(MixedSourceError::Plain(3).source().is_none());
    assert_eq!(MixedSourceError::Plain(3).to_string(), "plain 3");
}

// ── Struct with positional `{0}` access and a width specifier ─────────────────
// Covers `build_struct_display` unnamed path including `{0:>spec}` rewriting.

#[derive(Debug, Error)]
#[error("[{0:>4}] {1}")]
struct PaddedStruct(u32, &'static str);

#[test]
fn struct_positional_with_width_specifier() {
    let e = PaddedStruct(7, "go");
    assert_eq!(e.to_string(), "[   7] go");
}

// ── Struct transparent: Display + source delegate to the single inner field ──

#[derive(Debug, Error)]
#[error(transparent)]
struct TransparentNamedStruct {
    inner: std::io::Error,
}

#[test]
fn transparent_named_struct_delegates() {
    let e = TransparentNamedStruct {
        inner: std::io::Error::other("delegated"),
    };
    assert_eq!(e.to_string(), "delegated");
    assert!(e.source().is_some());
}

// ── Enum transparent named variant (single named field) ──────────────────────

#[derive(Debug, Error)]
enum TransparentNamedVariant {
    #[error(transparent)]
    Inner { source: std::io::Error },
}

#[test]
fn transparent_named_variant_delegates() {
    let e = TransparentNamedVariant::Inner {
        source: std::io::Error::other("named-delegated"),
    };
    assert_eq!(e.to_string(), "named-delegated");
    assert!(e.source().is_some());
}

// ── Debug `{:?}` on a multi-field tuple via positional `{1:?}` ────────────────

#[derive(Debug, Error)]
enum DebugPositional {
    #[error("k={0} v={1:?}")]
    Pair(u8, &'static str),
}

#[test]
fn debug_positional_specifier() {
    assert_eq!(DebugPositional::Pair(1, "x").to_string(), r#"k=1 v="x""#);
}

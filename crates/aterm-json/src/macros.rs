// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `json!` literal macro.

/// Build a [`Value`](crate::Value) from JSON-shaped syntax.
///
/// ```
/// let body = aterm_json::json!({
///     "model": "qwen",
///     "messages": [{"role": "user", "content": "hi"}],
///     "stream": false,
///     "options": {"temperature": 0},
/// });
/// assert_eq!(body.get("stream").and_then(aterm_json::Value::as_bool), Some(false));
/// ```
///
/// `null`, `true` and `false` are recognised as JSON literals; `[...]` and
/// `{...}` nest; anything else is a Rust expression, converted through
/// [`to_value`](crate::to_value). Keys are single token trees — a string
/// literal in every real use — and a trailing comma is allowed.
#[macro_export]
macro_rules! json {
    ($($body:tt)+) => {
        $crate::json_build!($($body)+)
    };
}

/// The muncher behind [`json!`]. Not part of the public surface; it is
/// `#[macro_export]`ed only because `json!` expands to it at the call site.
#[doc(hidden)]
#[macro_export]
macro_rules! json_build {
    // ── literals ───────────────────────────────────────────────────────────
    (null) => { $crate::Value::Null };
    (true) => { $crate::Value::Bool(true) };
    (false) => { $crate::Value::Bool(false) };

    // ── arrays ─────────────────────────────────────────────────────────────
    ([]) => { $crate::Value::Array(::std::vec::Vec::new()) };
    ([ $($items:tt)+ ]) => {
        $crate::Value::Array($crate::json_build!(@array [] $($items)+))
    };

    // ── objects ────────────────────────────────────────────────────────────
    ({}) => { $crate::Value::Object($crate::Map::new()) };
    ({ $($entries:tt)+ }) => {{
        let mut map = $crate::Map::new();
        $crate::json_build!(@object map $($entries)+);
        $crate::Value::Object(map)
    }};

    // ── anything else is a Rust expression ─────────────────────────────────
    //
    // PANICS on a value that cannot serialize, which is what `serde_json`'s
    // `json!` does (`.unwrap()`), and deliberately not what this used to do:
    // `unwrap_or(Value::Null)` turned a map with a non-string key, or an
    // integer too wide for the model, into a document that quietly carried
    // `null` where the field should have been — in the LLM request-body builder
    // and the control-payload builder. A loud failure at the call site is the
    // only honest answer for a macro with no way to return one.
    ($other:expr) => {
        $crate::to_value(&$other).expect("json! value must serialize")
    };

    // ── array muncher ──────────────────────────────────────────────────────
    // Done: hand back the accumulated element vector.
    (@array [$($done:expr,)*]) => { ::std::vec![$($done,)*] };
    // A separating comma between elements.
    (@array [$($done:expr,)*] , $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)*] $($rest)*)
    };
    // The JSON literals, which must be matched before the `expr` arms — `null`
    // is a perfectly good Rust path expression and would otherwise be taken as
    // an (undefined) identifier.
    (@array [$($done:expr,)*] null $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::Value::Null,] $($rest)*)
    };
    (@array [$($done:expr,)*] true $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::Value::Bool(true),] $($rest)*)
    };
    (@array [$($done:expr,)*] false $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::Value::Bool(false),] $($rest)*)
    };
    // Nested array / object, matched as a single token tree so their contents
    // are re-munched as JSON rather than parsed as a Rust block.
    (@array [$($done:expr,)*] [$($inner:tt)*] $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::json_build!([$($inner)*]),] $($rest)*)
    };
    (@array [$($done:expr,)*] {$($inner:tt)*} $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::json_build!({$($inner)*}),] $($rest)*)
    };
    // A Rust expression element, with or without a following comma.
    (@array [$($done:expr,)*] $next:expr, $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::json_build!($next),] $($rest)*)
    };
    (@array [$($done:expr,)*] $last:expr) => {
        $crate::json_build!(@array [$($done,)* $crate::json_build!($last),])
    };

    // ── object muncher ─────────────────────────────────────────────────────
    (@object $map:ident) => {};
    (@object $map:ident , $($rest:tt)*) => {
        $crate::json_build!(@object $map $($rest)*)
    };
    (@object $map:ident $key:tt : null $($rest:tt)*) => {
        $map.insert(($key).into(), $crate::Value::Null);
        $crate::json_build!(@object $map $($rest)*)
    };
    (@object $map:ident $key:tt : true $($rest:tt)*) => {
        $map.insert(($key).into(), $crate::Value::Bool(true));
        $crate::json_build!(@object $map $($rest)*)
    };
    (@object $map:ident $key:tt : false $($rest:tt)*) => {
        $map.insert(($key).into(), $crate::Value::Bool(false));
        $crate::json_build!(@object $map $($rest)*)
    };
    (@object $map:ident $key:tt : [$($inner:tt)*] $($rest:tt)*) => {
        $map.insert(($key).into(), $crate::json_build!([$($inner)*]));
        $crate::json_build!(@object $map $($rest)*)
    };
    (@object $map:ident $key:tt : {$($inner:tt)*} $($rest:tt)*) => {
        $map.insert(($key).into(), $crate::json_build!({$($inner)*}));
        $crate::json_build!(@object $map $($rest)*)
    };
    (@object $map:ident $key:tt : $value:expr, $($rest:tt)*) => {
        $map.insert(($key).into(), $crate::json_build!($value));
        $crate::json_build!(@object $map $($rest)*)
    };
    (@object $map:ident $key:tt : $value:expr) => {
        $map.insert(($key).into(), $crate::json_build!($value));
    };
}

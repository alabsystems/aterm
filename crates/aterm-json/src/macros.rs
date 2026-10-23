// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `json!` literal macro.

/// Build a [`Value`](crate::Value) from JSON-shaped syntax.
///
/// # Examples
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
#[macro_export]
macro_rules! json {
    ($($body:tt)+) => {
        $crate::json_build!($($body)+)
    };
}

/// Internal token-tree muncher for [`json!`].
#[doc(hidden)]
#[macro_export]
macro_rules! json_build {
    (null) => { $crate::Value::Null };
    (true) => { $crate::Value::Bool(true) };
    (false) => { $crate::Value::Bool(false) };

    ([]) => { $crate::Value::Array(::std::vec::Vec::new()) };
    ([ $($items:tt)+ ]) => {
        $crate::Value::Array($crate::json_build!(@array [] $($items)+))
    };

    ({}) => { $crate::Value::Object($crate::Map::new()) };
    ({ $($entries:tt)+ }) => {{
        let mut map = $crate::Map::new();
        $crate::json_build!(@object map $($entries)+);
        $crate::Value::Object(map)
    }};

    ($other:expr) => {
        $crate::to_value(&$other).expect("json! value must serialize")
    };

    (@array [$($done:expr,)*]) => { ::std::vec![$($done,)*] };
    (@array [$($done:expr,)*] , $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)*] $($rest)*)
    };
    (@array [$($done:expr,)*] null $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::Value::Null,] $($rest)*)
    };
    (@array [$($done:expr,)*] true $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::Value::Bool(true),] $($rest)*)
    };
    (@array [$($done:expr,)*] false $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::Value::Bool(false),] $($rest)*)
    };
    (@array [$($done:expr,)*] [$($inner:tt)*] $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::json_build!([$($inner)*]),] $($rest)*)
    };
    (@array [$($done:expr,)*] {$($inner:tt)*} $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::json_build!({$($inner)*}),] $($rest)*)
    };
    (@array [$($done:expr,)*] $next:expr, $($rest:tt)*) => {
        $crate::json_build!(@array [$($done,)* $crate::json_build!($next),] $($rest)*)
    };
    (@array [$($done:expr,)*] $last:expr) => {
        $crate::json_build!(@array [$($done,)* $crate::json_build!($last),])
    };

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

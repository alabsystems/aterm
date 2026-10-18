// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Strict-gate log shims.
//!
//! `aterm_log::warn!` expands `format_args!` at the call site, and with runtime
//! arguments that expansion embeds unsafe `fmt::Arguments` construction. Under
//! the Trust lane (`ship trust run`) a function containing it loses the "no unsafe →
//! memory-safe warning" demotion, so any unlowerable construct elsewhere in it
//! (a `Cow`, a `VecDeque` call) becomes a hard error. These `#[inline(never)]`
//! shims confine the expansion to one tiny function; callers pre-compose their
//! message (see `error::dec_string`). Only the record's `module_path!()` and
//! `file!()` metadata change.

/// Log a pre-composed message at warn level.
#[inline(never)]
pub(crate) fn warn_str(msg: &str) {
    aterm_log::warn!("{msg}");
}

// No error-interpolating shim on purpose: a `format_args!` argument with a local
// `Display` impl (e.g. `ScrollbackError`, whose decimal arms call
// `error::dec_string`) makes the full verifier traverse that impl, and its CHC
// engine does not terminate on the decimal loop's division. Callers that must
// interpolate an error keep a direct `warn!` and carry the full-verify gap.

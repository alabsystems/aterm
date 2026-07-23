// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Strict-gate log shims.
//!
//! `aterm_log::warn!` expands `format_args!` at the call site, and with
//! runtime arguments that expansion embeds unsafe `fmt::Arguments` argument
//! construction. Under the strict Trust gate a function that contains that
//! macro-expanded unsafe loses the "no unsafe → memory-safe warning" demotion,
//! so any unlowerable construct elsewhere in the same function (a `Cow`, a
//! `VecDeque` call, ...) escalates to a hard build error.
//!
//! These `#[inline(never)]` shims confine the `format_args!` expansion to one
//! tiny function whose body the verifier can process, so callers pre-compose
//! their message (see `error::dec_string`) and stay free of macro-expanded
//! unsafe. The emitted log records are identical: same level, same rendered
//! text — only `module_path!()`/`file!()` metadata now name this module, which
//! no behavior depends on.

/// Log a pre-composed message at warn level.
#[inline(never)]
pub(crate) fn warn_str(msg: &str) {
    aterm_log::warn!("{msg}");
}

// NOTE: no error-interpolating shim on purpose. A shim whose `format_args!`
// argument has a LOCAL `Display` impl (e.g. `ScrollbackError`, whose `Io` arm
// delegates to `std::io::Error` and whose decimal arms call
// `error::dec_string`) makes the full verifier traverse that impl as a local
// callee, and its CHC engine then grinds without terminating on the decimal
// loop's division (observed twice, 40+ CPU-minutes, killed). Callers that must
// interpolate an error keep a direct `warn!` and carry the documented
// full-verify gap on themselves instead.

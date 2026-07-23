// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm-update-core` — the artifact-agnostic, portable POSIX primitives shared by
//! every aterm self-update / package flow.
//!
//! This crate holds the parts of the updater that are NOT tied to the macOS `.app`
//! shape: resolving the GitHub release source (owner/repo) under
//! env > config > default precedence with a URL-safety allowlist; an advisory
//! [`FileLock`] and a [`same_volume`] check; authenticated `curl` plumbing to the
//! private GitHub Releases API ([`api_get`], [`download_bytes`], [`download_to`]); the
//! per-machine [`token`] resolution chain; private-dir hardening
//! ([`ensure_private_dir`]); a `shasum`-backed [`sha256_file`]; and a generic
//! compile-time-pin idiom ([`compile_time_pin!`] + [`pin_active`]).
//!
//! `aterm-update` layers the macOS-only pieces (DMG mount/extract, codesign/spctl
//! verification, the atomic `RENAME_SWAP` bundle exchange + re-exec, the `.app`
//! staging layout) on top of these. No crypto crate is pulled in — `sha256_file`
//! shells `/usr/bin/shasum`, matching the release scripts.

// Under the Trust verifier, register the `trust` tool namespace so the
// `#[cfg_attr(trust_verify, trust::skip)]` opt-out on `Manifest::parse`
// resolves; plain rustc never sets `trust_verify`, so this is inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub mod manifest;
pub mod token;

mod hash;
mod http;
mod privatedir;
mod sentinel;
mod source;
mod sys;

pub use hash::sha256_file;
pub use http::{api_get, download_bytes, download_to};
pub use manifest::{Manifest, SUPPORTED_SCHEMA};
pub use privatedir::ensure_private_dir;
pub use sentinel::Sentinel;
pub use source::{DEFAULT_OWNER, DEFAULT_REPO, Source, is_valid_slug, pick_slug};
pub use sys::{FileLock, same_volume};

/// Read a compile-time pin from the named build environment variable, or `""` if it
/// was unset at build time. Generic over the variable name so each consumer pins a
/// different anchor (e.g. an Apple Team ID). Crucially, `option_env!` expands in the
/// crate that INVOKES the macro, so the pin reads that crate's OWN build env — not
/// this crate's — keeping the value identical to an inline `option_env!` const.
///
/// An empty pin is the fail-closed default: with no anchor compiled in there is
/// nothing to trust, so [`pin_active`] reports inactive and the consumer stays inert.
#[macro_export]
macro_rules! compile_time_pin {
    ($v:literal) => {
        match option_env!($v) {
            Some(x) => x,
            None => "",
        }
    };
}

/// Whether a compile-time pin is active: it must be non-empty (an anchor was baked
/// in at build time) AND the user must not have opted out via `opt_out_env`. Fail
/// closed — an empty pin is never active.
#[must_use]
pub fn pin_active(pin: &str, opt_out_env: &str) -> bool {
    !pin.is_empty() && std::env::var_os(opt_out_env).is_none()
}

/// Emit a non-fatal updater warning to the app log. Routed through `aterm_log` (the
/// global logger `aterm-gui` installs), with the same `aterm-update:` prefix the
/// rest of the updater uses, so the output is unchanged. A no-op if no logger is
/// installed (e.g. a dev harness). Used by [`token::resolve`]'s file-mode check.
pub(crate) fn warn(msg: &str) {
    aterm_log::warn!("aterm-update: {msg}");
}

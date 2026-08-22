// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm-update-core` — the artifact-agnostic, portable POSIX primitives shared by
//! every aterm self-update / package flow.
//!
//! This crate holds the parts of the updater that are NOT tied to the macOS `.app`
//! shape: resolving the GitHub release source (owner/repo) under
//! env > config > default precedence with a URL-safety allowlist; an advisory
//! [`FileLock`] and a [`same_volume`] check; token-optional `curl` plumbing to the
//! GitHub Releases API ([`api_get`], [`api_get_conditional`], [`download_bytes`],
//! [`download_to`] — anonymous when no token is provisioned, so a PUBLIC channel needs
//! no credential; conditional so a steady-state check costs a 304 instead of the whole
//! release history); the
//! per-machine [`token`] resolution chain; private-dir hardening
//! ([`ensure_private_dir`]); a `shasum`-backed [`sha256_file`]; the release-tag
//! grammar ([`tag`]) the publisher and the updater client BOTH classify with, so
//! they cannot disagree about which releases are candidates; the trust anchors
//! ([`pins`] — committed constants, never build-environment state); and the
//! master-signed machine [`roster`] that turns one paper master key into per-machine
//! signing authority with attribution and revocation.
//!
//! `aterm-update` layers the macOS-only pieces (DMG mount/extract, codesign/spctl
//! verification, the atomic `RENAME_SWAP` bundle exchange + re-exec, the `.app`
//! staging layout) on top of these. Content hashing still shells `/usr/bin/shasum`
//! (`sha256_file`), matching the release scripts; the one crypto dependency is
//! `ring`'s verify-only Ed25519, used by [`roster`] and shared with the owner-side
//! minting tool so producer and client cannot drift on what a valid chain is.

// Under the Trust verifier, register the `trust` tool namespace so the
// `#[cfg_attr(trust_verify, trust::skip)]` opt-out on `Manifest::parse`
// resolves; plain rustc never sets `trust_verify`, so this is inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub mod manifest;
pub mod pins;
pub mod roster;
pub mod tag;
pub mod token;

mod hash;
mod http;
mod privatedir;
mod sentinel;
mod source;
mod sys;

pub use hash::sha256_file;
pub use http::{
    ApiResponse, HttpError, RELEASE_ASSET_DOWNLOAD_BOUND, api_get, api_get_classified,
    api_get_conditional, download_bytes, download_error_is_rate_limit, download_to,
    download_to_resumable, validator_safe,
};
pub use manifest::{Manifest, SUPPORTED_SCHEMA};
pub use privatedir::ensure_private_dir;
pub use roster::{Attribution, Machine, Roster, RosterReject, VerifiedRoster, verify_roster};
pub use sentinel::Sentinel;
pub use source::{
    ATPKG_INDEX_OWNER, DEFAULT_OWNER, DEFAULT_REPO, PUBLISH_OWNER, PUBLISH_REPO, Source,
    is_valid_slug, pick_slug,
};
pub use sys::{FileLock, same_volume};

/// Emit a non-fatal updater warning to the app log. Routed through `aterm_log` (the
/// global logger `aterm-gui` installs), with the same `aterm-update:` prefix the
/// rest of the updater uses, so the output is unchanged. A no-op if no logger is
/// installed (e.g. a dev harness). Used by [`token::resolve`]'s file-mode check.
pub(crate) fn warn(msg: &str) {
    aterm_log::warn!("aterm-update: {msg}");
}

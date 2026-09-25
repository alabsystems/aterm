// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm-update-core` — the artifact-agnostic, portable POSIX primitives shared by
//! every aterm self-update / package flow.
//!
//! This crate holds the parts of the updater that are NOT tied to the macOS `.app`
//! shape: resolving the GitHub release source (owner/repo) — the compiled channel, which
//! only a development build may repoint — with a URL-safety allowlist; the one switch a
//! person has over the app updater ([`settings`], `[update] enabled`); an advisory
//! [`FileLock`] and a [`same_volume`] check; `curl` plumbing for two GitHub hosts —
//! the unmetered web host every public-channel byte moves over ([`cdn`] derives the
//! tag-specific `github.com/…/releases/download/…` URL, [`pointer`] learns the newest
//! release's tag from ONE redirect-refusing HEAD of `…/releases/latest/download/…`,
//! [`download_bytes`]/[`download_to`] fetch with no credential ever attached), and the
//! Releases API GET ([`api_get`], [`api_get_classified`] — a token, when a caller has
//! one, on stdin, never argv); private-dir hardening ([`ensure_private_dir`]); a
//! `shasum`-backed [`sha256_file`]; the release-tag grammar ([`tag`]) the publisher
//! and the updater client BOTH classify with, so they cannot disagree about which
//! releases are candidates; the trust anchors
//! ([`pins`] — committed constants, never build-environment state); the
//! master-signed machine [`roster`] that turns one paper master key into per-machine
//! signing authority with attribution and revocation; and the Developer ID requirement
//! and its bounded `/usr/bin/codesign` check ([`codesign`]), shared by the app updater
//! and atpkg's staged agent builds.
//!
//! `aterm-update` layers the macOS-only pieces (DMG mount/extract, spctl
//! assessment, the atomic `RENAME_SWAP` bundle exchange + re-exec, the `.app`
//! staging layout) on top of these. Linux content hashing uses `ring` directly;
//! other platforms retain their native command-backed hashing. The same crypto
//! dependency provides Ed25519 verification, used by [`roster`] and shared with the owner-side
//! minting tool so producer and client cannot drift on what a valid chain is.

// Under the Trust verifier, register the `trust` tool namespace so the
// `#[cfg_attr(trust_verify, trust::skip)]` opt-out on `Manifest::parse`
// resolves; plain rustc never sets `trust_verify`, so this is inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub mod cdn;
pub mod codesign;
pub mod linux;
pub mod manifest;
pub mod pins;
pub mod pkg_check;
pub mod pointer;
pub mod roster;
pub mod seal_guard;
pub mod settings;
pub mod tag;

mod hash;
mod http;
mod privatedir;
mod sentinel;
mod source;
mod sys;

pub use hash::sha256_file;
pub use http::{
    HeadAnswer, HttpError, RELEASE_ASSET_DOWNLOAD_BOUND, VendorResponse, api_get,
    api_get_classified, download_bytes, download_error_is_not_found, download_error_is_rate_limit,
    download_to, download_to_resumable, download_to_resumable_https_only, head_no_redirect,
    head_no_redirect_quick, vendor_content_length, vendor_download_to, vendor_get, vendor_get_hint,
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
/// installed (e.g. a dev harness).
pub(crate) fn warn(msg: &str) {
    aterm_log::warn!("aterm-update: {msg}");
}

/// An unasked notice from the package manager (atpkg) as a record in the host's log —
/// the sink `atpkg::notice` uses once a host (the terminal session, the window) has routed
/// them there, because nothing prints into a shell the user did not ask to update (Phase 2
/// of `docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`). atpkg links no logger of
/// its own; this crate's `aterm_log` edge is one it already has. Both hosts install a file
/// logger (`aterm.log`); where none is installed the record is discarded.
pub fn log_pkg_note(msg: &str) {
    aterm_log::warn!("atpkg: {msg}");
}

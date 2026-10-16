// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The artifact-agnostic update primitives shared by every aterm self-update and package flow.
//!
//! Here: the GitHub release [`Source`] (the compiled channel, repointable only in a development
//! build) with a URL-safety allowlist; the `[update] enabled` switch ([`settings`]); an
//! advisory [`FileLock`] and [`same_volume`]; `curl` plumbing for GitHub's web host ([`cdn`]
//! derives a tag's download URL, [`pointer`] learns the newest tag from one redirect-refusing
//! HEAD, [`download_bytes`]/[`download_to`] never attach a credential) and for vendor-direct
//! hosts ([`vendor_get`]); [`ensure_private_dir`]; [`sha256_file`]; the release-tag grammar
//! ([`tag`]) the publisher and the updater both classify with; the trust anchors ([`pins`]);
//! the master-signed machine [`roster`]; and the Developer ID requirement with its bounded
//! `/usr/bin/codesign` check ([`codesign`]).
//!
//! `aterm-update` layers the macOS `.app` pieces (DMG mount, spctl, the `RENAME_SWAP` bundle
//! exchange and re-exec, the staging layout) on top. Roster verification uses `ring`, shared
//! with the owner-side minting tool so producer and client cannot drift on a valid chain.

// Under the Trust verifier, register the `trust` tool namespace so the crate's
// `#[cfg_attr(trust_verify, trust::skip)]` opt-outs resolve; inert under plain rustc.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub mod cdn;
pub mod codesign;
/// The successor's signed handoff policy: the release cutter seals it into
/// the bundle, the outgoing build reads it from the verified candidate.
pub mod handoff_policy;
pub mod linux;
pub mod manifest;
pub mod pins;
pub mod pkg_check;
pub mod pointer;
pub mod roster;
pub mod seal_guard;
pub mod settings;
pub mod tag;

mod durable;
mod hash;
mod http;
mod privatedir;
mod sentinel;
mod source;
mod sys;

pub use hash::sha256_file;
#[cfg(all(unix, not(target_os = "linux")))]
pub use hash::{shasum_command, shasum_digest};
pub use http::{
    HeadAnswer, HttpError, RELEASE_ASSET_DOWNLOAD_BOUND, VendorResponse, download_bytes,
    download_error_is_local_write, download_error_is_not_found, download_error_is_rate_limit,
    download_to, download_to_resumable, download_to_resumable_https_only, head_no_redirect,
    head_no_redirect_quick, vendor_content_length, vendor_download_to, vendor_get, vendor_get_hint,
};
pub use manifest::{Manifest, SUPPORTED_SCHEMA};
pub use privatedir::ensure_private_dir;
pub use roster::{Attribution, Machine, Roster, RosterReject, VerifiedRoster, verify_roster};
pub use sentinel::{LaunchName, Sentinel};
pub use source::{
    ATPKG_INDEX_OWNER, DEFAULT_OWNER, DEFAULT_REPO, REPOINT_IS_A_DEV_SEAM, Source, is_valid_slug,
    pick_slug,
};
pub use sys::{FileLock, same_volume};

/// Log a non-fatal updater warning (`aterm-update:` prefix); a no-op with no logger installed.
pub(crate) fn warn(msg: &str) {
    aterm_log::warn!("aterm-update: {msg}");
}

/// Log an unasked atpkg notice in the host's log: the sink `atpkg::notice` uses once a host
/// routes notices there, since nothing prints into a shell the user did not ask to update.
/// Discarded where no logger is installed.
pub fn log_pkg_note(msg: &str) {
    aterm_log::warn!("atpkg: {msg}");
}

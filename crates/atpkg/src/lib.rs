// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg` — the aterm toolchain package manager.

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

macro_rules! println {
    () => {
        $crate::meter::println(::std::format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::meter::println(::std::format_args!($($arg)*))
    };
}
macro_rules! eprintln {
    () => {
        $crate::meter::eprintln(::std::format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::meter::eprintln(::std::format_args!($($arg)*))
    };
}

pub mod abrew;
pub mod activate;
#[cfg(target_os = "macos")]
pub mod activation_notice;
/// Whether this machine consents to the unattended adoption of the ALab toolset.
pub mod adoption;
pub mod alab;
pub mod apply;
pub mod build_cache;
/// Programs built into aterm (`amacs`).
pub mod builtin;
pub mod cache;
/// The shell that typed this command.
pub mod caller_shell;
/// The `atpkg` CLI (all verbs), callable in-process by the `aterm` binary.
pub mod cli;
/// Copy-on-write clones of a build's files.
pub(crate) mod clone;
/// Per-build exec roots (`<prefix>/compat/trust/<build>`) and `bin/` shim routing for Trust bundles.
pub mod compat;
pub mod config;
/// `aterm pkg doctor`'s checks as converge items.
pub mod converge;
/// Stale copies of aterm beside the installed release (macOS).
pub mod copies;
pub mod cost;
/// The Developer Tools list (macOS).
pub mod devtools;
/// Blocking directory and file watches (kqueue on macOS, inotify on Linux).
pub mod dirwatch;
pub mod discovery;
pub mod dispatch;
pub mod doctor;
pub mod extract;
pub mod flow;
pub mod freespace;
pub mod gate;
pub mod gc;
pub mod harness;
pub mod hooks;
/// Which program and build a running image is, from the executable the kernel mapped.
pub mod image;
/// Cheap, cross-process discovery of the next published toolchain index.
pub mod index_probe;
pub mod install;
/// The retired landing wait.
pub mod landing;
/// `atpkg lane`.
pub mod lane;
/// Laying executables (shims, stubs, tombstones).
pub mod lay;
/// Toolchain leases.
pub mod lease;
/// Which compiler built a toolchain, decided by content.
pub mod lineage;
pub mod linkmode;
pub mod lock;
pub mod machine;
/// The Mach-O files of a staged tree, found by magic, for the Developer ID stage gate.
pub mod macho;
pub mod managed_launch;
pub mod manifest;
mod metadata_io;
/// The one-line progress meter under a person's typed verb, and the printing every line
/// takes around it.
mod meter;
pub mod net;
/// Where atpkg's unasked notices go.
pub mod notice;
/// OpenPGP detached-signature verification.
pub(crate) mod openpgp;
pub mod ops;
/// `packages.log` beside `aterm.log`.
pub mod packages_log;
/// A waiting pending stub wakes when the package pass replaces a `bin/` shim.
mod pending_wake;
pub mod pin;
pub mod platform;
pub mod prereq;
pub mod progress;
/// Folders guarded by macOS TCC consent dialogs so unattended lanes never open them.
pub mod protected;
/// Flip when quiet.
pub mod quiet;
pub mod reroute;
/// The rustup toolchain seam.
pub mod seam;
pub mod select;
/// What `tools/install.sh` lays on an account beyond the binary, and its undo, as verbs of the installed binary.
#[cfg(unix)]
pub mod selfsetup;
/// The self-update intercept.
pub mod selfupdate;
/// Recorded answers for the index, manifests and `status.toml` (the serde corpus).
#[cfg(test)]
mod serde_corpus_tests;
pub mod shim_env;
pub mod sig;
/// Canonical per-program state spellings shared by `status.toml`, pass logs, `doctor`, and `which`.
pub mod state;
pub mod status;
pub mod store;
pub mod stub;
pub mod sysroot;
/// The read-only USTAR/PAX/GNU bundle parser the extractor drives.
pub mod tarread;
/// The fake programs `reroute`'s and `upstream_identity`'s unit tests exec.
#[cfg(all(test, unix))]
mod test_programs;
/// Unit-test scratch directories, removed when the test ends however it ends.
#[cfg(test)]
pub(crate) mod testdir;
/// THE toolchain resolution order (`aterm help rust`, `verify`, the release cutter), re-exported from `aterm-toolchain`, where its tests run.
pub mod toolchain;
pub mod tree;
pub mod upstream_identity;
/// Per-protocol row admission (`https`: host allow-list + payload shape), the `system = "<bin>"` satisfaction probe and the PATH-shadow probe.
pub mod vendor;
/// Vendor-direct agents.
pub mod vendor_direct;
pub mod verify;
/// Detached headless warmup of a newly landed vendor agent build.
pub mod warm;
/// Yolo by default on every managed agent name, and the `--yolo` alias for it.
pub mod yolo;

pub use activate::{Aliases, activate_build, atomic_symlink, install_shims};
pub use adoption::{Adoption, adoption_consented, session_bin_dir};
pub use apply::{Group, TxnOutcome, plan_groups};
pub use cache::IndexCache;
pub use config::{LinkTarget, PackagesConfig, classify_link, repo_overrides};
pub use discovery::resolve_account;
pub use extract::{
    EntryKind, ExtractError, ExtractReject, TooLargeReason, extract_tar_zst, vet_entry,
};
pub use flow::{
    AppliedMember, ChannelApplyReport, DepOutcome, DepResult, Fetcher, FlowError, InstallReport,
    InstallRequest, VendorFetchError, VendorGet, apply_channel, install, resolve_verified_index,
};
pub use gate::ApplyDecision;
pub use install::{StageError, verify_and_stage};
pub use linkmode::{
    LinkError, LinkOutcome, is_linked, link, linked_checkout, linked_checkout_checked,
    linked_programs, linked_programs_checked, linked_tool_names, refresh, unlink,
};
pub use manifest::{
    Artifact, Channel, Cost, Index, PkgManifest, Program, SUPPORTED_SCHEMA, TARGETS, parse_pkg,
};
pub use net::{DirFetcher, GithubFetcher};
pub use ops::{active_builds, installed_exposes, list_installed, uninstall, which};
pub use select::{Candidate, Selected, Selection, select_index};
pub use sig::{
    Anchor, BuildFloor, Floor, Reject, TrustedIndex, TrustedRoster, VerifiedBytes, admit_roster,
    check_freshness,
};
pub use status::{ProgramStatus, Status};
pub use store::{Layout, default_prefix, shim_allowed, vet_prefix};
pub use tree::{sha256_bytes, sha256_file, tree_root};

/// The base64 Ed25519 public key(s) of the paper master.
pub const PKG_TRUST_ANCHORS: &[&str] = aterm_update_core::pins::PAPER_MASTER_PUBKEYS;

/// The head of [`PKG_TRUST_ANCHORS`], for the surfaces that show one anchor to a human (the GUI's diagnostics row).
pub const PINNED_PKG_ROOTKEY: &str = if PKG_TRUST_ANCHORS.is_empty() {
    ""
} else {
    PKG_TRUST_ANCHORS[0]
};

/// Whether the manager can act.
#[inline]
#[must_use]
pub const fn enabled() -> bool {
    !PKG_TRUST_ANCHORS.is_empty()
}

/// Effective CLI posture.
#[inline]
#[must_use]
pub const fn manager_enabled() -> bool {
    manager_enabled_with(PKG_TRUST_ANCHORS)
}

/// Pure core of [`manager_enabled`].
#[inline]
#[must_use]
pub const fn manager_enabled_with(pinned: &[&str]) -> bool {
    !pinned.is_empty()
}

/// A short fingerprint of the pinned master keyset, so `atpkg doctor` can show which trust root is live.
#[must_use]
pub fn root_key_fingerprint() -> String {
    /// Separator: a control byte, so it can never appear inside a base64 key.
    const SEP: u8 = 0x1F;
    let mut h = aterm_hash::Fnv1a64::new();
    for key in PKG_TRUST_ANCHORS {
        h.write(key.as_bytes());
        h.write_u8(SEP);
    }
    format!("{:016x}", h.finish())
}

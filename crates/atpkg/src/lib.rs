// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Cross-platform (Unix + Windows). OS-specific primitives — symlink/shim
// activation, private-dir hardening, advisory file locks, free-space queries,
// PATH shimming, process exec, and shell hooks — are abstracted behind the
// `platform` module with per-OS backends; the rest of the crate is portable.

//! `atpkg` — the aterm toolchain package manager.
//!
//! The signature anchor ([`sig`]),
//! discovery ([`manifest`] schema parsers + [`discovery`] account resolution), the
//! per-program store, tar-slip-safe staging ([`extract`], [`install`]), the
//! all-or-nothing coherence-group apply ([`apply`]), activation ([`activate`]),
//! locking/pinning ([`lock`], [`pin`]), retention ([`gc`]) and the dev-link lane
//! ([`linkmode`]) compose the package lifecycle. The manager runs in-process as
//! `aterm pkg` or through the `atpkg` argv0 alias. The GUI drives `seed` once
//! and `update` on an interval.
//!
//! # ONE ROOT
//!
//! atpkg has no trust root of its own. It shares aterm's: the **paper master**
//! ([`PKG_TRUST_ANCHORS`], i.e. `aterm_update_core::pins::PAPER_MASTER_PUBKEYS`) signs a
//! roster of machine keys, and a machine on that roster signs the toolchain index exactly
//! as it signs a release appcast.
//!
//! ```text
//!   PAPER MASTER  --signs-->  aterm-machines.toml  (the roster: who may sign, who no longer may)
//!                                     |
//!                    a rostered machine signs:  index.toml
//!                    a rostered machine signs:  pkg-<program>-<build>.toml
//!                                     |
//!                            index --names--> program repos, channels, pins
//! ```
//!
//! The roster controls both authorization and revocation; revoking a machine does
//! not require republishing the index.
//!
//! 1. The roster is verified under the pinned master over its **exact raw bytes**, then
//!    admitted against a durable `roster_seq` ratchet and its own freshness window.
//! 2. `index.toml` is verified under the machines that roster still authorizes — revoked
//!    and expired ones having left the candidate set BEFORE any crypto — and then bound to
//!    the machine that actually signed it (`machine_id` / `roster_seq` live inside the
//!    signed bytes).
//! 3. Each `pkg-*.toml` is verified under that same roster generation
//!    ([`sig::TrustedIndex::verify_pkg`]).
//! 4. Verification happens **before any parse**, enforced *by construction*: the only way
//!    to obtain a [`sig::VerifiedBytes`] (which the parser consumes) is through the
//!    authorization functions. There is no public constructor, so handing unverified bytes
//!    to the parser does not type-check.
//!
//! **Fail-closed-on-empty.** With no master pinned the manager is fully inert:
//! [`enabled`] is false, [`select_index`] selects nothing (and observes nothing) before
//! touching a candidate, and
//! every roster admission returns [`sig::Reject::Disabled`] before any crypto runs.
//! The committed [`PKG_TRUST_ANCHORS`] enables installation and activation of
//! signed toolchains. See `docs/RELEASE-KEYS.md` for the key policy.

// EVERY LINE atpkg PRINTS STEPS AROUND THE TERMINAL METER ([`meter`]). These shadow the
// standard `println!` and `eprintln!` for the whole crate — declared before every module,
// so every module sees them. With no meter running (a pipe, the window's children, a
// host, a test) each is the std macro with the same bytes and the same panic on a closed
// pipe; under a person's typed verb the meter's line is cleared first and redrawn below.
// atpkg prints no partial line: its one `print!`, the extras' consent prompt, went with
// the extras (2026-09-24).
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]
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

pub mod activate;
#[cfg(target_os = "macos")]
pub mod activation_notice;
/// Whether this machine consents to the unattended adoption of the ALab toolset: the one
/// predicate the first-launch seed and the window both ask ([`adoption_consented`]).
pub mod adoption;
pub mod alab;
pub mod apply;
pub mod build_cache;
pub mod cache;
/// The shell that typed this command (2026-09-16): the parent process's executable when
/// it is a shell, else `$SHELL` — what `doctor`/`which` key their in-place remedy on.
pub mod caller_shell;
/// The `atpkg` CLI (all verbs), callable in-process by the ONE `aterm` binary.
pub mod cli;
/// Copy-on-write clones of a build's files — how the rustup view and the exec roots are
/// laid, instead of hard links to the store's own inodes.
pub(crate) mod clone;
/// Per-build exec roots (`<prefix>/compat/trust/<build>`) for the Trust bundles that ship
/// `bin/rustc` as a separate copy their own tippy refuses, and the stat-only route the
/// `bin/` shims take through them.
pub mod compat;
pub mod config;
/// Stale copies of aterm beside the installed release (macOS): what `aterm doctor` moves
/// to the Trash, and the one move the Settings panel's *Move to Trash* makes too.
pub mod copies;
pub mod cost;
/// The Developer Tools list (macOS): whether macOS holds each new program aterm starts
/// for its first-run check, and the `devtools:` row both doctors print.
pub mod devtools;
pub mod discovery;
pub mod dispatch;
pub mod doctor;
pub mod extract;
pub mod flow;
pub mod freespace;
pub mod gate;
pub mod gc;
pub mod hooks;
/// Cheap, cross-process discovery of the next published toolchain index. A hit only
/// wakes the ordinary signed update pass; it never authorizes an index by itself.
pub mod index_probe;
pub mod install;
/// The retired landing wait (2026-09-16 to 2026-09-22): the `__landing` verb older twins
/// still call, now an immediate silent `exec`, and the sweep of the markers they test for.
pub mod landing;
/// `atpkg lane`: the Rust-lane reader over one shell command line — the Trust spelling of
/// a stock `cargo`/`rustc`/`rustfmt`/clippy/rustdoc at command position, and exit 2.
pub mod lane;
/// Laying executables (shims, stubs, tombstones): one temp + `rename(2)` writer.
pub mod lay;
/// Toolchain leases: a run holds the store build it resolved for as long as it runs, so gc
/// never reclaims it and the unattended trust flip waits for it.
pub mod lease;
/// Which compiler built a toolchain, decided by content: compiler admission
/// ([`lineage::check_compiler_with`]) with the scan of one object it runs over the
/// compiler's own bytes, and the SEED-MINT and LINEAGE receipts it checks. The producer
/// (`atpkg-publish`) walks a staged payload with the same scan and writes the LINEAGE.
pub mod lineage;
pub mod linkmode;
pub mod lock;
pub mod machine;
/// The Mach-O files of a staged tree, found by magic, for the Developer ID stage gate; the
/// header test is public, so the producer's payload scan reads a head the same way.
pub mod macho;
pub mod manifest;
mod metadata_io;
/// The one-line progress meter under a person's typed verb, and the printing every line
/// takes around it.
mod meter;
pub mod net;
/// Where atpkg's unasked notices go: stderr in its own CLI, the host's log in the terminal
/// session and the window, which call it beside a live shell.
pub mod notice;
/// OpenPGP detached-signature verification (vendor-direct design §1.2): a v4 RSA
/// signature over a vendor's digest document, under a key compiled into this binary.
pub(crate) mod openpgp;
pub mod ops;
/// `packages.log` beside `aterm.log`: one whole line per pass and per program transition,
/// written by every lane, rotated under the store lock — what Settings ▸ Packages' Activity
/// reads (Phase 4).
pub mod packages_log;
/// A waiting pending stub wakes when the package pass replaces a `bin/` shim.
mod pending_wake;
pub mod pin;
pub mod platform;
pub mod prereq;
pub mod progress;
/// The folders macOS guards with a consent dialog, named once, so no UNATTENDED lane
/// (the update pass, the seed, the doctor's walk) ever opens one in aterm's name.
pub mod protected;
/// Flip when quiet: an unattended update stages a new Trust toolchain at once and flips it
/// only when no run leases or runs from the one it replaces — four hours at the most.
pub mod quiet;
pub mod reroute;
/// The rustup toolchain seam owner (Lockstep S1): `<rustup_home>/toolchains/trust` ->
/// `<prefix>/store/trust/current`, laid, adopted, re-asserted and recorded by atpkg.
pub mod seam;
pub mod select;
/// What `tools/install.sh` lays on an account beyond the binary, and its undo, as verbs of
/// the installed binary: `aterm pkg shell-setup` and `aterm pkg self-uninstall`.
#[cfg(unix)]
pub mod selfsetup;
/// The self-update intercept (2026-09-19): `claude update|upgrade|install` and `codex
/// update` typed on the MANAGED name are answered by the standard `aterm pkg update` —
/// the roster, the classification and every line, pure; the verb is `cli::cmd_selfupdate`.
pub mod selfupdate;
/// Recorded answers for the index, manifests and `status.toml` (the serde corpus).
#[cfg(test)]
mod serde_corpus_tests;
pub mod shim_env;
pub mod sig;
/// The CANONICAL per-program state spellings (`managed <build> — pinned by index <N>`,
/// `system: <path> — not managed by aterm`, …) shared by status.toml, the pass log,
/// `doctor` and `which`.
pub mod state;
pub mod status;
pub mod store;
pub mod stub;
pub mod sysroot;
/// The read-only USTAR/PAX/GNU bundle parser the extractor drives (retired the
/// `tar` crate, and with it `xattr` and `filetime` — aterm never WRITES tar).
pub mod tarread;
/// The fake programs `reroute`'s and `upstream_identity`'s unit tests exec.
#[cfg(all(test, unix))]
mod test_programs;
/// Unit-test scratch directories, removed when the test ends however it ends.
#[cfg(test)]
pub(crate) mod testdir;
/// THE toolchain resolution order (`aterm help rust`, `verify`, the release
/// cutter), re-exported from `aterm-toolchain`, where its tests run.
pub mod toolchain;
pub mod tree;
pub mod upstream_identity;
/// Per-protocol row admission (`https`: host allow-list + payload shape), the
/// `system = "<bin>"` satisfaction probe and the PATH-shadow probe — both
/// cross-platform (`PATHEXT` on Windows).
pub mod vendor;
/// Vendor-direct agents (design 2026-09-22, Phase 1): the compiled vendor table, the
/// version ↔ build id map, the update decision and the lane's durable records.
pub mod vendor_direct;
pub mod verify;
/// The one silent, detached, headless run of an agent build a vendor lane just landed, so
/// the vendor CLI's account-bound caches are fresh before the user's first launch.
pub mod warm;

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
// `parse_index` is deliberately NOT re-exported (and is `pub(crate)`): outside this
// crate, the only way to a parsed `Index` is `TrustedRoster::authorize_index`, which runs
// the machine-id bind the raw parse would let a caller skip. See its doc in `manifest`.
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

/// The base64 Ed25519 public key(s) of the **paper master** this binary trusts — atpkg's
/// one and only trust root, shared verbatim with the app update channel.
///
/// A committed constant ([`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`]), not a build
/// env var: what a binary trusts is a property of the source, identical on every machine.
/// A LIST because a client that accepts exactly one master cannot be told about a
/// replacement by a document it would refuse to verify.
///
/// EMPTY means unpinned means INERT, and inert grants nothing: no roster verifies, so no
/// machine is authorized, so no index verifies, so nothing installs. It never means
/// "accept anything". This tree is ARMED (2026-08-15): the paper master is pinned and
/// the manager is live.
///
/// The master's SECRET half is 52 base32 characters on paper and exists on no
/// computer; it is typed in only to provision a machine or to revoke one.
pub const PKG_TRUST_ANCHORS: &[&str] = aterm_update_core::pins::PAPER_MASTER_PUBKEYS;

/// The single-string spelling of [`PKG_TRUST_ANCHORS`]' head, for the surfaces that show
/// ONE anchor to a human (the GUI's diagnostics row, [`root_key_fingerprint`]).
///
/// It is derived, never a second anchor: `""` exactly when the keyset is empty, so
/// `!PINNED_PKG_ROOTKEY.is_empty()` and `!PKG_TRUST_ANCHORS.is_empty()` answer the same
/// question and a display surface cannot report "a root is available" while the verifier
/// considers itself unarmed. Authority is always the LIST — a rotation in flight puts a
/// second master beside the head, and only the list sees it.
///
/// It no longer names `pins::PKG_ROOT_PUBKEY`. That constant is retired: nothing in this
/// crate reads it, and arming it would arm nothing.
pub const PINNED_PKG_ROOTKEY: &str = if PKG_TRUST_ANCHORS.is_empty() {
    ""
} else {
    PKG_TRUST_ANCHORS[0]
};

/// Whether the manager can act: a paper master must be pinned. Fail closed — an empty
/// keyset is never active. Unlike `aterm-update::enabled` this is **not** macOS-gated:
/// the package manager is cross-platform.
///
/// It is a property of the BUILD, not of the machine. The machine's own switch is
/// `[packages] enabled` in aterm.toml — "Automatic updates", which gates the automatic
/// lanes (the window's loop, a terminal session's pass, the head watch) and never a verb
/// a person typed. `ATPKG_DISABLE`, the environment kill switch that used to sit here,
/// is gone (2026-09-23, R2: "NOT ENV VARS those are for development"): it reached only
/// what one shell launched, never the window's passes, so it was a second, partial
/// spelling of that switch.
#[must_use]
pub fn enabled() -> bool {
    !PKG_TRUST_ANCHORS.is_empty()
}

/// Effective CLI posture: the compiled root anchor. This is the shared admission
/// predicate for the CLI and aterm's native Packages surface.
///
/// `ATPKG_ROOTKEY_OVERRIDE` is GONE. It supplied "the same verification anchor the
/// verbs consume", so an environment variable could ENABLE an otherwise-unpinned
/// build — i.e. ambient state decided what the package manager trusted. The anchor
/// now lives in reviewed source ([`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`]) and
/// nothing outside a commit can change it. An alternate package owner commits their
/// own paper master, which is the same deliberate act, visible in a diff. The
/// `ATPKG_DISABLE` kill switch went the same way (see [`enabled`]).
#[must_use]
pub fn manager_enabled() -> bool {
    manager_enabled_with(PKG_TRUST_ANCHORS)
}

/// Pure core of [`manager_enabled`]: armed iff the keyset is non-empty. The keyset is
/// the input rather than a single key so this cannot be called with a head that is
/// present while the list behind it is not.
#[must_use]
pub fn manager_enabled_with(pinned: &[&str]) -> bool {
    !pinned.is_empty()
}

/// A short, dependency-free fingerprint of the pinned MASTER keyset, so `atpkg doctor`
/// can show **which** trust root is live (§8). FNV-1a/64 over every pinned base64 string
/// in order, separated by a byte base64 cannot contain (`0x1F`) so two different keysets
/// cannot collide by concatenation — purely operator-facing, never a security primitive
/// (the real anchor is the keys themselves, not this digest). Returns the all-zero seed
/// digest when nothing is pinned.
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

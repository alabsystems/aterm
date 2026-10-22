// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Cross-platform (Unix + Windows): the OS-specific primitives (shim activation, private-dir
// hardening, advisory locks, free-space queries, PATH shimming, exec, shell hooks) sit
// behind the `platform` module's per-OS backends.

//! `atpkg` — the aterm toolchain package manager.
//!
//! It holds the signature anchor ([`sig`]), discovery ([`manifest`] parsers, [`discovery`]
//! account resolution), the per-program store, tar-slip-safe staging ([`extract`],
//! [`install`]), the all-or-nothing coherence-group apply ([`apply`]), activation
//! ([`activate`]), locking and pinning ([`lock`], [`pin`]), retention ([`gc`]) and the
//! dev-link lane ([`linkmode`]). It runs in-process as `aterm pkg` or through the `atpkg`
//! argv0 alias; the GUI drives `seed` once and `update` on an interval.
//!
//! # One root
//!
//! atpkg shares aterm's trust root: the paper master ([`PKG_TRUST_ANCHORS`]) signs a roster
//! of machine keys, and a rostered machine signs the toolchain index as it signs a release
//! appcast.
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
//! The roster controls both authorization and revocation; revoking a machine does not
//! require republishing the index.
//!
//! 1. The roster is verified under the pinned master over its exact raw bytes, then admitted
//!    against a durable `roster_seq` ratchet and its own freshness window.
//! 2. `index.toml` is verified under the machines that roster still authorizes (revoked and
//!    expired ones leave the candidate set before any crypto), then bound to the machine
//!    that signed it (`machine_id` / `roster_seq` live inside the signed bytes).
//! 3. Each `pkg-*.toml` is verified under that same roster generation
//!    ([`sig::TrustedIndex::verify_pkg`]).
//! 4. Verification happens before any parse, by construction: a [`sig::VerifiedBytes`]
//!    (which the parser consumes) comes only from the authorization functions, so
//!    unverified bytes cannot reach the parser.
//!
//! With no master pinned the manager is inert: [`enabled`] is false, [`select_index`]
//! selects and observes nothing, and every roster admission returns
//! [`sig::Reject::Disabled`] before any crypto. The committed [`PKG_TRUST_ANCHORS`] pins
//! one master, so a build from this tree installs, verifies and activates signed
//! toolchains; `docs/RELEASE-KEYS.md` has the key policy.

// Every line atpkg prints steps around the terminal meter ([`meter`]): these shadow
// `println!` and `eprintln!` for the whole crate, so they are declared before every module.
// With no meter running (a pipe, the window's children, a host, a test) each is the std
// macro, with the same bytes and the same panic on a closed pipe; under a person's typed
// verb the meter's line is cleared first and redrawn below.
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

/// abrew — aterm's Homebrew, an atpkg extension (`docs/DESIGN-abrew-2026-10-19.md`):
/// relocation-free bottles and simple casks natively (everything else delegates to real brew before effects), the bundled Homebrew, and the `brew` reroute stub.
pub mod abrew;
pub mod activate;
#[cfg(target_os = "macos")]
pub mod activation_notice;
/// Whether this machine consents to the unattended adoption of the ALab toolset: the one
/// predicate the first-launch seed and the window both ask ([`adoption_consented`]).
pub mod adoption;
pub mod alab;
pub mod apply;
pub mod build_cache;
/// Programs built into aterm (`amacs`): served by the one binary under an argv0 alias,
/// never a store program — what `which`, `list`, `install`, `update` and `uninstall` say
/// of one, kept apart from the stub roster so the front door never runs one from the store.
pub mod builtin;
pub mod cache;
/// The shell that typed this command: the parent process's executable when it is a shell,
/// else `$SHELL` — what `doctor`/`which` key their in-place remedy on.
pub mod caller_shell;
/// The `atpkg` CLI (all verbs), callable in-process by the `aterm` binary.
pub mod cli;
/// Copy-on-write clones of a build's files — how the rustup view and the exec roots are
/// laid, instead of hard links to the store's own inodes.
pub(crate) mod clone;
/// Per-build exec roots (`<prefix>/compat/trust/<build>`) for the Trust bundles that ship
/// `bin/rustc` as a separate copy their own tippy refuses, and the stat-only route the
/// `bin/` shims take through them.
pub mod compat;
pub mod config;
/// `aterm pkg doctor`'s checks as converge items (docs/DESIGN-init-system-2026-10-20.md
/// §4.2): the rows its text and `--json` are both rendered from.
pub mod converge;
/// Stale copies of aterm beside the installed release (macOS): what `aterm doctor` moves
/// to the Trash, and the one move the Settings panel's *Move to Trash* makes too.
pub mod copies;
pub mod cost;
/// The Developer Tools list (macOS): whether macOS holds each new program aterm starts
/// for its first-run check, and the `devtools:` row both doctors print.
pub mod devtools;
/// Blocking directory and file watches (kqueue on macOS, inotify on Linux) for a thread that
/// sleeps until what it reads may have moved (`docs/DESIGN-card-engine-2026-10-19.md` §3.10).
pub mod dirwatch;
pub mod discovery;
pub mod dispatch;
pub mod doctor;
pub mod extract;
pub mod flow;
pub mod freespace;
pub mod gate;
pub mod gc;
pub mod hooks;
/// Which program and build a running image is, from the executable the kernel mapped
/// (`docs/DESIGN-card-engine-2026-10-19.md` §3.2).
pub mod image;
/// Cheap, cross-process discovery of the next published toolchain index. A hit only
/// wakes the ordinary signed update pass; it never authorizes an index by itself.
pub mod index_probe;
pub mod install;
/// The retired landing wait: the `__landing` verb older twins still call, now an immediate
/// silent `exec`, and the sweep of the markers they test for.
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
/// OpenPGP detached-signature verification: a v4 RSA signature over a vendor's digest
/// document, under a key compiled into this binary.
pub(crate) mod openpgp;
pub mod ops;
/// `packages.log` beside `aterm.log`: one whole line per pass and per program transition,
/// written by every lane, rotated under the store lock; Settings ▸ Packages' Activity
/// reads it.
pub mod packages_log;
/// A waiting pending stub wakes when the package pass replaces a `bin/` shim.
mod pending_wake;
pub mod pin;
pub mod platform;
pub mod prereq;
pub mod progress;
/// The folders macOS guards with a consent dialog, named once, so no unattended lane (the
/// update pass, the seed, the doctor's walk) ever opens one in aterm's name.
pub mod protected;
/// Flip when quiet: an unattended update stages a new Trust toolchain at once and flips it
/// only when no run leases or runs from the one it replaces — four hours at the most.
pub mod quiet;
pub mod reroute;
/// The rustup toolchain seam: `<rustup_home>/toolchains/trust` ->
/// `<prefix>/store/trust/current`, laid, adopted, re-asserted and recorded by atpkg.
pub mod seam;
pub mod select;
/// What `tools/install.sh` lays on an account beyond the binary, and its undo, as verbs of
/// the installed binary: `aterm pkg shell-setup` and `aterm pkg self-uninstall`.
#[cfg(unix)]
pub mod selfsetup;
/// The self-update intercept: `claude update|upgrade|install` and `codex update` typed on
/// the managed name are answered by the standard `aterm pkg update`. Pure; the verb is
/// `cli::cmd_selfupdate`.
pub mod selfupdate;
/// Recorded answers for the index, manifests and `status.toml` (the serde corpus).
#[cfg(test)]
mod serde_corpus_tests;
pub mod shim_env;
pub mod sig;
/// The canonical per-program state spellings (`managed <build> — pinned by index <N>`,
/// `system: <path> — not managed by aterm`, …) shared by status.toml, the pass log,
/// `doctor` and `which`.
pub mod state;
pub mod status;
pub mod store;
pub mod stub;
pub mod sysroot;
/// The read-only USTAR/PAX/GNU bundle parser the extractor drives; aterm never writes tar.
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
/// Vendor-direct agents: the compiled vendor table, the version ↔ build id map, the update
/// decision and the lane's durable records.
pub mod vendor_direct;
pub mod verify;
/// The one silent, detached, headless run of an agent build a vendor lane just landed, so
/// the vendor CLI's account-bound caches are fresh before the user's first launch.
pub mod warm;
/// Yolo by default on every managed agent name, and the `--yolo` alias for it:
/// the expansion to the vendor's own approvals-and-sandbox-off flags, injected
/// unless the launch typed its own posture, and the one stderr line naming the
/// command as run; the twin's block is `platform::sh_yolo_prelude`, the `run`
/// lane's arm is in `cli::cmd_run`.
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
// `parse_index` is `pub(crate)` and not re-exported: outside this crate the only way to a
// parsed `Index` is `TrustedRoster::authorize_index`, which runs the machine-id bind.
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

/// The base64 Ed25519 public key(s) of the paper master: atpkg's only trust root, shared
/// with the app update channel.
///
/// A committed constant, not a build env var, so what a binary trusts is a property of the
/// source. A list because a client that accepts exactly one master cannot be told about a
/// replacement by a document it would refuse to verify.
///
/// Empty means inert: no roster verifies, so no machine is authorized, no index verifies
/// and nothing installs. The master's secret half exists only on paper; it is typed in only
/// to provision or revoke a machine.
pub const PKG_TRUST_ANCHORS: &[&str] = aterm_update_core::pins::PAPER_MASTER_PUBKEYS;

/// The head of [`PKG_TRUST_ANCHORS`], for the surfaces that show one anchor to a human (the
/// GUI's diagnostics row).
///
/// Derived, never a second anchor: `""` exactly when the keyset is empty, so a display
/// cannot report a root the verifier lacks. Authority is the list: a rotation in flight puts
/// a second master beside the head.
pub const PINNED_PKG_ROOTKEY: &str = if PKG_TRUST_ANCHORS.is_empty() {
    ""
} else {
    PKG_TRUST_ANCHORS[0]
};

/// Whether the manager can act: a paper master must be pinned. Unlike
/// `aterm-update::enabled` this is not macOS-gated.
///
/// A property of the build. The machine's switch is `[packages] enabled` in aterm.toml
/// ("Automatic updates"), which gates the automatic lanes (the window's loop, a terminal
/// session's pass, the head watch), never a verb a person typed.
#[inline]
#[must_use]
pub const fn enabled() -> bool {
    !PKG_TRUST_ANCHORS.is_empty()
}

/// Effective CLI posture: the shared admission predicate for the CLI and aterm's native
/// Packages surface. The anchor lives in reviewed source; no environment variable can arm
/// an unpinned build.
#[inline]
#[must_use]
pub const fn manager_enabled() -> bool {
    manager_enabled_with(PKG_TRUST_ANCHORS)
}

/// Pure core of [`manager_enabled`]: armed iff the keyset is non-empty. It takes the list,
/// not a head key, so a present head cannot stand in for an empty list.
#[inline]
#[must_use]
pub const fn manager_enabled_with(pinned: &[&str]) -> bool {
    !pinned.is_empty()
}

/// A short fingerprint of the pinned master keyset, so `atpkg doctor` can show which trust
/// root is live: FNV-1a/64 over every pinned key in order, each followed by `0x1F` so two
/// keysets cannot collide by concatenation. Operator-facing only, never a security
/// primitive. With nothing pinned it is the FNV offset basis.
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

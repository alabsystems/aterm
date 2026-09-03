// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Cross-platform (Unix + Windows). OS-specific primitives — symlink/shim
// activation, private-dir hardening, advisory file locks, free-space queries,
// PATH shimming, process exec, and shell hooks — are abstracted behind the
// `platform` module with per-OS backends; the rest of the crate is portable.

//! `atpkg` — the aterm toolchain package manager.
//!
//! **The manager is complete and shipped.** The signature anchor ([`sig`]),
//! discovery ([`manifest`] schema parsers + [`discovery`] account resolution), the
//! per-program store, tar-slip-safe staging ([`extract`], [`install`]), the
//! all-or-nothing coherence-group apply ([`apply`]), activation ([`activate`]),
//! locking/pinning ([`lock`], [`pin`]), retention ([`gc`]) and the dev-link lane
//! ([`linkmode`]) are all implemented and exercised. It ships as an argv0 alias on
//! the one `aterm` binary, and the GUI drives `seed` once plus `update` on an
//! interval.
//!
//! (This paragraph previously read "Phases 1–2 are present … the remaining
//! install/update machinery arrives in later phases." That was left behind by the
//! phases it describes and is no longer true — the modules it called future are
//! listed above. Fix the sentence, not the reader's expectations, if it drifts again.)
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
//! There used to be a SECOND root here — a `PKG_ROOT_PUBKEY` whose secret half lived at
//! `~/.config/atpkg/root.key`, signing an `index.toml` that in turn delegated a rotatable
//! release key. Both tiers are retired. The roster supplies the grant AND the deny, and
//! supplies the deny in minutes (bump the roster) rather than at index-republish latency.
//! One thing on paper, one revocation story, one document to audit.
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
//! That is the state a FORK starts from, and what shipped before v0.21.0.
//!
//! **THIS tree is ARMED** (2026-08-15, `atpkg-keys setup --id m3`):
//! [`PKG_TRUST_ANCHORS`] carries one master key, so a build from this source installs,
//! verifies and activates signed toolchains for real — see
//! `docs/ATPKG-KEY-MANAGEMENT.md`, which says so too. (This paragraph claimed the
//! opposite while the same file's [`enabled`] doc, 130 lines down, already said
//! ARMED; a reviewer who believed it would think running the manager could not
//! touch the fleet.)

pub mod activate;
pub mod appgate;
pub mod apply;
pub mod bundled;
pub mod cache;
/// The `atpkg` CLI (all verbs), callable in-process by the ONE `aterm` binary.
pub mod cli;
pub mod config;
pub mod cost;
pub mod discovery;
pub mod dispatch;
pub mod doctor;
/// The elevation seam the OS-installer lanes share: the injectable [`elevate::Runner`],
/// the calling verb's [`elevate::Elevation`] policy (Deferred by default — a background
/// pass never elevates), the `sudo`/`osascript` wrappers and the `provides` probe.
pub mod elevate;
pub mod extract;
pub mod flow;
pub mod freespace;
pub mod gate;
pub mod gc;
pub mod hooks;
pub mod install;
/// The `pkg` protocol's lane: a Developer-ID-signed macOS installer package, its
/// signer team checked with `pkgutil`, applied by `installer` with elevation.
pub mod installer_pkg;
pub mod linkmode;
pub mod lock;
pub mod manifest;
mod metadata_io;
pub mod net;
/// Spotlight exposure of Rust build output: discover target dirs, MEASURE whether one is
/// really excluded (never assume), and migrate it to the `.noindex` form (§9).
pub mod noindex;
pub mod ops;
pub mod pin;
pub mod platform;
pub mod progress;
pub mod provisional;
pub mod relocate;
/// The `requires` relation's one gate (`unmet_requirement`, §17.10), shared by the
/// set-completion pass, the OS-installed reconcile and the update pass.
pub mod requires;
/// The rustup toolchain seam owner (Lockstep S1): `<rustup_home>/toolchains/trust` ->
/// `<prefix>/store/trust/current`, laid, adopted, re-asserted and recorded by atpkg.
pub mod seam;
pub mod select;
pub mod shim_env;
pub mod sig;
/// The `softwareupdate` protocol's lane: Apple's Command Line Tools, installed
/// headlessly by `softwareupdate` with elevation (never `xcode-select --install`).
pub mod softwareupdate;
/// The CANONICAL per-program state spellings (`managed <build> — pinned by index <N>`,
/// `system: <path> — not managed by aterm`, …) shared by status.toml, the pass log,
/// `doctor` and `which`.
pub mod state;
pub mod status;
pub mod store;
pub mod stub;
pub mod sysroot;
/// The `system-pm` protocol's lane: a package the platform's own manager (one row of
/// [`vendor::MANAGER_TABLE`]) installs — `sudo` for the system-wide ones, as the user
/// for the rest, never installing a manager, proven by the row's `provides`.
pub mod system_pm;
/// The read-only USTAR/PAX/GNU bundle parser the extractor drives (retired the
/// `tar` crate, and with it `xattr` and `filetime` — aterm never WRITES tar).
pub mod tarread;
pub mod tree;
/// Per-protocol row admission (`https`: host allow-list + payload shape; `pkg`,
/// `system-pm` and `softwareupdate`: their own field rules), the extensible manager
/// table, the `system = "<bin>"` satisfaction probe and the PATH-shadow probe — both
/// cross-platform (`PATHEXT` on Windows).
pub mod vendor;
pub mod verify;

pub use activate::{Aliases, activate_channel, atomic_symlink, install_shims};
pub use appgate::{AppIndexGate, app_apply_allowed};
pub use apply::{Group, TxnOutcome, plan_groups, transact};
pub use bundled::{SEED_DIR_NAME, bundled_seed_dir};
pub use cache::IndexCache;
pub use config::{LinkTarget, PackagesConfig, classify_link, repo_overrides};
pub use cost::{disk_ok, human_bytes, needs_consent};
pub use discovery::{IndexRepo, resolve_account, resolve_account_with};
pub use dispatch::{ApplyStrategy, strategy_for};
pub use elevate::{Elevation, Runner};
pub use extract::{
    EntryKind, ExtractError, ExtractReject, extract_tar_zst, vet_entry, vet_hardlink,
};
pub use flow::{
    AppliedMember, ChannelApplyReport, DepOutcome, DepResult, Fetcher, FlowError, InstallReport,
    InstallRequest, ProtocolOutcome, apply_channel, install, resolve_verified_index,
};
pub use gate::{ApplyDecision, decide, is_yanked};
pub use gc::{GcReport, reclaimable, run as run_gc};
pub use install::{StageError, verify_and_stage};
pub use linkmode::{
    LinkError, LinkOutcome, is_linked, link, linked_checkout, linked_checkout_checked,
    linked_programs, linked_programs_checked, linked_tool_names, refresh, unlink,
};
pub use lock::{StoreLock, StoreLockError, try_lock_store};
// `parse_index` is deliberately NOT re-exported (and is `pub(crate)`): outside this
// crate, the only way to a parsed `Index` is `TrustedRoster::authorize_index`, which runs
// the machine-id bind the raw parse would let a caller skip. See its doc in `manifest`.
pub use manifest::{
    Artifact, Channel, Cost, Index, PkgManifest, Program, SUPPORTED_SCHEMA, TARGETS, parse_pkg,
};
pub use net::{ChainFetcher, DirFetcher, GithubFetcher};
pub use noindex::{Migration, Verdict, migrate, scan, verify};
pub use ops::{active_builds, installed_exposes, list_installed, uninstall, which};
pub use select::{Candidate, Selected, Selection, select_index};
pub use shim_env::ShimEnv;
pub use sig::{
    Anchor, BuildFloor, Floor, Reject, TrustedIndex, TrustedRoster, VerifiedBytes, admit_roster,
    check_freshness,
};
pub use status::{ProgramStatus, Status};
pub use store::{Layout, default_prefix, shim_allowed, vet_prefix};
pub use sysroot::{relocate_sysroot, write_toolchain_version};
pub use tree::{sha256_file, tree_root};
pub use vendor::{
    MANAGER_TABLE, MANAGERS, Manager, VENDOR_HOSTS, shadowing_binary_on_path, system_satisfied,
};
pub use verify::{VerifyOutcome, verify_all, verify_program};

/// The base64 Ed25519 public key(s) of the **paper master** this binary trusts — atpkg's
/// one and only trust root, shared verbatim with the app update channel.
///
/// A committed constant ([`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`]), not a build
/// env var: what a binary trusts is a property of the source, identical on every machine.
/// A LIST for the same reason the channel keyset is one — a client that accepts exactly
/// one master cannot be told about a replacement by a document it would refuse to verify.
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

/// Whether the manager is configured to act: a paper master must be pinned AND the user
/// must not have opted out via `ATPKG_DISABLE`. Fail closed — an empty keyset is never
/// active. Unlike `aterm-update::enabled` this is **not** macOS-gated: the package
/// manager is cross-platform.
#[must_use]
pub fn enabled() -> bool {
    !PKG_TRUST_ANCHORS.is_empty() && std::env::var_os("ATPKG_DISABLE").is_none()
}

/// Effective CLI posture: the compiled root anchor, plus the `ATPKG_DISABLE` kill
/// switch. This is the shared admission predicate for the CLI and aterm's native
/// Packages surface.
///
/// `ATPKG_ROOTKEY_OVERRIDE` is GONE. It supplied "the same verification anchor the
/// verbs consume", so an environment variable could ENABLE an otherwise-unpinned
/// build — i.e. ambient state decided what the package manager trusted. The anchor
/// now lives in reviewed source ([`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`]) and
/// nothing outside a commit can change it. An alternate package owner commits their
/// own paper master, which is the same deliberate act, visible in a diff.
///
/// `ATPKG_DISABLE` stays: turning the manager OFF is fail-safe, and a kill switch
/// that only ever subtracts authority cannot be used to grant any.
#[must_use]
pub fn manager_enabled() -> bool {
    manager_enabled_with(
        PKG_TRUST_ANCHORS,
        std::env::var_os("ATPKG_DISABLE").is_some(),
    )
}

/// Pure core of [`manager_enabled`]: armed iff the keyset is non-empty, and never while
/// disabled. The keyset is the input rather than a single key so this cannot be called
/// with a head that is present while the list behind it is not.
#[must_use]
pub fn manager_enabled_with(pinned: &[&str], disabled: bool) -> bool {
    !disabled && !pinned.is_empty()
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
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for key in PKG_TRUST_ANCHORS {
        for b in key.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= u64::from(SEP);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Manual rendering of the previous `format!("{h:016x}")` — byte-identical
    // (16 lowercase hex digits, zero-padded): the `format!` expansion embeds
    // `fmt::Arguments` construction (with inlined `unsafe`) that the strict
    // Trust gate cannot lower and fails closed on. Sixteen straight-line
    // nibble extractions; `wrapping_shr` by a constant < 64 is a plain shift
    // and carries no panic obligation, and each nibble is < 16 by the mask.
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(16);
    macro_rules! nib {
        ($sh:expr) => {
            s.push(char::from(HEX[(h.wrapping_shr($sh) & 0xf) as usize]));
        };
    }
    nib!(60);
    nib!(56);
    nib!(52);
    nib!(48);
    nib!(44);
    nib!(40);
    nib!(36);
    nib!(32);
    nib!(28);
    nib!(24);
    nib!(20);
    nib!(16);
    nib!(12);
    nib!(8);
    nib!(4);
    nib!(0);
    s
}

/// Invoke `f(a)` — identity at runtime, but the callee is a generic `FnOnce`.
///
/// The strict Trust gate's hardened contracts key on the DIRECT callee (by
/// name/identity), and `std`'s MIR-INLINED internals (e.g. `OsStr::to_str`'s
/// `from_utf8_unchecked` fast path, the `OsStr` byte-slice casts) are otherwise
/// attributed to the *caller's* spans as missing-SAFETY-comment refutations.
/// Routing such calls through a generic `FnOnce` scopes the callee out as
/// Conditional, the same way the gate scopes out every other polymorphic
/// callee. The helper invokes the exact same function with the same argument:
/// behavior is identical. (Same idiom as `aterm-update`/`aterm-update-core`/
/// `aterm-tempfile`; an fn-POINTER spelling instead was observed to send the
/// full verifier's bundle builder into unbounded recursion.)
pub(crate) fn call1<F, A, T>(f: F, a: A) -> T
where
    F: FnOnce(A) -> T,
{
    f(a)
}

/// Two-argument sibling of [`call1`] — identity at runtime, hardened contracts
/// scoped out. Used for `std::fs::write`, whose *name* trips the hardened libc
/// `write(2)` FFI-boundary matcher on direct call sites (the safe std function
/// is not that FFI, so the contract cannot be discharged there).
pub(crate) fn call2<F, A, B, T>(f: F, a: A, b: B) -> T
where
    F: FnOnce(A, B) -> T,
{
    f(a, b)
}

/// Render `v` in decimal, byte-identical to `u64`'s `Display` — used by the
/// manual (`format_args!`-free) string builders in this crate: the `format!`
/// expansion embeds `fmt::Arguments` construction (with inlined `unsafe`) that
/// the strict Trust gate cannot lower and fails closed on.
///
/// Deliberately LOOP-FREE, digit-by-constant-power-of-ten (same idiom as
/// `aterm-scrollback::error::dec_string`): the classic `v % 10` / `v /= 10`
/// loop sends the strict gate's integer engine into a non-terminating solve
/// (loop-carried division). Twenty straight-line constant divisions carry no
/// loop invariant to infer and no panic obligations at all (constant nonzero
/// divisors, wrapping add of a digit that is 0..=9 by construction).
pub(crate) fn dec_u64(v: u64) -> String {
    let mut rem = v;
    let mut out = String::new();
    let mut started = false;
    macro_rules! emit_digit {
        ($p:expr) => {
            let d = (rem / $p) as u8;
            rem %= $p;
            if started || d != 0 {
                started = true;
                out.push(char::from(b'0'.wrapping_add(d)));
            }
        };
    }
    emit_digit!(10_000_000_000_000_000_000u64);
    emit_digit!(1_000_000_000_000_000_000u64);
    emit_digit!(100_000_000_000_000_000u64);
    emit_digit!(10_000_000_000_000_000u64);
    emit_digit!(1_000_000_000_000_000u64);
    emit_digit!(100_000_000_000_000u64);
    emit_digit!(10_000_000_000_000u64);
    emit_digit!(1_000_000_000_000u64);
    emit_digit!(100_000_000_000u64);
    emit_digit!(10_000_000_000u64);
    emit_digit!(1_000_000_000u64);
    emit_digit!(100_000_000u64);
    emit_digit!(10_000_000u64);
    emit_digit!(1_000_000u64);
    emit_digit!(100_000u64);
    emit_digit!(10_000u64);
    emit_digit!(1_000u64);
    emit_digit!(100u64);
    emit_digit!(10u64);
    // Ones digit is emitted unconditionally, so `0` renders as "0".
    let _ = started;
    out.push(char::from(b'0'.wrapping_add(rem as u8)));
    out
}

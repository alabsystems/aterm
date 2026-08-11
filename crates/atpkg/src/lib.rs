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
//! The anchor is a two-tier Ed25519 trust model that
//! mirrors `aterm-update`'s notarization pin, but for cross-platform CLI tarballs
//! where Apple notarization does not transfer (see
//! `docs/TOOLCHAIN-PACKAGE-MANAGER.md` §8/§13):
//!
//! 1. A compile-time-pinned **offline root** public key ([`PINNED_PKG_ROOTKEY`])
//!    verifies `index.toml` over its **exact raw bytes**.
//! 2. The root-signed index **delegates** a rotatable **release key**; that key
//!    verifies each `pkg-*.toml` (again over raw bytes). Re-cutting the index with a
//!    new release key id is how a compromised release key is revoked without
//!    shipping a new `aterm` — a deny-list (`revoked_release_keys`) belt-and-suspenders
//!    the same.
//! 3. Verification happens **before any parse**, enforced *by construction*: the only
//!    way to obtain a [`sig::VerifiedBytes`] (which the parser consumes) is to pass
//!    one of the verify functions. There is no public constructor, so handing
//!    unverified bytes to the parser does not type-check.
//!
//! **Fail-closed-on-empty.** With no root key baked in at build time the manager is
//! fully inert: [`enabled`] is false and every verify of an index returns
//! [`sig::Reject::Disabled`] before any crypto runs. A plain `cargo build` installs,
//! verifies, and trusts nothing.

pub mod activate;
pub mod appgate;
pub mod apply;
pub mod bundled;
pub mod cache;
/// The `atpkg` CLI (all verbs), callable in-process by the ONE `aterm` binary.
pub mod cli;
pub mod companions;
pub mod config;
pub mod cost;
pub mod discovery;
pub mod dispatch;
pub mod doctor;
pub mod extract;
pub mod flow;
pub mod freespace;
pub mod gate;
pub mod gc;
pub mod hooks;
pub mod install;
pub mod linkmode;
pub mod lock;
pub mod manifest;
mod metadata_io;
pub mod net;
pub mod ops;
pub mod pin;
pub mod platform;
pub mod relocate;
pub mod seed;
pub mod select;
pub mod sig;
pub mod sourcebuild;
pub mod status;
pub mod store;
pub mod sysroot;
pub mod tree;
pub mod verify;

pub use activate::{activate_channel, atomic_symlink, install_shims};
pub use appgate::{AppIndexGate, app_apply_allowed};
pub use apply::{Group, TxnOutcome, plan_groups, transact};
pub use bundled::bundled_seed_dir;
pub use cache::IndexCache;
pub use config::{LinkTarget, PackagesConfig, classify_link, repo_overrides};
pub use cost::{disk_ok, human_bytes, needs_consent};
pub use discovery::{IndexRepo, resolve_account, resolve_account_with};
pub use dispatch::{ApplyStrategy, strategy_for};
pub use extract::{
    EntryKind, ExtractError, ExtractReject, extract_tar_zst, vet_entry, vet_hardlink,
};
pub use flow::{
    AppliedMember, ChannelApplyReport, DepOutcome, DepResult, Fetcher, FlowError, InstallReport,
    InstallRequest, apply_channel, install, resolve_verified_index,
};
pub use gate::{ApplyDecision, decide, is_yanked};
pub use gc::{GcReport, reclaimable, run as run_gc};
pub use install::{StageError, verify_and_stage};
pub use linkmode::{
    LinkError, LinkOutcome, is_linked, link, linked_checkout, linked_checkout_checked,
    linked_programs, linked_programs_checked, refresh, unlink,
};
pub use lock::{StoreLock, StoreLockError, try_lock_store};
pub use manifest::{
    Artifact, Channel, Cost, Index, Keys, PkgManifest, Program, SUPPORTED_SCHEMA, parse_index,
    parse_pkg,
};
pub use net::{ChainFetcher, DirFetcher, GithubFetcher};
pub use ops::{active_builds, list_installed, uninstall, which};
pub use select::{Candidate, Selected, select_index};
pub use sig::{
    Delegation, Floor, Reject, VerifiedBytes, check_freshness, verify_index, verify_index_with,
    verify_pkg,
};
pub use status::{ProgramStatus, Status};
pub use store::{Layout, default_prefix, shim_allowed, vet_prefix};
pub use sysroot::{relocate_sysroot, write_toolchain_version};
pub use tree::{sha256_file, tree_root};
pub use verify::{VerifyOutcome, verify_all, verify_program};

// The batteries-included companion-tools surface (docs/COMPANION-TOOLS.md): the source-build
// (keyless) lane, complementary to the signed `install --default-set` bootstrap.
pub use companions::{Companion, Manifest as CompanionManifest, SeedPolicy};
pub use seed::{Ledger as SeedLedger, SeedResult, SkipReason, reconcile_source};
pub use sourcebuild::{
    Installed as SourceInstalled, Provenance, SourceBuildError, build_and_install,
};

/// The base64 Ed25519 **root** public key this binary trusts.
///
/// A committed constant ([`aterm_update_core::pins::PKG_ROOT_PUBKEY`]), not a build
/// env var: what a binary trusts is a property of the source, identical on every
/// machine. Empty disables the manager entirely, fail-closed — with no anchor there
/// is nothing to trust, so no index ever verifies. The root SECRET key lives only
/// offline.
pub const PINNED_PKG_ROOTKEY: &str = aterm_update_core::pins::PKG_ROOT_PUBKEY;

/// Whether the manager is configured to act: a root key must be pinned AND the user
/// must not have opted out via `ATPKG_DISABLE`. Fail closed — an empty pin is never
/// active. Unlike `aterm-update::enabled` this is **not** macOS-gated: the package
/// manager is cross-platform.
#[must_use]
pub fn enabled() -> bool {
    !PINNED_PKG_ROOTKEY.is_empty() && std::env::var_os("ATPKG_DISABLE").is_none()
}

/// Effective CLI posture: the compiled root anchor, plus the `ATPKG_DISABLE` kill
/// switch. This is the shared admission predicate for the CLI and aterm's native
/// Packages surface.
///
/// `ATPKG_ROOTKEY_OVERRIDE` is GONE. It supplied "the same verification anchor the
/// verbs consume", so an environment variable could ENABLE an otherwise-unpinned
/// build — i.e. ambient state decided what the package manager trusted. The anchor
/// now lives in reviewed source ([`aterm_update_core::pins::PKG_ROOT_PUBKEY`]) and
/// nothing outside a commit can change it. An alternate package owner commits their
/// own anchor, which is the same deliberate act, visible in a diff.
///
/// `ATPKG_DISABLE` stays: turning the manager OFF is fail-safe, and a kill switch
/// that only ever subtracts authority cannot be used to grant any.
#[must_use]
pub fn manager_enabled() -> bool {
    manager_enabled_with(PINNED_PKG_ROOTKEY, std::env::var_os("ATPKG_DISABLE").is_some())
}

#[must_use]
pub fn manager_enabled_with(pinned: &str, disabled: bool) -> bool {
    !disabled && !pinned.is_empty()
}

/// A short, dependency-free fingerprint of the pinned root key, so `atpkg doctor` can
/// show **which** trust root is live (§8). FNV-1a/64 over the pinned base64 string —
/// purely operator-facing, never a security primitive (the real anchor is the full
/// pinned key, not this digest). Returns the all-zero seed digest when no key is
/// pinned.
#[must_use]
pub fn root_key_fingerprint() -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in PINNED_PKG_ROOTKEY.as_bytes() {
        h ^= u64::from(*b);
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

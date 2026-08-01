// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The single shared **verification gate** for aterm's conformance and spec-link
//! tests — the honesty ratchet, TWO-TIER since 2026-07-06 (VERIFY-1, owner
//! decision).
//!
//! ## The two tiers
//!
//! **Tier default — the in-process interpreter** ([`crate::interp`]): every
//! DERIVED-model obligation (Tier-0 invariant checks, prove-and-catch
//! non-vacuity, per-transition conformance) is discharged by an exhaustive BFS
//! of the same bounded model through the embedded executable interpreter — a
//! REAL check that fails loudly on a violation, never a skip. A fresh clone
//! with no toolchain verifies for real, out of the box.
//!
//! **Tier escalation — the external Trust binaries**: wherever `ty` /
//! `trust-ir` / `ay` are installed, their applicable analyses run too. `ty` checks
//! the same derived model where supported, and those same-model verdicts must agree
//! with the interpreter. Tool-only analyses — hand-written `.tla`,
//! `--strict-vacuity` verdicts, TrustIr structural cross-reference analysis, and SMT
//! certificates — have their own scoped contracts. In particular, aterm's current
//! TrustIr artifact is explicitly `DesignOnly`: `spec-link` is a non-certifying
//! structural analysis, not Ob.3 certification. A same-model disagreement PANICS;
//! an external-only analysis reports a prominent one-line notice and returns early
//! where its tool is absent (the [`ty_escalation`]-family idiom).
//!
//! ## The checker is part of Trust
//!
//! `ty`/`trust-ir` live in the Trust toolchain (`$HOME/trust/first-party/{ty,trust-ir}`),
//! NOT a standalone checkout. Build them once:
//!
//! ```sh
//! cargo build --release -p tla-cli   # in $HOME/trust/first-party/ty       -> ty
//! cargo build --release              # in $HOME/trust/first-party/trust-ir -> trust-ir
//! ```
//!
//! [`find_ty`]/[`find_trust_ir`] then discover them automatically at their canonical
//! release paths — or anywhere the full-toolchain bootstrap (`build/<triple>/…`)
//! dropped them, or on `PATH`. The home directory (`$HOME`, or `%USERPROFILE%` on
//! Windows) and `PATH` are the only environment access; there is no path override
//! and nothing to remember to set.
//!
//! ## Caller idioms
//!
//! ```ignore
//! // SCALAR derived model — the overwhelmingly common case (66 of the 70 in
//! // tree, and every `ty_model!`-authored one). The interpreter always runs, so
//! // coverage is unconditional and the returned [`Covered`] is a log line, not
//! // a decision:
//! aterm_spec::verify::check_scalar(&m, "Thing Tier-0");
//! aterm_spec::verify::prove_and_catch_scalar(&m, "Thing non-vacuity");
//!
//! // FUNCTION-VALUED derived model — the interpreter cannot evaluate it, so
//! // without `ty` the obligation does NOT run. The `_tiered` forms return
//! // `Result<Covered, NotRun>` precisely so the caller must state a policy:
//! match aterm_spec::verify::check_model_tiered(&m, "Thing Tier-0") { /* ... */ }
//!
//! let (ok, why) = aterm_spec::verify::validate_transition_tiered(&m, &overrides, &prev, &next, Some("Push"), "Thing conformance");
//!
//! // External-tool analysis (runs only where the tool exists):
//! let Some(ty) = aterm_spec::verify::ty_escalation("Thing .tla check") else { return };
//! ```
//!
//! The LEGACY hard-require forms ([`ty`], [`trust_ir`], [`ay`]) still exist for
//! gates that must never run tool-less (none in-tree today outside migration).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use crate::derive::Model;
use crate::interp;

/// Discover the Trust `ty` model-checker. Searches, in order: the canonical
/// first-party cargo build, any full-toolchain bootstrap stage build, then `ty` on
/// `PATH`. The home directory and `PATH` are the only environment access.
#[must_use]
pub fn find_ty() -> Option<PathBuf> {
    find_trust_bin("ty", "ty/target/release")
}

/// Discover the Trust `trust-ir` `spec-link` cross-referencer. Mirrors [`find_ty`].
#[must_use]
pub fn find_trust_ir() -> Option<PathBuf> {
    find_trust_bin("trust-ir", "trust-ir/target/release")
}

/// Discover the Trust `ay` SAT/SMT/CHC solver. Mirrors [`find_ty`] exactly
/// (canonical first-party build → full-toolchain bootstrap stage scan → PATH),
/// plus the standalone `~/ay` release checkout some proof bundles' `verify.sh`
/// scripts also probe. Used by the always-on `sparkle_v2_ay_certificates`
/// gate: hand-encoded SMT-LIB2 certificates are re-checked fail-closed, the
/// same honesty ratchet as the `ty` Tier-0 gates.
#[must_use]
pub fn find_ay() -> Option<PathBuf> {
    if let Some(p) = find_trust_bin("ay", "ay/target/release") {
        return Some(p);
    }
    // Standalone `~/ay` checkout, probed the same Windows-aware way as `find_trust_bin`
    // (all candidate homes + the platform exe name) rather than `$HOME` only.
    let exe = exe_name("ay");
    for home in home_dirs() {
        let standalone = home.join("ay/target/release").join(&exe);
        if standalone.exists() {
            return Some(standalone);
        }
    }
    None
}

/// Shared discovery: the canonical `$HOME/trust/first-party/<rel_dir>/<bin>` cargo
/// build, then any matching tool the full-toolchain bootstrap left under
/// `$HOME/trust/build/<triple>/…`, then `<bin>` on `PATH`. All filesystem probes use
/// the platform executable name (`ty` vs `ty.exe`).
// Skip: drop glue for the `Vec<PathBuf>` search-path IntoIter (std/alloc
// internals — the drop-glue lane). Build-tooling discovery; every miss
// returns None (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
fn find_trust_bin(bin: &str, first_party_rel_dir: &str) -> Option<PathBuf> {
    let exe = exe_name(bin);
    for home in home_dirs() {
        let canonical = home
            .join("trust/first-party")
            .join(first_party_rel_dir)
            .join(&exe);
        if canonical.exists() {
            return Some(canonical);
        }
        if let Some(p) = scan_trust_bootstrap(&home.join("trust/build"), &exe) {
            return Some(p);
        }
    }
    // The atpkg-managed store (batteries-included installs): the per-tool shim
    // under the manager-owned prefix. Probed after the developer checkouts (a
    // live $HOME/trust always wins) and before PATH — atpkg's bin/ reaches PATH
    // only in interactive aterm shells (~/.aterm/shell.d, APPENDED), so
    // without this probe a seeded toolchain is invisible to `cargo test` and
    // CI processes. A shim is trusted only when it resolves to a real file
    // (a dangling link after a GC must not satisfy discovery).
    if let Some(p) = atpkg_store_probe(&exe) {
        return Some(p);
    }
    path_search(&exe)
}

/// The default atpkg store shim for `exe`, resolved. The prefix mirrors
/// `atpkg::platform::default_prefix` (`~/Library/Application Support/aterm/pkg`
/// on Unix, `%LOCALAPPDATA%\aterm\pkg` on Windows) — kept as a PATH MIRROR, not
/// a dependency edge, so the spec crate never drags the package manager (ring,
/// tar, zstd) into every conformance consumer; if atpkg ever moves its prefix,
/// update both sites (each carries this cross-reference).
// Skip: fs syscall wrappers (exists/canonicalize — absent std bodies); every
// miss returns None (fail-closed). Build-tooling discovery, not runtime code.
#[cfg_attr(trust_verify, trust::skip)]
fn atpkg_store_probe(exe: &str) -> Option<PathBuf> {
    let bin_dir = if cfg!(windows) {
        let local = std::env::var("LOCALAPPDATA").ok().filter(|d| !d.is_empty())?;
        PathBuf::from(local).join("aterm").join("pkg").join("bin")
    } else {
        unix_store_bin_dir(&home_dirs().into_iter().next()?)
    };
    resolve_store_shim(&bin_dir.join(exe))
}

/// `<home>/Library/Application Support/aterm/pkg/bin` — the Unix half of the
/// prefix mirror (see [`atpkg_store_probe`]).
fn unix_store_bin_dir(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("aterm")
        .join("pkg")
        .join("bin")
}

/// A shim is trusted only when it is a SYMLINK that resolves to a real file.
///
/// The symlink requirement is load-bearing, not incidental: atpkg disables a
/// yanked or below-floor build by REPLACING the forwarding symlink with a
/// failing regular-file TOMBSTONE script (`atpkg::activate::install_tombstone_shim`).
/// Accepting any regular file would hand discovery that tombstone — and because
/// this probe runs before the PATH search, a revoked build would then SHADOW a
/// working tool the developer already has on PATH (adversarial review
/// 2026-07-30). A tombstone therefore reads as "absent", which is exactly what
/// a revoked build should look like to the verification tier.
// Skip: fs syscall wrappers (symlink_metadata/canonicalize — absent std
// bodies); every miss returns None (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
fn resolve_store_shim(shim: &Path) -> Option<PathBuf> {
    if !std::fs::symlink_metadata(shim).is_ok_and(|m| m.file_type().is_symlink()) {
        return None;
    }
    let resolved = std::fs::canonicalize(shim).ok()?;
    resolved.is_file().then_some(resolved)
}

/// `<bin>` with the platform executable suffix (`.exe` on Windows, none on Unix).
fn exe_name(bin: &str) -> String {
    format!("{bin}{}", std::env::consts::EXE_SUFFIX)
}

/// Candidate home directories: `$HOME`, then (Windows) `%USERPROFILE%`. Native
/// Windows shells set only `USERPROFILE`; Git Bash sets `HOME`, sometimes as a
/// POSIX-style `/c/…` path that Win32 file APIs cannot resolve — normalize it.
fn home_dirs() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    for var in ["HOME", "USERPROFILE"] {
        if let Ok(dir) = std::env::var(var)
            && !dir.is_empty()
        {
            homes.push(PathBuf::from(normalize_home(dir)));
        }
    }
    homes
}

/// On Windows, rewrite a POSIX-style `/c/Users//…` home to `C:/Users//…`; otherwise
/// pass through unchanged.
fn normalize_home(dir: String) -> String {
    if cfg!(windows) {
        let b = dir.as_bytes();
        if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/' {
            return format!("{}:{}", b[1] as char, &dir[2..]);
        }
    }
    dir
}

/// Best-effort scan of the full-toolchain bootstrap output
/// (`$HOME/trust/build/<triple>/{stage2-tools-bin/<triple>,stage1/bin}/<exe>`) — the
/// layout `x.py`/bootstrap produces when the whole Trust compiler is built.
// Skip: the `read_dir` iterator's `next` is an absent std body under the
// generic trait path (fs iteration); every I/O miss returns None
// (fail-closed). Build-tooling discovery, not runtime code.
#[cfg_attr(trust_verify, trust::skip)]
fn scan_trust_bootstrap(build: &Path, exe: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(build).ok()?.flatten() {
        let triple_dir = entry.path();
        let triple = entry.file_name();
        for cand in [
            triple_dir.join("stage2-tools-bin").join(&triple).join(exe),
            triple_dir.join("stage1").join("bin").join(exe),
        ] {
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    None
}

/// `PATH` lookup via `std::env::split_paths` — portable, no shell dependency.
// Skip: `Path::is_file` is an fs syscall wrapper (absent body); every
// miss returns None (fail-closed). Build-tooling discovery.
#[cfg_attr(trust_verify, trust::skip)]
fn path_search(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|cand| cand.is_file())
}

/// Locate the Trust `ty` model-checker for `label`, or PANIC with a build hint.
/// Verification is ALWAYS required — no env var, no skip. A conformance test that
/// cannot reach `ty` FAILS rather than reporting a false `ok`.
#[must_use]
pub fn ty(label: &str) -> PathBuf {
    require(
        "ty",
        "cargo build --release -p tla-cli   (in $HOME/trust/first-party/ty)",
        find_ty(),
        label,
    )
}

/// Locate the Trust `trust-ir` `spec-link` tool for `label`, or PANIC with a build
/// hint. Always required — see [`ty`].
#[must_use]
pub fn trust_ir(label: &str) -> PathBuf {
    require(
        "trust-ir",
        "cargo build --release   (in $HOME/trust/first-party/trust-ir)",
        find_trust_ir(),
        label,
    )
}

/// Locate the Trust `ay` solver for `label`, or PANIC with a build hint.
/// Always required — see [`ty`] (the same fail-closed gate, no skip path).
#[must_use]
pub fn ay(label: &str) -> PathBuf {
    require(
        "ay",
        "cargo build --release   (in $HOME/trust/first-party/ay)",
        find_ay(),
        label,
    )
}

/// The gate: return the discovered path, or PANIC. There is no skip and no opt-out —
/// the honesty ratchet, batteries-on.
// AUDITED CONTRACT PANIC (T9 surface): this panic IS the product — the
// honesty ratchet's fail-closed gate. The declaration reclassifies its
// refuted panic-freedom obligation into the always-visible `contract-panic`
// gate column; it can never mask any other panic in this fn.
#[cfg_attr(
    trust_verify,
    trust::contract_panic(message_contains = "VERIFICATION GATE")
)]
// Skip: a build-tooling PRECONDITION — its panic is the deliberate
// "verifier binary missing, build it first" abort (the documented contract:
// verification is always required and a missing checker must FAIL loudly,
// never silently skip). Not shipping runtime code.
#[cfg_attr(trust_verify, trust::skip)]
fn require(bin: &str, build_hint: &str, found: Option<PathBuf>, label: &str) -> PathBuf {
    found.unwrap_or_else(|| {
        panic!(
            "VERIFICATION GATE: Trust `{bin}` not found — `{label}` could NOT be \
             model-checked / spec-linked. Build the Trust toolchain once: {build_hint}. \
             Verification is always required; this test FAILS rather than reporting a \
             false ok."
        )
    })
}

// ---------------------------------------------------------------------------
// The ESCALATION tier (VERIFY-1, owner decision 2026-07-06): EXTERNAL-tool
// analyses run only where their tool is installed. The notice is one prominent
// line naming exactly what did not run and how to enable it — never silent, and
// never claimed as a discharged check. Same-model agreement is required only for
// the tiered interpreter/ty paths below.
// ---------------------------------------------------------------------------

/// Shared escalation report + early-return decision.
fn escalation(bin: &str, build_hint: &str, found: Option<PathBuf>, label: &str) -> Option<PathBuf> {
    if found.is_none() {
        eprintln!(
            "VERIFY ESCALATION TIER NOT RUN: Trust `{bin}` is not installed, so `{label}` \
             (an external-tool-analysis obligation) did not run on this machine. The \
             applicable in-process derived-model checks still ran. Enable the \
             escalation tier once: {build_hint}."
        );
    }
    found
}

/// The `ty` ESCALATION tier: `Some(path)` when installed; `None` (with a
/// prominent notice) otherwise — the caller returns early. Use for obligations
/// the in-process tier cannot express: hand-written `.tla` specs,
/// `--strict-vacuity` verdicts, `ty trace` overclaim controls.
#[must_use]
pub fn ty_escalation(label: &str) -> Option<PathBuf> {
    escalation(
        "ty",
        "cargo build --release -p tla-cli   (in $HOME/trust/first-party/ty)",
        find_ty(),
        label,
    )
}

/// The `trust-ir` (`spec-link`) escalation tier. aterm's current emitted artifact is
/// explicitly `DesignOnly`, so this is non-certifying structural analysis rather than
/// Ob.3 certification. See [`ty_escalation`] for discovery/notice behavior.
#[must_use]
pub fn trust_ir_escalation(label: &str) -> Option<PathBuf> {
    escalation(
        "trust-ir",
        "cargo build --release   (in $HOME/trust/first-party/trust-ir)",
        find_trust_ir(),
        label,
    )
}

/// The `ay` (SMT/CHC certificate) escalation tier. See [`ty_escalation`].
#[must_use]
pub fn ay_escalation(label: &str) -> Option<PathBuf> {
    escalation(
        "ay",
        "cargo build --release   (in $HOME/trust/first-party/ay)",
        find_ay(),
        label,
    )
}

// ---------------------------------------------------------------------------
// The TIERED derived-model discharges (VERIFY-1): interpreter ALWAYS (a real,
// loud-failing check), external `ty` ADDITIONALLY wherever installed. The two
// tiers check the SAME derived model; disagreement panics.
// ---------------------------------------------------------------------------

/// How a tiered obligation WAS discharged — every variant is real coverage.
///
/// There is deliberately no "did not run" inhabitant: holding one of these is a
/// claim that the obligation ran, and a caller that wants coverage must not be
/// able to accept its absence by accident. The no-coverage case is [`NotRun`],
/// the `Err` half of the `_tiered` functions' return type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Covered {
    /// Interpreter BMC only (no `ty` installed) — still a real exhaustive check.
    Interpreter,
    /// Interpreter BMC AND the external `ty` binary agreed.
    InterpreterAndTy,
    /// Function-valued model: interpreter inapplicable, `ty` checked it.
    TyOnly,
}

/// The obligation did NOT run: a FUNCTION-VALUED model (which the in-process
/// interpreter cannot evaluate) on a machine with no Trust `ty`.
///
/// This is the `Err` half rather than a `Covered` variant because `Result` is
/// `#[must_use]`: an added call site cannot drop it with a bare statement, and a
/// scalar model that later grows an `fn_vars` entry turns every one of its call
/// sites into a compile error instead of a silent green. The `_tiered` functions
/// have already printed the detailed notice by the time this is returned — the
/// caller's job is to state a POLICY (skip loudly, or fail the gate), not to
/// re-report the fact.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NotRun {
    /// The model whose obligation went unchecked (`Model::name`).
    pub model: &'static str,
}

/// Run `ty check` on a derived model's generated spec + cfg; panics on failure
/// (with the generated TLA for diagnosis). `cfg` overrides come pre-rendered.
// Skip: shells out to `ty` and renders its output (absent std format/io
// bodies + the closure it drives). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
/// Returns (verdict, evidence): whether `ty check` exited 0, plus a rendered
/// transcript naming the exact binary and its full output — so a TIER
/// DISAGREEMENT panic identifies WHICH `ty` build said what (the 2026-07-20
/// `derived_native_tab_identity` disagreement was undiagnosable precisely
/// because the panic carried neither the binary path nor its output; the
/// culprit turned out to be transient toolchain drift, not the model).
fn ty_check_derived(ty: &Path, m: &Model, cfg: &str, label: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("aterm-tier-{}-{}", m.name, std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{}.tla", m.name));
    let cfgp = dir.join(format!("{}.cfg", m.name));
    std::fs::write(&spec, m.to_tla()).expect("write derived spec");
    std::fs::write(&cfgp, cfg).expect("write derived cfg");
    let mut cmd = Command::new(ty);
    cmd.arg("check").arg(&spec).arg("--config").arg(&cfgp);
    let out = ty_output(arm_whole_space_check(&mut cmd))
        .unwrap_or_else(|e| panic!("run ty check for {label}: {e}"));
    let _ = std::fs::remove_dir_all(&dir);
    let built = ty_build_stamp(ty);
    let evidence = format!(
        "ty binary: {} [{built}] ({:?})\n--- ty stdout ---\n{}\n--- ty stderr ---\n{}",
        ty.display(),
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    (out.status.success(), evidence)
}

/// The flags EVERY `ty check` of a derived model carries.
///
/// **Reduction off** (`--no-auto-por`, `--no-auto-symmetry`). The escalation
/// tier's job is to walk the SAME bounded space the interpreter walked, not a
/// cheaper one. Partial-order and symmetry reduction are speed features, and
/// speed is not what this tier buys: the largest model in the registry has 1712
/// reachable states, so the whole sweep costs seconds either way. What reduction
/// DOES cost is the only thing that makes two tiers worth running —
/// comparability. A reduced run explores a representative subset by design, so
/// its state count carries no information, and [`assert_same_space_explored`]
/// (the check that caught this) could not be written at all.
///
/// It is not hypothetical. A `ty` built 2026-07-02 explored ONE state of
/// `NyanJumpBurstLifecycle`'s 128 with reduction on and reported "No errors
/// found (exhaustive)" — a false proof at the committed config, and a missing
/// counterexample at `Buggy = 1`, which is the only reason it surfaced at all.
/// The model's stutter action (`SlowJump`, a self-loop that writes nothing)
/// formed a singleton ample set whose only successor is the expanding state, and
/// that build predated `ty`'s own C3 cycle proviso by four days. Two other
/// models (`NativeConfigTransaction` 1662/1712, `CursorCatCurseWince` 30/36)
/// were under-exploring the same way, silently, because a prove-only model has
/// no catch half to fail. A current `ty` refuses those reductions correctly —
/// which is exactly why aterm cannot rely on that: this tier is only as sound as
/// whichever binary is on the machine, and nothing here may assume it is new.
///
/// **`--initial-capacity 8192`** — tell `ty` these models are small. Left to
/// itself it pre-allocates fingerprint storage for a spec that might have
/// millions of states: measured on `NativeTabIdentity` (399 reachable states),
/// peak RSS was 4.25 GB unset versus 17 MB with the hint. 8192 is 4x the largest
/// model; the hash set still grows on its own, so the hint bounds nothing and
/// under-sizing it cannot truncate a search.
///
/// That footprint is what made the suite FLAKY rather than merely wasteful:
/// ~4 GB per concurrent `ty` collides with the auto-detected memory ceiling,
/// which is itself a function of how many `ty`/`cargo`/`rustc` processes are
/// alive, so a run could be handed a share too small to start in.
/// `NativeTabIdentity` stopped at 1 of 399 states with "memory limit reached"
/// AND EXIT 0 — a truncated run that an exit-status-only verdict books as a
/// proof. That is the likeliest reading of the 2026-07-20
/// `derived_native_tab_identity` "transient toolchain drift" note above: not
/// drift, a ceiling that moved with the load.
///
/// The ceiling is deliberately NOT pinned with `--memory-limit`. Pinning it was
/// tried and made things worse under exactly the load it was meant to fix:
/// `ty`'s limit probe is not purely per-process, so a small explicit ceiling
/// trips on a busy machine even when this run's own RSS is 17 MB — the same
/// false stop, now pinned on. Shrinking the footprint is the fix; the ceiling
/// was only ever a symptom of it.
// Skip: argument plumbing for a subprocess. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn arm_whole_space_check(cmd: &mut Command) -> &mut Command {
    cmd.arg("--no-auto-por")
        .arg("--no-auto-symmetry")
        .arg("--initial-capacity")
        .arg("8192")
}

/// Run a `ty` subprocess — ONE AT A TIME, across the whole test binary.
///
/// Not a fairness or disk-contention measure, a CORRECTNESS one. `ty` sizes its
/// memory budget as host RAM divided by the number of live `ty`/`cargo`/`rustc`
/// processes, so concurrent checks shrink each other's budget until one is
/// stopped mid-search — and a stopped search still prints "no errors" and still
/// exits 0. That makes the verdict a function of how busy the machine was, which
/// is not a property a proof is allowed to have. `cargo test`'s own parallelism
/// is exactly the load that triggers it, so the tests discharging these
/// obligations were racing the thing they were discharging.
///
/// The lock is around the SUBPROCESS, not the test: the interpreter tier — the
/// expensive half — still runs fully parallel. Measured cost on the
/// 194-obligation ring suite: ~55s, against a `--test-threads=1` upper bound of
/// 70s.
///
/// Poisoning is recovered rather than propagated: a model that panics while
/// holding this lock has already failed its own obligation loudly, and turning
/// every LATER model into a "poisoned lock" panic would bury that one real
/// diagnostic under a hundred fake ones.
// Skip: spawns a subprocess under a lock. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn ty_output(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    static TY_SERIAL: Mutex<()> = Mutex::new(());
    let _serial = TY_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    cmd.output()
}

/// The checker binary's AGE, for the evidence transcript.
///
/// `ty --version` reports a package version that does not move between builds,
/// so it cannot tell a checker built today from one built a month ago — and on
/// 2026-07-30 that distinction was the whole answer: the installed `ty` was four
/// days older than the fix for the very reduction bug it was exhibiting. The
/// mtime is the one cheap fact that would have said so on the first read.
// Skip: filesystem metadata formatting. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn ty_build_stamp(ty: &Path) -> String {
    std::fs::metadata(ty)
        .and_then(|md| md.modified())
        .map_or_else(
            |_| "mtime unknown".to_string(),
            |t| {
                let secs = t
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                format!("mtime {secs} (unix)")
            },
        )
}

/// The number of distinct states `ty` reports having explored, read from the
/// `States found: N` line of its statistics block.
///
/// `None` means the line was absent — a run that never reached the statistics
/// block, or an output-format drift. Callers treat `None` as a FAILURE to
/// establish agreement, never as agreement: this parse is the only evidence
/// aterm has that the escalation tier looked at anything at all.
// Skip: string scanning over another tool's output. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn ty_states_explored(evidence: &str) -> Option<u64> {
    evidence
        .lines()
        .find_map(|l| l.trim().strip_prefix("States found:"))
        .and_then(|n| n.trim().parse().ok())
}

/// The EVIDENCE half of tier agreement: a CLEAN `ty` verdict must also have
/// come from the same bounded reachable space the interpreter proved over.
///
/// The two tiers already had to agree on the VERDICT. That is weaker than it
/// looks, because "no errors found" is also what a checker says about a space
/// it never entered — and on 2026-07-30 that is exactly what happened
/// (`NyanJumpBurstLifecycle`: interpreter 128 states, `ty` 1, both "clean").
/// A verdict agreement between a real proof and a vacuous one is not evidence;
/// it is a coincidence that reads like evidence.
///
/// So the tiers must agree on the WORK as well as the answer. Both walk the
/// whole reachable space of the same finite machine, and `to_cfg` emits no
/// VIEW and no SYMMETRY, so with reduction off (see `ty_check_derived`) their
/// state counts are not merely close — they are EQUAL, verified across all 114
/// scalar models in `xref::model_registry()` at the time this landed.
///
/// Applies only to a clean verdict: a `Buggy = 1` run is SUPPOSED to stop at
/// the first counterexample, so its count is legitimately smaller and is not
/// compared.
// Skip: a verification-harness assert — the panic IS the gate. Not shipping
// runtime code.
#[cfg_attr(trust_verify, trust::skip)]
fn assert_same_space_explored(m: &Model, interp_states: usize, evidence: &str, label: &str) {
    let Some(ty_states) = ty_states_explored(evidence) else {
        panic!(
            "{label}: {} — `ty` returned a CLEAN verdict with no `States found:` line, so there \
             is no evidence it explored anything. The escalation tier cannot be credited on an \
             unparseable transcript (output-format drift?).\n{evidence}",
            m.name
        )
    };
    assert!(
        ty_states == interp_states as u64,
        "{label}: {} — TIER DISAGREEMENT ON THE EXPLORED SPACE: the interpreter walked \
         {interp_states} reachable states, `ty` reports {ty_states}. Both tiers claim an \
         exhaustive walk of the SAME machine, so these must be equal; they are not, which means \
         one tier's \"clean\" is about a space the other never saw. A `ty` count that is SMALLER \
         is a false proof (a state-space reduction that dropped reachable behaviour); a LARGER \
         one means the emitted TLA+ admits states the model does not.\n{evidence}",
        m.name
    );
}

/// TIERED Tier-0 check of a derived model: the interpreter proves every
/// invariant over the whole bounded reachable space (panics on a violation),
/// and `ty check` additionally proves the generated TLA+ wherever installed
/// (panics on failure or on tier disagreement). Function-valued models skip
/// the interpreter (inapplicable by construction) and REQUIRE the `ty` tier;
/// with no `ty` they report [`NotRun`] loudly AND return it, so the caller has
/// to state a policy. Scalar callers want [`check_scalar`], which discharges
/// that obligation once rather than at every site.
// Skip: a tiered verification DRIVER — shells out to `ty`, renders output,
// and its asserts are deliberate harness aborts. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn check_model_tiered(m: &Model, label: &str) -> Result<Covered, NotRun> {
    // `Some(n)` = the interpreter ran and proved the invariants over `n`
    // reachable states; that count is the yardstick the `ty` tier is held to
    // below. `None` = function-valued, so there is no interpreter tier and no
    // yardstick.
    let interp_states = if m.fn_vars.is_empty() {
        match interp::bmc(m) {
            Ok(n) => {
                eprintln!("{label}: {} proven over {n} states (interpreter).", m.name);
                Some(n)
            }
            Err((st, inv)) => panic!(
                "{label}: {} invariant `{inv}` VIOLATED at {st:?} (interpreter tier)",
                m.name
            ),
        }
    } else {
        None
    };
    let interp_ran = interp_states.is_some();
    match find_ty() {
        Some(ty) => {
            let (ok, evidence) = ty_check_derived(&ty, m, &m.to_cfg(), label);
            assert!(
                ok,
                "{label}: ty check FAILED on derived {} spec{}\n{evidence}\n--- generated ---\n{}",
                m.name,
                if interp_ran {
                    " — TIER DISAGREEMENT (interpreter proved it; checker bug?)"
                } else {
                    ""
                },
                m.to_tla()
            );
            if let Some(n) = interp_states {
                assert_same_space_explored(m, n, &evidence, label);
            }
            eprintln!(
                "{label}: {} additionally model-checked clean by ty.",
                m.name
            );
            if interp_ran {
                Ok(Covered::InterpreterAndTy)
            } else {
                Ok(Covered::TyOnly)
            }
        }
        None if interp_ran => Ok(Covered::Interpreter),
        None => {
            eprintln!(
                "VERIFY ESCALATION TIER NOT RUN: `{label}` ({}) is a FUNCTION-VALUED model \
                 the interpreter cannot evaluate and Trust `ty` is not installed — this \
                 obligation did not run. Build ty once: cargo build --release -p tla-cli \
                 (in $HOME/trust/first-party/ty).",
                m.name
            );
            Err(NotRun { model: m.name })
        }
    }
}

/// TIERED prove-and-catch (the `Buggy` convention): the interpreter proves the
/// invariant at `Buggy=0` and finds a counterexample at `Buggy=1` (panics
/// otherwise), and `ty` additionally does the same wherever installed (panics
/// on failure or tier disagreement). Function-valued models (`fn_vars`
/// non-empty: EvictFull, TierResidency, Recording, Coalesce — the only four in
/// tree) skip the interpreter (inapplicable by construction) and REQUIRE the
/// `ty` tier; with no `ty` they report [`NotRun`] loudly AND return it. Scalar
/// callers want [`prove_and_catch_scalar`].
// Skip: same tiered-driver class as `check_model_tiered`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_and_catch_tiered(m: &Model, label: &str) -> Result<Covered, NotRun> {
    // `prove_and_catch` proves the `Buggy = 0` arm and catches the `Buggy = 1`
    // one; re-walking the committed arm here is what gives the `ty` tier its
    // yardstick (see `assert_same_space_explored`). It is a second BFS over a
    // space `prove_and_catch` just proved terminates and holds, so it cannot
    // fail — but it is matched anyway rather than unwrapped, because a silent
    // `Err` here would be the interpreter contradicting itself.
    let interp_states = if m.fn_vars.is_empty() {
        interp::prove_and_catch(m);
        match interp::bmc(&interp::with_buggy(m, 0)) {
            Ok(n) => Some(n),
            Err((st, inv)) => panic!(
                "{label}: {} invariant `{inv}` VIOLATED at {st:?} on the Buggy=0 re-walk, after \
                 `prove_and_catch` proved the same arm clean — the interpreter tier is \
                 self-inconsistent",
                m.name
            ),
        }
    } else {
        None
    };
    let interp_ran = interp_states.is_some();
    match find_ty() {
        Some(ty) => {
            let (ok, evidence) = ty_check_derived(&ty, m, &m.to_cfg(), label);
            assert!(
                ok,
                "{label}: ty check FAILED at Buggy=0 on {}{}\n{evidence}\n--- generated ---\n{}",
                m.name,
                if interp_ran {
                    " — TIER DISAGREEMENT (interpreter proved it; checker bug?)"
                } else {
                    ""
                },
                m.to_tla()
            );
            // The committed arm is the one that claims a PROOF, so it is the one
            // held to the explored-space law. The `Buggy = 1` run below is
            // required to stop early at its counterexample, so its count is
            // legitimately smaller and is deliberately not compared.
            if let Some(n) = interp_states {
                assert_same_space_explored(m, n, &evidence, label);
            }
            let (caught_ok, evidence) =
                ty_check_derived(&ty, m, &m.to_cfg_with(&[("Buggy", 1)]), label);
            assert!(
                !caught_ok,
                "{label}: ty found NO counterexample at Buggy=1 on {}{}\n{evidence}",
                m.name,
                if interp_ran {
                    " — TIER DISAGREEMENT (interpreter caught it; checker bug?)"
                } else {
                    " — the property is trivial / does not catch the defect"
                }
            );
            eprintln!(
                "{label}: {} {}proven (Buggy=0) and caught (Buggy=1) by ty.",
                m.name,
                if interp_ran { "additionally " } else { "" }
            );
            if interp_ran {
                Ok(Covered::InterpreterAndTy)
            } else {
                Ok(Covered::TyOnly)
            }
        }
        None if interp_ran => Ok(Covered::Interpreter),
        None => {
            eprintln!(
                "VERIFY ESCALATION TIER NOT RUN: `{label}` ({}) is a FUNCTION-VALUED model \
                 the interpreter cannot evaluate and Trust `ty` is not installed — this \
                 obligation did not run. Build ty once: cargo build --release -p tla-cli \
                 (in $HOME/trust/first-party/ty).",
                m.name
            );
            Err(NotRun { model: m.name })
        }
    }
}

/// Discharge a SCALAR model's Tier-0 obligation, asserting the scalar shape that
/// makes the `_tiered` form's `Err` half unreachable: the interpreter runs for
/// every model with no `fn_vars`, so coverage is unconditional and the returned
/// [`Covered`] is a log line rather than a decision.
///
/// This states, ONCE, the assertion the ~21 scalar call sites used to make
/// implicitly by dropping the old `Discharge` with a bare statement — the same
/// guard [`deadlock_free_and_catches_tiered`] already writes out. Asserting the
/// shape up front rather than unwrapping the result is what makes it
/// machine-independent: a model that later grows an `fn_vars` entry fails here
/// on every machine, instead of passing on developer boxes that happen to have
/// `ty` installed and failing only on a fresh clone.
// Skip: thin policy wrapper over `check_model_tiered` (same verification-tooling
// tier); its assert is a deliberate harness abort.
#[cfg_attr(trust_verify, trust::skip)]
pub fn check_scalar(m: &Model, label: &str) -> Covered {
    assert_scalar(m, label, "check_scalar");
    match check_model_tiered(m, label) {
        Ok(c) => c,
        Err(n) => unreachable!("{label}: scalar model {} reported {n:?}", m.name),
    }
}

/// Prove-and-catch a SCALAR model's obligation. See [`check_scalar`] for why the
/// scalar shape is asserted rather than the result unwrapped.
// Skip: same thin-policy-wrapper class as `check_scalar`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_and_catch_scalar(m: &Model, label: &str) -> Covered {
    assert_scalar(m, label, "prove_and_catch_scalar");
    match prove_and_catch_tiered(m, label) {
        Ok(c) => c,
        Err(n) => unreachable!("{label}: scalar model {} reported {n:?}", m.name),
    }
}

/// PROVE, CATCH **and MULTIPLY** — the discharge protocol for a model whose
/// safety argument is GLOBAL across every live instance of an enforcing
/// structure, not local to one of them.
///
/// The flash limiter is the motivating case. `FlashLimiter` proves ≤ 2
/// ignitions per rolling second for ONE limiter, and that theorem stays true
/// however many limiters exist — so it cannot see the defect where each split
/// pane gets its own. A model that CAN see it needs a second dial beside
/// `Buggy`, and this protocol is what keeps that dial honest.
///
/// Five gates, all discharged on both tiers (the interpreter always, `ty`
/// wherever installed; a disagreement panics):
///
/// * **G1 SHAPE** — the model declares `Buggy`, `Local` and `Instances`, and
///   commits them to `Buggy = 0`, `Local = 0`, `Instances >= 2`. Committing
///   `Instances >= 2` is what makes G2 a statement about a MULTI-enforcer
///   world rather than a vacuous one.
/// * **G2 PROVE** — every invariant holds at every `scenarios` corner.
/// * **G3 CATCH** — the classic non-vacuity mutant: `Buggy = 1` at `buggy_at`
///   yields a counterexample.
/// * **G4 MULTIPLY** — `Local = 1` (each enforcer applying the bound to its
///   OWN state) yields a counterexample at EVERY corner. The asymmetry with G3
///   is load-bearing: an overlap-blind limiter needs the overlap scenario to
///   misbehave, while multiplying enforcers is wrong unconditionally.
/// * **G5 ATTRIBUTION** — `Local = 1, Instances = 1` HOLDS, which proves the
///   G4 counterexample is attributable to the instance COUNT and not to some
///   unrelated damage the `Local` dial does to the model.
///
/// # Panics
///
/// On any gate failing, or on a tier disagreement — the same honesty ratchet
/// as [`prove_and_catch_scalar`].
// Skip: a tiered verification DRIVER (shells out to `ty`, renders output, and
// its asserts are deliberate harness aborts). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_catch_and_multiply_scalar(
    m: &Model,
    scenarios: &[&[(&'static str, i64)]],
    buggy_at: &[(&'static str, i64)],
    label: &str,
) -> Covered {
    assert_scalar(m, label, "prove_catch_and_multiply_scalar");
    // ---- G1 SHAPE ----
    let committed = |name: &str| -> i64 {
        m.consts
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| {
                panic!(
                    "{label}: {} declares no `{name}` constant — a MULTIPLY obligation needs \
                     the `Buggy` / `Local` / `Instances` dials to exist",
                    m.name
                )
            })
    };
    assert_eq!(
        committed("Buggy"),
        0,
        "{label}: {} commits Buggy != 0",
        m.name
    );
    assert_eq!(
        committed("Local"),
        0,
        "{label}: {} commits Local != 0",
        m.name
    );
    let instances = committed("Instances");
    assert!(
        instances >= 2,
        "{label}: {} commits Instances = {instances} — the committed world must already \
         contain SEVERAL enforcers, or G2 proves nothing about aggregation",
        m.name
    );
    assert!(
        !scenarios.is_empty(),
        "{label}: {} was given no scenario corners to prove over",
        m.name
    );

    let ty = find_ty();
    let gate = |extra: &[(&'static str, i64)], scenario: &[(&'static str, i64)], gate: &str| {
        let mut cfg: Vec<(&'static str, i64)> = scenario.to_vec();
        for (n, v) in extra {
            match cfg.iter_mut().find(|(c, _)| c == n) {
                Some(slot) => slot.1 = *v,
                None => cfg.push((*n, *v)),
            }
        }
        let interp_verdict = interp::bmc(&interp::with_consts(m, &cfg));
        let ty_verdict = ty
            .as_ref()
            .map(|t| ty_check_derived(t, m, &m.to_cfg_with(&cfg), label));
        if let Some((ok, evidence)) = &ty_verdict {
            assert_eq!(
                *ok,
                interp_verdict.is_ok(),
                "{label}: {} TIER DISAGREEMENT at {gate} ({cfg:?}) — interpreter said {}, ty \
                 said {}\n{evidence}",
                m.name,
                if interp_verdict.is_ok() {
                    "HOLDS"
                } else {
                    "VIOLATED"
                },
                if *ok { "HOLDS" } else { "VIOLATED" },
            );
        }
        (cfg, interp_verdict)
    };

    // ---- G2 PROVE ----
    for scenario in scenarios {
        let (cfg, verdict) = gate(&[], scenario, "G2 PROVE");
        match verdict {
            Ok(n) => eprintln!(
                "{label}: G2 PROVE {} holds over {n} states at {cfg:?}.",
                m.name
            ),
            Err((st, inv)) => panic!(
                "{label}: G2 PROVE FAILED — {} invariant `{inv}` VIOLATED at {st:?} ({cfg:?})",
                m.name
            ),
        }
    }
    // ---- G3 CATCH ----
    {
        let (cfg, verdict) = gate(&[("Buggy", 1)], buggy_at, "G3 CATCH");
        match verdict {
            Ok(n) => panic!(
                "{label}: G3 CATCH FAILED — {} found NO counterexample over {n} states at \
                 {cfg:?}; the `Buggy` mutant is not caught, so the property is trivial",
                m.name
            ),
            Err((st, inv)) => {
                eprintln!(
                    "{label}: G3 CATCH {} `{inv}` VIOLATED at {st:?} ({cfg:?}).",
                    m.name
                );
            }
        }
    }
    // ---- G4 MULTIPLY ----
    for scenario in scenarios {
        let (cfg, verdict) = gate(&[("Local", 1)], scenario, "G4 MULTIPLY");
        match verdict {
            Ok(n) => panic!(
                "{label}: G4 MULTIPLY FAILED — {} stayed GREEN over {n} states at {cfg:?} with \
                 every enforcer applying the bound LOCALLY. That is the per-instance blindness \
                 this model exists to expose: either the aggregate invariant is not charged \
                 against every instance, or the `Local` dial does not actually make the \
                 enforcers blind to each other. Fix the MODEL — this gate is the machine-\
                 checked form of the sentence a scope-cardinality claim asserts in prose.",
                m.name
            ),
            Err((st, inv)) => {
                let witness: Vec<String> = st
                    .iter()
                    .filter(|(_, v)| **v != 0)
                    .map(|(k, v)| format!("{k}:{v}"))
                    .collect();
                eprintln!(
                    "{label}: G4 MULTIPLY {} `{inv}` VIOLATED at {{{}}} ({cfg:?}) — every \
                     enforcer individually within budget.",
                    m.name,
                    witness.join(", ")
                );
            }
        }
    }
    // ---- G5 ATTRIBUTION ----
    for scenario in scenarios {
        let (cfg, verdict) = gate(
            &[("Local", 1), ("Instances", 1)],
            scenario,
            "G5 ATTRIBUTION",
        );
        match verdict {
            Ok(n) => eprintln!(
                "{label}: G5 ATTRIBUTION {} holds over {n} states at {cfg:?} — the G4 \
                 counterexample is due to the instance COUNT, nothing else.",
                m.name
            ),
            Err((st, inv)) => panic!(
                "{label}: G5 ATTRIBUTION FAILED — {} invariant `{inv}` VIOLATED at {st:?} \
                 ({cfg:?}) with a SINGLE enforcer. The `Local` dial breaks the model even \
                 without multiplication, so G4's counterexample proves nothing about \
                 instance count.",
                m.name
            ),
        }
    }
    if ty.is_some() {
        eprintln!(
            "{label}: {} proved/caught/multiplied on BOTH tiers.",
            m.name
        );
        Covered::InterpreterAndTy
    } else {
        Covered::Interpreter
    }
}

/// The shared precondition of the `_scalar` forms. `caller` names the function
/// the site actually called, so the message says which one to stop using.
// Skip: a build-tooling PRECONDITION — the panic is the deliberate
// "this model can no longer be discharged unconditionally" abort.
#[cfg_attr(trust_verify, trust::skip)]
fn assert_scalar(m: &Model, label: &str, caller: &str) {
    assert!(
        m.fn_vars.is_empty(),
        "{label}: {} is FUNCTION-VALUED, so `{caller}` cannot discharge it — the interpreter \
         cannot evaluate it and the `ty` tier may be absent. Call the `_tiered` form and state a \
         policy for the `NotRun` case.",
        m.name
    );
}

/// The machine-verified NEGATIVE-CONTROL criterion for `--strict-vacuity`'s
/// dead-action class (the audited-exception mechanism of the `spec_xref_closure`
/// gate, upgraded from a hand-maintained model-name list to a proof):
///
/// A derived model may carry actions that are DEAD at its committed config ONLY
/// when every one of them is a genuine prove-and-catch mutant, which this fn
/// verifies with the in-process interpreter:
///
/// 1. the model is scalar (the interpreter can actually verify it — a
///    function-valued model gets NO dead-action relaxation, fail-closed);
/// 2. the model declares the `Buggy` dial, committed to 0 (a dead action in a
///    dial-less model has no catch config — it is a REAL vacuity);
/// 3. `ty_reported_dead` agrees EXACTLY with the interpreter's own dead set at
///    the committed config (tier agreement in BOTH directions: a `ty` verdict
///    the interpreter contradicts fails, and — because the caller derives
///    nothing from `ty`'s output alone — a parse/format drift that under-reports
///    `ty`'s dead set also fails rather than silently passing);
/// 4. the `Buggy = 1` baseline with ALL committed-dead actions removed still
///    satisfies every invariant (otherwise an unrelated Buggy branch could
///    supply the counterexample and make harmless dead actions look caught);
/// 5. each dead action, added back ALONE to that safe baseline, FIRES somewhere
///    in its reachable space (the mutant is independently exercisable — an
///    action dead at BOTH configs is a REAL vacuity, e.g. a typo'd guard);
/// 6. each isolated dead action makes that baseline violate an invariant (every
///    negative control is independently caught, rather than sharing one global
///    counterexample supplied by a different mutant).
///
/// Returns `Ok(n)` — the number of machine-verified negative controls (0 for a
/// strictly non-vacuous model) — or `Err(reason)`; the caller's gate must stay
/// RED on `Err`. Anything this fn cannot verify is a failure, never a waiver.
// Skip: verification-harness driver over the interpreter tier (BTreeSet ops +
// deliberate audit-failure strings). Not shipping runtime code.
#[cfg_attr(trust_verify, trust::skip)]
pub fn audit_dead_negative_controls(m: &Model, ty_reported_dead: &[&str]) -> Result<usize, String> {
    if !m.fn_vars.is_empty() {
        return Err(format!(
            "{}: function-valued model — the interpreter cannot verify negative controls, so \
             no dead-action relaxation is available; keep the model strictly non-vacuous",
            m.name
        ));
    }
    let all: BTreeSet<&str> = m.actions.iter().map(|a| a.name as &str).collect();
    for d in ty_reported_dead {
        if !all.contains(d) {
            return Err(format!(
                "{}: ty reported dead action `{d}` that the model does not declare — \
                 spec/emission drift",
                m.name
            ));
        }
    }
    let fired0 = interp::fired_actions(&interp::with_buggy(m, 0));
    let interp_dead: BTreeSet<&str> = all
        .iter()
        .copied()
        .filter(|a| !fired0.contains(a))
        .collect();
    let ty_dead: BTreeSet<&str> = ty_reported_dead.iter().copied().collect();
    if ty_dead != interp_dead {
        return Err(format!(
            "{}: TIER DISAGREEMENT on the committed-config dead set — ty reports {:?} but the \
             interpreter finds {:?}",
            m.name, ty_dead, interp_dead
        ));
    }
    if interp_dead.is_empty() {
        return Ok(0); // strictly non-vacuous — nothing to audit
    }
    if !m.consts.iter().any(|(n, v)| *n == "Buggy" && *v == 0) {
        return Err(format!(
            "{}: dead action(s) {:?} in a model with no committed `Buggy = 0` dial — there is \
             no catch config under which they could be negative controls; REAL vacuity",
            m.name, interp_dead
        ));
    }
    let buggy = interp::with_buggy(m, 1);
    let mut baseline = buggy.clone();
    baseline
        .actions
        .retain(|action| !interp_dead.contains(action.name));
    if let Err((state, invariant)) = interp::bmc(&baseline) {
        return Err(format!(
            "{}: Buggy=1 baseline with all committed-dead actions removed still violates \
             invariant `{invariant}` at {state:?} — an unrelated Buggy branch supplies the \
             counterexample, so no dead action can be credited as an independently caught \
             negative control",
            m.name
        ));
    }

    for d in &interp_dead {
        let mut isolated = buggy.clone();
        isolated
            .actions
            .retain(|action| !interp_dead.contains(action.name) || action.name == *d);
        if !interp::fired_actions(&isolated).contains(d) {
            return Err(format!(
                "{}: action `{d}` is dead at the committed config AND never fires at Buggy=1 \
                 when added alone to the safe baseline — not an independently exercisable \
                 negative control; REAL vacuity (typo'd guard or mutant dependency?)",
                m.name
            ));
        }
        if interp::bmc(&isolated).is_ok() {
            return Err(format!(
                "{}: action `{d}` fires when added alone to the Buggy=1 baseline, but NO \
                 invariant is violated — this mutant is independently caught by nothing, so \
                 it proves nothing",
                m.name
            ));
        }
    }
    Ok(interp_dead.len())
}

/// TIERED liveness / deadlock-freedom (the `CHECK_DEADLOCK` protocol): the
/// interpreter proves no non-final wedge is reachable at `Buggy=0` and finds
/// the wedge at `Buggy=1` ([`interp::find_deadlock`], panics otherwise), and
/// `ty` additionally does the same via `to_cfg_deadlock_with` wherever
/// installed (asserting the `Buggy=1` failure is a DEADLOCK, not an invariant
/// violation — panics on failure or tier disagreement). `is_final` names the
/// legitimate work-complete terminal states (the interpreter twin of the
/// model's `Done` stutter self-loop).
///
/// Scalar-only (asserted), so like the `_scalar` forms it returns a [`Covered`]
/// outright: the interpreter always runs, and there is no `NotRun` case to
/// decide about.
// Skip: a VERIFICATION-HARNESS driver — it shells out to `ty`, writes
// temp spec/cfg files, and every `expect(..)` is a deliberate test-time
// abort (a missing model checker MUST fail the run loudly). The lossy
// stdout/stderr rendering is display-only. Not shipping runtime code.
#[cfg_attr(trust_verify, trust::skip)]
pub fn deadlock_free_and_catches_tiered(
    m: &Model,
    is_final: impl Fn(&interp::State) -> bool,
    label: &str,
) -> Covered {
    assert_scalar(m, label, "deadlock_free_and_catches_tiered");
    assert!(
        interp::find_deadlock(&interp::with_buggy(m, 0), &is_final).is_none(),
        "{label}: {} (Buggy=0) must be DEADLOCK-FREE (interpreter tier)",
        m.name
    );
    let wedge = interp::find_deadlock(&interp::with_buggy(m, 1), &is_final);
    assert!(
        wedge.is_some(),
        "{label}: {} (Buggy=1) MUST reach a wedge (interpreter tier)",
        m.name
    );
    eprintln!(
        "{label}: {} deadlock-free (Buggy=0) and wedge caught at {:?} (interpreter, Buggy=1).",
        m.name,
        wedge.unwrap()
    );
    match find_ty() {
        Some(ty) => {
            let dir =
                std::env::temp_dir().join(format!("aterm-dl-{}-{}", m.name, std::process::id()));
            std::fs::create_dir_all(&dir).expect("mk tempdir");
            let spec = dir.join(format!("{}.tla", m.name));
            std::fs::write(&spec, m.to_tla()).expect("write spec");
            let run = |cfg_name: &str, cfg: String| -> (bool, String) {
                let cfgp = dir.join(cfg_name);
                std::fs::write(&cfgp, cfg).expect("write cfg");
                // Same arming as `ty_check_derived`, for the same reason: this
                // arm asserts NO DEADLOCK at Buggy=0, and a run stopped early by
                // the memory budget reports exactly that, with exit 0. A
                // deadlock gate that a busy machine can satisfy is not a gate.
                let mut cmd = Command::new(&ty);
                cmd.arg("check").arg(&spec).arg("--config").arg(&cfgp);
                let out = ty_output(arm_whole_space_check(&mut cmd)).expect("run ty check");
                (
                    out.status.success(),
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    ),
                )
            };
            let (ok, out) = run("ok.cfg", m.to_cfg_deadlock_with(&[]));
            assert!(
                ok,
                "{label}: ty says {} (Buggy=0) deadlocks — TIER DISAGREEMENT\n{out}",
                m.name
            );
            let (bug_ok, bug_out) = run("bug.cfg", m.to_cfg_deadlock_with(&[("Buggy", 1)]));
            assert!(
                !bug_ok,
                "{label}: ty found NO wedge at Buggy=1 on {} — TIER DISAGREEMENT\n{bug_out}",
                m.name
            );
            assert!(
                bug_out.contains("Deadlock"),
                "{label}: {} (Buggy=1) failure must be a DEADLOCK, not an invariant \
                 violation\n{bug_out}",
                m.name
            );
            let _ = std::fs::remove_dir_all(&dir);
            eprintln!(
                "{label}: {} additionally deadlock-checked by ty (free at Buggy=0, wedge at Buggy=1).",
                m.name
            );
            Covered::InterpreterAndTy
        }
        None => Covered::Interpreter,
    }
}

/// TIERED per-transition conformance (the Tier-1 `ty trace validate` twin):
/// does the model's `Next` admit the real `prev -> next` step?
///
/// The interpreter tier ALWAYS answers ([`interp::admits`]); the `ty` tier
/// additionally validates a two-step JSON trace against `transition_spec()`
/// wherever installed, and the verdicts MUST agree (disagreement panics).
/// Returns `(conforms, diagnostics)` so callers keep their positive AND
/// negative-control assertions unchanged. `overrides` are the cfg constant
/// overrides the real-code regime needs (e.g. a production `Cap`); `action`
/// optionally names the expected action in the trace (`None` lets any action
/// admit).
// Skip: the tiered validation driver shells out to `ty` and renders JSON
// traces (absent std format/iterator bodies + deliberate harness aborts).
// Verification tooling, not shipping runtime code.
#[cfg_attr(trust_verify, trust::skip)]
pub fn validate_transition_tiered(
    m: &Model,
    overrides: &[(&'static str, i64)],
    prev: &BTreeMap<&'static str, i64>,
    next: &BTreeMap<&'static str, i64>,
    action: Option<&str>,
    label: &str,
) -> (bool, String) {
    // Interpreter verdict: THE named action (when given) — or some action —
    // admits the step. The overrides are applied to the interpreter's constants
    // exactly as the cfg applies them to the `ty` tier, so both tiers evaluate
    // the SAME model instance. The named form asks the action directly rather
    // than trusting `admits`'s first-match order.
    let m_eff = interp::with_consts(m, overrides);
    let admitted = interp::admits(&m_eff, prev, next);
    let interp_ok = match action {
        Some(a) => m_eff.successors(a, prev).contains(next),
        None => admitted.is_some(),
    };
    let interp_why = match admitted {
        Some(a) => format!("interpreter: admitted by action `{a}`"),
        None => "interpreter: NO action admits this transition".to_string(),
    };

    let Some(ty) = find_ty() else {
        return (interp_ok, interp_why);
    };

    // ty tier: two-step trace against the parameterized-Init transition spec.
    // The dir is unique PER CALL (atomic counter), not per (model, pid): two
    // tests in one process validating the same model run concurrently, and a
    // shared dir gets torn spec/cfg writes (a real corrupted-cfg incident).
    static CONF_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-conf-{}-{}-{}",
        m.name,
        std::process::id(),
        CONF_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{}.tla", m.name));
    let cfg = dir.join(format!("{}.cfg", m.name));
    let trace = dir.join("t.json");
    std::fs::write(&spec, m.transition_spec()).expect("write spec");
    std::fs::write(&cfg, m.transition_cfg(prev, overrides)).expect("write cfg");
    std::fs::write(
        &trace,
        transition_trace_json(m, prev, next, action.or(admitted)),
    )
    .expect("write trace");
    // Serialised like every other `ty` spawn (see `ty_output`). `trace validate`
    // walks a two-state trace, not a state space, so it takes none of the
    // whole-space arming — but it is still a `ty` process, and while it lives it
    // is one more divisor in every CONCURRENT checker's memory budget.
    let mut cmd = Command::new(&ty);
    cmd.arg("trace")
        .arg("validate")
        .arg(&trace)
        .arg("--spec")
        .arg(&spec)
        .arg("--config")
        .arg(&cfg);
    let out =
        ty_output(&mut cmd).unwrap_or_else(|e| panic!("run ty trace validate for {label}: {e}"));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    let ty_ok = out.status.success();
    assert!(
        ty_ok == interp_ok,
        "{label}: TIER DISAGREEMENT on {} transition {prev:?} -> {next:?}: \
         interpreter={interp_ok} ty={ty_ok} (checker bug — never swallow this)\n\
         {interp_why}\n--- ty ---\n{combined}",
        m.name
    );
    (ty_ok, combined)
}

/// The two-step `ty trace validate` JSON for a derived model: `prev` at index 0,
/// `next` (+ the admitting action, when known) at index 1. Variables serialize
/// in the model's declared order.
// Skip: the field-render chain drives closure bodies + `Extend`/format
// (absent std bodies). Spec-harness trace emission, not runtime code.
#[cfg_attr(trust_verify, trust::skip)]
fn transition_trace_json(
    m: &Model,
    prev: &BTreeMap<&'static str, i64>,
    next: &BTreeMap<&'static str, i64>,
    action: Option<&str>,
) -> String {
    let vars: Vec<&str> = m.vars.iter().map(|v| v.name).collect();
    let state_json = |s: &BTreeMap<&'static str, i64>| -> String {
        let fields: Vec<String> = vars
            .iter()
            .map(|v| {
                format!(
                    "\"{v}\":{{\"type\":\"int\",\"value\":{}}}",
                    s.get(v).copied().unwrap_or(0)
                )
            })
            .collect();
        format!("{{{}}}", fields.join(","))
    };
    let var_list: Vec<String> = vars.iter().map(|v| format!("\"{v}\"")).collect();
    let action_field = action
        .map(|a| format!(",\"action\":{{\"name\":\"{a}\"}}"))
        .unwrap_or_default();
    format!(
        "{{\"version\":\"1\",\"module\":\"{}\",\"variables\":[{}],\"steps\":[\
         {{\"index\":0,\"state\":{}}},\
         {{\"index\":1,\"state\":{}{action_field}}}\
         ]}}",
        m.name,
        var_list.join(","),
        state_json(prev),
        state_json(next),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive::{config_catalog_snapshot_model, ring_model, transact_model};
    use crate::ty_model;

    /// The atpkg-store prefix mirror must keep matching
    /// `atpkg::platform::default_prefix` + `/bin` (the deliberate
    /// no-dependency duplication both sites cross-reference).
    #[test]
    fn store_bin_dir_mirrors_the_atpkg_default_prefix() {
        assert_eq!(
            unix_store_bin_dir(Path::new("/Users//x")),
            Path::new("/Users//x/Library/Application Support/aterm/pkg/bin")
        );
    }

    /// A dangling store shim never satisfies discovery; a live one resolves to
    /// the real store target (what the caller will execute), not the link.
    #[cfg(unix)]
    #[test]
    fn store_shim_resolution_is_fail_closed() {
        let d = std::env::temp_dir().join(format!("aterm-spec-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let shim = d.join("ty");
        // Absent → None.
        assert_eq!(resolve_store_shim(&shim), None);
        // Dangling symlink → None (a GC'd store build must not satisfy).
        std::os::unix::fs::symlink(d.join("store/gone/ty"), &shim).unwrap();
        assert_eq!(resolve_store_shim(&shim), None);
        // A TOMBSTONE (regular-file refusal script atpkg writes for a yanked
        // build) must read as ABSENT — else a revoked build would shadow a
        // working tool on PATH, since this probe runs first.
        std::fs::remove_file(&shim).unwrap();
        std::fs::write(&shim, b"#!/bin/sh\necho revoked >&2\nexit 1\n").unwrap();
        assert_eq!(
            resolve_store_shim(&shim),
            None,
            "a tombstone shim must never satisfy discovery"
        );
        // Live shim → the RESOLVED store path.
        std::fs::remove_file(&shim).unwrap();
        let target = d.join("real-ty");
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(&target, &shim).unwrap();
        let got = resolve_store_shim(&shim).expect("live shim resolves");
        assert_eq!(got, std::fs::canonicalize(&target).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A SCALAR model can never report [`NotRun`] — that is the fact
    /// [`check_scalar`]/[`prove_and_catch_scalar`] convert into an unconditional
    /// `Covered`, and the reason ~21 call sites are allowed to ignore the
    /// toolchain's presence. Pinned here rather than reasoned about: if the
    /// interpreter tier ever grew a bail-out, this goes red instead of those
    /// sites quietly returning coverage they no longer have.
    #[test]
    fn a_scalar_model_can_never_report_not_run() {
        let m = ring_model();
        assert!(m.fn_vars.is_empty(), "the probe model must be scalar");
        let d = check_model_tiered(&m, "scalar-discharge probe");
        assert!(
            matches!(d, Ok(Covered::Interpreter | Covered::InterpreterAndTy)),
            "{d:?}"
        );
    }

    /// The four committed-dead `ConfigCatalogSnapshot` mutants are verified
    /// negative controls: dial present, fire at Buggy=1, caught at Buggy=1.
    #[test]
    fn audit_accepts_config_catalog_snapshot_mutants() {
        let m = config_catalog_snapshot_model();
        assert_eq!(
            audit_dead_negative_controls(
                &m,
                &[
                    "AdmitStaleTrail",
                    "AdmitStaleNyan",
                    "AdmitStaleTheme",
                    "AdmitStaleSparkle",
                ],
            ),
            Ok(4)
        );
    }

    /// Transact's `BuggyCommit` — the original hand-audited exception — now
    /// passes the machine-verified criterion instead.
    #[test]
    fn audit_accepts_transact_buggy_commit() {
        assert_eq!(
            audit_dead_negative_controls(&transact_model(), &["BuggyCommit"]),
            Ok(1)
        );
    }

    /// A strictly non-vacuous model audits clean with an empty reported set.
    #[test]
    fn audit_accepts_strictly_nonvacuous_model() {
        assert_eq!(audit_dead_negative_controls(&ring_model(), &[]), Ok(0));
    }

    /// Tier agreement is required in BOTH directions: an empty ty report while
    /// the interpreter finds dead actions fails (this is the fail-open guard —
    /// a ty output-format drift cannot silently grant the relaxation), and an
    /// action ty names that the model does not declare fails.
    #[test]
    fn audit_rejects_tier_disagreement_and_unknown_actions() {
        let m = config_catalog_snapshot_model();
        let under_reported = audit_dead_negative_controls(&m, &[]);
        assert!(
            under_reported
                .as_ref()
                .is_err_and(|e| e.contains("TIER DISAGREEMENT")),
            "{under_reported:?}"
        );
        let unknown = audit_dead_negative_controls(&m, &["NoSuchAction"]);
        assert!(
            unknown
                .as_ref()
                .is_err_and(|e| e.contains("does not declare")),
            "{unknown:?}"
        );
    }

    /// A dead action in a model whose `Buggy` is not committed to 0 has no
    /// catch config — REAL vacuity, rejected.
    #[test]
    fn audit_rejects_dead_actions_without_committed_buggy_dial() {
        let m = interp::with_consts(&config_catalog_snapshot_model(), &[("Buggy", 2)]);
        let r = audit_dead_negative_controls(
            &m,
            &[
                "AdmitStaleTrail",
                "AdmitStaleNyan",
                "AdmitStaleTheme",
                "AdmitStaleSparkle",
            ],
        );
        assert!(
            r.as_ref()
                .is_err_and(|e| e.contains("no committed `Buggy = 0` dial")),
            "{r:?}"
        );
    }

    /// An action dead at BOTH configs (a typo'd guard, not a mutant) is a REAL
    /// vacuity, and a Buggy=1 space no invariant catches proves nothing — both
    /// rejected.
    #[test]
    fn audit_rejects_never_firing_and_uncaught_mutants() {
        // `Stuck` cannot fire at any config: its guard needs count > Cap while
        // count never exceeds Cap.
        let never = ty_model! {
            NeverFires {
                const Buggy = 0;
                const Cap = 2;
                var count = 0;
                action Step when (count <= Cap - 1) { count = count + 1; }
                action Stuck when (Buggy == 1 && count == Cap + 1) { count = 0; }
                invariant Bounded: count <= Cap;
            }
        };
        let r = audit_dead_negative_controls(&never, &["Stuck"]);
        assert!(
            r.as_ref()
                .is_err_and(|e| e.contains("never fires at Buggy=1")),
            "{r:?}"
        );

        // `Harmless` fires at Buggy=1 but violates nothing there — the "mutant"
        // is caught by no invariant, so it proves nothing.
        let uncaught = ty_model! {
            UncaughtMutant {
                const Buggy = 0;
                const Cap = 2;
                var count = 0;
                action Step when (count <= Cap - 1) { count = count + 1; }
                action Harmless when (Buggy == 1 && count == 0) { count = 1; }
                invariant Bounded: count <= Cap;
            }
        };
        let r = audit_dead_negative_controls(&uncaught, &["Harmless"]);
        assert!(
            r.as_ref().is_err_and(|e| e.contains("caught by nothing")),
            "{r:?}"
        );
    }

    /// One caught mutant must not launder a second harmless mutant through a
    /// shared Buggy=1 counterexample. Each committed-dead action earns the
    /// strict-vacuity relaxation only when it independently turns the safe
    /// all-live baseline into an invariant violation.
    #[test]
    fn audit_rejects_two_mutants_when_only_one_is_independently_caught() {
        let partly_vacuous = ty_model! {
            PartlyVacuousMutants {
                const Buggy = 0;
                const Cap = 1;
                var count = 0;
                action Step when (count <= Cap - 1) { count = count + 1; }
                action Reset when (count == Cap) { count = 0; }
                action Caught when (Buggy == 1 && count == 0) { count = Cap + 1; }
                action Harmless when (Buggy == 1 && count == 0) { count = Cap; }
                invariant Bounded: count <= Cap;
            }
        };
        let r = audit_dead_negative_controls(&partly_vacuous, &["Caught", "Harmless"]);
        assert!(
            r.as_ref().is_err_and(|e| {
                e.contains("Harmless") && e.contains("independently caught by nothing")
            }),
            "{r:?}"
        );
    }

    /// The state count is read from `ty`'s real statistics block, and ONLY from
    /// it. `None` on drift is the whole point: the caller turns `None` into a
    /// failure, so a `ty` whose output format moves takes the gate RED rather
    /// than silently retiring the explored-space obligation.
    #[test]
    fn ty_state_count_is_parsed_or_refused() {
        let real = "Model checking complete: No errors found (exhaustive).\n\n\
                    Statistics:\n  States found: 128\n  Initial states: 1\n  Transitions: 587\n";
        assert_eq!(ty_states_explored(real), Some(128));
        assert_eq!(ty_states_explored("Statistics:\n  States found: 1\n"), Some(1));
        // Drift / no statistics block / non-numeric — all refusals, not zeros.
        assert_eq!(ty_states_explored("No errors found (exhaustive)."), None);
        assert_eq!(ty_states_explored("  States explored: 128\n"), None);
        assert_eq!(ty_states_explored("  States found: many\n"), None);
    }

    /// REGRESSION (2026-07-30 FALSE PROOF): `ty` reported "No errors found
    /// (exhaustive)" for `NyanJumpBurstLifecycle` after exploring ONE of its 128
    /// reachable states — its partial-order reduction formed a singleton ample
    /// set out of the model's stutter action, whose only successor is the
    /// expanding state, and the C3 cycle proviso failed to reject it. The
    /// verdicts agreed; the work behind them did not.
    ///
    /// This pins the CONSEQUENCE rather than the cause: a smaller `ty` count is
    /// a false proof and must panic. The cause is fixed in `ty` itself, and this
    /// stays green either way — reduction is off for this driver, so a `ty`
    /// whose reduction regresses again cannot reach the gate through this door.
    #[test]
    #[should_panic(expected = "TIER DISAGREEMENT ON THE EXPLORED SPACE")]
    fn a_ty_verdict_from_a_smaller_space_is_not_a_proof() {
        let m = ring_model();
        assert_same_space_explored(
            &m,
            128,
            "--- ty stdout ---\nModel checking complete: No errors found (exhaustive).\n\
             Statistics:\n  States found: 1\n",
            "explored-space regression",
        );
    }

    /// The other direction is a failure too, and for a different reason: a `ty`
    /// count LARGER than the interpreter's means the emitted TLA+ admits states
    /// the model does not, so the two tiers are not checking the same machine.
    /// Neither direction may be waved through as "close enough".
    #[test]
    #[should_panic(expected = "TIER DISAGREEMENT ON THE EXPLORED SPACE")]
    fn a_ty_verdict_from_a_larger_space_is_not_a_proof_either() {
        let m = ring_model();
        assert_same_space_explored(
            &m,
            128,
            "Statistics:\n  States found: 200\n",
            "explored-space regression",
        );
    }

    /// A clean verdict on an unparseable transcript credits nothing.
    #[test]
    #[should_panic(expected = "no evidence it explored anything")]
    fn a_clean_verdict_without_a_state_count_credits_nothing() {
        let m = ring_model();
        assert_same_space_explored(&m, 128, "No errors found.", "explored-space regression");
    }
}

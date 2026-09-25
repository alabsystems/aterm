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
//! ## The checkers are atpkg programs
//!
//! `ty`, `trust-ir` and `ay` arrive with the Trust toolchain's package set —
//! `aterm pkg install --default-set`, or one at a time (`aterm pkg install ty`) —
//! and nothing here is built from source. [`find_ty`]/[`find_trust_ir`]/[`find_ay`]
//! discover each through the atpkg store's shim (`<prefix>/bin/<tool>`), then on
//! `PATH`. That is the whole order: the `$HOME/trust/first-party` cargo builds, the
//! `$HOME/trust/build` bootstrap stages and the standalone `~/ay` checkout were probed
//! FIRST until 2026-09-24, a month after they were retired from the delivery
//! (2026-08-29) — and on m3 a `$HOME/trust/first-party/trust-ir` 0.2.0 from 2026-08-17
//! was shadowing the store's 0.14.0 on every spec-link run. A developer who wants
//! their own build points the shim at it: `aterm pkg link <tool> <checkout>`. The
//! home directory (`$HOME`, or `%USERPROFILE%` on Windows), `%LOCALAPPDATA%`
//! (Windows) and `PATH` are the only environment access.
//!
//! ## The tier names the binary it ran
//!
//! Which binary answered used to be invisible: a passing check named no binary,
//! and the path reached output only inside a failure panic. The first discovery
//! of each tool in a process now writes one line straight to stderr (not through
//! `eprintln!`, which libtest swallows for a passing test):
//!
//! ```text
//! VERIFY ESCALATION TIER: ty = ~/Library/Application Support/aterm/pkg/store/ty/3007/bin/ty [atpkg managed store; ty 0.15.0; built 2026-09-24] — the managed store also holds store/trust/9192/bin/ty (built 2026-09-19)
//! ```
//!
//! (A `built` date is the file's mtime in UTC — a reinstall moves it — because
//! `--version` does not change between builds of one package version.)
//!
//! It costs one `--version` spawn per tool per process (bounded at five
//! seconds), a few `stat`s and a 256-byte head read per candidate; store
//! copies are described from the filesystem alone, never by running them. [`locate_ty`]/[`locate_ay`]/[`locate_trust_ir`] return the same
//! answer as a [`Located`] for a caller that wants the tier programmatically.
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

/// Discover the Trust `ty` model-checker: the atpkg-managed store's shim, then
/// `ty` on `PATH` (see [`locate_ty`] for which tier answered). The home directory,
/// `PATH` and (Windows) `%LOCALAPPDATA%` are the only environment access.
///
/// The first discovery of each tool in a process prints ONE
/// `VERIFY ESCALATION TIER: ty = …` line straight to stderr naming the binary,
/// the tier it came from, its `--version` and build date, and every managed
/// store copy it shadowed (the module doc, "The tier names the binary it ran").
#[must_use]
pub fn find_ty() -> Option<PathBuf> {
    locate_ty().map(|l| l.path)
}

/// Discover the Trust `trust-ir` `spec-link` cross-referencer. Mirrors [`find_ty`].
#[must_use]
pub fn find_trust_ir() -> Option<PathBuf> {
    locate_trust_ir().map(|l| l.path)
}

/// Discover the Trust `ay` SAT/SMT/CHC solver. Mirrors [`find_ty`] exactly (store
/// shim → PATH). Used by the always-on
/// `sparkle_v2_ay_certificates` gate: hand-encoded SMT-LIB2 certificates are
/// re-checked fail-closed, the same honesty ratchet as the `ty` Tier-0 gates.
#[must_use]
pub fn find_ay() -> Option<PathBuf> {
    locate_ay().map(|l| l.path)
}

/// [`find_ty`], plus WHERE the binary came from.
#[must_use]
pub fn locate_ty() -> Option<Located> {
    discover("ty")
}

/// [`find_trust_ir`], plus where the binary came from.
#[must_use]
pub fn locate_trust_ir() -> Option<Located> {
    discover("trust-ir")
}

/// [`find_ay`], plus where the binary came from.
#[must_use]
pub fn locate_ay() -> Option<Located> {
    discover("ay")
}

/// The discovery tier that answered, in precedence order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrustBinOrigin {
    /// The atpkg-managed store, through its `<prefix>/bin/<tool>` shim.
    Store,
    /// `<tool>` on `PATH`.
    Path,
}

impl TrustBinOrigin {
    /// The tier as a developer reads it in the provenance line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Store => "atpkg managed store",
            Self::Path => "PATH",
        }
    }
}

/// A discovered Trust binary and the tier that answered.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Located {
    /// What the caller will execute.
    pub path: PathBuf,
    /// Which discovery tier produced it.
    pub origin: TrustBinOrigin,
}

/// The real-environment discovery + the once-per-process provenance report.
// Skip: environment + filesystem discovery and a stderr report.
// Build-tooling discovery; every miss returns None (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
fn discover(bin: &'static str) -> Option<Located> {
    let homes = home_dirs();
    let store_bin = atpkg_store_bin_dir();
    let path_var = std::env::var_os("PATH");
    let found = find_trust_bin(bin, store_bin.as_deref(), path_var.as_deref());
    report_provenance_once(bin, found.as_ref(), &homes, store_bin.as_deref());
    found
}

/// Shared discovery — THE precedence: the atpkg store shim, then `<bin>` on `PATH`,
/// with the platform executable name (`ty` vs `ty.exe`). The environment (the
/// store's `bin/` dir, the `PATH` value) is passed in EXPLICITLY so the order is
/// testable without mutating the process environment.
///
/// The store comes first because atpkg's `bin/` reaches PATH only in INTERACTIVE
/// shells (aterm's integration, or the rc block atpkg appends), never in a `cargo
/// test` process. A shim is trusted only when it resolves to a real file (a
/// dangling link after a GC must not satisfy discovery).
// Skip: build-tooling discovery; every miss returns None (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
fn find_trust_bin(
    bin: &str,
    store_bin_dir: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<Located> {
    let exe = exe_name(bin);
    if let Some(p) = store_bin_dir.and_then(|d| resolve_store_shim(&d.join(&exe))) {
        return Some(Located {
            path: p,
            origin: TrustBinOrigin::Store,
        });
    }
    path_search_in(&exe, path_var).map(|path| Located {
        path,
        origin: TrustBinOrigin::Path,
    })
}

/// The atpkg store's `bin/` directory. The prefix mirrors
/// `atpkg::platform::default_prefix` (`~/Library/Application Support/aterm/pkg`
/// on macOS, `%LOCALAPPDATA%\aterm\pkg` on Windows) — kept as a PATH MIRROR, not
/// a dependency edge, so the spec crate never drags the package manager (ring,
/// tar, zstd) into every conformance consumer; if atpkg ever moves its prefix,
/// update both sites (each carries this cross-reference).
// Skip: environment reads; every miss returns None (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
fn atpkg_store_bin_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        let local = std::env::var("LOCALAPPDATA")
            .ok()
            .filter(|d| !d.is_empty())?;
        Some(PathBuf::from(local).join("aterm").join("pkg").join("bin"))
    } else {
        Some(unix_store_bin_dir(&home_dirs().into_iter().next()?))
    }
}

/// The Unix half of the prefix mirror (see [`atpkg_store_bin_dir`]):
/// `<home>/Library/Application Support/aterm/pkg/bin` on macOS,
/// `<home>/.local/share/aterm/pkg/bin` elsewhere.
fn unix_store_bin_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
            .join("bin")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".local")
            .join("share")
            .join("aterm")
            .join("pkg")
            .join("bin")
    }
}

/// A shim is trusted only when it FORWARDS to a real file: a symlink an older
/// atpkg laid, or the `#!/bin/sh` exec stub atpkg lays today.
///
/// Today's shape is the stub (`atpkg::platform::unix` writes one on purpose:
/// `targo` refuses a symlinked `current_exe`, so a shim `exec`s the store file
/// at its real path). Its target is the first trimmed line that reads
/// `exec '<path>' "$@"` — the same rule as atpkg's own
/// `platform::parse_sh_shim_target`, which every store answer (`which`, gc,
/// `prune_stale_shims`) is built on, including a routed shim whose guard line
/// starts with `[` and so never matches. Until 2026-09-23 this probe accepted
/// ONLY a symlink, so it answered `None` for every shim atpkg had laid since
/// the stubs replaced the links (measured on m3: 0 of 62 `bin/` entries are
/// symlinks) and the store tier never answered at all.
///
/// A TOMBSTONE must still read as ABSENT: atpkg disables a yanked or
/// below-floor build by replacing the shim with a failing notice script
/// (`atpkg::activate::install_tombstone_shim`) that carries no `exec` line, and
/// the pending-program stub `exec`s `"$ATPKG"`, never a quoted literal path.
/// Neither parses, so neither can satisfy discovery — and because this probe
/// runs before the PATH search, a revoked build would otherwise SHADOW a
/// working tool the developer already has on PATH (adversarial review
/// 2026-07-30). Anything that is not a symlink or a readable regular file of
/// at most 64 KiB (atpkg's own bound) — a directory, a FIFO — is absent too.
///
/// The answer is the RESOLVED target — what the shim would exec — never the
/// shim. The target must be an absolute path to a regular file (a GC'd store
/// build must not satisfy). It is deliberately NOT anchored to `<prefix>/store`:
/// a `aterm pkg dev link` points a shim at a checkout, the symlink branch always
/// accepted such a link, and the tool a developer linked is the one they meant.
///
/// Public for ONE consumer: atpkg's test
/// `aterm_spec_discovery_reads_every_shim_atpkg_lays` lays each shim shape through
/// atpkg's real writers and asks this function to read it — the pin that turns a
/// future change to atpkg's stub into a red test instead of a silently dead tier.
// Skip: fs syscall wrappers (symlink_metadata/canonicalize/read — absent std
// bodies); every miss returns None (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn resolve_store_shim(shim: &Path) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(shim).ok()?;
    let target = if meta.file_type().is_symlink() {
        shim.to_path_buf()
    } else if meta.file_type().is_file() && meta.len() <= MAX_STORE_SHIM_BYTES {
        let content = std::fs::read_to_string(shim).ok()?;
        let target = parse_exec_stub_target(&content)?;
        if !target.is_absolute() {
            return None;
        }
        target
    } else {
        return None;
    };
    let resolved = std::fs::canonicalize(target).ok()?;
    resolved.is_file().then_some(resolved)
}

/// The size bound on a shim this probe will read — atpkg's own `MAX_SHIM_BYTES`.
const MAX_STORE_SHIM_BYTES: u64 = 64 * 1024;

/// The target an atpkg `sh` exec stub forwards to: the first trimmed line of the
/// form `exec '<path>' "$@"`, with `'\''` unquoted — a mirror of
/// `atpkg::platform::parse_sh_shim_target` (kept as a mirror, not a dependency
/// edge, for the same reason as [`atpkg_store_bin_dir`]'s prefix; if atpkg ever
/// changes its stub, update both sites). `None` for a tombstone or a pending stub.
fn parse_exec_stub_target(content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("exec '")
            && let Some(end) = rest.rfind("' \"$@\"")
        {
            return Some(PathBuf::from(rest[..end].replace("'\\''", "'")));
        }
    }
    None
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

/// On Windows, rewrite a POSIX-style `/c/Users/…` home to `C:/Users/…`; otherwise
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

/// `PATH` lookup via `std::env::split_paths` — portable, no shell dependency.
/// `path_var` is the value of `PATH` (passed in, so discovery is testable).
// Skip: `Path::is_file` is an fs syscall wrapper (absent body); every
// miss returns None (fail-closed). Build-tooling discovery.
#[cfg_attr(trust_verify, trust::skip)]
fn path_search_in(exe: &str, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_var?)
        .map(|dir| dir.join(exe))
        .find(|cand| cand.is_file() && !is_pending_stub(cand))
}

// ---------------------------------------------------------------------------
// PROVENANCE: the escalation tier names the binary it ran. Measured 2026-09-23
// on m3, before discovery lost its developer-tree tiers: every derived-model
// check ran a bootstrap `$HOME/trust/build/host/stage1/bin/ty` and every spec-link
// a first-party trust-ir 0.2.0 while the managed store held 0.14.0 — a stale
// override nothing reported. The first discovery of each tool in a process
// prints one line naming the binary, its tier, its `--version`, its build date,
// and every other managed-store copy.
// ---------------------------------------------------------------------------

/// A copy of a tool in the atpkg store: `<prefix>/store/<program>/current/bin/<exe>`.
#[derive(Clone, PartialEq, Eq, Debug)]
struct StoreCopy {
    /// The store program that carries it (`ty`, or `trust` for the toolchain bundle).
    program: String,
    /// The build `current` points at (the store directory name, e.g. `3007`).
    build: String,
    /// The resolved file.
    path: PathBuf,
    /// Its mtime as a UTC date, when readable.
    built: Option<String>,
}

impl StoreCopy {
    fn describe(&self) -> String {
        let exe = self
            .path
            .file_name()
            .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
        format!(
            "store/{}/{}/bin/{exe} (built {})",
            self.program,
            self.build,
            self.built.as_deref().unwrap_or("date unknown")
        )
    }
}

/// Every live store copy of `exe`: for each program in `store_root`, the file
/// `current/bin/<exe>` resolves to, if it is a real file and not a pending
/// stub. No process is spawned for a copy — a `stat`, and the bounded head read
/// [`is_pending_stub`] makes (the one `--version` spent per tool goes to the
/// binary discovery actually chose). Sorted by program name so the report is
/// deterministic.
// Skip: read_dir / canonicalize — fs iteration with absent std bodies.
#[cfg_attr(trust_verify, trust::skip)]
fn store_copies_in(store_root: &Path, exe: &str) -> Vec<StoreCopy> {
    let Ok(entries) = std::fs::read_dir(store_root) else {
        return Vec::new();
    };
    let mut copies: Vec<StoreCopy> = entries
        .flatten()
        .filter_map(|e| {
            let build_dir = std::fs::canonicalize(e.path().join("current")).ok()?;
            let path = build_dir.join("bin").join(exe);
            if !path.is_file() || is_pending_stub(&path) {
                return None;
            }
            Some(StoreCopy {
                program: e.file_name().to_string_lossy().into_owned(),
                build: build_dir.file_name()?.to_string_lossy().into_owned(),
                built: build_date(&path),
                path,
            })
        })
        .collect();
    copies.sort_by(|a, b| a.program.cmp(&b.program));
    copies
}

/// The target `p` forwards to when it is an atpkg exec stub (read through
/// [`parse_exec_stub_target`], the same rule the store probe uses), else
/// `None`. Bounded: a file over atpkg's shim size is not read, so a
/// multi-megabyte binary is never slurped to learn it is not a shim.
// Skip: bounded file read. Build-tooling discovery.
#[cfg_attr(trust_verify, trust::skip)]
fn exec_stub_target(p: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(p).ok()?;
    if !meta.is_file() || meta.len() > MAX_STORE_SHIM_BYTES {
        return None;
    }
    parse_exec_stub_target(&std::fs::read_to_string(p).ok()?)
}

/// A file's mtime as a UTC `YYYY-MM-DD` — the build date a developer can act
/// on (`--version` does not move between builds of one package version).
#[cfg_attr(trust_verify, trust::skip)]
fn build_date(p: &Path) -> Option<String> {
    let secs = std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let (y, m, d) = civil_from_unix_days(i64::try_from(secs / 86_400).ok()?);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Days since 1970-01-01 → proleptic Gregorian (year, month, day), UTC.
/// Howard Hinnant's `civil_from_days`, so the report needs no date crate.
fn civil_from_unix_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    // Both are range-bounded by construction (1..=31, 1..=12).
    (
        y,
        u32::try_from(m).unwrap_or(0),
        u32::try_from(d).unwrap_or(0),
    )
}

/// The first line `<bin> --version` prints, or `None` if it fails, prints
/// nothing, or does not answer within five seconds (it is then killed — a
/// wedged probe must never wedge the suite). Called at most ONCE per tool per
/// process, from [`report_provenance_once`].
// Skip: spawns a subprocess and polls it. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn probe_version(bin: &Path) -> Option<String> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let first = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(160).collect::<String>())
    };
    first(&out.stdout).or_else(|| first(&out.stderr))
}

/// `p` with the home directory written as `~`, for a line a human reads.
fn tilde(p: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

/// What the store's own `<prefix>/bin/<exe>` shim is — the reason a store copy
/// can be present yet unreached.
#[derive(Clone, PartialEq, Eq, Debug)]
enum StoreShim {
    /// No file by that name in the store's `bin/`.
    Absent,
    /// A regular file with no `exec '<path>'` line — a tombstone or a pending
    /// stub, which [`resolve_store_shim`] reads as absent on purpose.
    Refusal,
    /// A symlink or exec stub whose target is not an absolute path to a real
    /// file (a reclaimed build) — or, if discovery took it, a live shim.
    Forwarding,
}

#[cfg_attr(trust_verify, trust::skip)]
fn classify_store_shim(shim: &Path) -> StoreShim {
    match std::fs::symlink_metadata(shim) {
        Err(_) => StoreShim::Absent,
        Ok(m) if m.file_type().is_symlink() => StoreShim::Forwarding,
        Ok(_) if exec_stub_target(shim).is_some() => StoreShim::Forwarding,
        Ok(_) => StoreShim::Refusal,
    }
}

/// The one-line provenance report — PURE, so the wording is testable without a
/// spawn. `found` is what discovery chose (`None` = nothing on any tier);
/// `version`/`built` describe it; `shadowed` are the store copies OTHER than
/// the chosen binary; `shim` explains a miss.
fn render_provenance(
    bin: &str,
    found: Option<&Located>,
    version: Option<&str>,
    built: Option<&str>,
    shadowed: &[StoreCopy],
    shim: &StoreShim,
    home: Option<&Path>,
) -> String {
    let copies = shadowed
        .iter()
        .map(StoreCopy::describe)
        .collect::<Vec<_>>()
        .join(", ");
    let Some(found) = found else {
        let mut line = format!(
            "VERIFY ESCALATION TIER: {bin} = NOT FOUND (probed the atpkg store shim, PATH)"
        );
        if !shadowed.is_empty() {
            let why = match shim {
                StoreShim::Absent => {
                    format!("no `{bin}` shim in the store's bin/ exposes it")
                }
                StoreShim::Refusal => format!(
                    "the store's `{bin}` shim is a refusal script (tombstone or pending stub)"
                ),
                StoreShim::Forwarding => format!(
                    "the store's `{bin}` shim does not resolve to a file (a reclaimed build?)"
                ),
            };
            line.push_str(&format!(
                " — the managed store holds {copies} but discovery does not reach it: {why}"
            ));
        }
        return line;
    };
    let shim_note = if found.origin == TrustBinOrigin::Path {
        exec_stub_target(&found.path)
            .map(|t| format!(" (atpkg shim → {})", tilde(&t, home)))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let mut line = format!(
        "VERIFY ESCALATION TIER: {bin} = {} [{}{shim_note}; {}; built {}]",
        tilde(&found.path, home),
        found.origin.label(),
        version.unwrap_or("version unknown"),
        built.unwrap_or("date unknown"),
    );
    if !shadowed.is_empty() {
        line.push_str(&format!(" — the managed store also holds {copies}"));
    }
    line
}

/// Everything the provenance line needs except the one spawn (`version`),
/// over an explicit environment: the store copies other than the chosen
/// binary, what the store's own shim is, the chosen binary's build date.
// Skip: fs probes + formatting. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn provenance_line(
    bin: &str,
    found: Option<&Located>,
    version: Option<&str>,
    homes: &[PathBuf],
    store_bin: Option<&Path>,
) -> String {
    let exe = exe_name(bin);
    let copies = store_bin
        .and_then(Path::parent)
        .map(|prefix| store_copies_in(&prefix.join("store"), &exe))
        .unwrap_or_default();
    // The chosen binary's real file (through an exec-stub shim when discovery
    // took one off PATH), so the store copy it IS is not listed as shadowed.
    let chosen = found.and_then(|f| {
        let real = exec_stub_target(&f.path).unwrap_or_else(|| f.path.clone());
        std::fs::canonicalize(real).ok()
    });
    let shadowed: Vec<StoreCopy> = copies
        .into_iter()
        .filter(|c| std::fs::canonicalize(&c.path).ok() != chosen)
        .collect();
    let shim = store_bin.map_or(StoreShim::Absent, |d| classify_store_shim(&d.join(&exe)));
    let built = found.and_then(|f| build_date(&f.path));
    render_provenance(
        bin,
        found,
        version,
        built.as_deref(),
        &shadowed,
        &shim,
        homes.first().map(PathBuf::as_path),
    )
}

/// First-report-per-tool bookkeeping: `true` exactly once per `bin` per `set`.
fn claim_first_report(set: &Mutex<BTreeSet<&'static str>>, bin: &'static str) -> bool {
    set.lock().unwrap_or_else(|e| e.into_inner()).insert(bin)
}

/// Print the provenance line for `bin`, ONCE per process (the first discovery
/// wins; the ~hundreds of later `find_ty` calls in a suite stay silent and
/// spawn nothing).
///
/// Written straight to the process's stderr handle rather than via
/// `eprintln!`: libtest captures the print macros for a PASSING test and
/// throws the text away, which is exactly why the success lines never told
/// anyone which `ty` ran. A direct handle write is not captured, so the line
/// reaches the developer's terminal and the gate's stage log on a green run.
// Skip: fs probes, one subprocess, a stderr write. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn report_provenance_once(
    bin: &'static str,
    found: Option<&Located>,
    homes: &[PathBuf],
    store_bin: Option<&Path>,
) {
    use std::io::Write;
    static REPORTED: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
    if !claim_first_report(&REPORTED, bin) {
        return;
    }
    let version = found.and_then(|f| probe_version(&f.path));
    let line = provenance_line(bin, found, version.as_deref(), homes, store_bin);
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// For a NOT RUN notice: the managed-store copies of `bin` that exist but that
/// discovery did not reach, as a sentence — empty when there are none.
#[cfg_attr(trust_verify, trust::skip)]
fn unreached_store_note(bin: &str) -> String {
    let copies = atpkg_store_bin_dir()
        .as_deref()
        .and_then(Path::parent)
        .map(|prefix| store_copies_in(&prefix.join("store"), &exe_name(bin)))
        .unwrap_or_default();
    if copies.is_empty() {
        return String::new();
    }
    let list = copies
        .iter()
        .map(StoreCopy::describe)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        " The managed store holds {list}, but no discovery tier reaches it (see the \
         `VERIFY ESCALATION TIER: {bin} = NOT FOUND` line)."
    )
}

/// Is this candidate atpkg's PENDING-PROGRAM stub rather than the tool?
///
/// A program the registry lists but has not installed yet still gets a file in
/// the store's `bin/`: a tiny script that prints "not installed yet" and exits
/// nonzero, so an interactive shell explains itself instead of saying "command
/// not found". Discovery must not mistake that placeholder for the tool — a
/// harness that runs it reads the refusal as the tool's OUTPUT and fails hard
/// (measured: the spec-xref gate hard-failed on this box the moment a pending
/// `trust-ir` stub reached PATH, where before it had cleanly skipped). The stub
/// names itself on its second line; anything unreadable is treated as a real
/// binary, because a false "pending" would hide an installed tool.
///
/// Only the HEAD is read. The marker sits on the stub's second line, and the
/// real tool is big (ty is ~157 MB in the store on m3, trust's bundled ty ~165
/// MB), so reading the whole file — which this did until 2026-09-24 — cost a
/// full read of every candidate and, with the store copies now described too,
/// hundreds of megabytes per tool per test process.
// Skip: bounded file read. Build-tooling discovery.
#[cfg_attr(trust_verify, trust::skip)]
fn is_pending_stub(cand: &Path) -> bool {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(cand) else {
        return false;
    };
    let mut head = Vec::new();
    if file.take(PENDING_STUB_HEAD).read_to_end(&mut head).is_err() {
        return false;
    }
    head.windows(PENDING_STUB_MARKER.len())
        .any(|w| w == PENDING_STUB_MARKER)
}

/// How much of a candidate [`is_pending_stub`] reads: the stub's marker is on
/// its second line, well inside this.
const PENDING_STUB_HEAD: u64 = 256;

/// The self-identifying line atpkg writes into every pending-program stub.
const PENDING_STUB_MARKER: &[u8] = b"atpkg pending-program stub";

/// Locate the Trust `ty` model-checker for `label`, or PANIC with the install hint.
/// Verification is ALWAYS required — no env var, no skip. A conformance test that
/// cannot reach `ty` FAILS rather than reporting a false `ok`.
#[must_use]
pub fn ty(label: &str) -> PathBuf {
    require(
        "ty",
        "aterm pkg install ty   (or `aterm pkg install --default-set`)",
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
        "aterm pkg install trust-ir   (or `aterm pkg install --default-set`)",
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
        "aterm pkg install ay   (or `aterm pkg install --default-set`)",
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
             model-checked / spec-linked. Install it once: {build_hint}. \
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
        // "Not installed" is only true when the store holds no copy either; a
        // copy discovery cannot reach is named, never reported as absent.
        let store_note = unreached_store_note(bin);
        let state = if store_note.is_empty() {
            "is not installed"
        } else {
            "was not found by discovery"
        };
        eprintln!(
            "VERIFY ESCALATION TIER NOT RUN: Trust `{bin}` {state}, so `{label}` \
             (an external-tool-analysis obligation) did not run on this machine. The \
             applicable in-process derived-model checks still ran. Enable the \
             escalation tier once: {build_hint}.{store_note}"
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
        "aterm pkg install ty   (or `aterm pkg install --default-set`)",
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
        "aterm pkg install trust-ir   (or `aterm pkg install --default-set`)",
        find_trust_ir(),
        label,
    )
}

/// The `ay` (SMT/CHC certificate) escalation tier. See [`ty_escalation`].
#[must_use]
pub fn ay_escalation(label: &str) -> Option<PathBuf> {
    escalation(
        "ay",
        "aterm pkg install ay   (or `aterm pkg install --default-set`)",
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
/// `RainbowJumpBurstLifecycle`'s 128 with reduction on and reported "No errors
/// found (exhaustive)" — a false proof at the committed config, and a missing
/// counterexample at `Buggy = 1`, which is the only reason it surfaced at all.
/// The model's stutter action (`SlowJump`, a self-loop that writes nothing)
/// formed a singleton ample set whose only successor is the expanding state, and
/// that build predated `ty`'s own C3 cycle proviso by four days. Two other
/// models (`NativeConfigTransaction` 1662/1712, `CursorCatCurseWince` 30/36)
/// were under-exploring the same way, silently, because a prove-only model has
/// no catch half to fail.
///
/// **That was written as past tense and it should not have been.** Re-measured
/// 2026-08-06 against the binary [`find_trust_bin`] actually selects on this
/// machine — `$HOME/trust/first-party/ty/target/release/ty`, `tla 0.10.0` — all
/// three are STILL under-explored by exactly the old amounts
/// (`RainbowJumpBurstLifecycle` 1/128, `NativeConfigTransaction` 1662/1712,
/// `CursorCatCurseWince` 30/36), and it is worse than a coverage gap: at
/// `Buggy = 1`, where `NoLostFadePayload` is violated three steps from `Init`,
/// that build prints `No errors found (exhaustive)` and exits 0. A FALSE PROOF,
/// under a `Soundness mode: Sound` banner. The trigger is the same singleton
/// ample set — drop the stutter action from `Next` and the full 128 states
/// appear. A 2026-07-20 bootstrap stage build on the same disk is correct
/// (`POR: 0/127 states reduced`), but `find_trust_bin` probed the first-party
/// path first and therefore always took the broken one (it probes neither
/// since 2026-09-24).
///
/// So this is not a historical note and not a belt-and-braces flag. It is the
/// only thing standing between this workspace and a checker that answers
/// "proved" about spaces it never entered — which is exactly why every `ty
/// check` in the workspace arms through [`arm_whole_space_check`], and why
/// nothing here may assume the binary is new.
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
/// **`--backend interpreter`** — the same "these models are small" argument, one
/// layer down. `ty`'s trust-codegen backend became the default engine under AUTO
/// selection, and it compiles every action to native code before exploring: on
/// `NativeTabIdentity` that is 9 actions and 17 invariants compiled to walk 399
/// states, 18.5s wall / 52s CPU against 0.58s interpreted, for the identical
/// verdict and the identical count. Native codegen pays for itself somewhere
/// north of a million states; none of these models are within three orders of
/// magnitude of that.
///
/// It buys less on the suite than that ratio suggests — 266s to 228s — and the
/// reason is worth writing down so nobody re-measures it hoping for more. With
/// the spawns serialised, suite wall-clock is ~390 runs times the PER-PROCESS
/// cost, and what dominated that was a fixed ~0.55s floor no backend choice can
/// touch. **That floor was mis-attributed here for a month** — this doc used to
/// blame "`ty`'s own startup: ~0.5s before it reads the spec" and concluded
/// "only spawning `ty` fewer times could" fix it. Both halves are false, and
/// measurement says so: `ty --version` returns in under 10ms, and the SAME spec
/// with `--bfs-only` finishes in 0.03s. The 0.55s was the fused CDEMC symbolic
/// lane, and it is now switched off below.
///
/// **`--bfs-only`** — run the pure explicit-state BFS lane, not `ty`'s fused
/// BFS+symbolic (CDEMC) default.
///
/// The fused default races explicit BFS against PDR / BMC / k-induction and
/// takes the first definitive answer. For the models in this workspace the
/// symbolic half has never once supplied that answer: over **639 real
/// comparisons** — all 125 `xref::model_registry()` models at `Buggy = 0` and
/// `Buggy = 1`, the same 250 again with `CHECK_DEADLOCK TRUE`, the 125-model
/// `--strict-vacuity` sweep the `aterm-gui` gate runs, and the 14 hand-written
/// `aterm-spec-models` specs — every fused run reported `Winner: BFS
/// (explicit-state)`, and the symbolic lanes reported `[ay-kind] k-induction
/// inconclusive` and `MODEL-UNCONFIRMED … rejected by mandatory strict
/// certification`. 0.55s per spawn to conclude nothing, ~400 times per
/// `tools/verify.sh` run.
///
/// It is not weaker, and that is the only question that mattered. Across those
/// 639 comparisons the two lanes agree byte-for-byte on every fact this
/// workspace reads out of a transcript: exit status, `States found:` (the input
/// to [`assert_same_space_explored`]), `Soundness mode: Sound`, `Search
/// completeness: exhaustive`, the `Deadlock reached` wording
/// `deadlock_free_and_catches_tiered` matches, the `dead action(s) (never
/// fired):` set `audit_dead_negative_controls` parses, and `is violated`. ZERO
/// differences. The one text that does change is the clean-run wording, and it
/// changes toward correctness: fused prints "No error has been found", BFS
/// prints "No errors found (exhaustive)" — the string
/// `examples/trust_models.rs` has always tested for and, under the fused
/// default, never saw.
///
/// The completeness argument is structural, not empirical. Every `.cfg` this
/// workspace hands `ty` is INVARIANT-only over a finite bounded machine
/// (`Model::to_cfg` emits CONSTANT / SPECIFICATION / INVARIANT /
/// CHECK_DEADLOCK and nothing else; `Model::to_tla` emits `Spec == Init /\
/// [][Next]_vars` with no fairness conjunct; the 14 hand-written specs match).
/// An exhaustive BFS of a finite reachable space IS the complete proof of a
/// safety invariant — there is no obligation left for a symbolic lane to
/// discharge. And a symbolic-only verdict could not be credited here anyway:
/// [`assert_same_space_explored`] demands a `States found:` count equal to the
/// interpreter's walk, which only the BFS lane produces.
///
/// The flag is safe for a cfg that grows a temporal PROPERTY, which was checked
/// rather than assumed: on a `WF_vars`-fair spec with a violated `<>(done)`,
/// both lanes print the identical four-state counterexample and exit 1.
///
/// **`--force`** — bypass `ty`'s local check cache. Load-bearing, and NOT a
/// speed flag: it is a fail-closed re-derivation flag that closes a hole that
/// exists today, before this patch. The cache is PATH-keyed (verified: identical
/// spec bytes in a fresh directory always miss), so the drivers that write a
/// per-PID temp dir never hit it — but `aterm-spec-models`' `model_check.rs`
/// checks the CHECKED-IN `specs/*.tla` at a stable repo path, and on a repeat
/// run `ty` replays `Cache hit: PASS` in 7ms, complete with a
/// `States found: 512` line that every parser in this file will take for fresh
/// evidence. That is a cached security verdict for the ISOLATION family
/// (Sandbox, PathConfine, ForkExec …) standing in for an exhaustive walk. It is
/// the same shape as the false proof this whole arming exists to prevent, so it
/// is refused the same way. Measured cost of refusing it: the ISOLATION specs go
/// from a 7ms replay to a 20ms genuine re-derivation.
///
/// This is a SPEED choice, not a trust one, and it is worth being explicit about
/// which: the native and interpreted engines are two implementations inside the
/// same checker, not two independent oracles, so picking one buys no confidence
/// the other would have. The independent check is the interpreter tier in this
/// crate, and the thing that makes it independent is
/// [`assert_same_space_explored`] — which holds either way.
///
/// The ceiling is deliberately NOT pinned with `--memory-limit`. Pinning it was
/// tried and made things worse under exactly the load it was meant to fix:
/// `ty`'s limit probe is not purely per-process, so a small explicit ceiling
/// trips on a busy machine even when this run's own RSS is 17 MB — the same
/// false stop, now pinned on. Shrinking the footprint is the fix; the ceiling
/// was only ever a symptom of it.
/// PUBLIC because there is more than one `ty` driver in this workspace, and the
/// only way two drivers cannot disagree about what "armed" means is for there to
/// be one place that says it.
///
/// That is not a style preference; it is the fix for a real escape. The
/// `spec_xref_closure` gate in `aterm-gui` built its own `ty` command and simply
/// did not carry these flags, so it ran the whole registry with partial-order
/// reduction ON while every doc here described reduction as off. On
/// `RainbowJumpBurstLifecycle` that reduced 128 reachable states to **1**
/// (`POR: 1/1 states reduced (100.0%)`), which meant four of its six actions
/// never fired and `--strict-vacuity` reported them dead — a dead set that is an
/// artifact of the reduction, not a property of the model. Anything reaching for
/// `ty` on a derived model must arm it through here.
// Skip: argument plumbing for a subprocess. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn arm_whole_space_check(cmd: &mut Command) -> &mut Command {
    cmd.arg("--no-auto-por")
        .arg("--no-auto-symmetry")
        .arg("--initial-capacity")
        .arg("8192")
        .arg("--backend")
        .arg("interpreter")
        .arg("--bfs-only")
        .arg("--force")
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
/// The identity of the checker a transcript came from — path AND build stamp —
/// so a gate panic names the binary an operator has to act on, not merely its
/// output.
///
/// Diagnosing the 2026-08-06 red gate took four parallel investigations, and a
/// good part of each was rediscovering, by hand, WHICH `ty` had produced the
/// transcript in the panic. Every driver printed stdout+stderr and nothing about
/// the process that wrote them. `--version` does not distinguish these builds
/// (the broken one says `tla 0.10.0`, the correct one `ty 0.10.0`, both "0.10.0");
/// the mtime does.
///
/// Safe to prepend to a transcript: [`ty_states_explored`] keys on a
/// `States found:` line and the dead-action parsers on
/// `dead action(s) (never fired): `. This line matches neither.
#[must_use]
// Skip: diagnostic string formatting for a verification harness.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ty_evidence_header(ty: &Path) -> String {
    format!("ty binary: {} [{}]\n", ty.display(), ty_build_stamp(ty))
}

/// The identity of the SOLVER a certificate bundle came from, for the same
/// reason [`ty_evidence_header`] exists one function up: a gate panic must name
/// the binary an operator has to act on.
///
/// The ay lane learned this on 2026-08-31, when `sparkle_v2` went red with
/// `nova_budget_closed_form got=unknown` on an obligation unchanged since July
/// and named no binary. FOUR ay builds were installed and discovery bound the
/// one that disagreed (a stale 0.10.0; 0.5.0, 0.13.0 and 0.22.0 all discharged
/// it). Unlike `ty`, ay names its OWN build on every solve
/// (`c ay.session.start … build.stamp=…`), so the bundle script records that
/// per verdict — this header is the harness-level companion for the failures
/// that never reach a solve at all (a missing script, a bundle that produced no
/// output), where the path and mtime are the only facts there are.
#[must_use]
// Skip: diagnostic string formatting for a verification harness.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ay_evidence_header(ay: &Path) -> String {
    format!("ay binary: {} [{}]\n", ay.display(), ty_build_stamp(ay))
}

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
/// (`RainbowJumpBurstLifecycle`: interpreter 128 states, `ty` 1, both "clean").
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
                 obligation did not run. Install ty once: aterm pkg install ty.",
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
                 obligation did not run. Install ty once: aterm pkg install ty.",
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
/// Refuse a `ty` dead set that was measured on a SMALLER space than the
/// interpreter walked — the companion guard to [`arm_whole_space_check`], for
/// any driver that reads a dead set out of a `ty` transcript.
///
/// Arming and checking are two different obligations and only one of them
/// survives a flag being dropped. `aterm-gui`'s gate hand-rolled its flags,
/// omitted `--no-auto-por`, and got a dead set off a 1-state reduction of a
/// 128-state model; nothing downstream could tell, because a dead set carries no
/// record of the space it was measured on. It does now: pass the transcript here
/// and a reduced run is a hard failure instead of four phantom dead actions.
///
/// Deliberately separate from [`audit_dead_negative_controls`] rather than folded
/// into its signature: the audit has a dozen call sites that pass a dead set from
/// somewhere other than a live `ty` run (fixtures, hand-written expectations),
/// and those have no transcript to offer.
///
/// # Panics
/// If `ty`'s transcript has no parsable `States found:` line, or reports a
/// different count than the interpreter's reachable space at `Buggy = 0`.
// Skip: a verification-harness assert — the panic IS the gate.
#[cfg_attr(trust_verify, trust::skip)]
pub fn assert_ty_saw_whole_space(m: &Model, evidence: &str, label: &str) {
    let committed = interp::with_buggy(m, 0);
    let interp_states = match interp::bmc(&committed) {
        Ok(n) => n,
        // NOT an early return. The tempting reading — "a violation is a louder
        // failure the caller already reports" — is wrong, and wrong in the exact
        // direction that matters: the caller's report is `ty`'s exit status, and
        // the whole reason this guard exists is that the discovered `ty` returns
        // CLEAN, exit 0, "exhaustive" on specs it never explored. So the caller
        // sees success, this returns silently, the dead sets agree at `{}`, and a
        // model the interpreter knows is violated is booked as proved. Failing
        // open here would reintroduce the false proof one layer down.
        Err((state, invariant)) => panic!(
            "{label}: {} — the interpreter finds invariant `{invariant}` VIOLATED at {state:?} \
             at the committed config, while `ty` returned a clean verdict. A clean `ty` over a \
             violated model is a false proof, not a disagreement about coverage.\n{evidence}",
            m.name
        ),
    };
    assert_same_space_explored(m, interp_states, evidence, label);
}

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

/// PER-INVARIANT non-vacuity: the invariants of `m` that NO `Buggy = 1` member
/// falsifies when each is checked ALONE, in model order.
///
/// **Why isolating them matters.** `prove_and_catch` stops at the FIRST violated
/// invariant, so a model passes it with one live property carrying the whole
/// catch while every other invariant is a GHOST — true by construction,
/// unfalsifiable by any mutant, stating nothing about the code. That is not
/// hypothetical: `press_custody_model` shipped it once over a self-reported flag,
/// and `selection_custody_model` shipped it again with five of eight invariants
/// unfalsifiable. Both were caught by hand, in the one test file someone thought
/// to check.
///
/// A non-empty result is NOT automatically a defect. A model's SPACE guards
/// (`StateBounds` and friends) state the bounds rather than a design claim, and
/// are expected here — which is why the two callers differ in what they do with
/// the list: the per-model gates name their space guards and demand the rest be
/// empty, while the workspace ratchet holds the whole set flat.
///
/// # Panics
///
/// Propagates the interpreter's own panic for a model it cannot evaluate (a
/// function-valued `Expr` is TLA+-generation only). That is deliberate: an
/// uninterpretable model must be VISIBLE to the caller, never quietly reported
/// as carrying no ghosts — which is the same silence this whole check exists to
/// refuse.
#[must_use]
pub fn uncaught_invariants(m: &Model) -> Vec<&'static str> {
    let buggy = interp::with_buggy(m, 1);
    m.invariants
        .iter()
        .filter(|inv| {
            let mut alone = buggy.clone();
            alone.invariants.retain(|other| other.name == inv.name);
            interp::bmc(&alone).is_ok()
        })
        .map(|inv| inv.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::is_pending_stub;

    /// A pending-program stub on PATH is NOT the tool: discovery must skip it,
    /// or a harness runs the placeholder and reads its refusal as tool output
    /// (the spec-xref gate hard-failed exactly that way once a pending
    /// `trust-ir` stub reached PATH).
    #[test]
    fn a_pending_stub_is_not_the_tool() {
        let dir = std::env::temp_dir().join(format!("aterm_stub_probe_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mk probe dir");
        let stub = dir.join("trust-ir-probe");
        std::fs::write(
            &stub,
            "#!/bin/sh\n# atpkg pending-program stub v1\n# Replaced by the real shim when the program installs.\nexit 127\n",
        )
        .expect("write stub");
        assert!(is_pending_stub(&stub), "the stub names itself");

        let real = dir.join("trust-ir-real");
        std::fs::write(&real, b"\x7fELF fake binary bytes").expect("write real");
        assert!(!is_pending_stub(&real), "a real binary is not a stub");

        let missing = dir.join("nope");
        assert!(
            !is_pending_stub(&missing),
            "unreadable candidates stay eligible — a false pending would hide an installed tool"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The stub check reads the HEAD of a candidate and nothing more: a real
    /// tool is ~160 MB, and every store copy of it is checked. Proved with a
    /// FIFO whose writer sends a stub's head and then holds the pipe OPEN — a
    /// whole-file read never sees EOF and blocks, a bounded one answers.
    #[cfg(unix)]
    #[test]
    fn the_pending_stub_check_reads_only_the_head() {
        let dir = std::env::temp_dir().join(format!("aterm_stub_head_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mk probe dir");
        let fifo = dir.join("endless");
        let made = std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo");
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let writer_path = fifo.clone();
        let writer = std::thread::spawn(move || {
            use std::io::Write as _;
            let mut w = std::fs::OpenOptions::new()
                .write(true)
                .open(&writer_path)
                .expect("open fifo for write");
            let mut head = b"#!/bin/sh\n# atpkg pending-program stub v1\n".to_vec();
            // Longer than the bound, so a bounded read stops inside it.
            head.resize(320, b'#');
            assert!(head.len() as u64 > super::PENDING_STUB_HEAD);
            let _ = w.write_all(&head);
            // Hold the pipe open: no EOF until the check has answered.
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(30));
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let reader_path = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(is_pending_stub(&reader_path));
        });
        let answer = rx.recv_timeout(std::time::Duration::from_secs(10));
        let _ = release_tx.send(());
        let _ = writer.join();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            answer,
            Ok(true),
            "the check must answer from the head while the writer holds the pipe open — \
             a whole-file read blocks here waiting for an EOF that never comes"
        );
    }

    use super::*;
    use crate::derive::{config_catalog_snapshot_model, ring_model, transact_model};
    use crate::ty_model;

    /// The arming is a LIST, and every entry on it is load-bearing for a
    /// different reason — so the list is pinned exactly, not merely "contains".
    ///
    /// `ty_drivers_are_armed` proves every driver CALLS this function. Nothing
    /// proved what the function then emits, which is the half that actually
    /// arms anything: the 2026-08-06 red gate was a driver that emitted four
    /// of these five flags. An `assert!(contains)` per flag would let the list
    /// be quietly reordered or extended; an exact match makes any edit to the
    /// arming a deliberate, reviewed act.
    ///
    /// It is also this file's REACH GUARD for the fast lane. `--bfs-only` is
    /// what makes each spawn 0.03s instead of 0.58s, and it is invisible
    /// everywhere else: drop it and every verdict in the workspace stays
    /// green while `tools/verify.sh` silently grows ~3.5 minutes back. The
    /// only place that regression can be caught is here.
    #[test]
    fn the_arming_emits_exactly_the_flags_it_documents() {
        let mut cmd = Command::new("ty");
        arm_whole_space_check(&mut cmd);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                // Reduction OFF, both halves — the whole-space obligation, and
                // the only reason `assert_same_space_explored` can be written.
                "--no-auto-por",
                "--no-auto-symmetry",
                // Footprint hint: 4x the largest model, so it bounds nothing.
                "--initial-capacity",
                "8192",
                // The oracle engine, not the native codegen lane.
                "--backend",
                "interpreter",
                // The fused CDEMC symbolic lane OFF: 0.55s per spawn to report
                // `Winner: BFS` on every model in the tree. Removing this is a
                // ~3.5 minute regression on every `tools/verify.sh` run.
                "--bfs-only",
                // No cached verdicts. The cache is path-keyed, and the
                // hand-written ISOLATION specs live at a stable path, so
                // without this a repeat run replays a security PASS it never
                // re-derived.
                "--force",
            ],
            "the `ty` arming changed — every flag here is load-bearing; see the \
             doc on `arm_whole_space_check` before editing this list"
        );
    }

    /// The atpkg-store prefix mirror must keep matching
    /// `atpkg::platform::default_prefix` + `/bin` (the deliberate
    /// no-dependency duplication both sites cross-reference).
    #[test]
    fn store_bin_dir_mirrors_the_atpkg_default_prefix() {
        #[cfg(target_os = "macos")]
        assert_eq!(
            unix_store_bin_dir(Path::new("/Users//x")),
            Path::new("/Users//x/Library/Application Support/aterm/pkg/bin")
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            unix_store_bin_dir(Path::new("/home/x")),
            Path::new("/home/x/.local/share/aterm/pkg/bin")
        );
    }

    /// A dangling store shim never satisfies discovery; a live one — a symlink an
    /// older atpkg laid, or today's exec stub — resolves to the real store target
    /// (what the caller will execute), not the shim.
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
        // THE SHAPE ATPKG LAYS TODAY: a `#!/bin/sh` exec stub, never a symlink. Its first
        // `exec '<path>' "$@"` line is the target — including a path with a quote in it.
        std::fs::remove_file(&shim).unwrap();
        let quoted = d.join("store/it's/ty");
        std::fs::create_dir_all(quoted.parent().unwrap()).unwrap();
        std::fs::write(&quoted, b"#!/bin/sh\n").unwrap();
        let stub = |target: &str| {
            let _ = std::fs::remove_file(&shim);
            std::fs::write(
                &shim,
                format!(
                    "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n\
                     exec '{target}' \"$@\"\n"
                ),
            )
            .unwrap();
        };
        stub(&quoted.to_string_lossy().replace('\'', "'\\''"));
        assert_eq!(
            resolve_store_shim(&shim),
            Some(std::fs::canonicalize(&quoted).unwrap()),
            "an exec stub resolves to the file it execs"
        );
        // A stub whose build was reclaimed never satisfies discovery, and neither does one
        // naming a RELATIVE path — which would resolve against the cwd: `Cargo.toml` is a
        // real file relative to a `targo test` run's cwd, so only the absolute-path rule
        // refuses it.
        stub(&d.join("store/gone/ty").to_string_lossy());
        assert_eq!(
            resolve_store_shim(&shim),
            None,
            "a reclaimed build is absent"
        );
        assert!(
            Path::new("Cargo.toml").is_file(),
            "fixture: the relative file exists"
        );
        stub("Cargo.toml");
        assert_eq!(
            resolve_store_shim(&shim),
            None,
            "a relative target is absent"
        );
        // atpkg's pending-program stub execs `"$ATPKG"`, never a quoted literal path.
        std::fs::remove_file(&shim).unwrap();
        std::fs::write(
            &shim,
            b"#!/bin/sh\n# atpkg pending-program stub v1\nATPKG='/a/atpkg'\n\
              if [ -x \"$ATPKG\" ]; then\n  exec \"$ATPKG\" __pending 'ty' \"$@\"\nfi\nexit 127\n",
        )
        .unwrap();
        assert_eq!(resolve_store_shim(&shim), None, "a pending stub is absent");
        let _ = std::fs::remove_dir_all(&d);
    }

    // -----------------------------------------------------------------------
    // PROVENANCE — the escalation tier names the binary it ran. Every test
    // below runs over a TEMP home + store prefix through the explicit-env
    // resolver, so nothing reads or mutates the process environment and no
    // test depends on what this machine has installed.
    // -----------------------------------------------------------------------

    /// A scratch HOME and atpkg prefix for one test.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("aterm-spec-prov-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("home")).unwrap();
            std::fs::create_dir_all(root.join("prefix/bin")).unwrap();
            // Canonical, so `~` shortening matches the paths discovery returns
            // (macOS temp_dir is itself behind the /var -> /private/var link).
            let root = std::fs::canonicalize(&root).unwrap();
            Self { root }
        }
        fn home(&self) -> PathBuf {
            self.root.join("home")
        }
        fn store_bin(&self) -> PathBuf {
            self.root.join("prefix/bin")
        }
        fn file(&self, rel: &str) -> PathBuf {
            let p = self.root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"\x7fELF not really\n").unwrap();
            p
        }
        /// `prefix/store/<program>/<build>/bin/<exe>` + `current -> <build>`.
        #[cfg(unix)]
        fn store_copy(&self, program: &str, build: &str, exe: &str) -> PathBuf {
            let p = self.file(&format!("prefix/store/{program}/{build}/bin/{exe}"));
            let dir = self.root.join(format!("prefix/store/{program}"));
            std::os::unix::fs::symlink(dir.join(build), dir.join("current")).unwrap();
            p
        }
        fn locate(&self, bin: &str, path_var: Option<&std::ffi::OsStr>) -> Option<Located> {
            find_trust_bin(bin, Some(&self.store_bin()), path_var)
        }
        fn line(&self, bin: &str, found: Option<&Located>) -> String {
            provenance_line(
                bin,
                found,
                Some("ty 9.9.9"),
                &[self.home()],
                Some(&self.store_bin()),
            )
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Store PRESENT, PATH empty. With no usable
    /// `ty` shim (m3's store lost its own-name `ty`/`ay` shims to a trust
    /// rollback, measured 2026-09-23) nothing is found, and the line names the
    /// store copy and WHY discovery missed it instead of implying the tool is
    /// not installed. With a live shim — today's exec stub, or an older
    /// symlink — the store tier answers and the line says so.
    #[cfg(unix)]
    #[test]
    fn provenance_store_present_names_the_unreached_copy() {
        let s = Scratch::new("nobs-st");
        s.store_copy("ty", "3007", "ty");
        let shim = s.store_bin().join("ty");
        let unreached = |why: &str| {
            assert_eq!(s.locate("ty", None), None, "{why}");
            let line = s.line("ty", None);
            assert!(
                line.starts_with("VERIFY ESCALATION TIER: ty = NOT FOUND"),
                "{line}"
            );
            assert!(
                line.contains("the managed store holds store/ty/3007/bin/ty (built 2")
                    && line.ends_with(&format!("but discovery does not reach it: {why}")),
                "{line}"
            );
        };
        unreached("no `ty` shim in the store's bin/ exposes it");

        std::fs::write(&shim, "#!/bin/sh\necho revoked >&2\nexit 1\n").unwrap();
        unreached("the store's `ty` shim is a refusal script (tombstone or pending stub)");

        let stub = |target: &Path| {
            std::fs::write(
                &shim,
                format!(
                    "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n\
                     exec '{}' \"$@\"\n",
                    target.display()
                ),
            )
            .unwrap();
        };
        stub(&s.root.join("prefix/store/ty/2999/bin/ty"));
        unreached("the store's `ty` shim does not resolve to a file (a reclaimed build?)");

        // atpkg's CURRENT shim form, live: the store tier answers, and the copy
        // it resolves to is not listed as shadowing itself.
        let target = s.root.join("prefix/store/ty/3007/bin/ty");
        stub(&target);
        let found = s
            .locate("ty", None)
            .expect("a live exec stub is the store tier");
        assert_eq!(
            (found.origin, found.path.clone()),
            (TrustBinOrigin::Store, target.clone())
        );
        let line = s.line("ty", Some(&found));
        assert!(
            line.starts_with(&format!(
                "VERIFY ESCALATION TIER: ty = {} [atpkg managed store; ty 9.9.9; built 2",
                target.display()
            )),
            "{line}"
        );
        assert!(
            !line.contains(" — "),
            "the chosen copy is not its own shadow: {line}"
        );

        // An older atpkg's symlink shim is the store tier too.
        std::fs::remove_file(&shim).unwrap();
        std::os::unix::fs::symlink(&target, &shim).unwrap();
        let found = s
            .locate("ty", None)
            .expect("a symlink shim is the store tier");
        assert_eq!(found.origin, TrustBinOrigin::Store);
    }

    /// Nothing installed: not found, and the plain notice with no store clause.
    #[test]
    fn provenance_nothing_installed_is_a_plain_not_found() {
        let s = Scratch::new("none");
        assert_eq!(s.locate("ty", None), None);
        assert_eq!(
            s.line("ty", None),
            "VERIFY ESCALATION TIER: ty = NOT FOUND (probed the atpkg store shim, PATH)"
        );
    }

    /// THE order, pinned: the store shim beats PATH, and a developer tree under home
    /// — a first-party cargo build, a bootstrap stage, a standalone `~/ay` — is never
    /// a candidate (m3's stale first-party trust-ir 0.2.0 shadowed store 1123 until
    /// 2026-09-24). Another copy in the store is named beside the chosen one.
    #[cfg(unix)]
    #[test]
    fn discovery_order_is_store_then_path_and_no_developer_tree() {
        let s = Scratch::new("order");
        s.file("home/trust/first-party/trust-ir/target/release/trust-ir");
        s.file("home/trust/build/host/stage1/bin/trust-ir");
        s.file("home/ay/target/release/ay");
        assert_eq!(
            s.locate("trust-ir", None),
            None,
            "no developer tree is probed"
        );
        assert_eq!(s.locate("ay", None), None, "no standalone ~/ay either");

        let on_path = s.file("pathdir/trust-ir");
        let path_var = std::ffi::OsString::from(on_path.parent().unwrap());
        let got = s.locate("trust-ir", Some(&path_var)).unwrap();
        assert_eq!((got.origin, got.path), (TrustBinOrigin::Path, on_path));

        let store = s.store_copy("trust-ir", "1123", "trust-ir");
        s.store_copy("trust", "9192", "trust-ir");
        std::os::unix::fs::symlink(&store, s.store_bin().join("trust-ir")).unwrap();
        let got = s.locate("trust-ir", Some(&path_var)).unwrap();
        assert_eq!(
            (got.origin, got.path.clone()),
            (TrustBinOrigin::Store, store)
        );
        let line = s.line("trust-ir", Some(&got));
        assert!(
            line.contains("[atpkg managed store; ")
                && line.contains(" — the managed store also holds store/trust/9192/bin/trust-ir"),
            "{line}"
        );
    }

    /// A PATH hit that is an atpkg exec stub (a shim dir on PATH other than
    /// this prefix's `bin/`) names the store file it execs, and that store copy
    /// is not listed as shadowed (it is the one that ran).
    #[cfg(unix)]
    #[test]
    fn a_path_exec_stub_names_its_target() {
        let s = Scratch::new("pathstub");
        let target = s.store_copy("trust-ir", "1123", "trust-ir");
        let shim = s.root.join("pathbin/trust-ir");
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\n# atpkg shim\nexec '{}' \"$@\"\n",
                target.display()
            ),
        )
        .unwrap();
        let path_var = std::ffi::OsString::from(shim.parent().unwrap());
        let found = s.locate("trust-ir", Some(&path_var)).unwrap();
        assert_eq!(found.origin, TrustBinOrigin::Path);
        let line = s.line("trust-ir", Some(&found));
        let want = format!("[PATH (atpkg shim → {}); ", target.display());
        assert!(line.contains(&want), "{line}");
        assert!(!line.contains(" — "), "{line}");
    }

    /// The exec-stub reader mirrors `atpkg::platform::parse_sh_shim_target`.
    #[test]
    fn exec_stub_target_reads_atpkg_shims_and_rejects_refusals() {
        let s = Scratch::new("stubparse");
        let shim = s.root.join("shim");
        std::fs::write(
            &shim,
            "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\nexec '/a/it'\\''s/ty' \"$@\"\n",
        )
        .unwrap();
        assert_eq!(exec_stub_target(&shim), Some(PathBuf::from("/a/it's/ty")));
        std::fs::write(&shim, "#!/bin/sh\necho revoked >&2\nexit 1\n").unwrap();
        assert_eq!(exec_stub_target(&shim), None, "a tombstone has no target");
        assert_eq!(classify_store_shim(&shim), StoreShim::Refusal);
        assert_eq!(classify_store_shim(&s.root.join("nope")), StoreShim::Absent);
    }

    /// The report is once per tool per process: the first claim wins, every
    /// later one (the hundreds of `find_ty` calls in a suite) is silent and
    /// spawns nothing.
    #[test]
    fn provenance_is_claimed_once_per_tool() {
        let set = Mutex::new(BTreeSet::new());
        assert!(claim_first_report(&set, "ty"));
        assert!(!claim_first_report(&set, "ty"));
        assert!(claim_first_report(&set, "ay"));
        assert!(!claim_first_report(&set, "ay"));
    }

    #[test]
    fn build_dates_are_utc_civil_dates() {
        assert_eq!(civil_from_unix_days(0), (1970, 1, 1));
        assert_eq!(civil_from_unix_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_unix_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_unix_days(20_719), (2026, 9, 23));
        let s = Scratch::new("date");
        let d = build_date(&s.file("x")).expect("a fresh file has an mtime");
        assert_eq!((d.len(), &d[4..5], &d[7..8]), (10, "-", "-"), "{d}");
    }

    /// The one spawn: `--version`'s first non-empty line. `/bin/echo` echoes
    /// its argument on the BSD userland and names itself on GNU — either way a
    /// line, from a binary that is not freshly written (a fresh file can stall
    /// in exec assessment under load, measured on this host).
    #[cfg(unix)]
    #[test]
    fn the_version_probe_returns_the_first_line() {
        let v = probe_version(Path::new("/bin/echo")).expect("echo answers");
        assert!(!v.is_empty() && !v.contains('\n'), "{v:?}");
        assert_eq!(probe_version(Path::new("/nonexistent/ty")), None);
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
                    "AdmitStaleKitty",
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
                "AdmitStaleKitty",
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
        assert_eq!(
            ty_states_explored("Statistics:\n  States found: 1\n"),
            Some(1)
        );
        // Drift / no statistics block / non-numeric — all refusals, not zeros.
        assert_eq!(ty_states_explored("No errors found (exhaustive)."), None);
        assert_eq!(ty_states_explored("  States explored: 128\n"), None);
        assert_eq!(ty_states_explored("  States found: many\n"), None);
    }

    /// REGRESSION (2026-07-30 FALSE PROOF): `ty` reported "No errors found
    /// (exhaustive)" for `RainbowJumpBurstLifecycle` after exploring ONE of its 128
    /// reachable states — its partial-order reduction formed a singleton ample
    /// set out of the model's stutter action, whose only successor is the
    /// expanding state, and the C3 cycle proviso failed to reject it. The
    /// verdicts agreed; the work behind them did not.
    ///
    /// This pins the CONSEQUENCE rather than the cause: a smaller `ty` count is
    /// a false proof and must panic. The cause is fixed in `ty` itself, and this
    /// stays green either way — reduction is off for this driver, so a `ty`
    /// whose reduction regresses again cannot reach the gate through this door.
    /// REGRESSION (2026-08-06 RED GATE): `spec_xref_closure` hand-rolled its own
    /// `ty` flag list and omitted `--no-auto-por`, so it ran the whole registry
    /// with partial-order reduction ON. On `RainbowJumpBurstLifecycle` that was
    /// `POR: 1/1 states reduced (100.0%)` — 128 reachable states explored as 1 —
    /// and four of its six actions therefore "never fired". `--strict-vacuity`
    /// duly called them dead, and the tier comparison blew up on a dead set that
    /// was an artifact of the reduction.
    ///
    /// The arming is now shared ([`arm_whole_space_check`]) so no driver can
    /// omit it, and this is the second lock: a dead set carries no record of the
    /// space it was measured on, so the transcript must be shown to have covered
    /// the whole thing before anything in it is believed.
    /// The guard must not fail OPEN on the one input that matters most: a clean
    /// `ty` verdict over a model the interpreter knows is violated.
    ///
    /// This looks like a case the caller already reports — it is not. The
    /// caller's report is `ty`'s exit status, and the whole premise of this guard
    /// is that the discovered `ty` returns clean/exit-0 on spaces it never
    /// entered. Returning early here would let exactly that combination through.
    #[test]
    fn a_clean_ty_verdict_over_a_violated_model_is_a_false_proof_not_a_coverage_gap() {
        // The guard forces `Buggy = 0` itself, so the violation has to come from
        // somewhere it does not override. `LenBounded` is `seq - lo + 1 <= Cap`,
        // and the ring starts at `seq = 0, lo = 1` — so `Cap = -1` is violated in
        // the INITIAL state, which is as committed as a config gets.
        let m = interp::with_consts(&ring_model(), &[("Cap", -1)]);
        assert!(
            interp::bmc(&interp::with_buggy(&m, 0)).is_err(),
            "fixture must be a model whose committed config is violated"
        );
        let panicked = std::panic::catch_unwind(|| {
            assert_ty_saw_whole_space(
                &m,
                "Model checking complete: No errors found (exhaustive).\n\
                 Statistics:\n  States found: 1\n",
                "false-proof guard",
            );
        });
        assert!(
            panicked.is_err(),
            "a clean ty verdict over a violated committed config must never pass silently"
        );
    }

    /// The checker-identity header rides along with the transcript, so it must be
    /// invisible to everything that reads one.
    #[test]
    fn the_ty_evidence_header_is_invisible_to_the_transcript_parsers() {
        let body = "Model checking complete: No errors found (exhaustive).\n\
                    WARNING: 2 dead action(s) (never fired): Alpha, Beta\n\
                    Statistics:\n  States found: 128\n";
        let header = ty_evidence_header(Path::new("/some/where/ty"));
        assert!(header.contains("/some/where/ty"), "names the binary");
        assert!(header.ends_with('\n'), "must not run into the first line");
        let with = format!("{header}{body}");
        assert_eq!(
            ty_states_explored(&with),
            ty_states_explored(body),
            "the header must not disturb the state count"
        );
        assert_eq!(ty_states_explored(&with), Some(128));
        // And the dead-action marker still resolves to the same first match.
        const MARKER: &str = "dead action(s) (never fired): ";
        let pick = |t: &str| -> Option<String> {
            t.lines()
                .find_map(|l| l.find(MARKER).map(|i| l[i + MARKER.len()..].to_string()))
        };
        assert_eq!(pick(&with), pick(body));
        assert_eq!(pick(&with).as_deref(), Some("Alpha, Beta"));
    }

    #[test]
    fn a_dead_set_from_a_reduced_run_is_refused_by_the_space_guard() {
        let m = ring_model();
        let full = interp::bmc(&interp::with_buggy(&m, 0)).expect("committed config is clean");
        // The honest transcript passes.
        assert_ty_saw_whole_space(
            &m,
            &format!(
                "Model checking complete: No errors found (exhaustive).\n\
                 Statistics:\n  States found: {full}\n"
            ),
            "whole-space guard",
        );
        // The reduced one — ty's real shape when POR collapses the space — does not.
        let reduced = std::panic::catch_unwind(|| {
            assert_ty_saw_whole_space(
                &m,
                "POR: 1/1 states reduced (100.0%), 1 actions skipped\n\
                 Model checking complete: No errors found (exhaustive).\n\
                 Statistics:\n  States found: 1\n",
                "whole-space guard",
            );
        });
        assert!(
            reduced.is_err(),
            "a dead set measured on a 1-state reduction of a {full}-state model must be refused"
        );
    }

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

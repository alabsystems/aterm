// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The single shared **verification gate** for aterm's conformance and spec-link
//! tests — the honesty ratchet, TWO-TIER (VERIFY-1).
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
//! `PATH`. That is the whole order: no source checkout or bootstrap stage is
//! probed, because a stale one would shadow the store's build. A developer who
//! wants their own build points the shim at it: `aterm pkg link <tool> <checkout>`. The
//! store is looked for under this ACCOUNT's home (`getpwuid_r`, never `$HOME`,
//! which a test process moves — `home_dirs` says what that cost); `PATH`, and on
//! Windows `%HOME%`/`%USERPROFILE%` and `%LOCALAPPDATA%`, are the only
//! environment access.
//!
//! ## The tier names the binary it ran
//!
//! Discovery writes one line straight to stderr (not through `eprintln!`, which libtest swallows
//! for a passing test) the first time it resolves each tool in a process, and
//! AGAIN whenever the build it resolves to changes mid-process:
//!
//! ```text
//! VERIFY ESCALATION TIER: ty = ~/Library/Application Support/aterm/pkg/store/ty/3007/bin/ty [atpkg managed store; ty 0.15.0; built 2026-09-24T17:02:11Z; 157309456 bytes] — the managed store also holds store/trust/9192/bin/ty (built 2026-09-19); caps=none
//! ```
//!
//! (`built` is the file's mtime in UTC and `bytes` its length — a reinstall or a
//! rebuild moves them — because `--version` does not change between builds of
//! one package version. `caps=` is what `ty capabilities --json` advertised, so
//! the line names the protocol that answered.)
//!
//! **The line names the build actually used.** "The build it resolves to" is the chosen path plus
//! its identity (`BinaryIdentity`): length, mtime, device, inode and ctime, followed through a
//! wrapper script to the file it `exec`s. So a discovery that sees an `aterm pkg` update repoint
//! the store shim, a tool rebuilt in place, a yank, or a PATH wrapper whose target alone was
//! replaced prints a fresh line, ending `changed mid-process (was …)`. A wrapper whose exec target
//! does not read statically (computed from `$0` or a variable, conditional, through `env`) is the
//! limit: its line says so, and its `built`/`bytes` are the script's own.
//!
//! Every discovery, [`find_ty`] included, resolves afresh on every call, so
//! the first call after a change is the one that prints its line. The `ty`
//! verdicts follow the same identity (`memoized_verdict`): a step is answered
//! by the build the last line names, never replayed from, or asked of the live
//! co-process of, another.
//!
//! It costs one `--version` spawn per printed line (and, for `ty`, one
//! `capabilities` spawn per build; each bounded by `PROBE_HANG`), and per
//! discovery a few `stat`s and a 256-byte head read per candidate plus a
//! two-byte read of the chosen file; store copies are described from the
//! filesystem alone, never by running them. [`locate_ty`]/[`locate_ay`]/
//! [`locate_trust_ir`] return the same answer as a [`Located`] for a caller that
//! wants the tier programmatically.
//!
//! ## Caller idioms
//!
//! ```ignore
//! // SCALAR derived model — the overwhelmingly common case (66 of the 70 in
//! // tree, and every `ty_model!`-authored one). The interpreter always runs, so
//! // coverage is unconditional and the returned [`Covered`] is a log line, not
//! // a decision:
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
//! The hard-require form [`ay`] is for a gate that must never run tool-less (the `ay` certificate
//! test is the one in-tree caller).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use crate::derive::{Liveness, Model};
use crate::interp;

/// Discover the Trust `ty` model-checker: the atpkg-managed store's shim, then
/// `ty` on `PATH` (see [`locate_ty`] for which tier answered). The store is
/// this account's (its home from the account database, not `$HOME`); `PATH` and
/// (Windows) the home variables and `%LOCALAPPDATA%` are the only environment
/// access.
///
/// The first discovery of each tool in a process, and every later one that
/// resolves to a different build, prints ONE `VERIFY ESCALATION TIER: ty = …`
/// line straight to stderr naming the binary, the tier it came from, its
/// `--version`, build time and size, and every managed store copy it shadowed
/// (the module doc, "The tier names the binary it ran").
///
/// Discovered afresh on every call, never cached: the answer is the build the store's shim forwards
/// to NOW. The store tier answers the RESOLVED build (`store/ty/<build>/bin/ty`), and an `aterm
/// pkg` update, link or yank rewrites only the shim and leaves the old build on disk, so a cache
/// would keep a superseded or yanked build for the rest of the process (and an old build's accept
/// would hide a TIER DISAGREEMENT). A call is a few `stat`s, a shim read and a PATH walk, about 65
/// µs, beside the `ty` run that follows. Every call also takes the answer's [`BinaryIdentity`], so
/// the call that first sees another build is the one that prints its line.
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

/// The real-environment discovery + the provenance report.
// Skip: thin wrapper over `discover_identified`. Build-tooling discovery.
#[cfg_attr(trust_verify, trust::skip)]
fn discover(bin: &'static str) -> Option<Located> {
    discover_identified(bin).map(|(located, _)| located)
}

/// [`discover`], plus the [`BinaryIdentity`] of what it found — the one the
/// provenance line was judged by — printing the line whenever what discovery
/// resolves to differs from the last line printed for `bin`
/// ([`report_provenance_on_change`]).
// Skip: environment + filesystem discovery and a stderr report.
// Build-tooling discovery; every miss returns None (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
fn discover_identified(bin: &'static str) -> Option<(Located, Option<BinaryIdentity>)> {
    let homes = home_dirs();
    let store_bin = atpkg_store_bin_dir();
    let path_var = std::env::var_os("PATH");
    let found = find_trust_bin(bin, store_bin.as_deref(), path_var.as_deref()).map(|located| {
        let identity = binary_identity(&located.path);
        (located, identity)
    });
    report_provenance_on_change(bin, found.as_ref(), &homes, store_bin.as_deref());
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
/// update both sites (each carries this cross-reference). The `~` is the
/// account's home on Unix ([`home_dirs`]), so every thread of a process asks
/// the same store.
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

/// A shim is trusted only when it FORWARDS to a real file: a symlink, or the
/// `#!/bin/sh` exec stub atpkg lays.
///
/// atpkg lays the stub on purpose (`atpkg::platform::unix`:
/// `targo` refuses a symlinked `current_exe`, so a shim `exec`s the store file
/// at its real path). Its target is the first trimmed line that reads
/// `exec '<path>' "$@"` — the same rule as atpkg's own
/// `platform::parse_sh_shim_target`, which every store answer (`which`, gc,
/// `prune_stale_shims`) is built on, including a routed shim whose guard line
/// starts with `[` and so never matches.
///
/// A TOMBSTONE must still read as ABSENT: atpkg disables a yanked or below-floor build by replacing
/// the shim with a failing notice script (`atpkg::activate::install_tombstone_shim`) that carries
/// no `exec` line, and the pending-program stub `exec`s `"$ATPKG"`, never a quoted literal path.
/// Neither parses, so neither can satisfy discovery — and because this probe runs before the PATH
/// search, a revoked build would otherwise SHADOW a working tool the developer already has on PATH.
/// Anything that is not a symlink or a readable regular file of at most 64 KiB (atpkg's own bound)
/// — a directory, a FIFO — is absent too.
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

/// Candidate home directories, the one discovery uses first.
///
/// On Unix that is THIS ACCOUNT's home ([`account_home`]), never the process's
/// `$HOME`. A test process moves its `$HOME`: aterm-gui's handoff fixtures
/// point it at a scratch directory while they live, so discovery that read it
/// would miss the store and take whatever `PATH` offered — or nothing, and the
/// escalation tests that needed `ty` would pass without running. The account's
/// home is the same answer for every thread for the life of the process, and it
/// is where the account's atpkg laid its store, so a process STARTED with a
/// synthetic `$HOME` finds that store too. `$HOME` answers only when the account
/// cannot be resolved.
///
/// Windows: `%HOME%`, then `%USERPROFILE%`. Native Windows shells set only
/// `USERPROFILE`; Git Bash sets `HOME`, sometimes as a POSIX-style `/c/…` path
/// that Win32 file APIs cannot resolve — normalize it.
fn home_dirs() -> Vec<PathBuf> {
    #[cfg(unix)]
    if let Some(home) = account_home() {
        return vec![home];
    }
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

/// This account's home directory as the OS knows it —
/// `getpwuid_r(getuid())->pw_dir`, which a process cannot move by setting
/// `$HOME` — resolved once per process. A mirror of
/// `atpkg::platform::unix::account_home` (a mirror, not a dependency edge, for
/// the reason [`atpkg_store_bin_dir`] gives). `None` when the lookup fails or
/// names no absolute directory; a failure is not remembered, so a later call
/// asks again.
// Skip: a process-wide cache of one libc answer. Build-tooling discovery.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn account_home() -> Option<PathBuf> {
    if let Some(home) = ACCOUNT_HOME.get() {
        return Some(home.clone());
    }
    let home = lookup_account_home().filter(|home| home.is_absolute())?;
    Some(ACCOUNT_HOME.get_or_init(|| home).clone())
}

/// [`account_home`]'s answer, once resolved. Module-level so a test's child
/// process can seat a scratch home before its first discovery (the store-update
/// test lays its own store there); nothing else writes it.
#[cfg(unix)]
static ACCOUNT_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// One `getpwuid_r` lookup of this uid's `pw_dir`, with a buffer sized for any
/// real passwd record (an `ERANGE` answers `None`, as any failure does).
// Skip: one libc call through raw out-pointers. Build-tooling discovery; every
// failure returns None (the caller falls back to `$HOME`).
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn lookup_account_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: `passwd` is plain integers and pointers, for which all-zero is a
    // valid value; it is only an out-param here.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = [0u8; 4096];
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: `pw` is a valid, writable passwd out-param; `buf` is a valid
    // writable buffer of the stated length that outlives every read of `pw`'s
    // string fields, which point INTO it on success; `out` is a valid
    // out-pointer. getpwuid_r writes nothing past buflen. `pw_dir` is read only
    // when `out` is non-null (success), and is then a NUL-terminated C string
    // inside `buf`, copied out before `buf` goes out of scope.
    unsafe {
        let rc = libc::getpwuid_r(
            libc::getuid(),
            &raw mut pw,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &raw mut out,
        );
        if rc != 0 || out.is_null() || pw.pw_dir.is_null() {
            return None;
        }
        let bytes = std::ffi::CStr::from_ptr(pw.pw_dir).to_bytes();
        (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
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
// PROVENANCE: the escalation tier names the binary it ran, so a stale override
// cannot answer unreported. Discovery prints one line naming the binary, its
// tier, its `--version`, its build time and size, and every other managed-store
// copy — the first time it resolves each tool in a process, and again whenever
// the [`BinaryIdentity`] it resolves to changes.
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

/// What replacing a file changes even when its path does not. The ctime is the
/// part no writer can set: an in-place rewrite to the same length whose mtime
/// is then put back (`cp -p`, `touch -r`, `rsync --inplace -t`, a tar extract)
/// leaves the other four equal. Device, inode and ctime are 0 off unix.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct FileIdentity {
    len: u64,
    modified: Option<std::time::SystemTime>,
    dev: u64,
    ino: u64,
    ctime: (i64, i64),
}

impl FileIdentity {
    /// The identity in `meta` (one `metadata` call, symlinks followed).
    // Skip: fs metadata accessors (absent std bodies). Verification tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let (dev, ino, ctime) = {
            use std::os::unix::fs::MetadataExt as _;
            (meta.dev(), meta.ino(), (meta.ctime(), meta.ctime_nsec()))
        };
        #[cfg(not(unix))]
        let (dev, ino, ctime) = (0, 0, (0, 0));
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            dev,
            ino,
            ctime,
        }
    }
}

/// How a wrapper script hands over to the next file in its exec chain.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum ExecForm {
    /// atpkg's own shim (its second line starts [`ATPKG_SHIM_HEADER`]),
    /// `exec '<path>' "$@"` — read by [`parse_exec_stub_target`], the rule the
    /// store probe and atpkg itself use.
    AtpkgShim,
    /// A hand-written forwarder whose only command is one `exec <literal
    /// absolute path> ["$@"]` (see [`literal_exec_target`]).
    LiteralExec,
}

/// One file in a [`BinaryIdentity`] chain.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct ChainLink {
    /// The path as the previous link (or discovery) named it.
    path: PathBuf,
    /// What the file there is now.
    file: FileIdentity,
    /// How this file execs the next link; `None` for the last.
    execs: Option<ExecForm>,
}

/// Where a [`BinaryIdentity`] chain stopped.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum ChainEnd {
    /// The last link is not a `#!` script: the binary that runs.
    Binary,
    /// The last link is a `#!` script whose exec target cannot be read
    /// statically. THE LIMIT of this scheme: a wrapper that computes its target
    /// (`"$(dirname "$0")/ty"`, `$HOME/…`, a bare name looked up on `PATH`),
    /// `exec`s conditionally or more than once, runs any other command, launches
    /// through `env`/an interpreter, or passes arguments of its own is identified
    /// by its OWN bytes only — replacing the binary behind it changes nothing
    /// here. (So is a script that runs no other program at all: a scripted
    /// test `ty` reads the same way, since nothing static can tell the two
    /// apart.) The provenance line says so.
    ///
    /// Not followed either: a shebang-less script (the kernel refuses it; `sh`
    /// runs it) reads as [`ChainEnd::Binary`] — atpkg never writes one — and on
    /// Windows a `.cmd` forwarder is a binary too.
    OpaqueScript,
    /// The last link execs this path, which cannot be stat'ed.
    Dangling(PathBuf),
    /// More than [`MAX_EXEC_HOPS`] links, or a link that execs an earlier one.
    TooDeep,
}

/// The identity of what running a path executes: the file itself and, while
/// it is a wrapper script whose exec target reads statically, each file down
/// that chain to the binary at its end. Two discoveries that answer the same
/// path name the same build only if their identities are equal — a wrapper
/// that keeps its own bytes while the binary it execs is replaced (a PATH `ty`
/// that forwards into a store build an update moved, a linked checkout
/// rebuilt) is a different identity.
///
/// It keys the provenance line (re-printed when it changes), [`ty_caps`], and
/// the `ty` verdict memo and `--cases` pool ([`TyChecker`]), so the line, the
/// protocol the caller speaks and the verdicts it replays can never name
/// different builds.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct BinaryIdentity {
    /// From the path asked about, in exec order; never empty.
    links: Vec<ChainLink>,
    /// Why the chain stopped.
    end: ChainEnd,
}

impl BinaryIdentity {
    /// The last file reached: the binary itself when [`ChainEnd::Binary`].
    /// (`binary_identity` never builds an empty chain.)
    fn last(&self) -> Option<&ChainLink> {
        self.links.last()
    }
}

/// How many wrapper hops [`binary_identity`] follows before it gives up.
const MAX_EXEC_HOPS: usize = 8;

/// The [`BinaryIdentity`] of `path`; `None` when `path` itself cannot be
/// stat'ed. Per link: one `metadata` call and a two-byte read (only a `#!`
/// file of at most atpkg's 64 KiB shim bound is read further), so the big
/// binary at the end costs a `stat` and an open.
// Skip: fs metadata + bounded reads (absent std bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn binary_identity(path: &Path) -> Option<BinaryIdentity> {
    let mut links: Vec<ChainLink> = Vec::new();
    let mut at = path.to_path_buf();
    loop {
        let meta = match std::fs::metadata(&at) {
            Ok(meta) => meta,
            Err(_) if links.is_empty() => return None,
            Err(_) => {
                return Some(BinaryIdentity {
                    links,
                    end: ChainEnd::Dangling(at),
                });
            }
        };
        let file = FileIdentity::of(&meta);
        if links.len() == MAX_EXEC_HOPS || links.iter().any(|l| l.file == file) {
            return Some(BinaryIdentity {
                links,
                end: ChainEnd::TooDeep,
            });
        }
        let hop = if meta.is_file() {
            script_exec_target(&at, meta.len())
        } else {
            Hop::Binary
        };
        let (execs, next) = match hop {
            Hop::Execs(target, form) => (Some(form), Ok(target)),
            Hop::Binary => (None, Err(ChainEnd::Binary)),
            Hop::Opaque => (None, Err(ChainEnd::OpaqueScript)),
        };
        links.push(ChainLink {
            path: at,
            file,
            execs,
        });
        match next {
            Ok(target) => at = target,
            Err(end) => return Some(BinaryIdentity { links, end }),
        }
    }
}

/// What a file is, as an exec chain sees it: where the chain goes next.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Hop {
    /// Not a `#!` script (or unreadable): the binary, the end of the chain.
    Binary,
    /// A `#!` script whose exec target does not read statically.
    Opaque,
    /// A `#!` script that execs this absolute path.
    Execs(PathBuf, ExecForm),
}

/// Classify the file at `p` (of length `len`) for [`binary_identity`]. Only
/// the first two bytes of a non-script are read.
// Skip: bounded file reads (absent std bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn script_exec_target(p: &Path, len: u64) -> Hop {
    use std::io::Read as _;
    let Ok(mut file) = std::fs::File::open(p) else {
        return Hop::Binary;
    };
    let mut magic = [0u8; 2];
    if file.read_exact(&mut magic).is_err() || magic != *b"#!" {
        return Hop::Binary;
    }
    let mut text = String::from("#!");
    if len > MAX_STORE_SHIM_BYTES
        || file
            .take(MAX_STORE_SHIM_BYTES)
            .read_to_string(&mut text)
            .is_err()
    {
        return Hop::Opaque;
    }
    let atpkg = text
        .lines()
        .nth(1)
        .is_some_and(|l| l.starts_with(ATPKG_SHIM_HEADER));
    let (target, form) = if atpkg {
        (parse_exec_stub_target(&text), ExecForm::AtpkgShim)
    } else {
        (literal_exec_target(&text), ExecForm::LiteralExec)
    };
    match target {
        Some(t) if t.is_absolute() => Hop::Execs(t, form),
        _ => Hop::Opaque,
    }
}

/// How the second line of every `sh` shim atpkg writes begins (the full line is
/// `# atpkg shim — exec so the tool authenticates at its real path.`, from
/// `atpkg::platform::sh_shim_body` — a mirror, like [`parse_exec_stub_target`];
/// if atpkg ever changes its header, update both sites). Only a file that
/// carries it is read by atpkg's own target rule: that rule takes the first
/// `exec '<path>' "$@"` line ANYWHERE in the file, which is right for atpkg's
/// shapes (an `export` block, a route guard ahead of the store `exec`) and wrong
/// for a hand-written script that might run something else before it. Without
/// the header a script is held to [`literal_exec_target`].
const ATPKG_SHIM_HEADER: &str = "# atpkg shim";

/// The one file a hand-written forwarder script `exec`s, when the WHOLE script
/// reads statically: after the `#!` line every line is blank, a comment, or a
/// `set` of shell options (`set -e`, `set -euo pipefail`), except the last such
/// line, which is `exec <path>` or `exec <path> "$@"` with `<path>` a literal
/// absolute path — bare (no shell metacharacters), in `'…'`, or in `"…"` with
/// nothing to expand. Anything else — any other command (it could run another
/// program first), a second or conditional `exec`, a computed target,
/// `exec env …`, extra arguments — is `None`: the script is opaque (see
/// [`ChainEnd::OpaqueScript`]).
fn literal_exec_target(text: &str) -> Option<PathBuf> {
    let mut exec_line = None;
    for line in text.lines().skip(1).map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if exec_line.is_some() {
            return None;
        }
        if !is_shell_option_line(line) {
            exec_line = Some(line);
        }
    }
    let rest = exec_line?.strip_prefix("exec")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let (target, after) = shell_literal_word(rest.trim_start())?;
    matches!(after.trim(), "" | "\"$@\"").then(|| PathBuf::from(target))
}

/// `set` with nothing but option words (`-e`, `+x`, `-o`, `pipefail`): it changes
/// how the shell runs the script, never which program the script runs.
fn is_shell_option_line(line: &str) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some("set")
        && words.all(|w| {
            let name = w.strip_prefix(['-', '+']).unwrap_or(w);
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic())
        })
}

/// The leading shell word of `s` when it is a literal — `'…'`, `"…"` with no
/// `$`, `` ` `` or `\`, or bare with no shell metacharacter — and what follows
/// it; `None` for anything the shell would expand.
///
/// No byte offsets: `split_once`/`strip_suffix` leave no slice bound and no
/// `+ 1` for the Trust lane to discharge (an offset-based first draft added
/// six refuted obligations to aterm-spec's advisory lane).
fn shell_literal_word(s: &str) -> Option<(&str, &str)> {
    let quoted = |q: char, forbidden: &str| {
        let (word, after) = s.strip_prefix(q)?.split_once(q)?;
        let glued = after.chars().next().is_some_and(|c| !c.is_whitespace());
        (!word.chars().any(|c| forbidden.contains(c)) && !glued).then_some((word, after))
    };
    match s.chars().next()? {
        '\'' => quoted('\'', ""),
        '"' => quoted('"', "$`\\"),
        _ => {
            let after = s.trim_start_matches(|c: char| !c.is_whitespace());
            let word = s.strip_suffix(after)?;
            let meta = "$`\\\"'*?[]{}~;&|<>()";
            (!word.chars().any(|c| meta.contains(c))).then_some((word, after))
        }
    }
}

/// `t` as a UTC `YYYY-MM-DDTHH:MM:SSZ` — a build time precise enough to tell
/// two builds of one day apart.
fn utc_timestamp(t: std::time::SystemTime) -> Option<String> {
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let (y, m, d) = civil_from_unix_days(i64::try_from(secs / 86_400).ok()?);
    let s = secs % 86_400;
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    ))
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

/// How long a once-per-process probe may take before it is called wedged: a
/// HANG DETECTOR, not a latency budget. The answer is cached for the rest of
/// the process (an empty [`ty_caps`] set sends every case to a process of its
/// own), so a bound a loaded machine can exceed turns one slow first exec
/// into a permanent loss. Measured: the first exec of a fresh script on macOS
/// took over five seconds at load 80-100.
const PROBE_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// `<bin> <args>`'s output, or `None` if it cannot start or does not finish
/// within [`PROBE_HANG`] (it is then killed — a wedged probe must never wedge
/// the suite for good). The one bound both once-per-process probes share.
// Skip: spawns a subprocess and polls it. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn probe_output(bin: &Path, args: &[&str]) -> Option<std::process::Output> {
    probe_output_within(bin, args, PROBE_HANG)
}

/// [`probe_output`] with its bound given: the seam the tests hold the kill
/// and the wait to without waiting a minute.
// Skip: spawns a subprocess and polls it. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn probe_output_within(
    bin: &Path,
    args: &[&str],
    bound: std::time::Duration,
) -> Option<std::process::Output> {
    use std::process::Stdio;
    use std::time::Instant;
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + bound;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    child.wait_with_output().ok()
}

/// The first line `<bin> --version` prints, or `None` if it fails, prints
/// nothing, or does not answer within [`probe_output`]'s bound. Called once per
/// printed line, from [`report_provenance_on_change`].
// Skip: spawns a subprocess. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn probe_version(bin: &Path) -> Option<String> {
    let out = probe_output(bin, &["--version"])?;
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

/// `ty` speaks the `--cases` co-process protocol (A10 below).
const CAP_TRACE_CASES: &str = "trace-cases-v1";
/// `ty`'s automatic memory budget is its own, not divided by the live process
/// count — the precondition for keeping idle co-processes alive (ty withdraws
/// `trace-cases-v1` with it).
const CAP_BUDGET_SELF: &str = "check-budget-self-v1";

/// What the `ty` at `ty` says it guarantees (`ty capabilities --json`), probed
/// ONCE per build per process and cached.
///
/// WHY (docs/design/TY-TRACE-CASES-AND-FAIL-CLOSED-2026-10-01.md A8): store builds of one `ty`
/// release share a version string, so a protocol is detected by the capability the build
/// advertises, never by `--version`. A `ty` older than the command answers `unrecognized
/// subcommand` with exit 2, which is the empty set — and so is every other failure to answer: an
/// empty set is the per-process behaviour every `ty` supports. Keyed by the path AND its
/// [`BinaryIdentity`], not a single value: [`find_ty`] re-resolves on every call (an `aterm pkg`
/// update moves it to another build mid-run), and neither a build at another path nor a `ty`
/// rebuilt in place (or behind an unchanged wrapper) may inherit its predecessor's answer — the
/// `caps=` its provenance line prints is this answer, so a stale one would name another build's
/// protocol.
// Skip: a cached subprocess probe. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn ty_caps(ty: &Path) -> Vec<String> {
    static CAPS: CapsByBuild = Mutex::new(BTreeMap::new());
    caps_cached(&CAPS, ty, |ty| {
        probe_output(ty, &["capabilities", "--json"])
            .map(|out| caps_from(out.status.code(), &out.stdout))
            .unwrap_or_default()
    })
}

/// [`ty_caps`]'s cache: one answer per (path, [`BinaryIdentity`]).
type CapsByBuild = Mutex<BTreeMap<(PathBuf, Option<BinaryIdentity>), Vec<String>>>;

/// [`ty_caps`] over an explicit cache and probe: the cached answer for this
/// build of `ty`, else `probe`'s, remembered.
// Skip: a Mutex-guarded cache plus fs metadata (absent std bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn caps_cached(
    cache: &CapsByBuild,
    ty: &Path,
    probe: impl FnOnce(&Path) -> Vec<String>,
) -> Vec<String> {
    let build = (ty.to_path_buf(), binary_identity(ty));
    let mut caps = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(known) = caps.get(&build) {
        return known.clone();
    }
    let probed = probe(ty);
    caps.insert(build, probed.clone());
    probed
}

/// The capability list in a `ty capabilities --json` reply
/// (`{"ty":"0.15.0","capabilities":[...]}`), in `ty`'s order. Anything but
/// exit 0 with that document — exit 2 from a `ty` without the command, a
/// malformed reply — is the empty set.
fn caps_from(code: Option<i32>, stdout: &[u8]) -> Vec<String> {
    if code != Some(0) {
        return Vec::new();
    }
    let Ok(doc) = aterm_json::from_slice::<aterm_json::Value>(stdout.trim_ascii()) else {
        return Vec::new();
    };
    doc.get("capabilities")
        .and_then(aterm_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The provenance line's protocol clause: `; caps=<a,b,…>` or `; caps=none`.
fn caps_clause(caps: &[String]) -> String {
    if caps.is_empty() {
        "; caps=none".to_string()
    } else {
        format!("; caps={}", caps.join(","))
    }
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
/// `identity` is what running it executes (see [`BinaryIdentity`]) and
/// `version` its `--version`; `shadowed` are the store copies OTHER than the
/// chosen binary; `shim` explains a miss.
///
/// The build the line names — `built` (mtime, UTC, to the second) and `bytes`
/// — is the LAST file of the identity's exec chain: the binary a wrapper
/// forwards to, not the wrapper.
fn render_provenance(
    bin: &str,
    found: Option<&Located>,
    identity: Option<&BinaryIdentity>,
    version: Option<&str>,
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
    let last = identity.and_then(BinaryIdentity::last);
    let built = last
        .and_then(|l| l.file.modified)
        .and_then(utc_timestamp)
        .unwrap_or_else(|| "date unknown".to_owned());
    let bytes = last.map_or_else(
        || "size unknown".to_owned(),
        |l| format!("{} bytes", l.file.len),
    );
    let mut line = format!(
        "VERIFY ESCALATION TIER: {bin} = {} [{}{}; {}; built {built}; {bytes}]",
        tilde(&found.path, home),
        found.origin.label(),
        identity.map_or_else(String::new, |id| describe_exec_chain(id, home)),
        version.unwrap_or("version unknown"),
    );
    if !shadowed.is_empty() {
        line.push_str(&format!(" — the managed store also holds {copies}"));
    }
    line
}

/// How the chosen path reaches the binary it runs, as a ` (…)` note for the
/// provenance line — empty when the chosen path IS that binary.
fn describe_exec_chain(id: &BinaryIdentity, home: Option<&Path>) -> String {
    let mut note = String::new();
    if let Some(form) = id.links.first().and_then(|l| l.execs) {
        note.push_str(match form {
            ExecForm::AtpkgShim => "atpkg shim",
            ExecForm::LiteralExec => "wrapper",
        });
        for link in id.links.iter().skip(1) {
            note.push_str(&format!(" → {}", tilde(&link.path, home)));
        }
    }
    match &id.end {
        ChainEnd::Binary => {}
        ChainEnd::OpaqueScript => {
            if !note.is_empty() {
                note.push_str("; ");
            }
            note.push_str(
                "script whose exec target does not read statically: built/bytes are the \
                 script's own",
            );
        }
        ChainEnd::Dangling(p) => note.push_str(&format!(" → {} (missing)", tilde(p, home))),
        ChainEnd::TooDeep => {
            note.push_str(&format!(" → … (over {MAX_EXEC_HOPS} hops, or a loop)"));
        }
    }
    if note.is_empty() {
        note
    } else {
        format!(" ({note})")
    }
}

/// Everything the provenance line needs except the spawns (`version`), over an
/// explicit environment: the store copies other than the binary the chosen
/// path runs, and what the store's own shim is.
// Skip: fs probes + formatting. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn provenance_line(
    bin: &str,
    found: Option<&Located>,
    identity: Option<&BinaryIdentity>,
    version: Option<&str>,
    homes: &[PathBuf],
    store_bin: Option<&Path>,
) -> String {
    let exe = exe_name(bin);
    let copies = store_bin
        .and_then(Path::parent)
        .map(|prefix| store_copies_in(&prefix.join("store"), &exe))
        .unwrap_or_default();
    // The file the chosen path runs (through every wrapper the identity
    // followed), so the store copy it IS is not listed as shadowed.
    let chosen = identity
        .and_then(BinaryIdentity::last)
        .and_then(|l| std::fs::canonicalize(&l.path).ok());
    let shadowed: Vec<StoreCopy> = copies
        .into_iter()
        .filter(|c| std::fs::canonicalize(&c.path).ok() != chosen)
        .collect();
    let shim = store_bin.map_or(StoreShim::Absent, |d| classify_store_shim(&d.join(&exe)));
    render_provenance(
        bin,
        found,
        identity,
        version,
        &shadowed,
        &shim,
        homes.first().map(PathBuf::as_path),
    )
}

/// What one provenance line describes: what discovery chose, and the identity
/// of what it runs (`None` inside when it could not be stat'ed). `None` = NOT
/// FOUND.
type ProvenanceKey = Option<(Located, Option<BinaryIdentity>)>;

/// The last line printed for ONE tool, by its [`ProvenanceKey`]; `None` until
/// the first.
type LastLine = Mutex<Option<ProvenanceKey>>;

/// Per tool, its own [`LastLine`]: a line holds only its own tool's lock while
/// its probes run, so a slow first exec of one tool's new build never holds up
/// another tool's discovery.
type LastReported = Mutex<BTreeMap<&'static str, std::sync::Arc<LastLine>>>;

/// What [`claim_report`] decided for one discovery.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Claim {
    /// The last line printed for this tool describes exactly this: stay silent.
    Unchanged,
    /// No line for this tool yet in this process.
    First,
    /// The tool now resolves to something else; the key of the line it replaces.
    Changed(ProvenanceKey),
}

/// The bookkeeping behind "print when it changes": `last` holds one tool's key
/// of the last line printed. A discovery equal to it is silent; any other
/// becomes the new last line.
fn claim_report(last: &mut Option<ProvenanceKey>, now: &ProvenanceKey) -> Claim {
    if last.as_ref() == Some(now) {
        return Claim::Unchanged;
    }
    match last.replace(now.clone()) {
        None => Claim::First,
        Some(prev) => Claim::Changed(prev),
    }
}

/// What a repeated line says it replaced when the path is the same: a file
/// the path runs (it, or a file down its exec chain) was rewritten or replaced
/// — or only had its metadata changed (a `chmod`, a new hard link), which
/// moves the ctime the identity needs to see a same-length rewrite whose mtime
/// was put back. It never claims the bytes changed.
const SAME_PATH_CHANGED: &str =
    "this same path; a file it runs was since rewritten, replaced or had its metadata changed";

/// The suffix a REPEATED line carries: it replaces an earlier line of this
/// process, and says what that line named.
fn change_note(prev: &ProvenanceKey, now: Option<&Located>, home: Option<&Path>) -> String {
    let was = match (prev, now) {
        (None, _) => "NOT FOUND".to_owned(),
        (Some((p, _)), Some(n)) if p.path == n.path => SAME_PATH_CHANGED.to_owned(),
        (Some((p, _)), _) => tilde(&p.path, home),
    };
    format!(" — changed mid-process (was {was})")
}

/// Print the provenance line for `bin` when what discovery resolved differs
/// from the last line printed for `bin` in this process: the first discovery,
/// and then every store update, yank, in-place rebuild or wrapper retarget
/// that changes the [`ProvenanceKey`]. The hundreds of other discoveries in a
/// suite stay silent and spawn nothing.
///
/// Written straight to the process's stderr handle rather than via
/// `eprintln!`: libtest captures the print macros for a PASSING test and
/// throws the text away. A direct handle write is not captured, so the line
/// reaches the developer's terminal and the gate's stage log on a green run.
// Skip: fs probes, subprocesses, a stderr write. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn report_provenance_on_change(
    bin: &'static str,
    found: Option<&(Located, Option<BinaryIdentity>)>,
    homes: &[PathBuf],
    store_bin: Option<&Path>,
) {
    use std::io::Write;
    static LAST: LastReported = Mutex::new(BTreeMap::new());
    report_on_change(
        &LAST,
        bin,
        found,
        homes,
        store_bin,
        |path| {
            // A8: the protocol that will answer, so every
            // transcript and receipt names it (per-process for `caps=none`,
            // `--cases` for a `ty` advertising `trace-cases-v1`).
            let caps = if bin == "ty" {
                caps_clause(&ty_caps(path))
            } else {
                String::new()
            };
            (probe_version(path), caps)
        },
        |line| {
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        },
    );
}

/// [`report_provenance_on_change`] over explicit state: `last` is the
/// bookkeeping, `probe` answers the chosen path's `--version` and the text the
/// line ends with (`ty`'s `caps=` clause), `write` takes a line to print.
///
/// The tool's [`LastLine`] stays locked until the line is written, so a tool's
/// lines are written in the order their keys were claimed: with two threads
/// racing across a change, the last line written for a tool is always the one
/// its [`LastLine`] holds. Only a claimed line holds it for long (its probes,
/// each bounded by [`PROBE_HANG`]), and only for that tool: the map of tools
/// is released before the claim.
// Skip: fs probes, caller-supplied probes and writer. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn report_on_change(
    last: &LastReported,
    bin: &'static str,
    found: Option<&(Located, Option<BinaryIdentity>)>,
    homes: &[PathBuf],
    store_bin: Option<&Path>,
    probe: impl FnOnce(&Path) -> (Option<String>, String),
    write: impl FnOnce(&str),
) {
    let now: ProvenanceKey = found.cloned();
    let mut tools = last
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let slot = std::sync::Arc::clone(tools.entry(bin).or_default());
    drop(tools);
    let mut last = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let prev = match claim_report(&mut last, &now) {
        Claim::Unchanged => return,
        Claim::First => None,
        Claim::Changed(prev) => Some(prev),
    };
    let located = now.as_ref().map(|(l, _)| l);
    let identity = now.as_ref().and_then(|(_, id)| id.as_ref());
    let (version, ending) = located.map(|l| probe(&l.path)).unwrap_or_default();
    let mut line = provenance_line(bin, located, identity, version.as_deref(), homes, store_bin);
    line.push_str(&ending);
    if let Some(prev) = &prev {
        line.push_str(&change_note(
            prev,
            located,
            homes.first().map(PathBuf::as_path),
        ));
    }
    write(&line);
    drop(last);
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
/// MB), so reading the whole file would cost hundreds of megabytes per tool per
/// test process.
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

/// Locate the Trust `ay` solver for `label`, or PANIC with the install hint.
/// Verification is ALWAYS required — no env var, no skip. A test that cannot
/// reach `ay` FAILS rather than reporting a false `ok`.
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
// The ESCALATION tier (VERIFY-1): EXTERNAL-tool
// analyses run only where their tool is installed. The notice is one prominent
// line naming exactly what did not run and how to enable it — never silent, and
// never claimed as a discharged check. Same-model agreement is required only for
// the tiered interpreter/ty paths below.
// ---------------------------------------------------------------------------

/// Write one notice line straight to the process's stderr handle. libtest
/// captures `eprintln!` for a PASSING test and discards it (see
/// [`report_provenance_on_change`]), and a NOT RUN notice must reach the
/// gate's log precisely when the test reports `ok`.
#[cfg_attr(trust_verify, trust::skip)]
fn notice(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// What a missing `bin` needs: the install command when the store holds no
/// copy, else the store note and the repair, since installing again would not
/// make an unreached copy reachable.
fn missing_tool_tail(bin: &str) -> (&'static str, String) {
    let store_note = unreached_store_note(bin);
    if store_note.is_empty() {
        (
            "is not installed",
            format!(" Install: aterm pkg install {bin}"),
        )
    } else {
        (
            "was not found",
            format!("{store_note} Repair: aterm pkg doctor, then aterm pkg repair"),
        )
    }
}

/// The NOT RUN notice for a function-valued model on a machine where no `ty`
/// was found.
fn function_valued_not_run(label: &str, model: &str) {
    let (_, tail) = missing_tool_tail("ty");
    notice(&format!(
        "NOT RUN (not a pass): `{label}` ({model}) is function-valued, and no Trust `ty` \
         was found, so it was not checked.{tail}"
    ));
}

/// Shared escalation report + early-return decision.
fn escalation(bin: &str, found: Option<PathBuf>, label: &str) -> Option<PathBuf> {
    if found.is_none() {
        // "Not installed" is only true when the store holds no copy either; a
        // copy discovery cannot reach is named, never reported as absent.
        let (state, tail) = missing_tool_tail(bin);
        notice(&format!(
            "NOT RUN (not a pass): `{label}` needs Trust `{bin}`, which {state}.{tail}"
        ));
    }
    found
}

/// The `ty` ESCALATION tier: `Some(path)` when installed; `None` (with a
/// prominent notice) otherwise — the caller returns early. Use for obligations
/// the in-process tier cannot express: hand-written `.tla` specs,
/// `--strict-vacuity` verdicts, `ty trace` overclaim controls.
#[must_use]
pub fn ty_escalation(label: &str) -> Option<PathBuf> {
    escalation("ty", find_ty(), label)
}

/// The `trust-ir` (`spec-link`) escalation tier. aterm's current emitted artifact is
/// explicitly `DesignOnly`, so this is non-certifying structural analysis rather than
/// Ob.3 certification. See [`ty_escalation`] for discovery/notice behavior.
#[must_use]
pub fn trust_ir_escalation(label: &str) -> Option<PathBuf> {
    escalation("trust-ir", find_trust_ir(), label)
}

/// The `ay` (SMT/CHC certificate) escalation tier. See [`ty_escalation`].
#[must_use]
pub fn ay_escalation(label: &str) -> Option<PathBuf> {
    escalation("ay", find_ay(), label)
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

/// Run `ty check` on a derived model's generated spec + cfg (`cfg` overrides
/// come pre-rendered); panics if `ty` cannot run.
///
/// Returns (verdict, evidence): whether `ty check` exited 0, plus a rendered
/// transcript naming the exact binary and its full output — so a TIER
/// DISAGREEMENT panic identifies WHICH `ty` build said what.
// Skip: shells out to `ty` and renders its output (absent std format/io
// bodies + the closure it drives). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn ty_check_derived(ty: &Path, m: &Model, cfg: &str, label: &str) -> (bool, String) {
    let dir = scratch_dir("aterm-tier", m.name);
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

/// A fresh scratch directory for ONE checker call:
/// `<temp>/<kind>-<model>-<pid>-<n>`, `n` unique per call in this process.
///
/// Per CALL, not per `(model, pid)`: every test in one process that checks the
/// same model at once would otherwise share the directory, and each writes its
/// spec and cfg, runs its own `ty` and removes the directory afterwards — so
/// one test's cfg (a `Buggy = 0` prove arm, say) could be read by the other's
/// `ty` (a `Buggy = 1` catch arm), or deleted under it.
// Skip: formats a path from the process id and a counter (absent std format
// bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn scratch_dir(kind: &str, model: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "{kind}-{model}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// The flags EVERY `ty check` of a derived model carries.
///
/// **Reduction off** (`--no-auto-por`, `--no-auto-symmetry`). The escalation
/// tier must walk the SAME bounded space the interpreter walked, not a cheaper
/// one: a reduced run explores a representative subset by design, so its state
/// count carries no information and [`assert_same_space_explored`] could not be
/// written. The largest registered model has 1712 reachable states, so the
/// whole sweep costs seconds either way.
///
/// It is not hypothetical. A `ty` build (`tla 0.10.0`) whose ample-set
/// reduction predated `ty`'s C3 cycle proviso explored ONE state of
/// `RainbowJumpBurstLifecycle`'s 128 with reduction on — the stutter action
/// `SlowJump` formed a singleton ample set — and at `Buggy = 1`, where
/// `NoLostFadePayload` is violated three steps from `Init`, printed `No errors
/// found (exhaustive)` and exited 0 under a `Soundness mode: Sound` banner. A
/// FALSE PROOF. `NativeConfigTransaction` (1662/1712) and `CursorCatCurseWince`
/// (30/36) under-explored the same way. Nothing here may assume the binary is
/// new, which is why every `ty check` in the workspace arms through this fn.
///
/// **`--initial-capacity 8192`** — tell `ty` these models are small. Left to
/// itself it pre-allocates fingerprint storage for millions of states:
/// measured on `NativeTabIdentity` (399 reachable states), peak RSS was 4.25 GB
/// unset versus 17 MB with the hint. 8192 is 4x the largest model; the hash set
/// still grows on its own, so under-sizing it cannot truncate a search. The
/// 4 GB footprint also collided with `ty`'s load-dependent memory ceiling:
/// `NativeTabIdentity` stopped at 1 of 399 states with "memory limit reached"
/// AND EXIT 0 (now refused by [`ty_output`]).
///
/// **`--backend interpreter`** — `ty`'s trust-codegen backend compiles every
/// action before exploring: on `NativeTabIdentity` 18.5 s wall / 52 s CPU
/// against 0.58 s interpreted, for the identical verdict and count. Native
/// codegen pays for itself north of a million states; none of these models are
/// within three orders of magnitude of that. The engines are two
/// implementations inside one checker, not two oracles, so this is a speed
/// choice; the independent check is the interpreter tier here, held by
/// [`assert_same_space_explored`].
///
/// **`--bfs-only`** — the pure explicit-state lane, not the fused BFS+symbolic
/// (CDEMC) default. Over 639 real comparisons (all 125 registry models at
/// `Buggy = 0` and `1`, the same with `CHECK_DEADLOCK TRUE`, the
/// `--strict-vacuity` sweep and the 14 hand-written `aterm-spec-models` specs)
/// every fused run's winner was BFS, the symbolic lanes concluded nothing, and
/// the two lanes agreed byte-for-byte on every fact a transcript is read for:
/// exit status, `States found:`, `Soundness mode`, `Search completeness`, the
/// `Deadlock reached` and `dead action(s) (never fired):` wording, and `is
/// violated`. Only the clean-run wording differs (BFS prints the `No errors
/// found (exhaustive)` `examples/trust_models.rs` tests for). The fused lane cost
/// a fixed ~0.55 s per spawn, ~400 spawns per gate; `ty --version` returns in
/// under 10 ms.
///
/// Completeness is structural: every cfg here is a finite bounded machine,
/// INVARIANT-only (`Model::to_cfg`; `Model::to_tla` emits no fairness), and an
/// exhaustive BFS of a finite space IS the complete proof of a safety
/// invariant. The one other kind, a LIVENESS cfg (`Liveness::to_cfg_with`,
/// checked by [`liveness_proves_and_catches_tiered`]), builds the whole finite
/// graph and runs the fair-cycle search on it; checked, not assumed: on
/// `NativeUpdateApplyLadder`'s `TheLadderLands` both lanes prove it over the
/// interpreter's 3080 states and reject each of its four mutants alike.
///
/// **`--workers 1`** — these spaces are small; at one worker `ty` also skips
/// the pilot's second walk. The parallelism that pays is the test binary's own
/// threads, each running its own `ty`. Over 559 checks replayed one by one:
/// 57.1 s -> 40.1 s, with identical exit status, `States found` and violation
/// lines.
///
/// **`--force`** — bypass `ty`'s PATH-keyed check cache. A fail-closed flag, not
/// a speed one: `aterm-spec-models`' `model_check.rs` checks the checked-in
/// `specs/*.tla` at a stable path, and a repeat run would replay `Cache hit:
/// PASS` with a `States found:` line every parser here takes for fresh
/// evidence — a cached verdict for the ISOLATION family standing in for a walk.
/// Cost: a 7 ms replay becomes a 20 ms re-derivation.
///
/// The ceiling is deliberately NOT pinned with `--memory-limit`: `ty`'s limit
/// probe is not purely per-process, so a small explicit ceiling trips on a busy
/// machine even at 17 MB of own RSS. Shrinking the footprint is the fix.
///
/// PUBLIC so every `ty` driver in the workspace arms the same way: the
/// `spec_xref_closure` gate in `aterm-gui` once built its own command without
/// these flags and ran with reduction ON, reducing `RainbowJumpBurstLifecycle`
/// to one state and reporting four of its six actions dead. Anything reaching
/// for `ty` on a derived model must arm it through here.
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
        .arg("--workers")
        .arg("1")
        .arg("--force")
}

/// Run a `ty` subprocess, and FAIL CLOSED on a search that stopped early: a
/// run `ty` cut short is an `Err` naming INCOMPLETE, never an `Output` a
/// caller could read as a verdict.
///
/// **The hazard** (measured on store `ty` 0.13.0 for
/// `docs/design/TY-TRACE-CASES-AND-FAIL-CLOSED-2026-10-01.md` §1). `ty` sizes
/// its automatic memory budget as 0.9 x host RAM divided by the number of live
/// `ty`/`cargo`/`rustc` processes, counted once at the start of the run, and a
/// search that budget stops EXITS 0 — under a header, printed before the walk
/// began, that still says `Search completeness: exhaustive`, and with
/// `--require-exhaustive` checked only before the run. With 120 idle fake
/// `rustc` processes alive the budget fell to 271 MB, and an eight-worker check
/// stopped at 74,553 of 1,367,631 states and exited 0. An exit-status verdict
/// books that as a proof.
///
/// **The guard** is the classification below: exit code 5 (`ty`'s INCOMPLETE,
/// once the store build carries it) or a stdout line starting
/// `Model checking stopped:` — the line every limit-reached path prints — is an
/// `Err`. No driver in this workspace passes `--max-states`, `--max-depth`,
/// `--allow-incomplete`, `--memory-limit` or `--output` (only the test that
/// pins this guard, to provoke a stop), so every stop line is a truncation
/// nobody asked for. Converting it HERE, not in each caller, is
/// load-bearing: a `Buggy = 1` catch arm accepts any non-success exit, so a raw
/// exit 5 would book a vacuous catch, and every caller already fails on an
/// `Err` from this function (the tests panic; `examples/trust_models.rs` reports
/// it `Unknown`). `a_stopped_search_is_an_err_never_a_pass` pins the
/// classifier on a real stopped search.
///
/// **No lock.** Serialising spawns cost about 95-100 s per gate, never covered
/// the `cargo` and `rustc` that count in the same divisor, and with the guard
/// has no correctness role: an armed check peaks at about 10 MB, so on 36 GiB
/// the divisor alone needs roughly 2,800 live heavy processes to stop one, and
/// if it ever does the result is a loud red, never a false green.
///
/// PUBLIC because the rule binds every `ty` driver in the workspace, not only
/// the ones in this file: a driver that calls `Command::output()` itself is
/// invisible to the classification (`tests/ty_drivers_are_armed.rs` enforces
/// the routing).
// Skip: spawns a subprocess and classifies its transcript. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ty_output(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    with_scratch_ty_home(cmd);
    let out = cmd.output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if out.status.code() == Some(5)
        || stdout
            .lines()
            .any(|l| l.starts_with("Model checking stopped:"))
    {
        return Err(std::io::Error::other(format!(
            "ty INCOMPLETE: the search was stopped by a resource limit — nothing was proved\n\
             {}{cmd:?} ({:?})\n--- ty stdout ---\n{stdout}\n--- ty stderr ---\n{}",
            ty_evidence_header(Path::new(cmd.get_program())),
            out.status,
            String::from_utf8_lossy(&out.stderr),
        )));
    }
    Ok(out)
}

/// Spawn a long-lived `ty` co-process — the `--cases` pool's one door — with
/// [`ty_output`]'s scratch HOME ([`scratch_ty_env`]), stdin and stdout piped,
/// built by aterm-uds's `spawnfd::command`: it outlives the call, so it must
/// inherit its stdio and nothing else (a plain `Command`, on a std without SP-1,
/// hands it whatever another thread holds unflagged at that instant). The caller sends
/// stderr to a FILE, never a pipe: nobody drains a pipe between cases, and a
/// `ty` blocked on a full stderr pipe answers nothing.
///
/// A co-process is no run-to-exit transcript, so [`ty_output`]'s classification
/// has nothing to read; the client protocol in [`cases_verdict`] fails closed
/// instead (a request with no verdict line is never an accept).
/// `tests/ty_drivers_are_armed.rs` accepts this name as routed.
// Skip: spawns a subprocess. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn ty_child(
    cmd: &mut aterm_uds::spawnfd::Command,
) -> std::io::Result<aterm_uds::spawnfd::Child> {
    use aterm_uds::spawnfd::Stdio;
    for (key, value) in scratch_ty_env() {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()
}

/// Run `ty` with a HOME of its own, whose check cache starts empty, and
/// nothing taken from this process's `HOME`.
///
/// EVERY `ty` CHILD GETS IT, a capable one too. A `ty` with
/// `check-no-cache-v1` reads and writes no check cache under `--force`, but
/// its compiled-artifact cache, and whatever else it keeps under HOME, would
/// still follow a test process's moving `HOME` (below), so the capability
/// does not make the scratch HOME optional.
///
/// WHY (measured). `ty` keeps a check cache at `$HOME/.ty/cache.json`,
/// keyed by spec path, and reads and rewrites the whole file on every run, even
/// under `--force`. Every spec here is written to a per-process temp dir, so no
/// entry is ever read back, and the file only grows: 18.7 MB and 12,625 entries
/// after one day of gates, and a three-state spec took 57.5 ms under it against
/// 12.6 ms with an empty one — 45 ms per run, paid by every `ty` check on the
/// box (`derived_ring_ty`'s 552 of them: 48 s against 32 s).
///
/// The compiled-artifact cache lives in that HOME too, named explicitly by
/// `TY_CACHE_DIR` unless the caller already named one (the child inherits
/// it), never the process `HOME`'s: a test process moves its `HOME`
/// (aterm-gui's handoff tests point it at a scratch directory and then remove
/// it), so a `ty` another test thread started meanwhile would re-create that
/// scratch. Nothing a verdict depends on is in that cache, and starting it cold
/// costs nothing measurable (0.04 s and 0.18 s cold against 0.03 s and 0.21 s
/// warm on two specs).
///
/// Concurrent runs share the one HOME and can tear its `cache.json`. That costs
/// nothing a verdict depends on: `ty` warns on stderr and starts from an empty
/// cache when the load fails, a failed save only warns, and `--force` means no
/// entry is ever read back (`ty`'s `cmd_check` load and save paths).
// Skip: environment plumbing for a subprocess. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn with_scratch_ty_home(cmd: &mut Command) {
    for (key, value) in scratch_ty_env() {
        cmd.env(key, value);
    }
}

/// The environment [`with_scratch_ty_home`] sets, for a `ty` built by another
/// `Command` (the pooled co-process's).
fn scratch_ty_env() -> Vec<(&'static str, std::path::PathBuf)> {
    let home = std::env::temp_dir().join("aterm-ty-home");
    // A run that loses this race to another `ty` finds at most the one entry
    // that run wrote, for that run's own temp path.
    let _ = std::fs::remove_file(home.join(".ty/cache.json"));
    let mut env = Vec::with_capacity(2);
    if std::env::var_os("TY_CACHE_DIR").is_none() {
        env.push(("TY_CACHE_DIR", home.join(".cache/ty/compiled")));
    }
    env.push(("HOME", home));
    env
}

/// The identity of the checker a transcript came from — path AND build stamp —
/// so a gate panic names the binary an operator has to act on, not merely its
/// output. `--version` does not distinguish builds of one package version; the
/// mtime does.
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
/// Several ay builds can be installed at once, and a stale one can disagree
/// about an unchanged obligation. Unlike `ty`, ay names its OWN build on every solve
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

/// The checker binary's AGE, for an evidence header: the mtime of the LAST file of
/// its exec chain ([`binary_identity`]) — so a wrapper on `PATH` is stamped
/// with the binary it forwards to, which is then named — or `mtime unknown`.
// Skip: fs metadata + bounded reads + formatting. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn ty_build_stamp(ty: &Path) -> String {
    let identity = binary_identity(ty);
    let Some((last, modified)) = identity
        .as_ref()
        .and_then(BinaryIdentity::last)
        .and_then(|l| Some((l, l.file.modified?)))
    else {
        return "mtime unknown".to_string();
    };
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if identity.as_ref().is_some_and(|id| id.links.len() > 1) {
        format!("mtime {secs} (unix) of {}", last.path.display())
    } else {
        format!("mtime {secs} (unix)")
    }
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
/// it never entered (`RainbowJumpBurstLifecycle` under reduction: interpreter
/// 128 states, `ty` 1, both "clean").
/// A verdict agreement between a real proof and a vacuous one is not evidence;
/// it is a coincidence that reads like evidence.
///
/// So the tiers must agree on the WORK as well as the answer. Both walk the
/// whole reachable space of the same finite machine, and `to_cfg` emits no
/// VIEW and no SYMMETRY, so with reduction off (see `ty_check_derived`) their
/// state counts are not merely close — they are EQUAL.
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
/// to state a policy. A scalar model never reports [`NotRun`] (the interpreter
/// always runs), which [`prove_and_catch_scalar`] asserts once rather than at
/// every site.
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
            function_valued_not_run(label, m.name);
            Err(NotRun { model: m.name })
        }
    }
}

/// TIERED prove-and-catch (the `Buggy` convention): the interpreter proves the
/// invariant at `Buggy=0` and finds a counterexample at `Buggy=1` (panics
/// otherwise), and `ty` additionally does the same wherever installed (panics
/// on failure or tier disagreement). Function-valued models (`fn_vars`
/// non-empty: EvictFull, TierResidency, Recording — the only three in tree)
/// skip the interpreter (inapplicable by construction) and REQUIRE the
/// `ty` tier; with no `ty` they report [`NotRun`] loudly AND return it. Scalar
/// callers want [`prove_and_catch_scalar`].
// Skip: same tiered-driver class as `check_model_tiered`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_and_catch_tiered(m: &Model, label: &str) -> Result<Covered, NotRun> {
    // `prove_and_catch` proves the `Buggy = 0` arm and catches the `Buggy = 1`
    // one, and hands back the state count of the committed arm it just proved —
    // the `ty` tier's yardstick (see `assert_same_space_explored`).
    let interp_states = if m.fn_vars.is_empty() {
        Some(interp::prove_and_catch(m))
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
            function_valued_not_run(label, m.name);
            Err(NotRun { model: m.name })
        }
    }
}

/// Prove-and-catch a SCALAR model's obligation, asserting the scalar shape that
/// makes the `_tiered` form's `Err` half unreachable: the interpreter runs for
/// every model with no `fn_vars`, so coverage is unconditional and the returned
/// [`Covered`] is a log line rather than a decision. Asserting the shape up
/// front rather than unwrapping the result is what makes it machine-independent:
/// a model that later grows an `fn_vars` entry fails here on every machine,
/// instead of passing on developer boxes that happen to have `ty` installed and
/// failing only on a fresh clone.
// Skip: thin policy wrapper over `prove_and_catch_tiered` (same
// verification-tooling tier); its assert is a deliberate harness abort.
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

/// Refuse a `ty` dead set that was measured on a SMALLER space than the
/// interpreter walked — the companion guard to [`arm_whole_space_check`], for
/// any driver that reads a dead set out of a `ty` transcript.
///
/// Arming and checking are two different obligations and only one of them
/// survives a flag being dropped. `aterm-gui`'s gate hand-rolled its flags,
/// omitted `--no-auto-por`, and got a dead set off a 1-state reduction of a
/// 128-state model; a dead set carries no record of the space it was measured
/// on. Pass the transcript here and a reduced run is a hard failure instead of
/// phantom dead actions.
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
            let dir = scratch_dir("aterm-dl", m.name);
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

/// The interpreter's full discharge of a [`Liveness`] obligation, returned
/// rather than panicked so the registry sweep can name every failure at once.
/// `Ok(n)` is the number of mutants that break it; anything this cannot
/// establish is an `Err`, never a waiver:
///
/// 1. the model is scalar and carries the `Buggy` dial committed to 0, and the
///    obligation names at least one mutant — a liveness property nothing can
///    break would pass whatever the lane did, a ghost;
/// 2. every fairness name is an action that FIRES at the committed config (a
///    fairness assumption about a dead action assumes nothing), and every mutant
///    is an action that does NOT (it is a negative control, not the lane);
/// 3. at the committed config no fair behaviour leaves the goal for good;
/// 4. the `Buggy = 1` baseline with every committed-dead action removed has no
///    such behaviour either — otherwise an unrelated `Buggy` branch supplies the
///    counterexample and no mutant's catch is its own;
/// 5. each named mutant, added back ALONE to that baseline, produces one;
/// 6. every fairness assumption is LOAD-BEARING: dropped, the committed model
///    breaks the property, and a strong one demoted to weak breaks it too. The
///    verdict is only as true as the world is fair, so it may claim to rest on
///    nothing it does not need — an extra `WF` is a promise about the
///    environment the proof never used, and a reader could not tell it apart
///    from one it did.
///
/// The property and the fairness are the obligation's own; the model is not
/// rewritten beyond the `Buggy` flip and the mutant isolation, which are the same
/// two moves [`audit_dead_negative_controls`] makes for invariants.
// Skip: verification-harness driver over the interpreter tier (BTreeSet ops,
// model clones and deliberate audit-failure strings). Not shipping code.
#[cfg_attr(trust_verify, trust::skip)]
pub fn audit_liveness(m: &Model, live: &Liveness) -> Result<usize, String> {
    liveness_preflight(m, live)?;
    audit_liveness_on(m, live, &interp::Space::of(&interp::with_buggy(m, 0)))
}

/// The checks of [`audit_liveness`] that need no walk, so a model the
/// interpreter cannot walk is refused before anything tries to.
// Skip: BTreeSet/format machinery of the audit driver above.
#[cfg_attr(trust_verify, trust::skip)]
fn liveness_preflight(m: &Model, live: &Liveness) -> Result<(), String> {
    let label = format!("{} liveness `{}`", m.name, live.name);
    if !m.fn_vars.is_empty() {
        return Err(format!(
            "{label}: function-valued model — the interpreter cannot walk it"
        ));
    }
    if !m.consts.iter().any(|(n, v)| *n == "Buggy" && *v == 0) {
        return Err(format!(
            "{label}: no committed `Buggy = 0` dial, so no mutant can be isolated"
        ));
    }
    if live.mutants.is_empty() {
        return Err(format!(
            "{label}: names no mutant — a liveness property nothing breaks is a ghost"
        ));
    }
    let declared: BTreeSet<&str> = m.actions.iter().map(|a| a.name as &str).collect();
    for taken in [live.name.to_string(), live.spec_name()] {
        // `Init`, `Next`, `Spec` and `vars` are the module's own operators.
        let clashes = ["Init", "Next", "Spec", "vars"].contains(&taken.as_str())
            || declared.contains(taken.as_str())
            || m.invariants.iter().any(|i| i.name == taken)
            || m.vars.iter().any(|v| v.name == taken)
            || m.consts.iter().any(|(c, _)| *c == taken);
        if clashes {
            return Err(format!(
                "{label}: `{taken}` is already a name in the model; the emitted module \
                 would define it twice"
            ));
        }
    }
    Ok(())
}

/// [`audit_liveness`] after its preflight, over `committed`: `m`'s space at
/// `Buggy = 0`, walked by the caller. What fires, the committed verdict and each
/// fairness dropped are all read off that one walk, and a caller that needs more
/// of the same space (the tiered driver's state count and mutant isolation)
/// shares it rather than walking it again.
// Skip: BTreeSet/format machinery of the audit driver above.
#[cfg_attr(trust_verify, trust::skip)]
fn audit_liveness_on(
    m: &Model,
    live: &Liveness,
    committed: &interp::Space,
) -> Result<usize, String> {
    let label = format!("{} liveness `{}`", m.name, live.name);
    let declared: BTreeSet<&str> = m.actions.iter().map(|a| a.name as &str).collect();
    let fired = committed.fired();
    for name in live.weak.iter().chain(&live.strong) {
        if !declared.contains(name) {
            return Err(format!(
                "{label}: fairness names undeclared action `{name}`"
            ));
        }
        if !fired.contains(name) {
            return Err(format!(
                "{label}: fairness names `{name}`, which never fires at the committed \
                 config — an assumption about a dead action assumes nothing"
            ));
        }
    }
    let dead: BTreeSet<&str> = declared
        .iter()
        .copied()
        .filter(|a| !fired.contains(a))
        .collect();
    for mutant in &live.mutants {
        if !dead.contains(mutant) {
            return Err(format!(
                "{label}: mutant `{mutant}` is not an action dead at the committed config \
                 (dead: {dead:?}) — a negative control must be one"
            ));
        }
    }
    if let Some(cycle) = committed.nonprogress_under(live) {
        return Err(format!(
            "{label}: VIOLATED at the committed config (Buggy=0) — a fair behaviour \
             leaves the goal for good:\n{cycle}"
        ));
    }
    let assumptions = live
        .weak
        .iter()
        .map(|a| (a, "weak"))
        .chain(live.strong.iter().map(|a| (a, "strong")));
    for (name, kind) in assumptions {
        let mut without = live.clone();
        without.weak.retain(|a| a != name);
        without.strong.retain(|a| a != name);
        if committed.nonprogress_under(&without).is_none() {
            return Err(format!(
                "{label}: the {kind} fairness of `{name}` is not needed — the property holds \
                 without it. Drop it, so the verdict does not claim to rest on it"
            ));
        }
        if kind == "strong" {
            without.weak.push(name);
            if committed.nonprogress_under(&without).is_none() {
                return Err(format!(
                    "{label}: `{name}` needs only WEAK fairness — the property holds with it; \
                     state the weaker assumption"
                ));
            }
        }
    }
    let mut baseline = interp::with_buggy(m, 1);
    baseline.actions.retain(|a| !dead.contains(a.name));
    if let Some(cycle) = interp::nonprogress_under(&baseline, live) {
        return Err(format!(
            "{label}: the Buggy=1 baseline with every committed-dead action removed \
             already breaks it, so no mutant's catch is its own:\n{cycle}"
        ));
    }
    for mutant in &live.mutants {
        if interp::nonprogress_under(&isolate_mutant(m, fired, mutant), live).is_none() {
            return Err(format!(
                "{label}: mutant `{mutant}`, alone on the Buggy=1 baseline, does not break \
                 it — the property cannot see the defect that mutant is"
            ));
        }
    }
    Ok(live.mutants.len())
}

/// The `Buggy = 1` copy of `m` with every committed-dead action removed but
/// `mutant`: the healthy lane plus that one defect, and nothing another mutant
/// could lend it. The model a liveness mutant is checked on — by
/// [`audit_liveness`], and emitted for `ty` by
/// [`liveness_proves_and_catches_tiered`] — and the one a test drives to show
/// what that defect does on its own.
#[must_use]
// Skip: model clone + BTreeSet filter (absent std bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn buggy_baseline_with(m: &Model, mutant: &str) -> Model {
    isolate_mutant(m, &interp::fired_actions(&interp::with_buggy(m, 0)), mutant)
}

/// [`buggy_baseline_with`] for a caller that already holds the actions firing at
/// the committed config, so the space is not walked again to learn them.
// Skip: model clone + BTreeSet filter (absent std bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn isolate_mutant(m: &Model, fired: &BTreeSet<&'static str>, mutant: &str) -> Model {
    let mut isolated = interp::with_buggy(m, 1);
    isolated
        .actions
        .retain(|a| fired.contains(a.name) || a.name == mutant);
    isolated
}

/// Run `ty check` on a liveness module (`Liveness::to_tla`) with its cfg, in a
/// directory of its own per call ([`scratch_dir`]): the same model's invariant
/// or liveness check may be writing its own spec from a parallel test at the
/// same moment, and the file must be named after its module.
// Skip: shells out to `ty` and renders its output (absent std format/io bodies).
// Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn ty_check_liveness(
    ty: &Path,
    m: &Model,
    module: &str,
    cfg: &str,
    tag: &str,
    label: &str,
) -> (bool, String) {
    let dir = scratch_dir(&format!("aterm-live-{tag}"), m.name);
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{}.tla", m.name));
    let cfgp = dir.join(format!("{}.cfg", m.name));
    std::fs::write(&spec, module).expect("write liveness spec");
    std::fs::write(&cfgp, cfg).expect("write liveness cfg");
    let mut cmd = Command::new(ty);
    cmd.arg("check").arg(&spec).arg("--config").arg(&cfgp);
    let out = ty_output(arm_whole_space_check(&mut cmd))
        .unwrap_or_else(|e| panic!("run ty check for {label}: {e}"));
    let _ = std::fs::remove_dir_all(&dir);
    let evidence = format!(
        "{}({:?})\n--- ty stdout ---\n{}\n--- ty stderr ---\n{}",
        ty_evidence_header(ty),
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    (out.status.success(), evidence)
}

/// TIERED liveness: [`audit_liveness`] on the interpreter tier (always; panics on
/// any `Err`), and `ty` checking the SAME property wherever it is installed —
/// `PROPERTY <name>` under `<name>Spec == Spec /\ WF_vars(…) /\ SF_vars(…)`, the
/// module [`Liveness::to_tla`] emits. `ty` must prove it at the committed config
/// over the same number of states the interpreter walked, and must report a
/// TEMPORAL violation for each mutant's isolated model; a disagreement panics.
///
/// Scalar-only (asserted), so like the `_scalar` forms it returns a [`Covered`]
/// outright.
// Skip: a tiered verification DRIVER — shells out to `ty`, renders output, and
// its asserts are deliberate harness aborts. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn liveness_proves_and_catches_tiered(m: &Model, live: &Liveness, label: &str) -> Covered {
    assert_scalar(m, label, "liveness_proves_and_catches_tiered");
    // The committed config's space is walked once, for the audit and for what
    // the `ty` comparison below needs of it.
    let committed = interp::with_buggy(m, 0);
    let ty = find_ty();
    // Every `ty` check here is a single-worker process of its own (see
    // [`arm_whole_space_check`]) and none reads another's result, so they run
    // side by side: the committed one beside the interpreter's audit, each
    // mutant's beside the others once the audit has named the dead actions.
    // Their verdicts are then judged in the order they always were.
    let (space, committed_check, mutant_checks) = std::thread::scope(|scope| {
        let committed_check = ty.as_deref().map(|ty| {
            let (module, cfg) = (
                live.to_tla(&committed),
                live.to_cfg_with(m, &[("Buggy", 0)]),
            );
            scope.spawn(move || ty_check_liveness(ty, m, &module, &cfg, "committed", label))
        });
        let (caught, space) = match liveness_preflight(m, live).and_then(|()| {
            let space = interp::Space::of(&committed);
            audit_liveness_on(m, live, &space).map(|caught| (caught, space))
        }) {
            Ok(audited) => audited,
            Err(why) => panic!("{label}: {why}"),
        };
        eprintln!(
            "{label}: {} `{}` proven under its fairness (Buggy=0) and broken by {caught} \
             mutant(s) alone (interpreter).",
            m.name, live.name
        );
        let mutant_checks: Vec<_> = match ty.as_deref() {
            Some(ty) => live
                .mutants
                .iter()
                .map(|mutant| {
                    let isolated = isolate_mutant(m, space.fired(), mutant);
                    let (module, cfg) =
                        (live.to_tla(&isolated), live.to_cfg_with(m, &[("Buggy", 1)]));
                    let check =
                        scope.spawn(move || ty_check_liveness(ty, m, &module, &cfg, mutant, label));
                    (mutant, check)
                })
                .collect(),
            None => Vec::new(),
        };
        let join = |check: std::thread::ScopedJoinHandle<'_, (bool, String)>| {
            check
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        };
        (
            space,
            committed_check.map(join),
            // A mutant's panic is held, not raised here: the committed
            // verdict below is judged first, as it was before the checks ran
            // side by side.
            mutant_checks
                .into_iter()
                .map(|(mutant, check)| (mutant, check.join()))
                .collect::<Vec<_>>(),
        )
    });
    let Some((ok, evidence)) = committed_check else {
        return Covered::Interpreter;
    };
    assert!(
        ok,
        "{label}: ty says {} `{}` FAILS at Buggy=0 — TIER DISAGREEMENT (the interpreter \
         proved it)\n{evidence}\n--- generated ---\n{}",
        m.name,
        live.name,
        live.to_tla(&committed)
    );
    let reachable = space.states();
    assert_same_space_explored(m, reachable, &evidence, label);
    for (mutant, check) in mutant_checks {
        let (held, evidence) = check.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        assert!(
            !held,
            "{label}: ty found NO violation of {} `{}` with mutant `{mutant}` alone — TIER \
             DISAGREEMENT (the interpreter found a fair non-progress cycle)\n{evidence}",
            m.name, live.name
        );
        assert!(
            evidence.contains("Temporal properties were violated")
                || evidence.contains("Liveness violation"),
            "{label}: ty rejected {} with mutant `{mutant}` alone, but not with a TEMPORAL \
             violation — a parse or config failure is not a catch\n{evidence}",
            m.name
        );
    }
    eprintln!(
        "{label}: {} `{}` additionally proven (Buggy=0, {reachable} states) and broken by \
         each mutant by ty.",
        m.name, live.name
    );
    Covered::InterpreterAndTy
}

/// TIERED per-transition conformance (the Tier-1 `ty trace validate` twin):
/// does the model's `Next` admit the real `prev -> next` step?
///
/// The interpreter tier ALWAYS answers ([`interp::admits`]); the `ty` tier
/// additionally validates a two-step JSON trace `[prev, next]` against
/// `transition_spec()` wherever installed, and the verdicts MUST agree
/// (disagreement panics). That spec's `Init` is open (`TRUE`), so step 0 is
/// whatever `prev` says and the verdict is `Next`'s alone on `prev -> next`;
/// its cfg depends only on the model and `overrides`, never on `prev`.
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

    // ty tier: two-step trace against the open-Init transition spec.
    let spec_text = m.transition_spec();
    let cfg_text = m.transition_cfg(overrides);
    let trace_text = transition_trace_json(m, prev, next, action.or(admitted));
    let (ty_ok, combined) = ty_trace_verdict(&ty, m.name, spec_text, cfg_text, trace_text, label);
    assert!(
        ty_ok == interp_ok,
        "{label}: TIER DISAGREEMENT on {} transition {prev:?} -> {next:?}: \
         interpreter={interp_ok} ty={ty_ok} (checker bug — never swallow this)\n\
         {interp_why}\n--- ty ---\n{combined}",
        m.name
    );
    (ty_ok, combined)
}

/// `ty trace validate`'s verdict on these exact three files, asked once per
/// process of each `ty` binary. `ty` is deterministic, so one checker given the
/// same spec, cfg and trace always gives the same answer — and an exhaustive
/// Tier-1 walk that replays each path from its start hands it the same
/// transition over and over. Measured on aterm-agent's
/// conformance_footer_model: 466 s with `ty` installed, 50 s without, the
/// difference a serialised `ty` process per transition. Every distinct
/// transition is still validated by `ty`; the caller still asserts the two
/// tiers agree on every call. Asked of the `--cases` co-process where `ty` has
/// one ([`cases_verdict`]), else of one `ty` process ([`ty_trace_per_process`]).
/// The memo and the pool are both keyed on the checker ([`memoized_verdict`]).
// Skip: shells out to `ty`; verification tooling, not shipping runtime code.
#[cfg_attr(trust_verify, trust::skip)]
fn ty_trace_verdict(
    ty: &Path,
    model: &str,
    spec_text: String,
    cfg_text: String,
    trace_text: String,
    label: &str,
) -> (bool, String) {
    memoized_verdict(
        ty,
        spec_text,
        cfg_text,
        trace_text,
        |checker, spec, cfg, trace| {
            // A10: a `ty` advertising `trace-cases-v1` answers from one co-process
            // per (checker, spec, cfg); anything that co-process fails to answer is
            // answered here by one process for this case.
            checker
                .and_then(|checker| cases_verdict(checker, model, spec, cfg, trace, label))
                .unwrap_or_else(|| ty_trace_per_process(ty, model, spec, cfg, trace, label))
        },
    )
}

/// The `ty` that answers: the path [`find_ty`] gave and the [`BinaryIdentity`]
/// of what running it executes — the file itself and, through a wrapper script
/// whose exec target reads statically, the binary at the end of that chain. So
/// a PATH wrapper whose target alone was rebuilt or replaced is another
/// checker, as a binary rewritten in place is.
type TyChecker = (PathBuf, BinaryIdentity);

/// [`ty_trace_verdict`]'s memo: a verdict is replayed only for the same
/// rendered bytes (never the model name, so a `Buggy` twin or a re-bounded copy
/// that shares a name cannot borrow one) asked of THE SAME CHECKER — the path
/// `ty` and the identity of what it runs ([`TyChecker`]). `run` answers
/// anything else; it is handed the checker (`None` when `ty` cannot be stat'ed)
/// so the co-process pool keys on the same one.
///
/// WHY the checker: an `aterm pkg` update, link or yank moves [`find_ty`] to
/// another path, and a `ty` rebuilt or reinstalled in place keeps its path but
/// not its identity. A key without the checker would replay one build's verdict
/// for a step that should be asked of the next, so a shim repointed at a
/// REJECTING build would never raise its TIER DISAGREEMENT. A binary
/// that cannot be stat'ed, or whose identity moved while `run` ran, is answered
/// but never memoized: which build gave that answer is not known.
// Skip: a Mutex-guarded map plus fs metadata (absent std bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn memoized_verdict(
    ty: &Path,
    spec_text: String,
    cfg_text: String,
    trace_text: String,
    run: impl FnOnce(Option<&TyChecker>, &str, &str, &str) -> (bool, String),
) -> (bool, String) {
    type Verdicts = BTreeMap<(TyChecker, String), BTreeMap<(String, String), (bool, String)>>;
    static SEEN: Mutex<Verdicts> = Mutex::new(BTreeMap::new());
    let Some(identity) = binary_identity(ty) else {
        return run(None, &spec_text, &cfg_text, &trace_text);
    };
    let question = ((ty.to_path_buf(), identity), spec_text);
    let step = (cfg_text, trace_text);
    let seen = SEEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&question)
        .and_then(|by_step| by_step.get(&step).cloned());
    if let Some(verdict) = seen {
        return verdict;
    }
    let verdict = run(Some(&question.0), &question.1, &step.0, &step.1);
    if binary_identity(ty).as_ref() == Some(&question.0.1) {
        SEEN.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(question)
            .or_default()
            .insert(step, verdict.clone());
    }
    verdict
}

/// One `ty trace validate` process for one case: the path every `ty` takes,
/// and the co-process pool's fallback for any case it did not answer.
// Skip: shells out to `ty`; verification tooling, not shipping runtime code.
#[cfg_attr(trust_verify, trust::skip)]
fn ty_trace_per_process(
    ty: &Path,
    model: &str,
    spec_text: &str,
    cfg_text: &str,
    trace_text: &str,
    label: &str,
) -> (bool, String) {
    // Unique PER CALL (see `scratch_dir`): two tests in one process validating
    // the same model run concurrently.
    let dir = scratch_dir("aterm-conf", model);
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{model}.tla"));
    let cfg = dir.join(format!("{model}.cfg"));
    let trace = dir.join("t.json");
    std::fs::write(&spec, spec_text).expect("write spec");
    std::fs::write(&cfg, cfg_text).expect("write cfg");
    std::fs::write(&trace, trace_text).expect("write trace");
    // Routed like every other `ty` spawn (see `ty_output`). `trace validate`
    // walks a two-state trace, not a state space, so it takes none of the
    // whole-space arming; the routing keeps one place deciding what a `ty`
    // transcript may be credited as.
    let mut cmd = Command::new(ty);
    cmd.arg("trace")
        .arg("validate")
        .arg(&trace)
        .arg("--spec")
        .arg(&spec)
        .arg("--config")
        .arg(&cfg);
    let out =
        ty_output(&mut cmd).unwrap_or_else(|e| panic!("run ty trace validate for {label}: {e}"));
    let verdict = (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    );
    let _ = std::fs::remove_dir_all(&dir);
    verdict
}

/// How long a `--cases` co-process may take to write its header or one
/// verdict: a hang detector, not a latency budget (a case takes well under a
/// millisecond).
const CASES_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// Live co-processes at once; the least recently used is dropped first.
const CASES_POOL_CAP: usize = 16;

/// Co-process failures one (checker, spec, cfg) may have — the first spawn and
/// one respawn — before it is answered per process for the rest of the process.
const CASES_TRIES: u8 = 2;

/// A pool slot: `None` until its child is spawned, and again once it is retired.
type CasesSlot = std::sync::Arc<Mutex<Option<CasesChild>>>;

/// The `--cases` co-process's verdict on one two-step trace, or `None` — and
/// the caller answers this case with one `ty` process of its own.
///
/// WHY (A10 of docs/design/TY-TRACE-CASES-AND-FAIL-CLOSED-2026-10-01.md):
/// one `ty trace validate` process costs ~6 ms of spawn and setup for
/// microseconds of work, and the footer suite asks for 8,064 of them.
/// `ty trace validate --spec S --config C --cases -` loads the spec and cfg
/// once, then answers one request line with one verdict line, each case in a
/// fresh evaluation context; ty's own differential test holds it equal to one
/// process per case, and the caller's per-call TIER DISAGREEMENT assert still
/// holds every verdict against the interpreter.
///
/// Used only when `ty` advertises `trace-cases-v1` AND `check-budget-self-v1`
/// (an idle co-process must not shrink a running check's memory budget). It
/// fails CLOSED: any failure — no spawn, a missing or foreign header, EOF, a
/// malformed or out-of-order line, a timeout — kills the child (SIGKILL),
/// drops it, prints one notice with its stderr tail and returns `None`; after
/// [`CASES_TRIES`] failures the (checker, spec, cfg) stays per-process. A verdict
/// marked `end` (ty's first-seen-table tripwire fired) is a valid answer whose
/// child is then retired, not a failure. A request with no verdict line is
/// NEVER an accept.
///
/// A child answers only for the checker it was spawned from: the pool and the
/// failure counts are keyed on it as well as on (spec, cfg), so after an
/// `aterm pkg` update, or a `ty` rebuilt in place, the next case spawns a child
/// of the new build instead of asking the old build's live one. The old child
/// idles until it is evicted.
// Skip: drives a co-process; verification tooling, not shipping runtime code.
#[cfg_attr(trust_verify, trust::skip)]
fn cases_verdict(
    checker: &TyChecker,
    model: &str,
    spec: &str,
    cfg: &str,
    trace: &str,
    label: &str,
) -> Option<(bool, String)> {
    use std::sync::PoisonError;
    type Failures = BTreeMap<TyChecker, BTreeMap<String, BTreeMap<String, u8>>>;
    static POOL: Mutex<Vec<(TyChecker, String, String, CasesSlot)>> = Mutex::new(Vec::new());
    static FAILED: Mutex<Failures> = Mutex::new(BTreeMap::new());
    let ty = checker.0.as_path();
    let failures = || {
        FAILED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(checker)
            .and_then(|by_spec| by_spec.get(spec))
            .and_then(|by_cfg| by_cfg.get(cfg).copied())
            .unwrap_or(0)
    };
    let caps = ty_caps(ty);
    let has = |cap: &str| caps.iter().any(|c| c == cap);
    // One request is one line; `transition_trace_json` writes no newline.
    if !(has(CAP_TRACE_CASES) && has(CAP_BUDGET_SELF))
        || trace.contains('\n')
        || failures() >= CASES_TRIES
    {
        return None;
    }
    let slot = {
        let mut pool = POOL.lock().unwrap_or_else(PoisonError::into_inner);
        let at = pool
            .iter()
            .position(|(k, s, c, _)| k == checker && s == spec && c == cfg);
        let entry = match at {
            Some(at) => pool.remove(at),
            None => {
                if pool.len() >= CASES_POOL_CAP {
                    // Its child is killed once no case holds it any more.
                    pool.remove(0);
                }
                (
                    checker.clone(),
                    spec.to_string(),
                    cfg.to_string(),
                    CasesSlot::default(),
                )
            }
        };
        let slot = std::sync::Arc::clone(&entry.3);
        pool.push(entry);
        slot
    };
    let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
    let answer = match held.as_mut() {
        Some(child) => child.case(trace),
        // A thread that held this slot first may have spent the last try.
        None if failures() >= CASES_TRIES => return None,
        None => CasesChild::start(ty, model, spec, cfg, trace)
            .and_then(|child| held.insert(child).case(trace)),
    };
    match answer {
        Ok(answer) => {
            if answer.end {
                *held = None;
            }
            drop(held);
            // A12: `class: "error"` says the question could not be
            // answered as asked (an evaluation failed, a label names no
            // action) — not that `Next` refutes the step. Read as a refusal it
            // would agree with an interpreter that refuses and pass. Enabled
            // after the qualification run: 0 of 8,928 co-process verdicts
            // (aterm-agent's suite, aterm-gui's lib) were errors.
            assert!(
                !answer.unevaluated,
                "{label}: ty COULD NOT EVALUATE this `{model}` transition — an evaluation \
                 error is not a refutation, so neither tier's verdict stands\n{}",
                answer.evidence
            );
            Some((answer.ok, answer.evidence))
        }
        Err(why) => {
            *held = None;
            drop(held);
            let tries = {
                let mut failed = FAILED.lock().unwrap_or_else(PoisonError::into_inner);
                let n = failed
                    .entry(checker.clone())
                    .or_default()
                    .entry(spec.to_string())
                    .or_default()
                    .entry(cfg.to_string())
                    .or_default();
                *n += 1;
                *n
            };
            let then = if tries >= CASES_TRIES {
                "so is every later case of this spec and cfg in this process"
            } else {
                "the next case respawns the co-process once"
            };
            eprintln!(
                "{label}: ty --cases co-process for `{model}` failed, so this case is answered by \
                 one `ty` process of its own and {then}: {why}"
            );
            None
        }
    }
}

/// One verdict line, read.
struct CasesAnswer {
    ok: bool,
    /// `ty (cases) <exe> [<build stamp>] pid N` and what the verdict says.
    evidence: String,
    /// ty's last verdict (`"end"`): the child exits after it.
    end: bool,
    /// `class: "error"`: not answered as asked (A12).
    unevaluated: bool,
}

/// One live `ty trace validate --cases -` co-process for one (spec, cfg).
struct CasesChild {
    child: aterm_uds::spawnfd::Child,
    stdin: std::process::ChildStdin,
    /// The child's stdout lines, forwarded by its reader thread.
    lines: std::sync::mpsc::Receiver<String>,
    /// A read handle on the child's stderr FILE (its path is removed once the
    /// header is in), for a failure notice's tail.
    stderr: std::fs::File,
    /// `ty (cases) <exe> [<build stamp>] pid N`.
    who: String,
    next_id: u64,
}

impl CasesChild {
    /// Spawn the co-process and read its header, which must name `model` and
    /// the variables `trace` observes; `Err` describes any failure, the child
    /// already killed.
    // Skip: spawns a subprocess. Verification tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    fn start(ty: &Path, model: &str, spec: &str, cfg: &str, trace: &str) -> Result<Self, String> {
        let dir = scratch_dir("aterm-cases", model);
        let started = Self::spawn(ty, model, spec, cfg, &dir).and_then(|mut child| {
            match child.expect_header(model, trace) {
                Ok(()) => Ok(child),
                Err(why) => Err(child.failure(&why)),
            }
        });
        // The header is written once setup has loaded the spec and cfg; the
        // stderr file stays readable through the handle.
        let _ = std::fs::remove_dir_all(&dir);
        started
    }

    // Skip: spawns a subprocess and its reader thread. Verification tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    fn spawn(ty: &Path, model: &str, spec: &str, cfg: &str, dir: &Path) -> Result<Self, String> {
        let io = |e: std::io::Error| format!("ty (cases) {}: {e}", ty.display());
        std::fs::create_dir_all(dir).map_err(io)?;
        let spec_path = dir.join(format!("{model}.tla"));
        let cfg_path = dir.join(format!("{model}.cfg"));
        let stderr_path = dir.join("stderr");
        std::fs::write(&spec_path, spec).map_err(io)?;
        std::fs::write(&cfg_path, cfg).map_err(io)?;
        let stderr = std::fs::File::create(&stderr_path).map_err(io)?;
        let stderr_reader = std::fs::File::open(&stderr_path).map_err(io)?;
        let mut cmd = aterm_uds::spawnfd::command(ty);
        cmd.arg("trace")
            .arg("validate")
            .arg("--spec")
            .arg(&spec_path)
            .arg("--config")
            .arg(&cfg_path)
            .arg("--cases")
            .arg("-")
            .stderr(stderr);
        let mut child = ty_child(&mut cmd).map_err(io)?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("ty (cases) {}: no stdio pipes", ty.display()));
        };
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead as _;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            who: format!(
                "ty (cases) {} [{}] pid {}",
                ty.display(),
                ty_build_stamp(ty),
                child.id()
            ),
            child,
            stdin,
            lines,
            stderr: stderr_reader,
            next_id: 0,
        })
    }

    /// The next stdout line, within [`CASES_HANG`].
    fn recv(&self, what: &str) -> Result<String, String> {
        use std::sync::mpsc::RecvTimeoutError;
        self.lines.recv_timeout(CASES_HANG).map_err(|e| match e {
            RecvTimeoutError::Timeout => format!("no {what} within {} s", CASES_HANG.as_secs()),
            RecvTimeoutError::Disconnected => format!("stdout closed before {what}"),
        })
    }

    // Skip: JSON field reads over another tool's output. Verification tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    fn expect_header(&self, model: &str, trace: &str) -> Result<(), String> {
        use aterm_json::Value;
        let line = self.recv("its header")?;
        let names = |doc: &Value| -> Option<Vec<String>> {
            let mut names = doc
                .get("variables")?
                .as_array()?
                .iter()
                .map(|n| n.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()?;
            names.sort();
            Some(names)
        };
        let want = aterm_json::from_str::<Value>(trace)
            .ok()
            .and_then(|t| names(&t));
        let fits = aterm_json::from_str::<Value>(&line).is_ok_and(|h| {
            h.get("ty_trace_cases").and_then(Value::as_u64) == Some(1)
                && h.get("module").and_then(Value::as_str) == Some(model)
                && h.get("action_label_mode").and_then(Value::as_str) == Some("error")
                && want.is_some()
                && names(&h) == want
        });
        if fits {
            Ok(())
        } else {
            Err(format!("a header that is not `{model}`'s: {line}"))
        }
    }

    /// One request, one verdict line, read strictly: the id must echo the
    /// request's, and `ok` must be a boolean.
    // Skip: pipe I/O and JSON field reads. Verification tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    fn case(&mut self, trace: &str) -> Result<CasesAnswer, String> {
        use aterm_json::Value;
        use std::io::Write as _;
        let id = self.next_id;
        self.next_id += 1;
        writeln!(self.stdin, "{{\"id\":{id},\"trace\":{trace}}}")
            .and_then(|()| self.stdin.flush())
            .map_err(|e| self.failure(&format!("request {id} could not be written: {e}")))?;
        let line = self
            .recv(&format!("the verdict for request {id}"))
            .map_err(|why| self.failure(&why))?;
        let verdict = aterm_json::from_str::<Value>(&line).ok();
        let field = |key: &str| verdict.as_ref().and_then(|v| v.get(key));
        let ok = field("ok").and_then(Value::as_bool);
        let class = field("class").and_then(Value::as_str);
        let (Some(ok), true) = (ok, field("id").and_then(Value::as_u64) == Some(id)) else {
            return Err(self.failure(&format!("not the verdict for request {id}: {line}")));
        };
        if !ok && !matches!(class, Some("refuted" | "error")) {
            return Err(self.failure(&format!("a refusal with no class: {line}")));
        }
        let text = |key: &str| field(key).and_then(Value::as_str).unwrap_or("?");
        let mut evidence = if ok {
            let steps = field("steps").and_then(Value::as_u64).unwrap_or(0);
            format!("{}\naccepted: {steps} steps", self.who)
        } else {
            let step = field("step")
                .and_then(Value::as_u64)
                .map_or_else(|| "null".to_string(), |s| s.to_string());
            format!(
                "{}\n{} at step {step}: {}",
                self.who,
                text("kind"),
                text("message")
            )
        };
        let warnings = field("warnings").and_then(Value::as_array);
        for warning in warnings.into_iter().flatten() {
            evidence.push_str(&format!("\nwarning: {}", warning.as_str().unwrap_or("?")));
        }
        Ok(CasesAnswer {
            ok,
            evidence,
            end: field("end").is_some(),
            unevaluated: class == Some("error"),
        })
    }

    /// Kill the child — SIGKILL through `Child::kill`, never a signal that
    /// writes a crash report (AGENTS.md), and harmless on one that has
    /// exited — and describe the failure with its exit status and the tail
    /// of its stderr.
    // Skip: process control and a file read. Verification tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    fn failure(&mut self, why: &str) -> String {
        use std::io::Read as _;
        let _ = self.child.kill();
        let status = self
            .child
            .wait()
            .map_or_else(|e| e.to_string(), |s| s.to_string());
        let mut stderr = String::new();
        let _ = self.stderr.read_to_string(&mut stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        let tail = lines[lines.len().saturating_sub(20)..].join("\n");
        format!(
            "{}: {why} ({status})\n--- ty stderr (tail) ---\n{tail}",
            self.who
        )
    }
}

impl Drop for CasesChild {
    /// A retired, evicted or failed child is killed and reaped, never left a
    /// zombie. A child still pooled when the test process exits sees EOF on
    /// its stdin and exits by itself.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The two-step `ty trace validate` JSON for a derived model: `prev` at index 0,
/// `next` (+ the admitting action, when known) at index 1. Variables serialize
/// in the model's declared order.
///
/// # Panics
///
/// If `prev` or `next` lacks a model variable. A projection that misses one is
/// a harness bug, and no value invented for it here is a fact about the code.
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
                let Some(value) = s.get(v) else {
                    panic!(
                        "{}: the transition state {s:?} has no `{v}` — project every model \
                         variable",
                        m.name
                    )
                };
                format!("\"{v}\":{{\"type\":\"int\",\"value\":{value}}}")
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
///
/// **One walk, not one per invariant.** "Checked alone" is computed by
/// [`interp::violated_invariants`], a single BFS of the `Buggy = 1` space that
/// returns exactly the set per-invariant isolated `bmc` runs would, panics
/// included (its doc carries the argument; the per-invariant form survives as
/// the test oracle it is compared against).
#[must_use]
pub fn uncaught_invariants(m: &Model) -> Vec<&'static str> {
    let violated = interp::violated_invariants(&interp::with_buggy(m, 1));
    m.invariants
        .iter()
        .map(|inv| inv.name)
        .filter(|name| !violated.contains(name))
        .collect()
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;

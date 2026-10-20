// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The shared verification helpers for aterm's derived models and their conformance tests, in
//! two tiers with one switch between them.
//!
//! ## The in-process interpreter, always
//!
//! [`crate::interp`] discharges every derived-model obligation (Tier-0 invariants, prove-and-catch
//! non-vacuity, per-transition conformance) by an exhaustive BFS of the bounded model: a real check
//! that fails loudly, never a skip. It is all a plain `targo test` runs; nothing here looks for an
//! external checker unless asked, so a test's verdict is the same on every machine.
//!
//! ## The Trust checkers, when asked
//!
//! `ty` and `ay` run only when the process asks for them: `spec-check` ([`crate::check`]) calls
//! [`require_checkers`], and a test built with aterm-spec's `checkers` feature runs the obligations
//! inside it ([`checkers_required`]). Asked for, a missing checker fails the obligation instead of
//! reading as absent. `ty` checks the same derived model and must agree with the interpreter on the
//! verdict and the explored state count. The tool-only analyses (`--strict-vacuity`, SMT
//! certificates) live in `spec-check`.
//!
//! ## Discovery
//!
//! `ty` and `ay` are atpkg programs (`aterm pkg install --default-set`, or one at a time), never
//! built from source here. Discovery ([`locate_ty`], [`locate_ay`]) looks in the atpkg store's shim
//! (`<prefix>/bin/<tool>`), then on `PATH`, and nowhere else: a stale checkout would shadow the
//! store's build. A developer's own build goes through the shim (`aterm pkg link <tool>
//! <checkout>`). The store is under this account's home (`getpwuid_r`, never `$HOME`, which a test
//! process moves); `PATH`, and on Windows `%HOME%`/`%USERPROFILE%` and `%LOCALAPPDATA%`, are all
//! the environment discovery reads.
//!
//! ## The tier names the binary it ran
//!
//! Discovery writes one line straight to stderr (libtest swallows `eprintln!` from a passing test)
//! the first time it resolves each tool in a process, and again whenever that build changes:
//!
//! ```text
//! VERIFY ESCALATION TIER: ty = ~/Library/Application Support/aterm/pkg/store/ty/3007/bin/ty [atpkg managed store; ty 0.15.0; built 2026-09-24T17:02:11Z; 157309456 bytes] — the managed store also holds store/trust/9192/bin/ty (built 2026-09-19); caps=none
//! ```
//!
//! `built` (the mtime, UTC) and `bytes` tell builds apart where `--version` does not; `caps=` is
//! what `ty capabilities --json` advertised.
//!
//! The build is the chosen path plus its `BinaryIdentity` (length, mtime, device, inode, ctime),
//! followed through a wrapper script to the file it `exec`s. So a shim an update repointed, a tool
//! rebuilt in place, a yank, or a `PATH` wrapper whose target alone was replaced prints a fresh
//! line ending `changed mid-process (was …)`. A wrapper whose target does not read statically is
//! the limit: its line says so, and its `built`/`bytes` are the script's own.
//!
//! Discovery resolves afresh on every call, so the first call after a change prints. `ty` verdicts
//! follow the same identity (`memoized_verdict`): a step is answered by the build the last line
//! names. The cost is one `--version` spawn per printed line (plus one `capabilities` spawn per
//! `ty` build, each bounded by `PROBE_HANG`), and per discovery a few `stat`s and small head reads;
//! store copies are described from the filesystem, never run. [`locate_ty`]/[`locate_ay`] return
//! the answer as a [`Located`].
//!
//! ## Caller idioms
//!
//! ```no_run
//! use std::collections::BTreeMap;
//! use aterm_spec::{derive::Model, verify::{self, Covered, NotRun}};
//!
//! // SCALAR derived model — the common case. The interpreter always runs, so
//! // coverage is unconditional and the returned Covered is a log line.
//! fn check_scalar(m: &Model) -> Covered {
//!     verify::prove_and_catch_scalar(m, "Thing non-vacuity")
//! }
//!
//! // FUNCTION-VALUED derived model — only `ty` can walk it, so outside a
//! // checker run the obligation does NOT run, and the `_tiered` forms return
//! // `Result<Covered, NotRun>` so the caller must state a policy (a registered
//! // one is `spec-check models`'s). Propagate NotRun; it is not coverage.
//! fn check_function_valued(m: &Model) -> Result<Covered, NotRun> {
//!     verify::prove_and_catch_tiered(m, "Thing Tier-0")
//! }
//!
//! fn check_transition(
//!     m: &Model,
//!     overrides: &[(&'static str, i64)],
//!     prev: &BTreeMap<&'static str, i64>,
//!     next: &BTreeMap<&'static str, i64>,
//! ) -> (bool, String) {
//!     verify::validate_transition_tiered(
//!         m, overrides, prev, next, Some("Push"), "Thing conformance",
//!     )
//! }
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use crate::derive::{Liveness, Model};
use crate::interp;

/// Discover the Trust `ty` model checker: the atpkg store's shim, then `ty` on `PATH`
/// ([`locate_ty`] says which answered). The first discovery in a process, and each later one
/// that resolves to a different build, prints one `VERIFY ESCALATION TIER: ty = …` line (see the
/// module doc).
///
/// Never cached: an `aterm pkg` update, link or yank rewrites only the shim and leaves the old
/// build on disk, so a cache would keep a superseded or yanked build (and an old build's accept
/// would hide a tier disagreement). A call is a few `stat`s, a shim read and a `PATH` walk, about
/// 65 µs.
///
/// `None` unless this process runs the external checkers ([`checkers_required`]); with them on, a
/// missing `ty` panics instead of reading as absent.
#[must_use]
pub fn find_ty() -> Option<PathBuf> {
    checker("ty", locate_ty)
}

/// Discover `ty` and where it came from, whatever [`checkers_required`] says: what `spec-check`
/// asks before it turns the checkers on.
#[must_use]
pub fn locate_ty() -> Option<Located> {
    discover("ty")
}

/// [`locate_ty`] for `ay`.
#[must_use]
pub fn locate_ay() -> Option<Located> {
    discover("ay")
}

/// Whether this process runs the external checkers; decided once.
static CHECKERS_REQUIRED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Turn the external checkers on for this process: from here every `ty` and
/// `ay` obligation runs, and a missing checker fails it. `spec-check` calls
/// this before anything else.
///
/// # Panics
///
/// If this process already decided to run without them (an obligation asked
/// first), since its earlier answers would not have been checked.
pub fn require_checkers() {
    let _ = CHECKERS_REQUIRED.set(true);
    assert!(
        checkers_required(),
        "require_checkers() after this process already ran without the external checkers"
    );
}

/// Whether this process runs the external checkers: only when asked, by [`require_checkers`] or a
/// build with aterm-spec's `checkers` feature. Off (every plain `targo test`), each derived-model
/// obligation is the interpreter's alone; on, every tiered helper also runs `ty`, and a
/// disagreement fails.
///
/// Every Trust checker, on purpose (each fails closed on a missing tool):
///
/// ```text
/// targo --unverified run -q -p aterm-models --bin spec-check
/// targo --unverified test --workspace --features aterm-spec/checkers
/// ```
///
/// The first holds the registry to `ty` and the `ay` bundles to their declared verdicts; the
/// second re-runs every test that discharges a model through a tiered helper, with `ty` agreeing.
#[must_use]
pub fn checkers_required() -> bool {
    *CHECKERS_REQUIRED.get_or_init(|| cfg!(feature = "checkers"))
}

/// `bin`, when the external checkers are on: discovered, or a panic that says how to install it.
/// `None` when they are off.
// Audited contract panic (T9): the fail-closed gate a requested checker run rests on; it masks no
// other panic.
#[cfg_attr(
    trust_verify,
    trust::contract_panic(message_contains = "VERIFICATION GATE")
)]
#[cfg_attr(trust_verify, trust::skip)]
fn checker(bin: &str, locate: fn() -> Option<Located>) -> Option<PathBuf> {
    if !checkers_required() {
        return None;
    }
    Some(locate().map(|l| l.path).unwrap_or_else(|| {
        let (state, tail) = missing_tool_tail(bin);
        panic!(
            "VERIFICATION GATE: the external checkers were asked for, and Trust `{bin}` {state}.{tail}"
        )
    }))
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

/// The discovery precedence: the atpkg store shim, then `<bin>` on `PATH`, with the platform
/// executable name. The store's `bin/` and the `PATH` value are passed in so the order is
/// testable. The store comes first because atpkg's `bin/` reaches `PATH` only in interactive
/// shells, never in a test process.
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

/// The atpkg store's `bin/` directory. The prefix mirrors `atpkg::platform::default_prefix` rather
/// than depending on atpkg, which would pull ring, tar and zstd into every conformance consumer;
/// change both together. The `~` is the account's home on Unix ([`home_dirs`]), the same for every
/// thread.
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

/// The file a store shim forwards to, when it forwards to a real one: through a symlink, or the
/// `#!/bin/sh` exec stub atpkg lays (`targo` refuses a symlinked `current_exe`, so the stub `exec`s
/// the store file at its real path). The stub is read by the rule of atpkg's
/// `platform::parse_sh_shim_target`, which every store answer (`which`, gc, `prune_stale_shims`)
/// uses.
///
/// A tombstone (a yanked or below-floor build's failing notice script) has no `exec` line, and the
/// pending-program stub `exec`s `"$ATPKG"`, never a quoted path: neither parses, so neither
/// satisfies discovery or shadows a working tool on `PATH`. Anything that is not a symlink or a
/// regular file of at most 64 KiB is absent too.
///
/// The answer is the resolved target, an absolute path to a regular file (a GC'd build does not
/// satisfy). It is not anchored to `<prefix>/store`: `aterm pkg link` points a shim at a
/// checkout, and that is the tool the developer meant.
///
/// Public for atpkg's `aterm_spec_discovery_reads_every_shim_atpkg_lays`, which lays each shim
/// shape with atpkg's real writers and reads it back here.
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

/// The target an atpkg `sh` exec stub forwards to: the first trimmed line `exec '<path>' "$@"`,
/// with `'\''` unquoted. Mirrors `atpkg::platform::parse_sh_shim_target` (change both together).
/// `None` for a tombstone or a pending stub.
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
/// On Unix, this account's home ([`account_home`]), never the process's `$HOME`: test fixtures
/// point `$HOME` at a scratch directory, and discovery that read it would miss the store and take
/// whatever `PATH` offers, or nothing. `$HOME` answers only when the account cannot be resolved.
///
/// Windows: `%HOME%`, then `%USERPROFILE%`. Git Bash may set `HOME` as a POSIX-style `/c/…` path,
/// which Win32 cannot resolve, so it is normalized.
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

/// This account's home as the OS knows it (`getpwuid_r(getuid())->pw_dir`, which setting `$HOME`
/// cannot move), resolved once per process. Mirrors `atpkg::platform::unix::account_home`. `None`
/// when the lookup fails or names no absolute directory; a failure is not remembered.
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
    // SAFETY: `pw`, `buf` (of the stated length) and `out` are valid writable out-params, and
    // getpwuid_r writes nothing past buflen. `pw_dir` is read only on success (`out` non-null),
    // when it is a NUL-terminated string inside `buf`, copied out while `buf` lives.
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

/// `exe` on `path_var` (the `PATH` value, passed in so discovery is testable), skipping a pending
/// stub.
// Skip: `Path::is_file` is an fs syscall wrapper (absent body); every
// miss returns None (fail-closed). Build-tooling discovery.
#[cfg_attr(trust_verify, trust::skip)]
fn path_search_in(exe: &str, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_var?)
        .map(|dir| dir.join(exe))
        .find(|cand| cand.is_file() && !is_pending_stub(cand))
}

// ---------------------------------------------------------------------------
// Provenance: discovery names the binary it resolved (see the module doc).
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

/// Every live store copy of `exe`: per program in `store_root`, the file `current/bin/<exe>`
/// resolves to, unless it is missing or a pending stub. Nothing is spawned (a `stat` and
/// [`is_pending_stub`]'s bounded read). Sorted by program.
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
    /// The last link is a `#!` script whose exec target does not read statically: it computes
    /// its target (`"$(dirname "$0")/ty"`, `$HOME/…`, a bare name), `exec`s conditionally or
    /// twice, runs another command, launches through `env` or an interpreter, or passes arguments
    /// of its own. It is identified by its own bytes only, so replacing the binary behind it
    /// changes nothing here; the provenance line says so.
    ///
    /// A shebang-less script reads as [`ChainEnd::Binary`] (atpkg never writes one), as does a
    /// `.cmd` forwarder on Windows.
    OpaqueScript,
    /// The last link execs this path, which cannot be stat'ed.
    Dangling(PathBuf),
    /// More than [`MAX_EXEC_HOPS`] links, or a link that execs an earlier one.
    TooDeep,
}

/// The identity of what running a path executes: the file and, while it is a wrapper script whose
/// exec target reads statically, each file down the chain to the binary. A wrapper that keeps its
/// bytes while the binary it execs is replaced is a different identity.
///
/// It keys the provenance line, [`ty_caps`], and the `ty` verdict memo and `--cases` pool
/// ([`TyChecker`]), so those never name different builds.
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

/// How the second line of every `sh` shim atpkg writes begins (mirrors
/// `atpkg::platform::sh_shim_body`; change both together). Only a file with it is read by atpkg's
/// target rule, which takes the first `exec '<path>' "$@"` line anywhere: right for atpkg's shapes
/// (an `export` block, a route guard before the `exec`), wrong for a hand-written script that may
/// run something else first. Other scripts are held to [`literal_exec_target`].
const ATPKG_SHIM_HEADER: &str = "# atpkg shim";

/// The one file a hand-written forwarder script `exec`s, when the whole script reads statically:
/// after the `#!` line, every line is blank, a comment or a `set` of shell options, except the last
/// command, which is `exec <path>` or `exec <path> "$@"` with `<path>` a literal absolute path
/// (bare, `'…'`, or `"…"` with nothing to expand). Anything else (another command, a second or
/// conditional `exec`, a computed target, `exec env …`, extra arguments) is `None`: the script is
/// opaque ([`ChainEnd::OpaqueScript`]).
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

/// The leading shell word of `s` when it is a literal (`'…'`, `"…"` with no `$`, `` ` `` or `\`,
/// or bare with no shell metacharacter), and what follows it; `None` for anything the shell would
/// expand. No byte offsets, so no slice bound or `+ 1` for the Trust lane to discharge.
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
// Skip: the sealed prover (321aaeda7) runs out of the 120 s function budget on
// the whole body, its constant divisions included (71 of 71 obligations
// TIMEOUT, 2026-10-18; bounding the input changed nothing), and a permanent
// lapse lets the lane read any drop below aterm-spec's floor as
// NOT-VERIFIED(clock). Report date formatting; verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
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

/// How long a once-per-process probe may take before it is called wedged: a hang detector, not a
/// latency budget. The answer is cached for the process (an empty [`ty_caps`] set sends every case
/// to a process of its own), so a bound a loaded machine can exceed turns one slow first exec into
/// a permanent loss; a fresh script's first exec on macOS took over five seconds at load 80-100.
const PROBE_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// `<bin> <args>`'s output, or `None` if it cannot start or does not finish within
/// [`PROBE_HANG`], when it is killed. The bound both once-per-process probes share.
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

/// What the `ty` at `ty` says it guarantees (`ty capabilities --json`), probed once per build per
/// process.
///
/// Store builds of one `ty` release share a version string, so a protocol is detected by the
/// capability a build advertises, never by `--version`
/// (docs/design/TY-TRACE-CASES-AND-FAIL-CLOSED-2026-10-01.md A8). A `ty` without the command (exit
/// 2), like any other failure to answer, gives the empty set: the per-process behaviour every `ty`
/// supports. Keyed by path and [`BinaryIdentity`]: [`find_ty`] re-resolves on every call, and a
/// build at another path or rebuilt in place must not inherit its predecessor's answer, which the
/// provenance line prints as `caps=`.
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

/// The one-line provenance report, pure so the wording is testable without a spawn. `found` is
/// what discovery chose (`None`: nothing on any tier), `identity` what running it executes,
/// `version` its `--version`, `shadowed` the store copies other than the chosen binary, and `shim`
/// explains a miss. `built` and `bytes` describe the last file of the exec chain: the binary a
/// wrapper forwards to, not the wrapper.
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

/// How the chosen path reaches the binary it runs, as a ` (…)` note for the provenance line;
/// empty when the chosen path is that binary.
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
    // The file the chosen path runs (through every wrapper the identity followed), so that store
    // copy is not listed as shadowed.
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

/// What one provenance line describes: what discovery chose, and the identity of what it runs
/// (`None` inside when it could not be stat'ed). `None`: not found.
type ProvenanceKey = Option<(Located, Option<BinaryIdentity>)>;

/// The last line printed for one tool, by its [`ProvenanceKey`]; `None` until the first.
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

/// What a repeated line says it replaced when the path is the same: a file it runs was rewritten,
/// replaced, or only had its metadata changed (a `chmod` or a new hard link moves the ctime). It
/// never claims the bytes changed.
const SAME_PATH_CHANGED: &str =
    "this same path; a file it runs was since rewritten, replaced or had its metadata changed";

/// The suffix a repeated line carries: it replaces an earlier line of this
/// process, and says what that line named.
fn change_note(prev: &ProvenanceKey, now: Option<&Located>, home: Option<&Path>) -> String {
    let was = match (prev, now) {
        (None, _) => "NOT FOUND".to_owned(),
        (Some((p, _)), Some(n)) if p.path == n.path => SAME_PATH_CHANGED.to_owned(),
        (Some((p, _)), _) => tilde(&p.path, home),
    };
    format!(" — changed mid-process (was {was})")
}

/// Print the provenance line for `bin` when what discovery resolved differs from the last line
/// printed for it in this process: the first discovery, then every store update, yank, in-place
/// rebuild or wrapper retarget that changes the [`ProvenanceKey`]. Other discoveries stay silent
/// and spawn nothing.
///
/// Written straight to the stderr handle: libtest discards `eprintln!` from a passing test, and the
/// line must reach the terminal and `verify`'s run log on a green run.
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
            // A8: the line names the protocol that will answer (per-process for `caps=none`,
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

/// [`report_provenance_on_change`] over explicit state: `last` is the bookkeeping, `probe`
/// answers the chosen path's `--version` and the line's ending (`ty`'s `caps=` clause), `write`
/// prints.
///
/// The tool's [`LastLine`] stays locked until the line is written, so a tool's lines are written
/// in the order their keys were claimed and the last one written is the one [`LastLine`] holds.
/// Only a claimed line holds it long (its probes, bounded by [`PROBE_HANG`]), and only for that
/// tool: the map of tools is released before the claim.
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

/// For a missing-tool failure ([`missing_tool_tail`]): the managed-store copies of `bin` that exist
/// but that discovery did not reach, as a sentence; empty when there are none.
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

/// Whether this candidate is atpkg's pending-program stub rather than the tool.
///
/// A program the registry lists but has not installed still gets a file in the store's `bin/`: a
/// script that prints "not installed yet" and exits nonzero, so a shell explains itself. A harness
/// that ran it would read the refusal as the tool's output and fail hard. The stub names itself on
/// its second line; anything unreadable is a real binary, since a false "pending" would hide an
/// installed tool.
///
/// Only the head is read: the real tool is large (`ty` is about 157 MB in the store).
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

// ---------------------------------------------------------------------------
// The tiered derived-model discharges: the interpreter always, and `ty` too when the checkers run,
// on the same derived model; a disagreement panics.
// ---------------------------------------------------------------------------

/// How a tiered obligation was discharged; every variant is real coverage. The no-coverage case is
/// [`NotRun`], the `Err` half of the `_tiered` functions' return, so holding a `Covered` always
/// means the obligation ran.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Covered {
    /// Interpreter BMC only (the checkers are off), still a real exhaustive check.
    Interpreter,
    /// Interpreter BMC AND the external `ty` binary agreed.
    InterpreterAndTy,
    /// Function-valued model: interpreter inapplicable, `ty` checked it.
    TyOnly,
}

/// The obligation did not run: a function-valued model, which only `ty` can walk, in a process
/// that does not run the external checkers ([`checkers_required`]).
///
/// The `Err` half rather than a `Covered` variant because `Result` is `#[must_use]`: a call site
/// cannot drop it silently, and a scalar model that grows an `fn_vars` entry turns its call sites
/// into compile errors instead of a silent green. It is never a pass: `spec-check models` holds a
/// registered function-valued model under `ty`, so a test has nothing to add by discharging one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NotRun {
    /// The model whose obligation went unchecked (`Model::name`).
    pub model: &'static str,
}

/// Run `ty check` on a derived model's generated spec and `cfg` (overrides pre-rendered); panics
/// if `ty` cannot run. Returns whether it exited 0, and a transcript naming the binary and its
/// full output, so a tier-disagreement panic says which `ty` build said what.
// Skip: shells out to `ty` and renders its output. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn ty_check_derived(ty: &Path, m: &Model, cfg: &str, label: &str) -> (bool, String) {
    let dir = scratch_dir("aterm-tier", m.name);
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{}.tla", m.name));
    let cfgp = dir.join(format!("{}.cfg", m.name));
    std::fs::write(&spec, m.to_tla()).expect("write derived spec");
    std::fs::write(&cfgp, cfg).expect("write derived cfg");
    let out = TyCommand::check(ty, &spec, &cfgp)
        .output()
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

/// A fresh scratch directory for one checker call, `<temp>/<kind>-<model>-<pid>-<n>`. Per call,
/// not per `(model, pid)`: two tests checking one model at once would otherwise read or delete
/// each other's spec and cfg.
// Skip: formats a path from the process id and a counter (absent std format
// bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn scratch_dir(kind: &str, model: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "{kind}-{model}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// The flags every `ty check` carries, so the checker walks the whole bounded space the interpreter
/// walks.
const WHOLE_SPACE: [&str; 10] = [
    // Reduction off: a reduced run explores a representative subset, so its
    // state count says nothing about the model, and a `ty` whose reduction
    // lacked the cycle proviso proved a spec violated three steps from `Init`.
    "--no-auto-por",
    "--no-auto-symmetry",
    // These models are small: unhinted, `ty` pre-allocates for millions of
    // states (4.25 GB against 17 MB measured), which its own memory ceiling
    // then cuts short.
    "--initial-capacity",
    "8192",
    // The interpreted engine: codegen compiles every action first, ~30x slower
    // on spaces this size for the same verdict.
    "--backend",
    "interpreter",
    // The explicit-state lane alone: the fused symbolic lane costs ~0.55 s a
    // spawn and never decides one of these models.
    "--bfs-only",
    // One worker: the parallelism that pays is one `ty` per caller thread.
    "--workers",
    "1",
    // No cached verdicts: the cache is keyed by path, so a spec at a stable
    // path would replay a PASS it never re-derived.
    "--force",
];

/// One `ty` run, and the only way to start one here: [`TyCommand::check`] arms the whole space
/// ([`WHOLE_SPACE`]), and [`TyCommand::output`] gives `ty` a HOME of its own and fails closed on a
/// search a resource limit stopped, so no caller can forget either.
#[derive(Debug)]
pub struct TyCommand(Command);

impl TyCommand {
    /// `ty check <spec> --config <cfg>`, armed for the whole space.
    #[must_use]
    pub fn check(ty: &Path, spec: &Path, cfg: &Path) -> Self {
        let mut cmd = Command::new(ty);
        cmd.arg("check")
            .arg(spec)
            .arg("--config")
            .arg(cfg)
            .args(WHOLE_SPACE);
        Self(cmd)
    }

    /// `ty trace validate <trace> --spec <spec> --config <cfg>`: a two-state
    /// trace, not a state space, so it takes no arming.
    #[must_use]
    pub fn trace_validate(ty: &Path, trace: &Path, spec: &Path, cfg: &Path) -> Self {
        let mut cmd = Command::new(ty);
        cmd.arg("trace")
            .arg("validate")
            .arg(trace)
            .arg("--spec")
            .arg(spec)
            .arg("--config")
            .arg(cfg);
        Self(cmd)
    }

    /// One more argument (`--strict-vacuity`, `--require-exhaustive`, …).
    pub fn arg(&mut self, arg: impl AsRef<std::ffi::OsStr>) -> &mut Self {
        self.0.arg(arg);
        self
    }

    /// Run it, failing closed on a search that stopped early.
    ///
    /// `ty` sizes its memory budget by the live `ty`/`cargo`/`rustc` processes, and a search that
    /// budget stops still exits 0 under an `exhaustive` header, so an exit-status verdict would
    /// book it as a proof. Exit 5 or a `Model checking stopped:` line is therefore an `Err` naming
    /// INCOMPLETE, never an `Output` a caller could read as a verdict.
    ///
    /// # Errors
    ///
    /// When `ty` cannot be spawned, or its search was stopped.
    // Skip: spawns a subprocess and classifies its transcript. Verification tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn output(&mut self) -> std::io::Result<std::process::Output> {
        with_scratch_ty_home(&mut self.0);
        let out = self.0.output()?;
        if stopped_early(&out) {
            return Err(std::io::Error::other(format!(
                "ty INCOMPLETE: the search was stopped by a resource limit — nothing was proved\n\
                 {}{:?} ({:?})\n--- ty stdout ---\n{}\n--- ty stderr ---\n{}",
                ty_evidence_header(Path::new(self.0.get_program())),
                self.0,
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            )));
        }
        Ok(out)
    }
}

/// Whether `ty` stopped its search short: exit 5 (its INCOMPLETE), or the
/// `Model checking stopped:` line every limit-reached path prints. Nothing
/// here asks for a bounded search, so every such stop is a truncation.
fn stopped_early(out: &std::process::Output) -> bool {
    out.status.code() == Some(5)
        || String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l.starts_with("Model checking stopped:"))
}

/// Spawn a long-lived `ty` co-process (the `--cases` pool's one door) with
/// [`TyCommand::output`]'s scratch HOME and piped stdin and stdout. Built by aterm-uds's
/// `spawnfd::command` because it outlives the call and must inherit its stdio and nothing else.
/// The caller sends stderr to a file, never a pipe: nobody drains it between cases, and a `ty`
/// blocked on a full pipe answers nothing.
///
/// [`TyCommand::output`]'s classification has no transcript to read here; [`cases_verdict`]'s
/// protocol fails closed instead (a request with no verdict line is never an accept).
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

/// Run `ty` with a HOME of its own, whose check cache starts empty.
///
/// `ty` keeps a check cache at `$HOME/.ty/cache.json`, keyed by spec path, and reads and rewrites
/// the whole file on every run, even under `--force`. Every spec here is in a per-process temp
/// dir, so no entry is read back and the file only grows (18.7 MB after a day of gates, 45 ms per
/// run). A `ty` with `check-no-cache-v1` skips that cache under `--force` but still keeps its
/// compiled-artifact cache under HOME, so every `ty` child gets the scratch HOME.
///
/// The compiled-artifact cache is named by `TY_CACHE_DIR` unless the caller set one, never under
/// the process's `HOME`: tests move `HOME` to a scratch directory and remove it, and a `ty`
/// started meanwhile would re-create it. No verdict depends on that cache, and a cold start costs
/// nothing measurable.
///
/// Concurrent runs share the one HOME and can tear its `cache.json`; `ty` then warns and starts
/// empty, and under `--force` no entry is read back.
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

/// The identity of the checker a transcript came from, path and build stamp, so a gate panic names
/// the binary to act on (`--version` does not tell builds of one version apart; the mtime does).
/// Safe to prepend: it matches neither the `States found:` nor the `dead action(s) (never fired): `
/// parsers.
#[must_use]
// Skip: diagnostic string formatting for a verification harness.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ty_evidence_header(ty: &Path) -> String {
    format!("ty binary: {} [{}]\n", ty.display(), ty_build_stamp(ty))
}

/// The checker binary's age for an evidence header: the mtime of the last file of its exec chain
/// ([`binary_identity`]), naming that file when a wrapper forwards to it, or `mtime unknown`.
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

/// The distinct states `ty` reports exploring (its `States found: N` line). `None` (no line: the
/// run never reached its statistics, or the format drifted) is a failure to establish agreement,
/// never agreement: this parse is the only evidence the `ty` tier looked at anything.
// Skip: string scanning over another tool's output. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn ty_states_explored(evidence: &str) -> Option<u64> {
    evidence
        .lines()
        .find_map(|l| l.trim().strip_prefix("States found:"))
        .and_then(|n| n.trim().parse().ok())
}

/// The evidence half of tier agreement: a clean `ty` verdict must come from the same bounded space
/// the interpreter proved over. "No errors found" is also what a checker says of a space it never
/// entered (`RainbowJumpBurstLifecycle` under reduction: interpreter 128 states, `ty` 1, both
/// clean), so the tiers must agree on the work as well as the answer. Both walk the whole reachable
/// space of one finite machine, and `to_cfg` emits no VIEW or SYMMETRY, so with reduction off
/// ([`WHOLE_SPACE`]) their counts are equal.
///
/// Only a clean verdict is compared: a `Buggy = 1` run stops at its first counterexample.
// Skip: a verification-harness assert whose panic is the gate. Not shipped code.
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

/// Tiered prove-and-catch (the `Buggy` convention): the interpreter proves the invariants at
/// `Buggy=0` and finds a counterexample at `Buggy=1`, and when the checkers run, `ty` does the
/// same; a failure or a tier disagreement panics. A function-valued model skips the interpreter
/// and needs `ty`, so outside a checker run it returns [`NotRun`]. Scalar callers want
/// [`prove_and_catch_scalar`].
// Skip: a tiered verification driver (shells out to `ty`; its asserts are the gate).
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_and_catch_tiered(m: &Model, label: &str) -> Result<Covered, NotRun> {
    // `prove_and_catch` returns the committed arm's state count, the `ty` tier's yardstick.
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
            // The committed arm claims a proof, so it is held to the explored-space law; the
            // `Buggy = 1` run stops at its counterexample and is not compared.
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
        None => Err(NotRun { model: m.name }),
    }
}

/// Prove-and-catch a scalar model, asserting the shape that makes the `_tiered` form's `Err`
/// unreachable: the interpreter runs for every model with no `fn_vars`, so the returned
/// [`Covered`] is a log line, not a decision. Asserting the shape up front means a model that grows
/// an `fn_vars` entry fails here in every run, not only in one without the checkers.
// Skip: a thin policy wrapper over `prove_and_catch_tiered`; its assert is the gate.
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_and_catch_scalar(m: &Model, label: &str) -> Covered {
    assert_scalar(m, label, "prove_and_catch_scalar");
    match prove_and_catch_tiered(m, label) {
        Ok(c) => c,
        Err(n) => unreachable!("{label}: scalar model {} reported {n:?}", m.name),
    }
}

/// Prove, catch and multiply: the discharge protocol for a model whose safety argument is global
/// across every live instance of an enforcing structure, not local to one.
///
/// `FlashLimiter` proves ≤ 2 ignitions per rolling second for one limiter, which stays true
/// however many limiters exist, so it cannot see each split pane getting its own. A model that can
/// needs a second dial beside `Buggy`, and this protocol keeps that dial honest. Five gates, on
/// both tiers (a disagreement panics):
///
/// * **G1 shape**: the model declares `Buggy`, `Local` and `Instances`, committed to `Buggy = 0`,
///   `Local = 0`, `Instances >= 2`, so G2 is about a multi-enforcer world.
/// * **G2 prove**: every invariant holds at every `scenarios` corner.
/// * **G3 catch**: `Buggy = 1` at `buggy_at` yields a counterexample.
/// * **G4 multiply**: `Local = 1` (each enforcer bounding only its own state) yields a
///   counterexample at every corner. Unlike G3 it needs no special scenario: multiplying
///   enforcers is wrong unconditionally.
/// * **G5 attribution**: `Local = 1, Instances = 1` holds, so G4's counterexample is due to the
///   instance count, not other damage the `Local` dial does.
///
/// # Panics
///
/// On any gate failing, or on a tier disagreement.
// Skip: a tiered verification driver (shells out to `ty`; its asserts are the gate).
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_catch_and_multiply_scalar(
    m: &Model,
    scenarios: &[&[(&'static str, i64)]],
    buggy_at: &[(&'static str, i64)],
    label: &str,
) -> Covered {
    assert_scalar(m, label, "prove_catch_and_multiply_scalar");
    // G1: shape.
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

    // G2: prove.
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
    // G3: catch.
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
    // G4: multiply.
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
    // G5: attribution.
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
// Skip: a build-tooling precondition; the panic is the contract.
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

/// Refuse a `ty` dead set measured on a smaller space than the interpreter walked: the companion
/// to [`TyCommand::check`]'s arming for any driver that reads a dead set out of a `ty` transcript.
/// A dead set carries no record of the space it came from, so a driver that dropped a flag would
/// read phantom dead actions off a reduced run; with the transcript passed here, that is a hard
/// failure.
///
/// # Panics
///
/// If the transcript has no parsable `States found:` line, or its count differs from the
/// interpreter's reachable space at `Buggy = 0`.
// Skip: a verification-harness assert whose panic is the gate.
#[cfg_attr(trust_verify, trust::skip)]
pub fn assert_ty_saw_whole_space(m: &Model, evidence: &str, label: &str) {
    let committed = interp::with_buggy(m, 0);
    let interp_states = match interp::bmc(&committed) {
        Ok(n) => n,
        // Not an early return: the caller's report is `ty`'s exit status, and the discovered `ty`
        // can return clean on specs it never explored, so failing open here would book a model
        // the interpreter knows is violated as proved.
        Err((state, invariant)) => panic!(
            "{label}: {} — the interpreter finds invariant `{invariant}` VIOLATED at {state:?} \
             at the committed config, while `ty` returned a clean verdict. A clean `ty` over a \
             violated model is a false proof, not a disagreement about coverage.\n{evidence}",
            m.name
        ),
    };
    assert_same_space_explored(m, interp_states, evidence, label);
}

/// `ty`'s dead-action report for one configuration, held to the interpreter's: the
/// committed-config dead set of `m` both tiers agree on, or why they do not. Agreement runs both
/// ways, so an output-format drift that under-reports `ty`'s dead set fails rather than granting
/// the relaxation.
///
/// # Errors
///
/// When `ty` names an action `m` does not declare, or the two sets differ.
pub fn dead_actions_agree(
    m: &Model,
    ty_reported_dead: &[&str],
) -> Result<BTreeSet<&'static str>, String> {
    for d in ty_reported_dead {
        if !m.actions.iter().any(|a| a.name == *d) {
            return Err(format!(
                "{}: ty reported dead action `{d}` that the model does not declare — \
                 spec/emission drift",
                m.name
            ));
        }
    }
    let fired = interp::fired_actions(m);
    let interp_dead: BTreeSet<&'static str> = m
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !fired.contains(a))
        .collect();
    let ty_dead: BTreeSet<&str> = ty_reported_dead.iter().copied().collect();
    if ty_dead != interp_dead.iter().copied().collect() {
        return Err(format!(
            "{}: TIER DISAGREEMENT on the dead set — ty reports {ty_dead:?} but the \
             interpreter finds {interp_dead:?}",
            m.name
        ));
    }
    Ok(interp_dead)
}

/// Whether the actions of a model that never fire with no mutant on are negative controls rather
/// than vacuity: `configs` is the model in every configuration it declares (the committed one
/// first, then each `scenario`), and an action counts as dead only when it fires in none of them.
///
/// Each dead action must be one of two things, proved by the interpreter:
///
/// * **a violation step** — under some mutant dial, in some configuration, it
///   fires from a state where every law holds, and every such firing breaks
///   one ([`interp::violation_step`]): the defect's own step, or the world's
///   answer to one, so its deadness is what the laws state;
/// * **an isolated mutant** — in some configuration at some dial, the model
///   with every dead action removed holds every law (so no other branch of
///   the mutant supplies the counterexample), and adding this one back alone
///   makes it fire and break a law.
///
/// A dead action that fires under no mutant is real vacuity (a typo'd guard, a
/// scenario nothing declares); one that fires but is caught by nothing proves
/// nothing. A function-valued model gets no relaxation: the interpreter cannot
/// evaluate it.
///
/// Returns the number of negative controls (0 for a model with no dead action).
///
/// # Errors
///
/// Why one dead action is neither; the caller's check must stay red on it.
pub fn audit_dead_negative_controls(configs: &[Model]) -> Result<usize, String> {
    let Some(m) = configs.first() else {
        return Ok(0);
    };
    if !m.fn_vars.is_empty() {
        return Err(format!(
            "{}: function-valued model — the interpreter cannot verify negative controls, so \
             no dead-action relaxation is available; keep the model strictly non-vacuous",
            m.name
        ));
    }
    let mut dead: BTreeSet<&'static str> = m.actions.iter().map(|a| a.name).collect();
    for config in configs {
        let fired = interp::fired_actions(&interp::with_buggy(config, 0));
        dead.retain(|a| !fired.contains(a));
    }
    if dead.is_empty() {
        return Ok(0);
    }
    if !m.consts.iter().any(|(n, v)| *n == "Buggy" && *v == 0) {
        return Err(format!(
            "{}: dead action(s) {dead:?} in a model with no committed `Buggy = 0` dial — there \
             is no catch config under which they could be negative controls; REAL vacuity",
            m.name
        ));
    }
    let mutants: Vec<Model> = configs
        .iter()
        .flat_map(|config| {
            m.mutant_dials()
                .into_iter()
                .map(|dial| interp::with_buggy(config, dial))
        })
        .collect();
    for d in &dead {
        if mutants
            .iter()
            .any(|mutant| interp::violation_step(mutant, d) == Some(true))
        {
            continue;
        }
        if !mutants
            .iter()
            .any(|mutant| interp::fired_actions(mutant).contains(d))
        {
            return Err(format!(
                "{}: action `{d}` is dead with no mutant on AND fires under no mutant dial \
                 either — REAL vacuity (a typo'd guard, or a configuration no `scenario` \
                 declares)",
                m.name
            ));
        }
        // The first counterexample a baseline gave, while none has held.
        let (mut unsafe_baseline, mut any_safe, mut fires_alone) = (None, false, false);
        let isolated = mutants.iter().any(|mutant| {
            let mut baseline = mutant.clone();
            baseline.actions.retain(|a| !dead.contains(a.name));
            if let Err(broken) = interp::bmc(&baseline) {
                unsafe_baseline.get_or_insert(broken);
                return false;
            }
            any_safe = true;
            let mut alone = mutant.clone();
            alone
                .actions
                .retain(|a| !dead.contains(a.name) || a.name == *d);
            fires_alone |= interp::fired_actions(&alone).contains(d);
            interp::bmc(&alone).is_err()
        });
        if isolated {
            continue;
        }
        return Err(match unsafe_baseline.filter(|_| !any_safe) {
            Some((state, invariant)) => format!(
                "{}: action `{d}` is no violation step, and with every dead action removed \
                 each mutant still violates (`{invariant}` at {state:?}) — another branch \
                 supplies the counterexample, so `{d}` cannot be credited as an independently \
                 caught negative control",
                m.name
            ),
            _ if !fires_alone => format!(
                "{}: action `{d}` fires only beside another dead action — it is no \
                 independently exercisable negative control",
                m.name
            ),
            _ => format!(
                "{}: action `{d}` fires when added alone to the mutant baseline, but NO \
                 invariant is violated — this mutant is independently caught by nothing, so it \
                 proves nothing",
                m.name
            ),
        });
    }
    Ok(dead.len())
}

/// Tiered deadlock freedom (the `CHECK_DEADLOCK` protocol): the interpreter proves no non-final
/// wedge is reachable at `Buggy=0` and finds one at `Buggy=1` ([`interp::find_deadlock`]), and when
/// the checkers run, `ty` does the same via `to_cfg_deadlock_with`, its `Buggy=1` failure required
/// to be a deadlock rather than an invariant violation; any failure or disagreement panics.
/// `is_final` names the legitimate work-complete terminals (the interpreter twin of the model's
/// `Done` stutter). Scalar-only (asserted), so it returns a [`Covered`] outright.
// Skip: a verification-harness driver (shells out to `ty`, writes temp spec and cfg files; each
// `expect` is a deliberate test failure). Not shipped code.
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
                let out = TyCommand::check(&ty, &spec, &cfgp)
                    .output()
                    .expect("run ty check");
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

/// The interpreter's full discharge of a [`Liveness`] obligation, returned rather than panicked so
/// the registry sweep can name every failure at once. `Ok(n)` is the number of mutants that break
/// it; anything unestablished is an `Err`, never a waiver:
///
/// 1. the model is scalar with `Buggy` committed to 0, and the obligation names at least one
///    mutant (a property nothing can break would pass whatever the lane did);
/// 2. every fairness name is an action that fires at the committed config (an assumption about a
///    dead action assumes nothing), and every mutant is one that does not;
/// 3. at the committed config no fair behaviour leaves the goal for good;
/// 4. the `Buggy = 1` baseline with every committed-dead action removed has no such behaviour
///    either (else an unrelated `Buggy` branch supplies the counterexample);
/// 5. each mutant, added back alone to that baseline, produces one;
/// 6. every fairness assumption is load-bearing: dropped, the committed model breaks the
///    property, and a strong one demoted to weak breaks it too. An extra `WF` would be a promise
///    about the environment the proof never used.
///
/// The model is not rewritten beyond the `Buggy` flip and the mutant isolation, the same two
/// moves [`audit_dead_negative_controls`] makes for invariants.
// Skip: a verification-harness driver over the interpreter tier. Not shipped code.
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

/// [`audit_liveness`] after its preflight, over `committed` (`m`'s space at `Buggy = 0`, walked
/// by the caller), so a caller that needs more of that space shares the walk.
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

/// The `Buggy = 1` copy of `m` with every committed-dead action removed except `mutant`: the
/// healthy lane plus that one defect. The model a liveness mutant is checked on
/// ([`audit_liveness`], and for `ty` [`liveness_proves_and_catches_tiered`]), and the one a test
/// drives to show what the defect does alone.
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
pub(crate) fn isolate_mutant(m: &Model, fired: &BTreeSet<&'static str>, mutant: &str) -> Model {
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
// Skip: shells out to `ty` and renders its output. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn ty_check_liveness(
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
    let out = TyCommand::check(ty, &spec, &cfgp)
        .output()
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

/// Tiered liveness: [`audit_liveness`] on the interpreter (always; any `Err` panics), and, when the
/// checkers run, `ty` checking the same property as `PROPERTY <name>` under the
/// `<name>Spec == Spec /\ WF_vars(…) /\ SF_vars(…)` module [`Liveness::to_tla`] emits. `ty` must
/// prove it at the committed config over the interpreter's state count and report a temporal
/// violation for each mutant's isolated model; a disagreement panics. Scalar-only (asserted), so
/// it returns a [`Covered`] outright.
// Skip: a tiered verification driver (shells out to `ty`; its asserts are the gate).
#[cfg_attr(trust_verify, trust::skip)]
pub fn liveness_proves_and_catches_tiered(m: &Model, live: &Liveness, label: &str) -> Covered {
    assert_scalar(m, label, "liveness_proves_and_catches_tiered");
    // The committed config's space is walked once, for the audit and for what
    // the `ty` comparison below needs of it.
    let committed = interp::with_buggy(m, 0);
    let ty = find_ty();
    // Each `ty` check is a single-worker process of its own and none reads another's result, so
    // they run side by side: the committed one beside the interpreter's audit, the mutants' once
    // the audit has named the dead actions. Their verdicts are judged in a fixed order after.
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
            // A mutant's panic is held, not raised here, so the committed verdict below is
            // judged first.
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

/// Tiered per-transition conformance (the Tier-1 `ty trace validate` twin): does the model's
/// `Next` admit the real `prev -> next` step?
///
/// The interpreter always answers ([`interp::admits`]); when the checkers run, `ty` also validates
/// the two-step trace `[prev, next]` against `transition_spec()`, and a disagreement panics. That
/// spec's `Init` is `TRUE`, so the verdict is `Next`'s alone, and its cfg depends only on the model
/// and `overrides` (the cfg constants the real-code regime needs, such as a production `Cap`).
/// `action` names the expected action; `None` lets any action admit. Returns
/// `(conforms, diagnostics)`.
// Skip: the tiered validation driver (shells out to `ty`, renders JSON traces). Verification
// tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn validate_transition_tiered(
    m: &Model,
    overrides: &[(&'static str, i64)],
    prev: &BTreeMap<&'static str, i64>,
    next: &BTreeMap<&'static str, i64>,
    action: Option<&str>,
    label: &str,
) -> (bool, String) {
    // The named action (or any action) admits the step, with `overrides` applied as the cfg
    // applies them to `ty`, so both tiers evaluate the same instance. A named action is asked
    // directly, not through `admits`'s first match.
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

/// `ty trace validate`'s verdict on these exact three files, asked once per process of each `ty`
/// build: `ty` is deterministic, and an exhaustive Tier-1 walk hands it the same transition over
/// and over (aterm-agent's `conformance_footer_model` took 466 s with a `ty` process per
/// transition, 50 s without `ty`). Every distinct transition is still validated, and the caller
/// asserts tier agreement on every call. Asked of the `--cases` co-process where `ty` has one
/// ([`cases_verdict`]), else of one process ([`ty_trace_per_process`]); both are keyed on the
/// checker ([`memoized_verdict`]).
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

/// The `ty` that answers: the path [`find_ty`] gave and its [`BinaryIdentity`], so a wrapper whose
/// target alone was replaced is another checker, as a binary rewritten in place is.
type TyChecker = (PathBuf, BinaryIdentity);

/// [`ty_trace_verdict`]'s memo: a verdict is replayed only for the same rendered bytes (never the
/// model name, which a `Buggy` twin can share) asked of the same checker ([`TyChecker`]). `run`
/// answers anything else, handed the checker (`None` when `ty` cannot be stat'ed) so the
/// co-process pool keys on it too.
///
/// An update, link or yank moves [`find_ty`] to another path, and a rebuild in place keeps the
/// path but not the identity; a key without the checker would replay one build's verdict for the
/// next and hide a rejecting build's tier disagreement. A verdict from a binary that cannot be
/// stat'ed, or whose identity moved during `run`, is never memoized.
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
    // Unique per call (see `scratch_dir`): two tests in one process validating
    // the same model run concurrently.
    let dir = scratch_dir("aterm-conf", model);
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{model}.tla"));
    let cfg = dir.join(format!("{model}.cfg"));
    let trace = dir.join("t.json");
    std::fs::write(&spec, spec_text).expect("write spec");
    std::fs::write(&cfg, cfg_text).expect("write cfg");
    std::fs::write(&trace, trace_text).expect("write trace");
    let out = TyCommand::trace_validate(ty, &trace, &spec, &cfg)
        .output()
        .unwrap_or_else(|e| panic!("run ty trace validate for {label}: {e}"));
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

/// The `--cases` co-process's verdict on one two-step trace, or `None`, when the caller answers
/// the case with a `ty` process of its own.
///
/// One `ty trace validate` process costs ~6 ms of spawn and setup for microseconds of work, and
/// the footer suite asks for 8,064 (A10 of
/// docs/design/TY-TRACE-CASES-AND-FAIL-CLOSED-2026-10-01.md). `--cases -` loads the spec and cfg
/// once, then answers one request line with one verdict line, each in a fresh evaluation context;
/// the caller's tier-disagreement assert still holds every verdict against the interpreter.
///
/// Used only when `ty` advertises `trace-cases-v1` and `check-budget-self-v1` (an idle co-process
/// must not shrink a running check's memory budget). It fails closed: any failure (no spawn, a
/// missing or foreign header, EOF, a malformed or out-of-order line, a timeout) kills the child,
/// prints one notice with its stderr tail and returns `None`; after [`CASES_TRIES`] failures the
/// (checker, spec, cfg) stays per-process. A verdict marked `end` is a valid answer whose child is
/// then retired. A request with no verdict line is never an accept.
///
/// The pool and the failure counts are keyed on the checker as well as (spec, cfg), so after an
/// update or an in-place rebuild the next case spawns a child of the new build; the old child idles
/// until evicted.
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
            // A12: `class: "error"` means the question could not be answered as asked (an
            // evaluation failed, a label names no action), not that `Next` refutes the step. Read
            // as a refusal, it would agree with a refusing interpreter and pass.
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
    /// A read handle on the child's stderr file (its path is removed once the
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

    /// Kill the child (SIGKILL through `Child::kill`, which writes no crash report and is harmless
    /// on one that exited) and describe the failure with its exit status and stderr tail.
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
// Skip: format and closure chains over std bodies the prover lacks. Spec-harness trace emission.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn transition_trace_json(
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

/// Per-invariant non-vacuity: the invariants of `m` that no `Buggy = 1` member falsifies when each
/// is checked alone, in model order.
///
/// `prove_and_catch` stops at the first violated invariant, so a model can pass it with one live
/// property carrying the whole catch while every other invariant is a ghost, true by construction
/// and stating nothing about the code. A non-empty result is not automatically a defect: a model's
/// declared `bound`s state the box rather than a design claim and are expected here;
/// [`sweep_faults`] judges the model by its own declarations.
///
/// One BFS of the `Buggy = 1` space ([`interp::violated_invariants`]) returns exactly the set that
/// per-invariant `bmc` runs would.
///
/// # Panics
///
/// Propagates the interpreter's panic for a model it cannot evaluate (a function-valued `Expr`),
/// so an uninterpretable model is never reported as carrying no ghosts.
#[must_use]
pub fn uncaught_invariants(m: &Model) -> Vec<&'static str> {
    let violated = interp::violated_invariants(&interp::with_buggy(m, 1));
    m.invariants
        .iter()
        .map(|inv| inv.name)
        .filter(|name| !violated.contains(name))
        .collect()
}

/// What the non-vacuity sweep finds wrong with one model, one line per
/// fault, over every configuration it declares (`configs[0]` the committed
/// one, then each `scenario`):
///
/// * a `bound` that is not a range ([`Model::malformed_bounds`]);
/// * a law broken with no mutant on, in any configuration;
/// * a design claim (`invariant`) that no member of the mutant family — each
///   `Buggy` dial the model is written for ([`Model::mutant_dials`]), in each
///   configuration — breaks within the interpreter's budget: it asserts
///   nothing, so a proof of it proves nothing;
/// * a `bound` some mutant breaks: it is a caught design claim, and a stale
///   declaration would exempt it from the line above;
/// * an action that never fires with no mutant on and is no audited negative
///   control ([`audit_dead_negative_controls`]).
///
/// A function-valued model is the interpreter's blind spot: `spec-check
/// models` holds it under `ty`, so only its bounds are read here.
///
/// # Panics
///
/// When a configuration with no mutant on has more reachable states than the
/// interpreter's budget: its laws could not be checked at all.
// Skip: drives the interpreter's walks; verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn sweep_faults(configs: &[Model]) -> Vec<String> {
    use crate::derive::InvariantKind;
    let Some(m) = configs.first() else {
        return Vec::new();
    };
    let mut faults = m.malformed_bounds();
    if m.is_function_valued() {
        return faults;
    }
    for (i, config) in configs.iter().enumerate() {
        for law in interp::violated_invariants(&interp::with_buggy(config, 0)) {
            faults.push(format!(
                "{}::{law}: broken with no mutant on, {}",
                m.name,
                if i == 0 {
                    "at the committed config".to_string()
                } else {
                    format!("in the scenario {:?}", config_overrides(m, config))
                }
            ));
        }
    }
    let mut caught: BTreeSet<&'static str> = BTreeSet::new();
    for config in configs {
        for dial in m.mutant_dials() {
            caught.extend(interp::violated_within_budget(&interp::with_buggy(
                config, dial,
            )));
        }
    }
    for inv in &m.invariants {
        match (inv.kind, caught.contains(inv.name)) {
            (InvariantKind::Claim, false) => faults.push(format!(
                "{}::{}: no mutant breaks it — give it one, delete it if it restates another \
                 law or no defect can reach it, or declare it a `bound` if it states the space",
                m.name, inv.name
            )),
            (InvariantKind::Bound, true) => faults.push(format!(
                "{}::{}: declared `bound`, but a mutant breaks it — it is a caught design \
                 claim: declare it `invariant`",
                m.name, inv.name
            )),
            _ => {}
        }
    }
    if let Err(why) = audit_dead_negative_controls(configs) {
        faults.push(why);
    }
    faults
}

/// The constants `config` sets differently from `m`'s committed ones.
fn config_overrides(m: &Model, config: &Model) -> Vec<(&'static str, i64)> {
    config
        .consts
        .iter()
        .filter(|c| !m.consts.contains(c))
        .copied()
        .collect()
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;

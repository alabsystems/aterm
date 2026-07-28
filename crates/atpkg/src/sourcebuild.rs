// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The **source-build lane** (POC path) for a companion tool: fetch a public repo at an
//! exact pinned commit, build it from source with a neutralized/hardened toolchain
//! invocation, and stage the exposed binaries into the immutable store — then activate +
//! shim. This lane runs **regardless of the atpkg root key** (an INERT manager still
//! source-builds), because its trust basis is NOT the signed index but:
//!
//! 1. the OWNER-declared compiled-in manifest ([`crate::companions`]), attested by aterm's
//!    own notarized release signature, and
//! 2. an EXACT pinned commit of a public repo, whose checked-out HEAD is asserted
//!    byte-equal to the pin after a full clone (a moving ref is never trusted), plus
//! 3. the repo's `Cargo.lock` re-hashed against the manifest pin, so the build CLOSURE
//!    (transitive deps) is fixed, and the build runs `--locked`.
//!
//! It is a fenced, lower-assurance lane: a source build NEVER satisfies a signed pin, never
//! joins a coherence tuple, and is labelled `provenance = "source"` everywhere (a sidecar
//! next to the store build). See `docs/COMPANION-TOOLS.md` and the WEDGE §10 invariants
//! it preserves by REUSE (immutable per-build dir, `.ready` last, atomic activation,
//! append-not-prepend shim-allowlisted PATH, separate OS process).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::companions::{Companion, SeedPolicy};
use crate::platform::ensure_private_dir;
use crate::store::Layout;

/// The per-build provenance sidecar (`store/<program>/<build>.provenance`) — a sibling of
/// the `.ready` marker, OUTSIDE the build tree so it never perturbs the `tree_root`. It is
/// how `verify`/`gc`/UX tell a lower-assurance source build apart from a signed prebuilt;
/// its presence must never read as a signature match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    /// Always `"source"` for this lane (signed installs record no sidecar).
    pub provenance: String,
    /// The program id.
    pub program: String,
    /// The store build number (`git rev-list --count <commit>`).
    pub build: u64,
    /// The exact 40-hex commit built.
    pub commit: String,
    /// The `owner/repo` slug the source came from.
    pub repo: String,
    /// The self-computed `tree_root` over the staged build (self-attestation, NOT a signature).
    pub tree_root: String,
    /// Unix seconds when the build was staged.
    pub built_unix: u64,
    /// sha256 of each staged exposed binary (audit anchor for "did a re-seed change bytes").
    #[serde(default)]
    pub bins: BTreeMap<String, String>,
}

/// A successful source build + install.
#[derive(Debug, Clone)]
pub struct Installed {
    /// The program id.
    pub program: String,
    /// The store build number.
    pub build: u64,
    /// The commit built.
    pub commit: String,
    // There is deliberately no `refused_shims` here, unlike `flow::InstallReport`. The
    // released-tool lane admits an UNVERIFIED manifest's `exposes`, so a refusal is a normal
    // outcome it must report. This lane's `expose` list comes from the in-tree companion
    // ledger, which `companions.rs` check 6 already validates through `shim_allowed`, and
    // `stage_and_activate` turns a refused name into a hard `Stage` error rather than staging
    // a binary nobody can ever run. A refusal therefore cannot reach a successful install,
    // and a field that is provably always empty is a claim the code does not make.
    /// Whether the build was reused (already complete + active for this commit).
    pub reused: bool,
}

/// Every failure mode of the source-build lane. All are non-fatal to the wider seed
/// reconcile (one companion failing never blocks the others).
#[derive(Debug)]
pub enum SourceBuildError {
    /// `git` or `cargo` is not on PATH.
    MissingToolchain(&'static str),
    /// `rustc` is below the companion's `min_toolchain`.
    RustcTooOld { have: String, need: String },
    /// Not enough free space for the build target.
    LowDisk { have_gb: u64, need_gb: u64 },
    /// A git operation failed.
    Git(String),
    /// The checked-out HEAD did not byte-equal the pinned commit (moving-ref / rewrite).
    PinMismatch { want: String, got: String },
    /// The repo `Cargo.lock` sha256 did not match the manifest pin (closure drift).
    LockMismatch { want: String, got: String },
    /// The `cargo build` failed, timed out, or a watchdog killed it.
    Build(String),
    /// A build-redirecting repo `.cargo/config` was detected but could NOT be neutralized
    /// (rename failed) — fail closed rather than build under attacker-controlled config.
    ConfigNeutralize(String),
    /// The source-build lane is not permitted here (opt-in/policy) — a defense-in-depth
    /// re-assertion at the build choke point, independent of the seed gate.
    NotPermitted(&'static str),
    /// Staging the built binaries into the store failed.
    Stage(String),
    /// A declared exposed binary was not produced by the build.
    MissingBinary(String),
    /// A filesystem/hardening error.
    Io(String),
}

impl std::fmt::Display for SourceBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingToolchain(t) => {
                write!(f, "{t} is not installed (needed to build from source)")
            }
            Self::RustcTooOld { have, need } => {
                write!(f, "rustc {have} is below the required {need}")
            }
            Self::LowDisk { have_gb, need_gb } => {
                write!(
                    f,
                    "insufficient free space: {have_gb} GiB available, {need_gb} GiB required"
                )
            }
            Self::Git(e) => write!(f, "git: {e}"),
            Self::PinMismatch { want, got } => {
                write!(
                    f,
                    "pin mismatch: checked-out HEAD {got} != pinned {want} (refusing a moving ref)"
                )
            }
            Self::LockMismatch { want, got } => {
                write!(
                    f,
                    "Cargo.lock sha256 {got} != pinned {want} (build-closure drift)"
                )
            }
            Self::Build(e) => write!(f, "cargo build: {e}"),
            Self::ConfigNeutralize(e) => write!(f, "could not neutralize repo .cargo/config: {e}"),
            Self::NotPermitted(e) => write!(f, "source-build not permitted: {e}"),
            Self::Stage(e) => write!(f, "stage: {e}"),
            Self::MissingBinary(b) => write!(f, "build did not produce exposed binary '{b}'"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for SourceBuildError {}

/// The channel a source-built companion activates under: its own program name (a source
/// companion is a standalone member, never a coherence-tuple channel).
fn channel_for(c: &Companion) -> &str {
    &c.name
}

/// Probe `rustc --version` → `(major, minor, patch)`.
#[must_use]
pub fn probe_rustc_version() -> Option<(u64, u64, u64)> {
    let out = Command::new("rustc").arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    // "rustc 1.95.0 (59807616e 2026-04-14)"
    let ver = s.split_whitespace().nth(1)?;
    parse_semver(ver)
}

/// Parse a dotted `major.minor.patch` prefix (trailing `-pre`/build metadata ignored).
#[must_use]
pub fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().unwrap_or("0").parse().ok()?;
    let patch = it.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// Whether tool `name` resolves on PATH (a lightweight `command -v`).
#[must_use]
pub fn have_tool(name: &str) -> bool {
    // `<name> --version` is cheap and true for git/cargo/rustc; fall back to a spawn probe.
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Read a build's provenance sidecar, if present.
#[must_use]
pub fn read_provenance(layout: &Layout, program: &str, build: u64) -> Option<Provenance> {
    let path = provenance_path(&layout.build_dir(program, build));
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

/// The sidecar path for a build dir: `store/<program>/<build>.provenance` (sibling, outside).
fn provenance_path(build_dir: &Path) -> PathBuf {
    let name = build_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("0");
    build_dir.with_file_name(format!("{name}.provenance"))
}

/// Build companion `c` from source at its pinned commit and install it into the store.
/// `log` receives human progress lines. Idempotent: an already-complete + active build for
/// the SAME commit is reused (no rebuild).
///
/// The whole fetch → build → stage → mark-ready → activate → shim sequence runs under an
/// exclusive per-program [`crate::platform::file_lock`], so concurrent seed reconciles (first-run
/// spawn + the 6h loop) cannot corrupt the shared build dir or channel flip.
pub fn build_and_install(
    layout: &Layout,
    c: &Companion,
    seed: &SeedPolicy,
    log: &mut dyn FnMut(&str),
) -> Result<Installed, SourceBuildError> {
    // Store mutation (fetch/build/stage/activate/shim) is serialized by the store-wide
    // single-writer lock that the CLI edge (`cli::main_entry` → `mutator_store_lock`, the
    // `seed` verb is in `verb_mutates_store`) holds for the whole command — callers MUST hold
    // it. This replaces the earlier ad-hoc per-program lock; the store-wide lock also covers
    // the signed default-set lane, so both cannot race.
    ensure_private_dir(&layout.prefix).map_err(|e| SourceBuildError::Io(e.to_string()))?;

    // Defense-in-depth: re-assert consent + policy AT THE CHOKE POINT, independent of the
    // seed gate — this `pub fn` must never build without opt-in/policy even if called directly.
    if !c.source_build_allowed() {
        return Err(SourceBuildError::NotPermitted(
            "policy forbids source build",
        ));
    }
    if !c.coherence.is_empty() {
        return Err(SourceBuildError::NotPermitted(
            "coherence-grouped tool is prebuilt-only",
        ));
    }
    if std::env::var_os("ATPKG_DISABLE").is_some()
        || std::env::var_os("ATPKG_NO_SOURCE_BUILD").is_some()
        || std::env::var_os("ATPKG_MANAGED").is_some()
    {
        return Err(SourceBuildError::NotPermitted("disabled/managed"));
    }
    let opted_in = seed.source_build_default
        || std::env::var("ATPKG_SOURCE_BUILD").is_ok_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        });
    if !opted_in {
        return Err(SourceBuildError::NotPermitted(
            "not opted in (ATPKG_SOURCE_BUILD)",
        ));
    }

    // Toolchain gates (probe here too, not only in the seed predicate: this fn is the
    // single choke point and must never spawn a build without them).
    if !have_tool("git") {
        return Err(SourceBuildError::MissingToolchain("git"));
    }
    if !have_tool("cargo") {
        return Err(SourceBuildError::MissingToolchain("cargo"));
    }
    if !c.min_toolchain.is_empty()
        && let (Some(have), Some(need)) = (probe_rustc_version(), parse_semver(&c.min_toolchain))
        && have < need
    {
        return Err(SourceBuildError::RustcTooOld {
            have: format!("{}.{}.{}", have.0, have.1, have.2),
            need: c.min_toolchain.clone(),
        });
    }

    // Free-space HARD gate (budget the multi-GB target dir, not the ~62 MB artifact). Fail
    // CLOSED on an indeterminate reading — a "hard gate" that cannot prove enough space must
    // refuse, not silently proceed.
    let need = seed.target_free_gb_min.saturating_mul(1 << 30);
    match crate::freespace::available_bytes(&layout.prefix) {
        Some(avail) if avail < need => {
            return Err(SourceBuildError::LowDisk {
                have_gb: avail >> 30,
                need_gb: seed.target_free_gb_min,
            });
        }
        None => {
            return Err(SourceBuildError::LowDisk {
                have_gb: 0,
                need_gb: seed.target_free_gb_min,
            });
        }
        Some(_) => {}
    }

    // --- 1. fetch, fail-closed: full clone + checkout the pin + assert HEAD == pin --------
    let src = layout.prefix.join("src").join(&c.name);
    ensure_private_dir(&layout.prefix.join("src"))
        .map_err(|e| SourceBuildError::Io(e.to_string()))?;
    fetch_pinned(&src, c, log)?;

    // --- 2. verify the build CLOSURE pin (Cargo.lock sha256) -----------------------------
    let lock = src.join("Cargo.lock");
    let got = crate::tree::sha256_file(&lock).map_err(|e| SourceBuildError::Io(e.to_string()))?;
    if !got.eq_ignore_ascii_case(&c.cargo_lock_sha256) {
        return Err(SourceBuildError::LockMismatch {
            want: c.cargo_lock_sha256.clone(),
            got,
        });
    }
    log("verified Cargo.lock closure pin");

    // --- build number = full-history commit count (matches the eventual prebuilt) --------
    let build = commit_count(&src, &c.commit)?;

    // Idempotency: complete + active for this commit → reuse.
    let build_dir = layout.build_dir(&c.name, build);
    if crate::store::build_is_complete(&build_dir) {
        let active = crate::ops::active_builds(layout).get(&c.name).copied() == Some(build);
        let same_commit =
            read_provenance(layout, &c.name, build).is_some_and(|p| p.commit == c.commit);
        if active && same_commit {
            log("already installed for this commit — reusing");
            return Ok(Installed {
                program: c.name.clone(),
                build,
                commit: c.commit.clone(),
                reused: true,
            });
        }
    }

    // --- 3. neutralize repo-supplied cargo config (fail CLOSED if it can't be moved) ------
    neutralize_repo_cargo_config(&src, log)?;

    // --- 4. build under a clean CARGO_HOME + scrubbed env + a wall-clock watchdog --------
    let target_dir = layout.prefix.join("src").join(format!("{}-target", c.name));
    let cargo_home = layout.prefix.join("cargohome");
    ensure_private_dir(&cargo_home).map_err(|e| SourceBuildError::Io(e.to_string()))?;
    run_cargo_build(&src, &target_dir, &cargo_home, c, seed, log)?;

    // --- 5. stage immutably: copy exposed bins, sidecar, .ready LAST, then activate+shim --
    let rel = target_dir.join("release");
    stage_and_activate(layout, c, build, &build_dir, &rel, log)
}

/// Full (non-shallow) clone + detached checkout of the exact pin, asserting HEAD == pin.
/// Reuses an existing checkout only when it is ALREADY at the pin (fast path for re-seed).
fn fetch_pinned(
    src: &Path,
    c: &Companion,
    log: &mut dyn FnMut(&str),
) -> Result<(), SourceBuildError> {
    let url = format!("https://github.com/{}.git", c.repo);

    // Fast path: an existing checkout already at the pin. HEAD matching is necessary but not
    // sufficient — the working tree could have been dirtied — so force it pristine to the pin
    // (reset + clean) before trusting it for the build.
    if src.join(".git").is_dir() {
        if let Ok(head) = git(src, &["rev-parse", "HEAD"])
            && head.trim() == c.commit
        {
            let reset_ok = git(src, &["reset", "--hard", &c.commit]).is_ok()
                && git(src, &["clean", "-ffdx"]).is_ok();
            if reset_ok {
                log("source cache already at the pinned commit (tree reset to pin)");
                return Ok(());
            }
            // Could not force-clean the reused tree → fall through to a fresh clone.
        }
        // Wrong commit (a re-pin): wipe and re-clone for a clean, full history.
        let _ = std::fs::remove_dir_all(src);
    } else if src.exists() {
        let _ = std::fs::remove_dir_all(src);
    }

    if let Some(parent) = src.parent() {
        ensure_private_dir(parent).map_err(|e| SourceBuildError::Io(e.to_string()))?;
    }
    log(&format!("cloning {} (full history)…", c.repo));
    git_run(&["clone", &url, &src.to_string_lossy()], None)?;

    // Detached checkout of the exact commit. If the commit is not an ancestor of the default
    // branch (unusual), fetch it explicitly first — still by exact sha, never a moving ref.
    if git(src, &["checkout", "--detach", &c.commit]).is_err() {
        git_run(
            &["-C", &src.to_string_lossy(), "fetch", "origin", &c.commit],
            None,
        )?;
        git(src, &["checkout", "--detach", &c.commit]).map_err(SourceBuildError::Git)?;
    }

    // The load-bearing assertion: HEAD byte-equals the pin.
    let head = git(src, &["rev-parse", "HEAD"]).map_err(SourceBuildError::Git)?;
    let head = head.trim().to_string();
    if head != c.commit {
        return Err(SourceBuildError::PinMismatch {
            want: c.commit.clone(),
            got: head,
        });
    }
    log("pin verified: HEAD == pinned commit");
    Ok(())
}

/// `git rev-list --count <commit>` over the full clone → the store build number.
fn commit_count(src: &Path, commit: &str) -> Result<u64, SourceBuildError> {
    let out = git(src, &["rev-list", "--count", commit]).map_err(SourceBuildError::Git)?;
    out.trim().parse::<u64>().map_err(|_| {
        SourceBuildError::Git(format!("rev-list --count returned non-numeric: {out:?}"))
    })
}

/// Rename any repo-supplied `.cargo/config[.toml]` aside if it can redirect the build to
/// arbitrary code, so the pinned tree can never do so. Detection PARSES the TOML (a substring
/// scan is bypassable via `include = […]`, `build.rustc`, `rustc-workspace-wrapper`, whitespace
/// `[ env ]`, or dotted keys), and is fail-CLOSED: an unparseable config, or one that is
/// dangerous but cannot be moved aside, aborts the build. Logged, never silent.
fn neutralize_repo_cargo_config(
    src: &Path,
    log: &mut dyn FnMut(&str),
) -> Result<(), SourceBuildError> {
    for name in [".cargo/config.toml", ".cargo/config"] {
        let path = src.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if cargo_config_is_dangerous(&text) {
            let aside = path.with_extension("toml.neutralized-by-atpkg");
            std::fs::rename(&path, &aside)
                .map_err(|e| SourceBuildError::ConfigNeutralize(format!("{name}: {e}")))?;
            log(&format!(
                "neutralized repo {name} (build-redirecting keys) — building with clean defaults"
            ));
        }
    }
    Ok(())
}

/// Whether a `.cargo/config` can redirect the build to arbitrary code. Parses the TOML and
/// checks structured keys; an unparseable config is treated as dangerous (fail closed). Any
/// `include` is dangerous (it merges an unvetted sibling file that could carry any of these).
fn cargo_config_is_dangerous(text: &str) -> bool {
    let Ok(val) = text.parse::<toml::Value>() else {
        return true; // cannot prove safe → neutralize
    };
    // `include` merges an arbitrary sibling file — treat as dangerous regardless of its body.
    if val.get("include").is_some() {
        return true;
    }
    // `[build]` compiler/linker overrides (all spellings).
    if let Some(build) = val.get("build").and_then(toml::Value::as_table) {
        for k in [
            "rustc",
            "rustc-wrapper",
            "rustc-workspace-wrapper",
            "rustdoc",
            "rustflags",
            "target",
        ] {
            if build.contains_key(k) {
                return true;
            }
        }
    }
    // Per-target linker / runner / rustflags.
    if let Some(targets) = val.get("target").and_then(toml::Value::as_table) {
        for tv in targets.values() {
            if let Some(t) = tv.as_table() {
                for k in ["linker", "runner", "rustflags"] {
                    if t.contains_key(k) {
                        return true;
                    }
                }
            }
        }
    }
    // Environment injection, source replacement, and alternate registries.
    val.get("env").is_some()
        || val.get("source").is_some()
        || val.get("registries").is_some()
        || val.get("registry").is_some()
}

/// Run `cargo build --release --locked <build_args>` with a clean `CARGO_HOME`, a scrubbed
/// environment (no ambient RUSTFLAGS/wrappers/tokens), and a hard wall-clock watchdog that
/// kills the whole process tree on timeout.
fn run_cargo_build(
    src: &Path,
    target_dir: &Path,
    cargo_home: &Path,
    c: &Companion,
    seed: &SeedPolicy,
    log: &mut dyn FnMut(&str),
) -> Result<(), SourceBuildError> {
    log(&format!(
        "building {} (~{} MiB, up to {} min)…",
        c.name,
        c.size_hint_mb,
        seed.build_timeout_secs / 60
    ));
    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("--release").arg("--locked");
    for a in &c.build_args {
        cmd.arg(a);
    }
    cmd.current_dir(src);
    // HARDENED ENV — clear EVERYTHING, then re-add only a benign allowlist. A deny-list is
    // unsafe here: cargo/rustc/the dynamic loader honor many equally-powerful spellings
    // (RUSTC, CARGO_BUILD_RUSTC[_WRAPPER], CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER,
    // CARGO_TARGET_<triple>_LINKER/RUNNER, CC/CXX for `cc`-crate build scripts, and
    // LD_PRELOAD / DYLD_INSERT_LIBRARIES / DYLD_LIBRARY_PATH), any of which redirects the
    // compiler, linker, or loader into attacker code during the "hardened" build. An
    // allowlist is the only robust boundary (docs/COMPANION-TOOLS.md §6).
    cmd.env_clear();
    const KEEP: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "TERM",
        "TMPDIR",
        "TZ",
        "SHELL",
        "LANG",
        // macOS C-toolchain / SDK discovery, when present in the ambient env.
        "SDKROOT",
        "DEVELOPER_DIR",
        "MACOSX_DEPLOYMENT_TARGET",
    ];
    for (k, v) in std::env::vars_os() {
        let keep = k
            .to_str()
            .is_some_and(|k| KEEP.contains(&k) || k.starts_with("LC_"));
        if keep {
            cmd.env(&k, &v);
        }
    }
    cmd.env("CARGO_HOME", cargo_home)
        .env("CARGO_TARGET_DIR", target_dir);
    // New process group so the watchdog can kill the whole tree (rustc children, cc, etc.).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| SourceBuildError::Build(e.to_string()))?;
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(seed.build_timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    log("build ok");
                    return Ok(());
                }
                return Err(SourceBuildError::Build(format!("exited with {status}")));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_tree(pid);
                    let _ = child.wait();
                    return Err(SourceBuildError::Build(format!(
                        "timed out after {}s — process tree killed",
                        seed.build_timeout_secs
                    )));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(SourceBuildError::Build(e.to_string())),
        }
    }
}

/// Kill the child's whole process group (Unix); best-effort single-process kill elsewhere.
fn kill_tree(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: `kill(2)` with a negative pid targets the process GROUP; the child was
        // spawned into its own group (`process_group(0)`), so pid == pgid. A stale pid at
        // worst signals nothing (ESRCH). No memory is touched.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid; // best-effort; the try_wait loop still returns the timeout error.
    }
}

/// Stage the exposed binaries into an immutable build dir, write the provenance sidecar,
/// mark `.ready` LAST, then atomically activate + install shims. Any failure discards the
/// half-staged build (so `build_is_complete` never reads a partial tree as installed).
fn stage_and_activate(
    layout: &Layout,
    c: &Companion,
    build: u64,
    build_dir: &Path,
    rel: &Path,
    log: &mut dyn FnMut(&str),
) -> Result<Installed, SourceBuildError> {
    // Fresh, immutable build dir.
    crate::store::discard_build(build_dir);
    let bin_out = build_dir.join("bin");
    if let Err(e) = std::fs::create_dir_all(&bin_out) {
        crate::store::discard_build(build_dir);
        return Err(SourceBuildError::Stage(e.to_string()));
    }

    let mut bins: BTreeMap<String, String> = BTreeMap::new();
    // The admitted names, carried to the shim install below. Collecting them HERE rather
    // than re-splitting `c.expose` there is what keeps the two consistent: this loop already
    // decided that a refused name aborts the build, so the shim pass cannot see a name this
    // one let through, and there is no second, silently-different refusal outcome.
    let mut tools: Vec<crate::store::ToolName> = Vec::new();
    for raw in &c.expose {
        // The ledger validator already gates every `expose` name through `shim_allowed`
        // (companions.rs check 6), so this refusal is unreachable from a valid ledger. It is
        // still an error rather than a skip: the ONLY purpose of staging a binary is the shim
        // that name is refused, so silently copying it would grow the store with a tool no
        // one can ever run.
        let Some(tool) = crate::store::ToolName::new(raw) else {
            crate::store::discard_build(build_dir);
            return Err(SourceBuildError::Stage(format!(
                "exposed name '{raw}' is refused a shim (sensitive/malformed)"
            )));
        };
        // The executable's own file name (`<tool>.exe` on Windows) on BOTH sides of the copy
        // — `exe_file` is the one place that suffix is appended.
        let from = rel.join(tool.exe_file());
        if !from.is_file() {
            crate::store::discard_build(build_dir);
            return Err(SourceBuildError::MissingBinary(raw.clone()));
        }
        let to = bin_out.join(tool.exe_file());
        if let Err(e) = copy_exe(&from, &to) {
            crate::store::discard_build(build_dir);
            return Err(SourceBuildError::Stage(e.to_string()));
        }
        match crate::tree::sha256_file(&to) {
            Ok(sum) => {
                bins.insert(raw.clone(), sum);
            }
            Err(e) => {
                crate::store::discard_build(build_dir);
                return Err(SourceBuildError::Stage(e.to_string()));
            }
        }
        tools.push(tool);
    }

    // Self-attest the staged tree (sidecar record; NOT a signature).
    let tree_root =
        crate::tree::tree_root(build_dir).map_err(|e| SourceBuildError::Stage(e.to_string()))?;
    let prov = Provenance {
        provenance: "source".to_string(),
        program: c.name.clone(),
        build,
        commit: c.commit.clone(),
        repo: c.repo.clone(),
        tree_root,
        built_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        bins,
    };
    if let Err(e) = write_provenance(build_dir, &prov) {
        crate::store::discard_build(build_dir);
        return Err(SourceBuildError::Stage(e.to_string()));
    }

    // The completeness marker LAST — only now is the build "installed".
    if let Err(e) = crate::store::mark_build_ready(build_dir) {
        crate::store::discard_build(build_dir);
        return Err(SourceBuildError::Stage(e.to_string()));
    }

    // Atomic activation + PATH shims. A failure here must DISCARD the build — otherwise it is
    // left complete-but-inactive/half-shimmed, which `build_is_complete`/`active_builds`
    // would mis-read as installed and never retry.
    //
    // `install_tools` with the set the staging loop admitted, NOT `install_shims(&c.expose)`:
    // the latter re-splits the raw list and hands back "refused" names, but this function
    // already returned `Err` for every one of those, so that branch was unreachable and its
    // `NOTE: refused shims` log could never print. Passing the admitted set says the same
    // thing in the types instead of in a comment.
    if let Err(e) = crate::activate::activate_channel(layout, channel_for(c), build_dir) {
        // The per-program witness half may have flipped before the channel half
        // failed — undo whatever points at the build being discarded.
        crate::activate::undo_activation(layout, channel_for(c), build_dir);
        crate::store::discard_build(build_dir);
        return Err(SourceBuildError::Stage(e.to_string()));
    }
    if let Err(e) = crate::activate::install_tools(layout, build_dir, &tools) {
        // Activation SUCCEEDED and some shims may already be written. Undo both
        // before discarding, or the deleted tree stays named by both `current`
        // links and half a shim set — a program on PATH that runs nothing.
        crate::activate::undo_activation(layout, channel_for(c), build_dir);
        crate::store::discard_build(build_dir);
        return Err(SourceBuildError::Stage(e.to_string()));
    }
    log(&format!(
        "installed {} build {} ({} bin{}) — provenance=source",
        c.name,
        build,
        c.expose.len(),
        if c.expose.len() == 1 { "" } else { "s" }
    ));

    Ok(Installed {
        program: c.name.clone(),
        build,
        commit: c.commit.clone(),
        reused: false,
    })
}

/// Write the provenance sidecar next to the build dir (outside the tree).
fn write_provenance(build_dir: &Path, prov: &Provenance) -> std::io::Result<()> {
    let text = toml::to_string(prov)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(provenance_path(build_dir), text)
}

/// Copy an executable, preserving the exec bit (0755 on Unix).
fn copy_exe(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::copy(from, to)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Run `git <args>` in `cwd`, returning stdout on success.
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Run `git <args>` with no fixed cwd (for `clone`), mapping failure to a `Git` error.
fn git_run(args: &[&str], cwd: Option<&Path>) -> Result<(), SourceBuildError> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let out = cmd
        .output()
        .map_err(|e| SourceBuildError::Git(e.to_string()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(SourceBuildError::Git(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parses_and_orders() {
        assert_eq!(parse_semver("1.95.0"), Some((1, 95, 0)));
        assert_eq!(parse_semver("1.74.0-nightly"), Some((1, 74, 0)));
        assert!(parse_semver("1.95.0") > parse_semver("1.74.0"));
        assert!(parse_semver("1.73.0") < parse_semver("1.74.0"));
    }

    #[test]
    fn provenance_roundtrips() {
        let mut bins = BTreeMap::new();
        bins.insert("ay".to_string(), "abc".to_string());
        let p = Provenance {
            provenance: "source".to_string(),
            program: "ay".to_string(),
            build: 42,
            commit: "8af5cbb3a7aa7779f7a429c1f5772b59737b6cd1".to_string(),
            repo: "alabsystems/ay".to_string(),
            tree_root: "deadbeef".to_string(),
            built_unix: 1_700_000_000,
            bins,
        };
        let text = toml::to_string(&p).unwrap();
        let back: Provenance = toml::from_str(&text).unwrap();
        assert_eq!(back.commit, p.commit);
        assert_eq!(back.build, 42);
        assert_eq!(back.provenance, "source");
        assert_eq!(back.bins.get("ay").unwrap(), "abc");
    }

    #[test]
    fn provenance_path_is_sibling_outside_tree() {
        let bd = Path::new("/x/store/ay/1234");
        assert_eq!(
            provenance_path(bd),
            Path::new("/x/store/ay/1234.provenance")
        );
    }
}

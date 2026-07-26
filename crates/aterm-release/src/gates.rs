// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pre-claim gates (release spec §6 `gates.rs`, plus the changelog gates of
//! §3): macOS arm64 host, clean tree, on main, HEAD == origin/main, tag absent
//! local+remote, changelog non-empty/no-`'''`, `gh auth status`, Trust rustc
//! probe (always on — the repo compiles with Trust, there is no opt-out
//! lane), x86_64 rustup target probe with printed remediation (`--arm64-only`
//! opt-out), disk space. All fail closed BEFORE anything is committed or
//! pushed — a failed gate costs seconds, not a burned ledger number.
//!
//! Git-backed gates go through the injectable [`GitRunner`] seam (same one the
//! claim uses); host-tool gates (`gh`, `rustup`, `df`, the Trust rustc) shell
//! out directly — they interrogate THIS machine, which is exactly what the
//! gate is for.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::changelog;
use crate::ledger::{Error, GitRunner, Result, git_ok, rev_parse};

/// Free-disk floor for a cut. A universal release build carries two full
/// `--release` target trees (Trust arm64 + rustup x86_64, both with
/// `CARGO_PROFILE_RELEASE_DEBUG=1` for the dSYM) plus the .app/DMG staging in
/// `dist/` — 10 GiB is a conservative floor that fails BEFORE a 4-minute
/// build dies at 99% on ENOSPC.
pub const MIN_FREE_DISK_GIB: u64 = 10;

/// Where cargo resolves the `trust` toolchain from (rust-toolchain.toml pins
/// it): the rustup toolchain link, normally a symlink at
/// `$HOME/trust/build/host/stage2`. The gate probes THIS path — the exact
/// resolution the build lane will use — so a broken link fails here, in
/// seconds, with the remediation printed.
pub const TRUST_RUSTUP_TOOLCHAIN_BIN: &str = ".rustup/toolchains/trust/bin";

/// The gate knobs that come off the `cut` command line.
pub struct GateOpts {
    /// Release version being cut, e.g. "0.2.0" — drives the tag gates.
    pub version: String,
    /// `--arm64-only`: single-arch build; skips the x86_64 target probe.
    pub arm64_only: bool,
    /// Recut (spec §5): the notes were already rolled into `## [version]` by
    /// the earlier wedged cut, so the changelog gate judges THAT section — the
    /// fresh `[Unreleased]` scaffold above it is legitimately empty.
    pub recut: bool,
}

/// What the gates learned — everything the cut transcript's `gates` lines
/// print. Informational; the gate *decisions* already happened (any failure
/// returned Err instead).
pub struct GateReport {
    /// Short (8-hex) HEAD sha for the banner.
    pub head_short: String,
    /// Top-level bullet count of the `[Unreleased]` real body.
    pub changelog_entries: usize,
    /// The gh account name, when it could be parsed from `gh auth status`.
    pub gh_account: Option<String>,
    /// The probed Trust rustc path (the rustup `trust` toolchain the build
    /// lane resolves). Always probed — there is no opt-out lane.
    pub trust_rustc: PathBuf,
    /// false under `--arm64-only`.
    pub universal: bool,
    /// Free disk in GiB at gate time.
    pub free_disk_gib: u64,
}

/// Run every gate, in the transcript's order, first failure wins. Cheap and
/// side-effect-free by construction (the one network touch is a fetch): this
/// is the always-on preflight — the optional deep gate (`--gate` →
/// tools/verify.sh --full) layers on top in chunk C, never replaces this.
pub fn run_all(git: &dyn GitRunner, repo: &Path, opts: &GateOpts) -> Result<GateReport> {
    host_gate()?;
    clean_tree(git)?;
    on_main(git)?;
    let head = head_matches_origin(git)?;
    tag_free(git, &opts.version)?;
    let cl = changelog_gate(
        repo,
        if opts.recut {
            &opts.version
        } else {
            "Unreleased"
        },
    )?;
    let gh_account = gh_auth()?;
    locked_metadata_gate(repo)?;
    let trust_rustc = trust_rustc_probe()?;
    let universal = if opts.arm64_only {
        false
    } else {
        x86_target_probe()?;
        true
    };
    let free_disk_gib = disk_gate(repo)?;
    Ok(GateReport {
        head_short: head.chars().take(8).collect(),
        changelog_entries: cl.entries,
        gh_account,
        trust_rustc,
        universal,
        free_disk_gib,
    })
}

/// Prove the committed Cargo.lock already resolves the workspace without any
/// rewrite or network access. App cuts deliberately preserve the independent
/// source version and lockfile, so a stale lock must fail before the ledger
/// claim rather than dirtying the tree during the expensive build.
pub fn locked_metadata_gate(repo: &Path) -> Result<()> {
    let lock_path = repo.join("Cargo.lock");
    let lock_before = fs::read(&lock_path).map_err(|error| {
        Error::new(format!(
            "read committed Cargo.lock before release metadata check: {error}"
        ))
    })?;
    let out = Command::new("cargo")
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .current_dir(repo)
        .output()
        .map_err(|error| Error::new(format!("failed to run locked Cargo metadata: {error}")))?;
    let lock_after = match fs::read(&lock_path) {
        Ok(bytes) => bytes,
        Err(read_error) => {
            fs::write(&lock_path, &lock_before).map_err(|restore_error| {
                Error::new(format!(
                    "Cargo.lock disappeared during locked metadata ({read_error}) and restoring \
                     its exact prior bytes failed: {restore_error}"
                ))
            })?;
            return Err(Error::new(format!(
                "Cargo.lock disappeared during locked metadata ({read_error}); exact prior bytes \
                 were restored"
            )));
        }
    };
    if lock_after != lock_before {
        fs::write(&lock_path, &lock_before).map_err(|error| {
            Error::new(format!(
                "locked Cargo metadata changed Cargo.lock and restoring its exact prior bytes \
                 failed: {error}"
            ))
        })?;
        return Err(Error::new(
            "locked Cargo metadata attempted to rewrite Cargo.lock; exact prior bytes were \
             restored — refresh and commit the lock before cutting"
                .to_string(),
        ));
    }
    if !out.status.success() {
        return Err(Error::new(format!(
            "Cargo.lock is not an exact offline resolution of Cargo.toml — refresh and commit \
             it before cutting: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Releases are cut ONLY on the owner's arm64 Mac (spec §6). Compile-time cfg
/// IS the host check here: the ship binary is always built on the cutting
/// machine via the `cargo ship` run alias — never cross-compiled, never
/// `cargo install`ed (spec decision 13) — so target == host by construction.
pub fn host_gate() -> Result<()> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(())
    } else {
        Err(Error::new(
            "releases are cut only on a macOS arm64 host (codesign/hdiutil/lipo and the \
             Trust toolchain live there)"
                .to_string(),
        ))
    }
}

/// Clean tree: the release commit must be EXACTLY the changelog-roll + ledger
/// changes, and a rejected-push retry does `reset --hard` — stray local edits
/// would be destroyed, so refuse them up front.
pub fn clean_tree(git: &dyn GitRunner) -> Result<()> {
    let out = git_ok(git, &["status", "--porcelain"])?;
    let dirty = out.stdout_utf8();
    if dirty.trim().is_empty() {
        return Ok(());
    }
    let mut lines: Vec<&str> = dirty.lines().take(5).collect();
    if dirty.lines().count() > 5 {
        lines.push("…");
    }
    Err(Error::new(format!(
        "working tree is dirty — commit/stash first (a claim retry resets --hard):\n  {}",
        lines.join("\n  ")
    )))
}

/// On main: the claim pushes to origin/main; cutting from any other branch
/// would either fail the push or, worse, publish a side branch's tree.
pub fn on_main(git: &dyn GitRunner) -> Result<()> {
    let branch = git_ok(git, &["rev-parse", "--abbrev-ref", "HEAD"])?.stdout_utf8();
    let branch = branch.trim();
    if branch == "main" {
        Ok(())
    } else {
        Err(Error::new(format!(
            "on branch {branch:?} — releases are cut only from main"
        )))
    }
}

/// Fetch + require HEAD == origin/main (spec §2 step 1, surfaced early as a
/// gate so it costs seconds). Fail closed when offline: no offline cuts — the
/// ledger claim IS a push. Returns the HEAD sha for the banner.
pub fn head_matches_origin(git: &dyn GitRunner) -> Result<String> {
    git_ok(git, &["fetch", "origin", "main"])
        .map_err(|e| Error::new(format!("cannot reach origin (no offline cuts): {e}")))?;
    let head = rev_parse(git, "HEAD")?;
    let origin_tip = rev_parse(git, "origin/main")?;
    if head != origin_tip {
        return Err(Error::new(format!(
            "HEAD ({head}) != origin/main ({origin_tip}) — pull first"
        )));
    }
    Ok(head)
}

/// Tag vX.Y.Z must be absent BOTH locally and on origin: the publish step mints
/// it late (spec decision 5), so any pre-existing tag means this version was
/// already cut (or half-cut) — colliding with it would re-point a published
/// artifact. Local and remote are checked separately because either alone can
/// be stale.
pub fn tag_free(git: &dyn GitRunner, version: &str) -> Result<()> {
    let tag = format!("v{version}");
    // Local: `rev-parse -q --verify` exits 0 iff the ref EXISTS — existence is
    // the failure here, so this is the one git call whose non-zero exit is the
    // happy path.
    let local = git.git(&["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")])?;
    if local.success() {
        return Err(Error::new(format!(
            "tag {tag} already exists locally — this version was already cut (bump \
             [workspace.package] version's MINOR in Cargo.toml, or delete the stale \
             tag if that cut was abandoned)"
        )));
    }
    let remote = git_ok(
        git,
        &["ls-remote", "--tags", "origin", &format!("refs/tags/{tag}")],
    )?;
    if !remote.stdout_utf8().trim().is_empty() {
        return Err(Error::new(format!(
            "tag {tag} already exists on origin — v{version} was already cut/published \
             elsewhere; bump [workspace.package] version's MINOR in Cargo.toml"
        )));
    }
    Ok(())
}

/// Changelog gates (spec §3), delegated to changelog.rs: the section's real
/// body non-empty, no `'''`. `section` is "Unreleased" for a fresh cut, the
/// version itself for a recut (see [`GateOpts::recut`]).
pub fn changelog_gate(repo: &Path, section: &str) -> Result<changelog::GateSummary> {
    let path = repo.join(changelog::CHANGELOG_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
    if section == "Unreleased" {
        changelog::gate_unreleased(&text)
    } else {
        changelog::gate_section(&text, section)
    }
}

/// `gh auth status` must succeed — the publish half of the cut is all `gh`,
/// and discovering a dead token AFTER the build wastes ten minutes. Returns
/// the account name when parseable (transcript garnish; never load-bearing).
pub fn gh_auth() -> Result<Option<String>> {
    let out = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .map_err(|e| {
            Error::new(format!(
                "failed to run `gh auth status` — is the GitHub CLI installed? ({e})"
            ))
        })?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "`gh auth status` failed — run `gh auth login` first: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // gh has historically split this output between stdout and stderr; scan
    // both for "… account <name> …".
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let account = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|w| {
            (w[0] == "account").then(|| {
                w[1].trim_matches(|c: char| !c.is_alphanumeric() && c != '-')
                    .to_string()
            })
        });
    Ok(account)
}

/// Probe the Trust rustc the native slice uses — the rustup `trust`
/// toolchain that rust-toolchain.toml pins for the whole repo (spec §6
/// buildplan.rs). Always on: the repo compiles with Trust, so a missing or
/// broken toolchain is a broken toolchain, never a fallback. The exact path
/// is printed on failure so the remediation is copy-pasteable.
///
/// The probe COMPILES a trivial program under the same off-switch
/// .cargo/config.toml applies (`-Zno-trust-verify=yes`), not just
/// `--version`: a stage2 whose library build never landed has a runnable
/// rustc but no std rlibs in its sysroot (the 2026-07-07 dry-run failure —
/// every crate E0463s twenty seconds into the real build). Compiling is the
/// only honest check that the toolchain can do what the build lane is about
/// to ask of it; the metadata-only emit keeps it fast (no codegen, no link).
pub fn trust_rustc_probe() -> Result<PathBuf> {
    let home = env::var("HOME")
        .map_err(|_| Error::new("HOME is unset — cannot locate the Trust toolchain"))?;
    let rustc = Path::new(&home)
        .join(TRUST_RUSTUP_TOOLCHAIN_BIN)
        .join("rustc");
    let probe = Command::new(&rustc).arg("--version").output();
    match probe {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            return Err(Error::new(format!(
                "Trust rustc at {} exists but failed --version: {}",
                rustc.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Err(e) => {
            return Err(Error::new(format!(
                "Trust rustc not runnable at {} ({e}) — the repo compiles with Trust \
                 always (rust-toolchain.toml). Rebuild the stage2 toolchain \
                 (`python3 x.py build --stage 2` in $HOME/trust) and ensure the rustup \
                 link points at $HOME/trust/build/host/stage2",
                rustc.display()
            )));
        }
    }
    // Sysroot smoke-compile: same verify off-switch the config lane applies.
    let dir = std::env::temp_dir().join(format!("aterm-trust-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| Error::new(format!("probe tmpdir: {e}")))?;
    let src = dir.join("probe.rs");
    std::fs::write(&src, "fn main() {}\n").map_err(|e| Error::new(format!("probe src: {e}")))?;
    let out = Command::new(&rustc)
        .arg("-Zno-trust-verify=yes")
        .arg("--emit=metadata")
        .arg("--out-dir")
        .arg(&dir)
        .arg(&src)
        .output();
    let result = match out {
        Ok(o) if o.status.success() => Ok(rustc),
        Ok(o) => Err(Error::new(format!(
            "Trust rustc at {} runs but cannot COMPILE (stage2 library missing/stale? \
             rebuild it with `python3 x.py build --stage 2` in $HOME/trust): {}",
            rustc.display(),
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .find(|l| l.starts_with("error"))
                .unwrap_or("(no error line)")
        ))),
        Err(e) => Err(Error::new(format!("probe compile spawn: {e}"))),
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The x86_64 compat slice builds on upstream stable (buildplan.rs pins
/// `RUSTUP_TOOLCHAIN=stable`), so probe STABLE's installed targets — a bare
/// `rustup target list` would resolve the repo's `trust` toolchain via
/// rust-toolchain.toml, and custom toolchains never carry rustup-managed
/// targets. When the target is absent, print the exact remediation and
/// require the explicit `--arm64-only` to proceed single-arch (spec decision
/// 18) — never silently ship a thinner artifact than v0.25 did.
pub fn x86_target_probe() -> Result<()> {
    let out = Command::new("rustup")
        .env("RUSTUP_TOOLCHAIN", "stable")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| Error::new(format!("failed to run rustup ({e}) — is rustup installed?")))?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "`rustup target list --installed` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let installed = String::from_utf8_lossy(&out.stdout);
    if installed.lines().any(|l| l.trim() == "x86_64-apple-darwin") {
        return Ok(());
    }
    Err(Error::new(
        "x86_64-apple-darwin target missing from the stable toolchain — a universal \
         build is impossible.\n  \
         fix:  rustup +stable target add x86_64-apple-darwin\n  \
         or:   re-run with --arm64-only to deliberately ship a single-arch build"
            .to_string(),
    ))
}

/// Free-disk gate via `df -Pk` (POSIX output format; std has no statfs).
/// Returns free GiB for the transcript.
pub fn disk_gate(repo: &Path) -> Result<u64> {
    let out = Command::new("df")
        .arg("-Pk")
        .arg(repo)
        .output()
        .map_err(|e| Error::new(format!("failed to run df: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "df -Pk failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // -P guarantees one header line then one line per filesystem; field 4 is
    // "Available" in 1024-byte blocks.
    let avail_kib: u64 = text
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().nth(3))
        .and_then(|f| f.parse().ok())
        .ok_or_else(|| Error::new(format!("could not parse df -Pk output:\n{text}")))?;
    let free_gib = avail_kib / (1024 * 1024);
    if free_gib < MIN_FREE_DISK_GIB {
        return Err(Error::new(format!(
            "only {free_gib} GiB free — a universal release build needs at least \
             {MIN_FREE_DISK_GIB} GiB (two release target trees + dist/ staging)"
        )));
    }
    Ok(free_gib)
}

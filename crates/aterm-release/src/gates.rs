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
use crate::mirror;

/// Free-disk floor for a cut. A universal release build carries two full
/// `--release` target trees (Trust arm64 + rustup x86_64, both with
/// `CARGO_PROFILE_RELEASE_DEBUG=1` for the dSYM) plus the .app/DMG staging in
/// `dist/` — 10 GiB is a conservative floor that fails BEFORE a 4-minute
/// build dies at 99% on ENOSPC.
pub const MIN_FREE_DISK_GIB: u64 = 10;

/// The Trust toolchain's stage2 tool dir (`targo`, `trustc`, `trustdoc`, …).
/// `TRUST_STAGE2_BIN` overrides (same contract as tools/verify.sh); the default
/// is `$HOME/trust/build/host/stage2/bin`. Resolved to the PHYSICAL path: Trust's
/// `build/host` is commonly a target-triple symlink and the protected Trust
/// drivers refuse a symlinked toolchain path. The gates resolve the trust-named
/// binaries directly rather than a PATH `cargo` — correctness does not depend
/// on the operator's rustup state. (An earlier revision of this comment claimed
/// the stock-name `{rustc,cargo}` compatibility entries were purged from stage2;
/// current stage2 builds ship them again, and the rustup `trust` link over the
/// stage2 dir is exactly what makes `cargo ship …` dispatch into Trust — the
/// front door `provision` audits. The gates still never rely on it.)
pub fn trust_stage2_bin() -> Result<PathBuf> {
    // Resolution order, first hit wins. No step requires anyone to remember an
    // environment variable: a toolchain installed by `atpkg install trust` is found
    // automatically, which is the ordinary way to get one.
    //
    //   1. $TRUST_STAGE2_BIN — an explicit override for an unusual location. This is
    //      a LOCATION knob, not a trust knob: it cannot change what anything trusts,
    //      only where the compiler is found.
    //   2. the atpkg store, resolved exactly as atpkg resolves it (so a configured
    //      `[packages].prefix` — e.g. a root-owned system prefix — is honoured).
    //   3. $HOME/trust/build/host/stage2/bin — a toolchain built from source with x.py.
    let candidates = trust_stage2_candidates();
    let mut tried = Vec::new();
    for dir in candidates {
        match fs::canonicalize(&dir) {
            Ok(resolved) if resolved.join("trustc").is_file() => return Ok(resolved),
            _ => tried.push(dir.display().to_string()),
        }
    }
    Err(Error::new(format!(
        "no Trust toolchain found. Looked in: {}. Install one with `atpkg install trust`, \
         build one with `python3 x.py build --stage 2` in $HOME/trust, or point \
         TRUST_STAGE2_BIN at a stage2 bin dir",
        tried.join(", ")
    )))
}

/// The ordered places a Trust toolchain may live. Split out so the order is readable
/// and testable without a filesystem.
fn trust_stage2_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = env::var_os("TRUST_STAGE2_BIN") {
        out.push(PathBuf::from(explicit));
    }
    // The atpkg store, via atpkg's own config + prefix validation.
    let home = aterm_types::dirs::home_dir();
    let configured = atpkg::config::load().prefix_path(home.as_deref());
    if let Some(layout) = atpkg::store::resolve(configured.as_deref()) {
        out.push(layout.program_current("trust").join("bin"));
    }
    if let Ok(home) = env::var("HOME") {
        out.push(Path::new(&home).join("trust/build/host/stage2/bin"));
    }
    out
}

/// The `targo` build driver from the stage2 tool dir. All native-lane builds and
/// metadata queries go through THIS binary — never a PATH `cargo`, which since
/// the stock-name purge resolves to a rustup shim with nothing behind it.
pub fn resolve_targo() -> Result<PathBuf> {
    let targo = trust_stage2_bin()?.join("targo");
    if targo.is_file() {
        Ok(targo)
    } else {
        Err(Error::new(format!(
            "targo missing at {} — the stage2 toolchain is incomplete; rebuild it \
             (`python3 x.py build --stage 2` in $HOME/trust) or point TRUST_STAGE2_BIN at a \
             stage2 bin dir that carries targo",
            targo.display()
        )))
    }
}

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
    /// This cut publishes nowhere the real channel can be compared against —
    /// `--dry-run` (no uploads at all) or `--rehearse OWNER/REPO` (uploads to a
    /// scratch repo). The channel-version gate is inapplicable then, because the
    /// public channel is not the destination and cannot be expected to carry
    /// this version.
    ///
    /// This is DERIVED FROM THE CLI FLAGS, never from the environment. It
    /// replaces the former `ATERM_SKIP_CHANNEL_VERSION_GATE` env opt-out, which
    /// violated the repo rule that verification is default-on and fail-closed
    /// with no ambient skip switches: an exported variable would silently
    /// disable the gate for a REAL cut, which is precisely the failure that
    /// shipped the mismatched v0.6.0 tag.
    pub offline: bool,
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
    /// The probed trustc path (the Trust stage2 compiler the native build lane
    /// resolves via targo). Always probed — there is no opt-out lane.
    pub trustc: PathBuf,
    /// false under `--arm64-only`.
    pub universal: bool,
    /// Free disk in GiB at gate time.
    pub free_disk_gib: u64,
    /// `Some(version)` when the public update channel's source tree was read and
    /// carries exactly this version; `None` when there is no channel configured,
    /// no manifest on it yet, or the gate was explicitly skipped.
    pub channel_version: Option<String>,
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
    let trustc = trustc_probe(repo)?;
    let universal = if opts.arm64_only {
        false
    } else {
        x86_target_probe()?;
        true
    };
    let free_disk_gib = disk_gate(repo)?;
    // Last, because it is the only gate that talks to the public channel: the cheap
    // local refusals should all have fired before we spend a network round trip.
    let channel_version = channel_version_gate(repo, &opts.version, opts.offline)?;
    Ok(GateReport {
        head_short: head.chars().take(8).collect(),
        changelog_entries: cl.entries,
        gh_account,
        trustc,
        universal,
        free_disk_gib,
        channel_version,
    })
}

/// Prove the public channel's source tree already carries the version being cut.
///
/// The pure comparison is [`mirror::check_channel_version`]; this is only its I/O
/// shell — resolve the channel slug from the local manifest, read that channel's
/// `Cargo.toml` at `main`, hand both to the pure function.
///
/// `Ok(None)` when there is nothing to check: no `update_channel` configured, or
/// the channel carries no workspace manifest. `Ok(Some(version))` on agreement.
///
/// Fetch failures fail CLOSED. An unreachable channel is indistinguishable from a
/// channel that disagrees, and the whole purpose of the gate is to stop guessing
/// about the channel's contents. Cutting anyway is the behaviour that shipped the
/// mismatched v0.6.0 tag. `offline` (from `--dry-run` / `--rehearse`, never from
/// the environment) is the ONLY opt-out, and it cannot apply to a real cut.
///
/// In particular a bare 404 is NOT taken as "no manifest": GitHub returns 404 for an
/// unauthorized repository as well as a missing file, so an absent channel token
/// would otherwise disable this gate silently. The 404 path probes the repository
/// itself and only skips when the repository is demonstrably readable.
pub fn channel_version_gate(repo: &Path, version: &str, offline: bool) -> Result<Option<String>> {
    let local = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("cannot read workspace Cargo.toml: {e}")))?;
    let Some(slug) = mirror::update_channel_slug(&local)? else {
        return Ok(None);
    };
    if offline {
        return Ok(None);
    }

    let out = channel_api(&format!("repos/{slug}/contents/Cargo.toml?ref=main"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if !is_not_found(&err) {
            return Err(Error::new(format!(
                "cannot read Cargo.toml from the public channel {slug}: {}",
                err.trim()
            )));
        }
        // A 404 is AMBIGUOUS and must not be trusted as "no manifest". GitHub also
        // answers 404 for a repository the credential cannot read, precisely so it
        // leaks nothing — so a missing or wrong channel token would otherwise make
        // this gate silently skip itself, which is the opposite of failing closed.
        // Distinguish the two by asking whether the REPO is readable at all.
        let probe = channel_api(&format!("repos/{slug}"))?;
        if !probe.status.success() {
            return Err(Error::new(format!(
                "the public channel {slug} is not readable with the available credential, so \
                 this cut cannot confirm the channel carries v{version}. GitHub answers 404 \
                 for both \"no such file\" and \"not authorized\", so treating this as \
                 \"nothing to compare\" would silently disable the check. Provide \
                 ~/.secrets/gh_access_token_alabsystems. There is no env opt-out: a \
                 cut that must not consult the channel is a --dry-run or a \
                 --rehearse, both of which set this gate aside explicitly."
            )));
        }
        // Repo readable, file absent: the genuine empty-channel/first-publish case.
        return Ok(None);
    }

    let body = String::from_utf8_lossy(&out.stdout).to_string();
    match mirror::check_channel_version(version, &body)? {
        mirror::ChannelVersion::Agrees => Ok(Some(version.to_string())),
        mirror::ChannelVersion::NoManifest => Ok(None),
    }
}

/// One `gh api` call against the public channel, credentialed for the release org.
///
/// The channel is a DIFFERENT org than the dev remote, and `gh auth`'s account is
/// the dev one, which cannot read it. The token goes in the environment, never in
/// argv, so it cannot surface in a process listing or a transcript.
fn channel_api(path: &str) -> Result<std::process::Output> {
    let mut command = Command::new("gh");
    command
        .arg("api")
        .arg(path)
        .args(["-H", "Accept: application/vnd.github.raw"]);
    if let Some(token) = crate::publish::channel_token() {
        command.env("GH_TOKEN", token);
    }
    command
        .output()
        .map_err(|e| Error::new(format!("cannot run gh to read the public channel: {e}")))
}

/// Does this `gh` stderr report a 404? `gh` prints `gh: Not Found (HTTP 404)` plus
/// the JSON body, so either spelling is enough — and both are matched because the
/// caller does NOT act on the answer alone (see the repo probe above).
fn is_not_found(stderr: &str) -> bool {
    stderr.contains("404") || stderr.contains("Not Found")
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
    let targo = resolve_targo()?;
    let out = Command::new(&targo)
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .current_dir(repo)
        .output()
        .map_err(|error| Error::new(format!("failed to run locked targo metadata: {error}")))?;
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

/// The native-lane rustflags the repo's `.cargo/config.toml` applies to the
/// Trust slice's triple — the ONE temporary verification opt-out. Read from the
/// file, never hardcoded: a hardcoded off-switch spelling drifted from the
/// config twice (`-Zno-trust-verify=yes` vs `-Ztrust-verify=off`) and either
/// direction of that drift kills the cut in the build step. Empty when the
/// table is gone (the Trust-Std campaign greened): the probe then compiles
/// batteries-on, which is exactly what the build lane will do.
fn native_lane_rustflags(repo: &Path) -> Result<Vec<String>> {
    let path = repo.join(".cargo/config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::new(format!("read {}: {error}", path.display())));
        }
    };
    let value: toml::Value = text
        .parse()
        .map_err(|error| Error::new(format!("parse {}: {error}", path.display())))?;
    Ok(value
        .get("target")
        .and_then(|targets| targets.get("aarch64-apple-darwin"))
        .and_then(|target| target.get("rustflags"))
        .and_then(|flags| flags.as_array())
        .map(|flags| {
            flags
                .iter()
                .filter_map(|flag| flag.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

/// Probe the trustc the native slice uses — the Trust stage2 compiler that
/// `targo` drives (spec §6 buildplan.rs). Always on: the repo compiles with
/// Trust, so a missing or broken toolchain is a broken toolchain, never a
/// fallback. The exact path is printed on failure so the remediation is
/// copy-pasteable.
///
/// The probe COMPILES a trivial program under the exact rustflags
/// .cargo/config.toml applies to the native lane, not just `--version`: a
/// stage2 whose library build never landed has a runnable trustc but no std
/// rlibs in its sysroot (the 2026-07-07 dry-run failure — every crate E0463s
/// twenty seconds into the real build), and an off-switch spelling this trustc
/// does not parse fails every unit the same way. Compiling is the only honest
/// check that the toolchain can do what the build lane is about to ask of it;
/// the metadata-only emit keeps it fast (no codegen, no link).
pub fn trustc_probe(repo: &Path) -> Result<PathBuf> {
    let trustc = trust_stage2_bin()?.join("trustc");
    let probe = Command::new(&trustc).arg("--version").output();
    match probe {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            return Err(Error::new(format!(
                "trustc at {} exists but failed --version: {}",
                trustc.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Err(e) => {
            return Err(Error::new(format!(
                "trustc not runnable at {} ({e}) — the repo compiles with Trust always. \
                 Rebuild the stage2 toolchain (`python3 x.py build --stage 2` in $HOME/trust) \
                 or point TRUST_STAGE2_BIN at a stage2 bin dir that carries trustc",
                trustc.display()
            )));
        }
    }
    // Sysroot smoke-compile under the exact native-lane flags the config applies.
    let flags = native_lane_rustflags(repo)?;
    let dir = std::env::temp_dir().join(format!("aterm-trust-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| Error::new(format!("probe tmpdir: {e}")))?;
    let src = dir.join("probe.rs");
    std::fs::write(&src, "fn main() {}\n").map_err(|e| Error::new(format!("probe src: {e}")))?;
    let out = Command::new(&trustc)
        .args(&flags)
        .arg("--emit=metadata")
        .arg("--out-dir")
        .arg(&dir)
        .arg(&src)
        .output();
    let result = match out {
        Ok(o) if o.status.success() => Ok(trustc),
        Ok(o) => Err(Error::new(format!(
            "trustc at {} runs but cannot COMPILE under the native-lane rustflags {flags:?} \
             (stage2 library missing/stale — rebuild with `python3 x.py build --stage 2` in \
             $HOME/trust — or the config's off-switch spelling does not match this trustc; \
             `trustc -Z help | grep trust-verify` decides): {}",
            trustc.display(),
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
        .map_err(|e| {
            // Name the escape hatch. rustup is NOT this repo's toolchain — THE
            // toolchain is the Trust stage2 tree — and it is wanted here for
            // exactly one thing: upstream stable's x86_64-apple-darwin std, which
            // Trust does not have. So on a Trust-only machine this is not a broken
            // setup to go fix; it is a choice about what to ship, and the operator
            // needs to be told that rather than sent to install a toolchain manager
            // the repo otherwise refuses. Its twin in buildplan.rs already says so.
            Error::new(format!(
                "failed to run rustup ({e}). The x86_64 compat slice needs upstream \
                 stable's std for that target (Trust has none — the one documented \
                 exception to the single-Trust lane). Either install rustup and run \
                 `rustup +stable target add x86_64-apple-darwin`, or pass --arm64-only \
                 to ship an Apple-Silicon-only build deliberately."
            ))
        })?;
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

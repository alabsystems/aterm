// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI surface (release spec §5): hand-rolled `std::env::args` parsing (no
//! third-party arg crate — same rule as `aterm-ctl`) for the whole command
//! surface: `cut [--dry-run] [--resume] [--abandon vX.Y.Z]
//! [--set-version X.Y.Z]
//! [--min-build N] [--gate] [--rehearse OWNER/REPO] [--arm64-only]`,
//! `recover vX.Y.Z <claim-sha> --old-publisher-stopped`, `status`,
//! `verify [vX.Y.Z]`, `yank <build>`.

use std::process::Command;

use crate::ledger::{self, Error};
use crate::{publish, verify};

pub const USAGE: &str = "aterm-release — the `cargo ship` release cutter

USAGE
  cargo ship cut [--dry-run] [--resume] [--abandon vX.Y.Z] [--set-version X.Y.Z]
                 [--min-build N] [--gate] [--rehearse OWNER/REPO]
                 [--arm64-only]
      Cut a release: gates → ledger claim → universal build → bundle/sign/DMG
      → draft-first publish → late tag → flip → verify.
        --dry-run          gates + provisional number + full local build into
                           dist/; zero commits, zero uploads
        --resume           re-enter the journaled cut (dist/cut-state.toml) at
                           its first incomplete step
        --abandon vX.Y.Z   delete that version's draft release + the local
                           journal (the claim commit stays; a later cut recuts)
        --set-version X.Y.Z
                           override the version derived from
                           [workspace.package] version (DEV → 0)
        --min-build N      emit an operator apply floor into the manifest
        --gate             additionally run tools/verify.sh --full inline
        --rehearse O/R     full real cut published to the scratch repo O/R
                           (provisional number, no ledger push, no tag)
        --arm64-only       ship a single-arch build (explicit opt-out)

  cargo ship status        version · ledger tail · dangling claims · newest
                           published build
  cargo ship recover vX.Y.Z <full-claim-sha> --old-publisher-stopped
                           explicit killed-machine recovery: exact-CAS rotate
                           its fence only after operator stop proof; abandon
                           unpublished state or validate + finish a published
                           exact-identity cut
  cargo ship verify [vX.Y.Z]
                           re-run the post-publish check anytime
  cargo ship yank <build>  publish + fully verify a min_build-ratcheted
                           successor FIRST; only then remove the inert bad
                           tag and release (crash-convergent cleanup)
";

/// A parsed invocation. `Cut.abandon` rides outside [`publish::CutOptions`]
/// because abandoning is not a cut — it never reaches the pipeline.
#[derive(Debug, PartialEq)]
pub enum Cmd {
    Help,
    Cut {
        opts: publish::CutOptions,
        abandon: Option<String>,
    },
    Status,
    Recover {
        version: String,
        owner: String,
        release_credentials: Option<std::path::PathBuf>,
    },
    Verify {
        version: Option<String>,
    },
    Yank {
        build: u64,
    },
}

/// Entry point for the whole binary — `main()` delegates here so every code
/// path (parsing included) is reachable from tests without spawning a
/// process. Exit codes: 0 ok, 1 pipeline failure, 2 usage error.
pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aterm-release: {e}\n");
            eprint!("{USAGE}");
            return 2;
        }
    };
    match dispatch(cmd) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("aterm-release: FAILED — {e}");
            1
        }
    }
}

/// Pure parser (unit-tested in tests/resume.rs).
pub fn parse(args: &[String]) -> std::result::Result<Cmd, String> {
    let mut it = args.iter().map(String::as_str);
    let Some(cmd) = it.next() else {
        return Ok(Cmd::Help);
    };
    match cmd {
        "help" | "--help" | "-h" => Ok(Cmd::Help),
        "cut" => parse_cut(&mut it),
        "status" => {
            if let Some(extra) = it.next() {
                return Err(format!("status takes no arguments (got {extra:?})"));
            }
            Ok(Cmd::Status)
        }
        "recover" => {
            let version = normalize_version(
                it.next()
                    .ok_or("recover needs a version and full claim SHA")?,
            )?;
            let owner = it
                .next()
                .ok_or("recover needs the full claim SHA printed by the release lease")?
                .to_string();
            let acknowledgement = it.next().ok_or(
                "recover requires --old-publisher-stopped after independently proving the old publisher exited",
            )?;
            if acknowledgement != publish::RECOVERY_STOPPED_PROCESS_FLAG {
                return Err(format!(
                    "recover requires --old-publisher-stopped, got {acknowledgement:?}"
                ));
            }
            // Recovery signs, so it accepts the one credentials flag — and nothing
            // else. It used to refuse every extra argument, which was correct when
            // the key was ambient and is wrong now that it must be named.
            let mut release_credentials: Option<std::path::PathBuf> = None;
            while let Some(arg) = it.next() {
                if arg == "--release-credentials" {
                    if release_credentials.is_some() {
                        return Err("--release-credentials given twice".to_string());
                    }
                    release_credentials = Some(std::path::PathBuf::from(
                        it.next().ok_or("--release-credentials needs a path")?,
                    ));
                } else {
                    return Err(format!(
                        "recover takes a version, full claim SHA, --old-publisher-stopped, \
                         and optionally --release-credentials (got {arg:?})"
                    ));
                }
            }
            Ok(Cmd::Recover {
                version,
                owner,
                release_credentials,
            })
        }
        "verify" => {
            let version = it.next().map(normalize_version).transpose()?;
            if let Some(extra) = it.next() {
                return Err(format!("verify takes at most one version (got {extra:?})"));
            }
            Ok(Cmd::Verify { version })
        }
        "yank" => {
            let build = it
                .next()
                .ok_or("yank needs the bad release's build number: cargo ship yank <build>")?;
            let build: u64 = build
                .parse()
                .map_err(|_| format!("yank: {build:?} is not a build number (u64)"))?;
            if let Some(extra) = it.next() {
                return Err(format!(
                    "yank takes exactly one build number (got {extra:?})"
                ));
            }
            Ok(Cmd::Yank { build })
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

fn parse_cut<'a>(it: &mut impl Iterator<Item = &'a str>) -> std::result::Result<Cmd, String> {
    let mut opts = publish::CutOptions::default();
    let mut abandon: Option<String> = None;
    while let Some(flag) = it.next() {
        match flag {
            "--dry-run" => opts.dry_run = true,
            "--resume" => opts.resume = true,
            "--gate" => opts.gate = true,
            "--arm64-only" => opts.arm64_only = true,
            // The ONE signing input. A path in the command, never an ambient file:
            // "what signed this?" is answered by reading the command that ran.
            "--release-credentials" => {
                if opts.release_credentials.is_some() {
                    return Err("--release-credentials given twice".to_string());
                }
                opts.release_credentials = Some(std::path::PathBuf::from(
                    it.next().ok_or("--release-credentials needs a path")?,
                ));
            }
            "--abandon" => {
                let v = it.next().ok_or("--abandon needs a version (vX.Y.Z)")?;
                abandon = Some(normalize_version(v)?);
            }
            "--set-version" => {
                let v = it.next().ok_or("--set-version needs a version (X.Y.Z)")?;
                opts.set_version = Some(normalize_version(v)?);
            }
            "--min-build" => {
                let n = it.next().ok_or("--min-build needs a number")?;
                let n: u64 = n
                    .parse()
                    .map_err(|_| format!("--min-build: {n:?} is not a u64"))?;
                opts.min_build = Some(n);
            }
            "--rehearse" => {
                let slug = it.next().ok_or("--rehearse needs OWNER/REPO")?;
                let ok = matches!(slug.split('/').collect::<Vec<_>>().as_slice(),
                    [o, r] if !o.is_empty() && !r.is_empty());
                if !ok {
                    return Err(format!("--rehearse: {slug:?} is not OWNER/REPO"));
                }
                opts.rehearse = Some(slug.to_string());
            }
            other => return Err(format!("unknown cut flag {other:?}")),
        }
    }
    // Mode exclusivity: each of abandon/resume is a whole flow of its own —
    // silently ignoring a second flag would do something the operator did not
    // ask for, on the one command where that costs a burned ledger number.
    if abandon.is_some()
        && (opts.dry_run
            || opts.resume
            || opts.gate
            || opts.arm64_only
            || opts.set_version.is_some()
            || opts.min_build.is_some()
            || opts.rehearse.is_some())
    {
        return Err("--abandon combines with no other cut flag".to_string());
    }
    if opts.resume
        && (opts.dry_run
            || opts.gate
            || opts.arm64_only
            || opts.set_version.is_some()
            || opts.min_build.is_some()
            || opts.rehearse.is_some())
    {
        return Err(
            "--resume combines with no other cut flag (the journal already fixed the \
             cut's parameters)"
                .to_string(),
        );
    }
    if opts.dry_run && opts.rehearse.is_some() {
        return Err("--dry-run and --rehearse are mutually exclusive".to_string());
    }
    Ok(Cmd::Cut { opts, abandon })
}

/// Accept "0.2.0" or "v0.2.0"; store the bare canonical MAJOR.MINOR.PATCH
/// everywhere. Two-component spellings are the retired scheme and are refused.
fn normalize_version(v: &str) -> std::result::Result<String, String> {
    let bare = v.strip_prefix('v').unwrap_or(v);
    ledger::check_version_shape(bare).map_err(|e| e.to_string())?;
    Ok(bare.to_string())
}

fn dispatch(cmd: Cmd) -> ledger::Result<()> {
    match cmd {
        Cmd::Help => {
            print!("{USAGE}");
            Ok(())
        }
        Cmd::Cut {
            abandon: Some(v), ..
        } => verify::run_abandon(&repo_root()?, &v),
        Cmd::Cut { opts, .. } => publish::run_cut(&repo_root()?, &opts),
        Cmd::Status => verify::run_status(&repo_root()?),
        Cmd::Recover {
            version,
            owner,
            release_credentials,
        } => {
            // Recovery signs too, so it needs the SAME explicit credentials as a
            // fresh cut. The old code could recover only because the material was
            // ambient; with one flag, every signing entry point must name it.
            let creds = release_credentials
                .as_deref()
                .map(crate::sign::ReleaseCredentials::load)
                .transpose()
                .map_err(|e| e.to_string())?;
            publish::run_recover_lost(&repo_root()?, &version, &owner, true, creds.as_ref())
        }
        Cmd::Verify { version } => verify::run_verify(&repo_root()?, version),
        Cmd::Yank { build } => verify::run_yank(&repo_root()?, build),
    }
}

/// The workspace root, from git — the `cargo ship` alias may be invoked from
/// any subdirectory of the checkout, and every pipeline path is repo-relative.
fn repo_root() -> ledger::Result<std::path::PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| Error::new(format!("failed to run git rev-parse --show-toplevel: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(
            "not inside a git checkout — run `cargo ship` from the aterm workspace".to_string(),
        ));
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(std::path::PathBuf::from(root))
}

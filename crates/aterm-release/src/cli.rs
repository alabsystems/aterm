// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI surface (release spec §5): hand-rolled `std::env::args` parsing (no
//! third-party arg crate — same rule as `aterm-ctl`) for the whole command
//! surface: `cut [--dry-run] [--resume] [--abandon vX.Y.Z]
//! [--min-build N] [--gate] [--rehearse OWNER/REPO] [--arm64-only]
//! [--release-credentials <profile.toml>]`,
//! `recover vX.Y.Z <claim-sha> --old-publisher-stopped`, `status`,
//! `verify [vX.Y.Z]`,
//! `yank <build> [--release-credentials <profile.toml>]`.

use std::process::Command;

use crate::ledger::{self, Error};
use crate::{publish, verify};

/// The retired pre-roster acknowledgement. A leftover spelling in a script or a
/// runbook is refused by name, in one sentence, rather than as an unknown flag.
pub const RETIRED_STRAND_FLAG: &str = "--strand-pre-roster-clients";

/// The one sentence [`RETIRED_STRAND_FLAG`] is refused with.
pub const RETIRED_STRAND_REFUSAL: &str = "--strand-pre-roster-clients is retired: installs older \
     than v0.21.0 are abandoned and every current client authorizes a release by the machine \
     roster alone, so a rostered key needs no acknowledgement — drop the flag";

pub const USAGE: &str = "aterm-release — the `targo --unverified ship` release cutter

USAGE
  targo --unverified ship linux-build --version X.Y.Z --build-number N
                 --commit FULL-CLAIM-SHA --out EXISTING-PRIVATE-DIRECTORY
      Local native Linux worker: sealed Trust build + provenance handoff.
      No signing or publication. Run on both x86_64 and aarch64 Linux,
      then give the complete handoff directory to the canonical cutter.

  targo --unverified ship cut [--dry-run] [--resume] [--abandon vX.Y.Z]
                 [--min-build N] [--gate] [--rehearse OWNER/REPO]
                 [--arm64-only] [--no-paint-smoke] [--release-credentials <profile.toml>]
                 [--linux-artifacts DIRECTORY] [--linux-target aarch64|x86_64]...
      Cut a release: gates → ledger claim → universal build → bundle/sign/DMG
      → draft-first publish → late tag → flip → verify.
        --dry-run          gates + provisional number + full local build into
                           dist/; zero commits, zero uploads
        --resume           re-enter the journaled cut (dist/cut-state.toml) at
                           its first incomplete step
        --abandon vX.Y.Z   delete that version's draft release + the local
                           journal (the claim commit stays; a later cut recuts)
        --retire-unmirrored vX.Y.Z
                           release the lease and retire the journal, leaving the
                           origin release exactly as it is. The supported exit
                           for a cut that flipped on the origin but whose mirror
                           step the fleet's roster floor now refuses (a roster
                           join landed between the origin flip and the public
                           flip): the public channel never made it the head,
                           and the next cut, attributed under the current
                           generation, supersedes it
        --min-build N      emit an operator apply floor into the manifest
        --gate             additionally run tools/verify.sh --full inline
        --rehearse O/R     full real cut published to the scratch repo O/R
                           (provisional number, no ledger push, no tag)
        --arm64-only       ship a single-arch build (explicit opt-out)
        --linux-artifacts DIRECTORY
                           require declared Linux handoffs before signing;
                           missing workers pause the journaled cut for resume
        --linux-target ARCH
                           explicit native subset (repeatable); defaults to both
        --no-paint-smoke   EMERGENCY ONLY: skip the self-check's paint smoke
                           (the 29-keystroke pixel proof that the just-built
                           bundle actually paints its flagship effect — the
                           check born from v0.48.0/v0.49.0 shipping the rainbow
                           trail dark, docs/RELEASE-PROOF-DISCIPLINE.md).
                           Refused on a notarized real cut unless
                           ATERM_NO_PAINT_SMOKE_ACK=this-cut-may-ship-dark is
                           also set
        --release-credentials <profile.toml>
                           the ONE signing input: the credentials profile this
                           cut signs with. Omitted, the machine key provisioned
                           at ~/.aterm/machine.key is used; a signature-required
                           cut with neither refuses before it claims. The key
                           must belong to a machine the master-signed roster
                           names and has not revoked

  targo --unverified ship provision --id <machine-id> [--check] [--cert-dir <folder>]
                           ON A BARE MACHINE, RUN tools/bootstrap-publisher.sh
                           FIRST — provision refuses without a Trust toolchain.
                           Then: make THIS machine a publisher. Seeds the newest
                           master-signed roster pair from the channel release
                           into dist/, audits the WHOLE publishing stack — Trust
                           stage2 (real smoke-compile), the targo/tippy/ty
                           drivers, the store's targo + trustc front door
                           (rustup's link reported informationally), the stable x86_64
                           slice, Apple identity + live-tested notary
                           credential, the credentials profile, gh auth, channel
                           token — each gap with its exact remedy, and only on a
                           CLEAN pass mints this machine's key via the join
                           ceremony (the paper phrase, typed once, is the only
                           input; a roster id is irreversible and is never
                           consumed on a machine that cannot release).
                           Idempotent: a provisioned machine is audited and
                           bound through the real authorize_cut gate, never
                           re-minted. Ends in a READY TO CUT verdict.
        --cert-dir <folder>
                           the one visible folder the Apple browser errand may
                           use: the certificate request is copied there for the
                           upload dialog, and the issued .cer is looked for there
                           (and in ~/.aterm/apple). Without it every read stays
                           inside ~/.aterm/apple — provision never touches
                           ~/Downloads or another folder macOS guards on its own
                           initiative; naming one is the consent, and a note says
                           so before the errand.
        --check            audit only: no mint, no dist/ writes. Exits non-zero
                           if anything is open, so a caller can gate on it. The
                           mode to run when something is wrong and you do not
                           yet want to touch the machine.

  targo --unverified ship status        version · ledger tail · dangling claims · newest
                           published build
  targo --unverified ship recover vX.Y.Z <full-claim-sha> --old-publisher-stopped
        [--release-credentials <profile.toml>] [--no-draft-was-posted]
                           explicit killed-machine recovery: exact-CAS rotate
                           its fence only after operator stop proof; abandon
                           unpublished state or validate + finish a published
                           exact-identity cut
  targo --unverified ship verify [vX.Y.Z]
                           re-run the post-publish check anytime
  targo --unverified ship yank <build> [--release-credentials <profile.toml>]
                           publish + fully verify a min_build-ratcheted
                           successor FIRST; only then remove the inert bad
                           tag and release (crash-convergent cleanup). That
                           successor is a REAL cut, so it takes the cut's
                           signing input and means exactly what it means there
                           — with the paper master armed it refuses pre-claim
                           without it, having deleted nothing
";

/// A parsed invocation. `Cut.abandon` rides outside [`publish::CutOptions`]
/// because abandoning is not a cut — it never reaches the pipeline.
#[derive(Debug, PartialEq)]
pub enum Cmd {
    Help,
    LinuxBuild {
        version: String,
        build: u64,
        commit: String,
        out: std::path::PathBuf,
    },
    Cut {
        opts: publish::CutOptions,
        abandon: Option<String>,
        retire_unmirrored: Option<String>,
    },
    Provision {
        id: String,
        check: bool,
        /// `--cert-dir`: the ONE visible folder the browser errand may use — the request
        /// is copied there and the certificate looked for there. `None` keeps every read
        /// inside `~/.aterm/apple`; `~/Downloads` is never touched unasked.
        cert_dir: Option<std::path::PathBuf>,
    },
    Status,
    Recover {
        version: String,
        owner: String,
        release_credentials: Option<std::path::PathBuf>,
        /// `--no-draft-was-posted`: the operator has checked the releases page and
        /// answers for a lost journal that no create POST ever landed.
        no_draft_posted: bool,
    },
    Verify {
        version: Option<String>,
    },
    Yank {
        build: u64,
        /// A yank PUBLISHES a successor before it deletes anything, so it takes
        /// the cut's signing inputs — and since the paper master was armed it
        /// must, or that cut refuses pre-claim. See [`verify::YankOptions`].
        opts: verify::YankOptions,
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
    let outcome = dispatch(cmd);
    // The ONE flush in the binary, and it earns its place. Everything above prints to
    // stdout; this prints to stderr. Under `… 2>&1 | tee provision.log` — the obvious
    // thing to do with a run you may have to show someone — stdout block-buffers while
    // stderr does not, so the summary landed ABOVE the evidence it summarises. Two lines
    // of code and the log becomes the document the terminal showed.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    match outcome {
        Ok(()) => 0,
        Err(e) => {
            // A message that names its own verdict word KEEPS it. `provision` ends in
            // "NOT DONE — 1 waiting: the certificate errand is at Apple", whose remedy is
            // to wait; printing FAILED in front of that is the tool contradicting itself
            // in the one line the shell shows. `tally`'s own doc already conceded the
            // point — the counting phrase was fixed and the sentence around it was not.
            //
            // The exit code stays 1 either way: a script must never read "waiting" as
            // ready.
            if e.to_string().starts_with("NOT DONE") {
                eprintln!("aterm-release: {e}");
            } else {
                eprintln!("aterm-release: FAILED — {e}");
            }
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
        "linux-build" => {
            let mut values = std::collections::BTreeMap::new();
            while let Some(flag) = it.next() {
                if !["--version", "--build-number", "--commit", "--out"].contains(&flag) {
                    return Err(format!("unknown linux-build flag {flag:?}"));
                }
                let value = it
                    .next()
                    .ok_or_else(|| format!("{flag} requires a value"))?;
                if value.is_empty() || values.insert(flag, value).is_some() {
                    return Err(format!("empty or duplicated linux-build {flag}"));
                }
            }
            let version = normalize_version(
                values
                    .get("--version")
                    .ok_or("linux-build needs --version")?,
            )?;
            let build = values
                .get("--build-number")
                .ok_or("linux-build needs --build-number")?
                .parse::<u64>()
                .map_err(|_| "linux-build requires a positive u64 build number")?;
            let commit = values
                .get("--commit")
                .ok_or("linux-build needs --commit")?
                .to_string();
            crate::buildplan::linux::check_identity(&version, build, &commit)?;
            let out =
                std::path::PathBuf::from(values.get("--out").ok_or("linux-build needs --out")?);
            Ok(Cmd::LinuxBuild {
                version,
                build,
                commit,
                out,
            })
        }
        "provision" => {
            let mut id: Option<String> = None;
            let mut check = false;
            let mut cert_dir: Option<std::path::PathBuf> = None;
            while let Some(flag) = it.next() {
                match flag {
                    "--id" => {
                        if id.is_some() {
                            return Err("--id given twice".to_string());
                        }
                        id = Some(
                            it.next()
                                .ok_or("--id needs a machine id (e.g. m2)")?
                                .to_string(),
                        );
                    }
                    "--check" => check = true,
                    "--cert-dir" => {
                        if cert_dir.is_some() {
                            return Err("--cert-dir given twice".to_string());
                        }
                        let dir = it
                            .next()
                            .ok_or("--cert-dir needs a folder (e.g. --cert-dir ~/Downloads)")?;
                        if dir.is_empty() {
                            return Err(
                                "--cert-dir needs a folder, not an empty string".to_string()
                            );
                        }
                        cert_dir = Some(std::path::PathBuf::from(dir));
                    }
                    other => return Err(format!("unknown provision flag {other:?}")),
                }
            }
            let id = id.ok_or(
                "provision needs --id <machine-id> — the roster name this machine signs \
                 under (e.g. targo --unverified ship provision --id m2)",
            )?;
            Ok(Cmd::Provision {
                id,
                check,
                cert_dir,
            })
        }
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
            let mut no_draft_posted = false;
            while let Some(arg) = it.next() {
                if arg == "--release-credentials" {
                    if release_credentials.is_some() {
                        return Err("--release-credentials given twice".to_string());
                    }
                    release_credentials = Some(std::path::PathBuf::from(
                        it.next().ok_or("--release-credentials needs a path")?,
                    ));
                } else if arg == publish::RECOVERY_NO_DRAFT_POSTED_FLAG {
                    if no_draft_posted {
                        return Err("--no-draft-was-posted given twice".to_string());
                    }
                    no_draft_posted = true;
                } else {
                    return Err(format!(
                        "recover takes a version, full claim SHA, --old-publisher-stopped, \
                         and optionally --release-credentials / --no-draft-was-posted \
                         (got {arg:?})"
                    ));
                }
            }
            Ok(Cmd::Recover {
                version,
                owner,
                release_credentials,
                no_draft_posted,
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
            let build = it.next().ok_or(
                "yank needs the bad release's build number: targo --unverified ship yank <build>",
            )?;
            let build: u64 = build
                .parse()
                .map_err(|_| format!("yank: {build:?} is not a build number (u64)"))?;
            // With the paper master armed, the successor cut a yank publishes
            // refuses pre-claim unless it is told which credentials profile signs —
            // so a yank that cannot forward that answer cannot retire a bad build
            // at all. Same flag, same spelling, same meaning as `cut`.
            //
            // And ONLY that one: every other cut flag either contradicts what
            // a yank's successor is (--dry-run/--rehearse publish nothing to
            // prove, --resume belongs to the journal) or is the yank's own
            // decision (--min-build is fixed at bad build + 1), so accepting one
            // could only mean ignoring it.
            let mut opts = verify::YankOptions::default();
            while let Some(flag) = it.next() {
                match flag {
                    "--release-credentials" => {
                        if opts.release_credentials.is_some() {
                            return Err("--release-credentials given twice".to_string());
                        }
                        opts.release_credentials = Some(std::path::PathBuf::from(
                            it.next().ok_or("--release-credentials needs a path")?,
                        ));
                    }
                    RETIRED_STRAND_FLAG => return Err(RETIRED_STRAND_REFUSAL.to_string()),
                    extra => {
                        return Err(format!(
                            "yank takes one build number and optionally --release-credentials \
                             <profile.toml> (got {extra:?})"
                        ));
                    }
                }
            }
            Ok(Cmd::Yank { build, opts })
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

fn parse_cut<'a>(it: &mut impl Iterator<Item = &'a str>) -> std::result::Result<Cmd, String> {
    let mut opts = publish::CutOptions::default();
    let mut abandon: Option<String> = None;
    let mut retire_unmirrored: Option<String> = None;
    while let Some(flag) = it.next() {
        match flag {
            "--retire-unmirrored" => {
                let v = it
                    .next()
                    .ok_or("--retire-unmirrored needs a version (vX.Y.Z)")?;
                retire_unmirrored = Some(normalize_version(v)?);
            }
            "--dry-run" => opts.dry_run = true,
            "--resume" => opts.resume = true,
            "--gate" => opts.gate = true,
            "--arm64-only" => opts.arm64_only = true,
            "--linux-artifacts" => {
                if opts.linux_artifacts.is_some() {
                    return Err("--linux-artifacts given twice".into());
                }
                opts.linux_artifacts = Some(std::path::PathBuf::from(
                    it.next().ok_or("--linux-artifacts needs a directory")?,
                ));
            }
            "--linux-target" => {
                let triple = match it.next().ok_or("--linux-target needs aarch64 or x86_64")? {
                    "aarch64" => "aarch64-unknown-linux-gnu",
                    "x86_64" => "x86_64-unknown-linux-gnu",
                    other => {
                        return Err(format!("unsupported Linux release architecture {other:?}"));
                    }
                };
                if opts.linux_targets.iter().any(|t| t == triple) {
                    return Err("duplicate --linux-target".into());
                }
                opts.linux_targets.push(triple.into());
            }
            // An EMERGENCY ESCAPE, not a setting — publish::paint_smoke_policy
            // owns the refusal on notarized real cuts and the ack it demands.
            publish::NO_PAINT_SMOKE_FLAG => opts.no_paint_smoke = true,
            RETIRED_STRAND_FLAG => return Err(RETIRED_STRAND_REFUSAL.to_string()),
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
    if !opts.linux_targets.is_empty() && opts.linux_artifacts.is_none() {
        return Err("--linux-target requires --linux-artifacts".into());
    }
    // Mode exclusivity: each of abandon/resume is a whole flow of its own —
    // silently ignoring a second flag would do something the operator did not
    // ask for, on the one command where that costs a burned ledger number.
    if abandon.is_some()
        && (opts.dry_run
            || opts.resume
            || opts.gate
            || opts.arm64_only
            || opts.linux_artifacts.is_some()
            || opts.no_paint_smoke
            || opts.min_build.is_some()
            || opts.rehearse.is_some())
    {
        return Err("--abandon combines with no other cut flag".to_string());
    }
    if opts.resume
        && (opts.dry_run
            || opts.gate
            || opts.arm64_only
            || opts.linux_artifacts.is_some()
            || opts.no_paint_smoke
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
    Ok(Cmd::Cut {
        opts,
        abandon,
        retire_unmirrored,
    })
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
        Cmd::LinuxBuild {
            version,
            build,
            commit,
            out,
        } => crate::buildplan::linux::run_worker(&repo_root()?, &out, &version, build, &commit)
            .map_err(Error::new),
        Cmd::Cut {
            abandon: Some(v), ..
        } => verify::run_abandon(&repo_root()?, &v),
        Cmd::Cut {
            retire_unmirrored: Some(v),
            ..
        } => verify::run_retire_unmirrored(&repo_root()?, &v),
        Cmd::Cut { opts, .. } => publish::run_cut(&repo_root()?, &opts),
        Cmd::Provision {
            id,
            check,
            cert_dir,
        } => crate::provision::run_provision(&repo_root()?, &id, check, cert_dir.as_deref()),
        Cmd::Status => verify::run_status(&repo_root()?),
        Cmd::Recover {
            version,
            owner,
            release_credentials,
            no_draft_posted,
        } => {
            // Recovery signs too, so it resolves credentials the SAME one-path way
            // as a fresh cut: the flag when given, else this machine's provisioned
            // identity. Different resolution here would mean a cut and its recovery
            // could sign as different machines.
            let creds = crate::sign::ReleaseCredentials::resolve(
                release_credentials.as_deref(),
                &repo_root()?,
            )?;
            publish::run_recover_lost(
                &repo_root()?,
                &version,
                &owner,
                true,
                no_draft_posted,
                creds.as_ref(),
                release_credentials.as_deref(),
            )
        }
        Cmd::Verify { version } => verify::run_verify(&repo_root()?, version),
        Cmd::Yank { build, opts } => verify::run_yank(&repo_root()?, build, &opts),
    }
}

/// The workspace root, from git — the `targo --unverified ship` alias may be invoked from
/// any subdirectory of the checkout, and every pipeline path is repo-relative.
fn repo_root() -> ledger::Result<std::path::PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| Error::new(format!("failed to run git rev-parse --show-toplevel: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(
            "not inside a git checkout — run `targo --unverified ship` from the aterm workspace"
                .to_string(),
        ));
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(std::path::PathBuf::from(root))
}

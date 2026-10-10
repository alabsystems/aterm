// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified ship release` — THE way to release aterm: one command, from a
//! clean clone of the dev repository on `main` to a live release, unattended, and
//! resumable by running it again.
//!
//! ```text
//! targo --unverified ship release --dry-run --bump --mac-only  # the plan; nothing changes
//! targo --unverified ship release --bump --mac-only            # the next MINOR, from main's tip
//! targo --unverified ship release --commit <rev> --mac-only    # a named commit on origin/main
//! targo --unverified ship release                               # again: resumes where it stopped
//! ```
//!
//! THE STEPS, in order ([`Step`]):
//!
//! ```text
//!  1 checks           everything knowable now, refused NOW with its remedy
//!  2 sync             the publication engine to its origin/main (appended audit rows
//!                     kept), and the vendor/astream submodule
//!  3 commit           the release commit R: `--bump` commits the next MINOR on main's
//!                     tip and pushes it; else main's tip, or `--commit`
//!  4 preflight-early  every export guard (the engine's own dry run, never skipped),
//!                     the fixtures, the version, the engine and the wasm32 engine; the
//!                     machine's provisioning; the handoff fixture guard (once per R)
//!  5 trust            tools/trust-gate-all.sh, the whole tree (the CURRENT lane)
//!  6 lock             this machine's gate lock, held from here to the end
//!  7 build            BUILD ONCE: reserve the release's identity (`ledger::reserve`:
//!                     build n and the release commit C, made from objects, claimed
//!                     nowhere), then `ship build` — both slices at once from the
//!                     snapshot of C into the release cache, the unsigned bundle kept
//!                     by what it was built as (`artifact.rs`)
//!  8 measure          tools/verify.sh --measure <that bundle>: the bytes that ship
//!  9 preflight        the preflight and the provisioning again, with what changed
//!                     while the long steps ran (the cut's disk, the version's tag, the
//!                     cut's read-only pre-claim gates): READY, or nothing is staged
//! 10 stage            pub stage aterm --commit R --repo-path <this clone>
//! 11 publish          pub publish aterm --repo-path <this clone>
//! 12 cut              ship cut: claim the reservation, then sign, notarize and
//!                     package a byte copy of the MEASURED bundle (a claimed cut:
//!                     --resume; the cut never builds)
//! 13 after-cut        the published release's shape, from what GitHub serves, then
//!                     alab.systems brought to it
//! 14 check            the channel's `latest` is vX.Y.0
//! ```
//!
//! Then ONE final line: `RELEASED …`, or `REFUSED at <step>: <why> — <what is public>.
//! <remedy>`. Each step prints its start, its end and its duration, and the plan an ETA
//! from this machine's measured step times.
//!
//! RESUMABLE BY FACTS. Every step asks whether its work is ALREADY DONE for R, never
//! whether it ran: the trust verdict for R from the release's prover, the bundle the
//! journal names COMPLETE in the release cache with its bytes re-proved, a `measured
//! yes` MEASURE receipt filed under its executable's digest, a promotable attestation
//! of R in `publish/.out`, R in the engine's ledger (`mappings.json`), the cut journal
//! (`dist/cut-state.toml`: finished, or claimed and resumed). The release's own journal
//! (`<git common dir>/aterm-release/release.toml`) fixes the version, R, the prover,
//! the Linux declaration, the reserved identity and the bundle built from it, so a bare
//! re-run resumes and never chases main's moving tip, nor rebuilds what it built: the
//! READY candidate is kept, and only `--commit` replaces it, out loud, while it is
//! unpublished.
//!
//! BUILD ONCE. The bytes MEASURE judges are the bytes the cut signs: step 7 reserves
//! the identity they carry (build n, release commit C) without claiming it, builds them
//! once, and step 8 measures them; the cut claims that reservation and signs a byte
//! copy of them, refusing a copy whose executable is not the measured one. A release
//! that refuses before the cut's claim burns no number; a claim the ledger has passed
//! meanwhile refuses with nothing burned, and the next run reserves anew and builds
//! again.
//!
//! A COULD NOT RUN IS A STOP, NOT A RETRY. A step that decided nothing (the trust lane
//! left libraries unmeasured, a MEASURE stage could not run, Apple's notary did not
//! answer) ends the run as a resumable stop: the journal keeps the candidate, and the
//! same command resumes at that step. A FAIL is never re-run, and COULD NOT RUN is never
//! a pass.
//!
//! THE OWNER IS ASKED ONCE, AND NOTHING WAITS FOR HIM THAT DOES NOT NEED HIM. This
//! machine's provisioning ([`crate::provision::audit`]) is audited before the long steps
//! and before every step that needs it. A gap this machine's operator closes (a missing
//! x86 slice, a stale toolchain) refuses before the hours of the long steps. A gap only
//! the owner closes (Apple's developer agreement, a notary credential, the channel
//! token) is raised ONCE as a standing ask on the owner's aterm (`aterm ctl notice ask`:
//! `Message::now(Ask::standing(StandingWhy::OnlyYou(..), Then::step(..)))`, the one
//! action in words), while the trust lane and MEASURE run; the release then waits at
//! the strict preflight — the candidate READY, nothing public — re-auditing every ten
//! minutes, and goes on the moment the gap closes, resolving the ask.
//!
//! ONE RUN PER CLONE: `<git common dir>/aterm-release/running` names the live run (its
//! pid and that pid's start time, so a reused pid is no holder); a second run is
//! refused naming it. `running.child` names the step's child while it runs (in its own
//! session and process group), so a run killed outright leaves no step running unseen:
//! while that child lives, the next run is refused naming it. The run re-executes
//! itself as R's own cutter once the clone is at R (`gates::own_cutter`), so every
//! judgment it makes of R — the receipts, the pre-claim gates, the fixture guard — is
//! R's; `exec` keeps the pid and its start time, so the claim is the same run's. Every
//! relative path flag is anchored once, where the release started
//! ([`ReleaseOptions::anchored`]), for the steps and the re-exec, which run from the
//! clone's root.
//!
//! INTERRUPTED, NEVER ORPHANED. Ctrl-C, a closed terminal or `kill -TERM <pid>` is
//! passed to the running step's process group — every member but a Trust trustd,
//! which is never signalled; the release waits for that step to end, lets the
//! machine's lock go, and ends in ONE line, `INTERRUPTED at <step> — <what is public>.
//! …`, exit 130. A second signal kills the step's group (trustd aside) outright. The
//! waits that run no child — for the machine's lock, for the owner — end within a
//! second of the signal the same way. A signal the release was started ignoring
//! (`nohup`) stays ignored.
//!
//! Exit: 0 released (or a dry run a real run would carry through) · 1 refused (the
//! final line names the step, why and the remedy) · 2 bad usage · 3 a resumable stop
//! (COULD NOT RUN: run it again) · 130 interrupted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::provision::{AskWords, Audited, Finding};

/// How an operator runs a release — the ONE spelling every remedy names.
pub const RELEASE_COMMAND: &str = crate::ledger::RELEASE_COMMAND;

/// The release's directory in the clone's git common directory: its journal, its
/// run claim — shared by every worktree of the clone.
pub const RELEASE_DIR: &str = "aterm-release";

/// The release journal, in [`RELEASE_DIR`].
pub const JOURNAL_FILE: &str = "release.toml";

/// The live run's claim, beside the journal.
pub const RUNNING_FILE: &str = "running";

/// The running step's child (its pid, start time and step), beside the claim: what a
/// run killed outright would leave running.
pub const RUNNING_CHILD_FILE: &str = "running.child";

/// How often an owner gap is audited again while the release waits on it: a poll
/// cadence, never a verdict.
pub const OWNER_POLL_SECS: u64 = 600;

/// The disk the cut's universal build needs (`gates::MIN_FREE_DISK_GIB`).
const CUT_GIB: u64 = crate::gates::MIN_FREE_DISK_GIB;

// ---------------------------------------------------------------------------
// the steps
// ---------------------------------------------------------------------------

/// One step of the release, in plan order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Checks,
    Sync,
    Commit,
    PreflightEarly,
    Trust,
    Lock,
    Build,
    Measure,
    Preflight,
    Stage,
    Publish,
    Cut,
    AfterCut,
    Check,
}

impl Step {
    /// Every step, in the order a release runs them.
    pub const ALL: [Step; 14] = [
        Step::Checks,
        Step::Sync,
        Step::Commit,
        Step::PreflightEarly,
        Step::Trust,
        Step::Lock,
        Step::Build,
        Step::Measure,
        Step::Preflight,
        Step::Stage,
        Step::Publish,
        Step::Cut,
        Step::AfterCut,
        Step::Check,
    ];

    /// The step's name on the transcript, in its log's file name and in `steps.tsv`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Step::Checks => "checks",
            Step::Sync => "sync",
            Step::Commit => "commit",
            Step::PreflightEarly => "preflight-early",
            Step::Trust => "trust",
            Step::Lock => "lock",
            Step::Build => "build",
            Step::Measure => "measure",
            Step::Preflight => "preflight",
            Step::Stage => "stage",
            Step::Publish => "publish",
            Step::Cut => "cut",
            Step::AfterCut => "after-cut",
            Step::Check => "check",
        }
    }

    /// 1-based.
    #[must_use]
    pub fn number(self) -> usize {
        Step::ALL
            .iter()
            .position(|s| *s == self)
            .map_or(0, |i| i + 1)
    }

    /// `n/14 name`, as a refusal names the step.
    #[must_use]
    pub fn at(self) -> String {
        format!("{}/{} {}", self.number(), Step::ALL.len(), self.name())
    }

    /// What a step took before this machine measured it (the 2026-09-29 plan's and
    /// critique's figures, and 0.101's cut log): the two slices' build 7m04s and
    /// 6m33s one after the other, at once about 7.5 min, plus the lipo, dSYM and
    /// Rosetta drive; MEASURE 858 s for 0.99, of which the 307.8 s fat-LTO build it no
    /// longer does; stage 3.5 min, publish 3 min, the cut 19.5-39.7 min over
    /// 0.94-0.98 less the build and paint smoke it no longer runs, after-cut 52 s, and the whole-tree
    /// trust lane 2-4 h.
    fn default_secs(self) -> u64 {
        match self {
            Step::Checks | Step::Sync => 15,
            Step::Commit | Step::PreflightEarly | Step::Preflight | Step::AfterCut => 60,
            Step::Trust => 10_800,
            Step::Build => 600,
            Step::Measure => 550,
            Step::Stage => 210,
            Step::Publish => 180,
            Step::Cut => 900,
            Step::Check => 5,
            Step::Lock => 0,
        }
    }
}

/// Why a step stops the release, and what to do: the `REFUSED` line's parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub why: String,
    pub remedy: String,
    /// Lines printed above the final line (the final line stays one line).
    pub detail: Vec<String>,
    /// Nothing about the code was decided: a resumable stop (exit 3), never a pass.
    pub could_not_run: bool,
}

impl Refusal {
    #[must_use]
    pub fn new(why: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            remedy: remedy.into(),
            detail: Vec::new(),
            could_not_run: false,
        }
    }

    #[must_use]
    pub fn detail(mut self, lines: impl IntoIterator<Item = String>) -> Self {
        self.detail.extend(lines);
        self
    }

    #[must_use]
    pub fn could_not_run(mut self) -> Self {
        self.could_not_run = true;
        self
    }
}

// ---------------------------------------------------------------------------
// options
// ---------------------------------------------------------------------------

/// The Linux declaration a cut requires: `--mac-only`, or `--linux-artifacts DIR` with
/// its targets (triples) and workers. Journaled, so a bare re-run resumes with it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxDecl {
    #[serde(default)]
    pub mac_only: bool,
    #[serde(default)]
    pub artifacts: Option<PathBuf>,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub workers: Vec<String>,
}

impl LinuxDecl {
    /// Whether a declaration was made at all.
    #[must_use]
    pub fn declared(&self) -> bool {
        self.mac_only || self.artifacts.is_some()
    }

    /// The `ship cut` flags that say it.
    #[must_use]
    pub fn cut_args(&self) -> Vec<String> {
        let Some(dir) = &self.artifacts else {
            return vec!["--mac-only".to_string()];
        };
        let mut args = vec!["--linux-artifacts".to_string(), dir.display().to_string()];
        for triple in &self.targets {
            let arch = triple.split('-').next().unwrap_or(triple);
            args.extend(["--linux-target".to_string(), arch.to_string()]);
        }
        for worker in &self.workers {
            args.extend(["--linux-worker".to_string(), worker.clone()]);
        }
        args
    }
}

/// Every `release` flag (cli.rs parses them).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReleaseOptions {
    /// `--bump`: commit the next MINOR on origin/main's tip and release it.
    pub bump: bool,
    /// `--commit <rev>`: release this commit of origin/main.
    pub commit: Option<String>,
    pub linux: LinuxDecl,
    /// `--release-credentials <path>`; default `~/.aterm/release-credentials.toml`.
    pub release_credentials: Option<PathBuf>,
    /// `--engine <path>`; default `$PUBLICATION_ENGINE`, else `~/Developer/publication` (`~/publication` until it moves there).
    pub engine: Option<PathBuf>,
    pub dry_run: bool,
    pub verbose: bool,
    /// `--forget`: drop this clone's release journal (nothing public changes).
    pub forget: bool,
    /// `--abandon vX.Y.Z`: withdraw this clone's claimed, unpublished cut of it.
    pub abandon: Option<String>,
}

/// The path flags whose values [`ReleaseOptions::anchored_args`] rewrites.
const PATH_FLAGS: [&str; 3] = ["--engine", "--release-credentials", "--linux-artifacts"];

/// `path` as the directory `cwd` reads it: an absolute path as given; a relative one
/// joined to `cwd`, and canonical when it exists.
#[must_use]
pub fn anchor(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let joined = cwd.join(path);
    joined.canonicalize().unwrap_or(joined)
}

impl ReleaseOptions {
    /// RELATIVE PATHS ARE ANCHORED ONCE, where the release was started (`cwd`):
    /// `--engine`, `--release-credentials` and `--linux-artifacts`. Every step runs
    /// from the clone's root, and so does the re-exec as R's own cutter, where a
    /// relative path would name another place, or nothing — and the journal keeps the
    /// Linux declaration for a re-run from anywhere.
    #[must_use]
    pub fn anchored(&self, cwd: &Path) -> Self {
        let at = |p: &Option<PathBuf>| p.as_deref().map(|p| anchor(p, cwd));
        let mut opts = self.clone();
        opts.engine = at(&self.engine);
        opts.release_credentials = at(&self.release_credentials);
        opts.linux.artifacts = at(&self.linux.artifacts);
        opts
    }

    /// `args` (a process's own, after the program) with each path flag's value replaced
    /// by these options' — anchored ones, for a re-exec from another directory.
    #[must_use]
    pub fn anchored_args(&self, args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
        let value = |flag: &str| match flag {
            "--engine" => self.engine.as_ref(),
            "--release-credentials" => self.release_credentials.as_ref(),
            _ => self.linux.artifacts.as_ref(),
        };
        let mut out = Vec::with_capacity(args.len());
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let flag = arg
                .to_str()
                .filter(|a| PATH_FLAGS.contains(a))
                .map(str::to_string);
            out.push(arg);
            if let Some(flag) = flag
                && let Some(given) = args.next()
            {
                out.push(value(&flag).map_or(given, |p| p.clone().into_os_string()));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// the journal
// ---------------------------------------------------------------------------

/// THIS CLONE'S RELEASE: what a re-run resumes with.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// The release version, `X.Y.0`.
    pub version: String,
    /// R, full.
    pub commit: String,
    /// The prover's bin dir, physical.
    pub prover: String,
    /// Its `trustc -vV` commit-hash.
    pub trustc: String,
    /// R is the bump this release committed.
    #[serde(default)]
    pub bumped: bool,
    /// Epoch seconds the candidate was chosen.
    #[serde(default)]
    pub started: u64,
    #[serde(default)]
    pub linux: LinuxDecl,
    /// Facts only this journal holds for R: `fixture-guard`, `after-cut`,
    /// `site-deferred`, `check`.
    #[serde(default)]
    pub done: Vec<String>,
    /// The owner asks DELIVERED and not yet resolved, by key — carried over when the
    /// candidate is replaced, withdrawn by `--forget`.
    #[serde(default)]
    pub asks: Vec<String>,
    /// R's reserved identity (`ledger::reserve`, step 7): what the bundle is built
    /// as, and what the cut claims. Kept until the claim lands it; replaced only when
    /// origin's ledger has passed it.
    #[serde(default)]
    pub reserved: Option<Reservation>,
    /// The bundle step 7 built from it, once: what MEASURE judges and the cut signs.
    #[serde(default)]
    pub artifact: Option<Artifact>,
}

/// A reserved release identity, as the journal keeps it ([`crate::ledger::Reserved`]).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    /// `n`.
    pub build: u64,
    /// The release commit C (full): R plus the ledger line and the rolled changelog.
    pub commit: String,
    /// The ledger line C appends.
    pub line: String,
}

impl Reservation {
    #[must_use]
    pub fn ledger(&self) -> crate::ledger::Reserved {
        crate::ledger::Reserved {
            build: self.build,
            commit: self.commit.clone(),
            line: self.line.clone(),
        }
    }
}

/// The bundle built ONCE for a reservation, as the journal keeps it: its store key,
/// where it is, and the digest of the executable MEASURE judges.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// The [`crate::artifact::key`] it is kept under in the release cache.
    pub key: String,
    /// `<cache>/artifacts/<key>/aterm.app`.
    pub app: PathBuf,
    /// SHA-256 of `aterm.app/Contents/MacOS/aterm`.
    pub macho_sha256: String,
    /// The build number it carries.
    pub build: u64,
}

impl Journal {
    /// The journal at `path`; `Ok(None)` without one.
    ///
    /// # Errors
    /// A file that does not parse as a release journal.
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => aterm_toml::from_str(&text)
                .map(Some)
                .map_err(|e| format!("{} is not a release journal: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    /// Written whole: a temporary, flushed, renamed, the directory flushed.
    ///
    /// # Errors
    /// Any write.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text =
            aterm_toml::to_string(self).map_err(|e| format!("serialize the journal: {e}"))?;
        let dir = path
            .parent()
            .ok_or_else(|| format!("{} has no directory", path.display()))?;
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let tmp = dir.join(format!(".{JOURNAL_FILE}.{}.tmp", std::process::id()));
        let write = || -> std::io::Result<()> {
            let mut file = std::fs::File::create(&tmp)?;
            std::io::Write::write_all(&mut file, text.as_bytes())?;
            crate::durable::sync_all(&file)?;
            std::fs::rename(&tmp, path)?;
            crate::durable::sync_dir(dir)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("write {}: {e}", path.display())
        })
    }

    /// The release reached its check: everything is done for R.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.has("check")
    }

    #[must_use]
    pub fn has(&self, fact: &str) -> bool {
        self.done.iter().any(|d| d == fact)
    }

    fn add(&mut self, fact: &str) {
        if !self.has(fact) {
            self.done.push(fact.to_string());
        }
    }
}

/// `<git common dir>/`[`RELEASE_DIR`] of the repository `git` runs in: where its
/// release journal is, whichever worktree asks.
///
/// # Errors
/// git cannot name the common dir.
pub fn release_dir(git: &dyn crate::ledger::GitRunner) -> Result<PathBuf, String> {
    let out = crate::ledger::git_ok(
        git,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map_err(|e| e.to_string())?;
    Ok(PathBuf::from(out.stdout_utf8().trim()).join(RELEASE_DIR))
}

// ---------------------------------------------------------------------------
// one run per clone
// ---------------------------------------------------------------------------

/// The live run's claim: `pid`, `started` (that pid's start time, as `ps -o lstart=`
/// prints it) and `logs`, created exclusively. Removed when the run ends; an `exec`
/// ends nothing, so the claim survives the re-exec at R (same pid, same start time).
#[derive(Debug)]
pub struct RunClaim {
    path: PathBuf,
    /// This process made the claim before an `exec` (the re-exec at R).
    pub adopted: bool,
}

impl Drop for RunClaim {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A live claim of another run: who, since when, and its logs — or, `step` set, the
/// step's child a run killed outright left running (its pid leads its own process
/// group).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub started: String,
    pub logs: String,
    /// `Some(the step)` for a child its run left behind.
    pub step: Option<String>,
}

fn read_holder(path: &Path) -> Option<Holder> {
    let text = std::fs::read_to_string(path).ok()?;
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_string())
    };
    Some(Holder {
        pid: field("pid ")?.parse().ok()?,
        started: field("started ").unwrap_or_default(),
        logs: field("logs ").unwrap_or_default(),
        step: field("step "),
    })
}

fn alive(holder: &Holder, started_of: &mut dyn FnMut(u32) -> Option<String>) -> bool {
    !holder.started.is_empty() && started_of(holder.pid).as_deref() == Some(&holder.started)
}

/// The live holder of `path`, if any: a claim whose pid is alive with the start time
/// it recorded — else a step's child its run left alive ([`RUNNING_CHILD_FILE`] beside
/// it), which holds the clone as surely: a second run would start the same step beside
/// it. `started_of` answers a pid's start time (`None`: no such process).
pub fn live_holder(
    path: &Path,
    started_of: &mut dyn FnMut(u32) -> Option<String>,
) -> Option<Holder> {
    if let Some(holder) = read_holder(path).filter(|h| alive(h, started_of)) {
        return Some(holder);
    }
    let child = read_holder(&path.with_file_name(RUNNING_CHILD_FILE))?;
    alive(&child, started_of).then(|| Holder {
        step: Some(child.step.clone().unwrap_or_else(|| "?".into())),
        ..child
    })
}

impl RunClaim {
    /// Claim `path` for this process (`me`: its pid and start time). A live claim of
    /// ANOTHER process refuses, naming it; this process's own (made before an exec) is
    /// adopted; a stale one (its pid dead, or reused by another process) is renamed
    /// aside first.
    ///
    /// # Errors
    /// The live holder, or the claim cannot be written.
    pub fn take(
        path: &Path,
        me: (u32, &str),
        logs: &Path,
        started_of: &mut dyn FnMut(u32) -> Option<String>,
    ) -> Result<Self, std::result::Result<Holder, String>> {
        if let Some(holder) = live_holder(path, started_of) {
            if holder.step.is_none() && holder.pid == me.0 && holder.started == me.1 {
                return Ok(Self {
                    path: path.to_path_buf(),
                    adopted: true,
                });
            }
            return Err(Ok(holder));
        }
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| Err(format!("create {}: {e}", dir.display())))?;
        // a dead run's child that is dead too holds nothing
        let _ = std::fs::remove_file(dir.join(RUNNING_CHILD_FILE));
        if path.exists() {
            let aside = dir.join(format!("{RUNNING_FILE}.stale"));
            std::fs::rename(path, &aside)
                .map_err(|e| Err(format!("move the stale {} aside: {e}", path.display())))?;
        }
        let body = format!("pid {}\nstarted {}\nlogs {}\n", me.0, me.1, logs.display());
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, body.as_bytes()));
        match created {
            Ok(()) => Ok(Self {
                path: path.to_path_buf(),
                adopted: false,
            }),
            // Another run created it between the look and the create: it holds.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(read_holder(path).ok_or_else(|| format!("{} appeared", path.display())))
            }
            Err(e) => Err(Err(format!("write {}: {e}", path.display()))),
        }
    }
}

// ---------------------------------------------------------------------------
// measured step times
// ---------------------------------------------------------------------------

/// `<state>/steps.tsv`: `epoch\tversion\tstep\tsecs`, a line per step that RAN.
/// The estimate of a step is the median of this machine's last five runs of it.
#[must_use]
pub fn estimates(tsv: &str) -> BTreeMap<String, u64> {
    let mut runs: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for line in tsv.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if let [_, _, step, secs] = cols.as_slice()
            && let Ok(secs) = secs.trim().parse::<u64>()
        {
            runs.entry((*step).to_string()).or_default().push(secs);
        }
    }
    runs.into_iter()
        .map(|(step, all)| {
            let mut last: Vec<u64> = all[all.len().saturating_sub(5)..].to_vec();
            last.sort_unstable();
            let median = last[last.len() / 2];
            (step, median)
        })
        .collect()
}

/// `42s` · `3m05s` · `2h38m`.
#[must_use]
pub fn fmt_dur(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, secs % 3600 / 60)
    }
}

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

// ---------------------------------------------------------------------------
// what the backend answers
// ---------------------------------------------------------------------------

/// The release's ONE prover: the physical store build every child is handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prover {
    /// Its bin dir (holds `trustc` and `targo`), physical.
    pub bin: PathBuf,
    /// `trustc -vV`'s commit-hash.
    pub commit: String,
    /// Where it came from, for the plan line.
    pub source: String,
}

/// The cut journal (`dist/cut-state.toml`) as the release reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutState {
    pub version: String,
    pub build: u64,
    /// `unlock` journaled.
    pub finished: bool,
}

/// What the after-cut step did (step 13): the published release's shape checked as
/// GitHub serves it, and alab.systems brought to it — or not from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterCut {
    /// The step's line.
    pub note: String,
    /// This machine has no usable site checkout (`publish/post-promote` exited 3 —
    /// none found, or one it cannot build or deploy from — having touched nothing):
    /// the site is a hand step on a machine that has one
    /// ([`SITE_BY_HAND`]), recorded and said, and the release goes on to its check.
    pub site_deferred: bool,
}

/// The hand step a deferred site leaves ([`AfterCut::site_deferred`]), as the step,
/// the final line and every later run of the release say it. No ` — ` in it: the final
/// line's parts are separated by that.
pub const SITE_BY_HAND: &str = "alab.systems was NOT synced: this machine has no usable \
     site checkout (publish/post-promote exited 3, touching nothing), so run \
     `publish/post-promote --latest` on a machine with one";

/// The journal's fact for a deferred site.
const SITE_DEFERRED: &str = "site-deferred";

/// The engine's ledger (`mappings.json`) as the release reads it, for one commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineRows {
    /// The newest aterm row's commit.
    pub newest: Option<String>,
    /// The commit asked about has a row.
    pub has: bool,
    /// Its row's release revision on the public channel.
    pub rev: Option<String>,
}

/// A judge's answer: yes or no, in its own words — or no answer at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub yes: bool,
    pub words: String,
    /// The judge could not answer (its store unreadable, its prover unnamable): nothing
    /// is known — never a "no" the long steps would answer, a resumable stop at once.
    pub unanswered: bool,
}

impl Answer {
    #[must_use]
    pub fn no(words: impl Into<String>) -> Self {
        Self {
            yes: false,
            words: words.into(),
            unanswered: false,
        }
    }

    #[must_use]
    pub fn yes(words: impl Into<String>) -> Self {
        Self {
            yes: true,
            words: words.into(),
            unanswered: false,
        }
    }

    #[must_use]
    pub fn unanswered(words: impl Into<String>) -> Self {
        Self {
            yes: false,
            words: words.into(),
            unanswered: true,
        }
    }
}

/// THE FACTS every skip rule reads — read once after the commit step and again after
/// each step that changes one, never re-derived per question.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub cut: Option<CutState>,
    pub engine: EngineRows,
    /// A promotable attestation of R in `publish/.out`.
    pub attested: bool,
    /// The trust verdict for R, from the release's prover.
    pub trusted: Answer,
    /// The journal's bundle: COMPLETE in the release cache, its bytes re-proved,
    /// built by the release's prover.
    pub built: Answer,
    /// The MEASURE verdict for that bundle's executable.
    pub measured: Answer,
}

/// The release commit and its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub commit: String,
    pub version: String,
    /// How R was chosen, for the plan line.
    pub why: String,
    /// `--bump` from this version, before the bump is made.
    pub bump_from: Option<String>,
}

/// What a step's child said when it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub code: i32,
    /// The lines that say why (a log tail, the gate's summary lines).
    pub detail: Vec<String>,
}

/// Everything the release asks of the world — git, the engine, the receipts, the
/// children, the clock, the owner's aterm. [`crate::release_steps`] is the real one;
/// the tests drive the same machine with a fake.
pub trait Backend {
    // the world
    fn now(&mut self) -> u64;
    /// `HH:MM:SS` of an epoch, local time.
    fn hms(&mut self, epoch: u64) -> String;
    fn sleep(&mut self, secs: u64);
    /// One line to the terminal.
    fn say(&mut self, line: &str);
    /// The signal that interrupted this run (INT, TERM, HUP), if one did: the running
    /// step's child was handed it.
    fn interrupted(&mut self) -> Option<i32>;
    /// The run is at `step` now, before any of its work: what a child started from
    /// here on is named by while it runs.
    fn entered(&mut self, _step: Step) {}
    /// The log a step's children write to.
    fn log_path(&mut self, step: Step) -> PathBuf;
    /// This process: pid and start time.
    fn me(&mut self) -> (u32, String);
    /// A pid's start time, `None` when no such process lives.
    fn started_of(&mut self, pid: u32) -> Option<String>;
    /// `<git common dir>/aterm-release`.
    fn release_dir(&mut self) -> PathBuf;
    /// The measured step times (`steps.tsv`), and recording one.
    fn times(&mut self) -> String;
    fn record_time(&mut self, version: &str, step: Step, secs: u64);

    // step 1
    /// The prover: TRUST_STAGE2_BIN, else the journal's (when it is still there),
    /// else the store's current build.
    fn prover(&mut self, journaled: Option<&str>) -> Result<Prover, String>;
    /// Hold the prover for the release's life (atpkg's lease).
    fn lease(&mut self, prover: &Prover);
    /// The build-steering variables the gate records, cleared for every child.
    fn cleared_env(&mut self) -> Vec<String>;
    /// The machine's step-1 refusals: an explicit credentials path that is no file, the
    /// cut's disk, the engine checkout, this clone (clean by
    /// `aterm_verify::identity::dirty_against`, on main). The roster id is the answer
    /// (empty when none is named: the audit's owner gap). gh, the credentials profile and
    /// the roster are the provisioning audit's alone ([`Backend::audit`]).
    fn machine(&mut self, creds: &Path, explicit: bool) -> (String, Vec<Refusal>);
    /// `git fetch origin main` and the tags on origin; the tip of origin/main.
    fn fetch(&mut self) -> Result<String, Refusal>;
    fn head(&mut self) -> String;
    fn branch_is_main(&mut self) -> bool;
    fn rev(&mut self, rev: &str) -> Option<String>;
    fn version_at(&mut self, rev: &str) -> Option<String>;
    fn is_ancestor(&mut self, a: &str, b: &str) -> bool;
    /// The commit a tag peels to on origin, from step 1's listing.
    fn origin_tag(&mut self, tag: &str) -> Option<String>;
    /// The same, asked of origin NOW (`git ls-remote --tags origin refs/tags/<tag>`):
    /// a rostered peer may have cut the version while the long steps ran.
    fn origin_tag_now(&mut self, tag: &str) -> Result<Option<String>, String>;
    /// The tag exists in this clone.
    fn local_tag(&mut self, tag: &str) -> bool;
    /// `RELEASES.ledger`'s versions at `rev`, oldest first.
    fn ledger_versions(&mut self, rev: &str) -> Vec<String>;
    fn engine_rows(&mut self, commit: Option<&str>) -> EngineRows;
    fn cut_journal(&mut self) -> Option<CutState>;
    /// The Linux handoff directory exists.
    fn is_dir(&mut self, path: &Path) -> bool;

    // steps
    fn sync(&mut self) -> Result<String, Refusal>;
    /// The bump: the engine's `pub-bump-edit`, the lockfile, one commit, one push.
    /// The bump commit.
    fn bump(&mut self, from: &str, to: &str, prover: &Prover) -> Result<String, Refusal>;
    /// This clone's main to R, its submodules at their gitlinks, and clean there.
    fn move_to(&mut self, commit: &str) -> Result<(), Refusal>;
    /// Whether this binary is R's own cutter (`gates::own_cutter`).
    fn own_cutter(&mut self, commit: &str) -> Result<String, String>;
    /// Exec R's own cutter with this process's arguments. Returns only on failure.
    fn reexec(&mut self, prover: &Prover) -> Refusal;
    /// The facts for R; `artifact` is the journal's bundle, when it names one.
    fn facts(&mut self, c: &Candidate, prover: &Prover, artifact: Option<&Artifact>) -> Facts;
    /// Free GiB here, and whether the MEASURE run's lanes are warm.
    fn disk(&mut self) -> (Option<u64>, bool);
    /// The preflight: every export guard (the engine's dry run), the fixtures and
    /// changelog (R's own pre-claim gates), the tag, the engine and the wasm32
    /// engine. The blocks; a `receipt` block is one steps 5-8 answer. `dry`: only
    /// what reads — the engine's dry run (it appends an audit row) and the wasm32
    /// compile are a real run's. `again` (step 9): R's gates and the engine's
    /// readiness, not the engine's dry run and the wasm32 compile — step 4 judged
    /// both for this R, and `pub stage`, the step after, runs both as its own gates.
    fn preflight(&mut self, c: &Candidate, dry: bool, again: bool) -> Vec<(bool, Refusal)>;
    /// The cut's own pre-claim gates that read without building and need nothing the
    /// publish makes (`publish::pre_publish_gates`): the locked metadata, no process run
    /// out of a staged bundle, the release lease, the channel's write permission, the
    /// signature policy, the roster ratchet and the channel floor. Asked at step 8, so a
    /// refusal there leaves nothing public.
    fn cut_gates(&mut self, c: &Candidate, creds: &Path) -> Vec<Refusal>;
    fn fixture_guard(&mut self, prover: &Prover) -> Result<String, Refusal>;
    fn audit(&mut self, machine_id: &str) -> Vec<Finding>;
    /// Raise the owner ask: `Ok(its reply)` only when the owner's aterm took it.
    fn ask(&mut self, words: &AskWords) -> Result<String, String>;
    /// Resolve it (`notice done <key> ok`).
    fn resolve(&mut self, key: &str) -> Result<String, String>;
    /// Withdraw it (`notice done <key> withdraw`): its release is forgotten.
    fn withdraw(&mut self, key: &str) -> Result<String, String>;
    fn trust_pending(&mut self, c: &Candidate, prover: &Prover) -> Vec<String>;
    fn trust_lane(&mut self, prover: &Prover, remeasure: bool) -> Ran;
    fn lock(&mut self, caller: &str) -> Result<String, Refusal>;
    fn unlock(&mut self);
    /// Origin's ledger tail (the newest claimed build), from step 1's fetch.
    fn origin_tail(&mut self) -> Option<u64>;
    /// Reserve R's identity (`ledger::reserve`): build n and the release commit C,
    /// made from objects, claimed nowhere.
    fn reserve(&mut self, c: &Candidate) -> Result<Reservation, Refusal>;
    /// Build the bundle of `reserved` ONCE (`ship build`, `artifact::build`): both
    /// slices at once into the release cache, or the COMPLETE entry already there.
    fn build(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> Result<Artifact, Refusal>;
    /// `tools/verify.sh --measure <app> …`; `true` when a MEASURE stage itself could
    /// not run.
    fn measure(&mut self, prover: &Prover, app: &Path) -> (Ran, bool);
    /// `pub stage`, the engine synced before and after it; `Err` when either sync failed
    /// (the audit rows it kept are named).
    fn stage(&mut self, c: &Candidate, prover: &Prover) -> Result<Ran, Refusal>;
    /// `pub publish`, the engine synced before it; `Err` when that sync failed.
    fn publish(&mut self, prover: &Prover) -> Result<Ran, Refusal>;
    fn cut(&mut self, args: &[String], prover: &Prover) -> Ran;
    /// The published release's shape, then the site; `Err` with the step's why.
    fn after_cut(&mut self, version: &str) -> Result<AfterCut, Refusal>;
    /// The channel's `latest` tag.
    fn latest(&mut self) -> Result<String, String>;
}

// ---------------------------------------------------------------------------
// the run
// ---------------------------------------------------------------------------

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    /// The final line.
    pub last: String,
}

/// Why the machine stopped before its end.
enum Halt {
    /// The final line is said; the exit code.
    Said(i32),
}

type Flow = Result<(), Halt>;

/// A reservation origin's ledger has reached ([`Run::spent`]).
struct Spent {
    reservation: Reservation,
    /// Origin's ledger tail.
    tail: u64,
    /// Origin/main holds the reservation's release commit: this clone's claim of it
    /// LANDED, and no cut journal holds it.
    claimed_here: bool,
}

impl Spent {
    /// What happened to the number, in the operator's words.
    fn words(&self) -> String {
        let r = &self.reservation;
        if self.claimed_here {
            format!(
                "its reserved build {} was CLAIMED from this clone — origin/main holds its \
                 release commit {} — but no cut journal holds the claim (the cut stopped \
                 between its push and its journal), so nothing was signed or published under \
                 it: build {} is spent, a gap in the ledger",
                r.build,
                short(&r.commit),
                r.build
            )
        } else {
            format!(
                "its reserved build {} is at or below origin's ledger tail {} — another \
                 release claimed past it (nothing was claimed here, nothing burned)",
                r.build, self.tail
            )
        }
    }
}

struct Run<'b> {
    b: &'b mut dyn Backend,
    opts: ReleaseOptions,
    dry: bool,
    journal_path: PathBuf,
    journal: Option<Journal>,
    claim: Option<RunClaim>,
    cur: Step,
    cur_t0: u64,
    t_start: u64,
    /// A dry run's refusals, `n/14 step: why`.
    would: Vec<String>,
    /// A dry run's owner waits.
    waits: Vec<String>,
    public: String,
    prover: Option<Prover>,
    machine_id: String,
    creds: PathBuf,
    cand: Option<Candidate>,
    tip: String,
    facts: Facts,
    linux: LinuxDecl,
    planned: Vec<Step>,
    est: BTreeMap<String, u64>,
    plan_secs: u64,
    locked: bool,
    /// Seconds the current step waited for the owner (not its own time).
    waited: u64,
    /// A dry run audits once.
    dry_audit: Option<Vec<Finding>>,
    last: String,
}

/// Run a release with `b`.
pub fn run_with(b: &mut dyn Backend, opts: &ReleaseOptions, home: &Path) -> Outcome {
    let release_dir = b.release_dir();
    let creds = opts
        .release_credentials
        .clone()
        .unwrap_or_else(|| home.join(".aterm/release-credentials.toml"));
    let mut run = Run {
        b,
        opts: opts.clone(),
        dry: opts.dry_run,
        journal_path: release_dir.join(JOURNAL_FILE),
        journal: None,
        claim: None,
        cur: Step::Checks,
        cur_t0: 0,
        t_start: 0,
        would: Vec::new(),
        waits: Vec::new(),
        public: "nothing public".to_string(),
        prover: None,
        machine_id: String::new(),
        creds,
        cand: None,
        tip: String::new(),
        facts: Facts::default(),
        linux: LinuxDecl::default(),
        planned: Vec::new(),
        est: BTreeMap::new(),
        plan_secs: 0,
        locked: false,
        waited: 0,
        dry_audit: None,
        last: String::new(),
    };
    // A run that stops unexpectedly still ends in one line: the journal is durable,
    // and the same command resumes.
    let went = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run.go()));
    let code = match went {
        Ok(Ok(())) => 0,
        Ok(Err(Halt::Said(code))) => code,
        Err(_) => {
            let step = run.cur;
            let log = run.b.log_path(step);
            run.last = format!(
                "REFUSED at {}: {RELEASE_COMMAND} stopped unexpectedly — {}. Read {}, then \
                 run it again to resume",
                step.at(),
                run.public,
                log.display()
            );
            let last = run.last.clone();
            run.b.say(&last);
            1
        }
    };
    if run.locked {
        run.b.unlock();
    }
    Outcome {
        code,
        last: run.last,
    }
}

impl Run<'_> {
    fn go(&mut self) -> Flow {
        if self.opts.forget {
            return self.forget();
        }
        self.journal = match Journal::load(&self.journal_path) {
            Ok(j) => j,
            Err(why) => {
                return self.final_refusal(Refusal::new(
                    why,
                    format!("move it aside, or drop it: {RELEASE_COMMAND} --forget"),
                ));
            }
        };
        self.t_start = self.b.now();
        if self.dry {
            self.say(
                "release: DRY RUN — nothing is changed; each step says what a real run would do",
            );
        }

        self.begin(Step::Checks)?;
        let note = self.checks()?;
        self.finish(&note);

        self.begin(Step::Sync)?;
        if self.dry {
            self.finish(
                "would fast-forward the engine to its origin/main (appended audit rows kept) \
                 and run `git submodule update --init vendor/astream`",
            );
        } else {
            match self.b.sync() {
                Ok(note) => self.finish(&note),
                Err(r) => return self.refuse(r),
            }
        }

        self.begin(Step::Commit)?;
        let note = self.commit()?;
        self.finish(&note);
        self.own_cutter()?;

        self.refresh();
        self.plan_print();

        self.begin(Step::PreflightEarly)?;
        if let Some(why) = self.claimed_words() {
            self.skip(&why);
        } else {
            self.disk_still_needed()?;
            self.preflight(false)?;
        }

        self.begin(Step::Trust)?;
        if let Some(why) = self.skip_trust() {
            self.skip(&why);
        } else {
            self.trust()?;
        }

        self.begin(Step::Lock)?;
        if !self.lock_needed() {
            self.skip(
                "nothing heavy or judged on the clock remains (built, measured, and the cut \
                 finished)",
            );
        } else if self.dry {
            self.finish("would take this machine's gate lock in its line and hold it to the end");
        } else {
            self.take_lock()?;
        }

        self.begin(Step::Build)?;
        if let Some(why) = self.skip_build() {
            self.skip(&why);
        } else {
            self.build()?;
        }

        self.begin(Step::Measure)?;
        if let Some(why) = self.skip_measure() {
            self.skip(&why);
        } else {
            self.measure()?;
        }

        self.begin(Step::Preflight)?;
        if let Some(why) = self.claimed_words() {
            self.skip(&why);
        } else {
            self.preflight(true)?;
        }

        self.begin(Step::Stage)?;
        if let Some(why) = self.skip_stage() {
            self.skip(&why);
        } else {
            self.stage()?;
        }

        self.begin(Step::Publish)?;
        if let Some(why) = self.skip_publish() {
            self.skip(&why);
        } else {
            self.publish()?;
        }

        self.begin(Step::Cut)?;
        match self.cut_mode() {
            CutMode::Done(why) => self.skip(&why),
            mode => self.cut(&mode)?,
        }

        self.begin(Step::AfterCut)?;
        if self.journal_has("after-cut") {
            let v = self.version();
            let site = self.site_words();
            self.skip(&format!("done for v{v} (this release's journal){site}"));
        } else if self.dry {
            let v = self.version();
            self.finish(&format!(
                "would check v{v}'s shape as GitHub serves it, then bring alab.systems to it \
                 (publish/post-promote --latest)"
            ));
        } else {
            self.after_cut()?;
        }

        self.begin(Step::Check)?;
        self.check()
    }

    // -- output ------------------------------------------------------------

    fn say(&mut self, line: &str) {
        self.b.say(line);
    }

    fn version(&self) -> String {
        self.cand
            .as_ref()
            .map_or_else(|| "?".to_string(), |c| c.version.clone())
    }

    fn commit_r(&self) -> String {
        self.cand
            .as_ref()
            .map_or_else(|| "?".to_string(), |c| c.commit.clone())
    }

    fn begin(&mut self, step: Step) -> Flow {
        self.cur = step;
        self.b.entered(step);
        self.waited = 0;
        self.cur_t0 = self.b.now();
        self.halt_if_interrupted()?;
        let at = self.b.hms(self.cur_t0);
        self.say(&format!(
            "[{:>2}/{}] {:<15} start {at}",
            step.number(),
            Step::ALL.len(),
            step.name()
        ));
        Ok(())
    }

    /// A signal arrived (the step's child was handed it and has ended): the machine's
    /// lock goes, and the run ends in its ONE line, exit 130. The journal is durable,
    /// so the same command resumes.
    fn halt_if_interrupted(&mut self) -> Flow {
        let Some(sig) = self.b.interrupted() else {
            return Ok(());
        };
        if self.locked {
            self.b.unlock();
            self.locked = false;
        }
        let step = self.cur;
        let cut = if step == Step::Cut {
            " The cut stopped with this run; running the command again resumes it."
        } else {
            ""
        };
        self.last = format!(
            "INTERRUPTED at {} (by {}) — {}.{cut} Run {RELEASE_COMMAND} again to resume",
            step.at(),
            signal_name(sig),
            self.public
        );
        let last = self.last.clone();
        self.say(&last);
        Err(Halt::Said(130))
    }

    fn finish(&mut self, note: &str) {
        let now = self.b.now();
        let secs = now.saturating_sub(self.cur_t0);
        let step = self.cur;
        if self.dry {
            if step == Step::Checks {
                let at = self.b.hms(now);
                self.say(&format!(
                    "[{:>2}/{}] {:<15} ok    {at}  {}  {note}",
                    step.number(),
                    Step::ALL.len(),
                    step.name(),
                    fmt_dur(secs)
                ));
            } else {
                self.say(&format!(
                    "[{:>2}/{}] {:<15} plan  {note}",
                    step.number(),
                    Step::ALL.len(),
                    step.name()
                ));
            }
            return;
        }
        let at = self.b.hms(now);
        let eta = self.eta_words();
        self.say(&format!(
            "[{:>2}/{}] {:<15} ok    {at}  {}  {note}{eta}",
            step.number(),
            Step::ALL.len(),
            step.name(),
            fmt_dur(secs)
        ));
        let v = self.version();
        // the step's own time, not its waits for the owner
        self.b
            .record_time(&v, step, secs.saturating_sub(self.waited));
    }

    fn skip(&mut self, why: &str) {
        let step = self.cur;
        self.say(&format!(
            "[{:>2}/{}] {:<15} skip  {why}",
            step.number(),
            Step::ALL.len(),
            step.name()
        ));
    }

    /// REFUSE: a real run says it in the final line and stops; a dry run notes what a
    /// real run would refuse and goes on planning.
    fn refuse(&mut self, r: Refusal) -> Flow {
        let step = self.cur;
        if self.dry {
            self.would.push(format!("{}: {}", step.at(), r.why));
            self.say(&format!(
                "[{:>2}/{}] {:<15} WOULD REFUSE  {}",
                step.number(),
                Step::ALL.len(),
                step.name(),
                r.why
            ));
            for line in &r.detail {
                self.b.say(&format!("        | {line}"));
            }
            self.say(&format!("        fix: {}", r.remedy));
            return Ok(());
        }
        self.final_refusal(r)
    }

    /// A refusal no dry run can plan past (there is no release commit to plan for).
    fn refuse_hard(&mut self, r: Refusal) -> Flow {
        self.refuse(r)?;
        self.dry_final()
    }

    fn final_refusal(&mut self, r: Refusal) -> Flow {
        // a step that failed because it was interrupted ends as interrupted
        self.halt_if_interrupted()?;
        let step = self.cur;
        let now = self.b.now();
        let at = self.b.hms(now);
        let log = self.b.log_path(step);
        let word = if r.could_not_run {
            "COULD NOT RUN"
        } else {
            "REFUSED"
        };
        self.say(&format!(
            "[{:>2}/{}] {:<15} {word}  {at}  {}  (log: {})",
            step.number(),
            Step::ALL.len(),
            step.name(),
            fmt_dur(now.saturating_sub(self.cur_t0)),
            log.display()
        ));
        for line in &r.detail {
            self.b.say(&format!("        | {line}"));
        }
        if self.locked {
            self.b.unlock();
            self.locked = false;
        }
        let why = if r.could_not_run {
            format!("COULD NOT RUN — {}", r.why)
        } else {
            r.why.clone()
        };
        self.last = format!(
            "REFUSED at {}: {why} — {}. {}",
            step.at(),
            self.public,
            r.remedy
        );
        let last = self.last.clone();
        self.say(&last);
        Err(Halt::Said(if r.could_not_run { 3 } else { 1 }))
    }

    fn dry_final(&mut self) -> Flow {
        let v = self.version();
        let r = self.commit_r();
        let waits = if self.waits.is_empty() {
            String::new()
        } else {
            format!(
                ", and wait for the owner at {} ({})",
                Step::Preflight.at(),
                self.waits.join("; ")
            )
        };
        if self.would.is_empty() {
            self.last = format!(
                "DRY RUN aterm {v} from {}: a real run would go ahead, about {} of work{waits} \
                 — nothing was changed",
                short(&r),
                fmt_dur(self.plan_secs)
            );
            let last = self.last.clone();
            self.say(&last);
            return Err(Halt::Said(0));
        }
        self.last = format!(
            "DRY RUN aterm {v} from {}: a real run would refuse at {} ({} refusal(s) above) \
             — nothing was changed",
            short(&r),
            self.would[0],
            self.would.len()
        );
        let last = self.last.clone();
        self.say(&last);
        Err(Halt::Said(1))
    }

    fn eta_words(&mut self) -> String {
        let total: u64 = self
            .planned
            .iter()
            .filter(|s| **s > self.cur)
            .map(|s| self.est_secs(*s))
            .sum();
        if total == 0 {
            return String::new();
        }
        let now = self.b.now();
        let at = self.b.hms(now + total);
        format!(
            "  · ETA {} ({} to go)",
            &at[..at.len().min(5)],
            fmt_dur(total)
        )
    }

    fn est_secs(&self, step: Step) -> u64 {
        self.est
            .get(step.name())
            .copied()
            .unwrap_or_else(|| step.default_secs())
    }

    // -- the journal ----------------------------------------------------------

    fn journal_has(&self, fact: &str) -> bool {
        let r = self.commit_r();
        self.journal
            .as_ref()
            .is_some_and(|j| j.commit == r && j.has(fact))
    }

    fn journal_add(&mut self, fact: &str) -> Flow {
        self.journal_add_all(&[fact])
    }

    /// Facts that are one fact, in one save: a stop can never keep one without the
    /// rest.
    fn journal_add_all(&mut self, facts: &[&str]) -> Flow {
        if self.dry {
            return Ok(());
        }
        let Some(j) = self.journal.as_mut() else {
            return Ok(());
        };
        for fact in facts {
            j.add(fact);
        }
        let saved = j.save(&self.journal_path);
        match saved {
            Ok(()) => Ok(()),
            Err(why) => self.refuse(Refusal::new(
                why,
                "check the permissions of the clone's .git/aterm-release, then re-run",
            )),
        }
    }

    fn forget(&mut self) -> Flow {
        let running = self.b.release_dir().join(RUNNING_FILE);
        let mut started_of = |pid| self.b.started_of(pid);
        if let Some(h) = live_holder(&running, &mut started_of) {
            let r = running_refusal(&h);
            self.last = format!(
                "release: {}; its journal is left alone — {}, then --forget",
                r.why, r.remedy
            );
            let last = self.last.clone();
            self.say(&last);
            return Err(Halt::Said(1));
        }
        // the owner's open asks go with the release they were raised for
        let asks = Journal::load(&self.journal_path)
            .ok()
            .flatten()
            .map(|j| j.asks)
            .unwrap_or_default();
        for key in asks {
            let how = match self.b.withdraw(&key) {
                Ok(reply) => format!("withdrawn: {reply}"),
                Err(why) => format!("not withdrawn: {why}"),
            };
            self.say(&format!("release: the ask {key} — {how}"));
        }
        self.last = match std::fs::remove_file(&self.journal_path) {
            Ok(()) => format!(
                "release: forgot this clone's release journal ({}); nothing public changed",
                self.journal_path.display()
            ),
            Err(_) => "release: no release journal in this clone".to_string(),
        };
        let last = self.last.clone();
        self.say(&last);
        Err(Halt::Said(0))
    }

    // -- step 1 -----------------------------------------------------------

    fn checks(&mut self) -> Result<String, Halt> {
        let mut notes: Vec<String> = Vec::new();

        // this clone's release is not running already
        let release_dir = self.b.release_dir();
        let me = self.b.me();
        let logs = self.b.log_path(Step::Checks);
        let logs = logs.parent().map(Path::to_path_buf).unwrap_or_default();
        if self.dry {
            let mut started_of = |pid| self.b.started_of(pid);
            if let Some(h) = live_holder(&release_dir.join(RUNNING_FILE), &mut started_of)
                && h.pid != me.0
            {
                self.refuse_hard(running_refusal(&h))?;
            }
        } else {
            let mut started_of = |pid| self.b.started_of(pid);
            match RunClaim::take(
                &release_dir.join(RUNNING_FILE),
                (me.0, &me.1),
                &logs,
                &mut started_of,
            ) {
                Ok(claim) => self.claim = Some(claim),
                Err(Ok(h)) => self.refuse_hard(running_refusal(&h))?,
                Err(Err(why)) => self.refuse_hard(Refusal::new(
                    why,
                    "check the permissions of the clone's .git/aterm-release",
                ))?,
            }
        }

        // the prover
        let journaled = self
            .journal
            .as_ref()
            .filter(|j| !j.finished())
            .map(|j| j.prover.clone());
        match self.b.prover(journaled.as_deref()) {
            Ok(p) => {
                if !self.dry {
                    self.b.lease(&p);
                }
                self.prover = Some(p);
            }
            Err(why) => self.refuse(Refusal::new(
                format!("no Trust prover: {why}"),
                "aterm pkg install trust (then aterm pkg doctor), or export \
                 TRUST_STAGE2_BIN=<a stage2 bin dir holding trustc and targo>",
            ))?,
        }

        // one build environment
        let cleared = self.b.cleared_env();
        if !cleared.is_empty() {
            notes.push(format!("cleared for every step: {}", cleared.join(" ")));
        }

        // the machine: an explicit credentials path, the disk, the engine, this clone (gh,
        // the profile and the roster are the provisioning audit's, step 4)
        let creds = self.creds.clone();
        let explicit = self.opts.release_credentials.is_some();
        let (machine_id, refusals) = self.b.machine(&creds, explicit);
        self.machine_id = machine_id;
        for r in refusals {
            self.refuse(r)?;
        }
        if !self.b.branch_is_main() {
            self.refuse(Refusal::new(
                "this clone is not on main — the preflight and the stage release main's history",
                "release from a clean clone on main (git switch main)",
            ))?;
        }
        match self.b.fetch() {
            Ok(tip) => self.tip = tip,
            Err(r) => self.refuse_hard(r)?,
        }
        let head = self.b.head();
        if self.b.branch_is_main() && !self.b.is_ancestor(&head, &self.tip.clone()) {
            self.refuse(Refusal::new(
                "this clone's main has commits origin/main does not — a release is staged from \
                 origin's history",
                "push them (git push origin main), or release from a fresh clone",
            ))?;
        }

        self.choose_candidate()?;
        if self.cand.as_ref().is_some_and(|c| c.bump_from.is_none()) {
            self.public_state();
        }

        // the Linux declaration the cut requires: given now, else the journal's
        self.linux = self.opts.linux.clone();
        let r = self.commit_r();
        if !self.linux.declared()
            && self.linux.targets.is_empty()
            && self.linux.workers.is_empty()
            && let Some(j) = self.journal.as_ref().filter(|j| j.commit == r)
        {
            self.linux = j.linux.clone();
        }
        if self.linux.mac_only && self.linux.artifacts.is_some() {
            self.refuse(Refusal::new(
                "--mac-only and --linux-artifacts both given",
                "pass one: --mac-only ships no Linux, --linux-artifacts <dir> ships it",
            ))?;
        } else if self.linux.artifacts.is_none()
            && (!self.linux.targets.is_empty() || !self.linux.workers.is_empty())
        {
            self.refuse(Refusal::new(
                "--linux-target/--linux-worker without --linux-artifacts",
                "add --linux-artifacts <dir>, or drop them for --mac-only",
            ))?;
        } else if let Some(dir) = self.linux.artifacts.clone()
            && !self.b.is_dir(&dir)
        {
            self.refuse(Refusal::new(
                format!("--linux-artifacts {} is not a directory", dir.display()),
                "create the handoff directory (docs/RELEASING.md § Native Linux release \
                 handoff), or pass --mac-only",
            ))?;
        } else if !self.linux.declared() && !self.claimed() {
            self.refuse(Refusal::new(
                "the cut must declare Linux, and nothing here does",
                "pass --mac-only (no Linux build, on purpose), or --linux-artifacts <dir> with \
                 a --linux-worker per native host",
            ))?;
        }

        // an unfinished cut of ANOTHER version holds this clone's cut journal
        let v = self.version();
        if let Some(cut) = self.b.cut_journal()
            && !cut.finished
            && cut.version != v
        {
            self.refuse(Refusal::new(
                format!(
                    "this clone holds an unfinished cut of v{} (build {}, dist/cut-state.toml)",
                    cut.version, cut.build
                ),
                format!(
                    "finish it: {RELEASE_COMMAND} --commit <its published commit>; or withdraw \
                     it: {RELEASE_COMMAND} --abandon v{}",
                    cut.version
                ),
            ))?;
        }

        let mut note = notes.join("; ");
        if !self.would.is_empty() {
            note = format!(
                "{} refusal(s) above{}",
                self.would.len(),
                if note.is_empty() {
                    String::new()
                } else {
                    format!("; {note}")
                }
            );
        }
        if note.is_empty() {
            note = "nothing to refuse".to_string();
        }
        Ok(note)
    }

    fn claimed(&mut self) -> bool {
        let v = self.version();
        self.b.cut_journal().is_some_and(|c| c.version == v)
    }

    /// What the final line says is public, from the cut journal and the engine's
    /// ledger, for V.
    fn public_state(&mut self) {
        let v = self.version();
        let r = self.commit_r();
        let cut = self.b.cut_journal().filter(|c| c.version == v);
        let rows = self.b.engine_rows(Some(&r));
        self.public = match cut {
            Some(c) if c.finished => format!("v{v} is cut (build {})", c.build),
            Some(c) => format!("build {} of v{v} is claimed", c.build),
            None if rows.has => format!(
                "the source of v{v} is public (alabsystems {}), no build claimed",
                short(rows.rev.as_deref().unwrap_or("?"))
            ),
            None => "nothing public".to_string(),
        };
    }

    /// The release commit R and its version V — from this clone's release in progress,
    /// `--commit`, `--bump` or main's tip — each checked the way the stage and the cut
    /// will check it.
    fn choose_candidate(&mut self) -> Flow {
        let tip = self.tip.clone();
        let j = self.journal.clone();
        let unfinished = j.as_ref().filter(|j| !j.finished());
        // THE RE-EXEC KEEPS R. This process chose R and journaled it (step 3) before it
        // re-ran as R's own cutter — the same pid, so its claim is adopted — and the
        // arguments it re-ran with are the operator's: a `--commit origin/main` resolved
        // again would name main's tip NOW, moved while R's cutter was built.
        let reexeced = self.claim.as_ref().is_some_and(|c| c.adopted);
        let mut cand = if let Some(j) = j.as_ref().filter(|_| reexeced) {
            Candidate {
                commit: j.commit.clone(),
                version: j.version.clone(),
                why: "the candidate this run chose before it re-ran as its own cutter".into(),
                bump_from: None,
            }
        } else if let Some(rev) = self.opts.commit.clone() {
            let Some(r) = self.b.rev(&format!("{rev}^{{commit}}")) else {
                return self.refuse_hard(Refusal::new(
                    format!("--commit {rev} is not a commit in this clone"),
                    "git fetch origin, and name a commit on origin/main",
                ));
            };
            let mut why = "--commit".to_string();
            if let Some(j) = unfinished
                && j.commit != r
            {
                let published = self.b.engine_rows(Some(&j.commit)).has;
                let claimed = self.b.cut_journal().is_some_and(|c| c.version == j.version);
                if published || claimed {
                    self.cand = Some(Candidate {
                        commit: j.commit.clone(),
                        version: j.version.clone(),
                        why: String::new(),
                        bump_from: None,
                    });
                    self.public_state();
                    return self.refuse_hard(Refusal::new(
                        format!(
                            "the release in progress here (v{} from {}) is already published — \
                             the cut releases that commit",
                            j.version,
                            short(&j.commit)
                        ),
                        format!(
                            "finish it: {RELEASE_COMMAND} without --commit; or drop it and \
                             release the next MINOR: {RELEASE_COMMAND} --forget, then --bump"
                        ),
                    ));
                }
                // THE READY CANDIDATE IS REPLACED ONLY OUT LOUD: what it costs.
                let lane = self.est_or_default(Step::Trust);
                let measure = self.est_or_default(Step::Measure);
                self.say(&format!(
                    "DECISION: --commit {} replaces the unpublished candidate v{} from {} — \
                     {} costs a whole-tree Trust lane (about {}) and a MEASURE run (about {}) \
                     unless its facts already stand",
                    short(&r),
                    j.version,
                    short(&j.commit),
                    short(&r),
                    fmt_dur(lane),
                    fmt_dur(measure)
                ));
                why = format!(
                    "--commit, replacing the unpublished candidate {}",
                    short(&j.commit)
                );
            }
            Candidate {
                commit: r,
                version: String::new(),
                why,
                bump_from: None,
            }
        } else if let Some(j) = unfinished {
            let mut why =
                "the release in progress in this clone (its READY candidate is kept)".to_string();
            if self.opts.bump {
                why.push_str("; its bump is made, and --bump never bumps twice");
            }
            Candidate {
                commit: j.commit.clone(),
                version: j.version.clone(),
                why,
                bump_from: None,
            }
        } else if let Some(j) = j.as_ref().filter(|j| {
            !self.opts.bump && self.b.version_at(&tip).as_deref() == Some(j.version.as_str())
        }) {
            // this clone's FINISHED release, main not bumped since: every step skips
            // and the check says it again
            Candidate {
                commit: j.commit.clone(),
                version: j.version.clone(),
                why: "this clone's finished release; --bump starts the next".to_string(),
                bump_from: None,
            }
        } else if self.opts.bump {
            let v0 = self.b.version_at(&tip).unwrap_or_default();
            let Some((major, minor)) = public_version(&v0) else {
                return self.refuse_hard(Refusal::new(
                    format!("origin/main's version '{v0}' is not MAJOR.MINOR.0"),
                    "fix [workspace.package] version on main",
                ));
            };
            if !self.version_used(&v0, &tip) {
                return self.refuse_hard(Refusal::new(
                    format!(
                        "origin/main already declares {v0}, which no release has used (no tag, \
                         no ledger line, not published) — a bump would skip it"
                    ),
                    "release it as it is: drop --bump",
                ));
            }
            let v = format!("{major}.{}.0", minor + 1);
            Candidate {
                commit: tip.clone(),
                why: format!("--bump {v0} -> {v} on origin/main's tip {}", short(&tip)),
                version: v,
                bump_from: Some(v0),
            }
        } else {
            Candidate {
                commit: tip.clone(),
                version: String::new(),
                why: "origin/main's tip".to_string(),
                bump_from: None,
            }
        };

        // R is on origin/main: `pub stage --commit` stages nothing else
        if !self.b.is_ancestor(&cand.commit, &tip) {
            self.cand = Some(cand.clone());
            return self.refuse_hard(Refusal::new(
                format!(
                    "{} is not on origin/main — the engine stages only mainline commits",
                    short(&cand.commit)
                ),
                format!("land it on main first, then re-run {RELEASE_COMMAND}"),
            ));
        }
        if cand.version.is_empty() {
            cand.version = self.b.version_at(&cand.commit).unwrap_or_default();
        }
        self.cand = Some(cand.clone());
        let (r, v) = (cand.commit.clone(), cand.version.clone());
        if public_version(&v).is_none() {
            return self.refuse_hard(Refusal::new(
                format!(
                    "{} declares version '{v}', not a public MAJOR.MINOR.0",
                    short(&r)
                ),
                "--bump commits the next MINOR",
            ));
        }
        // A BUMP IS REFUSED BEFORE ITS PUSH by what would refuse its commit: the checks
        // below read R as its parent (main's tip, which the bump commit only extends) and
        // V as the version it will declare; only the engine's rows wait for the commit.

        // R contains the release it succeeds: that release's CLAIM commit (what its tag
        // on origin names), so its ledger line and changelog roll are in R
        if let Some(prev) = self.prev_release(&r, &v) {
            if !newer(&v, &prev) {
                return self.refuse_hard(Refusal::new(
                    format!("v{v} is not newer than v{prev}, the release it would succeed"),
                    "--bump commits the next MINOR",
                ));
            }
            let claim = self.b.origin_tag(&format!("v{prev}")).unwrap_or_default();
            if claim.is_empty() || !self.b.is_ancestor(&claim, &r) {
                return self.refuse_hard(Refusal::new(
                    format!(
                        "{} does not contain v{prev}'s claim commit {}, so the release it \
                         succeeds is not in its history",
                        short(&r),
                        short(&claim)
                    ),
                    format!(
                        "release a commit of origin/main after that claim: {RELEASE_COMMAND} \
                         --commit origin/main"
                    ),
                ));
            }
        }

        // the version is free, or this clone's cut of it goes on
        if self.b.origin_tag(&format!("v{v}")).is_some() && !self.claimed() {
            let remedy = match &cand.bump_from {
                Some(v0) => format!(
                    "origin/main still declares {v0}: find what cut v{v} (git ls-remote --tags \
                     origin v{v}) and bring main's version past it, then {RELEASE_COMMAND} --bump"
                ),
                None => format!("release the next MINOR: {RELEASE_COMMAND} --bump"),
            };
            return self.refuse_hard(Refusal::new(
                format!("v{v} is already cut (its tag is on origin), and this clone journals no cut of it"),
                remedy,
            ));
        }
        if self.b.origin_tag(&format!("v{v}")).is_none()
            && self.b.local_tag(&format!("v{v}"))
            && !self.claimed()
        {
            return self.refuse_hard(Refusal::new(
                format!("tag v{v} exists in this clone and not on origin — a leftover"),
                format!("git tag -d v{v}, then re-run {RELEASE_COMMAND}"),
            ));
        }
        if cand.bump_from.is_some() {
            return Ok(());
        }

        // the engine's newest source release is this version from another commit: the
        // cut would release THAT commit
        let rows = self.b.engine_rows(Some(&r));
        if let Some(newest) = rows.newest.as_deref()
            && !rows.has
            && self.b.version_at(newest).as_deref() == Some(v.as_str())
            && self.b.origin_tag(&format!("v{v}")).is_none()
        {
            return self.refuse_hard(Refusal::new(
                format!(
                    "v{v}'s source was published from {}, not {} — the cut releases the \
                     published commit",
                    short(newest),
                    short(&r)
                ),
                format!(
                    "finish that release: {RELEASE_COMMAND} --commit {newest}; or release the \
                     next MINOR: {RELEASE_COMMAND} --bump"
                ),
            ));
        }
        // R is published, but a newer row followed it: the cut would release THAT one
        if let Some(r) = self.newest_row_refusal() {
            return self.refuse_hard(r);
        }
        Ok(())
    }

    fn est_or_default(&mut self, step: Step) -> u64 {
        if self.est.is_empty() {
            let tsv = self.b.times();
            self.est = estimates(&tsv);
        }
        self.est_secs(step)
    }

    /// A version some release has used: its tag is on origin, the ledger lists it, or
    /// the engine's newest source release declares it.
    fn version_used(&mut self, version: &str, tip: &str) -> bool {
        if self.b.origin_tag(&format!("v{version}")).is_some() {
            return true;
        }
        if self.b.ledger_versions(tip).iter().any(|v| v == version) {
            return true;
        }
        let rows = self.b.engine_rows(None);
        rows.newest
            .as_deref()
            .is_some_and(|n| self.b.version_at(n).as_deref() == Some(version))
    }

    /// The release this one succeeds: the newest version in `rev`'s ledger, other than
    /// `version`, whose tag is on origin.
    fn prev_release(&mut self, rev: &str, version: &str) -> Option<String> {
        let versions = self.b.ledger_versions(rev);
        versions
            .iter()
            .rev()
            .filter(|v| v.as_str() != version)
            .find(|v| self.b.origin_tag(&format!("v{v}")).is_some())
            .cloned()
    }

    /// THE CUT BUILDS THE ENGINE'S NEWEST aterm ROW, whatever R is. So once R is
    /// published and not yet claimed, R must still be that row — asked at step 1, after
    /// the sync and on the synced engine just before a fresh cut, since the engine's
    /// origin moves while the long runs do.
    fn newest_row_refusal(&mut self) -> Option<Refusal> {
        let r = self.commit_r();
        let rows = self.b.engine_rows(Some(&r));
        let newest = rows.newest.clone()?;
        if !rows.has || newest == r || self.claimed() {
            return None;
        }
        let nv = self.b.version_at(&newest).unwrap_or_else(|| "?".into());
        Some(Refusal::new(
            format!(
                "the engine's newest aterm row is {} (v{nv}), not {} — the cut releases the \
                 newest row, so it would cut that commit, not this release",
                short(&newest),
                short(&r)
            ),
            format!(
                "release the newest row instead: {RELEASE_COMMAND} --forget, then \
                 {RELEASE_COMMAND} --commit {newest}; or leave both behind: {RELEASE_COMMAND} \
                 --forget, then {RELEASE_COMMAND} --bump"
            ),
        ))
    }

    // -- step 3 -----------------------------------------------------------

    fn commit(&mut self) -> Result<String, Halt> {
        let Some(mut cand) = self.cand.clone() else {
            return Ok(String::new());
        };
        if self.dry {
            return Ok(match &cand.bump_from {
                Some(_) => format!(
                    "would commit `release: {} — the version and the lockfile move together` on \
                     {} (pub-bump-edit, then targo update --workspace --offline) and push it to \
                     main",
                    cand.version,
                    short(&self.tip)
                ),
                None => format!(
                    "would put this clone's main at {} (v{})",
                    short(&cand.commit),
                    cand.version
                ),
            });
        }
        let prover = self.prover.clone().ok_or(Halt::Said(1))?;
        let mut bumped = self
            .journal
            .as_ref()
            .is_some_and(|j| j.commit == cand.commit && j.bumped);
        if let Some(from) = cand.bump_from.clone() {
            match self.b.bump(&from, &cand.version, &prover) {
                Ok(r) => {
                    bumped = true;
                    cand.why = format!("the bump {from} -> {}, pushed to main", cand.version);
                    cand.commit = r;
                    cand.bump_from = None;
                    self.cand = Some(cand.clone());
                }
                Err(r) => {
                    self.refuse(r)?;
                }
            }
        }
        if let Err(r) = self.b.move_to(&cand.commit) {
            self.refuse(r)?;
        }
        let same = self
            .journal
            .as_ref()
            .is_some_and(|j| j.commit == cand.commit);
        if !same {
            let now = self.b.now();
            // the owner's open asks are this machine's, whatever the candidate
            let asks = self
                .journal
                .as_ref()
                .map(|j| j.asks.clone())
                .unwrap_or_default();
            self.journal = Some(Journal {
                version: cand.version.clone(),
                commit: cand.commit.clone(),
                prover: prover.bin.display().to_string(),
                trustc: prover.commit.clone(),
                bumped,
                started: now,
                linux: self.linux.clone(),
                done: Vec::new(),
                asks,
                reserved: None,
                artifact: None,
            });
        } else if !self.claimed()
            && let Some(j) = self.journal.as_mut()
        {
            // before a claim, a new prover is this release's prover from now on — and
            // a bundle another prover built is not this release's (step 7 builds it
            // again: `artifact::key` holds the prover's commit); after the claim the
            // bundle is built and measured, and no prover changes its bytes
            j.prover = prover.bin.display().to_string();
            j.trustc = prover.commit.clone();
            if self.linux.declared() {
                j.linux = self.linux.clone();
            }
        }
        if let Some(j) = &self.journal
            && let Err(why) = j.save(&self.journal_path)
        {
            self.refuse(Refusal::new(
                why,
                "check the permissions of the clone's .git/aterm-release",
            ))?;
        }
        // the engine was synced at step 2: is R (if published) still its newest row?
        if let Some(r) = self.newest_row_refusal() {
            self.refuse(r)?;
        }
        self.public_state();
        Ok(format!(
            "v{} at {} ({})",
            cand.version,
            short(&cand.commit),
            cand.why
        ))
    }

    /// EVERY JUDGMENT OF R IS R'S OWN CUTTER'S. Once the clone is at R, a binary built
    /// from another commit whose cutter sources moved re-execs as R's own cutter
    /// (`targo --unverified ship release`, same arguments, same pid). A re-exec that is
    /// still not R's cutter refuses instead of looping.
    fn own_cutter(&mut self) -> Flow {
        if self.dry {
            return Ok(());
        }
        let r = self.commit_r();
        match self.b.own_cutter(&r) {
            Ok(_) => Ok(()),
            Err(why) => {
                if self.claim.as_ref().is_some_and(|c| c.adopted) {
                    return self.refuse(Refusal::new(
                        format!(
                            "this cutter was rebuilt at {} and is still not its own ({why})",
                            short(&r)
                        ),
                        format!(
                            "targo clean --release -p aterm-release, then {RELEASE_COMMAND} \
                             (nothing was claimed)"
                        ),
                    ));
                }
                // an exec forgets the signals this process caught: one caught in step 3
                // (the bump's push, the move to R) ends the run here, never in R's cutter
                self.halt_if_interrupted()?;
                self.say(&format!(
                    "release: {why} — re-running as {}'s own cutter ({RELEASE_COMMAND}, same \
                     arguments)",
                    short(&r)
                ));
                let prover = self.prover.clone().ok_or(Halt::Said(1))?;
                let refusal = self.b.reexec(&prover);
                self.refuse(refusal)
            }
        }
    }

    // -- facts and skip rules -----------------------------------------------

    fn refresh(&mut self) {
        let Some(c) = self.cand.clone() else {
            return;
        };
        if self.dry && c.bump_from.is_some() {
            let pending = Answer::no("the bump commit is not made yet");
            self.facts = Facts {
                trusted: pending.clone(),
                built: pending.clone(),
                measured: pending,
                ..Facts::default()
            };
            return;
        }
        let Some(p) = self.prover.clone() else {
            return;
        };
        let artifact = self.artifact();
        let mut facts = self.b.facts(&c, &p, artifact.as_ref());
        facts.cut = facts.cut.filter(|cut| cut.version == c.version);
        // A reservation that can never be claimed now — origin's ledger has reached
        // it — is never signed: its bytes carry its number, so they are neither built
        // nor measured for this release any more, and step 7 reserves anew.
        if facts.cut.is_none()
            && let Some(spent) = self.spent()
        {
            let stale = Answer::no(format!(
                "{}; step 7 reserves anew and builds again",
                spent.words()
            ));
            facts.built = stale.clone();
            facts.measured = stale;
        }
        self.facts = facts;
        if c.bump_from.is_none() {
            self.public_state();
        }
    }

    fn claimed_words(&self) -> Option<String> {
        let cut = self.facts.cut.as_ref()?;
        Some(format!(
            "v{} is claimed (build {}): the cut is past its pre-claim gates",
            cut.version, cut.build
        ))
    }

    fn skip_trust(&self) -> Option<String> {
        self.claimed_words().or_else(|| {
            self.facts
                .trusted
                .yes
                .then(|| self.facts.trusted.words.clone())
        })
    }

    fn skip_build(&self) -> Option<String> {
        self.claimed_words()
            .or_else(|| self.facts.built.yes.then(|| self.facts.built.words.clone()))
    }

    fn skip_measure(&self) -> Option<String> {
        self.claimed_words().or_else(|| {
            self.facts
                .measured
                .yes
                .then(|| self.facts.measured.words.clone())
        })
    }

    /// The journal's reservation of R, if it holds one.
    fn reservation(&self) -> Option<Reservation> {
        let r = self.commit_r();
        self.journal
            .as_ref()
            .filter(|j| j.commit == r)
            .and_then(|j| j.reserved.clone())
    }

    /// The journal's reservation of R when origin's ledger tail has reached it, so its
    /// bytes can never ship — and why: another release claimed past it, or THIS clone's
    /// claim of it landed (origin/main holds its release commit) while no cut journal
    /// holds it (the cut stopped between its push and its journal).
    fn spent(&mut self) -> Option<Spent> {
        let r = self.reservation()?;
        let tail = self.b.origin_tail()?;
        if tail < r.build {
            return None;
        }
        let claimed_here = self.b.is_ancestor(&r.commit, "origin/main");
        Some(Spent {
            reservation: r,
            tail,
            claimed_here,
        })
    }

    /// The journal's bundle of R, if it holds one.
    fn artifact(&self) -> Option<Artifact> {
        let r = self.commit_r();
        self.journal
            .as_ref()
            .filter(|j| j.commit == r)
            .and_then(|j| j.artifact.clone())
    }

    fn skip_stage(&self) -> Option<String> {
        let r = self.commit_r();
        if let Some(cut) = &self.facts.cut {
            return Some(format!("v{} is claimed (build {})", cut.version, cut.build));
        }
        if self.facts.engine.has {
            return Some(format!(
                "the source of {} is published (alabsystems {})",
                short(&r),
                short(self.facts.engine.rev.as_deref().unwrap_or("?"))
            ));
        }
        self.facts.attested.then(|| {
            format!(
                "{} is staged (publish/.out holds its promotable attestation)",
                short(&r)
            )
        })
    }

    fn skip_publish(&self) -> Option<String> {
        let r = self.commit_r();
        if let Some(cut) = &self.facts.cut {
            return Some(format!("v{} is claimed (build {})", cut.version, cut.build));
        }
        self.facts.engine.has.then(|| {
            format!(
                "the engine's ledger records {} -> alabsystems {}",
                short(&r),
                short(self.facts.engine.rev.as_deref().unwrap_or("?"))
            )
        })
    }

    fn cut_mode(&self) -> CutMode {
        match &self.facts.cut {
            Some(cut) if cut.finished => CutMode::Done(format!(
                "the cut of v{} finished (build {})",
                cut.version, cut.build
            )),
            Some(cut) => CutMode::Resume(cut.build),
            None => CutMode::Fresh,
        }
    }

    /// The build, the MEASURE run or the cut is still to come.
    fn lock_needed(&self) -> bool {
        self.skip_build().is_none()
            || self.skip_measure().is_none()
            || !matches!(self.cut_mode(), CutMode::Done(_))
    }

    fn plan_print(&mut self) {
        let tsv = self.b.times();
        self.est = estimates(&tsv);
        let measured_here = !tsv.trim().is_empty();
        let mut run = Vec::new();
        let mut done = Vec::new();
        for step in &Step::ALL[3..] {
            let skipped = match step {
                Step::PreflightEarly | Step::Preflight => self.claimed_words().is_some(),
                Step::Trust => self.skip_trust().is_some(),
                Step::Lock => !self.lock_needed(),
                Step::Build => self.skip_build().is_some(),
                Step::Measure => self.skip_measure().is_some(),
                Step::Stage => self.skip_stage().is_some(),
                Step::Publish => self.skip_publish().is_some(),
                Step::Cut => matches!(self.cut_mode(), CutMode::Done(_)),
                Step::AfterCut => self.journal_has("after-cut"),
                _ => false,
            };
            if skipped {
                done.push(step.name());
            } else {
                run.push(*step);
            }
        }
        self.plan_secs = run.iter().map(|s| self.est_secs(*s)).sum();
        self.planned = run.clone();
        let (v, r) = (self.version(), self.commit_r());
        let why = self
            .cand
            .as_ref()
            .map(|c| c.why.clone())
            .unwrap_or_default();
        let prover = self.prover.as_ref().map_or_else(
            || "none".to_string(),
            |p| {
                format!(
                    "{} (trustc {}; {})",
                    p.bin.display(),
                    short(&p.commit),
                    p.source
                )
            },
        );
        self.say(&format!(
            "release: aterm {v} from {} ({why}); prover {prover}",
            short(&r)
        ));
        let names: Vec<&str> = run.iter().map(|s| s.name()).collect();
        self.say(&format!(
            "release: to run: {}; done already: {}",
            if names.is_empty() {
                "nothing".to_string()
            } else {
                names.join(" ")
            },
            if done.is_empty() {
                "nothing".to_string()
            } else {
                done.join(" ")
            }
        ));
        let now = self.b.now();
        let at = self.b.hms(now + self.plan_secs);
        self.say(&format!(
            "release: ETA {} — {} of work, by {} (a wait for the machine's lock or for the \
             owner is not in it)",
            &at[..at.len().min(5)],
            fmt_dur(self.plan_secs),
            if measured_here {
                "this machine's measured step times"
            } else {
                "the plan's measured defaults"
            }
        ));
    }

    // -- step 4 and 8: the preflight ------------------------------------------

    /// The disk the steps still to come need, asked before the hours of the trust lane:
    /// the build and its MEASURE run as the gate budgets them (39 GiB from empty lanes,
    /// 30 on warm ones), or for MEASURED bytes whose tree is not yet TRUSTED the trust
    /// lane's 6 then the cut's 10.
    fn disk_still_needed(&mut self) -> Flow {
        if self.facts.measured.yes && self.facts.trusted.yes {
            return Ok(());
        }
        let (free, warm) = self.b.disk();
        let Some(free) = free else {
            return Ok(());
        };
        if self.facts.measured.yes {
            if free < 6 + CUT_GIB {
                return self.refuse(Refusal::new(
                    format!("only {free} GiB free — the trust run needs 6, then the cut {CUT_GIB}"),
                    "free space, then re-run",
                ));
            }
            return Ok(());
        }
        let (need, what) = if warm {
            (30, "30 (its lanes are warm)")
        } else {
            (39, "39 (cold)")
        };
        if free < need {
            return self.refuse(Refusal::new(
                format!(
                    "only {free} GiB free — the build and MEASURE run still to come need {what}, \
                     then the cut {CUT_GIB}"
                ),
                "free space, then re-run",
            ));
        }
        Ok(())
    }

    /// The preflight: EARLY, every block but the receipt ones (MEASURE, trust: what
    /// steps 5-8 answer) refuses; STRICT, every block — and, asked again right before
    /// the stage, what the cut would refuse that can change while the long steps run
    /// (its disk, the version's tag, its read-only pre-claim gates). A judge that could
    /// not answer is a resumable stop at once, never a "no" the long steps would
    /// answer. Then this machine's provisioning (a machine gap refuses, an owner gap is
    /// asked — and, strict, waited on), then the handoff fixture guard, once per R.
    fn preflight(&mut self, strict: bool) -> Flow {
        let Some(c) = self.cand.clone() else {
            return Ok(());
        };
        let Some(p) = self.prover.clone() else {
            return Ok(());
        };
        let mut words = Vec::new();
        // a dry run's step 9 plans steps 5-8 rather than asking their receipts again
        let planned = self.dry && strict;
        let mut blocks = if planned {
            words.push(
                "would run the preflight again, and stage only on READY; asked now: what \
                 only reads"
                    .to_string(),
            );
            Vec::new()
        } else {
            if self.dry {
                words.push(
                    "would run the engine's own dry run (every export guard) and the wasm32 \
                     check; asked now: what reads"
                        .to_string(),
                );
            }
            self.b.preflight(&c, self.dry, strict)
        };
        if !planned {
            // the two receipts the cut requires, which steps 5-8 answer
            for (name, answer, remedy) in [
                (
                    "MEASURE tier",
                    &self.facts.measured,
                    "steps 7-8 build and measure it",
                ),
                ("TRUST lane", &self.facts.trusted, "step 5 runs the lane"),
            ] {
                if !answer.yes && !answer.unanswered {
                    blocks.push((
                        true,
                        Refusal::new(format!("{name} — {}", answer.words), remedy),
                    ));
                }
            }
        }
        if strict {
            for r in self.cut_still_possible(&c, true) {
                blocks.push((false, r));
            }
        }
        let receipts = blocks.iter().filter(|(receipt, _)| *receipt).count();
        let real: Vec<Refusal> = blocks
            .into_iter()
            .filter(|(receipt, _)| strict || !*receipt)
            .map(|(_, r)| r)
            .collect();
        if !real.is_empty() {
            self.refuse(blocked(
                format!("the preflight BLOCKED ({} finding(s))", real.len()),
                &real,
            ))?;
        }
        // a judge that could not answer decided nothing: stop now, not after the hours
        // of the long steps whose receipts it would then not read either
        if !planned {
            let unanswered: Vec<String> = [&self.facts.measured, &self.facts.trusted]
                .into_iter()
                .filter(|a| a.unanswered)
                .map(|a| a.words.clone())
                .collect();
            if !unanswered.is_empty() {
                self.refuse(
                    Refusal::new(
                        format!(
                            "the cutter's own judge could not answer: {}",
                            unanswered.join("; ")
                        ),
                        format!(
                            "run {RELEASE_COMMAND} again once it answers (the candidate is kept)"
                        ),
                    )
                    .could_not_run(),
                )?;
            }
        }
        words.push(if !real.is_empty() {
            format!("BLOCKED ({} finding(s), above)", real.len())
        } else if receipts > 0 && !strict {
            format!("no block but the {receipts} receipt block(s) steps 5-8 answer")
        } else {
            "READY".to_string()
        });
        words.push(self.provision(strict)?);
        if self.journal_has("fixture-guard") {
            words.push(format!(
                "the handoff fixture guard passed for {} (this release's journal)",
                short(&c.commit)
            ));
        } else if self.dry {
            words.push(
                "would run the handoff fixture guard (targo --unverified test -p aterm-gui --lib, \
                 as the cut runs it)"
                    .to_string(),
            );
        } else {
            match self.b.fixture_guard(&p) {
                Ok(note) => {
                    self.journal_add("fixture-guard")?;
                    words.push(note);
                }
                Err(r) => self.refuse(r)?,
            }
        }
        self.finish(&words.join("; "));
        Ok(())
    }

    /// What the cut would refuse that can change while the long steps run — its disk
    /// (the trust lane and MEASURE write tens of GiB), the version's tag (a rostered
    /// peer may cut it meanwhile) and, with `gates`, the cut's own read-only pre-claim
    /// gates — asked right before the stage, and the first two again right before a
    /// fresh cut, so the refusal arrives with nothing (more) public.
    fn cut_still_possible(&mut self, c: &Candidate, gates: bool) -> Vec<Refusal> {
        let mut out = Vec::new();
        if let (Some(free), _) = self.b.disk()
            && free < CUT_GIB
        {
            out.push(Refusal::new(
                format!("only {free} GiB free — the cut needs {CUT_GIB}"),
                "free space, then re-run",
            ));
        }
        let tag = format!("v{}", c.version);
        match self.b.origin_tag_now(&tag) {
            Ok(Some(at)) if !self.claimed() => out.push(Refusal::new(
                format!(
                    "{tag} is on origin now (at {}) — it was cut while this release ran",
                    short(&at)
                ),
                format!(
                    "release the next MINOR: {RELEASE_COMMAND} --forget, then {RELEASE_COMMAND} \
                     --bump"
                ),
            )),
            Ok(_) => {
                if !self.claimed() && self.b.local_tag(&tag) && self.b.origin_tag(&tag).is_none() {
                    out.push(Refusal::new(
                        format!("tag {tag} exists in this clone and not on origin — a leftover"),
                        format!("git tag -d {tag}, then re-run {RELEASE_COMMAND}"),
                    ));
                }
            }
            Err(why) => out.push(Refusal::new(
                format!("cannot ask origin whether {tag} is cut: {why}"),
                "check the network and your git credentials, then re-run",
            )),
        }
        if gates {
            let creds = self.creds.clone();
            out.extend(self.b.cut_gates(c, &creds));
        }
        out
    }

    /// ASKED AGAIN RIGHT BEFORE WHAT GOES PUBLIC. The stage and the publish each audit
    /// the provisioning first, and may wait there for the owner — hours, the machine's
    /// lock let go — after step 8 asked [`Run::cut_still_possible`]. So it is asked
    /// again after that wait, immediately before `pub stage` (whose attestation the
    /// publish promotes) and before `pub publish` (which makes the source public): the
    /// cut's disk, the version's tag on origin now, a leftover local tag, and the cut's
    /// read-only pre-claim gates (the release lease, the roster, the channel floor). A
    /// refusal here leaves no source public ahead of a binary that cannot follow it.
    fn still_possible(&mut self, c: &Candidate) -> Flow {
        let found = self.cut_still_possible(c, true);
        if found.is_empty() {
            return Ok(());
        }
        let step = self.cur.name();
        self.refuse(blocked(
            format!(
                "asked again right before the {step}, the cut would refuse ({} finding(s))",
                found.len()
            ),
            &found,
        ))
    }

    /// THIS MACHINE'S PROVISIONING, audited now ([`crate::provision::audit`]): a gap
    /// this machine's operator closes refuses; a gap only the owner closes is asked
    /// ONCE (a standing ask on the owner's aterm, journaled only once it was delivered,
    /// and raised again at each audit until it is) and — at a step that needs it closed
    /// (`wait`) — waited on, re-audited every [`OWNER_POLL_SECS`], the machine's lock let
    /// go for the whole wait and taken again, in its line, only once the audit passes
    /// (a lock still busy then is a resumable stop); an audit that could not be asked is
    /// a resumable stop where it is needed. Returns the line the step says.
    fn provision(&mut self, wait: bool) -> Result<String, Halt> {
        let mut said_waiting = false;
        let mut relock = false;
        loop {
            let id = self.machine_id.clone();
            let found = match (&self.dry_audit, self.dry) {
                (Some(found), true) => found.clone(),
                _ => {
                    let found = self.b.audit(&id);
                    if self.dry {
                        self.dry_audit = Some(found.clone());
                    }
                    found
                }
            };
            let mut owner: Vec<(String, AskWords)> = Vec::new();
            let mut unknown: Vec<String> = Vec::new();
            // the asks a check that got no answer may still need: never resolved on that
            let mut unknown_keys: BTreeSet<String> = BTreeSet::new();
            for f in &found {
                match &f.state {
                    Audited::Pass => {}
                    Audited::Gap {
                        what,
                        fix,
                        owner: false,
                    } => {
                        let r = Refusal::new(
                            format!(
                                "this machine's provisioning has a gap the cut would refuse after \
                                 the publish: {} — {what}",
                                f.label
                            ),
                            format!(
                                "{fix} (targo --unverified ship provision --check --id {id} \
                                 audits it), then {RELEASE_COMMAND}"
                            ),
                        );
                        self.refuse(r)?;
                    }
                    Audited::Gap {
                        what, owner: true, ..
                    } => owner.push((
                        format!("{} — {what}", f.label),
                        crate::provision::ask_words(f.label, what),
                    )),
                    Audited::CouldNotRun(what) => {
                        unknown.push(format!("{} — {what}", f.label));
                        unknown_keys.insert(crate::provision::ask_words(f.label, what).key);
                    }
                }
            }
            self.settle_asks(&owner, &unknown_keys)?;
            if owner.is_empty() && (unknown.is_empty() || !wait) {
                if relock {
                    let caller = self.lock_caller();
                    match self.b.lock(&caller) {
                        Ok(_) => self.locked = true,
                        Err(r) => self.refuse(r.could_not_run())?,
                    }
                }
                let mut line = format!("this machine's provisioning passes ({id})");
                if !unknown.is_empty() {
                    line.push_str(&format!(
                        "; not answered now, asked again before the stage: {}",
                        unknown.join("; ")
                    ));
                }
                return Ok(line);
            }
            if !wait {
                let gaps: Vec<&str> = owner.iter().map(|(g, _)| g.as_str()).collect();
                return Ok(format!(
                    "{} the owner once, and the long steps go on: {}",
                    if self.dry { "would ask" } else { "asked" },
                    gaps.join("; ")
                ));
            }
            if owner.is_empty() {
                self.refuse(
                    Refusal::new(
                        format!(
                            "this machine's provisioning could not be asked: {}",
                            unknown.join("; ")
                        ),
                        format!(
                            "run {RELEASE_COMMAND} again once it answers (the candidate is kept)"
                        ),
                    )
                    .could_not_run(),
                )?;
                return Ok("would stop: the provisioning could not be asked".to_string());
            }
            let gaps: Vec<String> = owner.iter().map(|(g, _)| g.clone()).collect();
            if self.dry {
                self.waits.extend(gaps.iter().cloned());
                self.say(&format!(
                    "[{:>2}/{}] {:<15} WOULD WAIT  for the owner: {} (asked once; re-audited \
                     every {} until it closes)",
                    self.cur.number(),
                    Step::ALL.len(),
                    self.cur.name(),
                    gaps.join("; "),
                    fmt_dur(OWNER_POLL_SECS)
                ));
                return Ok("would wait for the owner".to_string());
            }
            if !said_waiting {
                let (v, r) = (self.version(), self.commit_r());
                let ready = if self.facts.trusted.yes && self.facts.measured.yes {
                    "READY (trusted, built once, measured)"
                } else {
                    "not yet READY"
                };
                let public = self.public.clone();
                self.say(&format!(
                    "WAITING (owner): {} — v{v} from {} is {ready}; {public}. Re-audited every \
                     {}; the release goes on when it closes",
                    gaps.join("; "),
                    short(&r),
                    fmt_dur(OWNER_POLL_SECS)
                ));
                said_waiting = true;
            }
            // the machine's lock is let go for the whole wait: peers' gates go on
            if self.locked {
                self.b.unlock();
                self.locked = false;
                relock = true;
            }
            let t0 = self.b.now();
            self.b.sleep(OWNER_POLL_SECS);
            self.waited += self.b.now().saturating_sub(t0);
            self.halt_if_interrupted()?;
        }
    }

    /// Raise each open owner gap's ask once — journaled by key only when the owner's
    /// aterm took it, so an ask that did not reach him is raised again at the next
    /// audit — and resolve each journaled ask whose gap's check now PASSES (a check
    /// that got no answer resolves nothing).
    fn settle_asks(&mut self, owner: &[(String, AskWords)], unknown: &BTreeSet<String>) -> Flow {
        if self.dry {
            return Ok(());
        }
        let open: BTreeSet<String> = owner.iter().map(|(_, w)| w.key.clone()).collect();
        let asked: Vec<String> = self
            .journal
            .as_ref()
            .map(|j| j.asks.clone())
            .unwrap_or_default();
        let mut changed = false;
        let mut now_asked: BTreeSet<String> = BTreeSet::new();
        for (_, words) in owner {
            if asked.contains(&words.key) || !now_asked.insert(words.key.clone()) {
                continue;
            }
            match self.b.ask(words) {
                Ok(reply) => {
                    self.say(&format!(
                        "        asked the owner once ({}): {} — {} (delivered: {reply})",
                        words.key, words.reason, words.step
                    ));
                    if let Some(j) = self.journal.as_mut() {
                        j.asks.push(words.key.clone());
                        changed = true;
                    }
                }
                Err(why) => self.say(&format!(
                    "        could not ask the owner ({}): {why} — asked again at the next audit",
                    words.key
                )),
            }
        }
        for key in asked
            .iter()
            .filter(|k| !open.contains(*k) && !unknown.contains(*k))
        {
            match self.b.resolve(key) {
                Ok(reply) => {
                    self.say(&format!("        the owner closed {key} ({reply})"));
                    if let Some(j) = self.journal.as_mut() {
                        j.asks.retain(|k| k != key);
                        changed = true;
                    }
                }
                Err(why) => self.say(&format!(
                    "        the owner closed {key}; its row could not be resolved ({why}) — \
                     resolved again at the next audit"
                )),
            }
        }
        if changed
            && let Some(j) = &self.journal
            && let Err(why) = j.save(&self.journal_path)
        {
            self.refuse(Refusal::new(
                why,
                "check the permissions of the clone's .git/aterm-release",
            ))?;
        }
        Ok(())
    }

    // -- step 5: trust ----------------------------------------------------------

    fn trust(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        let pending = if self.dry && c.bump_from.is_some() {
            Vec::new()
        } else {
            self.b.trust_pending(&c, &p)
        };
        let words = self.facts.trusted.words.clone();
        if self.dry {
            if pending.is_empty() {
                self.finish(&format!(
                    "would run tools/trust-gate-all.sh over the whole tree ({words})"
                ));
            } else {
                self.finish(&format!(
                    "would run tools/trust-gate-all.sh --remeasure on only what the stored facts \
                     left undecided: {} ({words})",
                    pending.join(" ")
                ));
            }
            return Ok(());
        }
        self.provision(false)?;
        let log = self.b.log_path(Step::Trust);
        if pending.is_empty() {
            self.say(&format!("        runs: {words}"));
        } else {
            self.say(&format!(
                "        runs: --remeasure, only what the stored facts left undecided: {} ({words})",
                pending.join(" ")
            ));
        }
        self.say(&format!("        log: {}", log.display()));
        let ran = self.b.trust_lane(&p, !pending.is_empty());
        self.refresh();
        if self.facts.trusted.yes {
            let words = self.facts.trusted.words.clone();
            self.finish(&words);
            return Ok(());
        }
        let words = self.facts.trusted.words.clone();
        if self.facts.trusted.unanswered {
            return self.refuse(
                Refusal::new(
                    format!(
                        "the lane ran (exit {}), and its judge could not answer: {words}",
                        ran.code
                    ),
                    format!(
                        "run {RELEASE_COMMAND} again once it answers (the lane's facts are kept)"
                    ),
                )
                .detail(ran.detail)
                .could_not_run(),
            );
        }
        let r = short(&c.commit).to_string();
        // 1 (FAIL) outranks 2 (COULD NOT RUN): a failed library is the tree's to fix.
        if ran.code == 4 {
            return self.refuse(
                Refusal::new(
                    format!(
                        "this machine cannot verify every library of {r} (NARROWED BY MEMORY; \
                         nothing failed)"
                    ),
                    "release from a clone on a machine with more RAM (each line below names its need)",
                )
                .detail(ran.detail),
            );
        }
        if ran.code != 1 {
            let left = self.b.trust_pending(&c, &p);
            if !left.is_empty() {
                return self.refuse(
                    Refusal::new(
                        format!(
                            "the trust lane decided nothing about {} for {r} (exit {}; {words}) — \
                             nothing failed, the machine's clock or trustd left them unmeasured",
                            left.join(" "),
                            ran.code
                        ),
                        format!(
                            "run {RELEASE_COMMAND} again, from any session in this clone, when the \
                             machine is quieter: it measures only those again (--remeasure)"
                        ),
                    )
                    .detail(ran.detail)
                    .could_not_run(),
                );
            }
            if ran.code != 0 {
                return self.refuse(
                    Refusal::new(
                        format!(
                            "the whole-tree trust lane could not run (exit {}; {words})",
                            ran.code
                        ),
                        format!(
                            "nothing failed: {RELEASE_COMMAND} again once the lane's own cause \
                             (below) is gone"
                        ),
                    )
                    .detail(ran.detail)
                    .could_not_run(),
                );
            }
        }
        self.refuse(
            Refusal::new(
                format!(
                    "the whole-tree trust lane did not pass (exit {}; {words})",
                    ran.code
                ),
                format!(
                    "fix the library on main (docs/RELEASING.md, the whole-tree Trust advisory \
                     lane), then {RELEASE_COMMAND} --commit origin/main"
                ),
            )
            .detail(ran.detail),
        )
    }

    // -- step 6: the lock -------------------------------------------------------

    fn lock_caller(&self) -> String {
        format!("{RELEASE_COMMAND} v{}", self.version())
    }

    fn take_lock(&mut self) -> Flow {
        let caller = self.lock_caller();
        match self.b.lock(&caller) {
            Ok(note) => {
                self.locked = true;
                self.finish(&note);
                Ok(())
            }
            Err(r) => self.refuse(r),
        }
    }

    // -- step 7: build, once -------------------------------------------------

    /// BUILD ONCE: reserve R's identity unless the journal holds a reservation of R
    /// the ledger has not passed — a fresh one when it has (another release claimed
    /// past it while this one waited; nothing was claimed here, so nothing is burned)
    /// — journal it, then build its bundle once (or take the COMPLETE one the release
    /// cache holds) and journal that. A new reservation is new bytes: the bundle and its
    /// measurement follow it.
    fn build(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        if self.dry {
            let reserved = match (self.reservation(), self.b.origin_tail()) {
                (Some(r), Some(tail)) if r.build > tail => format!(
                    "build {} reserved as {} (this release's journal)",
                    r.build,
                    short(&r.commit)
                ),
                _ => "would reserve build n = max(origin's ledger tail + 1, now) and the \
                      release commit (R + the ledger line + the rolled changelog), claimed \
                      nowhere"
                    .to_string(),
            };
            self.finish(&format!(
                "{reserved}; would build both slices at once from its snapshot into the \
                 release cache, the unsigned bundle kept by what it is built as ({})",
                self.facts.built.words
            ));
            return Ok(());
        }
        self.provision(false)?;
        let reserved = match self.reservation() {
            Some(r) if self.b.origin_tail().is_some_and(|tail| r.build > tail) => r,
            _ => {
                if let Some(spent) = self.spent() {
                    self.say(&format!("        {} — reserving anew", spent.words()));
                }
                let reserved = match self.b.reserve(&c) {
                    Ok(r) => r,
                    Err(r) => return self.refuse(r),
                };
                if let Some(j) = self.journal.as_mut() {
                    j.reserved = Some(reserved.clone());
                    j.artifact = None;
                }
                self.save_journal()?;
                self.say(&format!(
                    "        reserved build {} as {} (`{}`), claimed nowhere until the cut",
                    reserved.build,
                    short(&reserved.commit),
                    reserved.line
                ));
                self.refresh();
                reserved
            }
        };
        let log = self.b.log_path(Step::Build);
        self.say(&format!("        log: {}", log.display()));
        let artifact = match self.b.build(&c, &reserved, &p) {
            Ok(a) => a,
            Err(r) => return self.refuse(r),
        };
        if let Some(j) = self.journal.as_mut() {
            j.artifact = Some(artifact.clone());
        }
        self.save_journal()?;
        self.refresh();
        if !self.facts.built.yes {
            let words = self.facts.built.words.clone();
            return self.refuse(Refusal::new(
                format!("the build ended, and its bundle is not the journal's: {words}"),
                format!("read its log, then {RELEASE_COMMAND} (it builds again)"),
            ));
        }
        self.finish(&format!(
            "built ONCE: {} — executable sha256 {}",
            artifact.app.display(),
            artifact.macho_sha256
        ));
        Ok(())
    }

    fn save_journal(&mut self) -> Flow {
        let Some(j) = &self.journal else {
            return Ok(());
        };
        if let Err(why) = j.save(&self.journal_path) {
            return self.refuse(Refusal::new(
                why,
                "check the permissions of the clone's .git/aterm-release",
            ));
        }
        Ok(())
    }

    // -- step 8: measure ------------------------------------------------------

    fn measure(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        let words = self.facts.measured.words.clone();
        if self.dry {
            self.finish(&format!(
                "would run tools/verify.sh --measure <the bundle step 7 built> under the \
                 release's lock ({words})"
            ));
            return Ok(());
        }
        let Some(artifact) = self.artifact() else {
            return self.refuse(Refusal::new(
                "no bundle is built for this release — MEASURE judges the bytes that ship",
                format!("{RELEASE_COMMAND} (step 7 builds it)"),
            ));
        };
        self.provision(false)?;
        let log = self.b.log_path(Step::Measure);
        self.say(&format!("        runs: {words}"));
        self.say(&format!("        judges: {}", artifact.app.display()));
        self.say(&format!("        log: {}", log.display()));
        let (ran, stage_could_not_run) = self.b.measure(&p, &artifact.app);
        self.refresh();
        if let Some(spent) = self.spent() {
            return self.refuse(
                Refusal::new(
                    format!(
                        "origin's ledger reached the reservation while it was measured: {}; \
                         these bytes can never ship",
                        spent.words()
                    ),
                    format!(
                        "run {RELEASE_COMMAND} again — it reserves anew, then builds and measures"
                    ),
                )
                .could_not_run(),
            );
        }
        if self.facts.measured.yes {
            let words = self.facts.measured.words.clone();
            self.finish(&words);
            return Ok(());
        }
        let words = self.facts.measured.words.clone();
        if self.facts.measured.unanswered {
            return self.refuse(
                Refusal::new(
                    format!(
                        "the MEASURE run ended (exit {}), and its judge could not answer: {words}",
                        ran.code
                    ),
                    format!("run {RELEASE_COMMAND} again once it answers (its receipt is kept)"),
                )
                .detail(ran.detail)
                .could_not_run(),
            );
        }
        let r = short(&c.commit).to_string();
        if ran.code == 3 && stage_could_not_run {
            return self.refuse(
                Refusal::new(
                    format!(
                        "the MEASURE tier could not run for {r} (exit 3, COULD NOT RUN; {words}) — \
                         nothing failed"
                    ),
                    format!(
                        "run {RELEASE_COMMAND} again, from any session in this clone, when the \
                         machine is quieter"
                    ),
                )
                .detail(ran.detail)
                .could_not_run(),
            );
        }
        if ran.code == 3 {
            return self.refuse(
                Refusal::new(
                    format!(
                        "the MEASURE gate could not run for {r} (exit 3; {words}) — nothing failed, \
                         and no MEASURE stage was starved: the cause below is not the machine's \
                         load"
                    ),
                    format!("remove the cause the gate names (below), then {RELEASE_COMMAND}"),
                )
                .detail(ran.detail)
                .could_not_run(),
            );
        }
        self.refuse(
            Refusal::new(
                format!(
                    "the MEASURE tier did not measure {r} (exit {}; {words})",
                    ran.code
                ),
                format!("a real red is fixed on main, then {RELEASE_COMMAND} --commit origin/main"),
            )
            .detail(ran.detail),
        )
    }

    // -- steps 10 and 11 ------------------------------------------------------

    fn receipts_stand(&mut self, what_refuses: &str) -> Flow {
        let r = short(&self.commit_r()).to_string();
        if !self.facts.measured.yes {
            let words = self.facts.measured.words.clone();
            self.refuse(Refusal::new(
                format!("no MEASURE receipt for the bundle of {r} ({words}) — {what_refuses}"),
                format!("{RELEASE_COMMAND} (steps 7-8 build and measure it)"),
            ))?;
        }
        if !self.facts.trusted.yes {
            let words = self.facts.trusted.words.clone();
            self.refuse(Refusal::new(
                format!("the whole-tree trust lane does not verify {r} ({words}) — {what_refuses}"),
                format!("{RELEASE_COMMAND} (step 5 runs the lane)"),
            ))?;
        }
        Ok(())
    }

    fn stage(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        if self.dry {
            let r = if c.bump_from.is_some() {
                "<the bump commit>".to_string()
            } else {
                c.commit.clone()
            };
            self.finish(&format!(
                "would run: pub stage aterm --commit {r} --repo-path <this clone>"
            ));
            return Ok(());
        }
        self.receipts_stand("the cut would refuse after the publish")?;
        self.provision(true)?;
        self.still_possible(&c)?;
        let ran = match self.b.stage(&c, &p) {
            Ok(ran) => ran,
            Err(r) => {
                self.refresh();
                return self.refuse(r);
            }
        };
        self.refresh();
        if self.facts.attested || self.facts.engine.has {
            self.finish(&format!(
                "staged {} (a promotable attestation in publish/.out)",
                short(&c.commit)
            ));
            return Ok(());
        }
        self.refuse(
            Refusal::new(
                format!(
                    "pub stage did not stage {} (exit {})",
                    short(&c.commit),
                    ran.code
                ),
                format!(
                    "read its log; fix the source on main, then {RELEASE_COMMAND} --commit \
                     origin/main"
                ),
            )
            .detail(ran.detail),
        )
    }

    fn publish(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        if self.dry {
            self.finish("would run: pub publish aterm --repo-path <this clone>");
            return Ok(());
        }
        self.provision(true)?;
        self.still_possible(&c)?;
        let ran = match self.b.publish(&p) {
            Ok(ran) => ran,
            Err(r) => return self.refuse(r),
        };
        self.refresh();
        let r = short(&c.commit).to_string();
        if !self.facts.engine.has {
            return self.refuse(
                Refusal::new(
                    format!(
                        "pub publish did not record {r} in the engine's ledger (exit {})",
                        ran.code
                    ),
                    format!("read its log; pub publish is resumable: {RELEASE_COMMAND}"),
                )
                .detail(ran.detail),
            );
        }
        if self.facts.engine.newest.as_deref() != Some(c.commit.as_str()) {
            let newest = self.facts.engine.newest.clone().unwrap_or_default();
            return self.refuse(Refusal::new(
                format!(
                    "the engine's newest aterm row is {}, not {r} — the cut releases the newest",
                    short(&newest)
                ),
                "read the engine's mappings.json",
            ));
        }
        let tail = if ran.code == 0 {
            String::new()
        } else {
            format!(
                " (pub publish exited {} after recording it: {})",
                ran.code,
                ran.detail.last().cloned().unwrap_or_default()
            )
        };
        let rev = self.facts.engine.rev.clone().unwrap_or_default();
        self.finish(&format!(
            "published {r} -> alabsystems {}{tail}",
            short(&rev)
        ));
        Ok(())
    }

    // -- step 12: the cut -------------------------------------------------------

    fn cut_args(&self, mode: &CutMode) -> Vec<String> {
        let mut args = vec![
            "cut".to_string(),
            "--release-credentials".to_string(),
            self.creds.display().to_string(),
        ];
        match mode {
            CutMode::Resume(_) => args.push("--resume".to_string()),
            _ => args.extend(self.linux.cut_args()),
        }
        args
    }

    fn cut(&mut self, mode: &CutMode) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        let args = self.cut_args(mode);
        if self.dry {
            self.finish(&format!(
                "would run: targo --unverified ship {}",
                args.join(" ")
            ));
            return Ok(());
        }
        self.provision(true)?;
        if matches!(mode, CutMode::Fresh) {
            // the engine as the cut will read it: R must be its newest aterm row
            if let Err(r) = self.b.sync() {
                return self.refuse(r);
            }
            self.refresh();
            let r = short(&c.commit).to_string();
            if !self.facts.engine.has {
                return self.refuse(Refusal::new(
                    format!("the engine's ledger does not record {r} — the cut signs only a published commit's build"),
                    format!("{RELEASE_COMMAND} (step 11 publishes it)"),
                ));
            }
            if let Some(refusal) = self.newest_row_refusal() {
                return self.refuse(refusal);
            }
            self.receipts_stand("the cut refuses pre-claim")?;
            // what changes while a release waits: the disk, the version's tag
            if let Some(refusal) = self.cut_still_possible(&c, false).into_iter().next() {
                return self.refuse(refusal);
            }
        }
        let log = self.b.log_path(Step::Cut);
        self.say(&format!(
            "        log: {}   (targo --unverified ship {})",
            log.display(),
            args.join(" ")
        ));
        let ran = self.b.cut(&args, &p);
        self.refresh();
        if let Some(cut) = self.facts.cut.clone()
            && cut.finished
        {
            self.finish(&format!("cut v{}, build {}", cut.version, cut.build));
            return Ok(());
        }
        let remedy = match &self.facts.cut {
            Some(cut) => format!("{RELEASE_COMMAND} — it resumes build {}", cut.build),
            None => format!("{RELEASE_COMMAND} — it starts the cut again"),
        };
        self.refuse(
            Refusal::new(
                format!("the cut did not finish (ship cut exited {})", ran.code),
                remedy,
            )
            .detail(ran.detail),
        )
    }

    // -- steps 13 and 14 ---------------------------------------------------------

    /// A SITE THIS MACHINE CANNOT SYNC IS A HAND STEP, NEVER A STOP. `publish/
    /// post-promote` exits 3 on a machine with no usable site checkout, before it
    /// touches anything: re-running here could only meet that again, and the release is
    /// cut and well shaped. So it is journaled (`site-deferred` with `after-cut`, in one
    /// save), said with the command for a machine that has the checkout — here, in the
    /// final line and by every later run of this release — and the release goes on to
    /// its check.
    fn after_cut(&mut self) -> Flow {
        let v = self.version();
        match self.b.after_cut(&v) {
            Ok(done) => {
                if done.site_deferred {
                    self.journal_add_all(&[SITE_DEFERRED, "after-cut"])?;
                } else {
                    self.journal_add("after-cut")?;
                }
                let site = self.site_words();
                self.finish(&format!("{}{site}", done.note));
                Ok(())
            }
            Err(r) => self.refuse(r),
        }
    }

    /// `; <the hand step>` when this release's site was deferred, else nothing.
    fn site_words(&self) -> String {
        if self.journal_has(SITE_DEFERRED) {
            format!("; {SITE_BY_HAND}")
        } else {
            String::new()
        }
    }

    fn check(&mut self) -> Flow {
        let v = self.version();
        if self.dry {
            self.finish(&format!(
                "would require v{v} to be the channel's latest release"
            ));
            return self.dry_final();
        }
        let latest = self.b.latest();
        if latest.as_deref() != Ok(&format!("v{v}")) {
            let said = latest.unwrap_or_else(|e| format!("unreadable ({e})"));
            return self.refuse(Refusal::new(
                format!("the channel's latest release is {said}, not v{v}"),
                format!(
                    "read the cut's log, then {RELEASE_COMMAND} (its after-cut step checks the \
                     release's shape again)"
                ),
            ));
        }
        self.journal_add("check")?;
        self.finish(&format!("v{v} is the channel's latest release"));
        if self.locked {
            self.b.unlock();
            self.locked = false;
        }
        let build = self
            .facts
            .cut
            .as_ref()
            .map(|c| format!(" (build {})", c.build))
            .unwrap_or_default();
        let r = self.commit_r();
        let took = self.b.now().saturating_sub(self.t_start);
        let site = self.site_words();
        self.last = format!(
            "RELEASED aterm {v}{build} from {} — v{v} is the channel's latest{site} — {}",
            short(&r),
            fmt_dur(took)
        );
        let last = self.last.clone();
        self.say(&last);
        Ok(())
    }
}

/// Where the cut stands for V.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CutMode {
    Fresh,
    Resume(u64),
    Done(String),
}

/// What the cut would refuse, as ONE refusal: `why`, then each finding with its own fix
/// line (the preflight's block, and the re-check right before the stage and publish).
fn blocked(why: String, found: &[Refusal]) -> Refusal {
    let detail: Vec<String> = found
        .iter()
        .flat_map(|r| {
            std::iter::once(r.why.clone())
                .chain(std::iter::once(format!("  fix: {}", r.remedy)))
                .chain(r.detail.iter().map(|d| format!("  {d}")))
        })
        .collect();
    Refusal::new(
        why,
        format!(
            "fix each (each block's fix line above) — on main for the source's, then \
             {RELEASE_COMMAND} --commit origin/main; on this machine for its own, then \
             {RELEASE_COMMAND}"
        ),
    )
    .detail(detail)
}

fn running_refusal(h: &Holder) -> Refusal {
    if let Some(step) = &h.step {
        // its pid, never its group: a trustd the step started may share that group, and
        // a signalled trustd records DIRTY (release_steps.rs, THE INTERRUPT)
        return Refusal::new(
            format!(
                "the {step} step of a {RELEASE_COMMAND} that was killed is still running in this \
                 clone (pid {}, since {})",
                h.pid, h.started
            ),
            format!(
                "let it end, or stop it (kill -TERM {} — the step's own process, never its \
                 group, which may hold a trustd), then run {RELEASE_COMMAND} again to resume",
                h.pid
            ),
        );
    }
    Refusal::new(
        format!(
            "another {RELEASE_COMMAND} is running in this clone (pid {}, since {})",
            h.pid, h.started
        ),
        format!(
            "leave it (its logs: {}); or stop it — Ctrl-C in its terminal, or kill -TERM {} \
             (it stops its step's child, waits for it, and says INTERRUPTED) — then run \
             {RELEASE_COMMAND} again to resume",
            if h.logs.is_empty() { "?" } else { &h.logs },
            h.pid
        ),
    )
}

/// `SIGINT` · `SIGTERM` · `SIGHUP`, for the INTERRUPTED line.
fn signal_name(sig: i32) -> String {
    match sig {
        1 => "SIGHUP".into(),
        2 => "SIGINT".into(),
        15 => "SIGTERM".into(),
        n => format!("signal {n}"),
    }
}

/// `MAJOR.MINOR.0` → (MAJOR, MINOR).
#[must_use]
pub fn public_version(v: &str) -> Option<(u64, u64)> {
    let mut parts = v.split('.');
    let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || patch != "0" {
        return None;
    }
    let number = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    Some((number(major)?, number(minor)?))
}

/// `a` is a later MAJOR.MINOR than `b`.
fn newer(a: &str, b: &str) -> bool {
    let key = |v: &str| {
        let mut p = v.split('.').map(|x| x.parse::<u64>().unwrap_or(0));
        (p.next().unwrap_or(0), p.next().unwrap_or(0))
    };
    key(a) > key(b)
}

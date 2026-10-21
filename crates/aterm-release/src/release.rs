// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified ship release`: the one way to release aterm, from a clean clone of
//! the dev repository on `main` to a live release, unattended, and resumable by running
//! it again.
//!
//! ```text
//! targo --unverified ship release --dry-run --bump --mac-only  # the plan; nothing changes
//! targo --unverified ship release --bump --mac-only            # the next MINOR, from main's tip
//! targo --unverified ship release --commit <rev> --mac-only    # a named commit on origin/main
//! targo --unverified ship release                               # again: resumes where it stopped
//! ```
//!
//! The steps, in order ([`Step`]):
//!
//! ```text
//!  1 checks           everything knowable now, refused now with its remedy: one run per
//!                     clone, the prover, the machine, the candidate, this machine's
//!                     provisioning (first audit), and what the cut would refuse that
//!                     needs nothing the publish makes (`publish::pre_publish_gates`)
//!  2 sync             the publication engine to its origin/main (appended audit rows
//!                     kept), and the vendor/astream submodule
//!  3 commit           the release commit R: `--bump` commits the next MINOR on main's
//!                     tip and pushes it; else main's tip, or `--commit`
//!  4 stage            R's own pre-claim gates that read only the commit (seconds; the
//!                     journal's `commit-gates`, which the cut reads), then the REAL `pub
//!                     stage aterm --commit R --repo-path <this clone>` — every export
//!                     guard, the public-clone gate with its wasm32 clause, the private
//!                     staging tier
//!  5 build            BUILD ONCE: reserve the release's identity (`ledger::reserve`:
//!                     build n and the release commit C, made from objects, claimed
//!                     nowhere), then `ship build` — both slices at once from the
//!                     snapshot of C into the release cache, the unsigned bundle kept
//!                     by what it was built as (`artifact.rs`) — and beside it the
//!                     release's OWN handoff fixtures, written by the in-tree generator
//!                     under the reserved identity into the release cache
//!                     (`fixtures::stage`): a generator that refuses, refuses here
//!  6 verify           `verify release --app <that bundle>` in the clone: every check
//!                     a release requires, MEASURE of the bytes that ship among them;
//!                     on a pass, the record that R's tree passed on those bytes
//!  7 ready            READY: that record stands for R, or nothing
//!                     goes public (`--prepare` stops here)
//!  8 publish          this machine's provisioning (audit 2: a gap only the owner closes
//!                     is waited on here), what the cut would refuse, the engine that
//!                     staged R (else the stage runs again, by the engine as it is now),
//!                     then pub publish aterm --repo-path <this clone>
//!  9 cut              ship cut: claim the reservation, then sign, notarize and
//!                     package a byte copy of the VERIFIED bundle (a claimed cut:
//!                     --resume; the cut never builds, and reads what depends only on
//!                     R's tree from this release instead of judging it again)
//! 10 after-cut        the published release's shape, from what GitHub serves, then
//!                     alab.systems brought to it, then the staged handoff fixtures
//!                     landed on main with their PINNED_DESKS rows and shipped-state
//!                     answers (`fixtures::land`, a compare-and-swap; nothing to do
//!                     when main holds them)
//! 11 check            the channel's `latest` is vX.Y.0
//! ```
//!
//! Then one final line: `RELEASED …`, or `REFUSED at <step>: <why> — <what is public>.
//! <remedy>`. Each step prints its start, end and duration, and the plan an ETA from this
//! machine's measured step times.
//!
//! RESUMABLE BY FACTS. Every step asks whether its work is ALREADY DONE for R, never
//! whether it ran: the bundle the
//! journal names COMPLETE in the release cache with its bytes re-proved, the record that
//! `verify release` passed for R's tree on that bundle's executable, a promotable attestation
//! of R in `publish/.out`, R in the engine's ledger (`mappings.json`), the cut journal
//! (`dist/cut-state.toml`: finished, or claimed and resumed). The release's own journal
//! (`<git common dir>/aterm-release/release.toml`) fixes the version, R, the prover,
//! the Linux declaration, the reserved identity and the bundle built from it, so a bare
//! re-run resumes and never chases main's moving tip, nor rebuilds what it built: the
//! READY candidate is kept, and only `--commit` replaces it, out loud, while it is
//! unpublished. What depends only on R's tree is judged ONCE, in this journal — R's
//! commit-only gates (`commit-gates`) at step 4, and every check of the `release` set
//! at `verify` — and the cut reads those facts,
//! refusing without them, instead of judging R's tree again.
//!
//! Build once: the bytes `verify release` judges are the bytes the cut signs. `build`
//! reserves their identity without claiming it; the cut claims it and refuses a copy whose
//! executable is not the verified one. A refusal before the claim burns no number, and a
//! reservation the ledger has passed is replaced and rebuilt.
//!
//! `--prepare` stops after `ready` with nothing public, records PREPARED ([`Prepared`])
//! and lets go of this clone's run markers. A bare run on a PREPARED journal first asks the
//! live world again (`Run::reask_prepared`) and refuses a stale prepare with nothing public
//! or claimed; `--prepare` again redoes only what changed. Only a claimed cut is past its
//! prepare.
//!
//! COULD NOT RUN (`verify release` could not run a check, Apple's notary did not answer)
//! is a resumable stop, never a retry or a pass; a FAIL is never re-run.
//!
//! This machine's provisioning ([`crate::provision::audit`]) is audited twice: at the
//! checks, and right before the publish, where it must hold. A gap the operator closes (a
//! missing x86 slice, a stale toolchain) refuses at the checks. A gap only the owner
//! closes (Apple's developer agreement, a notary credential, the channel token) is raised
//! once, at the checks, as a standing ask on the owner's aterm (`aterm ctl notice ask`)
//! while the stage, build and verify run; the release then waits at the publish, READY
//! with nothing public, re-auditing every [`OWNER_POLL_SECS`]. The cut keeps its own
//! credential and roster checks.
//!
//! ONE RUN PER CLONE: `<git common dir>/aterm-release/running` names the live run (its
//! pid and that pid's start time, so a reused pid is no holder); a second run is
//! refused naming it. `running.child` names the step's child while it runs (in its own
//! session and process group), so a run killed outright leaves no step running unseen:
//! while that child lives, the next run is refused naming it. The run re-executes
//! itself as R's own cutter once the clone is at R (`gates::own_cutter`), so every
//! judgment it makes of R — the records, the pre-claim gates — is
//! R's; `exec` keeps the pid and its start time, so the claim is the same run's. Every
//! relative path flag is anchored once, where the release started
//! ([`ReleaseOptions::anchored`]), for the steps and the re-exec, which run from the
//! clone's root.
//!
//! Ctrl-C, a closed terminal or `kill -TERM <pid>` is handed to the running step's group,
//! never to a trustd (`release_steps::interrupt`); the release waits for the step to end
//! and says `INTERRUPTED at <step> — <what is public>. …`, exit 130.
//!
//! Exit: 0 released, prepared, or a dry run a real run would carry through · 1 refused ·
//! 2 bad usage · 3 a resumable stop (COULD NOT RUN) · 130 interrupted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::provision::{AskWords, Audited, Finding};

/// How an operator runs a release: the one spelling every remedy names.
pub const RELEASE_COMMAND: &str = crate::ledger::RELEASE_COMMAND;

/// What `verify` runs in the clone at R after the release's own `targo`, as an agent
/// would run it (every check a release requires), then `--app <the bundle>`, the bytes
/// it judges. The spawn and every printed rerun line are built from it.
pub const VERIFY_ARGS: [&str; 7] = [
    "--unverified",
    "run",
    "-q",
    "-p",
    "aterm-verify",
    "--",
    "release",
];

/// [`VERIFY_ARGS`] as a command an operator pastes, `app` the bundle it judges.
#[must_use]
pub fn verify_command(app: &str) -> String {
    format!("targo {} --app {}", VERIFY_ARGS.join(" "), shell_word(app))
}

/// `word` as one shell word: as it is when nothing in it is special to a shell, else
/// single-quoted.
pub(crate) fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+=:,@%".contains(&b));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// The release's directory in the clone's git common directory: its journal, its
/// run claim — shared by every worktree of the clone.
pub const RELEASE_DIR: &str = "aterm-release";

/// The release journal, in [`RELEASE_DIR`].
pub const JOURNAL_FILE: &str = "release.toml";

/// The live run's claim, beside the journal.
pub const RUNNING_FILE: &str = "running";

/// The running step's children, beside the claim: one note per child,
/// `running.child.<pid>` (its pid, start time and step; a step runs several at once),
/// naming what a run killed outright would leave running. A bare `running.child`, an
/// older cutter's note, is read the same way.
pub const RUNNING_CHILD_FILE: &str = "running.child";

/// The note of the step's child `pid` in `dir` ([`RUNNING_CHILD_FILE`]).
#[must_use]
pub fn child_note(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{RUNNING_CHILD_FILE}.{pid}"))
}

/// Every step child's note in `dir`: `running.child.<pid>`, and a bare `running.child`.
#[must_use]
pub fn child_notes(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut notes: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| is_child_note(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    notes.sort();
    notes
}

/// `running.child`, or `running.child.<pid>`.
fn is_child_note(name: &str) -> bool {
    name == RUNNING_CHILD_FILE
        || name
            .strip_prefix(RUNNING_CHILD_FILE)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// The journal's fact that R's own pre-claim gates that read only the commit passed (the
/// changelog, the predecessor's handoff fixtures, the handoff policy, the locked
/// metadata). Written before anything is staged and read by the cut
/// (`publish::verified_release_bundle`), which refuses without it: R's tree is judged
/// once, never again at the claim.
pub const COMMIT_GATES: &str = "commit-gates";

/// The journal's fact that the release's own handoff fixtures are staged in the release
/// cache for the journal's reservation: written beside the build, dropped with
/// the reservation when a fresh one replaces it.
pub const FIXTURES_STAGED: &str = "fixtures-staged";

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
    Stage,
    Build,
    Verify,
    Ready,
    Publish,
    Cut,
    AfterCut,
    Check,
}

impl Step {
    /// Every step, in the order a release runs them.
    pub const ALL: [Step; 11] = [
        Step::Checks,
        Step::Sync,
        Step::Commit,
        Step::Stage,
        Step::Build,
        Step::Verify,
        Step::Ready,
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
            Step::Stage => "stage",
            Step::Build => "build",
            Step::Verify => "verify",
            Step::Ready => "ready",
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

    /// `n/N name`, as a refusal names the step.
    #[must_use]
    pub fn at(self) -> String {
        format!("{}/{} {}", self.number(), Step::ALL.len(), self.name())
    }

    /// What a step takes before this machine has timed it: the plan's estimates, until
    /// `steps.tsv` holds this machine's own.
    fn default_secs(self) -> u64 {
        match self {
            Step::Sync | Step::Ready => 15,
            Step::Checks | Step::Commit | Step::AfterCut => 60,
            Step::Stage => 330,
            Step::Build => 600,
            Step::Verify => 3_600,
            Step::Publish => 180,
            Step::Cut => 900,
            Step::Check => 5,
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
    /// `--engine <path>`; default `$PUBLICATION_ENGINE`, else `~/publication`.
    pub engine: Option<PathBuf>,
    pub dry_run: bool,
    pub verbose: bool,
    /// `--prepare`: run the heavy half and stop after READY, PREPARED, nothing public; a
    /// bare run then finishes it ([`Prepared`]).
    pub prepare: bool,
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
    /// Anchor `--engine`, `--release-credentials` and `--linux-artifacts` once, where the
    /// release was started (`cwd`): every step and the re-exec as R's own cutter run from
    /// the clone's root, where a relative path would name another place.
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

/// This clone's release: what a re-run resumes with.
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
    /// Facts only this journal holds for R: [`COMMIT_GATES`], [`FIXTURES_STAGED`],
    /// `after-cut`, `site-deferred`, `check`.
    #[serde(default)]
    pub done: Vec<String>,
    /// The owner asks delivered and not yet resolved, by key: carried over when the
    /// candidate is replaced, withdrawn by `--forget`.
    #[serde(default)]
    pub asks: Vec<String>,
    /// R's reserved identity (`ledger::reserve`, at `build`): what the bundle is built as
    /// and the cut claims. Replaced only when origin's ledger has passed it.
    #[serde(default)]
    pub reserved: Option<Reservation>,
    /// The bundle built from that reservation: what `verify release` judges and the cut
    /// signs.
    #[serde(default)]
    pub artifact: Option<Artifact>,
    /// Written only at `--prepare`'s stop (everything heavy done for R, nothing claimed):
    /// what a bare run asks the live world again before it goes on from READY. A
    /// replaced candidate starts without one.
    #[serde(default)]
    pub prepared: Option<Prepared>,
}

/// A PREPARED release, as `--prepare` left it: R staged, its bundle built once and passed
/// by `verify release`, nothing claimed (nothing public, unless a visible half published
/// the source and stopped short of the claim), and the identities the visible half
/// requires again.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepared {
    /// Epoch seconds the prepare stopped.
    pub at: u64,
    /// The prover the prepare leased: its bin dir, physical — the build the visible half
    /// re-leases.
    pub prover: String,
    /// That prover's `trustc -vV` commit-hash. A run whose prover differs in this or its
    /// bin dir refuses before the claim: the prepare's facts are that prover's.
    pub trustc: String,
    /// The build number it reserved (the journal's reservation), built and verified.
    pub build: u64,
    /// SHA-256 of the executable `verify release` judged (the record it passed on).
    pub macho_sha256: String,
    /// The engine commit R's attestation names (the engine that staged); empty only when
    /// R's source was already published and publish/.out no longer names it.
    pub engine: String,
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

/// The bundle built once for a reservation, as the journal keeps it: its store key,
/// where it is, and the digest of the executable `verify release` judges.
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
/// it recorded — else a step's child its run left alive (a note beside it,
/// [`child_notes`]), which holds the clone as surely: a second run would start the same
/// step beside it. `started_of` answers a pid's start time (`None`: no such process).
pub fn live_holder(
    path: &Path,
    started_of: &mut dyn FnMut(u32) -> Option<String>,
) -> Option<Holder> {
    if let Some(holder) = read_holder(path).filter(|h| alive(h, started_of)) {
        return Some(holder);
    }
    live_child(path.parent().unwrap_or(Path::new(".")), started_of)
}

/// The first step child noted in `dir` that is still alive: its note's holder, its step
/// named.
fn live_child(dir: &Path, started_of: &mut dyn FnMut(u32) -> Option<String>) -> Option<Holder> {
    child_notes(dir)
        .into_iter()
        .filter_map(|note| read_holder(&note))
        .find(|child| alive(child, started_of))
        .map(|child| Holder {
            step: Some(child.step.clone().unwrap_or_else(|| "?".into())),
            ..child
        })
}

impl RunClaim {
    /// Claim `path` for this process (`me`: its pid and start time). Another process's
    /// live claim refuses, naming it; this process's own (made before an exec) is
    /// adopted; a stale one (its pid dead, or reused) is renamed aside first.
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
        // a dead run's children that are dead too hold nothing
        for note in child_notes(dir) {
            let _ = std::fs::remove_file(note);
        }
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

/// `<state>/steps.tsv`: `epoch\tversion\tstep\tsecs`, a line per step that ran.
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

/// The release's one prover: the physical store build every child is handed.
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

/// What the after-cut step did: the published release's shape checked as
/// GitHub serves it, and alab.systems brought to it — or not from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterCut {
    /// The step's line.
    pub note: String,
    /// `ship site` found no usable site checkout here (or one it cannot build or deploy
    /// from) and touched nothing: the site is a hand step ([`SITE_BY_HAND`]) and the
    /// release goes on to its check.
    pub site_deferred: bool,
}

/// The hand step a deferred site leaves ([`AfterCut::site_deferred`]), as the step,
/// the final line and every later run of the release say it. No ` — ` in it: the final
/// line's parts are separated by that.
pub const SITE_BY_HAND: &str = "alab.systems was NOT synced: this machine has no usable \
     site checkout (ship site touched nothing), so run \
     `targo --unverified ship site --latest` on a machine with one";

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

/// The publication engine as a PREPARED release's visible half finds it, against the
/// engine that staged R ([`Backend::engine_now`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineNow {
    /// At its origin/main, with the code that staged (its two ledgers aside: the audit
    /// log and `mappings.json`, which every stage and every publish append to).
    Same(String),
    /// At its origin/main, and its code moved since it staged R: the stage runs again,
    /// so the engine that publishes is the engine whose guards judged the export.
    Moved(String),
    /// Not at its origin/main: `pub stage` and `pub publish` refuse such an engine.
    Off(String),
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

/// The facts every skip rule reads, read after the commit step and again after each step
/// that changes one, never re-derived per question.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub cut: Option<CutState>,
    pub engine: EngineRows,
    /// A promotable attestation of R in `publish/.out`.
    pub attested: bool,
    /// The engine commit that attestation names (`engine.commit`): the engine that
    /// staged R. `None` without a promotable attestation of R.
    pub staged_by: Option<String>,
    /// The journal's bundle: COMPLETE in the release cache, its bytes re-proved,
    /// built by the release's prover.
    pub built: Answer,
    /// Whether `verify release` passed for R's tree on that bundle's executable (the
    /// record `verify` writes, [`crate::gates::verified`]).
    pub verified: Answer,
    /// The release's own handoff fixtures on origin/main, as the next cut's gate judges
    /// them (`fixtures::on_commit`): landed by `after-cut`, or by hand.
    pub fixtures: Answer,
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
    /// The lines that say why (a log tail, a check's summary lines).
    pub detail: Vec<String>,
}

/// What the build step's child answered, and the release's own handoff fixtures staged
/// beside it (`None`: not asked for).
#[derive(Debug)]
pub struct Built {
    pub artifact: Result<Artifact, Refusal>,
    pub fixtures: Option<Result<String, Refusal>>,
}

/// Everything the release asks of the world — git, the engine, the records, the
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
    /// The build-steering variables cleared for every child, as the checks line names them.
    fn cleared_env(&mut self) -> Vec<String>;
    /// The machine's step-1 refusals (an explicit credentials path that is no file, the
    /// cut's disk, the engine checkout, this clone: clean by [`crate::clean`], on main)
    /// and the roster id (empty when none is named). gh, the profile and the roster are
    /// [`Backend::audit`]'s, asked right after.
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
    /// The same, asked of origin now (`git ls-remote --tags origin refs/tags/<tag>`):
    /// a rostered peer may have cut the version while the long steps ran.
    fn origin_tag_now(&mut self, tag: &str) -> Result<Option<String>, String>;
    /// The tag exists in this clone.
    fn local_tag(&mut self, tag: &str) -> bool;
    /// `RELEASES.ledger`'s versions at `rev`, oldest first.
    fn ledger_versions(&mut self, rev: &str) -> Vec<String>;
    fn engine_rows(&mut self, commit: Option<&str>) -> EngineRows;
    /// The engine now, against `staged` (the engine commit R's attestation names): at its
    /// origin/main with the code that staged, moved, or off its origin/main. Read-only.
    /// `Err` when it cannot be asked.
    fn engine_now(&mut self, staged: &str) -> Result<EngineNow, String>;
    /// The private staging tier now, against R's attestation in publish/.out: `Ok(None)`
    /// while `<repo>-staging`'s main is the commit the attestation names
    /// (`staging.attested_rev`), `Ok(Some(why))` once it moved (any peer's `pub stage
    /// aterm` moves it, and `pub promote` then dies "staging moved after attestation;
    /// re-stage"). Read-only (`git ls-remote`); `Err` when it cannot be asked.
    fn staging_moved(&mut self) -> Result<Option<String>, String>;
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
    fn own_cutter(&mut self, commit: &str) -> Result<String, crate::cutter::NotOwn>;
    /// Exec R's own cutter with this process's arguments. Returns only on failure.
    fn reexec(&mut self, prover: &Prover) -> Refusal;
    /// The facts for R; `artifact` is the journal's bundle, when it names one.
    fn facts(&mut self, c: &Candidate, prover: &Prover, artifact: Option<&Artifact>) -> Facts;
    /// Free GiB here, and whether the clone's target directory is warm.
    fn disk(&mut self) -> (Option<u64>, bool);
    /// Step 4's fast half, before the stage: R's own pre-claim gates that read only the
    /// commit (the changelog, the predecessor's handoff fixtures and the handoff policy,
    /// each read from R's objects and judged for the release's version, a bump not made
    /// yet on its parent; the locked metadata, from the clone at R) and the engine's
    /// readiness (gitleaks installed, the pre-promote seam). The blocks, each with its
    /// fix. `dry`: a dry run's (it says when this cutter is not R's own yet).
    fn commit_gates(&mut self, c: &Candidate, dry: bool) -> Vec<Refusal>;
    /// The cut's own read-only pre-claim gates that need nothing the publish makes
    /// (`publish::pre_publish_gates`: no process run out of a staged bundle, the release
    /// lease, the channel's write permission, the signature policy, the roster ratchet,
    /// the channel floor), plus R's two commit-only gates that read origin (the
    /// predecessor's handoff fixtures and the handoff policy: the predecessor is the
    /// newest release tagged on origin, which a peer may tag while the long steps run).
    /// Asked at the checks and again right before the publish (or the fresh cut it does
    /// not publish for), so a refusal leaves nothing more public.
    fn cut_gates(&mut self, c: &Candidate, creds: &Path) -> Vec<Refusal>;
    /// Write the release's own handoff fixtures for its reserved identity into the
    /// release cache, with the in-tree generator in the clone, unless they are staged
    /// already (`fixtures::stage`): what was done, or the refusal.
    fn stage_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> Result<String, Refusal>;
    /// Land them on main (staging them first when the cache holds none), unless main
    /// holds them already (`fixtures::land`).
    fn land_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> Result<String, Refusal>;
    fn audit(&mut self, machine_id: &str) -> Vec<Finding>;
    /// Raise the owner ask: `Ok(its reply)` only when the owner's aterm took it.
    fn ask(&mut self, words: &AskWords) -> Result<String, String>;
    /// Resolve it (`notice done <key> ok`).
    fn resolve(&mut self, key: &str) -> Result<String, String>;
    /// Withdraw it (`notice done <key> withdraw`): its release is forgotten.
    fn withdraw(&mut self, key: &str) -> Result<String, String>;
    /// Origin's ledger tail (the newest claimed build), from step 1's fetch.
    fn origin_tail(&mut self) -> Option<u64>;
    /// Reserve R's identity (`ledger::reserve`): build n and the release commit C,
    /// made from objects, claimed nowhere.
    fn reserve(&mut self, c: &Candidate) -> Result<Reservation, Refusal>;
    /// Build the bundle of `reserved` once (`ship build`, `artifact::build`): both
    /// slices at once into the release cache, or the COMPLETE entry already there; with
    /// `fixtures`, the release's own handoff fixtures staged beside it
    /// ([`Backend::stage_fixtures`]).
    fn build(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
        fixtures: bool,
    ) -> Built;
    /// `verify release --app <the bundle>` in the clone at R with the release's prover,
    /// its output streamed; on exit 0 the record that R's tree passed on the bundle's
    /// executable is written ([`crate::gates::record_verified`]). `Err` (could not run,
    /// nothing recorded) when the clone was not at R's tree and clean as the run started,
    /// or no longer was when it passed.
    fn verify(
        &mut self,
        c: &Candidate,
        prover: &Prover,
        artifact: &Artifact,
    ) -> Result<Ran, Refusal>;
    /// `pub stage`, the engine synced before and after it; `Err` when either sync failed
    /// (the audit rows it kept are named). Step 4 runs it, and the publish again when
    /// the engine that staged R moved since.
    fn stage(&mut self, c: &Candidate, prover: &Prover) -> Result<Ran, Refusal>;
    /// `pub publish`, the engine synced before it; `Err` when that sync failed.
    fn publish(&mut self, prover: &Prover) -> Result<Ran, Refusal>;
    /// The published source's signed manifest pair, read from the channel now. A
    /// ledger row alone can survive a publish that failed before signing it.
    fn source_publication(&mut self, c: &Candidate) -> Result<(), Refusal>;
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
    /// landed, and no cut journal holds it.
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
    /// A dry run's refusals, `n/N step: why`.
    would: Vec<String>,
    /// A dry run's owner waits, each gap once, and the step a real run would first wait
    /// at.
    waits: Vec<String>,
    wait_at: Option<Step>,
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
    /// `--prepare`: the steps the visible half runs after the stop, and their estimate.
    later: Vec<Step>,
    later_secs: u64,
    /// Set when the stage runs again: the engine moved since it staged R (found by
    /// [`Run::reask_prepared`], or right before any publish by
    /// [`Run::engine_still_staged`], where a moved staging tier also stages again). Holds
    /// the engine a fresh attestation must not name again, if any, and why.
    restage: Option<(Option<String>, String)>,
    /// Owner asks the checks delivered before this run had a journal (a first run):
    /// journaled with the candidate at step 3, so they are not raised again.
    early_asks: Vec<String>,
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
        wait_at: None,
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
        later: Vec::new(),
        later_secs: 0,
        restage: None,
        early_asks: Vec::new(),
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
            run.withdraw_early_asks();
            let last = run.last.clone();
            run.b.say(&last);
            1
        }
    };
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

        self.begin(Step::Stage)?;
        if let Some(why) = self.claimed_words() {
            self.skip(&why);
        } else {
            // a PREPARED release asks the live world again before anything else
            if let Some(prep) = self.pending_reask() {
                self.reask_prepared(&prep)?;
            }
            self.disk_still_needed()?;
            self.stage()?;
        }

        self.begin(Step::Build)?;
        if let Some(why) = self.skip_build() {
            self.skip(&why);
        } else {
            self.build()?;
        }

        self.begin(Step::Verify)?;
        if let Some(why) = self.skip_verify() {
            self.skip(&why);
        } else {
            self.verify()?;
        }

        self.begin(Step::Ready)?;
        if let Some(why) = self.claimed_words() {
            self.skip(&why);
        } else {
            self.ready()?;
        }

        // `--prepare` stops here: everything heavy done, nothing public
        if self.opts.prepare {
            self.prepare_stop()?;
        }

        self.begin(Step::Publish)?;
        match self.skip_publish() {
            Some(why) if matches!(self.cut_mode(), CutMode::Fresh) => {
                self.audit_before_cut(&why)?;
            }
            Some(why) => self.skip(&why),
            None => self.publish()?,
        }

        self.begin(Step::Cut)?;
        match self.cut_mode() {
            CutMode::Done(why) => self.skip(&why),
            mode => self.cut(&mode)?,
        }

        self.begin(Step::AfterCut)?;
        let shaped = self.journal_has("after-cut");
        if shaped && self.facts.fixtures.yes {
            let v = self.version();
            let site = self.site_words();
            let landed = self.facts.fixtures.words.clone();
            self.skip(&format!(
                "done for v{v} (this release's journal){site}; {landed}"
            ));
        } else if self.dry {
            let v = self.version();
            let mut words = Vec::new();
            if !shaped {
                words.push(format!(
                    "would check v{v}'s shape as GitHub serves it, then bring alab.systems to \
                     it (ship site --latest)"
                ));
            }
            words.push(if self.facts.fixtures.yes {
                self.facts.fixtures.words.clone()
            } else {
                format!(
                    "would land v{v}'s handoff fixtures, staged before the claim, on main: one \
                     commit on its tip adding {}/v{v}/, its PINNED_DESKS rows and its cases in {}, \
                     pushed as a compare-and-swap",
                    crate::gates::HANDOFF_FIXTURE_ROOT,
                    crate::fixtures::SHIPPED_STATE_CORPUS
                )
            });
            self.finish(&words.join("; then "));
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

    /// A signal arrived (the step's child was handed it and has ended): the run ends in
    /// its one line, exit 130. The journal is durable, so the same command resumes.
    fn halt_if_interrupted(&mut self) -> Flow {
        let Some(sig) = self.b.interrupted() else {
            return Ok(());
        };
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
        self.withdraw_early_asks();
        let last = self.last.clone();
        self.say(&last);
        Err(Halt::Said(130))
    }

    /// The owner asks the checks delivered with no journal to note them in, when the run
    /// stops before step 3 journals them: withdrawn and said before the final line, else
    /// the standing ask outlives the run with nothing to resolve or withdraw it
    /// (`--forget` withdraws only what a journal holds). The next run's audit raises it
    /// again if the gap still stands.
    fn withdraw_early_asks(&mut self) {
        for key in std::mem::take(&mut self.early_asks) {
            let line = match self.b.withdraw(&key) {
                Ok(reply) => format!(
                    "        withdrew the owner's ask {key} ({reply}): no journal holds it yet, \
                     and the next run's audit asks again"
                ),
                Err(why) => format!(
                    "        could not withdraw the owner's ask {key} ({why}): no journal holds \
                     it, so it stands until the owner closes it"
                ),
            };
            self.say(&line);
        }
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

    /// A real run says the refusal in the final line and stops; a dry run notes what a
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
        self.withdraw_early_asks();
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
                self.wait_at.unwrap_or(Step::Publish).at(),
                self.waits.join("; ")
            )
        };
        if self.would.is_empty() {
            self.last = if self.opts.prepare {
                format!(
                    "DRY RUN aterm {v} from {}: a real --prepare would go ahead and stop PREPARED \
                     after {}, about {} of work{waits}; then {RELEASE_COMMAND} would finish it, \
                     about {} — nothing was changed",
                    short(&r),
                    Step::Ready.at(),
                    fmt_dur(self.plan_secs),
                    fmt_dur(self.later_secs)
                )
            } else {
                format!(
                    "DRY RUN aterm {v} from {}: a real run would go ahead, about {} of \
                     work{waits} — nothing was changed",
                    short(&r),
                    fmt_dur(self.plan_secs)
                )
            };
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

    // -- prepare, then release --------------------------------------------------

    /// The journal's PREPARED record of R, if it holds one and R is not released yet (a
    /// finished release's record is history).
    fn prepared(&self) -> Option<Prepared> {
        let r = self.commit_r();
        self.journal
            .as_ref()
            .filter(|j| j.commit == r && !j.finished())
            .and_then(|j| j.prepared.clone())
    }

    /// The PREPARED record the visible half re-asks: a run without `--prepare`, on a
    /// journal that prepared R, before the claim (past it, the cut's journal resumes).
    fn pending_reask(&mut self) -> Option<Prepared> {
        if self.opts.prepare || self.claimed() {
            return None;
        }
        self.prepared()
    }

    /// `p` is the very build the prepare held: its bin dir and its `trustc` commit.
    fn same_prover(p: &Prover, prep: &Prepared) -> bool {
        p.bin.display().to_string() == prep.prover && p.commit == prep.trustc
    }

    /// This clone's run claim, let go: this run holds nothing now.
    fn let_go(&mut self) {
        self.claim = None;
    }

    /// A PREPARED release stages again ([`Run::reask_prepared`]): `old` is the engine R's
    /// attestation names, which a fresh stage must not name again; planned, with its ETA.
    fn stage_again(&mut self, old: Option<String>, why: String) {
        if !self.planned.contains(&Step::Stage) {
            let secs = self.est_secs(Step::Stage);
            self.planned.push(Step::Stage);
            self.planned.sort();
            self.plan_secs += secs;
        }
        self.restage = Some((old, why));
    }

    /// On a journal `--prepare` left PREPARED, the visible half asks the live world again
    /// before anything is claimed. Step 1 already re-leased the prover, required its
    /// identity and audited this machine's provisioning (an owner gap is waited on at the
    /// publish); here: the version's tag absent on origin, origin's ledger tail below the
    /// reserved build, the prepare's facts (built once, verified on those bytes), and the
    /// engine R's attestation names at its origin/main with the code that staged R, else
    /// the stage runs again (asked once more before the publish,
    /// [`Run::engine_still_staged`]). A mismatch refuses (the prepare is stale); what
    /// cannot be asked is COULD NOT RUN.
    fn reask_prepared(&mut self, prep: &Prepared) -> Flow {
        let (v, r) = (self.version(), self.commit_r());
        let when = self.b.hms(prep.at);
        // step 1's verdict on the prover: a real run that got here holds the same build;
        // a dry run noted a mismatch there and planned on
        let prover = if self
            .prover
            .as_ref()
            .is_some_and(|p| Self::same_prover(p, prep))
        {
            "its prover, re-leased at step 1, is the same build"
        } else {
            "its prover is NOT the prepared build (step 1's refusal, above)"
        };
        let mut words = vec![format!(
            "v{v} was PREPARED at {when}; asked again: {prover}"
        )];
        // what no longer holds (a refusal each, with its fix), and what could not be asked
        let mut stale: Vec<Refusal> = Vec::new();
        let mut unasked: Vec<String> = Vec::new();
        let again = format!(
            "prepare it again: {RELEASE_COMMAND} --prepare (it makes again only what this \
             undid), then {RELEASE_COMMAND}"
        );

        // the version is not cut
        let tag = format!("v{v}");
        match self.b.origin_tag_now(&tag) {
            Ok(None) => words.push(format!("{tag} is not on origin")),
            Ok(Some(at)) => {
                stale.push(Refusal::new(
                    format!(
                        "{tag} is on origin now (at {}): it was cut since the prepare",
                        short(&at)
                    ),
                    format!(
                        "release the next MINOR: {RELEASE_COMMAND} --forget, then \
                         {RELEASE_COMMAND} --bump"
                    ),
                ));
            }
            Err(why) => unasked.push(format!("whether {tag} is on origin: {why}")),
        }

        // the reserved build is still claimable
        let reserved = self.reservation().map(|res| res.build);
        match (reserved, self.b.origin_tail()) {
            (_, None) => {
                unasked.push("origin's ledger tail (origin/main:RELEASES.ledger)".to_string());
            }
            (Some(n), Some(tail)) if n == prep.build && tail < n => {
                words.push(format!(
                    "the ledger tail {tail} is below the reserved build {n}"
                ));
            }
            (Some(n), Some(tail)) if n == prep.build => {
                stale.push(Refusal::new(
                    format!(
                        "origin's ledger tail {tail} reached the reserved build {n}: another \
                         release claimed past it (nothing was claimed here, nothing burned)"
                    ),
                    format!("{again} (it reserves anew, then builds and verifies again)"),
                ));
            }
            (held, Some(_)) => {
                let held = held.map_or_else(|| "none".to_string(), |n| format!("build {n}"));
                stale.push(Refusal::new(
                    format!(
                        "the journal's reservation ({held}) is not the prepared build {}",
                        prep.build
                    ),
                    again.clone(),
                ));
            }
        }

        // the prepare's own facts still stand, for the bytes it verified
        let facts = [
            ("the bundle built once", self.facts.built.clone()),
            ("the `verify release` record", self.facts.verified.clone()),
        ];
        for (name, answer) in facts {
            if answer.unanswered {
                unasked.push(format!("{name}: {}", answer.words));
            } else if !answer.yes {
                let why = format!("{name} no longer stands: {}", answer.words);
                stale.push(Refusal::new(why, again.clone()));
            }
        }
        let verified = self.artifact().map(|a| a.macho_sha256);
        if verified.as_deref() == Some(prep.macho_sha256.as_str()) {
            words.push(format!(
                "built once and verified: executable {}",
                short(&prep.macho_sha256)
            ));
        } else {
            let why = format!(
                "the journal's bundle is not the one the prepare verified (executable {})",
                short(&prep.macho_sha256)
            );
            stale.push(Refusal::new(why, again.clone()));
        }

        // the engine that publishes is the engine that staged: the one R's attestation
        // names, never one this run assumes
        let staged = self.facts.staged_by.clone();
        if self.facts.engine.has {
            words.push("its source is published: the stage is behind it".to_string());
        } else if !self.facts.attested {
            words.push("publish/.out holds no attestation of R: the stage runs again".to_string());
        } else if let Some(staged) = staged {
            if self.dry {
                words.push(format!(
                    "would ask that the engine is at its origin/main with the code that staged \
                     {} (else the stage runs again)",
                    short(&staged)
                ));
            } else {
                match self.b.engine_now(&staged) {
                    Ok(EngineNow::Same(note)) => words.push(note),
                    Ok(EngineNow::Moved(why)) => {
                        words.push(format!("{why}: the stage runs again"));
                        self.stage_again(Some(staged), why);
                    }
                    Ok(EngineNow::Off(why)) => {
                        let fix = format!(
                            "{RELEASE_COMMAND} again: its sync step brings the engine to its \
                             origin/main"
                        );
                        stale.push(Refusal::new(why, fix));
                    }
                    Err(why) => {
                        unasked.push(format!(
                            "whether the engine is the one that staged {}: {why}",
                            short(&r)
                        ));
                    }
                }
            }
        } else {
            // `pub publish` refuses an attestation that names no engine commit
            let why = "R's attestation names no engine commit, which pub publish refuses";
            words.push(format!("{why}: the stage runs again"));
            self.stage_again(None, why.to_string());
        }
        let line = words.join("; ");
        self.say(&format!("        {line}"));

        // a decided mismatch refuses (exit 1) before what could not be asked (exit 3)
        if stale.len() == 1 {
            let one = stale.remove(0);
            let why = format!("the PREPARED v{v} is stale: {}", one.why);
            self.refuse(Refusal::new(why, one.remedy))?;
        } else if !stale.is_empty() {
            let detail: Vec<String> = stale
                .iter()
                .flat_map(|s| [s.why.clone(), format!("  fix: {}", s.remedy)])
                .collect();
            let why = format!("the PREPARED v{v} is stale ({} finding(s))", stale.len());
            self.refuse(Refusal::new(why, "each finding's fix line above").detail(detail))?;
        }
        if !unasked.is_empty() {
            self.refuse(
                Refusal::new(
                    format!(
                        "could not ask again for the PREPARED v{v}: {}",
                        unasked.join("; ")
                    ),
                    format!("run {RELEASE_COMMAND} again once it answers (the prepare is kept)"),
                )
                .could_not_run(),
            )?;
        }
        Ok(())
    }

    /// The stop of `--prepare`, after READY: everything heavy done for R and nothing
    /// claimed. Only when every fact it names stands now does the journal record PREPARED
    /// (the leased prover, the reserved build, the verified executable, the engine that
    /// staged); then this clone's run markers are let go (a live step child refuses) and
    /// the line names the finishing command, exit 0. A dry run says where it would stop
    /// and plans the visible half.
    ///
    /// Only a claimed cut is past its prepare (the cut's journal resumes it). A source
    /// published by a visible half that stopped short of the claim is prepared anew, so
    /// the next bare run re-asks against this run's prover, reservation and bytes.
    fn prepare_stop(&mut self) -> Flow {
        let (v, r) = (self.version(), self.commit_r());
        let later: Vec<&str> = self.later.iter().map(|s| s.name()).collect();
        let later = if later.is_empty() {
            "nothing".to_string()
        } else {
            later.join(" ")
        };
        let asks = "the prover re-leased and required to be the same build, this machine's \
                    provisioning (a gap only the owner closes waited on at the publish), the \
                    version's tag absent on origin, origin's ledger tail below the reserved \
                    build, the prepare's facts, and the engine at its origin/main with the \
                    code that staged (else the stage runs again)";
        if self.dry {
            self.say(&format!(
                "release: --prepare would stop here, PREPARED — the journal records the prover, \
                 the reserved build, the verified executable and the engine that staged; this \
                 clone's run is let go. Then {RELEASE_COMMAND} asks again: {asks}; and runs: \
                 {later} (about {})",
                fmt_dur(self.later_secs)
            ));
            return Ok(());
        }
        if let Some(why) = self.claimed_words() {
            self.let_go();
            self.last = format!(
                "release: aterm {v} from {} is past its prepare ({why}; its cut's own journal \
                 resumes it) — nothing was recorded. Finish it: {RELEASE_COMMAND}",
                short(&r)
            );
            let last = self.last.clone();
            self.say(&last);
            return Err(Halt::Said(0));
        }
        // a source already public needs no stage: its engine is asked of nobody again
        let published = self.facts.engine.has;

        // what the record names must stand now
        let mut missing: Vec<String> = Vec::new();
        for (name, answer) in [
            ("built once", &self.facts.built),
            ("verified", &self.facts.verified),
        ] {
            if !answer.yes {
                missing.push(format!("{name}: {}", answer.words));
            }
        }
        let attested = self.facts.attested;
        let staged_by = self.facts.staged_by.clone().filter(|_| attested);
        if !published && staged_by.is_none() {
            missing.push(
                "staged: publish/.out holds no promotable attestation of R (one naming the \
                 engine that staged it)"
                    .to_string(),
            );
        }
        let engine = staged_by.or_else(|| published.then(String::new));
        let record = (
            self.prover.clone(),
            self.reservation(),
            self.artifact(),
            engine,
        );
        let (p, reserved, artifact, engine) = match record {
            (Some(p), Some(reserved), Some(artifact), Some(engine)) if missing.is_empty() => {
                (p, reserved, artifact, engine)
            }
            _ => {
                if missing.is_empty() {
                    missing.push("the journal names no prover, reservation or bundle".to_string());
                }
                return self.refuse(
                    Refusal::new(
                        format!("nothing to record as PREPARED: {}", missing.join("; ")),
                        format!("{RELEASE_COMMAND} --prepare again (the candidate is kept)"),
                    )
                    .could_not_run(),
                );
            }
        };

        // a step's child still alive holds the clone: the prepare holds nothing running
        let release_dir = self.b.release_dir();
        let left = {
            let mut started_of = |pid| self.b.started_of(pid);
            live_child(&release_dir, &mut started_of)
        };
        if let Some(h) = left {
            return self.refuse(
                Refusal::new(
                    format!(
                        "the {} step's child is still running (pid {}, since {}), so nothing was \
                         recorded as PREPARED",
                        h.step.as_deref().unwrap_or("?"),
                        h.pid,
                        h.started
                    ),
                    format!("let it end, then {RELEASE_COMMAND} --prepare again"),
                )
                .could_not_run(),
            );
        }
        for note in child_notes(&release_dir) {
            let _ = std::fs::remove_file(note);
        }

        let now = self.b.now();
        if let Some(j) = self.journal.as_mut() {
            j.prepared = Some(Prepared {
                at: now,
                prover: p.bin.display().to_string(),
                trustc: p.commit.clone(),
                build: reserved.build,
                macho_sha256: artifact.macho_sha256.clone(),
                engine: engine.clone(),
            });
        }
        self.save_journal()?;
        self.let_go();

        let public = if published {
            self.public.clone()
        } else {
            "nothing public, nothing claimed".to_string()
        };
        self.say(&format!(
            "release: PREPARED in {} — {public}; this clone's run let go",
            self.journal_path.display()
        ));
        self.say(&format!(
            "        prover   {} (trustc {}; {}) — the next run re-leases it and goes on only \
             with this build",
            p.bin.display(),
            short(&p.commit),
            p.source
        ));
        self.say(&format!(
            "        bundle   {} — executable sha256 {}",
            artifact.app.display(),
            artifact.macho_sha256
        ));
        self.say(&format!(
            "        build    {} reserved as {}, claimed nowhere",
            reserved.build,
            short(&reserved.commit)
        ));
        let staged = match (published, engine.is_empty()) {
            (false, _) => format!("by the engine at {}", short(&engine)),
            (true, false) => format!(
                "by the engine at {}, and published: no engine is asked of it again",
                short(&engine)
            ),
            (true, true) => "and published; publish/.out names no engine now".to_string(),
        };
        self.say(&format!("        staged   {staged}"));
        self.say(&format!(
            "        then     {RELEASE_COMMAND} asks again: {asks}; and runs: {later} (about {})",
            fmt_dur(self.later_secs)
        ));
        let took = now.saturating_sub(self.t_start);
        self.last = format!(
            "PREPARED aterm {v} (build {}) from {} — built once, verified and staged; \
             {} — {}. Finish it: {RELEASE_COMMAND}",
            reserved.build,
            short(&r),
            self.public,
            fmt_dur(took)
        );
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
        // the profile and the roster are the provisioning audit's, just below)
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
        // audit 1, before anything changes and once what is public is known (a refusal
        // says it): an operator gap refuses now; an owner gap is asked now and waited on
        // at the publish (audit 2). R's own cutter asks it again after the exec, since the
        // cutter before it may be older and have asked neither this nor what follows; an
        // ask the journal holds is not raised twice.
        notes.push(self.provision(false)?);
        // what the cut would refuse that needs nothing the publish makes (the disk, the
        // version's tag on origin now, the cut's read-only pre-claim gates), asked now and
        // again right before the publish (or the cut it does not publish for); a claimed
        // cut is past every pre-claim gate
        if !self.claimed()
            && let Some(c) = self.cand.clone()
        {
            self.still_possible(&c, None)?;
        }

        // one prover across the prepare: the visible half re-leased the journal's prover
        // (above) and goes on only with the very build the prepare held
        if let Some(prep) = self.pending_reask()
            && let Some(p) = self.prover.clone()
        {
            if Self::same_prover(&p, &prep) {
                notes.push(format!(
                    "the PREPARED prover re-leased: {} (trustc {})",
                    prep.prover,
                    short(&prep.trustc)
                ));
            } else {
                let again = self.est_or_default(Step::Build) + self.est_or_default(Step::Verify);
                let v = self.version();
                self.refuse(Refusal::new(
                    format!(
                        "v{v} was PREPARED with the prover {} (trustc {}), and this run's is {} \
                         (trustc {}; {}) — the bundle and its `verify release` pass are the \
                         prepared prover's",
                        prep.prover,
                        short(&prep.trustc),
                        p.bin.display(),
                        short(&p.commit),
                        p.source
                    ),
                    format!(
                        "when TRUST_STAGE2_BIN names another prover, unset it and run \
                         {RELEASE_COMMAND} again; else prepare again with this one: \
                         {RELEASE_COMMAND} --prepare (the build and `verify \
                         release` again, about {}), then {RELEASE_COMMAND}",
                        fmt_dur(again)
                    ),
                ))?;
            }
        } else if self.opts.prepare
            && let Some(prep) = self.prepared()
            && let Some(p) = self.prover.clone()
            && !Self::same_prover(&p, &prep)
        {
            notes.push(format!(
                "DECISION: --prepare proves v{} again with the prover {} (trustc {}) — it was \
                 PREPARED with {} (trustc {}), whose facts this one makes anew",
                self.version(),
                p.bin.display(),
                short(&p.commit),
                prep.prover,
                short(&prep.trustc)
            ));
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

        // an unfinished cut of another version holds this clone's cut journal
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
        // The re-exec keeps R: this process journaled R (step 3) before re-running as R's
        // cutter (same pid, so its claim is adopted), and its arguments are the
        // operator's: `--commit origin/main` resolved again would name main's tip now.
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
                // the READY candidate is replaced only out loud, with what it costs
                let verify = self.est_or_default(Step::Verify);
                self.say(&format!(
                    "DECISION: --commit {} replaces the unpublished candidate v{} from {} — \
                     {} costs a `verify release` run (about {}) unless its record already \
                     stands",
                    short(&r),
                    j.version,
                    short(&j.commit),
                    short(&r),
                    fmt_dur(verify)
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
            // this clone's finished release, main not bumped since: every step skips
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
        // A bump is refused before its push by what would refuse its commit: the checks
        // below read R as its parent (main's tip, which the bump only extends) and V as
        // the version it will declare; only the engine's rows wait for the commit.

        // R contains the release it succeeds: that release's claim commit (what its tag
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
        // cut would release that commit
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
        // R is published, but a newer row followed it: the cut would release that one
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

    /// The cut builds the engine's newest aterm row, whatever R is, so a published,
    /// unclaimed R must still be that row: asked at step 1, after the sync, and on the
    /// synced engine just before a fresh cut.
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
                prepared: None,
            });
        } else if !self.claimed()
            && let Some(j) = self.journal.as_mut()
        {
            // before a claim, a new prover is this release's prover from now on (`build`
            // rebuilds: `artifact::key` holds the prover's commit); after the claim the
            // bundle is built and verified, and no prover changes its bytes
            j.prover = prover.bin.display().to_string();
            j.trustc = prover.commit.clone();
            if self.linux.declared() {
                j.linux = self.linux.clone();
            }
        }
        // the asks the checks delivered before there was a journal to note them in
        if !self.early_asks.is_empty()
            && let Some(j) = self.journal.as_mut()
        {
            for key in std::mem::take(&mut self.early_asks) {
                if !j.asks.contains(&key) {
                    j.asks.push(key);
                }
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

    /// Every judgment of R is R's own cutter's: once the clone is at R, a binary whose
    /// cutter sources differ re-execs as R's cutter (same arguments, same pid). A re-exec
    /// that is still not R's cutter refuses instead of looping.
    fn own_cutter(&mut self) -> Flow {
        if self.dry {
            return Ok(());
        }
        let r = self.commit_r();
        match self.b.own_cutter(&r) {
            Ok(_) => Ok(()),
            Err(not) => {
                if self.claim.as_ref().is_some_and(|c| c.adopted) {
                    return self.refuse(Refusal::new(
                        format!(
                            "this cutter was rebuilt at {} and is still not its own ({})",
                            short(&r),
                            not.why
                        ),
                        format!("{} (nothing was claimed)", not.fix),
                    ));
                }
                // an exec forgets the signals this process caught: one caught in step 3
                // (the bump's push, the move to R) ends the run here, never in R's cutter
                self.halt_if_interrupted()?;
                self.say(&format!(
                    "release: {} — re-running as {}'s own cutter ({RELEASE_COMMAND}, same \
                     arguments)",
                    not.why,
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
                built: pending.clone(),
                verified: pending,
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
        // A reservation origin's ledger has reached is never signed: its bytes carry its
        // number, so they are neither built nor verified for this release; `build`
        // reserves anew.
        if facts.cut.is_none()
            && let Some(spent) = self.spent()
        {
            let stale = Answer::no(format!(
                "{}; step 5 reserves anew and builds again",
                spent.words()
            ));
            facts.built = stale.clone();
            facts.verified = stale;
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

    /// The build step is done when its bundle is built and the release's own handoff
    /// fixtures are staged beside it for the same reservation (or are on main already).
    fn skip_build(&self) -> Option<String> {
        self.claimed_words().or_else(|| {
            let fixtures = if self.facts.fixtures.yes {
                self.facts.fixtures.words.clone()
            } else if self.journal_has(FIXTURES_STAGED) {
                "its handoff fixtures staged beside it (this release's journal)".to_string()
            } else {
                return None;
            };
            self.facts
                .built
                .yes
                .then(|| format!("{}; {fixtures}", self.facts.built.words))
        })
    }

    fn skip_verify(&self) -> Option<String> {
        self.claimed_words().or_else(|| {
            self.facts
                .verified
                .yes
                .then(|| self.facts.verified.words.clone())
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
    /// bytes can never ship, and why: another release claimed past it, or this clone's
    /// claim landed (origin/main holds its release commit) but no cut journal holds it.
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
        // a PREPARED release whose engine moved stages again ([`Run::reask_prepared`])
        if self.restage.is_some() {
            return None;
        }
        self.facts.attested.then(|| {
            format!(
                "{} is staged (publish/.out holds its promotable attestation)",
                short(&r)
            )
        })
    }

    /// Step 4 is done when R is staged (or published) and its preflight passed for it
    /// (this release's journal), or the cut is claimed.
    fn stage_done(&self) -> Option<String> {
        if let Some(why) = self.claimed_words() {
            return Some(why);
        }
        let staged = self.skip_stage()?;
        self.journal_has(COMMIT_GATES)
            .then(|| format!("{staged}; its preflight passed for it (this release's journal)"))
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

    fn plan_print(&mut self) {
        let tsv = self.b.times();
        self.est = estimates(&tsv);
        let measured_here = !tsv.trim().is_empty();
        let mut run = Vec::new();
        let mut done = Vec::new();
        for step in &Step::ALL[3..] {
            let skipped = match step {
                Step::Stage => self.stage_done().is_some(),
                Step::Ready => self.claimed_words().is_some(),
                Step::Build => self.skip_build().is_some(),
                Step::Verify => self.skip_verify().is_some(),
                Step::Publish => self.skip_publish().is_some(),
                Step::Cut => matches!(self.cut_mode(), CutMode::Done(_)),
                Step::AfterCut => self.journal_has("after-cut") && self.facts.fixtures.yes,
                _ => false,
            };
            if skipped {
                done.push(step.name());
            } else {
                run.push(*step);
            }
        }
        // `--prepare` runs up to its stop after READY; the visible half asks READY again,
        // then runs the publish onward
        if self.opts.prepare {
            self.later = run.iter().copied().filter(|s| *s >= Step::Ready).collect();
            run.retain(|s| *s <= Step::Ready);
        }
        self.later_secs = self.later.iter().map(|s| self.est_secs(*s)).sum();
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
        if self.opts.prepare {
            let later: Vec<&str> = self.later.iter().map(|s| s.name()).collect();
            self.say(&format!(
                "release: --prepare stops after {}, PREPARED with nothing public; then \
                 {RELEASE_COMMAND} asks the live world again and runs: {} (about {})",
                Step::Ready.at(),
                if later.is_empty() {
                    "nothing".to_string()
                } else {
                    later.join(" ")
                },
                fmt_dur(self.later_secs)
            ));
        }
        let now = self.b.now();
        let at = self.b.hms(now + self.plan_secs);
        self.say(&format!(
            "release: ETA {} — {} of work, by {} (a wait for the owner is not in it)",
            &at[..at.len().min(5)],
            fmt_dur(self.plan_secs),
            if measured_here {
                "this machine's measured step times"
            } else {
                "the plan's measured defaults"
            }
        ));
    }

    // -- step 4: the preflight, then the real stage ---------------------------

    /// The disk the steps still to come need, asked before the long steps: the build and
    /// `verify release` (39 GiB from an empty target directory, 30 on a warm one), then
    /// the cut's 10.
    fn disk_still_needed(&mut self) -> Flow {
        if self.facts.verified.yes {
            return Ok(());
        }
        let (free, warm) = self.b.disk();
        let Some(free) = free else {
            return Ok(());
        };
        let (need, what) = if warm {
            (30, "30 (its target directory is warm)")
        } else {
            (39, "39 (cold)")
        };
        if free < need {
            return self.refuse(Refusal::new(
                format!(
                    "only {free} GiB free — the build and `verify release` still to come need \
                     {what}, then the cut {CUT_GIB}"
                ),
                "free space, then re-run",
            ));
        }
        Ok(())
    }

    /// STEP 4, THE REAL STAGE FIRST. The preflight — R's own pre-claim gates that read
    /// only the commit and the engine's readiness, seconds — refuses before anything
    /// else, and its pass is journaled for R ([`COMMIT_GATES`]: the fact the cut reads
    /// instead of judging R's tree again); a judge of the records that could not answer
    /// stops here too. Then the real `pub stage` (every export guard, the public-clone
    /// gate with its wasm32 clause, the private staging tier), unless R is staged.
    fn stage(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        // asked on every unclaimed run, step 4 done or not: a resumed run must not run
        // `verify release` for an hour before a judge that cannot read its record says so
        self.verification_answer()?;
        if let Some(why) = self.stage_done() {
            self.skip(&why);
            return Ok(());
        }
        let mut words = vec![self.commit_gates(&c)?];
        match self.skip_stage() {
            Some(why) => words.push(why),
            None if self.dry => words.push(format!(
                "would run pub stage aterm --commit {} --repo-path <this clone>",
                if c.bump_from.is_some() {
                    "<the bump commit>"
                } else {
                    c.commit.as_str()
                }
            )),
            None => {
                let log = self.b.log_path(Step::Stage);
                self.say(&format!("        log: {}", log.display()));
                let ran = self.b.stage(&c, &p);
                self.refresh();
                match self.stage_verdict(&c, ran) {
                    Ok(note) => words.push(note),
                    Err(refusal) => return self.refuse(refusal),
                }
            }
        }
        self.finish(&words.join("; "));
        Ok(())
    }

    /// R's own pre-claim gates that read only the commit, and the engine's readiness:
    /// once per R. Their pass is journaled as [`COMMIT_GATES`], which the cut reads
    /// (`publish::verified_release_bundle`) and refuses without.
    fn commit_gates(&mut self, c: &Candidate) -> Result<String, Halt> {
        if self.journal_has(COMMIT_GATES) {
            return Ok(format!(
                "the preflight passed for {} (this release's journal)",
                short(&c.commit)
            ));
        }
        let found = self.b.commit_gates(c, self.dry);
        if found.is_empty() {
            self.journal_add(COMMIT_GATES)?;
            return Ok(String::from(
                "the preflight passes: R's commit-only gates (the changelog, the predecessor's \
                 handoff fixtures, the handoff policy, the locked metadata) and the engine's \
                 readiness",
            ));
        }
        let n = found.len();
        self.refuse(blocked(
            format!("the preflight BLOCKED ({n} finding(s))"),
            &found,
        ))?;
        Ok(format!("the preflight BLOCKED ({n} finding(s), above)"))
    }

    /// Stop before the long steps if their verification record cannot be read.
    fn verification_answer(&mut self) -> Flow {
        if !self.facts.verified.unanswered {
            return Ok(());
        }
        self.refuse(
            Refusal::new(
                format!(
                    "the cutter's own judge could not answer: {}",
                    self.facts.verified.words
                ),
                format!("run {RELEASE_COMMAND} again once it answers (the candidate is kept)"),
            )
            .could_not_run(),
        )
    }

    /// What `pub stage` ending with `ran` means for R, the facts read again after it: the
    /// step's words, or its refusal. A stage again (the engine that staged R moved) counts
    /// only when it passed and left an attestation by the engine as it is now; the old one
    /// in publish/.out is stale. One for a moved staging tier is by the same engine, so it
    /// names no old engine (`restage.0` is `None`), and its caller asks the staging tier
    /// again after it.
    fn stage_verdict(
        &mut self,
        c: &Candidate,
        ran: Result<Ran, Refusal>,
    ) -> Result<String, Refusal> {
        let r = short(&c.commit).to_string();
        let ran = ran?;
        if let Some((old, why)) = self.restage.clone() {
            let fresh = self.facts.attested
                && self.facts.staged_by.is_some()
                && self.facts.staged_by != old;
            if ran.code == 0 && fresh {
                self.restage = None;
                return Ok(format!(
                    "staged {r} again ({why}; a promotable attestation in publish/.out)"
                ));
            }
            return Err(Refusal::new(
                format!(
                    "pub stage did not stage {r} again by the engine as it is now (exit {})",
                    ran.code
                ),
                format!("read its log, then {RELEASE_COMMAND}"),
            )
            .detail(ran.detail));
        }
        if self.facts.attested || self.facts.engine.has {
            return Ok(format!(
                "staged {r} (a promotable attestation in publish/.out)"
            ));
        }
        Err(Refusal::new(
            format!("pub stage did not stage {r} (exit {})", ran.code),
            format!(
                "read its log; fix the source on main, then {RELEASE_COMMAND} --commit \
                 origin/main"
            ),
        )
        .detail(ran.detail))
    }

    /// Refuse what work run beside each other found (`what` names it when several did):
    /// one finding is the refusal itself; several are one refusal, each with its own fix,
    /// a COULD NOT RUN only when every one of them is.
    fn refuse_all(&mut self, what: &str, mut found: Vec<Refusal>) -> Flow {
        match found.len() {
            0 => Ok(()),
            1 => self.refuse(found.remove(0)),
            n => {
                let could_not_run = found.iter().all(|r| r.could_not_run);
                let mut r = blocked(format!("{what} refused ({n} finding(s))"), &found);
                r.could_not_run = could_not_run;
                self.refuse(r)
            }
        }
    }

    /// What the cut would refuse that can change while a release runs: its disk (the
    /// build and `verify release` write tens of GiB), the version's tag (a rostered peer
    /// may cut it meanwhile) and, with `gates`, the cut's read-only pre-claim gates. Asked
    /// at the checks, again before the publish, and the first two once more before a
    /// fresh cut, so a refusal leaves nothing more public. Origin not answering whether
    /// the tag is there is a COULD NOT RUN.
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
            Err(why) => out.push(
                Refusal::new(
                    format!("cannot ask origin whether {tag} is cut: {why}"),
                    "check the network and your git credentials, then re-run",
                )
                .could_not_run(),
            ),
        }
        if gates {
            let creds = self.creds.clone();
            out.extend(self.b.cut_gates(c, &creds));
        }
        out
    }

    /// [`Run::cut_still_possible`] with the cut's gates, asked where a refusal still
    /// leaves nothing public: at the checks (`before` none) and right before the publish
    /// (`before` names it), after the provisioning audit there, which may wait hours for
    /// the owner. A decided refusal (exit 1) comes before what could not be asked (exit
    /// 3), and a refusal here leaves no source public ahead of a binary that cannot
    /// follow it.
    fn still_possible(&mut self, c: &Candidate, before: Option<&str>) -> Flow {
        let found = self.cut_still_possible(c, true);
        let (unasked, decided): (Vec<Refusal>, Vec<Refusal>) =
            found.into_iter().partition(|r| r.could_not_run);
        let asked = before.map_or_else(String::new, |step| {
            format!("asked again right before the {step}, ")
        });
        if !decided.is_empty() {
            self.refuse(blocked(
                format!("{asked}the cut would refuse ({} finding(s))", decided.len()),
                &decided,
            ))?;
        }
        if !unasked.is_empty() {
            let mut r = blocked(
                format!(
                    "{asked}what the cut would refuse could not be asked ({} question(s))",
                    unasked.len()
                ),
                &unasked,
            );
            r.remedy = format!("run {RELEASE_COMMAND} again once it answers (nothing changed)");
            self.refuse(r.could_not_run())?;
        }
        Ok(())
    }

    /// The engine that publishes is the engine that staged, asked right before `pub
    /// publish` of every release: the stage (step 4) came before the long steps, and
    /// peers pull and push the shared engine checkout. The engine is synced first, as the
    /// publish will find it. One whose code moved since it staged R, a publish/.out
    /// without a promotable attestation of R naming its engine, or a staging tier whose
    /// main moved since R was attested (`pub promote` refuses that attestation) stages
    /// again here by the engine as it is now, and only a passing stage with a fresh
    /// attestation lets the publish go on. An engine off its origin/main after the sync,
    /// or an engine or staging tier that cannot be asked, is a resumable stop with
    /// nothing public.
    fn engine_still_staged(&mut self, c: &Candidate, p: &Prover) -> Flow {
        if let Err(r) = self.b.sync() {
            return self.refuse(r);
        }
        self.refresh();
        let r = short(&c.commit).to_string();
        let attested = self.facts.attested;
        let staged = self.facts.staged_by.clone().filter(|_| attested);
        // (the engine a fresh stage must not name again, why): a stage again for the
        // staging tier is by the same engine, so it names none
        let (old, why) = match staged.as_deref() {
            None => (
                None,
                format!(
                    "publish/.out holds no promotable attestation of {r} naming the engine \
                     that staged it"
                ),
            ),
            Some(engine) => match self.b.engine_now(engine) {
                Ok(EngineNow::Same(_)) => match self.staging_still(&r)? {
                    None => return Ok(()),
                    Some(why) => (None, why),
                },
                Ok(EngineNow::Moved(why)) => (staged.clone(), why),
                Ok(EngineNow::Off(why)) => {
                    return self.refuse(
                        Refusal::new(
                            format!("asked again right before the publish: {why}"),
                            format!(
                                "{RELEASE_COMMAND} again: its sync step brings the engine to its \
                                 origin/main (nothing is public)"
                            ),
                        )
                        .could_not_run(),
                    );
                }
                Err(why) => {
                    return self.refuse(
                        Refusal::new(
                            format!(
                                "could not ask again, right before the publish, whether the \
                                 engine is the one that staged {r}: {why}"
                            ),
                            format!("run {RELEASE_COMMAND} again once it answers"),
                        )
                        .could_not_run(),
                    );
                }
            },
        };
        self.say(&format!(
            "        {why}: {r} is staged again by the engine as it is now, before the publish"
        ));
        self.restage = Some((old, why));
        let ran = self.b.stage(c, p);
        self.refresh();
        let note = match self.stage_verdict(c, ran) {
            Ok(note) => note,
            Err(refusal) => return self.refuse(refusal),
        };
        // the fresh attestation names the staging tier's main as the stage left it: a
        // peer staging again meanwhile would make it stale as well
        if let Some(why) = self.staging_still(&r)? {
            return self.refuse(
                Refusal::new(
                    format!(
                        "{r} was staged again right before the publish, and the staging tier \
                         moved again: {why}"
                    ),
                    format!(
                        "{RELEASE_COMMAND} again once no peer is staging aterm (nothing is \
                         public)"
                    ),
                )
                .could_not_run(),
            );
        }
        self.say(&format!("        {note}"));
        Ok(())
    }

    /// Whether `<repo>-staging`'s main moved since R's attestation was made
    /// ([`Backend::staging_moved`]): `Some(why)` when it did. A staging tier that cannot
    /// be asked is a resumable stop, nothing public.
    fn staging_still(&mut self, r: &str) -> Result<Option<String>, Halt> {
        match self.b.staging_moved() {
            Ok(moved) => Ok(moved),
            Err(why) => {
                self.refuse(
                    Refusal::new(
                        format!(
                            "could not ask again, right before the publish, whether the \
                             staging tier's main is the commit {r}'s attestation names: {why}"
                        ),
                        format!("run {RELEASE_COMMAND} again once it answers"),
                    )
                    .could_not_run(),
                )?;
                Ok(None)
            }
        }
    }

    /// Audit this machine's provisioning now ([`crate::provision::audit`]): a gap the
    /// operator closes refuses; a gap only the owner closes is asked once (a standing ask
    /// on the owner's aterm, raised again at each audit until delivered) and, with `wait`,
    /// waited on, re-audited every [`OWNER_POLL_SECS`]; an audit that could not be asked is
    /// a resumable stop where it is needed. Returns the line the step says.
    fn provision(&mut self, wait: bool) -> Result<String, Halt> {
        let mut said_waiting = false;
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
                let mut line = format!("this machine's provisioning passes ({id})");
                if !unknown.is_empty() {
                    line.push_str(&format!(
                        "; not answered now, asked again before the publish: {}",
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
                // a real run waits at the first such step, and the gap is closed after it
                self.wait_at.get_or_insert(self.cur);
                for gap in &gaps {
                    if !self.waits.contains(gap) {
                        self.waits.push(gap.clone());
                    }
                }
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
                let ready = if self.facts.verified.yes {
                    "READY (built once, verified)"
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
            let t0 = self.b.now();
            self.b.sleep(OWNER_POLL_SECS);
            self.waited += self.b.now().saturating_sub(t0);
            self.halt_if_interrupted()?;
        }
    }

    /// Raise each open owner gap's ask once (journaled by key only when the owner's aterm
    /// took it, so an undelivered ask is raised again at the next audit), and resolve each
    /// journaled ask whose check now passes (a check that got no answer resolves nothing).
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
            if asked.contains(&words.key)
                || self.early_asks.contains(&words.key)
                || !now_asked.insert(words.key.clone())
            {
                continue;
            }
            match self.b.ask(words) {
                Ok(reply) => {
                    self.say(&format!(
                        "        asked the owner once ({}): {} — {} (delivered: {reply})",
                        words.key, words.reason, words.step
                    ));
                    match self.journal.as_mut() {
                        Some(j) => {
                            j.asks.push(words.key.clone());
                            changed = true;
                        }
                        // asked by the checks of a release with no journal yet: noted
                        // here, journaled with the candidate at step 3
                        None => self.early_asks.push(words.key.clone()),
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

    // -- build once, with the release's own handoff fixtures -----------------

    /// Reserve R's identity unless the journal holds a reservation the ledger has not
    /// passed (a passed one means another release claimed past it; nothing was claimed
    /// here, so nothing is burned), journal it, then build its bundle once (or take the
    /// COMPLETE one in the release cache) and journal that. A new reservation is new
    /// bytes, so the bundle, its verification and its staged fixtures follow it.
    ///
    /// Beside the build, before the claim, the release's own handoff fixtures are staged
    /// in the release cache under the identity the shipped binary stamps (the
    /// reservation), for `after-cut` to land, and journaled ([`FIXTURES_STAGED`]). A
    /// generator that refuses refuses the release here, with nothing claimed or
    /// published; the bundle built beside it is kept.
    fn build(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        if self.dry {
            let held = match (self.reservation(), self.b.origin_tail()) {
                (Some(r), Some(tail)) if r.build > tail => Some(r),
                _ => None,
            };
            let reserved = match &held {
                Some(r) => format!(
                    "build {} reserved as {} (this release's journal)",
                    r.build,
                    short(&r.commit)
                ),
                None => "would reserve build n = max(origin's ledger tail + 1, now) and the \
                         release commit (R + the ledger line + the rolled changelog), claimed \
                         nowhere"
                    .to_string(),
            };
            let fixtures = if self.facts.fixtures.yes {
                self.facts.fixtures.words.clone()
            } else {
                let identity = held.map_or_else(
                    || "the build it reserves".to_string(),
                    |r| format!("build {} ({})", r.build, short(&r.commit)),
                );
                format!(
                    "beside it, would write v{}'s handoff fixtures for {identity} with {} into \
                     the release cache, refusing here if it refuses; step 10 lands them on main",
                    c.version,
                    fixture_generator()
                )
            };
            self.finish(&format!(
                "{reserved}; would build both slices at once from its snapshot into the \
                 release cache, the unsigned bundle kept by what it is built as ({}); {fixtures}",
                self.facts.built.words
            ));
            return Ok(());
        }
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
                    j.done.retain(|fact| fact != FIXTURES_STAGED);
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
        let fixtures = !self.facts.fixtures.yes && !self.journal_has(FIXTURES_STAGED);
        let mut words = Vec::new();
        let mut refused: Vec<Refusal> = Vec::new();
        let staged = if self.facts.built.yes {
            words.push(self.facts.built.words.clone());
            fixtures.then(|| self.b.stage_fixtures(&c, &reserved, &p))
        } else {
            let log = self.b.log_path(Step::Build);
            self.say(&format!("        log: {}", log.display()));
            let built = self.b.build(&c, &reserved, &p, fixtures);
            match built.artifact {
                Ok(artifact) => {
                    if let Some(j) = self.journal.as_mut() {
                        j.artifact = Some(artifact.clone());
                    }
                    self.save_journal()?;
                    self.refresh();
                    if self.facts.built.yes {
                        words.push(format!(
                            "built ONCE: {} — executable sha256 {}",
                            artifact.app.display(),
                            artifact.macho_sha256
                        ));
                    } else {
                        let words = self.facts.built.words.clone();
                        refused.push(Refusal::new(
                            format!(
                                "the build ended, and its bundle is not the journal's: {words}"
                            ),
                            format!("read its log, then {RELEASE_COMMAND} (it builds again)"),
                        ));
                    }
                }
                Err(r) => refused.push(r),
            }
            built.fixtures
        };
        match staged {
            Some(Ok(note)) => {
                self.journal_add(FIXTURES_STAGED)?;
                words.push(note);
            }
            Some(Err(r)) => refused.push(r),
            None => {}
        }
        if self.facts.fixtures.yes {
            words.push(self.facts.fixtures.words.clone());
        }
        self.refuse_all("the build step", refused)?;
        self.finish(&words.join("; "));
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

    // -- verify ------------------------------------------------------------

    /// `verify release --app <the bundle>` in the clone at R: every check a release
    /// requires, MEASURE of the shipping bytes among them. A pass is recorded (R's tree,
    /// the bundle's executable) and read by a resumed release; any other exit refuses with
    /// the rerun command. A clone off R's tree or dirty, before or after the run, records
    /// nothing and stops resumably.
    fn verify(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        let words = self.facts.verified.words.clone();
        if self.dry {
            self.finish(&format!(
                "would run `targo {} --app <the bundle step 5 built>` in the clone ({words})",
                VERIFY_ARGS.join(" ")
            ));
            return Ok(());
        }
        let Some(artifact) = self.artifact() else {
            return self.refuse(Refusal::new(
                "no bundle is built for this release — `verify release` judges the bytes that \
                 ship",
                format!("{RELEASE_COMMAND} (step 5 builds it)"),
            ));
        };
        let rerun = verify_command(&artifact.app.display().to_string());
        let log = self.b.log_path(Step::Verify);
        self.say(&format!("        runs: {rerun}"));
        self.say(&format!("        log: {}", log.display()));
        let ran = self.b.verify(&c, &p, &artifact);
        self.refresh();
        if let Some(spent) = self.spent() {
            return self.refuse(
                Refusal::new(
                    format!(
                        "origin's ledger reached the reservation while it was verified: {}; \
                         these bytes can never ship",
                        spent.words()
                    ),
                    format!(
                        "run {RELEASE_COMMAND} again — it reserves anew, then builds and verifies"
                    ),
                )
                .could_not_run(),
            );
        }
        let ran = match ran {
            Ok(ran) => ran,
            Err(refusal) => return self.refuse(refusal),
        };
        if self.facts.verified.yes {
            let words = self.facts.verified.words.clone();
            self.finish(&words);
            return Ok(());
        }
        let words = self.facts.verified.words.clone();
        let r = short(&c.commit).to_string();
        let refusal = match ran.code {
            0 => Refusal::new(
                format!("`verify release` passed for {r}, and its record cannot be read: {words}"),
                format!("run {RELEASE_COMMAND} again once it reads (the candidate is kept)"),
            )
            .could_not_run(),
            1 => Refusal::new(
                format!("`verify release` failed for {r} (exit 1)"),
                format!(
                    "fix what it names on main (`{rerun}` in the clone runs it again), then \
                     {RELEASE_COMMAND} --commit origin/main"
                ),
            ),
            code => Refusal::new(
                format!(
                    "`verify release` could not run every check for {r} (exit {code}: {}) — \
                     nothing failed, and a release needs every check",
                    match code {
                        3 => "a check could not run or was skipped",
                        2 => "its command line was refused",
                        _ => "it did not finish",
                    }
                ),
                format!(
                    "remove the cause it names (above, and in its log), then {RELEASE_COMMAND}; \
                     `{rerun}` in the clone runs it alone"
                ),
            )
            .could_not_run(),
        };
        self.refuse(refusal.detail(ran.detail))
    }

    // -- READY, then publish --------------------------------

    fn records_stand(&mut self, what_refuses: &str) -> Flow {
        let r = short(&self.commit_r()).to_string();
        if !self.facts.verified.yes {
            let words = self.facts.verified.words.clone();
            self.refuse(Refusal::new(
                format!(
                    "`verify release` has not passed for the bundle of {r} ({words}) — \
                     {what_refuses}"
                ),
                format!("{RELEASE_COMMAND} (steps 5-6 build and verify it)"),
            ))?;
        }
        Ok(())
    }

    /// READY: the record the cut requires (`verify release` passed on R's bundle's
    /// executable) stands for R, or nothing goes public. `--prepare` stops after it.
    fn ready(&mut self) -> Flow {
        if self.dry {
            self.finish(
                "would require the `verify release` record to stand for R: READY, or nothing \
                 goes public",
            );
            return Ok(());
        }
        self.records_stand("nothing goes public")?;
        let source = if self.facts.engine.has {
            "its source published"
        } else if self.facts.attested {
            "its source staged"
        } else {
            "its source to be staged again at the publish"
        };
        let r = short(&self.commit_r()).to_string();
        self.finish(&format!(
            "READY: {r}, its bundle built once and verified, {source}"
        ));
        Ok(())
    }

    /// The publish, where the source goes public: audit 2 (this machine's provisioning,
    /// an owner gap waited on here with the candidate READY and nothing public), then what
    /// the cut would refuse and the engine that staged R ([`Run::engine_still_staged`],
    /// else the stage runs again), then `pub publish`.
    fn publish(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        if self.dry {
            let line = self.provision(true)?;
            self.finish(&format!(
                "{line}; would ask again what the cut would refuse and that the engine is the \
                 one that staged R (else stage again), then run: pub publish aterm --repo-path \
                 <this clone>"
            ));
            return Ok(());
        }
        self.records_stand("the cut would refuse after the publish")?;
        self.provision(true)?;
        self.still_possible(&c, Some("publish"))?;
        self.engine_still_staged(&c, &p)?;
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
        // A publish that exited nonzero is not done even when the ledger records it: what
        // it did not finish (e.g. sign-release, so no signed source manifest) would reach
        // the cut.
        if ran.code != 0 {
            return self.refuse(
                Refusal::new(
                    format!(
                        "pub publish recorded {r} but exited {}: {}",
                        ran.code,
                        ran.detail.last().cloned().unwrap_or_default()
                    ),
                    format!(
                        "finish what its last line names (e.g. pub sign-release aterm), then \
                         {RELEASE_COMMAND}"
                    ),
                )
                .detail(ran.detail),
            );
        }
        if let Err(r) = self.b.source_publication(&c) {
            return self.refuse(r);
        }
        let rev = self.facts.engine.rev.clone().unwrap_or_default();
        self.finish(&format!("published {r} -> alabsystems {}", short(&rev)));
        Ok(())
    }

    /// Audit 2 for a fresh cut this run publishes nothing for: an earlier run published R
    /// and stopped before the claim (interrupted, or refused pre-claim), so the cut claims
    /// with no publish between. The waiting audit and the re-check of what the cut would
    /// refuse run here all the same: the cut's own pre-claim checks ask Apple nothing, so
    /// an owner gap opened since (an expired developer agreement) would first be met after
    /// the claim, a build spent.
    fn audit_before_cut(&mut self, why: &str) -> Flow {
        let Some(c) = self.cand.clone() else {
            self.skip(why);
            return Ok(());
        };
        // the publish itself is skipped (no time of it is recorded): said as a skip
        let line = self.provision(true)?;
        if self.dry {
            self.skip(&format!(
                "{why}; {line}; would ask again what the cut would refuse, then go on to the cut"
            ));
            return Ok(());
        }
        self.still_possible(&c, Some("cut"))?;
        // A publish may have recorded R, then failed at sign-release. The ledger
        // skips the publish on a re-run, but never proves that its signed source
        // manifest exists: ask the channel before the cut can claim or make head.
        if let Err(r) = self.b.source_publication(&c) {
            return self.refuse(r);
        }
        self.skip(&format!(
            "{why}; asked again before the cut it does not publish for: {line}, and what the \
             cut would refuse"
        ));
        Ok(())
    }

    // -- cut ---------------------------------------------------------------

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
                    format!("{RELEASE_COMMAND} (step 8 publishes it)"),
                ));
            }
            if let Some(refusal) = self.newest_row_refusal() {
                return self.refuse(refusal);
            }
            self.records_stand("the cut refuses pre-claim")?;
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

    // -- after-cut and final check ------------------------------------------

    /// A site this machine cannot sync is a hand step, never a stop: `ship site` finds no
    /// usable site checkout here before it touches anything, so a re-run here would only
    /// meet that again. It is journaled (`site-deferred` with `after-cut`, in one save),
    /// said with the command for a machine that has the checkout (here, in the final line
    /// and by every later run), and the release goes on to its check.
    fn after_cut(&mut self) -> Flow {
        let v = self.version();
        let mut notes = Vec::new();
        if !self.journal_has("after-cut") {
            match self.b.after_cut(&v) {
                Ok(done) => {
                    if done.site_deferred {
                        self.journal_add_all(&[SITE_DEFERRED, "after-cut"])?;
                    } else {
                        self.journal_add("after-cut")?;
                    }
                    notes.push(format!("{}{}", done.note, self.site_words()));
                }
                Err(r) => return self.refuse(r),
            }
        }
        if !self.facts.fixtures.yes {
            notes.push(self.land_fixtures()?);
        }
        self.finish(&notes.join("; "));
        Ok(())
    }

    /// Land the release's own handoff fixtures (staged for the claimed reservation) on
    /// main, so the next cut's gate finds them: nothing when main holds them, and written
    /// again first when the cache lost them.
    fn land_fixtures(&mut self) -> Result<String, Halt> {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(String::new());
        };
        let claimed = self.facts.cut.as_ref().map(|cut| cut.build);
        // The identity the shipped binary stamps: the journal's reservation of the
        // claimed build, else (a claim resumed after `--forget`) the claimed build and
        // the release commit its published tag names.
        let mut reserved = self.reservation().filter(|r| Some(r.build) == claimed);
        if reserved.is_none()
            && let Some(build) = claimed
            && let Ok(Some(commit)) = self.b.origin_tag_now(&format!("v{}", c.version))
        {
            reserved = Some(Reservation {
                build,
                commit,
                line: format!("{build} {}", c.version),
            });
        }
        let Some(reserved) = reserved else {
            self.refuse(Refusal::new(
                format!(
                    "neither this release's journal nor a published tag names the release \
                     commit of the claimed build {} to land v{}'s handoff fixtures as",
                    claimed.map_or_else(|| "?".to_string(), |b| b.to_string()),
                    c.version
                ),
                format!(
                    "add them by hand ({}, \"By hand\")",
                    crate::gates::HANDOFF_FIXTURE_README
                ),
            ))?;
            return Ok(String::new());
        };
        match self.b.land_fixtures(&c, &reserved, &p) {
            Ok(note) => {
                self.refresh();
                Ok(note)
            }
            Err(r) => {
                self.refuse(r)?;
                Ok(String::new())
            }
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

/// What the cut would refuse, as one refusal: `why`, then each finding with its own fix
/// line.
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

/// The in-tree handoff fixture generator, as a plan line names it.
fn fixture_generator() -> String {
    format!(
        "the in-tree generator (targo --unverified test -p aterm-gui --lib -- {})",
        crate::fixtures::generator_args().join(" ")
    )
}

fn running_refusal(h: &Holder) -> Refusal {
    if let Some(step) = &h.step {
        // its pid, never its group: the group may hold a trustd, which is never signalled
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

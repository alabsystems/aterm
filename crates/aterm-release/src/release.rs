// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified ship release`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::check_record::{Half, VerifyRun};
use crate::provision::{AskWords, Audited, Finding};

/// How an operator runs a release: the one spelling every remedy names.
pub const RELEASE_COMMAND: &str = crate::ledger::RELEASE_COMMAND;

/// What `verify` runs in the clone at R after the release's own `targo`, as an agent would run it (every check a release requires), then `--app <the bundle>`, the bytes it judges.
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

/// `word` as one shell word: as it is when nothing in it is special to a shell, else single-quoted.
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

/// The release's directory in the clone's git common directory: its journal, its run claim — shared by every worktree of the clone.
pub const RELEASE_DIR: &str = "aterm-release";

/// The release journal, in [`RELEASE_DIR`].
pub const JOURNAL_FILE: &str = "release.toml";

/// The release line's source: a head whose tree lacks it has a cutter that keeps no line, so
/// `--fix` refuses to carry onto it.
pub const LINE_SOURCE: &str = "crates/aterm-release/src/line.rs";
const PENDING_LINE: &str = "locally-carried-line";
const SUPERSEDED_LINE: &str = "supersession-completed";
pub(crate) const SELECTION_FILE: &str = "candidate.toml";

/// One explicit line replacement, kept only in `candidate.toml` until its source
/// compare-and-swap is decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SupersedeTxn {
    /// The checkout HEAD before selecting the replacement.
    pub head: String,
    /// The authoritative `release.toml` line, or its authoritative absence.
    pub release_line: Option<crate::line::Line>,
    /// The exact fleet line the replacement retires.
    pub previous: crate::line::Line,
    /// The one replacement identity preflighted, journaled, and written.
    pub replacement: crate::line::Line,
    /// The source writer was entered, so only an authoritative exact reread may decide it.
    #[serde(default)]
    pub cas_unknown: bool,
}

/// The source compare-and-swap's structured answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupersedeResult {
    /// The exact requested supersession is durable.
    Committed(String),
    /// The write is authoritatively absent or was not attempted.
    Refused {
        /// Why the write did not become durable.
        why: String,
        /// An ambiguous earlier call was authoritatively absent and may be retried now.
        retry: bool,
        /// The authoritative replacement when a different writer won.
        winner: Option<crate::line::Line>,
    },
    /// The write may be durable, but an exact reread was unavailable.
    Unknown(String),
}

/// The live run's claim, beside the journal.
pub const RUNNING_FILE: &str = "running";

/// The running step's children, beside the claim.
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
        .filter(|entry| entry.file_name().to_str().is_some_and(is_child_note))
        .map(|entry| entry.path())
        .collect();
    notes.sort_unstable();
    notes
}

fn is_child_note(name: &str) -> bool {
    name == RUNNING_CHILD_FILE
        || name
            .strip_prefix(RUNNING_CHILD_FILE)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// The journal's fact that R's own pre-claim gates that read only the commit passed (the changelog, the predecessor's handoff fixtures, the handoff policy, the locked metadata).
pub const COMMIT_GATES: &str = "commit-gates";

/// The journal's fact that the release's own handoff fixtures are staged in the release cache for the journal's reservation.
pub const FIXTURES_STAGED: &str = "fixtures-staged";

/// How often an owner gap is audited again while the release waits on it: a poll cadence, never a verdict.
pub const OWNER_POLL_SECS: u64 = 600;

const CUT_GIB: u64 = crate::gates::MIN_FREE_DISK_GIB;

/// One step of the release, in plan order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Checks,
    Sync,
    Commit,
    /// AppKit judged first, then R's tree, bundle build and export checked at once ([`Branch`]).
    Qualify,
    /// The remaining bundle half of `verify release`, once build and tree ended (normally measure).
    Bundle,
    Ready,
    Publish,
    Cut,
    AfterCut,
    Check,
}

impl Step {
    /// Every step, in the order a release runs them.
    pub const ALL: [Step; 10] = [
        Step::Checks,
        Step::Sync,
        Step::Commit,
        Step::Qualify,
        Step::Bundle,
        Step::Ready,
        Step::Publish,
        Step::Cut,
        Step::AfterCut,
        Step::Check,
    ];

    /// The step's name on the transcript, in its log's file name and in `steps.tsv`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Step::Checks => "checks",
            Step::Sync => "sync",
            Step::Commit => "commit",
            Step::Qualify => "qualify",
            Step::Bundle => "bundle",
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

    const fn default_secs(self) -> u64 {
        match self {
            Step::Sync | Step::Ready => 15,
            Step::Checks | Step::Commit | Step::AfterCut => 60,
            Step::Qualify => 1_800,
            Step::Bundle => 600,
            Step::Publish => Side::Sign.default_secs(),
            Step::Cut => 300,
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
    /// This refusal proves that a previously recorded line lost its source-main record.
    lost_line_record: bool,
    /// The gate or build whose red this is, as the repair worker is told it: a real cut that
    /// refuses with one opens that worker ([`Backend::repair`]). `None` for every other stop.
    pub failing: Option<String>,
}

impl Refusal {
    #[must_use]
    pub fn new(why: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            remedy: remedy.into(),
            detail: Vec::new(),
            could_not_run: false,
            lost_line_record: false,
            failing: None,
        }
    }

    /// The red of the gate or build `gate` ([`Refusal::failing`]).
    #[must_use]
    pub fn failing(mut self, gate: impl Into<String>) -> Self {
        self.failing = Some(gate.into());
        self
    }

    #[must_use]
    pub fn detail(mut self, lines: impl IntoIterator<Item = String>) -> Self {
        self.detail.extend(lines);
        self
    }

    #[must_use]
    pub const fn could_not_run(mut self) -> Self {
        self.could_not_run = true;
        self
    }

    #[must_use]
    const fn lost_line_record(mut self) -> Self {
        self.lost_line_record = true;
        self
    }

    #[must_use]
    const fn lost_line_record_if(self, lost: bool) -> Self {
        if lost { self.lost_line_record() } else { self }
    }
}

/// What R's commit-only preflight ([`Backend::commit_gates`]) found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preflight {
    /// What a gate that passed says the run must show: a bump still to be written names the
    /// harness versions its own commit writes.
    pub notes: Vec<String>,
    /// Each gate's refusal; empty when the preflight passes.
    pub found: Vec<Refusal>,
}

/// What the release-input judgment ([`Backend::input_checks`]) found against a candidate. It
/// names no remedy: which command fixes it depends on the candidate's state, which only
/// [`Run::remedy`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputFinding {
    /// The judgment failed the source: what it said.
    Failed(String),
    /// The judgment could not be made (COULD NOT RUN): why.
    Unanswered(String),
}

impl InputFinding {
    /// What the judgment said, or why it could not be made.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Self::Failed(why) | Self::Unanswered(why) => why,
        }
    }
}

/// The Linux declaration a cut requires: `--mac-only`, or `--linux-artifacts DIR` with its targets (triples) and workers.
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
    pub const fn declared(&self) -> bool {
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
    /// Build and freeze this clean origin/main checkout without starting a cut.
    pub prefreeze: bool,
    /// `--bump`: commit the next product version on origin/main's tip and start a line at it.
    pub bump: bool,
    /// `--commit <rev>`: start a line at this commit of origin/main.
    pub commit: Option<String>,
    /// Explicitly retire an unpublished fleet line in favour of `commit`.
    pub supersede: bool,
    /// `--fix <commit>…`: carry these landed, non-merge main commits onto this clone's
    /// unpublished line as hops ([`crate::line`]).
    pub fix: Vec<String>,
    pub linux: LinuxDecl,
    /// `--release-credentials <path>`; default `~/.aterm/release-ring/release-credentials.toml`.
    pub release_credentials: Option<PathBuf>,
    /// `--engine <path>`; default `$PUBLICATION_ENGINE`, else `~/publication`: the engine whose
    /// origin the release's own engine is cloned from ([`aterm_verify::engine::engine_copy`]).
    pub engine: Option<PathBuf>,
    pub dry_run: bool,
    pub verbose: bool,
    /// `--prepare`: run the heavy half and stop after READY, PREPARED, nothing public; a bare run then finishes it ([`Prepared`]).
    pub prepare: bool,
    /// `--qualify`: check and build a private candidate without pushing staging or publishing.
    pub qualify: bool,
    /// `--forget`: drop this clone's release journal (nothing public changes).
    pub forget: bool,
    /// `--abandon vX.Y.Z`: withdraw this clone's claimed, unpublished cut of it.
    pub abandon: Option<String>,
}

const PATH_FLAGS: [&str; 2] = ["--release-credentials", "--linux-artifacts"];

/// `path` as the directory `cwd` reads it: an absolute path as given; a relative one joined to `cwd`, and canonical when it exists.
#[must_use]
pub fn anchor(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let joined = cwd.join(path);
    joined.canonicalize().unwrap_or(joined)
}

impl ReleaseOptions {
    /// Anchor `--engine`, `--release-credentials` and `--linux-artifacts` once, where the release was started (`cwd`).
    #[must_use]
    pub fn anchored(&self, cwd: &Path) -> Self {
        let at = |p: &Option<PathBuf>| p.as_deref().map(|p| anchor(p, cwd));
        let mut opts = self.clone();
        opts.engine = at(&self.engine);
        opts.release_credentials = at(&self.release_credentials);
        opts.linux.artifacts = at(&self.linux.artifacts);
        opts
    }

    /// `args` (a process's own, after the program) as R's frozen cutter receives them on exec:
    /// each path flag's value replaced by these options'; a real run leaves `--fix` and its
    /// commits out because the new image resumes the journaled head. A dry run retains them
    /// to reconstruct its unjournaled plan. Both leave out
    /// `--engine` with its directory left out, since the exec'd image is handed the engine source
    /// this release took (`ATERM_RELEASE_ENGINE_SOURCE`) and the repository it captured, beside
    /// this release's own engine as `PUBLICATION_ENGINE` ([`aterm_verify::engine::engine_copy`]),
    /// which a cutter older than that handover takes, and a cutter older than that copy would run
    /// and sync the directory `--engine` named in place.
    #[must_use]
    pub fn exec_args(&self, args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
        let value = |flag: &str| match flag {
            "--release-credentials" => self.release_credentials.as_ref(),
            _ => self.linux.artifacts.as_ref(),
        };
        let mut out = Vec::with_capacity(args.len());
        let mut args = args.into_iter().peekable();
        while let Some(arg) = args.next() {
            if arg.to_str() == Some("--supersede") {
                continue;
            }
            if arg.to_str() == Some("--fix") && !self.dry_run {
                while args.next_if(|a| a.to_str().is_some_and(is_value)).is_some() {}
                continue;
            }
            if arg.to_str() == Some("--engine") {
                let _ = args.next();
                continue;
            }
            let flag = arg
                .to_str()
                .and_then(|a| PATH_FLAGS.iter().copied().find(|&f| f == a));
            out.push(arg);
            if let Some(flag) = flag
                && let Some(given) = args.next()
            {
                out.push(value(flag).map_or(given, |p| p.clone().into_os_string()));
            }
        }
        out
    }

    /// A dispatch before judgment preserves a requested supersession until its replacement
    /// cutter has performed it. Subsequent execs use the ordinary completed-operation args.
    pub fn dispatch_args(&self, args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
        let mut args = self.exec_args(args);
        if self.supersede {
            args.push("--supersede".into());
        }
        args
    }
}

/// A flag's value as `release` parses it: a word that is not empty and is no flag.
#[must_use]
pub fn is_value(word: &str) -> bool {
    !word.is_empty() && !word.starts_with('-')
}

/// This clone's release: what a re-run resumes with.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// The frozen public product version: historical `X.Y.0` or calendar `1.YYYYMMDD.N`.
    pub version: String,
    /// R, full: the head of [`Journal::line`].
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
    /// Facts only this journal holds for R: [`COMMIT_GATES`], [`FIXTURES_STAGED`], `after-cut`, `tap-aligned`, `site-deferred`, `check`.
    #[serde(default)]
    pub done: Vec<String>,
    /// The owner asks delivered and not yet resolved, by key: carried over to each new head, withdrawn by `--forget`.
    #[serde(default)]
    pub asks: Vec<String>,
    /// R's reserved identity (`ledger::reserve`, at `build`): what the bundle is built as and the cut claims.
    #[serde(default)]
    pub reserved: Option<Reservation>,
    /// The bundle built from that reservation: what `verify release` judges and the cut signs.
    #[serde(default)]
    pub artifact: Option<Artifact>,
    /// Written only at `--prepare`'s stop (everything heavy done for R, nothing claimed).
    #[serde(default)]
    pub prepared: Option<Prepared>,
    /// The line R heads: its base on origin/main and the hops `--fix` carried onto it. A journal
    /// written before lines has none; [`Journal::line`] reads it as a line at R with no hops.
    #[serde(default)]
    pub line: Option<crate::line::Line>,
    /// Present only in `candidate.toml` while an explicit supersession is undecided.
    #[serde(default)]
    pub(crate) supersede: Option<SupersedeTxn>,
}

/// In-memory diagnostics never change the authoritative candidate or release journals.
struct LineSelection {
    head: String,
    source: String,
    holder: Option<String>,
    announced: bool,
    /// The fleet claim of the version was read since this selection was made: it stands, or
    /// the fleet's line replaced it ([`Run::fleet_selection`]).
    settled: bool,
}

/// A PREPARED release, as `--prepare` left it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepared {
    /// Epoch seconds the prepare stopped.
    pub at: u64,
    /// The prover the prepare leased: its bin dir, physical — the build the visible half re-leases.
    pub prover: String,
    /// That prover's `trustc -vV` commit-hash.
    pub trustc: String,
    /// The build number it reserved (the journal's reservation), built and verified.
    pub build: u64,
    /// SHA-256 of the executable `verify release` judged (the record it passed on).
    pub macho_sha256: String,
    /// The engine commit R's attestation names (the engine that staged).
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

/// The bundle built once for a reservation, as the journal keeps it.
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
    /// # Errors
    /// A file that does not parse as a release journal.
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        match crate::durable::read_regular_text(path, 1024 * 1024) {
            Ok(text) => aterm_toml::from_str(&text)
                .map(Some)
                .map_err(|e| format!("{} is not a release journal: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    /// Written whole: a temporary, flushed, renamed, the directory flushed.
    /// # Errors
    /// Any write.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text =
            aterm_toml::to_string(self).map_err(|e| format!("serialize the journal: {e}"))?;
        if path.parent().is_none() {
            return Err(format!("{} has no directory", path.display()));
        }
        crate::durable::write_atomic_durable(path, text.as_bytes(), 0o600)
            .map_err(|e| format!("write {}: {e}", path.display()))
    }

    /// The line R heads: the one journaled, else (a journal written before lines) a line at R
    /// with no hops.
    #[must_use]
    pub fn line(&self) -> crate::line::Line {
        self.line
            .clone()
            .unwrap_or_else(|| crate::line::Line::start(&self.version, &self.commit, self.started))
    }

    /// The release reached its check: everything is done for R.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.has("check") && self.has(TAP_ALIGNED) && !self.has(SITE_DEFERRED)
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

/// `<git common dir>/`[`RELEASE_DIR`] of the repository `git` runs in: where its release journal is, whichever worktree asks.
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

/// The live run's claim: `pid`, `started` (that pid's start time, as `ps -o lstart=` prints it)
/// and `logs`, created exclusively, continued by another image exec'd into the same process, or
/// adopted once from the live ancestor that a pre-B1 cutter marked `handed` before starting this
/// image.
#[derive(Debug)]
pub struct RunClaim {
    path: PathBuf,
    /// This process's pid and start, as the claim names it.
    me: (u32, String),
    /// This image continued the preceding cutter's claim, normally in the same process after an
    /// exec, or once from a pre-B1 handed ancestor.
    pub continued: bool,
    /// The continuation was the one remaining pre-B1 child-process handoff, not a native exec.
    pub legacy_handoff: bool,
    /// The logs the preceding image of this process named, when they differ from this image's.
    pub previous_logs: Option<PathBuf>,
}

impl Drop for RunClaim {
    /// Remove the claim while it names this process ([`RunClaim::remove_if_mine`]).
    fn drop(&mut self) {
        self.remove_if_mine(|| {});
    }
}

/// The lock beside the claim at `path` (`<RUNNING_FILE>.lock`), held across each read of the
/// claim and the write or removal it decides ([`RunClaim::take`], RunClaim's drop), so none lands
/// between another's read and its write: a continued image's take never lands between this
/// image's read of its claim and its removal of it. Taken blocking, by `flock`: each holds it only
/// for its read and the write it decides.
/// # Errors
/// The lock file cannot be opened or locked.
fn claim_lock(path: &Path) -> std::io::Result<aterm_update_core::FileLock> {
    aterm_update_core::FileLock::acquire(&path.with_extension("lock"))
}

/// A live claim of another run: who, since when, and its logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub started: String,
    pub logs: String,
    /// `Some(the step)` for a child its run left behind.
    pub step: Option<String>,
    /// A pre-B1 cutter marked this live claim for the child cutter it started.
    pub legacy_handed: bool,
}

pub(crate) fn read_holder(path: &Path) -> Option<Holder> {
    let text = crate::durable::read_regular_text(path, 64 * 1024).ok()?;
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(str::trim)
    };
    Some(Holder {
        pid: field("pid ")
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()?,
        started: field("started ").unwrap_or_default().to_string(),
        logs: field("logs ").unwrap_or_default().to_string(),
        step: field("step ").map(str::to_string),
        legacy_handed: text.lines().any(|line| line.trim() == "handed"),
    })
}

fn alive(holder: &Holder, started_of: &mut dyn FnMut(u32) -> Option<String>) -> bool {
    !holder.started.is_empty() && started_of(holder.pid).as_deref() == Some(&holder.started)
}

/// The live holder of `path`, if any: a claim whose pid is alive with the start time it recorded.
pub fn live_holder(
    path: &Path,
    started_of: &mut dyn FnMut(u32) -> Option<String>,
) -> Option<Holder> {
    if let Some(holder) = read_holder(path).filter(|h| alive(h, started_of)) {
        return Some(holder);
    }
    live_child(path.parent().unwrap_or(Path::new(".")), started_of)
}

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
    /// Claim `path` for this process (`me`: its pid and start time): created when no live run
    /// holds it, and continued, with its logs atomically rewritten, when its live holder has
    /// exactly this process identity ([`Self::continued`]). A claim written by a pre-B1 cutter is
    /// also adopted only when it carries that cutter's `handed` marker and its live holder is an
    /// ancestor of this process; this reader-only boundary lets a cut already in flight survive
    /// the B1 deployment without retaining the old writer or child-rerun mechanism. The claim's
    /// lock is held throughout ([`claim_lock`]).
    /// # Errors
    /// The live holder, or the claim's directory cannot be made, its lock taken, or the claim
    /// written.
    pub fn take(
        path: &Path,
        me: (u32, &str),
        logs: &Path,
        started_of: &mut dyn FnMut(u32) -> Option<String>,
    ) -> Result<Self, std::result::Result<Holder, String>> {
        let ancestors = crate::cutter::ancestors();
        Self::take_locking(path, me, logs, started_of, &ancestors, &claim_lock)
    }

    /// [`Self::take`] with the claim's lock taken by `lock`: a test's seam, to take it without
    /// waiting.
    fn take_locking(
        path: &Path,
        me: (u32, &str),
        logs: &Path,
        started_of: &mut dyn FnMut(u32) -> Option<String>,
        ancestors: &[u32],
        lock: &dyn Fn(&Path) -> std::io::Result<aterm_update_core::FileLock>,
    ) -> Result<Self, std::result::Result<Holder, String>> {
        let body = format!("pid {}\nstarted {}\nlogs {}\n", me.0, me.1, logs.display());
        let claim = |previous_logs: Option<PathBuf>, continued, legacy_handoff| Self {
            path: path.to_path_buf(),
            me: (me.0, me.1.to_string()),
            continued,
            legacy_handoff,
            previous_logs,
        };
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| Err(format!("create {}: {e}", dir.display())))?;
        let _lock = lock(path).map_err(|e| Err(format!("lock {}: {e}", path.display())))?;
        if let Some(holder) = live_holder(path, started_of) {
            if holder.step.is_none() && holder.pid == me.0 && holder.started == me.1 {
                crate::durable::write_atomic_durable(path, body.as_bytes(), 0o600)
                    .map_err(|e| Err(format!("continue {}: {e}", path.display())))?;
                let previous_logs = (!holder.logs.is_empty() && Path::new(&holder.logs) != logs)
                    .then(|| PathBuf::from(&holder.logs));
                return Ok(claim(previous_logs, true, false));
            }
            if holder.step.is_none() && holder.legacy_handed && ancestors.contains(&holder.pid) {
                crate::durable::write_atomic_durable(path, body.as_bytes(), 0o600)
                    .map_err(|e| Err(format!("adopt pre-B1 handoff {}: {e}", path.display())))?;
                let previous_logs = (!holder.logs.is_empty() && Path::new(&holder.logs) != logs)
                    .then(|| PathBuf::from(&holder.logs));
                return Ok(claim(previous_logs, true, true));
            }
            return Err(Ok(holder));
        }
        for note in child_notes(dir) {
            let _ = std::fs::remove_file(note);
        }
        if crate::bundle::exists_or_symlink(path) {
            let aside = dir.join(format!("{RUNNING_FILE}.stale"));
            std::fs::rename(path, &aside)
                .map_err(|e| Err(format!("move the stale {} aside: {e}", path.display())))?;
        }
        let created = crate::durable::open_create_new(path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, body.as_bytes()));
        match created {
            Ok(()) => Ok(claim(None, false, false)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(read_holder(path).ok_or_else(|| format!("{} appeared", path.display())))
            }
            Err(e) => Err(Err(format!("write {}: {e}", path.display()))),
        }
    }

    /// The claim on disk names this process (its pid and start).
    #[must_use]
    pub fn names_this_run(&self) -> bool {
        read_holder(&self.path)
            .is_some_and(|h| h.step.is_none() && h.pid == self.me.0 && h.started == self.me.1)
    }

    /// Remove the claim while it names this process, `between` run after the read that found
    /// it does and before the removal, the claim's lock held across all three
    /// ([`claim_lock`]). A lock that cannot be taken leaves the claim: its holder, this process,
    /// is gone once it ends, and the next run moves a dead holder's claim aside.
    fn remove_if_mine(&self, between: impl FnOnce()) {
        let Ok(_lock) = claim_lock(&self.path) else {
            return;
        };
        if self.names_this_run() {
            between();
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// `<state>/steps.tsv`: `epoch\tversion\tstep\tsecs`, a line per step that ran.
/// Returns the upper median (`sorted[len / 2]`) of the last ≤ 5 valid durations per step.
#[must_use]
pub fn estimates(tsv: &str) -> BTreeMap<String, u64> {
    let mut runs: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for line in tsv.lines() {
        let mut cols = line.split('\t');
        if let (Some(_), Some(_), Some(step), Some(secs), None) = (
            cols.next(),
            cols.next(),
            cols.next(),
            cols.next(),
            cols.next(),
        ) && let s = secs.trim()
            && !s.is_empty()
            && s.bytes().all(|b| b.is_ascii_digit())
            && let Ok(secs) = s.parse::<u64>()
        {
            runs.entry(step).or_default().push(secs);
        }
    }
    runs.into_iter()
        .map(|(step, mut all)| {
            let start = all.len().saturating_sub(5);
            let last = &mut all[start..];
            last.sort_unstable();
            let upper_median = last[last.len() / 2];
            (step.to_string(), upper_median)
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

/// Why [`Backend::cut_journal_read`] read no cut journal from a `dist/cut-state.toml` that is
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutJournalUnread {
    /// The read of a regular file failed (I/O, permissions), or the path could not be looked
    /// at; a retry may get through.
    Read(String),
    /// The path is no cut journal this cutter reads (no regular file: a directory, a FIFO, a
    /// symbolic link; not UTF-8, larger than any journal, or not parsing to a version, a build
    /// and a claim commit); every retry reads the same.
    Invalid(String),
}

/// The published release's shape checked, its website verified live and its
/// canonical tap aligned to the same validated website cohort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterCut {
    /// The step's line.
    pub note: String,
}

// Older journals recorded an unfinished website as a completed release.
// They must resume the website step before they can become complete.
const SITE_DEFERRED: &str = "site-deferred";

// Older after-cut facts cover the website only. They cannot authorize skipping
// the canonical tap operation after a resume.
const TAP_ALIGNED: &str = "tap-aligned";

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

/// The publication engine as a PREPARED release's visible half finds it, against the engine that staged R ([`Backend::engine_now`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineNow {
    /// At its origin/main, with the code that staged: what moved since is only its records (the ledger, the audit log, handoff records, which every stage and publish append to), its Rust port or its prose, which the engine's own stage guard sets aside too (`engine_code_drift`).
    Same(String),
    /// At its origin/main, and its code moved since it staged R.
    Moved(String),
    /// Not at its origin/main: `pub stage` and `pub publish` refuse such an engine.
    Off(String),
}

/// A judge's answer: yes or no, in its own words — or no answer at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub yes: bool,
    pub words: String,
    /// The judge could not answer (its store unreadable, its prover unnamable): nothing is known.
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

/// The facts every skip rule reads, read after the commit step and again after each step that changes one, never re-derived per question.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    pub cut: Option<CutState>,
    pub engine: EngineRows,
    /// A promotable attestation of R in `publish/.out`.
    pub attested: bool,
    /// The engine commit that attestation names (`engine.commit`): the engine that staged R.
    pub staged_by: Option<String>,
    /// The journal's bundle: COMPLETE in the release cache, its bytes re-proved, built by the release's prover.
    pub built: Answer,
    /// Each check's record of R's tree under this run's seal, the bundle's on that bundle's executable ([`crate::check_record::checks`]): complete, or what is missing and what FAILED.
    pub checks: crate::check_record::Checks,
    /// The release's own handoff fixtures on origin/main, as the next cut's gate judges them (`fixtures::on_commit`).
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
    /// `--fix`'s admitted fixes, before step 3 carries them onto the line's head (`commit`).
    pub carry: Vec<crate::line::Fix>,
}

/// What a step's child said when it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub code: i32,
    /// The lines that say why (a log tail, a check's summary lines).
    pub detail: Vec<String>,
}

/// What the build branch answered: the bundle its `ship build` child made, then the release's
/// own handoff fixtures staged after it (each `None`: not asked for).
#[derive(Debug)]
pub struct Built {
    pub artifact: Option<Result<Artifact, Refusal>>,
    pub fixtures: Option<Result<String, Refusal>>,
}

/// Whether the export check may publish its snapshot to the staging repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageTarget {
    Remote,
    Local,
}

/// One branch of step 4's qualify graph ([`Run::qualify`]): R's tree judged by `verify release
/// --tree`, its bundle built by `ship build`, its export checked by `pub stage`, at once, each
/// in a child of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Branch {
    Tree,
    Build,
    Export,
}

const QUALIFY_APPKIT_ROW: &str = "qualify-appkit";
const QUALIFY_APPKIT_DEFAULT_SECS: u64 = 60;

impl Branch {
    /// Every branch, in the order the graph starts them.
    pub const ALL: [Branch; 3] = [Branch::Build, Branch::Tree, Branch::Export];

    /// Its name on the transcript and in its log's file name (`04-qualify-<name>.log`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Branch::Tree => "tree",
            Branch::Build => "build",
            Branch::Export => "export",
        }
    }

    /// Its row in `steps.tsv`.
    #[must_use]
    pub fn row(self) -> String {
        format!("{}-{}", Step::Qualify.name(), self.name())
    }

    const fn default_secs(self) -> u64 {
        match self {
            Branch::Tree => 1_560,
            Branch::Build => 840,
            Branch::Export => 460,
        }
    }
}

/// How a branch of the qualify graph ended, as the graph acts on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Passed,
    /// The candidate failed: the other branches stop, except a proven test-execution red's
    /// already running build, and the release refuses R after the branches end.
    Red,
    /// Nothing about the code was decided: the other branches finish, so their records survive.
    CouldNotRun,
    /// Another branch's red stopped it.
    Stopped,
}

impl Ended {
    /// Its word in a branch line and its `qualify-branch` event.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Ended::Passed => "passed",
            Ended::Red => "red",
            Ended::CouldNotRun => "could-not-run",
            Ended::Stopped => "stopped",
        }
    }
}

/// One branch of step 7 ([`Run::publish`]): R's source published by `pub publish`, and R's
/// verified bundle signed, notarized, packaged and uploaded to a private draft by
/// `ship cut --sign`, at once, each in a child of its own. Neither claims; the cut
/// (step 8) claims once both passed and publishes that draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    Source,
    Sign,
}

impl Side {
    /// Every side, in the order step 7 starts them.
    pub const ALL: [Side; 2] = [Side::Source, Side::Sign];

    /// Its name on the transcript, in its `publish-branch` events and in its log's file name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Side::Source => "source",
            Side::Sign => "sign",
        }
    }

    /// Its row in `steps.tsv`.
    #[must_use]
    pub fn row(self) -> String {
        format!("{}-{}", Step::Publish.name(), self.name())
    }

    const fn default_secs(self) -> u64 {
        match self {
            Side::Source => 180,
            Side::Sign => 600,
        }
    }
}

/// The variable naming [`PublishGate`]'s file to the pre-promote hook `pub publish` runs.
pub const PUBLISH_GATE_ENV: &str = "ATERM_RELEASE_PUBLISH_GATE";

/// Step 7's gate between the source's push and the sign's red: one word, written once and
/// whole by whichever comes first. The pre-promote hook, run by `pub publish` after every
/// engine gate and right before its push, takes [`PublishGate::push`]; the release, as the
/// private sign goes red, takes [`PublishGate::stop`]. Each then knows the other's word: a
/// push that won finishes, and a stop that won leaves the hook refusing, so nothing is pushed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishGate {
    path: PathBuf,
}

impl PublishGate {
    const PUSH: &'static str = "push";
    const STOP: &'static str = "stop";

    /// The gate at `path`.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Where it is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open it again: neither word stands. Step 7 does this before its branches start.
    /// # Errors
    /// The file cannot be removed.
    pub fn reset(&self) -> std::io::Result<()> {
        match std::fs::remove_file(self.path.with_extension("sign")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The engine may be waiting for the private draft's id after its source
    /// push. A sign refusal wakes that wait even when the push already won.
    fn sign_finished(&self, word: &str) -> std::io::Result<()> {
        crate::durable::write_atomic_durable(
            &self.path.with_extension("sign"),
            word.as_bytes(),
            0o600,
        )
    }

    /// The word that stands, if one does.
    /// # Errors
    /// The file cannot be read.
    pub fn word(&self) -> std::io::Result<Option<String>> {
        match crate::durable::read_regular_text(&self.path, 64) {
            Ok(text) => Ok(Some(text.trim().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The push is about to begin: true when it may (this took the gate, or the gate said push
    /// already), false when the release's stop came first.
    /// # Errors
    /// The gate cannot be written or read: then nothing may push.
    pub fn push(&self) -> std::io::Result<bool> {
        Ok(self.take(Self::PUSH)? == Self::PUSH)
    }

    /// The sign went red: true when this stopped the push before it began, false when the push
    /// began first (it finishes).
    /// # Errors
    /// The gate cannot be written or read: then nothing says the push did not begin.
    pub fn stop(&self) -> std::io::Result<bool> {
        Ok(self.take(Self::STOP)? == Self::STOP)
    }

    /// Write `word` unless a word stands, created whole (a hard link of a flushed temporary,
    /// which fails where the gate exists): the word that stands afterwards. Each take writes a
    /// temporary of its own (this process's id and a count of its takes), so no two takers, in
    /// one process or two, ever share or remove each other's.
    fn take(&self, word: &str) -> std::io::Result<String> {
        static TAKES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let take = TAKES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = self
            .path
            .file_name()
            .map_or_else(|| "gate".into(), |n| n.to_string_lossy().into_owned());
        let temp = dir.join(format!(".{name}.{}-{take}.{word}", std::process::id()));
        crate::durable::write_atomic_durable(&temp, word.as_bytes(), 0o644)?;
        let linked = std::fs::hard_link(&temp, &self.path);
        let _ = std::fs::remove_file(&temp);
        match linked {
            Ok(()) => Ok(word.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => self
                .word()?
                .ok_or_else(|| std::io::Error::other("the gate went away while it was read")),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
#[path = "release_private_draft_gate_tests.rs"]
mod private_draft_gate_tests;

/// A branch's stop, fired by the qualify graph once another branch goes red: its child's group
/// is then sent the branch's signal and, [`BranchStop::GRACE`] later, `SIGKILL`
/// ([`crate::interrupt::Running::enter_stoppable`]), and a branch not started yet never starts.
/// An export whose staging push began ([`BranchStop::push`]) is never stopped: the push finishes.
/// Copies share one stop.
#[derive(Clone, Debug, Default)]
pub struct BranchStop {
    state: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The order the graph's decisions are told in, shared by its branches' stops
    /// ([`BranchStop::beside`]): a start, a staging push begun and a branch's end are each
    /// decided and said under it ([`BranchStop::in_order`]), so their events read in the order
    /// they were decided.
    told: std::sync::Arc<std::sync::Mutex<()>>,
}

/// [`BranchStop`]'s word while its staging push runs; 0 while it runs, else the unix ms it fired.
const PUSHING: u64 = 1;

impl BranchStop {
    /// How long a stopped branch's group has between its signal and `SIGKILL`.
    pub const GRACE: std::time::Duration = std::time::Duration::from_secs(10);

    /// Another branch's stop in the same graph: its own state, decided and told in one order
    /// with this one.
    #[must_use]
    pub fn beside(&self) -> Self {
        Self {
            state: std::sync::Arc::default(),
            told: std::sync::Arc::clone(&self.told),
        }
    }

    /// Run `decide` (a decision and the event that tells it) in the graph's one order.
    pub fn in_order<T>(&self, decide: impl FnOnce() -> T) -> T {
        let _told = self
            .told
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        decide()
    }

    /// Stop the branch: true when this stopped it; false when its push began (it finishes) or
    /// it was stopped already.
    pub fn fire(&self) -> bool {
        let at = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
        .max(PUSHING + 1);
        self.state
            .compare_exchange(
                0,
                at,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }

    /// Whether the branch was stopped.
    #[must_use]
    pub fn fired(&self) -> bool {
        self.state.load(std::sync::atomic::Ordering::SeqCst) > PUSHING
    }

    /// How long ago it was stopped, by the wall clock; `None` while it was not.
    #[must_use]
    pub fn fired_for(&self) -> Option<std::time::Duration> {
        let at = self.state.load(std::sync::atomic::Ordering::SeqCst);
        (at > PUSHING).then(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH + std::time::Duration::from_millis(at))
                .unwrap_or_default()
        })
    }

    /// The export's staging push begins: false when the branch was stopped first, and the push
    /// must not begin. Once it began, [`BranchStop::fire`] leaves the branch to finish.
    pub fn push(&self) -> bool {
        self.state
            .compare_exchange(
                0,
                PUSHING,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }

    /// Whether its staging push began.
    #[must_use]
    pub fn pushing(&self) -> bool {
        self.state.load(std::sync::atomic::Ordering::SeqCst) == PUSHING
    }
}

/// The part of the backend the qualify graph's branches run on, each from a thread of its own
/// ([`Run::qualify`]); `&self` throughout, so they run at once. Each branch's work is a child
/// process its stop reaches ([`BranchStop`]).
pub trait Branches: Sync {
    /// An executing test failed after compilation, with no other tree red. Only a build that
    /// already started may finish and retain its product for the next hop.
    fn keep_build_on_test_red(&self, _c: &Candidate) -> bool {
        false
    }
    fn say(&self, line: &str);
    fn now(&self) -> u64;
    /// Whether a person's signal reached the release ([`Backend::interrupted`]): it reaches
    /// every branch's group by itself, so the graph adds no stop of its own to it.
    fn interrupted(&self) -> bool;
    /// The log a branch's child writes.
    fn branch_log(&self, branch: Branch) -> PathBuf;
    /// Tell the build branch that this graph will run the tree compile which publishes its
    /// content-checked test-executable index. Backends without a separate fixture consumer need
    /// no state of their own.
    fn expect_tree_index(&self) {}
    /// Return once the release slices' one shared, serial or cold-learning parent holds memory,
    /// their successful all-Fresh bundle has already completed, or `ended` says the build ended.
    /// The measured critical path gets memory before the tree's moldable compile.
    fn build_admitted(&self, ended: &dyn Fn() -> bool);
    /// Return once the pooled release tree holds or queues its exact whole-stage Tippy claim, or
    /// `ended` says the build or tree ended. False means an exact production label carried a
    /// malformed envelope, so the graph must stop the tree before releasing its CPU barrier.
    /// Backends without a shared compiler pool have no admission barrier.
    fn tree_admitted(&self, _ended: &dyn Fn() -> bool) -> bool {
        true
    }
    /// Release the compiler-token barrier held while the build and tree reserve memory. This is
    /// idempotent, so every non-coordinated and early-ending graph path may call it.
    fn release_admission_barrier(&self) {}
    /// The export check of R: `pub stage` to the staging tier (`Remote`) or without its push
    /// (`Local`); its push begun is [`BranchStop::push`].
    fn export(
        &self,
        c: &Candidate,
        prover: &Prover,
        target: StageTarget,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal>;
    /// Build R's bundle under `reserved` (when `bundle`), then stage its handoff fixtures (when
    /// `fixtures`).
    fn build(
        &self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
        work: BuildWork,
        stop: &BranchStop,
    ) -> Built;
    /// Run `verify release` of `run`'s half in `step` at R — the checks it names, with the test
    /// binaries that passed — on `artifact` only where a named check reads the bundle, then keep
    /// one record per check it finished ([`crate::check_record::write_run`]).
    fn verify(
        &self,
        step: Step,
        c: &Candidate,
        prover: &Prover,
        run: &crate::check_record::VerifyRun,
        artifact: Option<&Artifact>,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal>;
    /// The log step 7's `side` writes.
    fn side_log(&self, side: Side) -> PathBuf;
    /// Step 7's gate between the source's push and the sign's red.
    fn publish_gate(&self) -> PublishGate;
    /// `pub publish` of R ([`Backend::publish_begin`] readied it), its pre-promote hook pushing
    /// only once it took `gate` ([`PublishGate::push`]).
    fn source(
        &self,
        prover: &Prover,
        gate: &PublishGate,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal>;
    /// `ship cut --sign` and the rest of `args`, in the clone: R's verified bundle signed,
    /// notarized and packaged privately under its reservation ([`crate::publish::run_cut_sign`]).
    fn sign(&self, args: &[String], prover: &Prover, stop: &BranchStop) -> Ran;
}

/// What the build branch makes: R's bundle, its handoff fixtures, or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildWork {
    pub bundle: bool,
    pub fixtures: bool,
}

/// An already-started private packaging child. Its stop also follows it into step 7's graph.
/// Dropping an unfinished child must stop and reap it.
pub trait PendingSign: Send {
    fn stop(&self) -> BranchStop;
    fn finish(self: Box<Self>) -> Ran;
}

/// What step 7 reads of R's source publication ([`Backend::source_publication`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    /// R's release carries its source attestation pair, each asset uploaded once.
    Attested,
    /// R's source is recorded and its attestation waits on the private draft, as said: no draft
    /// id is journaled (GitHub's lookup by tag serves no draft), or the journaled draft lacks the
    /// pair. The sign makes or adopts the draft and journals its id; the source signing then
    /// attaches the pair to exactly that draft ([`Backend::attest_draft`]).
    Pending(String),
}

/// What proves a privately signed copy may be removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignedDiscard {
    /// Stop unused preparation; a journaled draft still owns its signed bytes.
    CancelPreparation,
    /// The reservation was spent elsewhere, so this signed copy cannot ship.
    LostReservation,
}

/// What [`Backend::discard_signed`] left of a privately signed copy.
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub enum Discarded {
    /// Everything went but what is named, each a removal a retry may get through (a GitHub read
    /// or DELETE, a file); empty when nothing is left.
    Except(Vec<String>),
    /// Nothing went: the private draft is held, decided, and a retry decides the same. It is not
    /// one this clone can prove its own and unpublished, or what its create POST made cannot be
    /// seen; a person does what `remedy` says ([`crate::publish::discard_signed`]).
    Held { why: String, remedy: String },
}

impl Discarded {
    /// Nothing is left.
    #[must_use]
    pub fn complete(&self) -> bool {
        matches!(self, Self::Except(left) if left.is_empty())
    }

    /// What is left, a clause each; a held draft's says why and what a person does.
    #[must_use]
    pub fn left(&self) -> Vec<String> {
        match self {
            Self::Except(left) => left.clone(),
            Self::Held { why, remedy } => vec![format!("{why} — {remedy}")],
        }
    }

    /// What became of the copy, completing "the copy … ".
    #[must_use]
    pub fn fate(&self) -> String {
        match self {
            Self::Except(left) if left.is_empty() => "was discarded".to_string(),
            Self::Except(left) => format!("was discarded but for {}", left.join("; ")),
            Self::Held { why, remedy } => {
                format!("was kept, its private draft held: {why} — {remedy}")
            }
        }
    }
}

/// Everything the release asks of the world — git, the engine, the records, the children, the clock, the owner's aterm.
pub trait Backend {
    /// State the selected base's B8 evidence without steering selection or refusing a cut.
    fn main_lane(&mut self, _base: &str) -> Option<String> {
        None
    }
    /// Best-effort preparation for the next cut, after this run's windowed checks ended.
    fn start_prefreeze(&mut self) {}
    fn prepare_sign(
        &mut self,
        args: &[String],
        prover: &Prover,
    ) -> Result<Box<dyn PendingSign>, Refusal>;
    fn now(&mut self) -> u64;
    fn hms(&mut self, epoch: u64) -> String;
    fn sleep(&mut self, secs: u64);
    fn say(&mut self, line: &str);
    fn interrupted(&mut self) -> Option<i32>;
    fn entered(&mut self, _step: Step) {}
    /// Claim the version across the fleet, continuing this process's exact existing candidate
    /// line after an image exec, crossing the authenticated pre-B1 handed-parent boundary, or
    /// adopting a stale one by the fleet protocol.
    fn fleet_line(
        &mut self,
        line: &crate::line::Line,
        _continued: bool,
    ) -> Result<crate::line::Line, String> {
        Ok(line.clone())
    }
    /// Read the existing version line before judging a freshly selected main tip.
    fn fleet_existing(&mut self, _version: &str) -> Result<Option<crate::line::Line>, Refusal> {
        Ok(None)
    }
    /// Holder evidence retained by the existing claim read or acquisition; no network read.
    fn fleet_holder(&mut self, _head: &str) -> Option<String> {
        None
    }
    /// Local commits reachable from main but not head, and the subset changing cutter/check code.
    fn main_past_line(&mut self, _head: &str, _main: &str) -> Option<(usize, usize)> {
        None
    }
    fn fleet_supersession(
        &mut self,
        _version: &str,
    ) -> Result<Option<crate::line::Supersession>, Refusal> {
        Ok(None)
    }
    /// Read every public surface before a replacement changes local selection.
    fn preflight_supersede(
        &mut self,
        line: &crate::line::Line,
        _expected_previous: Option<&crate::line::Line>,
    ) -> Result<crate::line::Line, Refusal> {
        Ok(line.clone())
    }
    /// Classify a candidate retained after an ambiguous source write.
    fn classify_supersede(
        &mut self,
        _previous: &crate::line::Line,
        _line: &crate::line::Line,
    ) -> SupersedeResult {
        SupersedeResult::Unknown("supersession recovery is unavailable".into())
    }
    fn supersede_line(
        &mut self,
        _previous: &crate::line::Line,
        _line: &crate::line::Line,
    ) -> SupersedeResult {
        SupersedeResult::Unknown("supersession is unavailable".into())
    }
    /// Durably update the candidate after an authoritative source-write result. Tests inject
    /// only the write result; production uses the journal's ordinary durable writer.
    fn save_supersede_journal(&mut self, journal: &Journal, path: &Path) -> Result<(), String> {
        journal.save(path)
    }
    fn supersession_completed(&mut self) {}
    /// Record a step boundary and detect an adopted lease before starting more work.
    fn fleet_boundary(&mut self, _line: &crate::line::Line, _step: Step) -> Result<(), String> {
        Ok(())
    }
    /// The publication fence is entered only by the fleet line's current holder.
    fn fleet_audit(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// Record bounded operational observations before any release preparation.
    fn observe_operational(&mut self) {}
    fn log_path(&mut self, step: Step) -> PathBuf;
    fn me(&mut self) -> (u32, String);
    fn started_of(&mut self, pid: u32) -> Option<String>;
    fn release_dir(&mut self) -> PathBuf;
    fn times(&mut self) -> String;
    /// Keep `row`'s time (a step's name, or a qualify branch's [`Branch::row`]) in `steps.tsv`.
    fn record_time(&mut self, version: &str, row: &str, secs: u64);

    fn prover(&mut self, journaled: Option<&str>) -> Result<Prover, String>;
    fn lease(&mut self, prover: &Prover) -> Result<(), String>;
    /// Take or adopt the release's one build-inputs seal, under the toolchain lease, and keep it
    /// for every later step and child ([`crate::build_inputs::arm`]); never in a dry run.
    fn seal_inputs(&mut self) -> Result<(), String>;
    fn cleared_env(&mut self) -> Vec<String>;
    fn machine(&mut self, creds: &Path, explicit: bool) -> (String, Vec<Refusal>);
    fn fetch(&mut self) -> Result<String, Refusal>;
    /// Local safety check before selecting and freezing a candidate. No network or engine
    /// discovery belongs here: the selected cutter performs those judgments at step 1.
    fn prepare_clone(&mut self) -> Result<(), Refusal> {
        Ok(())
    }
    fn head(&mut self) -> String;
    fn branch_is_main(&mut self) -> bool;
    fn rev(&mut self, rev: &str) -> Option<String>;
    fn version_at(&mut self, rev: &str) -> Option<String>;
    fn is_ancestor(&mut self, a: &str, b: &str) -> bool;
    fn origin_tag(&mut self, tag: &str) -> Option<String>;
    fn origin_tag_now(&mut self, tag: &str) -> Result<Option<String>, String>;
    fn local_tag(&mut self, tag: &str) -> bool;
    /// Judge source changes not covered by landing evidence before any qualification work:
    /// what it found, whose remedy the caller names ([`InputFinding`]).
    fn input_checks(
        &mut self,
        _base: &str,
        _tip: &str,
        _prover: &Prover,
    ) -> Result<String, InputFinding> {
        Ok("release input checked".into())
    }
    fn ledger_versions(&mut self, rev: &str) -> Vec<String>;
    fn engine_rows(&mut self, commit: Option<&str>) -> EngineRows;
    fn engine_now(&mut self, staged: &str) -> Result<EngineNow, String>;
    fn candidate_missing(&mut self) -> Result<Option<String>, String>;
    /// The cut journal (`dist/cut-state.toml`), or why one is there that cannot be read.
    fn cut_journal_read(&mut self) -> Result<Option<CutState>, CutJournalUnread>;
    /// [`Backend::cut_journal_read`], a journal that cannot be read taken for none.
    fn cut_journal(&mut self) -> Option<CutState> {
        self.cut_journal_read().ok().flatten()
    }
    fn is_dir(&mut self, path: &Path) -> bool;
    /// `rev`'s tree holds the file `path` ([`crate::line::tree_has`]); COULD NOT RUN when git
    /// cannot say.
    fn tree_has(&mut self, rev: &str, path: &str) -> Result<bool, Refusal>;
    /// What `--fix <rev>` names for `line` ([`crate::line::admit`] on this clone, after
    /// [`crate::line::git_ready`]); it reads and changes nothing.
    fn fix_of(
        &mut self,
        line: &crate::line::Line,
        rev: &str,
    ) -> Result<crate::line::Admitted, Refusal>;
    /// Construct local hops in ancestry order. This does not read or write any remote.
    fn carry(
        &mut self,
        line: &crate::line::Line,
        fixes: &[crate::line::Fix],
    ) -> Result<(Vec<crate::line::Hop>, String), Refusal>;
    /// Record the proved, unpublished line on main through a content-neutral merge. Only the
    /// frozen head's cutter calls this, after its fresh publication and claim checks.
    fn record_line(&mut self, _line: &crate::line::Line) -> Result<String, Refusal> {
        Ok("line recorded".into())
    }
    /// The line's meaning check ([`crate::line::verify`]), which writes no ref: its summary.
    fn verify_line(&mut self, line: &crate::line::Line) -> Result<String, Refusal>;
    /// Read-only main-commit hints for a qualify failure, using its diagnostic evidence.
    fn failure_hints(
        &mut self,
        line: &crate::line::Line,
        evidence: &str,
        logs: &[String],
    ) -> Vec<String>;
    fn verify_pending_line(&mut self, line: &crate::line::Line) -> Result<String, Refusal> {
        self.verify_line(line)
    }

    fn sync(&mut self) -> Result<String, Refusal>;
    /// Construct an unreferenced bump commit without moving source refs or journaling.
    fn preview_bump(&mut self, c: &Candidate, prover: &Prover) -> Result<String, Refusal>;
    fn bump(&mut self, from: &str, to: &str, prover: &Prover) -> Result<String, Refusal>;
    fn move_to(&mut self, commit: &str) -> Result<(), Refusal>;
    /// Rewind only a clean, structurally proved, locally generated line to its newest source
    /// prefix before adopting another fleet line, refusing, or forgetting it. No owner-authored
    /// commit is discarded. It restores rather than starts work: the release's own stop (its
    /// fleet line lost, `interrupt::stop_self`) leaves it free to run, while a signal from
    /// elsewhere refuses it.
    fn rewind_selection(&mut self, _line: &crate::line::Line) -> Result<(), Refusal> {
        Ok(())
    }
    /// Provision the selected checkout's tools before staging or compiling the product.
    fn validation_tools(&mut self) -> Result<String, Refusal>;
    fn own_cutter(&mut self, commit: &str) -> Result<String, crate::cutter::NotOwn>;
    fn frozen_commit(&mut self) -> Option<String> {
        None
    }
    fn cutter_recovery(&mut self, _commit: &str) -> crate::release_tools::Recovery {
        crate::release_tools::Recovery::Initial
    }
    /// Build, freeze and prove R's tools, then replace this image with its cutter. A real
    /// success never returns; `Ok(())` is only a model backend's representation of that edge.
    fn exec_cutter(&mut self, commit: &str, prover: &Prover) -> Result<(), Refusal>;
    /// Start the verifier build after step 1; validation_tools joins and freezes it before
    /// qualification. A refusal or interrupt owns and settles the outstanding build.
    fn warm_tools(&mut self, _commit: &str, _prover: &Prover) -> Result<(), Refusal> {
        Ok(())
    }
    /// Settle preparation children before the run releases the clone claim, including after a
    /// panic. Another run must never move this clone while its old verifier still compiles.
    fn stop_tools(&mut self) {}
    fn facts(&mut self, c: &Candidate, prover: &Prover, artifact: Option<&Artifact>) -> Facts;
    fn disk(&mut self) -> (Option<u64>, bool);
    /// What this machine's last releases wrote, by their own records
    /// ([`crate::release_disk::RECORD`]).
    fn disk_record(&mut self) -> aterm_capacity::Footprints;
    /// Claim `bytes` of the clone's volume for the release `why` in the shared ledger, waiting
    /// for room: held until [`Backend::let_disk_go`], and naming itself to every child the
    /// release starts, which then claims and walks nothing ([`aterm_capacity::DISK_COVER_ENV`]).
    fn claim_disk(&mut self, bytes: u64, why: &str) -> Result<(), Refusal>;
    /// End the disk claim the release holds, if any (step 4's, or a stage's); where `passed`,
    /// this run's verify passed, and what the run wrote is kept for the next release's claim.
    fn let_disk_go(&mut self, passed: bool);
    /// R's commit-only preflight of `c`: each gate's refusal, and what a passing gate says.
    fn commit_gates(&mut self, c: &Candidate, dry: bool) -> Preflight;
    /// Whether this image can soundly judge this commit before its frozen handover.
    /// An older image must defer to the selected commit's rules.
    fn can_check_commit(&mut self, _c: &Candidate) -> bool {
        false
    }
    /// The cut's read-only pre-claim gates of this machine, its credentials and the channel.
    fn cut_gates(
        &mut self,
        c: &Candidate,
        creds: &Path,
        kind: crate::publish::CutKind,
    ) -> Vec<Refusal>;
    /// The cut's pre-claim gates of R's own tree: its handoff fixtures and handoff policy.
    fn head_gates(&mut self, c: &Candidate) -> Vec<Refusal>;
    fn land_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> Result<String, Refusal>;
    /// What this machine lacks to cut. A dry run's audit writes nothing; a real run's may
    /// re-assert what the cut it gates writes anyway (the release keychain's partition list),
    /// so its test signature runs before the publish. An explicitly selected credentials
    /// profile is the one audited and authorized against the current roster.
    fn audit(&mut self, machine_id: &str, profile: Option<&Path>, dry: bool) -> Vec<Finding>;
    fn ask(&mut self, words: &AskWords) -> Result<String, String>;
    fn resolve(&mut self, key: &str) -> Result<String, String>;
    fn withdraw(&mut self, key: &str) -> Result<String, String>;
    /// Open the repair worker of a real cut's red, once per failing input hash, in the release
    /// clone, its sid kept in the release directory's [`aterm_release::repair::RECORD_FILE`]
    /// ([`aterm_release::repair::open`]): the lines the cut says of it.
    fn repair(&mut self, failure: &aterm_release::repair::Failure) -> Vec<String>;
    /// One non-waiting look at the bundle build's exact slice-memory plans, through the same
    /// retained book, capacity ledger and machine reader as qualify.
    fn qualify_slice_memory(&mut self) -> Result<crate::buildplan::SliceMemoryForecast, String>;
    fn origin_tail(&mut self) -> Option<u64>;
    fn reserve(&mut self, c: &Candidate) -> Result<Reservation, Refusal>;
    /// Before the qualify graph starts: admit its one core claim and make the pool every branch
    /// draws from, partitioned only among the branches this plan runs, and ready the export's
    /// engine and scanner tools when it runs; its words.
    fn qualify_begin(&mut self, prover: &Prover, branches: &[Branch]) -> Result<String, Refusal>;
    /// The qualify graph ended: its pool and its core claim go.
    fn qualify_end(&mut self);
    /// What the qualify graph's branches run on.
    fn branches(&mut self) -> Box<dyn Branches + '_>;
    /// A stage of R outside the graph (again, right before the publish): the export, then the
    /// engine synced.
    fn stage(
        &mut self,
        c: &Candidate,
        prover: &Prover,
        target: StageTarget,
    ) -> Result<Ran, Refusal>;
    /// Ready step 7's `pub publish` before its branches start: the engine synced, its scanner
    /// tools and the source's signing tools prepared. The sign branch needs none of them.
    fn publish_begin(&mut self, prover: &Prover) -> Result<(), Refusal>;
    /// R's source publication as GitHub serves it: the private draft the clone journals for
    /// build `build` of R's version, read by its id, else R's published release, read by its tag
    /// (a draft journaled for another build is not R's). A refusal names what is wrong with it,
    /// a journaled draft GitHub no longer answers included; an answer that cannot be read is
    /// COULD NOT RUN.
    fn source_publication(&mut self, c: &Candidate, build: u64) -> Result<SourceState, Refusal>;
    /// After a sign that passed: the engine's source signing of R onto the private draft its
    /// journal names for build `build` (`pub sign-release aterm --tag v… --keep-draft
    /// --draft-state …`), logged into step 7's log; what ran. A journal that names no draft of
    /// that build refuses, and nothing runs.
    fn attest_draft(
        &mut self,
        c: &Candidate,
        build: u64,
        prover: &Prover,
    ) -> Result<String, Refusal>;
    fn cut(
        &mut self,
        args: &[String],
        prover: &Prover,
        candidate: &Candidate,
        reserved: Option<&Reservation>,
    ) -> Ran;
    /// Discard the copy step 7 signed of build `build` of `version` from the clone's `dist/`
    /// ([`crate::publish::discard_signed`]); what it left, and whether a retry may remove it.
    fn discard_signed(&mut self, version: &str, build: u64, reason: SignedDiscard) -> Discarded;
    fn after_cut(&mut self, version: &str) -> Result<AfterCut, Refusal>;
    /// Distribution and the main reader consume the public release independently.
    /// Both join before returning; each successful half keeps its own completion.
    fn after_cut_with_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> (Result<AfterCut, Refusal>, Result<String, Refusal>);
    /// The channel head tag. Transport errors are distinct from a successful response whose
    /// document names no tag, so step 10 can report only the former as COULD NOT RUN.
    fn latest(&mut self) -> Result<Option<String>, String>;
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    /// The final line.
    pub last: String,
}

enum Halt {
    /// The final line is said; the exit code.
    Said(i32),
}

type Flow = Result<(), Halt>;

/// Why a boundary ends the run whatever its step decided.
enum Stop {
    /// Another holder adopted the fleet line ([`crate::line_claim::failure`]).
    FleetLost(String),
    /// The signal this process caught.
    Signal(i32),
}

struct Spent {
    reservation: Reservation,
    /// Origin's ledger tail.
    tail: u64,
    /// Origin/main holds the reservation's release commit: this clone's claim of it landed, and no cut journal holds it.
    claimed_here: bool,
}

impl Spent {
    fn words(&self) -> String {
        let r = &self.reservation;
        if self.claimed_here {
            // Step 7 may have signed a private draft under it, which the release then withdraws
            // (Run::withdraw_signed): only publication is ruled out.
            format!(
                "its reserved build {} was CLAIMED from this clone — origin/main holds its \
                 release commit {} — but no cut journal holds the claim (the cut stopped \
                 between its push and its journal), so nothing was published under it: build \
                 {} is spent, a gap in the ledger",
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
    line_selection: Option<LineSelection>,
    journal: Option<Journal>,
    claim: Option<RunClaim>,
    /// Why this run could not claim the clone ([`Run::take_claim`]): its checks refuse with it.
    claim_refused: Option<Refusal>,
    cur: Step,
    cur_t0: u64,
    t_start: u64,
    /// A dry run's refusals, `n/N step: why`.
    would: Vec<String>,
    /// A dry run's owner waits, each gap once, and the step a real run would first wait at.
    waits: Vec<String>,
    wait_at: Option<Step>,
    /// A dry run's resource waits. Unlike an unclaimed normal-pressure shortage, these have no
    /// bounded refusal in qualify: a claim or pressure must change before admission continues.
    dry_resource_waits: Vec<String>,
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
    /// Set when the stage runs again: the engine moved since it staged R (found by [`Run::reask_prepared`], or right before any publish by [`Run::engine_still_staged`], where a moved staging tier also stages again).
    restage: Option<(Option<String>, String)>,
    /// Whether this run holds a disk claim ([`Run::claim_disk`]), step 4's or a stage's, or in a
    /// dry run has said it would.
    disk_held: bool,
    /// `--qualify`'s local export check passed in this run, so a second round of step 4 (a
    /// spent reservation) does not check it again.
    exported_locally: bool,
    /// Owner asks the checks delivered before this run had a journal (a first run): journaled with the candidate at step 3, so they are not raised again.
    early_asks: Vec<String>,
    /// Seconds the current step waited for the owner (not its own time).
    waited: u64,
    /// A dry run audits once.
    dry_audit: Option<Vec<Finding>>,
    last: String,
    early_sign: Option<Box<dyn PendingSign>>,
    /// The candidate-only explicit supersession transaction, if one is being selected or
    /// recovered.
    supersede: Option<SupersedeTxn>,
    dry_unavailable: Vec<String>,
}

/// Run a release with `b`.
pub fn run_with(b: &mut dyn Backend, opts: &ReleaseOptions, home: &Path) -> Outcome {
    let release_dir = b.release_dir();
    let creds = opts
        .release_credentials
        .clone()
        .unwrap_or_else(|| home.join(atpkg_keys::provision::RELEASE_CREDENTIALS_REL));
    let mut run = Run {
        b,
        opts: opts.clone(),
        dry: opts.dry_run,
        journal_path: release_dir.join(JOURNAL_FILE),
        line_selection: None,
        journal: None,
        claim: None,
        claim_refused: None,
        cur: Step::Checks,
        cur_t0: 0,
        t_start: 0,
        would: Vec::new(),
        waits: Vec::new(),
        wait_at: None,
        dry_resource_waits: Vec::new(),
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
        disk_held: false,
        exported_locally: false,
        early_asks: Vec::new(),
        waited: 0,
        dry_audit: None,
        last: String::new(),
        early_sign: None,
        supersede: None,
        dry_unavailable: Vec::new(),
    };
    let went = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run.go()));
    let code = match went {
        Ok(Ok(())) => 0,
        Ok(Err(Halt::Said(code))) => code,
        Err(_) => {
            let step = run.cur;
            let (state, code) = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run.settle_panicked_line()
            })) {
                Ok(Ok(Some(state))) => (format!("; {state}"), 1),
                Ok(Ok(None)) => (String::new(), 1),
                Ok(Err(refusal)) => (
                    format!(
                        "; restoring the release clone also failed: {} — {}",
                        refusal.why, refusal.remedy
                    ),
                    3,
                ),
                Err(_) => (
                    format!(
                        "; recovery also stopped unexpectedly; no release journal was \
                         intentionally removed — inspect main and {}, then run it again",
                        release_dir.join(SELECTION_FILE).display()
                    ),
                    3,
                ),
            };
            let log = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run.b.log_path(step).display().to_string()
            }))
            .unwrap_or_else(|_| "the current step log".into());
            run.last = format!(
                "REFUSED at {}: {RELEASE_COMMAND} stopped unexpectedly — {}. Read {}, then \
                 run it again to resume{state}",
                step.at(),
                run.public,
                log
            );
            run.withdraw_early_asks();
            let last = run.last.clone();
            run.b.say(&last);
            code
        }
    };
    run.discard_early_sign();
    // A failed run does not invalidate its private draft. It can already hold
    // the source engine's signed evidence; retain its journal for adoption.
    // The claim's lost-reservation path handles copies that can no longer ship.
    run.b.stop_tools();
    Outcome {
        code,
        last: run.last,
    }
}

impl Run<'_> {
    /// Dry supersession judgment precedes step 1's broad source/engine fetch, just as the real
    /// launcher's judgment precedes any local selection write.
    fn preflight_dry_supersede(&mut self) -> Flow {
        if !self.opts.supersede {
            return Ok(());
        }
        if let Some(txn) = self
            .supersede
            .clone()
            .filter(|transaction| transaction.cas_unknown)
        {
            match self.b.classify_supersede(&txn.previous, &txn.replacement) {
                SupersedeResult::Committed(note) => {
                    self.say(&note);
                    self.supersede = None;
                    if let Some(journal) = self.journal.as_mut() {
                        journal.supersede = None;
                        journal.add(SUPERSEDED_LINE);
                    }
                    self.opts.supersede = false;
                    return Ok(());
                }
                SupersedeResult::Refused {
                    why,
                    winner: Some(winner),
                    ..
                } => {
                    return self.refuse_hard(Refusal::new(
                        why,
                        format!(
                            "run the same command without --dry-run to adopt the authoritative \
                             line at {}",
                            short(winner.head())
                        ),
                    ));
                }
                SupersedeResult::Refused {
                    retry: true,
                    winner: None,
                    ..
                } => {}
                SupersedeResult::Refused { why, .. } => {
                    return self.refuse_hard(Refusal::new(why, "resume the authoritative line"));
                }
                SupersedeResult::Unknown(why) => {
                    let message = format!(
                        "COULD NOT RUN: the supersession write is still unknown: {why}; \
                         candidate.toml retains {} for an exact retry",
                        short(txn.replacement.head())
                    );
                    self.say(&message);
                    self.last = message;
                    return Err(Halt::Said(3));
                }
            }
        }
        self.choose_candidate(true)?;
        // prepare_candidate's guard: a rerun at the head whose supersession is written resumes
        // its line and preflights no supersession, so neither does its dry run.
        if self.supersession_written() {
            return Ok(());
        }
        let line = self.supersede.as_ref().map_or_else(
            || crate::line::Line::start(&self.version(), &self.commit_r(), self.t_start),
            |txn| txn.replacement.clone(),
        );
        let (_, expected) = self.supersede_release_lines()?;
        self.b
            .preflight_supersede(&line, expected.as_ref())
            .map(|_| ())
            .map_err(|refusal| self.halted(refusal))
    }

    fn go(&mut self) -> Flow {
        if self.opts.forget {
            return self.forget();
        }
        self.take_claim();
        let selection = self.b.release_dir().join(SELECTION_FILE);
        self.journal = match Journal::load(&selection).and_then(|pending| {
            pending.map_or_else(|| Journal::load(&self.journal_path), |j| Ok(Some(j)))
        }) {
            Ok(j) => j,
            Err(why) => {
                return self.final_refusal(Refusal::new(
                    why,
                    format!("move it aside, or drop it: {RELEASE_COMMAND} --forget"),
                ));
            }
        };
        self.supersede = self
            .journal
            .as_ref()
            .and_then(|journal| journal.supersede.clone());
        if !self.claim.as_ref().is_some_and(|claim| claim.continued)
            && self.opts.commit.is_none()
            && let Some(journal) = self.journal.as_ref()
        {
            self.line_selection = Some(LineSelection {
                head: journal.commit.clone(),
                source: format!(
                    "this clone's journal {}",
                    if selection.exists() {
                        selection.display().to_string()
                    } else {
                        self.journal_path.display().to_string()
                    }
                ),
                holder: None,
                announced: false,
                settled: false,
            });
        }
        if let Some(txn) = &self.supersede {
            let requested = self
                .opts
                .commit
                .as_deref()
                .and_then(|commit| self.b.rev(&format!("{commit}^{{commit}}")));
            if !self.opts.supersede || requested.as_deref() != Some(txn.replacement.head()) {
                let replacement = txn.replacement.head();
                return self.refuse_hard(
                    Refusal::new(
                        "candidate.toml retains an undecided supersession transaction",
                        format!(
                            "decide that exact transaction first: {RELEASE_COMMAND} --commit \
                             {replacement} --supersede"
                        ),
                    )
                    .could_not_run(),
                );
            }
        }
        self.t_start = self.b.now();
        self.cur_t0 = self.t_start;
        self.b.observe_operational();
        if self.dry {
            self.say(
                "release: DRY RUN — no release refs or journals change; local tool caches and Git objects may be written",
            );
            self.prepare_dry_candidate()?;
            self.preflight_dry_supersede()?;
        }

        if !self.dry {
            self.journal_path = selection;
            self.prepare_candidate()?;
        }

        self.begin(Step::Checks)?;
        let note = self.checks()?;
        self.finish(&note);

        if !self.dry {
            let prover = self.prover.clone().ok_or(Halt::Said(1))?;
            let commit = self.commit_r();
            if let Err(refusal) = self.b.warm_tools(&commit, &prover) {
                return self.refuse_hard(refusal);
            }
        }

        self.begin(Step::Sync)?;
        if self.dry {
            self.finish(
                "would bring the release's own engine to its origin/main (step 1 clones it \
                 where this clone has none; appended audit rows kept, unpushed records \
                 delivered) and run \
                 `git submodule update --init vendor/astream`",
            );
        } else {
            match self.b.sync() {
                Ok(note) => self.finish(&note),
                Err(r) => return self.refuse(r),
            }
        }

        self.begin(Step::Commit)?;
        self.journal_path = self.b.release_dir().join(JOURNAL_FILE);
        let note = self.commit(false)?;
        self.finish(&note);
        if !self.dry {
            let release_dir = self.b.release_dir();
            let selection = release_dir.join(SELECTION_FILE);
            match std::fs::remove_file(&selection) {
                Ok(()) => {
                    if let Err(error) = crate::durable::sync_dir(&release_dir) {
                        return self.refuse_hard(
                            Refusal::new(
                                format!(
                                    "the recorded release journal is current, but cannot make \
                                     candidate.toml removal durable: {error}"
                                ),
                                "make the release state directory writable and retry",
                            )
                            .could_not_run(),
                        );
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return self.refuse_hard(
                        Refusal::new(
                            format!(
                                "the recorded release journal is current, but cannot remove {}: \
                                 {error}",
                                selection.display()
                            ),
                            "make the release state directory writable and retry",
                        )
                        .could_not_run(),
                    );
                }
            }
        }
        // Selection and every image handover finished before step 1. Provision this
        // candidate's pins before qualification.
        if !self.dry && !self.journal.as_ref().is_some_and(Journal::finished) {
            match self.b.validation_tools() {
                Ok(note) => self.say(&format!("        validation tools: {note}")),
                Err(refusal) => self.refuse(refusal)?,
            }
        }

        self.refresh();
        if self.opts.qualify && self.facts.cut.is_some() {
            return self.refuse(Refusal::new(
                "a cut is already journaled; private qualification cannot reuse an active cut",
                "use a separate clean clone for the private candidate",
            ));
        }
        self.plan_print();

        // The qualify graph and the bundle half are all the release writes under its claim; a
        // stage after it claims for itself ([`Run::stage_claimed`]). Either step's refusal ends
        // the claim unpassed.
        let verified = self.qualify_and_bundle();
        self.let_disk_go(matches!(verified, Ok(true)));
        verified?;

        self.begin(Step::Ready)?;
        if let Some(why) = self.claimed_words() {
            self.skip(&why);
        } else {
            self.ready()?;
        }

        if self.opts.qualify {
            if self.dry {
                return self.dry_final();
            }
            self.let_go();
            self.last = format!(
                "QUALIFIED aterm {} from {} — its export checked locally and its bundle built \
                 and verified; no staging snapshot pushed, nothing published. \
                 {} cut --dry-run signs these verified bytes privately",
                self.version(),
                short(&self.commit_r()),
                crate::publish::SHIP_COMMAND
            );
            let last = self.last.clone();
            self.say(&last);
            return Err(Halt::Said(0));
        }
        if self.opts.prepare {
            self.prepare_stop()?;
        }

        if !self.dry {
            self.b.start_prefreeze();
        }

        self.begin(Step::Publish)?;
        match self.skip_publish() {
            // Published earlier and nothing claimed: the sign runs alone before the claim.
            Some(why) if matches!(self.cut_mode(), CutMode::Fresh) => self.publish(Some(why))?,
            Some(why) => self.skip(&why),
            None => self.publish(None)?,
        }

        self.begin(Step::Cut)?;
        match self.cut_mode() {
            CutMode::Done(why) => self.skip(&why),
            mode => self.cut(&mode)?,
        }

        self.begin(Step::AfterCut)?;
        let shaped = self.after_cut_complete();
        if shaped && self.facts.fixtures.yes {
            let v = self.version();
            let landed = self.facts.fixtures.words.clone();
            self.skip(&format!("done for v{v} (this release's journal); {landed}"));
        } else if self.dry {
            let v = self.version();
            let mut words = Vec::new();
            if !shaped {
                words.push(format!(
                    "would check v{v}'s shape as GitHub serves it, then bring alab.systems to \
                     it (ship site --latest) and align the canonical tap to the same validated \
                     site cohort (pub tap --bump --expect-version)"
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

    fn stage_event(&mut self, outcome: &str, detail: &str) {
        if self.dry {
            return;
        }
        let at = self.b.now();
        let elapsed = at.saturating_sub(self.cur_t0);
        let waiting = self.waited.min(elapsed);
        let log = self.b.log_path(self.cur);
        let log_val = log.to_str().map_or_else(
            || aterm_json::to_value(&log).expect("path"),
            |s| s.to_string().into(),
        );
        let event = aterm_json::Value::Object(aterm_json::Map::from([
            ("schema".to_string(), 1.into()),
            ("event".to_string(), "release-stage".into()),
            ("stage".to_string(), self.cur.name().into()),
            ("outcome".to_string(), outcome.into()),
            ("source".to_string(), self.commit_r().into()),
            ("version".to_string(), self.version().into()),
            ("at_unix_s".to_string(), at.into()),
            ("waiting_s".to_string(), waiting.into()),
            (
                "execution_s".to_string(),
                elapsed.saturating_sub(waiting).into(),
            ),
            ("log".to_string(), log_val),
            ("detail".to_string(), detail.into()),
            ("public".to_string(), self.public.clone().into()),
        ]));
        self.say(&format!("release-event: {event}"));
    }

    fn begin(&mut self, step: Step) -> Flow {
        if !self.dry
            && !self.opts.qualify
            && let Some(journal) = &self.journal
            && let Err(why) = self.b.fleet_boundary(&journal.line(), step)
        {
            return self.refuse(
                Refusal::new(why, "resume the adopted line on its holder").could_not_run(),
            );
        }
        self.cur = step;
        self.b.entered(step);
        self.waited = 0;
        self.cur_t0 = self.b.now();
        self.stage_event("started", "");
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

    /// What ends the run at a boundary whatever its step decided: the fleet line lost to
    /// another holder (which also signals this process, so it is asked first), else a signal.
    /// A private qualification holds no fleet line, so only a signal ends it.
    fn stop(&mut self) -> Option<Stop> {
        if !self.opts.qualify
            && let Some(why) = crate::line_claim::failure()
        {
            return Some(Stop::FleetLost(why.to_owned()));
        }
        self.b.interrupted().map(Stop::Signal)
    }

    fn halt_if_interrupted(&mut self) -> Flow {
        match self.stop() {
            None => Ok(()),
            Some(stop) => self.halt_at(stop),
        }
    }

    /// End the run on `stop`. Neither arm asks [`Run::stop`] again, so a refusal that meets a
    /// stop ends in exactly one final line.
    fn halt_at(&mut self, stop: Stop) -> Flow {
        let sig = match stop {
            Stop::FleetLost(why) => {
                let why = why.strip_prefix("COULD NOT RUN: ").unwrap_or(&why);
                let r =
                    Refusal::new(why, "resume on the fleet line's current holder").could_not_run();
                if self.dry {
                    self.note_dry_refusal(&r);
                    return self.dry_final();
                }
                return self.conclude(r);
            }
            Stop::Signal(sig) => sig,
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
        // The transcript's record of the step closes too, so no reader takes it as running.
        self.stage_event("INTERRUPTED", &last);
        self.say(&last);
        Err(Halt::Said(130))
    }

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
                let word = if !self.dry_unavailable.is_empty() {
                    "INCOMPLETE"
                } else if !self.would.is_empty() {
                    "REFUSALS"
                } else {
                    "ok"
                };
                self.say(&format!(
                    "[{:>2}/{}] {:<15} {word}    {at}  {}  {note}",
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
        self.stage_event("ok", note);
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
        self.b
            .record_time(&v, step.name(), secs.saturating_sub(self.waited));
    }

    fn skip(&mut self, why: &str) {
        self.stage_event("skip", why);
        let step = self.cur;
        self.say(&format!(
            "[{:>2}/{}] {:<15} skip  {why}",
            step.number(),
            Step::ALL.len(),
            step.name()
        ));
    }

    fn refuse(&mut self, mut r: Refusal) -> Flow {
        let step = self.cur;
        if step == Step::Qualify
            && !r.could_not_run
            && let Some(journal) = &self.journal
        {
            let logs = self
                .facts
                .checks
                .failed
                .iter()
                .map(|f| f.log.clone())
                .collect::<Vec<_>>();
            let evidence = format!("{}\n{}", r.why, r.detail.join("\n"));
            r.detail
                .extend(self.b.failure_hints(&journal.line(), &evidence, &logs));
        }
        if self.dry {
            self.note_dry_refusal(&r);
            return Ok(());
        }
        self.final_refusal(r)
    }

    /// A dry run notes what a real run would meet and goes on planning.
    fn note_dry_refusal(&mut self, r: &Refusal) {
        let step = self.cur;
        let word = if r.could_not_run {
            self.dry_unavailable
                .push(format!("{}: {}", step.at(), r.why));
            "COULD NOT RUN"
        } else {
            self.would.push(format!("{}: {}", step.at(), r.why));
            "WOULD REFUSE"
        };
        self.say(&format!(
            "[{:>2}/{}] {:<15} {word}  {}",
            step.number(),
            Step::ALL.len(),
            step.name(),
            r.why
        ));
        for line in &r.detail {
            self.b.say(&format!("        | {line}"));
        }
        self.say(&format!("        fix: {}", r.remedy));
    }

    fn refuse_hard(&mut self, r: Refusal) -> Flow {
        self.refuse(r)?;
        self.dry_final()
    }

    /// A stop that arrived while the step refused ends the run in the stop's line instead.
    fn final_refusal(&mut self, r: Refusal) -> Flow {
        match self.stop() {
            Some(stop) => self.halt_at(stop),
            None => self.conclude(r),
        }
    }

    /// Say a real run's refusal as its final line, after settling a private head it holds.
    /// It never asks [`Run::stop`]: [`Run::halt_at`] ends a lost fleet line through here.
    fn conclude(&mut self, r: Refusal) -> Flow {
        let r = self.settle_line_refusal(r);
        self.stage_event(if r.could_not_run { "NOT RUN" } else { "FAIL" }, &r.why);
        let step = self.cur;
        let now = self.b.now();
        let at = self.b.hms(now);
        let log = self.b.log_path(step);
        // A refusal before the step starts a child still owns a step log. Keep its native
        // diagnostic there before naming the path, rather than leaving the detail only in the
        // run transcript. Opening unconditionally also creates an empty log for a refusal with
        // no detail.
        let log_result = (|| -> std::io::Result<()> {
            if let Some(parent) = log.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = crate::durable::open_append(&log, true)?;
            for line in &r.detail {
                std::io::Write::write_all(&mut file, format!("{line}\n").as_bytes())?;
            }
            Ok(())
        })();
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
        if let Err(error) = log_result {
            self.b.say(&format!(
                "        | cannot write the named refusal log {}: {error}",
                log.display()
            ));
        }
        // A real run's refusal ends here, once. A red of a gate or build opens its repair
        // worker; whatever that says, this refusal is the run's end.
        if !self.dry
            && !r.could_not_run
            && let Some(gate) = r.failing.clone()
        {
            let prover = self.prover.as_ref();
            let failure = aterm_release::repair::Failure {
                gate,
                step: step.at(),
                log: log.clone(),
                why: r.why.clone(),
                detail: r.detail.clone(),
                remedy: r.remedy.clone(),
                version: self.version(),
                commit: self.commit_r(),
                prover: prover.map_or_else(|| "?".into(), |p| p.bin.display().to_string()),
                trustc: prover.map_or_else(|| "?".into(), |p| p.commit.clone()),
                at: now,
            };
            for line in self.b.repair(&failure) {
                self.b.say(&line);
            }
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

    /// Restore a clean clone from a journaled private head before a refusal lets go of it, the
    /// refusal a lost fleet line ends the run with included ([`Backend::rewind_selection`]). A
    /// pending candidate remains the exact retry key; a recorded line whose record merge main
    /// lost remains in release.toml until the operator explicitly forgets it.
    fn settle_line_refusal(&mut self, mut refusal: Refusal) -> Refusal {
        // A live competing run owns every checkout movement until its claim ends.
        if self.claim.is_none() {
            return refusal;
        }
        let Some(journal) = self.journal.clone().filter(|journal| {
            !journal.finished() && journal.supersede.is_none() && !journal.line().hops.is_empty()
        }) else {
            return refusal;
        };
        let line = journal.line();
        let pending = journal.has(PENDING_LINE);
        let recorded_lost = !pending && refusal.lost_line_record;
        if !pending && !recorded_lost {
            return refusal;
        }

        let clone_state = match self.settle_private_head(&line) {
            Ok(state) => state,
            Err(rewind) => {
                let journal_name = if pending {
                    SELECTION_FILE
                } else {
                    JOURNAL_FILE
                };
                let triggering_why = refusal.why.clone();
                let after = self.b.head();
                return Refusal::new(
                    format!(
                        "{}; restoring the release clone from its private head also failed: {}",
                        refusal.why, rewind.why
                    ),
                    format!(
                        "{}; {journal_name} still retains line {} and main is still at {}; {}",
                        rewind.remedy,
                        short(line.head()),
                        short(&after),
                        refusal.remedy
                    ),
                )
                .detail(
                    refusal
                        .detail
                        .into_iter()
                        .chain(rewind.detail)
                        .chain([format!("the triggering refusal was: {triggering_why}")]),
                )
                .could_not_run();
            }
        };
        refusal.remedy = if pending {
            format!(
                "{}; {clone_state}; {SELECTION_FILE} retains pending line {} — retry with \
                 {RELEASE_COMMAND}, or drop it with {RELEASE_COMMAND} --forget",
                refusal.remedy,
                short(line.head())
            )
        } else {
            format!(
                "{}; {clone_state}; {JOURNAL_FILE} retains lost line {} until --forget",
                refusal.remedy,
                short(line.head())
            )
        };
        refusal
    }

    /// Restore `line` only when this checkout is still at its exact private head. A checkout
    /// that has already moved is reported, never redirected.
    fn settle_private_head(&mut self, line: &crate::line::Line) -> Result<String, Refusal> {
        let before = self.b.head();
        if before != line.head() {
            return Ok(format!("clone main remains at {}", short(&before)));
        }
        self.b.rewind_selection(line)?;
        Ok(format!(
            "clone main was restored to its recorded prefix {}",
            short(&self.b.head())
        ))
    }

    /// An arbitrary panic may have changed the in-memory journal. Candidate.toml is the durable
    /// authority for deciding whether a private pending head needs restoration.
    fn settle_panicked_line(&mut self) -> Result<Option<String>, Refusal> {
        if self.claim.is_none() {
            return Ok(None);
        }
        let selection = self.b.release_dir().join(SELECTION_FILE);
        let journal = Journal::load(&selection).map_err(|why| {
            Refusal::new(
                why,
                format!(
                    "{SELECTION_FILE} was retained; inspect the clone's main, then retry or use \
                     {RELEASE_COMMAND} --forget"
                ),
            )
            .could_not_run()
        })?;
        let Some(journal) = journal.filter(|journal| {
            !journal.finished()
                && journal.has(PENDING_LINE)
                && journal.supersede.is_none()
                && !journal.line().hops.is_empty()
        }) else {
            return Ok(None);
        };
        let line = journal.line();
        self.settle_private_head(&line)
            .map(|state| {
                Some(format!(
                    "{state}; {SELECTION_FILE} retains pending line {} — retry with \
                     {RELEASE_COMMAND}, or drop it with {RELEASE_COMMAND} --forget",
                    short(line.head())
                ))
            })
            .map_err(|refusal| {
                Refusal::new(
                    refusal.why,
                    format!(
                        "{}; {SELECTION_FILE} retains pending line {} and main remains at {}",
                        refusal.remedy,
                        short(line.head()),
                        short(&self.b.head())
                    ),
                )
                .could_not_run()
            })
    }

    /// A fleet selection stop is not a [`Refusal`], but it owns the same pending-checkout
    /// invariant: leave no private head checked out after this run releases its clone claim.
    fn stop_pending_selection(&mut self, why: String, pending: Option<&crate::line::Line>) -> Flow {
        let code = if why.starts_with("CUTTING ELSEWHERE:") {
            4
        } else {
            3
        };
        let Some(line) = pending.filter(|line| !line.hops.is_empty()) else {
            self.say(&why);
            self.last = why;
            return Err(Halt::Said(code));
        };
        let before = self.b.head();
        let clone_state = if self.claim.is_some() {
            match self.settle_private_head(line) {
                Ok(state) => state,
                Err(refusal) => {
                    let after = self.b.head();
                    let message = format!(
                        "COULD NOT RUN: {why}; restoring the release clone also failed: {}; \
                         {SELECTION_FILE} retains line {} and main remains at {} — {}",
                        refusal.why,
                        short(line.head()),
                        short(&after),
                        refusal.remedy
                    );
                    self.say(&message);
                    self.last = message;
                    return Err(Halt::Said(3));
                }
            }
        } else {
            format!("clone main remains at {}", short(&before))
        };
        let message = format!(
            "{why}; {clone_state}; {SELECTION_FILE} retains pending line {} — retry with \
             {RELEASE_COMMAND}, or drop it with {RELEASE_COMMAND} --forget",
            short(line.head())
        );
        self.say(&message);
        self.last = message;
        Err(Halt::Said(code))
    }

    fn dry_final(&mut self) -> Flow {
        let v = self.version();
        let r = self.commit_r();
        if !self.dry_unavailable.is_empty() {
            self.last = format!(
                "DRY RUN aterm {v} from {}: cannot judge every step: {}{}; no release refs or journals changed",
                short(&r),
                self.dry_unavailable.join("; "),
                if self.would.is_empty() {
                    String::new()
                } else {
                    format!("; would refuse at {}", self.would.join("; "))
                },
            );
            let last = self.last.clone();
            self.say(&last);
            return Err(Halt::Said(if self.would.is_empty() { 3 } else { 1 }));
        }
        let owner_waits = if self.waits.is_empty() {
            String::new()
        } else {
            format!(
                ", and wait for the owner at {} ({})",
                self.wait_at.unwrap_or(Step::Publish).at(),
                self.waits.join("; ")
            )
        };
        if self.would.is_empty() {
            if !self.dry_resource_waits.is_empty() {
                let action = if self.opts.qualify {
                    "--qualify would"
                } else if self.opts.prepare {
                    "a real --prepare would"
                } else {
                    "a real run would"
                };
                self.last = format!(
                    "DRY RUN aterm {v} from {}: {action} wait at {} ({}); about {} of work remains after admission — no release refs or journals changed",
                    short(&r),
                    Step::Qualify.at(),
                    self.dry_resource_waits.join("; "),
                    fmt_dur(self.plan_secs)
                );
                let last = self.last.clone();
                self.say(&last);
                return Err(Halt::Said(0));
            }
            self.last = if self.opts.qualify {
                format!(
                    "DRY RUN aterm {v} from {}: --qualify would check the full export locally, build and verify, about {} of work{owner_waits}; no staging snapshot pushed or publication — no release refs or journals changed",
                    short(&r),
                    fmt_dur(self.plan_secs)
                )
            } else if self.opts.prepare {
                format!(
                    "DRY RUN aterm {v} from {}: a real --prepare would go ahead and stop PREPARED \
                     after {}, about {} of work{owner_waits}; then {RELEASE_COMMAND} would finish it, \
                     about {} — no release refs or journals changed",
                    short(&r),
                    Step::Ready.at(),
                    fmt_dur(self.plan_secs),
                    fmt_dur(self.later_secs)
                )
            } else {
                format!(
                    "DRY RUN aterm {v} from {}: a real run would go ahead, about {} of \
                     work{owner_waits} — no release refs or journals changed",
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
             — no release refs or journals changed",
            short(&r),
            self.would.join("; "),
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
        let at = self.b.hms(now.saturating_add(total));
        format!(
            "  · ETA {} ({} to go)",
            at.get(..5).unwrap_or(&at),
            fmt_dur(total)
        )
    }

    /// A step's estimate from this machine's `steps.tsv`, else the plan's default. Step 4 is its
    /// synchronous AppKit prefix plus its longest branch (the branches run at once).
    fn est_secs(&self, step: Step) -> u64 {
        if step == Step::Publish {
            return self
                .publish_sides()
                .into_iter()
                .map(|side| {
                    self.est
                        .get(&side.row())
                        .copied()
                        .unwrap_or_else(|| side.default_secs())
                })
                .max()
                .unwrap_or(0);
        }
        if step == Step::Qualify {
            let (plan, _) = self.qualify_plan();
            let appkit = plan.appkit.is_some().then(|| {
                self.est
                    .get(QUALIFY_APPKIT_ROW)
                    .copied()
                    .unwrap_or(QUALIFY_APPKIT_DEFAULT_SECS)
            });
            let graph = plan
                .branches()
                .into_iter()
                .map(|b| self.est_branch(b))
                .max()
                .unwrap_or(0);
            return appkit.unwrap_or(0).saturating_add(graph);
        }
        self.est
            .get(step.name())
            .copied()
            .unwrap_or_else(|| step.default_secs())
    }

    /// The sides step 7 runs: the sign alone where the source was published earlier.
    fn publish_sides(&self) -> Vec<Side> {
        if self.skip_publish().is_some() {
            vec![Side::Sign]
        } else {
            Side::ALL.to_vec()
        }
    }

    fn est_branch(&self, branch: Branch) -> u64 {
        self.est
            .get(&branch.row())
            .copied()
            .unwrap_or_else(|| branch.default_secs())
    }

    /// The journal, when it is R's: its facts (the done list, the reservation, the artifact, a
    /// PREPARED stop) are R's own. While `--fix` has fixes still to carry (step 3's, or a dry
    /// run's throughout), R is the line's old head: the head the release goes on with is not
    /// made yet, and its journal starts with none of them.
    fn r_journal(&self) -> Option<&Journal> {
        let c = self.cand.as_ref().filter(|c| c.carry.is_empty())?;
        self.journal.as_ref().filter(|j| j.commit == c.commit)
    }

    fn journal_has(&self, fact: &str) -> bool {
        self.r_journal().is_some_and(|j| j.has(fact))
    }

    fn journal_add(&mut self, fact: &str) -> Flow {
        self.journal_add_all(&[fact])
    }

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
        let release_dir = self.b.release_dir();
        let running = release_dir.join(RUNNING_FILE);
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
        let selected = release_dir.join(SELECTION_FILE);
        let pending = match Journal::load(&selected) {
            Ok(pending) => pending,
            Err(why) => {
                self.last = format!(
                    "COULD NOT RUN: {why}; candidate.toml may retain an undecided source write \
                     and was left byte-for-byte unchanged"
                );
                let last = self.last.clone();
                self.say(&last);
                return Err(Halt::Said(3));
            }
        };
        let release = match Journal::load(&self.journal_path) {
            Ok(release) => release,
            Err(why) => {
                self.last =
                    format!("COULD NOT RUN: {why}; release.toml was left byte-for-byte unchanged");
                let last = self.last.clone();
                self.say(&last);
                return Err(Halt::Said(3));
            }
        };
        if let Some(txn) = pending
            .as_ref()
            .and_then(|journal| journal.supersede.as_ref())
            .filter(|txn| txn.cas_unknown)
        {
            self.last = format!(
                "COULD NOT RUN: candidate.toml retains a supersession whose source write is \
                 unknown; classify it first: {RELEASE_COMMAND} --commit {} --supersede",
                txn.replacement.head()
            );
            let last = self.last.clone();
            self.say(&last);
            return Err(Halt::Said(3));
        }
        // The reservation's private draft leaves first, while release.toml still proves it this
        // clone's; after the journal is gone no tool could withdraw it.
        let mut withdrawn = None;
        if let Some(journal) = release.as_ref()
            && let Some(old) = journal.reserved.clone()
        {
            match self.withdraw_for(
                &journal.version,
                &old,
                &format!("{RELEASE_COMMAND} --forget"),
            ) {
                Ok(note) => {
                    withdrawn = Some(note.unwrap_or_else(|| {
                        format!(
                            "the cut journal claiming build {} keeps its private draft",
                            old.build
                        )
                    }));
                }
                Err(refusal) => {
                    let (code, head) = if refusal.could_not_run {
                        (3, "COULD NOT RUN")
                    } else {
                        (1, "release")
                    };
                    self.last = format!(
                        "{head}: {}; candidate.toml and release.toml were left byte-for-byte \
                         unchanged — {}",
                        refusal.why, refusal.remedy
                    );
                    let last = self.last.clone();
                    self.say(&last);
                    return Err(Halt::Said(code));
                }
            }
        }
        let mut restored = None;
        if let Some(txn) = pending
            .as_ref()
            .and_then(|journal| journal.supersede.clone())
        {
            if let Err(refusal) = self.restore_supersede_head(&txn, release.as_ref()) {
                self.last = format!(
                    "COULD NOT RUN: {}; candidate.toml and release.toml were retained, and main \
                     remains at {} — {}",
                    refusal.why,
                    short(&self.b.head()),
                    refusal.remedy
                );
                let last = self.last.clone();
                self.say(&last);
                return Err(Halt::Said(3));
            }
            restored = Some(format!(
                "clone main was first restored to the supersession's original head {}",
                short(&txn.head)
            ));
        } else if let Some(journal) = pending.as_ref().or(release.as_ref())
            && !journal.line().hops.is_empty()
        {
            let line = journal.line();
            let needs_rewind =
                journal.has(PENDING_LINE) || !self.b.is_ancestor(line.head(), "origin/main");
            if needs_rewind && self.b.head() == line.head() {
                if let Err(refusal) = self.b.rewind_selection(&line) {
                    let journal_name = if pending.is_some() {
                        SELECTION_FILE
                    } else {
                        JOURNAL_FILE
                    };
                    let after = self.b.head();
                    self.last = format!(
                        "COULD NOT RUN: {}; {journal_name} still retains line {} and main \
                         remains at {} — {}",
                        refusal.why,
                        short(line.head()),
                        short(&after),
                        refusal.remedy
                    );
                    let last = self.last.clone();
                    self.say(&last);
                    return Err(Halt::Said(3));
                }
                restored = Some(format!(
                    "clone main was first restored to its recorded prefix {}",
                    short(&self.b.head())
                ));
            }
        }
        let asks = pending
            .as_ref()
            .map(|journal| journal.asks.clone())
            .or_else(|| release.as_ref().map(|journal| journal.asks.clone()))
            .unwrap_or_default();
        for key in asks {
            let how = match self.b.withdraw(&key) {
                Ok(reply) => format!("withdrawn: {reply}"),
                Err(why) => format!("not withdrawn: {why}"),
            };
            self.say(&format!("release: the ask {key} — {how}"));
        }
        let removed_candidate = match std::fs::remove_file(&selected) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                self.last = format!(
                    "COULD NOT RUN: cannot remove {}: {error}; its journal was left in place",
                    selected.display()
                );
                let last = self.last.clone();
                self.say(&last);
                return Err(Halt::Said(3));
            }
        };
        let removed_release = match std::fs::remove_file(&self.journal_path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                let sync = if removed_candidate {
                    crate::durable::sync_dir(&release_dir)
                        .err()
                        .map(|error| format!("; candidate removal sync also failed: {error}"))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                self.last = format!(
                    "COULD NOT RUN: candidate journal removed={removed_candidate}, but cannot \
                     remove {}: {error}; the release journal remains{sync}",
                    self.journal_path.display(),
                );
                let last = self.last.clone();
                self.say(&last);
                return Err(Halt::Said(3));
            }
        };
        if (removed_candidate || removed_release)
            && let Err(error) = crate::durable::sync_dir(&release_dir)
        {
            self.last = format!(
                "COULD NOT RUN: candidate journal present={}, release journal present={}; \
                 cannot make their removal durable: {error}; clone main is at {}",
                selected.exists(),
                self.journal_path.exists(),
                short(&self.b.head())
            );
            let last = self.last.clone();
            self.say(&last);
            return Err(Halt::Said(3));
        }
        let restored: String = [restored, withdrawn]
            .into_iter()
            .flatten()
            .map(|state| format!("; {state}"))
            .collect();
        self.last = match (removed_candidate, removed_release) {
            (true, true) => format!(
                "release: forgot this clone's candidate and release journals{restored}; nothing \
                 public changed"
            ),
            (true, false) => format!(
                "release: forgot this clone's candidate journal{restored}; no release journal \
                 was present; nothing public changed"
            ),
            (false, true) => format!(
                "release: forgot this clone's release journal ({}){restored}; nothing public \
                 changed",
                self.journal_path.display()
            ),
            (false, false) => "release: no release journal in this clone".to_string(),
        };
        let last = self.last.clone();
        self.say(&last);
        Err(Halt::Said(0))
    }

    fn prepared(&self) -> Option<Prepared> {
        self.r_journal()
            .filter(|j| !j.finished())
            .and_then(|j| j.prepared.clone())
    }

    fn pending_reask(&mut self) -> Option<Prepared> {
        if self.opts.prepare || self.opts.qualify || self.claimed() {
            return None;
        }
        self.prepared()
    }

    fn same_prover(p: &Prover, prep: &Prepared) -> bool {
        p.bin.display().to_string() == prep.prover && p.commit == prep.trustc
    }

    /// Steps 4 and 5: whether the bundle half ran and passed.
    fn qualify_and_bundle(&mut self) -> Result<bool, Halt> {
        self.begin(Step::Qualify)?;
        if let Some(why) = self.claimed_words() {
            self.skip(&why);
        } else {
            // The FAIL rule (A19) comes first: a FAILed tree is refused at once, exit 1, before
            // the live world is asked again or room is sought for work it can never pass.
            self.verification_answer()?;
            self.fail_rule()?;
            // a PREPARED release asks the live world again before anything else.
            if let Some(prep) = self.pending_reask() {
                self.reask_prepared(&prep)?;
            }
            self.qualify()?;
        }

        self.begin(Step::Bundle)?;
        if let Some(why) = self.skip_bundle() {
            self.skip(&why);
            return Ok(false);
        }
        self.disk_still_needed("the bundle half")?;
        self.bundle()?;
        Ok(true)
    }

    fn let_go(&mut self) {
        self.claim = None;
    }

    /// Stage R again in step 4: its export branch runs, and the plan's time grows by it.
    fn stage_again(&mut self, old: Option<String>, why: String) {
        self.restage = Some((old, why));
        if !self.planned.contains(&Step::Qualify) {
            let secs = self.est_branch(Branch::Export);
            self.planned.push(Step::Qualify);
            self.planned.sort_unstable();
            self.plan_secs += secs;
        }
    }

    fn reask_prepared(&mut self, prep: &Prepared) -> Flow {
        let (v, r) = (self.version(), self.commit_r());
        let when = self.b.hms(prep.at);
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
        let mut stale: Vec<Refusal> = Vec::new();
        let mut unasked: Vec<String> = Vec::new();
        let again = format!(
            "prepare it again: {RELEASE_COMMAND} --prepare (it makes again only what this \
             undid), then {RELEASE_COMMAND}"
        );

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
                        "release the next product version: {RELEASE_COMMAND} --forget, then \
                         {RELEASE_COMMAND} --bump"
                    ),
                ));
            }
            Err(why) => unasked.push(format!("whether {tag} is on origin: {why}")),
        }

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

        let facts = [
            ("the bundle built once", self.facts.built.clone()),
            (
                "the `verify release` records",
                self.facts.checks.answer().clone(),
            ),
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
            let why = "R's attestation names no engine commit, which pub publish refuses";
            words.push(format!("{why}: the stage runs again"));
            self.stage_again(None, why.to_string());
        }
        let line = words.join("; ");
        self.say(&format!("        {line}"));

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
        let published = self.facts.engine.has;

        let mut missing: Vec<String> = Vec::new();
        for (name, answer) in [
            ("built once", &self.facts.built),
            ("verified", self.facts.checks.answer()),
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

    /// Claim the clone for this run before anything else it does but `--forget` (a dry run
    /// claims nothing). An image exec'd into this process continues its exact pid/start claim
    /// before it can say a final line; every other live holder is this run's checks' refusal
    /// ([`Run::claim_refused`]).
    fn take_claim(&mut self) {
        if self.dry {
            return;
        }
        let release_dir = self.b.release_dir();
        let me = self.b.me();
        let logs = self.b.log_path(Step::Checks);
        let logs = logs.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut started_of = |pid| self.b.started_of(pid);
        let taken = RunClaim::take(
            &release_dir.join(RUNNING_FILE),
            (me.0, &me.1),
            &logs,
            &mut started_of,
        );
        match taken {
            Ok(claim) => {
                // The preceding image kept its selection and freeze records in its own logs.
                // Link those records to this image's step timeline.
                if let Some(from) = &claim.previous_logs {
                    let event_name = if claim.legacy_handoff {
                        "handed"
                    } else {
                        "continued"
                    };
                    let event = aterm_json::Value::Object(aterm_json::Map::from([
                        ("schema".to_string(), 1.into()),
                        ("event".to_string(), event_name.into()),
                        ("from".to_string(), from.display().to_string().into()),
                    ]));
                    self.say(&format!("release-event: {event}"));
                }
                self.claim = Some(claim);
            }
            Err(Ok(h)) => self.claim_refused = Some(running_refusal(&h)),
            Err(Err(why)) => {
                self.claim_refused = Some(Refusal::new(
                    why,
                    "check the permissions of the clone's .git/aterm-release",
                ));
            }
        }
    }

    fn checks(&mut self) -> Result<String, Halt> {
        let _phase = crate::cut_timing::Timer::start(&self.b.log_path(Step::Checks), "checks");
        let mut notes: Vec<String> = Vec::new();

        if self.dry {
            let release_dir = self.b.release_dir();
            let me = self.b.me();
            let mut started_of = |pid| self.b.started_of(pid);
            if let Some(h) = live_holder(&release_dir.join(RUNNING_FILE), &mut started_of)
                && h.pid != me.0
            {
                self.refuse_hard(running_refusal(&h))?;
            }
        } else if let Some(refusal) = self.claim_refused.take() {
            self.refuse_hard(refusal)?;
        }

        let journaled = self
            .journal
            .as_ref()
            .filter(|j| !j.finished())
            .map(|j| j.prover.clone());
        match self.b.prover(journaled.as_deref()) {
            Ok(p) => {
                if !self.dry
                    && let Err(error) = self.b.lease(&p)
                {
                    self.refuse_hard(Refusal::new(error, "restore the toolchain lease and retry"))?;
                }
                self.prover = Some(p);
            }
            Err(why) => self.refuse(Refusal::new(
                format!("no Trust prover: {why}"),
                "aterm pkg install trust (then aterm pkg doctor), or export \
                 TRUST_STAGE2_BIN=<a stage2 bin dir holding trustc and targo>",
            ))?,
        }

        let cleared = self.b.cleared_env();
        if !cleared.is_empty() {
            notes.push(format!("cleared for every step: {}", cleared.join(" ")));
        }

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
        self.record_selected_line()?;
        let head = self.b.head();
        if self.b.branch_is_main()
            && head != self.commit_r()
            && !self.b.is_ancestor(&head, &self.tip.clone())
            && !self.pending_hop(&head)
        {
            self.refuse(if self.heads_a_hop(&head) {
                Refusal::new(
                    format!(
                        "this clone's main is its line's head {}, and origin/main no longer \
                         holds it",
                        short(&head)
                    ),
                    lost_record(),
                )
                .lost_line_record()
            } else {
                Refusal::new(
                    "this clone's main has commits origin/main does not — a release is staged \
                     from origin's history",
                    "push them (git push origin main), or release from a fresh clone",
                )
            })?;
        }

        self.choose_candidate(false)?;
        if self.cand.as_ref().is_some_and(|c| c.bump_from.is_none()) {
            self.public_state();
        }
        notes.push(self.provision(false)?);
        if !self.claimed()
            && let Some(c) = self.cand.clone()
        {
            self.still_possible(&c, None)?;
        }

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
                let bundle = self.est_or_default(Step::Bundle);
                let again = self
                    .est_branch(Branch::Tree)
                    .max(self.est_branch(Branch::Build))
                    + bundle;
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
                    "finish it: {}; or withdraw it: {RELEASE_COMMAND} --abandon v{}",
                    self.start_remedy("--commit <its published commit>"),
                    cut.version
                ),
            ))?;
        }

        // Every cheap local/remote refusal precedes the input judgment's first compile.
        // Even a dry run asks the whole preflight before reporting an input census red.
        if !self.claimed()
            && let Some(c) = self.cand.clone()
        {
            notes.push(self.commit_gates(&c)?);
        }
        if !self.dry {
            // Still under the toolchain lease, after all commit-only refusals.
            if let Err(error) = self.b.seal_inputs() {
                self.refuse_hard(Refusal::new(
                    format!("the release build inputs cannot be sealed: {error}"),
                    "restore the release build inputs and retry",
                ))?;
            }
        }
        let candidate = self.commit_r();
        let version = self.version();
        if let Some(previous) = self.prev_release(&candidate, &version)
            && let Some(base) = self.b.origin_tag(&format!("v{previous}"))
            && let Some(prover) = self.prover.clone()
        {
            match self.b.input_checks(&base, &candidate, &prover) {
                Ok(note) => notes.push(note),
                Err(found) => {
                    let c = self.cand.clone().ok_or(Halt::Said(1))?;
                    let remedy = self.remedy(&c);
                    self.refuse(match found {
                        InputFinding::Failed(why) => {
                            Refusal::new(why, remedy.source).failing("verify release-input")
                        }
                        InputFinding::Unanswered(why) => {
                            Refusal::new(why, format!("run {} again once it answers", remedy.again))
                                .could_not_run()
                        }
                    })?;
                }
            }
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

    fn record_selected_line(&mut self) -> Flow {
        let Some(journal) = self.journal.clone().filter(|j| j.has(PENDING_LINE)) else {
            return Ok(());
        };
        if self.opts.qualify {
            return self.refuse_hard(Refusal::new(
                "private qualification cannot record a pending fleet line",
                "use a separate clean clone for the private candidate",
            ));
        }
        let line = journal.line();
        // Local construction is not publication authority. Ask the live roster and cut state
        // in the new image before recording even the content-neutral merge on source main.
        let rows = self.b.engine_rows(None);
        let published = rows
            .newest
            .as_deref()
            .is_some_and(|rev| self.b.version_at(rev).as_deref() == Some(line.version.as_str()));
        if published
            || self
                .b
                .cut_journal()
                .is_some_and(|c| c.version == line.version)
        {
            let state = if self.dry {
                "the pending candidate would be dropped only after restoring main".to_string()
            } else {
                let release_path = self.b.release_dir().join(JOURNAL_FILE);
                let saved = match Journal::load(&release_path) {
                    Ok(Some(saved)) => saved,
                    Ok(None) => {
                        return self.refuse_hard(
                            Refusal::new(
                                "the published or claimed release has no authoritative release.toml; candidate.toml was retained",
                                "restore the release journal, then retry",
                            )
                            .could_not_run(),
                        );
                    }
                    Err(why) => {
                        return self.refuse_hard(
                            Refusal::new(
                                why,
                                "restore release.toml before dropping the superseded local candidate",
                            )
                            .could_not_run(),
                        );
                    }
                };
                if self.b.head() == line.head()
                    && let Err(refusal) = self.b.rewind_selection(&line)
                {
                    return self.refuse_hard(
                        refusal.detail([
                            "the already published or claimed release left candidate.toml intact"
                                .into(),
                        ]),
                    );
                }
                self.journal = Some(saved);
                self.journal_path = release_path;
                let selection = self.b.release_dir().join(SELECTION_FILE);
                match std::fs::remove_file(&selection) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return self.refuse_hard(
                            Refusal::new(
                                format!("cannot remove {}: {error}", selection.display()),
                                "make the release state directory writable and retry",
                            )
                            .could_not_run(),
                        );
                    }
                }
                if let Err(error) = crate::durable::sync_dir(&self.b.release_dir()) {
                    return self.refuse_hard(
                        Refusal::new(
                            format!(
                                "candidate.toml was removed, but its directory sync failed: \
                                 {error}"
                            ),
                            "make the release state directory writable and retry",
                        )
                        .could_not_run(),
                    );
                }
                format!(
                    "the local candidate was dropped after restoring main to {}",
                    short(&self.b.head())
                )
            };
            return self.refuse_hard(Refusal::new(
                "the release in progress is already published or claimed; its local fix was not recorded",
                format!(
                    "{state}; finish the published release, then start the next line"
                ),
            ));
        }
        if let Err(refusal) = self.b.verify_pending_line(&line) {
            return self.refuse_hard(refusal);
        }
        if !self.dry {
            if let Err(why) = self.b.fleet_audit() {
                return self.refuse_hard(Refusal::new(
                    why,
                    "retry the release after inspecting the line claim",
                ));
            }
            match self.b.record_line(&line) {
                Ok(note) => self.say(&format!("        {note}")),
                Err(refusal) => return self.refuse_hard(refusal),
            }
            self.tip = self
                .b
                .rev("origin/main^{commit}")
                .unwrap_or_else(|| self.tip.clone());
            if let Some(j) = self.journal.as_mut() {
                j.done.retain(|fact| fact != PENDING_LINE);
                if let Err(why) = j.save(&self.b.release_dir().join(SELECTION_FILE)) {
                    return self
                        .refuse_hard(Refusal::new(why, "restore the release journal and retry"));
                }
            }
        }
        Ok(())
    }

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

    fn saved_release_journal(&mut self) -> Result<Option<Journal>, Halt> {
        let path = self.b.release_dir().join(JOURNAL_FILE);
        Journal::load(&path).map_err(|why| {
            self.halted(Refusal::new(
                why,
                "restore release.toml before selecting a replacement line",
            ))
        })
    }

    /// release.toml's line, which a supersession transaction keeps as its rollback identity
    /// (`SupersedeTxn::release_line`), and the line its preflight expects to retire: that same
    /// line while release.toml is unfinished. A finished release.toml's line was released, so a
    /// supersession expects none and preflights the fleet's line as it finds it.
    fn supersede_release_lines(
        &mut self,
    ) -> Result<(Option<crate::line::Line>, Option<crate::line::Line>), Halt> {
        let saved = self.saved_release_journal()?;
        let release_line = match &self.supersede {
            Some(txn) => txn.release_line.clone(),
            None => saved.as_ref().map(Journal::line),
        };
        let expected = release_line
            .clone()
            .filter(|_| !saved.as_ref().is_some_and(Journal::finished));
        Ok((release_line, expected))
    }

    /// A line journal with no candidate-only or release-result facts. It keeps only the local
    /// prover declaration, Linux choice, and unresolved asks that remain meaningful on a
    /// different authoritative line.
    fn clean_line_journal(&self, line: &crate::line::Line) -> Journal {
        let current = self.journal.as_ref();
        Journal {
            version: line.version.clone(),
            commit: line.head().to_string(),
            prover: current.map_or_else(String::new, |journal| journal.prover.clone()),
            trustc: current.map_or_else(String::new, |journal| journal.trustc.clone()),
            started: line.started,
            linux: current.map_or_else(LinuxDecl::default, |journal| journal.linux.clone()),
            asks: current.map_or_else(Vec::new, |journal| journal.asks.clone()),
            line: Some(line.clone()),
            ..Journal::default()
        }
    }

    /// Restore the exact local state recorded in candidate.toml. HEAD moves first; if candidate
    /// removal fails, overwriting it with release.toml makes both preferred journals name the
    /// authoritative old line.
    fn restore_supersede_selection(&mut self) -> Result<(), Refusal> {
        let txn = self.supersede.clone().ok_or_else(|| {
            Refusal::new(
                "candidate.toml has no supersession transaction to restore",
                "keep the candidate and retry the exact --commit ... --supersede request",
            )
            .could_not_run()
        })?;
        let release_dir = self.b.release_dir();
        let release_path = release_dir.join(JOURNAL_FILE);
        let saved = Journal::load(&release_path).map_err(|why| {
            Refusal::new(
                why,
                "restore release.toml before rolling back the replacement",
            )
            .could_not_run()
        })?;
        self.restore_supersede_head(&txn, saved.as_ref())?;
        let selection = release_dir.join(SELECTION_FILE);
        let mut restored = saved.clone();
        match std::fs::remove_file(&selection) {
            Ok(()) => crate::durable::sync_dir(&release_dir).map_err(|error| {
                Refusal::new(
                    format!("cannot make candidate.toml removal durable: {error}"),
                    "retry after the release state directory is writable",
                )
                .could_not_run()
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                let fallback = saved
                    .clone()
                    .unwrap_or_else(|| self.clean_line_journal(&txn.previous));
                fallback.save(&selection).map_err(|why| {
                    Refusal::new(
                        format!("{why}; candidate.toml removal also failed: {error}"),
                        "make both release journals writable and retry",
                    )
                    .could_not_run()
                })?;
                restored = Some(fallback);
            }
        }
        self.journal = restored;
        self.journal_path = release_path;
        self.supersede = None;
        Ok(())
    }

    /// Restore the checkout half of an unentered supersession transaction. The saved release
    /// journal must still be the exact authority against which candidate.toml was written.
    fn restore_supersede_head(
        &mut self,
        txn: &SupersedeTxn,
        saved: Option<&Journal>,
    ) -> Result<(), Refusal> {
        if saved.map(Journal::line) != txn.release_line {
            return Err(Refusal::new(
                "release.toml changed after the replacement transaction began",
                "keep candidate.toml and reconcile the two release journals",
            )
            .could_not_run());
        }
        if self.b.head() != txn.head {
            self.b.move_to(&txn.head)?;
        }
        Ok(())
    }

    /// Converge this clone on a different authoritative winner. The preferred candidate is
    /// replaced first, then release.toml; a failure to write release.toml restores the exact
    /// transaction so its recovery identity is not lost.
    fn adopt_supersede_winner(
        &mut self,
        winner: &crate::line::Line,
    ) -> Result<Option<String>, Refusal> {
        let original = self.journal.clone().ok_or_else(|| {
            Refusal::new(
                "candidate.toml has no journal to replace with the authoritative winner",
                "keep the candidate and retry the exact --commit ... --supersede request",
            )
            .could_not_run()
        })?;
        let release_dir = self.b.release_dir();
        let selection = release_dir.join(SELECTION_FILE);
        let release_path = release_dir.join(JOURNAL_FILE);
        let adopted = self.clean_line_journal(winner);
        self.withdraw_dropped(&adopted)?;
        adopted.save(&selection).map_err(|why| {
            Refusal::new(
                why,
                "keep candidate.toml and retry adopting the authoritative fleet line",
            )
            .could_not_run()
        })?;
        if let Err(why) = adopted.save(&release_path) {
            let restored = original.save(&selection);
            let detail = restored
                .err()
                .map(|restore| format!("; restoring candidate.toml also failed: {restore}"))
                .unwrap_or_default();
            return Err(Refusal::new(
                format!("{why}{detail}"),
                "restore the release state directory, then retry the exact supersession request",
            )
            .could_not_run());
        }
        self.journal = Some(adopted);
        self.journal_path = release_path;
        self.supersede = None;
        if self.b.head() != winner.head() {
            self.b.move_to(winner.head()).map_err(|refusal| {
                refusal.detail([format!(
                    "both journals already name authoritative line {}",
                    short(winner.head())
                )])
            })?;
        }
        let retained = match std::fs::remove_file(&selection) {
            Ok(()) => crate::durable::sync_dir(&release_dir)
                .err()
                .map(|error| format!("candidate removal sync will retry: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => Some(format!(
                "candidate.toml remains as an equal authoritative journal: {error}"
            )),
        };
        Ok(retained)
    }

    fn refuse_supersede(&mut self, refusal: Refusal) -> Flow {
        if let Err(restore) = self.restore_supersede_selection() {
            return self.refuse_hard(
                restore.detail([format!("the triggering refusal was: {}", refusal.why)]),
            );
        }
        self.refuse_hard(refusal)
    }

    fn stop_supersede(&mut self, why: String) -> Flow {
        if let Err(restore) = self.restore_supersede_selection() {
            return self.refuse_hard(restore.detail([why]));
        }
        let code = if why.starts_with("CUTTING ELSEWHERE:") {
            4
        } else {
            2
        };
        self.say(&why);
        self.last = why;
        Err(Halt::Said(code))
    }

    fn stop_supersede_at_winner(&mut self, why: String, winner: crate::line::Line) -> Flow {
        let retained = match self.adopt_supersede_winner(&winner) {
            Ok(retained) => retained,
            Err(refusal) => {
                return self
                    .refuse_hard(refusal.detail([format!("the triggering refusal was: {why}")]));
            }
        };
        let retained = retained.map(|note| format!("; {note}")).unwrap_or_default();
        let message = format!(
            "{why}; adopted authoritative line {} in HEAD and both journals{retained}",
            short(winner.head())
        );
        self.say(&message);
        self.last = message;
        Err(Halt::Said(2))
    }

    fn halt_supersede(&mut self, halt: Halt) -> Flow {
        if matches!(&halt, Halt::Said(0)) {
            return Err(halt);
        }
        if let Err(restore) = self.restore_supersede_selection() {
            return self.refuse_hard(restore);
        }
        Err(halt)
    }

    /// Record a source-confirmed supersession. Until this final candidate write succeeds, the
    /// on-disk `cas_unknown` transaction remains the exact recovery request.
    fn finish_supersede_commit(&mut self, txn: SupersedeTxn, note: String) -> Flow {
        self.say(&note);
        let Some(journal) = self.journal.as_mut() else {
            return self.refuse_hard(
                Refusal::new(
                    "the committed supersession has no candidate journal",
                    format!(
                        "retry its exact recovery: {RELEASE_COMMAND} --commit {} --supersede",
                        txn.replacement.head()
                    ),
                )
                .could_not_run(),
            );
        };
        journal.add(SUPERSEDED_LINE);
        journal.supersede = None;
        self.supersede = None;
        if let Err(why) = self.b.save_supersede_journal(journal, &self.journal_path) {
            journal.done.retain(|fact| fact != SUPERSEDED_LINE);
            journal.supersede = Some(txn.clone());
            self.supersede = Some(txn.clone());
            let message = format!(
                "COULD NOT RUN: the exact supersession is durable, but its final candidate \
                 write failed ({why}); candidate.toml retains the exact recovery: \
                 {RELEASE_COMMAND} --commit {} --supersede",
                txn.replacement.head()
            );
            self.say(&message);
            self.last = message;
            return Err(Halt::Said(3));
        }
        self.opts.supersede = false;
        self.b.supersession_completed();
        Ok(())
    }

    /// Resolve an entered source write before any local gate can roll its transaction back.
    /// Until this authoritative reread answers, candidate.toml is the sole exact recovery key.
    /// It runs before candidate selection, so it reads the transaction alone: a journal never
    /// holds both an entered transaction and `SUPERSEDED_LINE` (finish_supersede_commit swaps
    /// one for the other, and back where its write fails).
    fn recover_unknown_supersession(&mut self) -> Flow {
        if !self.opts.supersede || !self.supersede.as_ref().is_some_and(|txn| txn.cas_unknown) {
            return Ok(());
        }
        let txn = self.supersede.clone().ok_or(Halt::Said(1))?;
        match self.b.classify_supersede(&txn.previous, &txn.replacement) {
            SupersedeResult::Committed(note) => self.finish_supersede_commit(txn, note),
            SupersedeResult::Refused {
                why,
                winner: Some(winner),
                ..
            } => self.stop_supersede_at_winner(why, winner),
            SupersedeResult::Refused {
                retry: true,
                winner: None,
                ..
            } => {
                if let Some(txn) = self.supersede.as_mut() {
                    txn.cas_unknown = false;
                }
                if let Some(journal) = self.journal.as_mut() {
                    journal.supersede = self.supersede.clone();
                }
                self.save_journal()
            }
            SupersedeResult::Refused { why, .. } => self.stop_supersede(why),
            SupersedeResult::Unknown(why) => {
                let message = format!(
                    "COULD NOT RUN: the supersession write is still unknown: {why}; \
                     candidate.toml retains {} for an exact retry",
                    short(txn.replacement.head())
                );
                self.say(&message);
                self.last = message;
                Err(Halt::Said(3))
            }
        }
    }

    /// Select and construct the same immutable head as preparation, without moving the clone,
    /// taking a release claim, recording a line or writing either release journal.
    fn prepare_dry_candidate(&mut self) -> Flow {
        let me = self.b.me();
        if let Some(holder) = live_holder(&self.b.release_dir().join(RUNNING_FILE), &mut |pid| {
            self.b.started_of(pid)
        }) && holder.pid != me.0
        {
            return self.refuse_hard(running_refusal(&holder));
        }
        if !self.b.branch_is_main() {
            return self.refuse_hard(Refusal::new(
                "this clone is not on main",
                "release from a clean clone on main (git switch main)",
            ));
        }
        self.b.prepare_clone().map_err(|r| self.halted(r))?;
        self.tip = self.b.rev("origin/main^{commit}").ok_or_else(|| {
            self.halted(Refusal::new(
                "no local origin/main candidate",
                "git fetch origin",
            ))
        })?;
        self.choose_candidate(true)?;
        if !self.opts.fix.is_empty()
            && let Some(journal) = self.journal.clone()
        {
            self.refuse_published(&journal, "--fix")?;
        }
        if !self.opts.supersede && !self.opts.qualify {
            let mut candidate = self.cand.clone().ok_or(Halt::Said(1))?;
            let previous = candidate.commit.clone();
            crate::line_claim::follow(self.b, &mut candidate, &mut self.journal)
                .map_err(|r| self.halted(r))?;
            if candidate.commit == previous {
                self.settle_line_selection();
            } else {
                self.fleet_selection(&candidate);
            }
            self.cand = Some(candidate);
            if self.commit_r() != previous && !self.opts.fix.is_empty() {
                let journal = self.journal.clone();
                self.cand = Some(self.fix_candidate(journal.as_ref(), false)?);
            }
        }
        let journaled = self
            .journal
            .as_ref()
            .filter(|j| !j.finished())
            .map(|j| j.prover.clone());
        self.prover = Some(self.b.prover(journaled.as_deref()).map_err(|why| {
            self.halted(Refusal::new(why, "restore the locked Trust toolchain").could_not_run())
        })?);
        let mut candidate = self.cand.clone().ok_or(Halt::Said(1))?;
        if candidate.bump_from.is_some() {
            let prover = self.prover.clone().ok_or(Halt::Said(1))?;
            candidate.commit = self
                .b
                .preview_bump(&candidate, &prover)
                .map_err(|r| self.halted(r))?;
            self.cand = Some(candidate.clone());
        }
        if !candidate.carry.is_empty() {
            let mut line = self.journal.as_ref().ok_or(Halt::Said(1))?.line();
            let (hops, _) = self
                .b
                .carry(&line, &candidate.carry)
                .map_err(|r| self.halted(r))?;
            for (i, hop) in hops.iter().enumerate() {
                self.say(&format!(
                    "        {} carries cleanly as hop {} ({}) — local plan, no refs changed",
                    short(&hop.carries),
                    line.hops.len() + i + 1,
                    short(&hop.commit)
                ));
            }
            line.hops.extend(hops);
            candidate.commit = line.head().to_owned();
            candidate.why = format!(
                "planned head of line v{} from {}; carries {} locally before step 1",
                line.version,
                short(&line.base),
                candidate
                    .carry
                    .iter()
                    .map(|fix| short(&fix.commit))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            // Give the frozen image the same pending identity as real preparation. It must
            // judge that identity through the ordinary acceptance path, never a dry-only
            // ancestry exemption. Nothing saves this in-memory journal.
            let prover = self.prover.clone().ok_or(Halt::Said(1))?;
            self.journal = Some(self.carried_journal(&candidate, line, &prover));
            self.cand = Some(candidate);
        }
        self.own_cutter()
    }

    /// Preflight an explicit replacement while selection is unchanged, then journal that one
    /// transaction in candidate.toml before dispatching to the candidate's cutter. An old local
    /// origin/main is a pinned candidate, not a claim about the current remote tip; `--fix`
    /// refreshes source refs before admission, and step 1 proves ancestry before a product build
    /// or staging begins.
    fn prepare_candidate(&mut self) -> Flow {
        let _timing =
            crate::cut_timing::Timer::start(&self.b.log_path(Step::Checks), "launch-prelude");
        if let Some(refusal) = self.claim_refused.take() {
            return self.refuse_hard(refusal);
        }
        self.recover_unknown_supersession()?;
        if !self.b.branch_is_main() {
            let refusal = Refusal::new(
                "this clone is not on main",
                "release from a clean clone on main (git switch main)",
            );
            return if self.supersede.is_some() {
                self.refuse_supersede(refusal)
            } else {
                self.refuse_hard(refusal)
            };
        }
        if let Err(refusal) = self.b.prepare_clone() {
            return if self.supersede.is_some() {
                self.refuse_supersede(refusal)
            } else {
                self.refuse_hard(refusal)
            };
        }
        self.tip = match self.b.rev("origin/main^{commit}") {
            Some(tip) => tip,
            None => {
                let refusal = Refusal::new(
                    "this clone has no local origin/main candidate",
                    "git fetch origin, then run the release again",
                );
                return if self.supersede.is_some() {
                    self.refuse_supersede(refusal)
                } else {
                    self.refuse_hard(refusal)
                };
            }
        };
        self.choose_candidate(true)?;
        let r = self.commit_r();
        let journaled = self
            .journal
            .as_ref()
            .filter(|j| !j.finished())
            .map(|j| j.prover.clone());
        let prover = match self.b.prover(journaled.as_deref()) {
            Ok(prover) => prover,
            Err(why) => {
                let refusal = Refusal::new(
                    format!("no Trust prover: {why}"),
                    "restore the locked Trust toolchain and retry",
                );
                return if self.supersede.is_some() {
                    self.refuse_supersede(refusal)
                } else {
                    self.refuse_hard(refusal)
                };
            }
        };
        if let Err(why) = self.b.lease(&prover) {
            let refusal = Refusal::new(why, "restore the toolchain lease and retry");
            return if self.supersede.is_some() {
                self.refuse_supersede(refusal)
            } else {
                self.refuse_hard(refusal)
            };
        }
        self.prover = Some(prover);

        if self.opts.supersede && !self.supersession_written() {
            let replacement = self.supersede.as_ref().map_or_else(
                || crate::line::Line::start(&self.version(), &r, self.t_start),
                |txn| txn.replacement.clone(),
            );
            let (release_line, expected) = self.supersede_release_lines()?;
            let previous = match self.b.preflight_supersede(&replacement, expected.as_ref()) {
                Ok(previous) => previous,
                Err(refusal) if self.supersede.is_some() => {
                    return self.refuse_supersede(refusal);
                }
                Err(refusal) => return self.refuse_hard(refusal),
            };
            if let Some(txn) = &self.supersede {
                if txn.previous != previous || txn.replacement != replacement {
                    return self.refuse_supersede(
                        Refusal::new(
                            "the replacement transaction changed between cutter images",
                            "retry the exact --commit ... --supersede request",
                        )
                        .could_not_run(),
                    );
                }
            } else {
                self.supersede = Some(SupersedeTxn {
                    head: self.b.head(),
                    release_line,
                    previous,
                    replacement,
                    cas_unknown: false,
                });
            }
        }
        if self.supersede.is_none()
            && self.b.head() != r
            && let Err(refusal) = self.b.move_to(&r)
        {
            return if self.supersede.is_some() {
                self.refuse_supersede(refusal)
            } else {
                self.refuse_hard(refusal)
            };
        }
        if let Some(c) = self.cand.clone().filter(|c| c.carry.is_empty())
            && self.b.can_check_commit(&c)
        {
            self.tip = self.b.fetch().map_err(|r| self.halted(r))?;
            self.choose_candidate(false)?;
            let preflight = self.b.commit_gates(&c, true);
            if !preflight.found.is_empty() {
                let remedy = self.remedy(&c);
                self.refuse(preflight_blocked(
                    format!(
                        "the preflight BLOCKED ({} finding(s))",
                        preflight.found.len()
                    ),
                    &preflight.found,
                    &remedy,
                ))?;
            }
            // What the passing preflight owes the operator: a bump still to be written names
            // the harness versions its commit writes, before that commit is pushed.
            for note in preflight.notes {
                self.say(&format!("release: {note}"));
            }
        }
        // A bump needs the publication engine. Dispatch to its base's content-proved cutter
        // first, so no previous line's engine discovery can hold the next line hostage.
        if self.cand.as_ref().is_some_and(|c| c.bump_from.is_some()) {
            self.own_cutter()?;
            if let Err(refusal) = self.b.sync() {
                return self.refuse_hard(refusal);
            }
        }
        if let Err(halt) = self.commit(true) {
            return if self.supersede.is_some() {
                self.halt_supersede(halt)
            } else {
                Err(halt)
            };
        }
        if self.supersede.is_some()
            && self.b.head() != r
            && let Err(refusal) = self.b.move_to(&r)
        {
            return self.refuse_supersede(refusal);
        }
        if let Err(halt) = self.own_cutter() {
            return if self.supersede.is_some() {
                self.halt_supersede(halt)
            } else {
                Err(halt)
            };
        }
        if self.opts.supersede && !self.supersession_written() {
            let mut txn = self.supersede.clone().ok_or(Halt::Said(1))?;
            txn.cas_unknown = true;
            self.supersede = Some(txn.clone());
            if let Some(journal) = self.journal.as_mut() {
                journal.supersede = Some(txn.clone());
            }
            if let Err(halt) = self.save_journal() {
                return self.halt_supersede(halt);
            }
            let note = match self.b.supersede_line(&txn.previous, &txn.replacement) {
                SupersedeResult::Committed(note) => note,
                SupersedeResult::Refused {
                    why,
                    winner: Some(winner),
                    ..
                } => {
                    return self.stop_supersede_at_winner(why, winner);
                }
                SupersedeResult::Refused { why, .. } => {
                    return self.stop_supersede(why);
                }
                SupersedeResult::Unknown(why) => {
                    let message = format!(
                        "COULD NOT RUN: the supersession write's result is unknown: {why}; \
                         candidate.toml retains {} for an exact retry",
                        short(txn.replacement.head())
                    );
                    self.say(&message);
                    self.last = message;
                    return Err(Halt::Said(3));
                }
            };
            self.finish_supersede_commit(txn, note)?;
        }
        if self.opts.supersede && self.supersession_written() {
            self.opts.supersede = false;
            self.b.supersession_completed();
        }
        // A private qualification owns only this clone and its selected source. It must not
        // adopt or claim a same-version public release line before judging that source.
        if self.opts.qualify {
            return Ok(());
        }
        // Fleet discovery also belongs to a frozen image, but must precede judgment: adopting
        // another head after checks would otherwise judge once with the wrong cutter.
        let pending = self
            .journal
            .clone()
            .filter(|j| j.has(PENDING_LINE) && j.supersede.is_none())
            .map(|j| j.line());
        let pending_fixes: Vec<String> = pending.as_ref().map_or_else(Vec::new, |line| {
            line.hops
                .iter()
                .filter(|hop| !self.b.is_ancestor(&hop.commit, &self.tip.clone()))
                .map(|hop| hop.carries.clone())
                .collect()
        });
        let mut selected = self.cand.clone().ok_or(Halt::Said(1))?;
        let established = Journal::load(&self.b.release_dir().join(JOURNAL_FILE))
            .map_err(|why| self.halted(Refusal::new(why, "restore the judged release journal")))?
            .is_some_and(|j| !j.finished() && j.version == selected.version)
            || pending.is_some();
        let mut following = if established {
            self.journal.clone()
        } else {
            None
        };
        if let Err(refusal) = crate::line_claim::follow(self.b, &mut selected, &mut following) {
            return self.refuse_hard(refusal);
        }
        if selected.commit != self.commit_r() || established {
            self.journal = following;
        } else if let (Some(journal), Some(following)) = (self.journal.as_mut(), following) {
            journal.line = following.line;
        }
        if let Err(why) = crate::line_claim::select(
            self.b,
            self.claim.as_ref().is_some_and(|c| c.continued),
            &mut selected,
            &mut self.journal,
            self.t_start,
        ) {
            return self.stop_pending_selection(why, pending.as_ref());
        }
        if selected.commit == self.commit_r() {
            self.settle_line_selection();
        } else {
            self.fleet_selection(&selected);
        }
        if selected.commit != self.commit_r() {
            if let Some(pending) = pending
                && let Err(rewind) = self.b.rewind_selection(&pending)
            {
                let after = self.b.head();
                return self.refuse_hard(
                    Refusal::new(
                        format!(
                            "the fleet selected a different release line, but restoring the \
                             clone from its prior pending head failed: {}",
                            rewind.why
                        ),
                        format!(
                            "{}; {SELECTION_FILE} retains pending line {} and main remains at \
                             {} — repair the checkout, then retry {RELEASE_COMMAND}",
                            rewind.remedy,
                            short(pending.head()),
                            short(&after)
                        ),
                    )
                    .detail(rewind.detail)
                    .could_not_run(),
                );
            }
            if !pending_fixes.is_empty() {
                let line = self
                    .journal
                    .as_ref()
                    .map(Journal::line)
                    .ok_or(Halt::Said(1))?;
                for fix in pending_fixes {
                    if self.b.is_ancestor(&fix, line.head()) {
                        continue;
                    }
                    match self.b.fix_of(&line, &fix) {
                        Ok(crate::line::Admitted::Fix(fix)) => selected.carry.push(fix),
                        Ok(crate::line::Admitted::Carried) => {}
                        Err(refusal) => return self.refuse_hard(refusal),
                    }
                }
            }
            self.cand = Some(selected);
            let commit = self.commit_r();
            if let Err(refusal) = self.b.move_to(&commit) {
                return self.refuse_hard(refusal);
            }
            let prover = self
                .b
                .prover(None)
                .map_err(|why| self.halted(Refusal::new(why, "restore the selected toolchain")))?;
            if let Err(why) = self.b.lease(&prover) {
                return self.refuse_hard(Refusal::new(why, "restore the selected toolchain lease"));
            }
            self.prover = Some(prover);
            self.commit(true)?;
            self.own_cutter()?;
        }
        // A selection the fleet claim kept is said here, where no second own_cutter says it.
        self.announce_line_selection(false);
        Ok(())
    }

    fn choose_candidate(&mut self, selecting: bool) -> Flow {
        let _timing = crate::cut_timing::Timer::start(
            &self.b.log_path(Step::Checks),
            "candidate-version-tags-ledger",
        );
        let tip = self.tip.clone();
        let j = self.journal.clone();
        let unfinished = j.as_ref().filter(|j| !j.finished());
        let continued = self.claim.as_ref().is_some_and(|c| c.continued);
        let mut cand = if !selecting {
            self.cand.clone().ok_or(Halt::Said(1))?
        } else if let Some(j) = j.as_ref().filter(|j| continued && !j.finished()) {
            Candidate {
                commit: j.commit.clone(),
                version: j.version.clone(),
                why: "the candidate this run chose before it re-ran as its own cutter".into(),
                bump_from: None,
                carry: Vec::new(),
            }
        } else if !self.opts.fix.is_empty() {
            self.fix_candidate(unfinished, selecting)?
        } else if let Some(rev) = self.opts.commit.clone() {
            let Some(r) = self.b.rev(&format!("{rev}^{{commit}}")) else {
                return self.refuse_hard(Refusal::new(
                    format!("--commit {rev} is not a commit in this clone"),
                    "git fetch origin, and name a commit on origin/main",
                ));
            };
            if let Some(j) = unfinished
                && j.commit != r
                && !self.opts.supersede
            {
                if !selecting {
                    self.refuse_published(j, "--commit")?;
                }
                let line = j.line();
                return self.refuse_hard(Refusal::new(
                    format!(
                        "this clone's release is line v{} at {} (base {}, {} hop(s)), \
                         and --commit only starts a line",
                        line.version,
                        short(line.head()),
                        short(&line.base),
                        line.hops.len()
                    ),
                    format!(
                        "carry the landed fix onto it: {RELEASE_COMMAND} --fix <its commit>; or \
                         drop the line: {RELEASE_COMMAND} --forget, then {RELEASE_COMMAND} \
                         --commit {rev}"
                    ),
                ));
            }
            Candidate {
                commit: r,
                version: String::new(),
                why: "--commit".to_string(),
                bump_from: None,
                carry: Vec::new(),
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
                carry: Vec::new(),
            }
        } else if let Some(j) = j.as_ref().filter(|j| {
            !self.opts.bump && self.b.version_at(&tip).as_deref() == Some(j.version.as_str())
        }) {
            Candidate {
                commit: j.commit.clone(),
                version: j.version.clone(),
                why: "this clone's finished release; --bump starts the next".to_string(),
                bump_from: None,
                carry: Vec::new(),
            }
        } else if self.opts.bump {
            let v0 = self.b.version_at(&tip).unwrap_or_default();
            if public_version(&v0).is_none() {
                return self.refuse_hard(Refusal::new(
                    format!("origin/main's version '{v0}' is not a canonical product release"),
                    "fix [workspace.package] version on main",
                ));
            }
            let used = if selecting {
                self.b.local_tag(&format!("v{v0}")) || self.b.ledger_versions(&tip).contains(&v0)
            } else {
                self.version_used(&v0, &tip)
            };
            if !used {
                return self.refuse_hard(Refusal::new(
                    format!(
                        "origin/main already declares {v0}, which no release has used (no tag, \
                         no ledger line, not published) — a bump would skip it"
                    ),
                    "release it as it is: drop --bump",
                ));
            }
            let v = match crate::publish::allocate_release_after(&v0, self.t_start) {
                Ok(version) => version,
                Err(error) => return self.refuse_hard(Refusal::new(
                    format!("cannot allocate after {v0}: {error}"),
                    "restore the UTC clock or advance the product source through its canonical version claim",
                )),
            };
            Candidate {
                commit: tip.clone(),
                why: format!("--bump {v0} -> {v} on origin/main's tip {}", short(&tip)),
                version: v,
                bump_from: Some(v0),
                carry: Vec::new(),
            }
        } else {
            Candidate {
                commit: tip.clone(),
                version: String::new(),
                why: "origin/main's tip".to_string(),
                bump_from: None,
                carry: Vec::new(),
            }
        };

        if selecting {
            if cand.version.is_empty() {
                cand.version = self.b.version_at(&cand.commit).unwrap_or_default();
            }
            if public_version(&cand.version).is_none() {
                return self.refuse_hard(Refusal::new(
                    format!(
                        "{} has no historical MAJOR.MINOR.0 or calendar 1.YYYYMMDD.N product version",
                        short(&cand.commit)
                    ),
                    "select a release commit with --commit",
                ));
            }
            self.cand = Some(cand);
            return Ok(());
        }

        if !self.b.is_ancestor(&cand.commit, &tip)
            && !self.pending_line_heads(&cand.commit)
            && !(self.dry && cand.bump_from.is_some())
        {
            self.cand = Some(cand.clone());
            let hop = self.heads_a_hop(&cand.commit);
            let remedy = if hop {
                lost_record()
            } else if cand.bump_from.is_some() {
                format!("bump origin/main as it is now: {}", self.again(&cand))
            } else {
                format!("land it on main first, then re-run {}", self.again(&cand))
            };
            return self.refuse_hard(
                Refusal::new(
                    format!(
                        "{} is not on origin/main — the engine stages only mainline commits",
                        short(&cand.commit)
                    ),
                    remedy,
                )
                .lost_line_record_if(hop),
            );
        }
        if cand.version.is_empty() {
            cand.version = self.b.version_at(&cand.commit).unwrap_or_default();
        }
        self.cand = Some(cand.clone());
        if let Some(evidence) = self.b.main_lane(&cand.commit) {
            self.b.say(&evidence);
        }
        let (r, v) = (cand.commit.clone(), cand.version.clone());
        if self.opts.supersede && !continued {
            if self.version_used(&v, &tip) {
                self.refuse(Refusal::new(
                    format!("v{v} has public release state and cannot be superseded"),
                    "finish the existing release",
                ))?;
            }
            if let Some(j) = unfinished {
                self.refuse_published(j, "--supersede")?;
            }
        }
        if public_version(&v).is_none() {
            let remedy = format!(
                "release the next product version: {}",
                self.start_remedy("--bump")
            );
            self.refuse(Refusal::new(
                format!(
                    "{} declares version '{v}', not a historical MAJOR.MINOR.0 or calendar 1.YYYYMMDD.N",
                    short(&r)
                ),
                remedy,
            ))?;
        }

        if let Some(prev) = self.prev_release(&r, &v) {
            if !newer(&v, &prev) {
                let remedy = format!(
                    "release the next product version: {}",
                    self.start_remedy("--bump")
                );
                self.refuse(Refusal::new(
                    format!("v{v} is not newer than v{prev}, the release it would succeed"),
                    remedy,
                ))?;
            }
            let claim = self.b.origin_tag(&format!("v{prev}")).unwrap_or_default();
            if claim.is_empty() || !self.b.is_ancestor(&claim, &r) {
                let remedy = format!(
                    "release a commit of origin/main after that claim: {}",
                    self.start_remedy("--commit origin/main")
                );
                self.refuse(Refusal::new(
                    format!(
                        "{} does not contain v{prev}'s claim commit {}, so the release it \
                         succeeds is not in its history",
                        short(&r),
                        short(&claim)
                    ),
                    remedy,
                ))?;
            }
        }

        if self.b.origin_tag(&format!("v{v}")).is_some() && !self.claimed() {
            let remedy = match &cand.bump_from {
                Some(v0) => format!(
                    "origin/main still declares {v0}: find what cut v{v} (git ls-remote --tags \
                     origin v{v}) and bring main's version past it, then {RELEASE_COMMAND} --bump"
                ),
                None => format!(
                    "release the next product version: {}",
                    self.start_remedy("--bump")
                ),
            };
            self.refuse(Refusal::new(
                format!("v{v} is already cut (its tag is on origin), and this clone journals no cut of it"),
                remedy,
            ))?;
        }
        if self.b.origin_tag(&format!("v{v}")).is_none()
            && self.b.local_tag(&format!("v{v}"))
            && !self.claimed()
        {
            let again = self.again(&cand);
            self.refuse(Refusal::new(
                format!("tag v{v} exists in this clone and not on origin — a leftover"),
                format!("git tag -d v{v}, then re-run {again}"),
            ))?;
        }
        if cand.bump_from.is_some() {
            return Ok(());
        }

        let rows = self.b.engine_rows(Some(&r));
        if let Some(newest) = rows.newest.clone()
            && !rows.has
            && self.b.version_at(&newest).as_deref() == Some(v.as_str())
            && self.b.origin_tag(&format!("v{v}")).is_none()
        {
            let remedy = format!(
                "finish that release: {}; or release the next product version: {}",
                self.start_remedy(&format!("--commit {newest}")),
                self.start_remedy("--bump")
            );
            self.refuse(Refusal::new(
                format!(
                    "v{v}'s source was published from {}, not {} — the cut releases the \
                     published commit",
                    short(&newest),
                    short(&r)
                ),
                remedy,
            ))?;
        }
        if let Some(r) = self.newest_row_refusal() {
            self.refuse(r)?;
        }
        Ok(())
    }

    /// The one rule for what a refused preflight, cut gate or input judgment names ([`blocked`],
    /// [`InputFinding`]), read from candidate `c`'s real state. A finding on this machine is
    /// fixed, then [`Run::again`] runs. A finding in the source is fixed by:
    /// * for a bump still to be written (`bump_from`), which heads no line: landing the fix on
    ///   main, where the next `--bump` writes its bump;
    /// * for a candidate whose cut is claimed and unfinished: withdrawing that cut first
    ///   (`--abandon`), since its version cannot change and checks() refuses any other
    ///   version's run while it stands, then landing the fix on main and releasing the next
    ///   product version;
    /// * for a candidate whose source is published, or whose cut finished, which cannot change:
    ///   landing the fix on main and releasing the next product version;
    /// * for a supersession's replacement, until the supersession is written
    ///   ([`Run::superseding`]): landing the fix on main and superseding the line again at
    ///   origin/main, which holds it, after `--forget` drops the replacement's transaction where
    ///   candidate.toml retains it (every other run refuses it);
    /// * for this clone's unfinished line, which `c` heads (a written supersession's line
    ///   included, which a rerun of its flags resumes): `--fix`, which carries the fix onto it
    ///   ([`fix_remedy`]);
    /// * for any other candidate, which starts a line no journal holds yet (origin/main's tip or
    ///   `--commit`, refused before its journal): landing the fix on main and starting the line
    ///   at a commit that holds it.
    fn remedy(&mut self, c: &Candidate) -> Remedy {
        let again = self.again(c);
        let cut = self.b.cut_journal().filter(|cut| cut.version == c.version);
        let source = if c.bump_from.is_some() {
            format!("land the fix on main, then {again}")
        } else if let Some(cut) = cut.as_ref().filter(|cut| !cut.finished) {
            format!(
                "withdraw its claimed cut first: {RELEASE_COMMAND} --abandon v{}; then land the \
                 fix on main and release the next product version: {}",
                cut.version,
                self.start_remedy("--bump")
            )
        } else if cut.is_some() || self.b.engine_rows(Some(&c.commit)).has {
            format!(
                "land the fix on main, then release the next product version: {}",
                self.start_remedy("--bump")
            )
        } else if self.superseding() {
            let restart = format!("{RELEASE_COMMAND} --commit origin/main --supersede");
            if self.journal.as_ref().is_some_and(|j| j.supersede.is_some()) {
                format!("land the fix on main, then {RELEASE_COMMAND} --forget, then {restart}")
            } else {
                format!("land the fix on main, then {restart}")
            }
        } else if self.heads_line(c) {
            fix_remedy(&c.commit)
        } else {
            format!("land the fix on main, then {}", self.start_remedy(""))
        };
        Remedy { source, again }
    }

    /// The run that selects candidate `c` again once a finding on this machine is fixed: the
    /// selection that chose it. For a bump still to be written, `--bump` (a bare run would
    /// re-check the finished release the bump succeeds, or refuse the version main still
    /// declares); for a supersession until it is written ([`Run::superseding`]), `--commit c
    /// --supersede` (candidate.toml's undecided transaction refuses every other run); while
    /// this clone's unfinished line is `c` (a written supersession's included), the bare run;
    /// else `--commit c` when that chose it, or the bare run ([`Run::start_remedy`]: after
    /// `--forget` while another line is unfinished).
    fn again(&self, c: &Candidate) -> String {
        if c.bump_from.is_some() {
            self.start_remedy("--bump")
        } else if self.superseding() {
            format!("{RELEASE_COMMAND} --commit {} --supersede", c.commit)
        } else if self.heads_line(c) {
            RELEASE_COMMAND.to_string()
        } else if self.opts.commit.is_some() {
            format!("{RELEASE_COMMAND} --commit {}", c.commit)
        } else {
            self.start_remedy("")
        }
    }

    /// This run replaces the fleet's line: `--commit … --supersede`, until the supersession
    /// is written. That is prepare_candidate's own guard: once this clone's journal holds the
    /// written supersession of the line this request names ([`Run::supersession_written`]), a
    /// rerun of the same flags skips it and resumes the line it wrote, though `opts.supersede`
    /// stays set until that rerun passes its supersession block (and, in a dry rerun, for the
    /// whole run).
    fn superseding(&self) -> bool {
        self.opts.commit.is_some() && self.opts.supersede && !self.supersession_written()
    }

    /// This clone's journal holds the written supersession (`SUPERSEDED_LINE`) of the line this
    /// request names: the journal is at the selected candidate. The fact belongs to the
    /// replacement head that wrote it, since every change of a journal's head drops it
    /// (`carried_journal`, `line_claim`'s adoption, and `commit`'s journal of another head), so
    /// it answers only a request at that head. There, unfinished, a rerun resumes the line it
    /// wrote; finished, a rerun of the completed request re-checks that release where it reaches
    /// this guard (a launcher that is not continued first refuses `--supersede` of a version with
    /// public release state, [`Run::version_used`]). At any other
    /// commit, finished or not, `--commit Y --supersede` writes its own supersession, of this
    /// clone's unfinished line as of any other fleet line. It needs the candidate, so nothing
    /// reads it before `choose_candidate` selects one.
    fn supersession_written(&self) -> bool {
        self.journal.as_ref().is_some_and(|journal| {
            journal.has(SUPERSEDED_LINE) && journal.commit == self.commit_r()
        })
    }

    /// This clone's unfinished journal is at `c`: the line `c` heads.
    fn heads_line(&self, c: &Candidate) -> bool {
        self.journal
            .as_ref()
            .is_some_and(|j| !j.finished() && j.commit == c.commit)
    }

    /// `{RELEASE_COMMAND} {args}` (the bare run when `args` is empty), which starts a line:
    /// after `--forget` while this clone holds an unfinished line, which neither `--commit` (it
    /// refuses) nor `--bump` (it resumes the line) replaces.
    fn start_remedy(&self, args: &str) -> String {
        let start = if args.is_empty() {
            RELEASE_COMMAND.to_string()
        } else {
            format!("{RELEASE_COMMAND} {args}")
        };
        if self.journal.as_ref().is_some_and(|j| !j.finished()) {
            format!("{RELEASE_COMMAND} --forget, then {start}")
        } else {
            start
        }
    }

    /// `commit` heads this clone's unfinished line past its base: a hop, which origin/main
    /// holds only through the line's record merge.
    fn heads_a_hop(&self, commit: &str) -> bool {
        self.journal
            .as_ref()
            .is_some_and(|j| !j.finished() && j.commit == commit && !j.line().hops.is_empty())
    }

    /// `commit` is a private hop this clone's own pending line made, not an owner's unpushed
    /// work: a hop of the in-memory pending line (a dry plan's carried line extends it), or of
    /// candidate.toml's durable one (an interrupted or killed `--fix` leaves main at its head).
    /// A run resumes from it as from its line's head: the real run carries on from there.
    fn pending_hop(&mut self, commit: &str) -> bool {
        let hop = |journal: &Journal| {
            !journal.finished()
                && journal.has(PENDING_LINE)
                && journal.line().hops.iter().any(|hop| hop.commit == commit)
        };
        self.journal.as_ref().is_some_and(hop)
            || Journal::load(&self.b.release_dir().join(SELECTION_FILE))
                .ok()
                .flatten()
                .is_some_and(|journal| hop(&journal))
    }

    /// `commit` is the exact locally constructed head whose record merge the frozen head still
    /// owes. It is intentionally absent from origin/main until [`Run::record_selected_line`].
    fn pending_line_heads(&self, commit: &str) -> bool {
        self.journal.as_ref().is_some_and(|journal| {
            !journal.finished()
                && journal.has(PENDING_LINE)
                && journal.commit == commit
                && journal.line().head() == commit
        })
    }

    /// [`Run::refuse_hard`]'s halt, for a caller that returns a value when it goes on.
    fn halted(&mut self, r: Refusal) -> Halt {
        self.refuse_hard(r).err().unwrap_or(Halt::Said(1))
    }

    /// Refuses when the release `j` journals is published or claimed: `flag` cannot change a
    /// line the cut releases (the candidate is set to it, for the final line's public state).
    fn refuse_published(&mut self, j: &Journal, flag: &str) -> Flow {
        let published = self.b.engine_rows(Some(&j.commit)).has;
        let claimed = self.b.cut_journal().is_some_and(|c| c.version == j.version);
        if !published && !claimed {
            return Ok(());
        }
        self.cand = Some(Candidate {
            commit: j.commit.clone(),
            version: j.version.clone(),
            why: String::new(),
            bump_from: None,
            carry: Vec::new(),
        });
        self.public_state();
        self.refuse_hard(Refusal::new(
            format!(
                "the release in progress here (v{} from {}) is already published — the cut \
                 releases that commit",
                j.version,
                short(&j.commit)
            ),
            format!(
                "finish it: {RELEASE_COMMAND} without {flag}; or drop it and release the next \
                 product version: {RELEASE_COMMAND} --forget, then --bump"
            ),
        ))
    }

    /// `--fix`'s candidate: the head of this clone's unfinished line, adopting the fleet's
    /// line when the journal is absent. It is neither published nor claimed, and its tree
    /// knows lines ([`LINE_SOURCE`]), with each commit `--fix` names
    /// admitted ([`Backend::fix_of`]) for preparation to carry before judgment. One a hop carries already is dropped;
    /// when every one is, the run resumes the line as it is.
    fn fix_candidate(
        &mut self,
        unfinished: Option<&Journal>,
        selecting: bool,
    ) -> Result<Candidate, Halt> {
        if self.opts.qualify {
            return Err(self.halted(Refusal::new(
                "private qualification cannot carry fixes onto a fleet line",
                "select the landed source with --commit in a separate clean clone",
            )));
        }
        let mut journal = unfinished.cloned();
        if journal.is_none() {
            let mut candidate = Candidate {
                commit: self.tip.clone(),
                version: self.b.version_at(&self.tip).unwrap_or_default(),
                why: "origin/main's tip".into(),
                bump_from: None,
                carry: Vec::new(),
            };
            crate::line_claim::follow(self.b, &mut candidate, &mut self.journal)
                .map_err(|refusal| self.halted(refusal))?;
            journal = self.journal.clone().filter(|journal| !journal.finished());
            if journal.is_some() {
                self.fleet_selection(&candidate);
            }
        }
        let Some(j) = journal.as_ref() else {
            return Err(self.halted(Refusal::new(
                "--fix carries landed fixes onto this clone's unpublished line, and this clone \
                 has none (no release journal, or a finished one)",
                format!(
                    "start a line: {RELEASE_COMMAND} (origin/main's tip), {RELEASE_COMMAND} \
                     --commit <rev> or {RELEASE_COMMAND} --bump; --fix carries a fix onto it \
                     after a red"
                ),
            )));
        };
        if !selecting {
            self.refuse_published(j, "--fix")?;
        } else if self
            .b
            .cut_journal()
            .is_some_and(|cut| cut.version == j.version)
        {
            return Err(self.halted(Refusal::new(
                "the release in progress is already published or claimed and cannot carry a fix",
                "finish this release and start the next line",
            )));
        }
        let line = j.line();
        let head = line.head().to_string();
        let knows_lines = match self.b.tree_has(&head, LINE_SOURCE) {
            Ok(has) => has,
            Err(refusal) => return Err(self.halted(refusal)),
        };
        if !knows_lines {
            return Err(self.halted(Refusal::new(
                format!(
                    "the line's head {} predates release lines (its tree has no {LINE_SOURCE}): \
                     its own cutter, which a hop's run executes as, keeps no line",
                    short(&head)
                ),
                format!("drop the line: {RELEASE_COMMAND} --forget, then start one on origin/main"),
            )));
        }
        let mut carry = Vec::new();
        for rev in self.opts.fix.clone() {
            match self.b.fix_of(&line, &rev) {
                Ok(crate::line::Admitted::Fix(fix)) => carry.push(fix),
                Ok(crate::line::Admitted::Carried) => {}
                Err(refusal) => return Err(self.halted(refusal)),
            }
        }
        let at = format!(
            "line v{} at {} (base {}, {} hop(s))",
            line.version,
            short(&head),
            short(&line.base),
            line.hops.len()
        );
        let why = if carry.is_empty() {
            format!("{at}: every commit --fix names is carried already, so it resumes as it is")
        } else {
            let named: Vec<&str> = carry.iter().map(|fix| short(&fix.commit)).collect();
            format!("{at}; step 3 carries {} onto it", named.join(", "))
        };
        Ok(Candidate {
            commit: head,
            version: j.version.clone(),
            why,
            bump_from: None,
            carry,
        })
    }

    fn est_or_default(&mut self, step: Step) -> u64 {
        if self.est.is_empty() {
            let tsv = self.b.times();
            self.est = estimates(&tsv);
        }
        self.est_secs(step)
    }

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

    fn prev_release(&mut self, rev: &str, version: &str) -> Option<String> {
        let versions = self.b.ledger_versions(rev);
        versions
            .iter()
            .rev()
            .filter(|v| v.as_str() != version)
            .find(|v| self.b.origin_tag(&format!("v{v}")).is_some())
            .cloned()
    }

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

    /// The constructing cutter's pending identity, used in memory by a dry run and saved in
    /// candidate.toml by real preparation. No facts of the previous head belong to this one.
    fn carried_journal(
        &mut self,
        candidate: &Candidate,
        line: crate::line::Line,
        prover: &Prover,
    ) -> Journal {
        let mut asks = self
            .journal
            .as_ref()
            .map(|journal| journal.asks.clone())
            .unwrap_or_default();
        for key in &self.early_asks {
            if !asks.contains(key) {
                asks.push(key.clone());
            }
        }
        Journal {
            version: candidate.version.clone(),
            commit: candidate.commit.clone(),
            prover: prover.bin.display().to_string(),
            trustc: prover.commit.clone(),
            started: self.b.now(),
            linux: self.linux.clone(),
            done: vec![PENDING_LINE.into()],
            asks,
            line: Some(line),
            ..Journal::default()
        }
    }

    fn commit(&mut self, selecting: bool) -> Result<String, Halt> {
        let Some(mut cand) = self.cand.clone() else {
            return Ok(String::new());
        };
        if self.dry {
            return Ok(match &cand.bump_from {
                Some(_) => format!(
                    "would commit `release: {} — the version and the lockfile move together` on \
                     {} (pub-bump-edit, then exact path-version lockfile rewrites and locked \
                     offline metadata validation; with the harness versions its window \
                     owes, `ship harness-bump`'s writes) and push it to main; private bump {} judged at step 1",
                    cand.version,
                    short(&self.tip),
                    short(&cand.commit)
                ),
                None if !cand.carry.is_empty() => format!(
                    "would record the planned head {} on main and move this clone to it; \
                     constructed locally with git merge-tree and git commit-tree, judged at step 1; no refs changed",
                    short(&cand.commit)
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
        if self.dry
            && let Err(why) = crate::line_claim::select(
                self.b,
                self.claim.as_ref().is_some_and(|claim| claim.continued),
                &mut cand,
                &mut self.journal,
                self.t_start,
            )
        {
            let code = if why.starts_with("CUTTING ELSEWHERE:") {
                4
            } else if why.starts_with("COULD NOT RUN:") {
                3
            } else {
                2
            };
            self.last = why;
            let last = self.last.clone();
            self.say(&last);
            return Err(Halt::Said(code));
        }
        self.cand = Some(cand.clone());
        let mut carried = None;
        if !cand.carry.is_empty() {
            let mut line = self.journal.as_ref().map_or_else(
                || crate::line::Line::start(&cand.version, &cand.commit, self.t_start),
                Journal::line,
            );
            match self.b.carry(&line, &cand.carry) {
                Ok((hops, recorded)) => {
                    let first = line.hops.len() + 1;
                    let named: Vec<String> = hops
                        .iter()
                        .enumerate()
                        .map(|(i, hop)| format!("{} as hop {}", short(&hop.carries), first + i))
                        .collect();
                    line.hops.extend(hops);
                    cand.why = format!(
                        "line v{} from {}: carried {}; {recorded}",
                        line.version,
                        short(&line.base),
                        named.join(", ")
                    );
                    line.head().clone_into(&mut cand.commit);
                    bumped = false;
                    cand.carry.clear();
                    self.cand = Some(cand.clone());
                    carried = Some(line);
                }
                Err(r) => self.refuse(r)?,
            }
        }
        // A carried head is private until its content-neutral record merge lands. Persist its
        // complete retry identity before moving main: move_to can change HEAD and then refuse
        // while synchronizing the submodule or proving the checkout clean.
        if selecting
            && self.supersede.is_none()
            && let Some(line) = carried.as_ref()
        {
            let pending = self.carried_journal(&cand, line.clone(), &prover);
            let path = self.b.release_dir().join(SELECTION_FILE);
            if let Err(why) = pending.save(&path) {
                self.refuse(Refusal::new(
                    why,
                    "check the permissions of the clone's .git/aterm-release",
                ))?;
            }
            // The durable candidate now owns every ask copied above. A later move refusal must
            // retain those asks with the candidate, not withdraw them as still-unjournaled.
            self.early_asks.clear();
            self.journal = Some(pending);
        }
        if !(selecting && self.supersede.is_some())
            && self.b.head() != cand.commit
            && let Err(r) = self.b.move_to(&cand.commit)
        {
            self.refuse(r)?;
        }
        let same = self
            .journal
            .as_ref()
            .is_some_and(|j| j.commit == cand.commit);
        if !same {
            let calendar = aterm_types::release_version::ReleaseVersion::parse(&cand.version)
                .is_some_and(|v| v.is_calendar());
            let now = if calendar { self.t_start } else { self.b.now() };
            let line = carried
                .or_else(|| {
                    self.supersede
                        .as_ref()
                        .filter(|txn| txn.replacement.head() == cand.commit)
                        .map(|txn| txn.replacement.clone())
                })
                .unwrap_or_else(|| crate::line::Line::start(&cand.version, &cand.commit, now));
            let started = if calendar { line.started } else { now };
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
                started,
                linux: self.linux.clone(),
                done: if selecting && !line.hops.is_empty() {
                    vec![PENDING_LINE.into()]
                } else {
                    Vec::new()
                },
                asks,
                reserved: None,
                artifact: None,
                prepared: None,
                line: Some(line),
                supersede: selecting.then(|| self.supersede.clone()).flatten(),
            });
        } else if !self.claimed()
            && let Some(j) = self.journal.as_mut()
        {
            j.prover = prover.bin.display().to_string();
            j.trustc = prover.commit.clone();
            if self.linux.declared() {
                j.linux = self.linux.clone();
            }
            if selecting {
                j.supersede = self.supersede.clone();
            }
        }
        if !self.early_asks.is_empty()
            && let Some(j) = self.journal.as_mut()
        {
            for key in std::mem::take(&mut self.early_asks) {
                if !j.asks.contains(&key) {
                    j.asks.push(key);
                }
            }
        }
        let path = if selecting {
            self.b.release_dir().join(SELECTION_FILE)
        } else {
            self.journal_path.clone()
        };
        if !selecting
            && let Some(next) = self.journal.clone()
            && let Err(refusal) = self.withdraw_dropped(&next)
        {
            self.refuse(refusal)?;
        }
        if let Some(j) = &self.journal
            && let Err(why) = j.save(&path)
        {
            self.refuse(Refusal::new(
                why,
                "check the permissions of the clone's .git/aterm-release",
            ))?;
        }
        self.halt_if_interrupted()?;
        if !selecting {
            if let Some(r) = self.newest_row_refusal() {
                self.refuse(r)?;
            }
            self.public_state();
        }
        Ok(format!(
            "v{} at {} ({})",
            cand.version,
            short(&cand.commit),
            cand.why
        ))
    }

    fn own_cutter(&mut self) -> Flow {
        self.announce_line_selection(false);
        let r = self.commit_r();
        match self.b.own_cutter(&r) {
            Ok(_) => Ok(()),
            Err(not) => {
                let recovery = self.b.cutter_recovery(&r);
                if recovery == crate::release_tools::Recovery::Refuse
                    || (recovery == crate::release_tools::Recovery::Initial
                        && self.claim.as_ref().is_some_and(|c| c.continued)
                        && self.journal.as_ref().is_some_and(|j| j.commit == r)
                        && self.b.frozen_commit().is_none_or(|commit| commit == r))
                {
                    let refusal = Refusal::new(
                        format!(
                            "this cutter was rebuilt at {} and is still not its own ({})",
                            short(&r),
                            not.why
                        ),
                        format!("{} (nothing was claimed)", not.fix),
                    );
                    return if self.dry {
                        self.refuse_hard(refusal.could_not_run())
                    } else {
                        self.refuse(refusal)
                    };
                }
                self.halt_if_interrupted()?;
                // The selected head's cutter may predate the notice: say it before exec, even
                // where the fleet claim is still to be read.
                self.announce_line_selection(true);
                self.say(&format!(
                    "release: {} — selecting and executing {}'s own cutter ({RELEASE_COMMAND}, \
                     same arguments; an exact pre-freeze, when present, is reported below)",
                    not.why,
                    short(&r)
                ));
                let prover = self.prover.clone().ok_or(Halt::Said(1))?;
                match self.b.exec_cutter(&r, &prover) {
                    // A real backend cannot reach this edge: exec replaced it. The model uses
                    // it to represent the old image producing no final line of its own.
                    Ok(()) => Err(Halt::Said(0)),
                    Err(refusal) => {
                        self.halt_if_interrupted()?;
                        if self.dry {
                            self.refuse_hard(refusal.could_not_run())
                        } else {
                            self.refuse(refusal)
                        }
                    }
                }
            }
        }
    }

    fn fleet_selection(&mut self, candidate: &Candidate) {
        self.line_selection = Some(LineSelection {
            head: candidate.commit.clone(),
            source: format!("fleet claim refs/aterm-release/line/{}", candidate.version),
            holder: self.b.fleet_holder(&candidate.commit),
            announced: false,
            settled: true,
        });
    }

    /// The fleet claim was read and kept this run's selection.
    fn settle_line_selection(&mut self) {
        if let Some(selection) = self.line_selection.as_mut() {
            selection.settled = true;
        }
    }

    /// Say the selected line once the fleet claim has settled it. `before_exec` says it before
    /// an exec into the selected head's cutter, which may predate this diagnostic, even while
    /// the fleet claim is still to be read: then as provisional, never as the line's content.
    fn announce_line_selection(&mut self, before_exec: bool) {
        let head = self.commit_r();
        let Some(journal) = self.journal.as_ref().filter(|j| j.commit == head) else {
            return;
        };
        let Some(selection) = self
            .line_selection
            .as_ref()
            .filter(|s| !s.announced && s.head == head && (s.settled || before_exec))
        else {
            return;
        };
        if head == self.tip {
            return;
        }
        let settled = selection.settled;
        let line = journal.line();
        let source = selection.source.clone();
        let holder = selection
            .holder
            .as_deref()
            .unwrap_or("not recorded")
            .to_string();
        let main = self.tip.clone();
        let gap = self.b.main_past_line(&head, &main).map_or_else(
            || "comparison with this head unavailable from local history".to_string(),
            |(all, tools)| format!("past this head by {all} commits, {tools} of them touching crates/aterm-release or crates/aterm-verify"),
        );
        // A claimed or published line takes no fix and cannot be superseded (fix_candidate,
        // refuse_published): offer neither for it.
        let public = if self.claimed() {
            Some("claimed")
        } else if self.b.engine_rows(Some(&head)).has {
            Some("published")
        } else {
            None
        };
        let next = public.map_or_else(
            || format!("Carry fixes onto the line: {RELEASE_COMMAND} --fix <c>...; or start the line again at main: {RELEASE_COMMAND} --commit {main} --supersede"),
            |state| format!("The {state} release resumes as it is: no fix is carried onto it, and it cannot be superseded"),
        );
        let content = if settled {
            "This release uses the line's content.".to_owned()
        } else {
            format!(
                "This selection is provisional: {}'s cutter reads the fleet claim of v{} next \
                 and may select another line.",
                short(&head),
                line.version
            )
        };
        self.say(&format!(
            "release: adopted/continued line v{} (base {}) from {source}; holder {holder}; head {head}; origin/main {main}: {gap}. {content} {next}",
            line.version, line.base,
        ));
        if let Some(selection) = self.line_selection.as_mut() {
            selection.announced = true;
        }
    }

    fn refresh(&mut self) {
        let _timing =
            crate::cut_timing::Timer::start(&self.b.log_path(self.cur), "release facts refresh");
        let Some(c) = self.cand.clone() else {
            return;
        };
        if self.dry && (c.bump_from.is_some() || !c.carry.is_empty()) {
            let pending = if c.bump_from.is_some() {
                "the private bump has no release records yet"
            } else {
                "the planned head has no release records yet"
            };
            self.facts = Facts {
                built: Answer::no(pending),
                checks: crate::check_record::Checks::no(pending),
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
        if facts.cut.is_none()
            && let Some(spent) = self.spent()
        {
            let stale = format!("{}; step 4 reserves anew and builds again", spent.words());
            facts.built = Answer::no(stale.clone());
            facts.checks = facts.checks.bundle_stale(stale);
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

    /// What step 4 runs, by the records that stand, and what it skips, in words: AppKit first and
    /// alone, then the graph's export unless R is staged (and always under `--qualify`, which
    /// checks it without a push), build unless its bundle is built and its handoff fixtures
    /// staged, and tree half unless every tree check's record stands.
    fn qualify_plan(&self) -> (Plan, Vec<String>) {
        let mut plan = Plan::default();
        let mut done = Vec::new();
        if self.opts.qualify && self.exported_locally {
            done.push("R's export checked locally in this run".to_string());
        } else if self.opts.qualify {
            plan.export = Some(StageTarget::Local);
        } else if let Some(why) = self.skip_stage() {
            done.push(why);
        } else {
            plan.export = Some(StageTarget::Remote);
        }
        let bundle = !self.facts.built.yes;
        let fixtures = !self.facts.fixtures.yes && !self.journal_has(FIXTURES_STAGED);
        if bundle || fixtures {
            plan.build = Some(BuildWork { bundle, fixtures });
        }
        if !bundle {
            done.push(self.facts.built.words.clone());
        }
        if self.facts.fixtures.yes {
            done.push(self.facts.fixtures.words.clone());
        } else if !fixtures {
            done.push("its handoff fixtures staged after it (this release's journal)".to_string());
        }
        let tree = self.facts.checks.run(Half::Tree);
        if tree.checks.is_empty() {
            done.push("every tree check's record of R stands under this seal".to_string());
        } else {
            plan.tree = Some(tree);
        }
        let appkit = self
            .facts
            .checks
            .run_only(Half::Bundle, &[aterm_verify::checks::Check::Appkit]);
        if appkit.checks.is_empty() {
            done.push("appkit's record of R stands under this seal".to_string());
        } else {
            plan.appkit = Some(appkit);
        }
        (plan, done)
    }

    /// Why the plan counts step 4 done: the cut is claimed, or R's preflight passed and every
    /// branch's record stands. Unless the cut is claimed, step 4 still runs its preflight: the
    /// FAIL rule, a PREPARED release's second look, the disk.
    fn skip_qualify(&self) -> Option<String> {
        self.claimed_words().or_else(|| {
            let (plan, done) = self.qualify_plan();
            (plan.is_empty() && self.journal_has(COMMIT_GATES)).then(|| {
                format!(
                    "{}; its preflight passed for it (this release's journal)",
                    done.join("; ")
                )
            })
        })
    }

    /// Why step 5 runs nothing: the cut is claimed, or measure's bundle-dependent record stands.
    /// A real run whose early AppKit record vanished enters step 5 only to refuse it there; a dry
    /// plan does not invent records, so its step-5 decision deliberately ignores AppKit.
    fn skip_bundle(&self) -> Option<String> {
        self.claimed_words().or_else(|| {
            let checks = &self.facts.checks;
            let appkit_missing = !checks
                .run_only(Half::Bundle, &[aterm_verify::checks::Check::Appkit])
                .checks
                .is_empty();
            if appkit_missing && !self.dry {
                return None;
            }
            checks
                .run_only(Half::Bundle, &[aterm_verify::checks::Check::Measure])
                .checks
                .is_empty()
                .then(|| checks.line().to_owned())
        })
    }

    fn reservation(&self) -> Option<Reservation> {
        self.r_journal().and_then(|j| j.reserved.clone())
    }

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

    fn artifact(&self) -> Option<Artifact> {
        self.r_journal().and_then(|j| j.artifact.clone())
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
                Step::Qualify => self.skip_qualify().is_some(),
                Step::Bundle => self.skip_bundle().is_some(),
                Step::Ready => self.claimed_words().is_some(),
                Step::Publish => {
                    self.skip_publish().is_some() && !matches!(self.cut_mode(), CutMode::Fresh)
                }
                Step::Cut => matches!(self.cut_mode(), CutMode::Done(_)),
                Step::AfterCut => self.after_cut_complete() && self.facts.fixtures.yes,
                _ => false,
            };
            if skipped {
                done.push(step.name());
            } else {
                run.push(*step);
            }
        }
        if self.opts.qualify {
            run.retain(|s| *s <= Step::Ready);
        } else if self.opts.prepare {
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
        if run.contains(&Step::Qualify) {
            let (plan, _) = self.qualify_plan();
            let branches: Vec<&str> = plan.branches().into_iter().map(Branch::name).collect();
            self.say(&format!(
                "release: {} runs {}at once: {} (about {})",
                Step::Qualify.at(),
                if plan.appkit.is_some() {
                    "appkit first and alone, then "
                } else {
                    ""
                },
                if branches.is_empty() {
                    "nothing".to_string()
                } else {
                    branches.join(" ")
                },
                fmt_dur(self.est_secs(Step::Qualify))
            ));
        }
        if run.contains(&Step::Publish) {
            let sides: Vec<&str> = self.publish_sides().into_iter().map(Side::name).collect();
            self.say(&format!(
                "release: {} runs {}: {} (about {}); the cut claims once {} passed",
                Step::Publish.at(),
                if sides.len() > 1 { "at once" } else { "alone" },
                sides.join(" "),
                fmt_dur(self.est_secs(Step::Publish)),
                if sides.len() > 1 { "both" } else { "it" }
            ));
        }
        if self.opts.qualify {
            self.say("release: --qualify stops after ready; the export is checked with pub stage --dry-run, without pushing its snapshot");
        } else if self.opts.prepare {
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
        let at = self.b.hms(now.saturating_add(self.plan_secs));
        self.say(&format!(
            "release: ETA {} — {} of work, by {} (a wait for the owner is not in it)",
            at.get(..5).unwrap_or(&at),
            fmt_dur(self.plan_secs),
            if measured_here {
                "this machine's measured step times"
            } else {
                "the plan's measured defaults"
            }
        ));
    }

    /// The release's disk claim, for what `covers` names, where this run holds none and the
    /// qualify graph still needs a build or tree branch: step 4 takes it after the synchronous
    /// AppKit gate and before the graph (the export, the build and the tree half of `verify
    /// release`). A bundle half that runs without step 4 takes it before it runs. It ends as step
    /// 5 does ([`Run::let_disk_go`]). It is as much as
    /// the most any of this machine's recorded releases wrote, and never less than
    /// [`crate::release_disk::WARM`] where the clone's build is warm, or
    /// [`crate::release_disk::COLD`]. Nothing is walked to size it.
    fn disk_still_needed(&mut self, covers: &str) -> Flow {
        if self.disk_held {
            return Ok(());
        }
        let (_, warm) = self.b.disk();
        let record = self.b.disk_record();
        let bytes = crate::release_disk::need(&record, warm).bytes;
        let floor = if warm {
            crate::release_disk::WARM
        } else {
            crate::release_disk::COLD
        };
        let basis = if bytes > floor {
            "the most a recorded release on this Mac wrote"
        } else if warm {
            "the floor where the clone's build is warm"
        } else {
            "the floor where the clone's build is cold"
        };
        self.claim_disk(bytes, basis, covers, "through verify")
    }

    /// A stage of R, `stage`, under the release's disk claim where this run holds it (step 4's),
    /// else under a claim of its own for [`crate::release_disk::STAGE`], ended as the stage ends:
    /// a stage compiles in the clone's build, and the claim covers each `verify` it runs, which
    /// then walks nothing. That is a stage again before the publish where the engine moved, and
    /// one of a candidate verified earlier; neither holds a claim through what comes after it,
    /// an owner's wait included. A dry run's `stage` does nothing, and says the claim it would
    /// take.
    fn stage_claimed<T>(&mut self, stage: impl FnOnce(&mut Self) -> T) -> Result<T, Halt> {
        if self.disk_held {
            return Ok(stage(self));
        }
        self.claim_disk(
            crate::release_disk::STAGE,
            "the room a release that verified is held to",
            "this stage",
            "a stage",
        )?;
        let staged = stage(self);
        self.let_disk_go(false);
        Ok(staged)
    }

    /// Claim `bytes` of the clone's volume, as much as `basis` says, for what `covers` names,
    /// held as the release's `span` until [`Run::let_disk_go`]; a dry run says it would.
    fn claim_disk(&mut self, bytes: u64, basis: &str, covers: &str, span: &str) -> Flow {
        let gib = bytes.div_ceil(1 << 30);
        if self.dry {
            self.disk_held = true;
            self.say(&format!(
                "        disk: would claim {gib} GiB ({basis}) for {covers}, walking nothing"
            ));
            return Ok(());
        }
        let why = format!("release v{} ({span})", self.version());
        match self.b.claim_disk(bytes, &why) {
            Ok(()) => {
                self.disk_held = true;
                self.say(&format!(
                    "        disk: claimed {gib} GiB ({basis}) for {covers}"
                ));
                Ok(())
            }
            Err(refusal) => self.refuse(refusal),
        }
    }

    /// End the disk claim this run holds, if any; `passed` keeps what the run wrote to size the
    /// next release's claim ([`Backend::let_disk_go`]).
    fn let_disk_go(&mut self, passed: bool) {
        if std::mem::take(&mut self.disk_held) && !self.dry {
            self.b.let_disk_go(passed);
        }
    }

    /// Step 4, after step 1 judged R's preflight: AppKit first and alone, before the release's
    /// full disk admission, any reservation or any slice starts. Only after it passes does step 4
    /// claim the graph's disk, reserve where the build needs one, then run the qualify graph — the
    /// tree half of `verify release`, the bundle's build and the export check, at once
    /// ([`run_graph`]) — and judge each branch. An AppKit red refuses R before either admission;
    /// a graph red refuses R at once, every other branch stopped; a COULD NOT RUN lets
    /// the others finish, so their records survive for the next run. A peer's cut that reaches the
    /// reservation while the graph runs sets its bundle aside: one more round reserves anew and
    /// builds again, the tree's records and the staging kept. Concurrent export validation
    /// is private; staging follows only after all of the graph's verdicts pass.
    fn qualify(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        let mut words = Vec::new();
        let (plan, _) = self.qualify_plan();
        if plan.export.is_some() {
            words.extend(self.line_holds(&c)?);
        }
        if self.dry {
            if let Some(run) = &plan.appkit {
                self.say(&format!(
                    "        appkit: would run `{}` before the release disk claim",
                    half_command(run, None)
                ));
            }
            if self.export_alone(&plan) {
                self.stage_claimed(|_| ())?;
            }
            if plan.build.is_some() || plan.tree.is_some() {
                self.disk_still_needed("the qualify graph and the bundle half")?;
            }
            words.push(self.qualify_dry(&c, &plan)?);
            self.finish(&words.join("; "));
            return Ok(());
        }
        if let Some(appkit) = plan.appkit {
            words.push(self.qualify_appkit(&c, &p, &appkit)?);
            self.halt_if_interrupted()?;
        }
        words.extend(self.qualify_round(&c, &p)?);
        if self.facts.cut.is_none()
            && let Some(spent) = self.spent()
        {
            self.say(&format!(
                "        {} while R qualified — step 4 reserves anew and builds again",
                spent.words()
            ));
            words.extend(self.qualify_round(&c, &p)?);
        }
        self.finish(&words.join("; "));
        Ok(())
    }

    /// The load-reported, windowed release check at the front of step 4. It has no bundle input,
    /// so its canonical bundle-half command runs without `--app`; waiting for it here keeps it
    /// away from the release slices' load and guarantees that its red starts none of them.
    fn qualify_appkit(
        &mut self,
        c: &Candidate,
        p: &Prover,
        run: &crate::check_record::VerifyRun,
    ) -> Result<String, Halt> {
        self.say(&format!(
            "        appkit first, alone: {}",
            half_command(run, None)
        ));
        let log = self
            .b
            .log_path(Step::Qualify)
            .with_file_name("04-qualify-appkit.log");
        self.say(&format!("        log: {}", log.display()));
        let began = self.b.now();
        let ran = self
            .b
            .branches()
            .verify(Step::Qualify, c, p, run, None, &BranchStop::default());
        let elapsed = self.b.now().saturating_sub(began);
        self.refresh();
        if let Some(refusal) = self.facts.checks.fail_refusal(&c.commit) {
            return Err(self.halt(refusal));
        }
        let ran = match ran {
            Ok(ran) => ran,
            Err(refusal) => return Err(self.halt(refusal)),
        };
        if self
            .facts
            .checks
            .run_only(Half::Bundle, &[aterm_verify::checks::Check::Appkit])
            .checks
            .is_empty()
        {
            self.b.record_time(&c.version, QUALIFY_APPKIT_ROW, elapsed);
            return Ok(format!(
                "appkit passed before the release build: its record of {} is ok under this seal",
                short(&c.commit)
            ));
        }
        let refusal = self.unrecorded(c, run, None, ran);
        Err(self.halt(refusal))
    }

    /// One run of the qualify graph over what still needs it, under the release's disk claim
    /// where it builds or runs the tree, else (the export alone) under a stage's own; its words.
    fn qualify_round(&mut self, c: &Candidate, p: &Prover) -> Result<Vec<String>, Halt> {
        let (plan, _) = self.qualify_plan();
        if plan.build.is_some() || plan.tree.is_some() {
            self.disk_still_needed("the qualify graph and the bundle half")?;
        }
        let reserved = match plan.build {
            Some(_) => Some(self.reserved_for_build(c)?),
            None => self.reservation(),
        };
        // A new reservation sets the old bundle aside, so the plan is asked again.
        let (mut plan, done) = self.qualify_plan();
        if plan.branches().is_empty() {
            return Ok(done);
        }
        // No append-only staging write may precede the other branches' verdicts.
        // The engine's dry run keeps the expensive export work parallel; its real
        // stage below revalidates rather than trusting a private attestation.
        let deferred = plan.export == Some(StageTarget::Remote)
            && (plan.build.is_some() || plan.tree.is_some());
        if deferred {
            plan.export = Some(StageTarget::Local);
        }
        let graph = if self.export_alone(&plan) {
            self.stage_claimed(|run| run.qualify_graph(c, p, reserved.as_ref(), &plan))?
        } else {
            self.qualify_graph(c, p, reserved.as_ref(), &plan)
        };
        let mut words = self.qualified(c, graph?, done, plan.export == Some(StageTarget::Local))?;
        if deferred {
            self.halt_if_interrupted()?;
            let ran = self.stage_claimed(|run| run.b.stage(c, p, StageTarget::Remote))?;
            self.refresh();
            match self.stage_verdict(c, ran) {
                Ok(note) => words.push(note),
                Err(refusal) => return Err(self.halt(refusal)),
            }
        }
        Ok(words)
    }

    /// `plan` runs the export alone, with no disk claim held: it takes a stage's own.
    fn export_alone(&self, plan: &Plan) -> bool {
        !self.disk_held && plan.export.is_some() && plan.build.is_none() && plan.tree.is_none()
    }

    /// The graph of `plan`, drawing from one pool of this Mac's cores, and each passed branch's
    /// time kept in `steps.tsv`.
    fn qualify_graph(
        &mut self,
        c: &Candidate,
        p: &Prover,
        reserved: Option<&Reservation>,
        plan: &Plan,
    ) -> Result<Graph, Halt> {
        let branches = plan.branches();
        let pool = match self.b.qualify_begin(p, &branches) {
            Ok(pool) => pool,
            Err(refusal) => return Err(self.halt(refusal)),
        };
        self.say(&format!("        pool: {pool}"));
        let graph = {
            let br = self.b.branches();
            for branch in branches {
                br.say(&format!(
                    "        {}: {}; log: {}",
                    branch.name(),
                    plan.what(branch),
                    br.branch_log(branch).display()
                ));
            }
            run_graph(&*br, c, p, reserved, plan)
        };
        self.b.qualify_end();
        let v = self.version();
        for (branch, ended, secs) in &graph.ended {
            if *ended == Ended::Passed {
                self.b.record_time(&v, &branch.row(), *secs);
            }
        }
        Ok(graph)
    }

    /// The journal's reservation while origin's ledger is below it, else a new one, journaled
    /// with no bundle and no fixtures staged under it. A reservation it replaces first loses what
    /// step 7 signed privately under it ([`Run::withdraw_signed`]), while release.toml still
    /// reserves it; a withdrawal that cannot finish refuses ([`Run::withdraw_for`]), and the old
    /// reservation stands.
    fn reserved_for_build(&mut self, c: &Candidate) -> Result<Reservation, Halt> {
        if let Some(r) = self.reservation()
            && self.b.origin_tail().is_some_and(|tail| r.build > tail)
        {
            return Ok(r);
        }
        if let Some(spent) = self.spent() {
            self.say(&format!("        {} — reserving anew", spent.words()));
        }
        if let Some((version, old)) = self
            .r_journal()
            .and_then(|j| j.reserved.clone().map(|r| (j.version.clone(), r)))
        {
            match self.withdraw_for(&version, &old, RELEASE_COMMAND) {
                Ok(note) => {
                    if let Some(note) = note {
                        self.say(&format!("        {note}"));
                    }
                }
                Err(refusal) => return Err(self.halt(refusal)),
            }
        }
        let reserved = match self.b.reserve(c) {
            Ok(r) => r,
            Err(refusal) => return Err(self.halt(refusal)),
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
        Ok(reserved)
    }

    /// Withdraw what step 7 signed privately under `old`, a reservation this release is about to
    /// drop (step 4 journaling another, or `--forget`): its private draft, the draft's journal and
    /// the signed copy. The draft goes while release.toml still reserves `old`, the proof its
    /// discard requires ([`crate::publish::discard_signed`]); once the reservation is gone nothing
    /// can prove the draft this clone's. A reservation a cut journal claims keeps its draft
    /// (`Ok(None)`): that cut finishes or withdraws it. A claim that landed with no cut journal is
    /// spent ([`Spent::words`]): no `cut --resume` and no `recover` uses its draft, so it goes as
    /// any dropped reservation's does. A cut journal that cannot be read may be the claim, so
    /// nothing is withdrawn: a read that failed is COULD NOT RUN, and a file that is no cut
    /// journal refuses, naming the way out ([`cut_journal_invalid`]); either remedy names
    /// `then`, the command that withdraws once it clears. Else what the discard left.
    fn withdraw_signed(
        &mut self,
        version: &str,
        old: &Reservation,
        then: &str,
    ) -> Result<Option<Discarded>, Refusal> {
        let cut = match self.b.cut_journal_read() {
            Ok(cut) => cut,
            Err(CutJournalUnread::Read(why)) => {
                return Err(Refusal::new(
                    format!(
                        "the cut journal cannot be read ({why}), so whether a claim owns build \
                         {}'s private draft is unknown",
                        old.build
                    ),
                    format!(
                        "make dist/cut-state.toml readable again, then {then} (build {} stays \
                         reserved until then)",
                        old.build
                    ),
                )
                .could_not_run());
            }
            Err(CutJournalUnread::Invalid(why)) => {
                return Err(cut_journal_invalid(version, old, &why, then));
            }
        };
        if cut.is_some_and(|cut| cut.version == version && cut.build == old.build) {
            return Ok(None);
        }
        Ok(Some(self.b.discard_signed(
            version,
            old.build,
            SignedDiscard::LostReservation,
        )))
    }

    /// Before `next` replaces release.toml wholesale (a new candidate, a `--fix` hop or a
    /// supersession's winner, each reserving nothing): when release.toml reserves a build `next`
    /// does not, what step 7 signed privately under it is withdrawn while that reservation still
    /// proves it this clone's ([`Run::withdraw_for`]). A refusal leaves release.toml as it is.
    fn withdraw_dropped(&mut self, next: &Journal) -> Result<(), Refusal> {
        let saved = Journal::load(&self.b.release_dir().join(JOURNAL_FILE)).map_err(|why| {
            Refusal::new(why, format!("restore release.toml, then {RELEASE_COMMAND}"))
                .could_not_run()
        })?;
        let Some((version, old)) = saved.and_then(|j| j.reserved.map(|r| (j.version, r))) else {
            return Ok(());
        };
        if next.reserved.as_ref() == Some(&old) {
            return Ok(());
        }
        if let Some(note) = self.withdraw_for(&version, &old, RELEASE_COMMAND)? {
            self.say(&format!("        {note}"));
        }
        Ok(())
    }

    /// [`Run::withdraw_signed`] as the release states it: that nothing build `old` signed
    /// privately remains (`None` when a cut journal's claim keeps it), or the refusal that keeps
    /// `old` reserved, whose remedy names `then`. A removal a retry may get through is COULD NOT
    /// RUN; a held draft, or a cut journal that is no cut journal, refuses, naming what a person
    /// does first.
    fn withdraw_for(
        &mut self,
        version: &str,
        old: &Reservation,
        then: &str,
    ) -> Result<Option<String>, Refusal> {
        let build = old.build;
        match self.withdraw_signed(version, old, then) {
            Ok(None) => Ok(None),
            Ok(Some(discarded)) if discarded.complete() => Ok(Some(format!(
                "nothing signed privately under build {build} remains"
            ))),
            Ok(Some(Discarded::Except(left))) => Err(Refusal::new(
                format!(
                    "what step 7 signed privately under build {build} could not be withdrawn: {}",
                    left.join("; ")
                ),
                format!("{then} again once that clears (build {build} stays reserved until then)"),
            )
            .could_not_run()),
            Ok(Some(Discarded::Held { why, remedy })) => Err(Refusal::new(
                format!("what step 7 signed privately under build {build} is held: {why}"),
                format!("{remedy}, then {then} (build {build} stays reserved until then)"),
            )),
            Err(refusal) => Err(refusal),
        }
    }

    /// A real run's refusal as the halt it is.
    fn halt(&mut self, refusal: Refusal) -> Halt {
        match self.refuse(refusal) {
            Err(halt) => halt,
            Ok(()) => Halt::Said(1),
        }
    }

    /// What a dry run's step 4 would run, branch by branch.
    fn qualify_dry(&mut self, c: &Candidate, plan: &Plan) -> Result<String, Halt> {
        let mut said = Vec::new();
        if let Some(run) = &plan.appkit {
            said.push(format!(
                "appkit first and alone — `{}`; only after it passes, the graph starts",
                half_command(run, None)
            ));
        }
        if plan.build.is_some_and(|work| work.bundle) {
            match self.b.qualify_slice_memory() {
                Ok(forecast) => {
                    match forecast.outcome {
                        crate::buildplan::SliceMemoryOutcome::Wait => {
                            self.dry_resource_waits.push(forecast.line.clone());
                            self.say(&format!(
                                "[{:>2}/{}] {:<15} WOULD WAIT  {}",
                                self.cur.number(),
                                Step::ALL.len(),
                                self.cur.name(),
                                forecast.line
                            ));
                        }
                        crate::buildplan::SliceMemoryOutcome::Shared
                        | crate::buildplan::SliceMemoryOutcome::Serial
                        | crate::buildplan::SliceMemoryOutcome::Exclusive => {}
                    }
                    said.push(forecast.line);
                }
                Err(why) => {
                    let line = format!("qualify slice memory could not be judged: {why}");
                    self.refuse(
                        Refusal::new(
                            line.clone(),
                            "restore the account capacity ledger and compiler-unit book, then rerun the dry run",
                        )
                        .could_not_run(),
                    )?;
                    said.push(line);
                }
            }
        }
        if let Some(run) = &plan.tree {
            said.push(format!(
                "the tree — `{}` ({})",
                half_command(run, None),
                self.facts.checks.line()
            ));
        }
        if let Some(work) = plan.build {
            let held = match (self.reservation(), self.b.origin_tail()) {
                (Some(r), Some(tail)) if r.build > tail => Some(r),
                _ => None,
            };
            let mut build = vec![match &held {
                Some(r) => format!(
                    "build {} reserved as {} (this release's journal)",
                    r.build,
                    short(&r.commit)
                ),
                None => "would reserve build n = max(origin's ledger tail + 1, now) and the \
                         release commit (R + the ledger line + the rolled changelog), claimed \
                         nowhere"
                    .to_string(),
            }];
            if work.bundle {
                build.push(format!(
                    "both slices at once from its snapshot into the release cache, the unsigned \
                     bundle kept by what it is built as ({})",
                    self.facts.built.words
                ));
            }
            if work.fixtures {
                let identity = held.map_or_else(
                    || "the build it reserves".to_string(),
                    |r| format!("build {} ({})", r.build, short(&r.commit)),
                );
                build.push(format!(
                    "then v{}'s handoff fixtures for {identity} with {} into the release cache, \
                     refusing here if it refuses; step 9 lands them on main",
                    c.version,
                    fixture_generator()
                ));
            }
            said.push(format!("the build — {}", build.join(", ")));
        }
        if let Some(target) = plan.export {
            let commit = if c.bump_from.is_some() {
                "<the bump commit>"
            } else {
                c.commit.as_str()
            };
            said.push(format!(
                "the export — pub stage aterm --commit {commit} --repo-path <this clone>{}",
                if target == StageTarget::Remote && (plan.build.is_some() || plan.tree.is_some()) {
                    " --dry-run, then stage remotely only after every parallel verdict passes"
                } else if target == StageTarget::Local {
                    " --dry-run, without a staging push"
                } else {
                    ""
                }
            ));
        }
        if said.is_empty() {
            return Ok("would run nothing: every branch's record stands".to_string());
        }
        Ok(format!(
            "would run in this order: {}; the graph draws from one pool of this Mac's cores and \
             a red stops the rest",
            said.join("; ")
        ))
    }

    /// Each branch's verdict after the graph ended: the build's bundle and fixtures journaled
    /// whatever else ended, so a resume reuses them; then the export's and the tree's. The first
    /// red to come refuses R, the branches it stopped named under it; with none, a COULD NOT RUN
    /// or a refusal no branch's exit showed; else step 4 passed.
    fn qualified(
        &mut self,
        c: &Candidate,
        graph: Graph,
        done: Vec<String>,
        local_export: bool,
    ) -> Result<Vec<String>, Halt> {
        let mut words = Vec::new();
        let mut refused: BTreeMap<Branch, Refusal> = BTreeMap::new();
        if let Some(built) = graph.built {
            match built.artifact {
                Some(Ok(artifact)) => {
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
                    } else if self.facts.cut.is_some() || self.spent().is_none() {
                        // A peer's cut that reached the reservation sets the bundle aside
                        // instead: step 4's next round reserves anew and builds again.
                        let built = self.facts.built.words.clone();
                        refused.insert(
                            Branch::Build,
                            Refusal::new(
                                format!(
                                    "the build ended, and its bundle is not the journal's: {built}"
                                ),
                                format!("read its log, then {RELEASE_COMMAND} (it builds again)"),
                            ),
                        );
                    }
                }
                Some(Err(refusal)) => {
                    refused.insert(Branch::Build, refusal);
                }
                None => {}
            }
            match built.fixtures {
                Some(Ok(note)) => {
                    self.journal_add(FIXTURES_STAGED)?;
                    words.push(note);
                }
                Some(Err(refusal)) => {
                    refused.entry(Branch::Build).or_insert(refusal);
                }
                None => {}
            }
        }
        self.refresh();
        match graph.export {
            Some(Ok(ran)) if local_export => {
                if ran.code == 0 {
                    self.exported_locally = true;
                    words.push(
                        "the full export qualified locally; no staging snapshot pushed".into(),
                    );
                } else {
                    let refusal = if self.opts.qualify {
                        Refusal::new(
                            format!("private export qualification failed (exit {})", ran.code),
                            "read its log and fix the candidate before qualifying it again",
                        )
                        .detail(ran.detail)
                    } else {
                        self.export_check_refusal(c, ran)
                    };
                    refused.insert(Branch::Export, refusal);
                }
            }
            Some(ran) => match self.stage_verdict(c, ran) {
                Ok(note) => words.push(note),
                Err(refusal) => {
                    refused.insert(Branch::Export, refusal);
                }
            },
            None => {}
        }
        if let Some(ran) = graph.tree {
            match self.tree_verdict(c, ran) {
                Ok(note) => words.push(note),
                Err(refusal) => {
                    refused.insert(Branch::Tree, refusal);
                }
            }
        }
        // What the branches made is journaled above; a person's signal ends the step with no
        // verdict, whatever its branches answered.
        self.halt_if_interrupted()?;
        let stopped: Vec<Branch> = graph
            .ended
            .iter()
            .filter(|(_, e, _)| *e == Ended::Stopped)
            .map(|(b, ..)| *b)
            .collect();
        for branch in &stopped {
            refused.remove(branch);
        }
        let red = graph
            .ended
            .iter()
            .find(|(_, e, _)| *e == Ended::Red)
            .map(|(b, ..)| *b);
        let first = red
            .into_iter()
            .chain(graph.ended.iter().map(|(b, ..)| *b))
            .chain(Branch::ALL)
            .find(|b| refused.contains_key(b));
        if let Some(branch) = first {
            let mut refusal = refused.remove(&branch).expect("found");
            for other in &stopped {
                refusal.detail.push(format!(
                    "the {} branch was stopped by the {} branch's red",
                    other.name(),
                    branch.name()
                ));
            }
            for (other, also) in refused {
                refusal
                    .detail
                    .push(format!("the {} branch also: {}", other.name(), also.why));
                refusal.could_not_run &= also.could_not_run;
            }
            if let Some(red) = red {
                refusal.could_not_run = false;
                if refusal.failing.is_none() {
                    refusal.failing = Some(format!("qualify's {} branch", red.name()));
                }
            }
            return Err(self.halt(refusal));
        }
        words.extend(done);
        Ok(words)
    }

    /// The tree half's verdict from its records: every tree check's record ok, else its FAIL,
    /// else why its records are missing.
    fn tree_verdict(
        &mut self,
        c: &Candidate,
        ran: Result<Ran, Refusal>,
    ) -> Result<String, Refusal> {
        let ran = ran?;
        if let Some(refusal) = self.facts.checks.fail_refusal(&c.commit) {
            return Err(refusal.detail(ran.detail));
        }
        let run = self.facts.checks.run(Half::Tree);
        if run.checks.is_empty() {
            return Ok(format!(
                "`verify release --tree` passed: every tree check's record of {} is ok under \
                 this seal",
                short(&c.commit)
            ));
        }
        Err(self.unrecorded(c, &run, None, ran))
    }

    /// Why `run` of `verify release` left records missing though no FAIL reads: its exit
    /// code's refusal.
    fn unrecorded(
        &self,
        c: &Candidate,
        run: &crate::check_record::VerifyRun,
        app: Option<&str>,
        ran: Ran,
    ) -> Refusal {
        let words = self.facts.checks.line().to_owned();
        let missing = run.checks.join(", ");
        let rerun = half_command(run, app);
        let r = short(&c.commit).to_string();
        let refusal = match ran.code {
            0 => Refusal::new(
                format!(
                    "`verify release` passed for {r}, and the records of {missing} cannot be \
                     read: {words}"
                ),
                format!("run {RELEASE_COMMAND} again once they read (the candidate is kept)"),
            )
            .could_not_run(),
            1 => Refusal::new(
                format!(
                    "`verify release` failed for {r} (exit 1), and no FAIL record of it reads: \
                     {words}"
                ),
                format!(
                    "`{rerun}` in the clone runs it again; {}",
                    fix_remedy(&c.commit)
                ),
            )
            .failing("verify release"),
            code => Refusal::new(
                format!(
                    "`verify release` could not run every check for {r} (exit {code}: {}) — \
                     a release needs every check, and {missing} have no passing record; the \
                     next run resumes only those",
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
        refusal.detail(ran.detail)
    }

    /// The meaning check before export. A dry pending head has already passed the frozen
    /// image's pending check at step 1; its record merge is still only a plan.
    fn line_holds(&mut self, c: &Candidate) -> Result<Option<String>, Halt> {
        let line = self
            .journal
            .as_ref()
            .map(Journal::line)
            .filter(|line| !line.hops.is_empty());
        let mut said = Vec::new();
        if self.dry && self.pending_line_heads(&c.commit) {
            said.push(format!(
                "planned head {} checked with line::verify_pending at step 1; would verify its record merge before export",
                short(&c.commit)
            ));
        } else if let Some(line) = line {
            match self.b.verify_line(&line) {
                Ok(summary) => said.push(summary),
                Err(refusal) => self.refuse(refusal)?,
            }
        }
        Ok((!said.is_empty()).then(|| said.join("; ")))
    }

    fn commit_gates(&mut self, c: &Candidate) -> Result<String, Halt> {
        // The tree is immutable, but advertised tags and engine readiness can change
        // between launches. Refresh the evidence before any compile on every run.
        let Preflight { notes, found } = self.b.commit_gates(c, self.dry);
        if found.is_empty() {
            self.journal_add(COMMIT_GATES)?;
            let mut words = String::from(
                "the preflight passes: R's commit-only gates (the changelog, the predecessor's \
                 handoff fixtures, the handoff policy, the locked metadata) and the engine's \
                 readiness",
            );
            if !c.carry.is_empty() {
                words.push_str(&format!(
                    ", asked of the planned head {} after the carry",
                    short(&c.commit)
                ));
            }
            for note in notes {
                words.push_str(&format!("; {note}"));
            }
            return Ok(words);
        }
        let n = found.len();
        let remedy = self.remedy(c);
        self.refuse(preflight_blocked(
            format!(
                "the preflight BLOCKED ({n} finding(s)): {}",
                found
                    .iter()
                    .map(|r| r.why.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            &found,
            &remedy,
        ))?;
        Ok(format!("the preflight BLOCKED ({n} finding(s), above)"))
    }

    fn verification_answer(&mut self) -> Flow {
        let answer = self.facts.checks.answer();
        if !answer.unanswered {
            return Ok(());
        }
        let words = answer.words.clone();
        self.refuse(
            Refusal::new(
                format!("the cutter's own judge could not answer: {words}"),
                format!("run {RELEASE_COMMAND} again once it answers (the candidate is kept)"),
            )
            .could_not_run(),
        )
    }

    /// The FAIL rule (A19): a FAIL record of R's tree under this seal refuses the tree at once,
    /// exit 1, before anything is staged, built or verified again; only a new hop changes it.
    fn fail_rule(&mut self) -> Flow {
        match self.facts.checks.fail_refusal(&self.commit_r()) {
            Some(refusal) => self.refuse(refusal),
            None => Ok(()),
        }
    }

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
            format!("read its log; {}", fix_remedy(&c.commit)),
        )
        .detail(ran.detail))
    }

    /// A cut's deferred export check (`pub stage --dry-run` of R, run beside the build and the
    /// tree before the real stage) found R unstageable: [`Run::stage_verdict`]'s verdict on a
    /// stage that did not stage R, with its remedies (the engine's own, when restaging).
    fn export_check_refusal(&self, c: &Candidate, ran: Ran) -> Refusal {
        let r = short(&c.commit);
        let refusal = if self.restage.is_some() {
            Refusal::new(
                format!(
                    "pub stage --dry-run found {r} unstageable by the engine as it is now \
                     (exit {})",
                    ran.code
                ),
                format!("read its log, then {RELEASE_COMMAND}"),
            )
        } else {
            Refusal::new(
                format!(
                    "pub stage --dry-run found {r} unstageable (exit {})",
                    ran.code
                ),
                format!("read its log; {}", fix_remedy(&c.commit)),
            )
        };
        refusal.detail(ran.detail)
    }

    fn cut_still_possible(&mut self, c: &Candidate, gates: bool) -> Vec<Refusal> {
        let _timing = crate::cut_timing::Timer::start(&self.b.log_path(self.cur), "cut-readiness");
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
                    "release the next product version: {RELEASE_COMMAND} --forget, then {RELEASE_COMMAND} \
                     --bump"
                ),
            )),
            Ok(_) => {
                if !self.claimed() && self.b.local_tag(&tag) && self.b.origin_tag(&tag).is_none() {
                    out.push(Refusal::new(
                        format!("tag {tag} exists in this clone and not on origin — a leftover"),
                        format!("git tag -d {tag}, then re-run {}", self.again(c)),
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
            let kind = if self.opts.qualify {
                crate::publish::CutKind::DryRun
            } else {
                crate::publish::CutKind::Real
            };
            out.extend(self.b.cut_gates(c, &creds, kind));
            // Real preparation has bumped or carried already; the dry plan's private bump or
            // planned head is also constructed before checks, so both ask these gates of the
            // head they would ship.
            out.extend(self.b.head_gates(c));
        }
        out
    }

    fn still_possible(&mut self, c: &Candidate, before: Option<&str>) -> Flow {
        let found = self.cut_still_possible(c, true);
        let (unasked, decided): (Vec<Refusal>, Vec<Refusal>) =
            found.into_iter().partition(|r| r.could_not_run);
        if decided.is_empty() && unasked.is_empty() {
            return Ok(());
        }
        let asked = before.map_or_else(String::new, |step| {
            format!("asked again right before the {step}, ")
        });
        let remedy = self.remedy(c);
        if !decided.is_empty() {
            self.refuse(blocked(
                format!("{asked}the cut would refuse ({} finding(s))", decided.len()),
                &decided,
                &remedy,
            ))?;
        }
        if !unasked.is_empty() {
            let mut r = blocked(
                format!(
                    "{asked}what the cut would refuse could not be asked ({} question(s))",
                    unasked.len()
                ),
                &unasked,
                &remedy,
            );
            r.remedy = format!(
                "run {} again once it answers (nothing changed)",
                remedy.again
            );
            self.refuse(r.could_not_run())?;
        }
        Ok(())
    }

    fn engine_still_staged(&mut self, c: &Candidate, p: &Prover) -> Flow {
        if let Err(r) = self.b.sync() {
            return self.refuse(r);
        }
        self.refresh();
        let r = short(&c.commit).to_string();
        let attested = self.facts.attested;
        let staged = self.facts.staged_by.clone().filter(|_| attested);
        let (old, why) = match staged.as_deref() {
            None => (
                None,
                format!(
                    "publish/.out holds no promotable attestation of {r} naming the engine \
                     that staged it"
                ),
            ),
            Some(engine) => match self.b.engine_now(engine) {
                Ok(EngineNow::Same(_)) => match self.candidate_still(&r)? {
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
        let ran = self.stage_claimed(|run| run.b.stage(c, p, StageTarget::Remote))?;
        self.refresh();
        let note = match self.stage_verdict(c, ran) {
            Ok(note) => note,
            Err(refusal) => return self.refuse(refusal),
        };
        if let Some(why) = self.candidate_still(&r)? {
            return self.refuse(
                Refusal::new(
                    format!(
                        "{r} was staged again right before the publish, but its qualified \
                         candidate is unavailable: {why}"
                    ),
                    format!(
                        "{RELEASE_COMMAND} again once the qualified candidate is retained \
                         (nothing is public)"
                    ),
                )
                .could_not_run(),
            );
        }
        self.say(&format!("        {note}"));
        Ok(())
    }

    fn candidate_still(&mut self, r: &str) -> Result<Option<String>, Halt> {
        match self.b.candidate_missing() {
            Ok(moved) => Ok(moved),
            Err(why) => {
                self.refuse(
                    Refusal::new(
                        format!(
                            "could not read the retained qualified candidate named by \
                             {r}'s attestation before publishing: {why}"
                        ),
                        format!("run {RELEASE_COMMAND} again once it answers"),
                    )
                    .could_not_run(),
                )?;
                Ok(None)
            }
        }
    }

    fn provision(&mut self, wait: bool) -> Result<String, Halt> {
        let mut said_waiting = false;
        loop {
            let id = self.machine_id.clone();
            let found = match (&self.dry_audit, self.dry) {
                (Some(found), true) => found.clone(),
                _ => {
                    let found =
                        self.b
                            .audit(&id, self.opts.release_credentials.as_deref(), self.dry);
                    if self.dry {
                        self.dry_audit = Some(found.clone());
                    }
                    found
                }
            };
            let mut owner: Vec<(String, AskWords)> = Vec::new();
            let mut unknown: Vec<String> = Vec::new();
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
            if self.opts.qualify {
                if !owner.is_empty() || !unknown.is_empty() {
                    let missing = owner
                        .iter()
                        .map(|(gap, _)| gap.as_str())
                        .chain(unknown.iter().map(String::as_str))
                        .collect::<Vec<_>>()
                        .join("; ");
                    let refusal = Refusal::new(
                        format!("private qualification prerequisites are unavailable: {missing}"),
                        "retry qualification when the recorded prerequisites are available",
                    );
                    self.refuse(if unknown.is_empty() {
                        refusal
                    } else {
                        refusal.could_not_run()
                    })?;
                    return Ok("private qualification prerequisites are unavailable; no owner request created".into());
                }
                return Ok(format!(
                    "this machine's qualification prerequisites pass ({id}); no owner request created"
                ));
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
                let ready = if self.facts.checks.complete() {
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

    /// Step 5: the remaining bundle half of `verify release` on the bundle step 4 built —
    /// normally measure, because step 4 ran AppKit first — beside private signing and notary work
    /// that executes no release product.
    /// The signed copy's windowed self-check stays in step 7, after this run ends.
    fn bundle(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        self.fail_rule()?;
        let appkit = self
            .facts
            .checks
            .run_only(Half::Bundle, &[aterm_verify::checks::Check::Appkit]);
        if !self.dry && !appkit.checks.is_empty() {
            return self.refuse(
                Refusal::new(
                    "AppKit's step-4 record no longer stands, so it cannot run late beside the built bundle",
                    format!("run {RELEASE_COMMAND} again — step 4 judges AppKit before any slice starts"),
                )
                .could_not_run(),
            );
        }
        let run = self
            .facts
            .checks
            .run_only(Half::Bundle, &[aterm_verify::checks::Check::Measure]);
        let words = self.facts.checks.line().to_owned();
        if self.dry {
            self.finish(&format!(
                "would run `{}` in the clone ({words}){}",
                half_command(&run, Some("<the bundle step 4 built>")),
                if self.opts.qualify || self.opts.prepare { "" } else {
                    "; beside it cut --sign-prepare privately signs and notarizes without product executions; step 7 joins it after READY"
                }
            ));
            return Ok(());
        }
        if let Some(spent) = self.spent() {
            return self.refuse(
                Refusal::new(
                    format!(
                        "origin's ledger reached the reservation while R qualified: {}; its \
                         bundle can never ship",
                        spent.words()
                    ),
                    format!(
                        "run {RELEASE_COMMAND} again — it reserves anew, then builds and verifies \
                         the bundle (the tree's records stand)"
                    ),
                )
                .could_not_run(),
            );
        }
        let Some(artifact) = self.artifact().filter(|_| self.facts.built.yes) else {
            return self.refuse(Refusal::new(
                format!(
                    "no bundle is built for this release ({}) — `verify release` judges the \
                     bytes that ship",
                    self.facts.built.words
                ),
                format!("{RELEASE_COMMAND} (step 4 builds it)"),
            ));
        };
        let app = artifact.app.display().to_string();
        let log = self.b.log_path(Step::Bundle);
        self.say(&format!("        runs: {}", half_command(&run, Some(&app))));
        self.say(&format!("        log: {}", log.display()));
        let prepare = !self.opts.qualify && !self.opts.prepare;
        let mut sign = self.sign_args();
        sign[1] = "--sign-prepare".into();
        if prepare {
            let log = self
                .b
                .branches()
                .side_log(Side::Sign)
                .with_file_name("05-sign-prepare.log");
            self.say(&format!(
                "        private sign: targo --unverified ship {}; no product executions; \
                 log: {}",
                sign.join(" "),
                log.display()
            ));
            self.early_sign = match self.b.prepare_sign(&sign, &p) {
                Ok(child) => Some(child),
                Err(refusal) => return self.refuse(refusal),
            };
        }
        let ran = self.b.branches().verify(
            Step::Bundle,
            &c,
            &p,
            &run,
            Some(&artifact),
            &BranchStop::default(),
        );
        self.refresh();
        if !self.facts.checks.complete() || ran.is_err() || self.b.branches().interrupted() {
            self.discard_early_sign();
        }
        if let Some(refusal) = self.facts.checks.fail_refusal(&c.commit) {
            return self.refuse(refusal);
        }
        let ran = match ran {
            Ok(ran) => ran,
            Err(refusal) => return self.refuse(refusal),
        };
        if self.facts.checks.complete() {
            let words = self.facts.checks.line().to_owned();
            self.finish(&words);
            return Ok(());
        }
        let refusal = self.unrecorded(&c, &run, Some(&app), ran);
        self.refuse(refusal)
    }

    fn records_stand(&mut self, what_refuses: &str) -> Flow {
        let r = short(&self.commit_r()).to_string();
        if !self.facts.checks.complete() {
            let words = self.facts.checks.line().to_owned();
            self.refuse(Refusal::new(
                format!(
                    "`verify release` has not passed for the bundle of {r} ({words}) — \
                     {what_refuses}"
                ),
                format!("{RELEASE_COMMAND} (steps 4-5 build and verify it)"),
            ))?;
        }
        Ok(())
    }

    fn discard_early_sign(&mut self) {
        let Some(child) = self.early_sign.take() else {
            return;
        };
        child.stop().fire();
        let _ = child.finish();
        let n = self.reservation().map_or(0, |r| r.build);
        let version = self.version();
        for why in self
            .b
            .discard_signed(&version, n, SignedDiscard::CancelPreparation)
            .left()
        {
            self.say(&format!("        discard early signature: {why}"));
        }
    }

    fn ready(&mut self) -> Flow {
        if self.dry {
            self.finish(
                "would require every check's `verify release` record to stand for R: READY, or \
                 nothing goes public",
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

    /// `cut --sign` with the signing profile: step 7's sign side.
    fn sign_args(&self) -> Vec<String> {
        let mut args = vec![
            "cut".to_string(),
            "--sign".to_string(),
            "--release-credentials".to_string(),
            self.creds.display().to_string(),
        ];
        args.extend(self.linux.cut_args());
        args
    }

    /// Step 7: R's source published by `pub publish` and R's verified bundle signed, notarized
    /// and packaged privately by `cut --sign`, at once ([`run_sides`]); neither claims, and the
    /// cut claims once both passed. `published` says why the source is skipped (published
    /// earlier, no build claimed): the sign then runs alone, and adopts the copy an earlier run
    /// signed of the same bytes. Its source publication is read first: where its attestation
    /// still waits on the private draft (a sign that failed after the source's push made or
    /// journaled none), the sign makes or adopts that draft, the source signing then attaches
    /// the pair to it ([`Backend::attest_draft`]), and it must read as attested before any
    /// claim. Everything the cut would refuse is asked again before either side starts, and the
    /// engine readied for the source; the sign reads only the release's own journal.
    fn publish(&mut self, published: Option<String>) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            if let Some(why) = published {
                self.skip(&why);
            }
            return Ok(());
        };
        let source = published.is_none();
        let sign = self.sign_args();
        if self.dry {
            let line = self.provision(true)?;
            let then = format!(
                "targo --unverified ship {} (R's verified bundle copied, signed, notarized and \
                 packaged privately under its reservation; nothing claimed)",
                sign.join(" ")
            );
            self.finish(&match &published {
                None => format!(
                    "{line}; would ask again what the cut would refuse and that the engine is \
                     the one that staged R (else stage again, under a stage's disk claim of {} \
                     GiB), then run at once: pub publish aterm --repo-path <this clone>, and \
                     {then}",
                    crate::release_disk::STAGE >> 30
                ),
                Some(why) => format!(
                    "{why}; {line}; would ask again what the cut would refuse and read R's \
                     source publication, then run: {then}; and, where its attestation still \
                     waits on the private draft, after the sign: pub sign-release aterm --tag \
                     v{} --keep-draft --draft-state <this clone>/dist/draft-state.toml \
                     --repo-path <this clone>, R attested before any claim",
                    c.version
                ),
            });
            return Ok(());
        }
        self.records_stand("the cut would refuse after the publish")?;
        if let Err(why) = self.b.fleet_audit() {
            return self.refuse(
                Refusal::new(why, "resume on the fleet line's current holder").could_not_run(),
            );
        }
        self.provision(true)?;
        self.still_possible(&c, Some(if source { "publish" } else { "cut" }))?;
        if source {
            self.engine_still_staged(&c, &p)?;
            if let Err(r) = self.b.publish_begin(&p) {
                return self.refuse(r);
            }
        }
        let pending = if source {
            None
        } else {
            let n = self.reservation().map_or(0, |r| r.build);
            match self.b.source_publication(&c, n) {
                Ok(SourceState::Attested) => None,
                Ok(SourceState::Pending(why)) => Some(why),
                Err(r) => return self.refuse(r),
            }
        };
        let sides = {
            let br = self.b.branches();
            let gate = br.publish_gate();
            match gate.reset() {
                Ok(()) => {
                    br.say(&match &published {
                        None => format!(
                            "        source: pub publish aterm, its push behind {}; log: {}",
                            gate.path().display(),
                            br.side_log(Side::Source).display()
                        ),
                        Some(why) => match &pending {
                            None => format!("        source: skipped — {why}"),
                            Some(waits) => format!(
                                "        source: skipped — {why}; its attestation waits on the \
                                 private draft ({waits}), attached after the sign"
                            ),
                        },
                    });
                    br.say(&format!(
                        "        sign: targo --unverified ship {}; log: {}",
                        sign.join(" "),
                        br.side_log(Side::Sign).display()
                    ));
                    Ok(run_sides(&*br, &p, source, &sign, self.early_sign.take()))
                }
                Err(e) => Err(Refusal::new(
                    format!("cannot open step 7's gate {}: {e}", gate.path().display()),
                    format!("fix what that names, then {RELEASE_COMMAND}"),
                )
                .could_not_run()),
            }
        };
        let mut sides = match sides {
            Ok(sides) => sides,
            Err(r) => return self.refuse(r),
        };
        let v = self.version();
        for (side, ended, secs) in &sides.ended {
            if *ended == Ended::Passed {
                self.b.record_time(&v, &side.row(), *secs);
            }
        }
        self.refresh();
        match sides
            .ended
            .iter()
            .find(|(_, ended, _)| *ended == Ended::Red)
            .map(|(side, ..)| *side)
        {
            Some(Side::Source) => return self.source_verdict(&c, sides.source),
            Some(Side::Sign) => return self.sign_refused(&c, sides),
            None => {}
        }
        if source {
            self.source_verdict(&c, sides.source.take())?;
        }
        if sides.of(Side::Sign) != Some(Ended::Passed) {
            let ran = sides.sign.unwrap_or(Ran {
                code: -1,
                detail: vec!["not started".into()],
            });
            return self.refuse(
                Refusal::new(
                    format!(
                        "cut --sign could not run (exit {}): {}",
                        ran.code,
                        ran.detail.last().cloned().unwrap_or_default()
                    ),
                    format!(
                        "{RELEASE_COMMAND} — step 7 signs again{}",
                        if self.facts.engine.has {
                            ", the source published already"
                        } else {
                            ""
                        }
                    ),
                )
                .could_not_run()
                .detail(ran.detail),
            );
        }
        let attested = match pending {
            None => String::new(),
            Some(_) => match self.attest(&c, &p) {
                Ok(words) => format!("; {words}"),
                Err(r) => return self.refuse(r),
            },
        };
        let n = self.reservation().map_or(0, |r| r.build);
        let r = short(&c.commit).to_string();
        let rev = self.facts.engine.rev.clone().unwrap_or_default();
        self.finish(&format!(
            "{} {r} -> alabsystems {}; signed build {n} privately{attested}",
            if source {
                "published"
            } else {
                "published earlier:"
            },
            short(&rev)
        ));
        Ok(())
    }

    /// Step 7's source signing onto the private draft the sign that just passed made or adopted
    /// ([`Backend::attest_draft`]), then R's source publication read again: it must stand
    /// attested before any claim. What was attached.
    fn attest(&mut self, c: &Candidate, p: &Prover) -> Result<String, Refusal> {
        let n = self.reservation().map_or(0, |r| r.build);
        let words = self.b.attest_draft(c, n, p)?;
        self.say(&format!("        attest: {words}"));
        match self.b.source_publication(c, n)? {
            SourceState::Attested => Ok(format!("its source attested: {words}")),
            SourceState::Pending(why) => Err(Refusal::new(
                format!(
                    "the source signing passed and R's source attestation still waits on its \
                     private draft: {why} — nothing was claimed"
                ),
                format!("read step 7's log, then {RELEASE_COMMAND}"),
            )),
        }
    }

    /// The source side's verdict, the engine read again: `pub publish` recorded R as the
    /// engine's newest aterm row, exited 0, and R's source release carries its attestation.
    fn source_verdict(&mut self, c: &Candidate, ran: Option<Result<Ran, Refusal>>) -> Flow {
        let ran = match ran {
            Some(Ok(ran)) => ran,
            Some(Err(r)) => return self.refuse(r),
            None => {
                return self.refuse(
                    Refusal::new(
                        "pub publish never started",
                        format!("{RELEASE_COMMAND} — pub publish is resumable"),
                    )
                    .could_not_run(),
                );
            }
        };
        let r = short(&c.commit).to_string();
        if !self.facts.engine.has {
            let refusal = Refusal::new(
                format!(
                    "pub publish did not record {r} in the engine's ledger (exit {})",
                    ran.code
                ),
                format!("read its log; pub publish is resumable: {RELEASE_COMMAND}"),
            )
            .detail(ran.detail);
            return self.refuse(if ran.code == 130 {
                refusal.could_not_run()
            } else {
                refusal
            });
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
        if ran.code != 0 {
            return self.refuse(
                Refusal::new(
                    format!(
                        "pub publish recorded {r} but exited {}: {}",
                        ran.code,
                        ran.detail.last().cloned().unwrap_or_default()
                    ),
                    format!(
                        "{RELEASE_COMMAND}: R is recorded, so step 7 then signs alone and attaches \
                         R's source attestation to the private draft itself (pub sign-release \
                         aterm --tag v{} --keep-draft); first finish by hand only a failure its \
                         last line names that is not that source signing",
                        c.version
                    ),
                )
                .detail(ran.detail),
            );
        }
        let n = self.reservation().map_or(0, |r| r.build);
        match self.b.source_publication(c, n) {
            Ok(SourceState::Attested) => Ok(()),
            Ok(SourceState::Pending(why)) => self.refuse(Refusal::new(
                format!(
                    "pub publish exited 0, and R's source attestation still waits on its private \
                     draft: {why}"
                ),
                format!(
                    "{RELEASE_COMMAND}: step 7 then signs alone and attaches it (nothing was \
                     claimed)"
                ),
            )),
            Err(r) => self.refuse(r),
        }
    }

    /// The sign side went red: nothing is claimed. Where the source's push had begun first (or
    /// an earlier run published it), R's source is public ahead of its binary, as after a cut
    /// that fails past the publish; else nothing is public.
    /// Step 7's verdict on the sign's first red. What is public follows from the engine's rows,
    /// else from this run's gate: a push the source's hook took the gate for may have reached
    /// public main though `pub publish` recorded no row. A red of the source after it is named.
    fn sign_refused(&mut self, c: &Candidate, sides: Sides) -> Flow {
        let source_red = match (sides.of(Side::Source), &sides.source) {
            (Some(Ended::Red), Some(Ok(ran))) => Some(format!(
                "exit {}: {}",
                ran.code,
                ran.detail.last().cloned().unwrap_or_default()
            )),
            (Some(Ended::Red), Some(Err(refusal))) => Some(refusal.why.clone()),
            _ => None,
        };
        let ran = sides.sign.unwrap_or(Ran {
            code: 1,
            detail: Vec::new(),
        });
        let r = short(&c.commit).to_string();
        let public = if self.facts.engine.has {
            format!(
                "the source of {r} is public (alabsystems {}) ahead of its binary, as after a \
                 cut that fails past the publish; no build is claimed",
                short(self.facts.engine.rev.as_deref().unwrap_or("?"))
            )
        } else if sides.pushed {
            self.public = format!(
                "the source of v{} may be public (its push began), no build claimed",
                self.version()
            );
            format!(
                "the source's push began (its pre-promote hook took the gate) and the engine \
                 records no row of {r}: its source may be public ahead of its binary, and no \
                 build is claimed"
            )
        } else {
            "the source's push never began in this run (the release took the gate first): \
             nothing was pushed or claimed"
                .to_string()
        };
        let last = ran.detail.last().cloned().unwrap_or_default();
        let mut detail = ran.detail;
        if let Some(why) = source_red {
            detail.push(format!("the source failed after it: {why}"));
        }
        self.refuse(
            Refusal::new(
                format!(
                    "cut --sign refused to sign R's verified bundle (exit {}: {last}) — {public}",
                    ran.code,
                ),
                format!(
                    "read its log (`{}`), fix what it names, then {RELEASE_COMMAND} (step 7 signs \
                     again before any claim)",
                    self.sign_args().join(" ")
                ),
            )
            .detail(detail),
        )
    }

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
        if let Err(why) = self.b.fleet_audit() {
            return self.refuse(
                Refusal::new(why, "resume on the fleet line's current holder").could_not_run(),
            );
        }
        if matches!(mode, CutMode::Fresh) {
            // the engine as the cut will read it: R must be its newest aterm row.
            if let Err(r) = self.b.sync() {
                return self.refuse(r);
            }
            self.refresh();
            let r = short(&c.commit).to_string();
            if !self.facts.engine.has {
                return self.refuse(Refusal::new(
                    format!("the engine's ledger does not record {r} — the cut signs only a published commit's build"),
                    format!("{RELEASE_COMMAND} (step 7 publishes it)"),
                ));
            }
            if let Some(refusal) = self.newest_row_refusal() {
                return self.refuse(refusal);
            }
            self.records_stand("the cut refuses pre-claim")?;
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
        let reserved = self.reservation();
        let ran = self.b.cut(&args, &p, &c, reserved.as_ref());
        self.refresh();
        if let Some(cut) = self.facts.cut.clone()
            && cut.finished
        {
            self.finish(&format!("cut v{}, build {}", cut.version, cut.build));
            return Ok(());
        }
        if self.facts.cut.is_none()
            && let Some(spent) = self.spent()
            && !spent.claimed_here
        {
            // The number is spent elsewhere, so the copy can never ship: discarded here whether
            // the cut reached its claim (which discards it too) or refused before it.
            let build = spent.reservation.build;
            let discarded = self
                .b
                .discard_signed(&self.version(), build, SignedDiscard::LostReservation)
                .fate();
            return self.refuse(
                Refusal::new(
                    format!(
                        "the cut's claim lost the ledger's race: {} — the copy step 7 signed, \
                         notarized and packaged privately as build {build} {discarded}, and \
                         nothing of it was public",
                        spent.words(),
                    ),
                    format!("{RELEASE_COMMAND} — it reserves anew, then builds and verifies again"),
                )
                .detail(ran.detail),
            );
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

    fn after_cut(&mut self) -> Flow {
        let v = self.version();
        let mut notes = Vec::new();
        if !self.after_cut_complete() {
            let work = if self.facts.fixtures.yes {
                None
            } else {
                self.fixture_work()?
            };
            let (result, fixtures) = match &work {
                Some((c, reserved, p)) => {
                    let (distribution, fixtures) = self.b.after_cut_with_fixtures(c, reserved, p);
                    (distribution, Some(fixtures))
                }
                None => (self.b.after_cut(&v), None),
            };
            match result {
                Ok(done) => {
                    if let Some(journal) = &mut self.journal {
                        journal.done.retain(|fact| fact != SITE_DEFERRED);
                    }
                    self.journal_add("after-cut")?;
                    self.journal_add(TAP_ALIGNED)?;
                    notes.push(done.note);
                }
                Err(r) => return self.refuse(r),
            }
            if let Some(fixtures) = fixtures {
                match fixtures {
                    Ok(note) => {
                        notes.push(note);
                        self.refresh();
                    }
                    Err(r) => return self.refuse(r),
                }
            }
        }
        if !self.facts.fixtures.yes {
            notes.push(self.land_fixtures()?);
        }
        self.finish(&notes.join("; "));
        Ok(())
    }

    fn land_fixtures(&mut self) -> Result<String, Halt> {
        let Some((c, reserved, p)) = self.fixture_work()? else {
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

    fn fixture_work(&mut self) -> Result<Option<(Candidate, Reservation, Prover)>, Halt> {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(None);
        };
        let claimed = self.facts.cut.as_ref().map(|cut| cut.build);
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
            return Ok(None);
        };
        Ok(Some((c, reserved, p)))
    }

    fn site_complete(&self) -> bool {
        self.journal_has("after-cut") && !self.journal_has(SITE_DEFERRED)
    }

    fn after_cut_complete(&self) -> bool {
        self.site_complete() && self.journal_has(TAP_ALIGNED)
    }

    fn check(&mut self) -> Flow {
        let v = self.version();
        if self.dry {
            self.finish(&format!(
                "would require v{v} to be the channel's latest release"
            ));
            return self.dry_final();
        }
        if let Some(refusal) = latest_refusal(&v, self.b.latest()) {
            return self.refuse(refusal);
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
        self.last = format!(
            "RELEASED aterm {v}{build} from {} — v{v} is the channel's latest — {}",
            short(&r),
            fmt_dur(took)
        );
        let last = self.last.clone();
        self.say(&last);
        Ok(())
    }
}

/// The verdict of step 10's live channel-head read. A transport error decides nothing about the
/// channel and is therefore resumable; a response naming no head or another head is a semantic
/// refusal.
fn latest_refusal(version: &str, latest: Result<Option<String>, String>) -> Option<Refusal> {
    let expected = format!("v{version}");
    match latest {
        Ok(Some(found)) if found == expected => None,
        Ok(found) => {
            let found = found.unwrap_or_else(|| "no release".to_string());
            Some(Refusal::new(
                format!("the channel's latest release is {found}, not {expected}"),
                format!(
                    "read the cut's log, then {RELEASE_COMMAND} (its after-cut step checks the \
                     release's shape again)"
                ),
            ))
        }
        Err(transport) => Some(
            Refusal::new(
                format!("the channel freshness read for {expected} did not run"),
                format!("restore the transport named in the log, then {RELEASE_COMMAND}"),
            )
            .detail([format!("latest release transport: {transport}")])
            .could_not_run(),
        ),
    }
}

/// Where the cut stands for V.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CutMode {
    Fresh,
    Resume(u64),
    Done(String),
}

/// The way out of a line whose hop origin/main lost (main rewritten without its record merge):
/// the line cannot ship, so it is dropped.
fn lost_record() -> String {
    format!(
        "origin/main no longer holds the line's record merge: drop the line ({RELEASE_COMMAND} \
         --forget) and start a new one"
    )
}

/// The refusal of a withdrawal under `old` (of `version`) beside a `dist/cut-state.toml` that is
/// no cut journal (`why`), whose remedy names `then`, the command that withdraws. Whether a
/// claim owns build `old`'s private draft is undecided, and every retry reads the same file, so
/// it refuses (exit 1) rather than COULD NOT RUN, whether or not anything was signed: every later
/// sign and cut refuses the file too. No tool reads it, so the way out moves it aside, and with
/// no cut journal the withdrawal goes on. A claim whose cut never finished also holds origin's
/// release lease, which only `recover` frees; without a cut journal recover finishes a published
/// release and deletes the tag's draft only while it holds nothing but this version's cut assets
/// ([`crate::publish::withdraw_unpublished_release`]), so it refuses, keeping the lease, the draft
/// such a claim adopts once step 7's source signing attached its pair. The way out therefore
/// withdraws that draft first, by its own journal (`--forget`, which signs nothing new under
/// the version), and then has recover drop the tag with `--no-draft-was-posted`.
fn cut_journal_invalid(version: &str, old: &Reservation, why: &str, then: &str) -> Refusal {
    let build = old.build;
    Refusal::new(
        format!(
            "dist/cut-state.toml is no cut journal this cutter reads ({why}), so whether a claim \
             owns build {build}'s private draft is undecided; every retry reads the same file, \
             and every later sign and cut refuses it too"
        ),
        format!(
            "move dist/cut-state.toml out of dist/ (`cut --resume`, `--abandon` and `recover` \
             refuse it too), then {then}, which withdraws build {build}'s private draft by \
             dist/draft-state.toml, since with no cut journal a claim of it is spent (build \
             {build} stays reserved until then). If origin's release lease (`git ls-remote \
             origin {lease}`) names build {build}'s release commit {short}, that cut never \
             finished and holds the release lock, which only `{ship} recover v{version} {commit} \
             --old-publisher-stopped` frees: it finishes v{version} if published, but without a \
             cut journal it deletes the tag's draft only while that holds nothing but this \
             version's cut assets, and refuses one carrying step 7's source pair (SHA256SUMS, \
             SHA256SUMS.sig), keeping the lock. So there, first `{release} --forget`, which \
             withdraws the draft and signs nothing new under v{version}; then, once \
             v{version}'s releases page shows no release, that recover with \
             `--no-draft-was-posted` drops the tag and frees the lock; then {release}",
            lease = crate::publish::RELEASE_LEASE_REF,
            short = short(&old.commit),
            ship = crate::publish::SHIP_COMMAND,
            commit = old.commit,
            release = RELEASE_COMMAND,
        ),
    )
}

/// The way out of a red on the line whose head is `head`: the fix lands on main as its own
/// commit, and `--fix` carries it onto the line as a hop, so the release never re-selects main.
pub(crate) fn fix_remedy(head: &str) -> String {
    format!(
        "land the fix on main, then {RELEASE_COMMAND} --fix <the fix's own non-merge commit>; \
         if it conflicts, author a backport on {} whose every changed path matches origin/main \
         in content and mode (including deletions), land it on main, then \
         {RELEASE_COMMAND} --fix <that non-merge commit>",
        short(head)
    )
}

/// The two ways on from a refused preflight, cut gate or input judgment, which [`Run::remedy`]
/// reads from the candidate's real state.
struct Remedy {
    /// How a finding in the source is fixed.
    source: String,
    /// The command that runs the release again once a finding on this machine is fixed.
    again: String,
}

/// The refusal of the findings `found`, each with its own fix line, then `remedy`'s two halves:
/// the source's, and this machine's (fix it, then run `remedy.again`).
fn blocked(why: String, found: &[Refusal], remedy: &Remedy) -> Refusal {
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
            "fix each (each block's fix line above) — the source's: {}; this machine's: fix it, \
             then {}",
            remedy.source, remedy.again
        ),
    )
    .detail(detail)
}

/// The preflight's block ([`blocked`]): the red of each of R's commit-only gates that judged R's
/// source red, the gates its findings name ([`Refusal::failing`]). A gate that could not ask
/// origin, and a finding of this machine or the publication engine (its prerequisites, its
/// readiness), decided nothing of R's source and names none.
fn preflight_blocked(why: String, found: &[Refusal], remedy: &Remedy) -> Refusal {
    let refusal = blocked(why, found, remedy);
    let red: Vec<&str> = found
        .iter()
        .filter(|r| !r.could_not_run)
        .filter_map(|r| r.failing.as_deref())
        .collect();
    if red.is_empty() {
        refusal
    } else {
        refusal.failing(format!("the preflight's gates: {}", red.join(", ")))
    }
}

/// What step 4 runs ([`Run::qualify_plan`]): AppKit's synchronous gate and each graph branch's
/// work, `None` where its record stands.
#[derive(Debug, Clone, Default)]
struct Plan {
    /// The quiet windowed check, run synchronously before [`Plan::branches`] start.
    appkit: Option<VerifyRun>,
    export: Option<StageTarget>,
    build: Option<BuildWork>,
    tree: Option<VerifyRun>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.appkit.is_none() && self.branches().is_empty()
    }

    /// The branches it runs, in the order the graph starts them.
    fn branches(&self) -> Vec<Branch> {
        Branch::ALL
            .into_iter()
            .filter(|b| match b {
                Branch::Tree => self.tree.is_some(),
                Branch::Build => self.build.is_some(),
                Branch::Export => self.export.is_some(),
            })
            .collect()
    }

    /// What `branch` runs, in words.
    fn what(&self, branch: Branch) -> String {
        match branch {
            Branch::Tree => self
                .tree
                .as_ref()
                .map(|run| format!("runs {}", half_command(run, None)))
                .unwrap_or_default(),
            Branch::Build => match self.build {
                Some(BuildWork {
                    bundle: true,
                    fixtures: true,
                }) => "builds the bundle, then stages its handoff fixtures".into(),
                Some(BuildWork { bundle: true, .. }) => "builds the bundle".into(),
                Some(_) => "stages the bundle's handoff fixtures".into(),
                None => String::new(),
            },
            Branch::Export => match self.export {
                Some(StageTarget::Local) => "checks the export without a staging push".into(),
                Some(StageTarget::Remote) => "stages the export".into(),
                None => String::new(),
            },
        }
    }
}

/// What step 4's graph answered: each branch's result (`None`: it never ran), and each branch's
/// end in the order they came, with the seconds it ran.
#[derive(Debug, Default)]
struct Graph {
    export: Option<Result<Ran, Refusal>>,
    built: Option<Built>,
    tree: Option<Result<Ran, Refusal>>,
    ended: Vec<(Branch, Ended, u64)>,
}

/// What a branch's thread tells the graph.
enum Note {
    /// It started at this unix second (its `started` event told already).
    Started(Branch, u64),
    /// It never started: another branch's red, or a person's signal, came first.
    Unstarted(Branch),
    Export(Result<Ran, Refusal>),
    Built(Built),
    Tree(Result<Ran, Refusal>),
}

/// How a child's result ends its branch: `verify release` exits 1 on a FAIL (A3 halts it at the
/// first), and `pub stage` on any refusal of its export; another exit is no verdict.
fn ran_ended(branch: Branch, ran: &Result<Ran, Refusal>) -> Ended {
    match ran {
        Ok(ran) if ran.code == 0 => Ended::Passed,
        Ok(ran) if ran.code == 1 => Ended::Red,
        Ok(ran) if branch == Branch::Export && ran.code > 1 && ran.code != 130 => Ended::Red,
        Ok(_) => Ended::CouldNotRun,
        Err(refusal) if refusal.could_not_run => Ended::CouldNotRun,
        Err(_) => Ended::Red,
    }
}

/// How the build's answer ends its branch: red on a refusal of the bundle or its fixtures.
fn built_ended(built: &Built) -> Ended {
    let refused = built
        .artifact
        .iter()
        .filter_map(|a| a.as_ref().err())
        .chain(built.fixtures.iter().filter_map(|f| f.as_ref().err()));
    let mut ended = Ended::Passed;
    for refusal in refused {
        if !refusal.could_not_run {
            return Ended::Red;
        }
        ended = Ended::CouldNotRun;
    }
    ended
}

/// A branch's first line of why, for its red line.
fn first_why(note: &Note) -> String {
    let of = |r: &Result<Ran, Refusal>| match r {
        Ok(ran) => {
            let exit = format!("exit {}", ran.code);
            match ran.detail.last().filter(|l| **l != exit) {
                Some(line) => format!("{exit}: {line}"),
                None => exit,
            }
        }
        Err(refusal) => refusal.why.clone(),
    };
    match note {
        Note::Export(r) | Note::Tree(r) => of(r),
        Note::Built(built) => built
            .artifact
            .iter()
            .filter_map(|a| a.as_ref().err())
            .chain(built.fixtures.iter().filter_map(|f| f.as_ref().err()))
            .map(|r| r.why.clone())
            .next()
            .unwrap_or_default(),
        Note::Started(..) | Note::Unstarted(_) => String::new(),
    }
}

/// The command a branch's red names to run it again.
fn rerun_of(branch: Branch, plan: &Plan, c: &Candidate) -> String {
    match branch {
        Branch::Tree => plan
            .tree
            .as_ref()
            .map(|run| format!("`{}`", half_command(run, None)))
            .unwrap_or_default(),
        Branch::Build => format!("`{} build`", crate::publish::SHIP_COMMAND),
        Branch::Export => format!(
            "`pub stage aterm --commit {}{}`",
            c.commit,
            if plan.export == Some(StageTarget::Local) {
                " --dry-run"
            } else {
                ""
            }
        ),
    }
}

/// A `qualify-branch` event: one branch's start or end, and the file name of its log
/// ([`Branches::branch_log`]), for the timeline's readers ([`crate::release_timeline`]) and the
/// `ReleaseQualify` model's conformance.
fn branch_event(br: &dyn Branches, branch: Branch, phase: &str, at: u64, seconds: u64) {
    let log = br.branch_log(branch);
    let log = log.file_name().map(|n| n.to_string_lossy().into_owned());
    let event = aterm_json::Value::Object(aterm_json::Map::from([
        ("schema".to_string(), 1.into()),
        ("event".to_string(), "qualify-branch".into()),
        ("branch".to_string(), branch.name().into()),
        ("log".to_string(), log.into()),
        ("phase".to_string(), phase.into()),
        ("at_unix_s".to_string(), at.into()),
        ("seconds".to_string(), seconds.into()),
    ]));
    br.say(&format!("release-event: {event}"));
}

fn apply_tree_admission(admitted: bool, stop: &BranchStop) {
    if !admitted {
        stop.fire();
    }
}

/// Run step 4's graph on `br`: the build first; the tree once the slices' future compiler memory
/// is reserved ([`Branches::build_admitted`]); and, where both compile, resolve the initial CPU
/// barrier once the tree's exact whole-stage Tippy claim is held or queued
/// ([`Branches::tree_admitted`], [`Branches::release_admission_barrier`]). A held request restores
/// the build-first all-but-one/one shares; a queued request transfers the whole non-export
/// allocation to build until it drains; an exact malformed shape transfers that allocation and
/// stops tree before releasing the barrier. Then export starts. The ledger's memory
/// claims decide what runs together inside the bounded allocation
/// [`Backend::qualify_begin`] made. A branch's red stops the other branches at once,
/// except a proven test-execution red retains an already running build for the next hop
/// ([`BranchStop::fire`]: an export whose staging push began finishes it, and a branch not
/// started never starts), and says so in a branch line with its rerun; a COULD NOT RUN stops
/// nothing. Returns once every branch it started ended.
fn run_graph(
    br: &dyn Branches,
    c: &Candidate,
    p: &Prover,
    reserved: Option<&Reservation>,
    plan: &Plan,
) -> Graph {
    use std::sync::atomic::{AtomicBool, Ordering};
    let first = BranchStop::default();
    let stops: [BranchStop; 3] = [first.beside(), first.beside(), first.beside()];
    let stop_of = |b: Branch| &stops[b as usize];
    // The build ended or a red came, told once the receiver fired every stop that end fires, so
    // a branch the starter starts after it sees them.
    let settled = AtomicBool::new(false);
    let red = AtomicBool::new(false);
    let mut graph = Graph::default();
    let t0 = br.now();
    let build_admission = plan
        .build
        .zip(reserved)
        .is_some_and(|(work, _)| work.bundle);
    let coordinated_admission = build_admission
        && plan
            .tree
            .as_ref()
            .is_some_and(|run| run.checks.iter().any(|check| check == "lint"));
    std::thread::scope(|s| {
        let (tx, rx) = std::sync::mpsc::channel::<Note>();
        let (settled, stops) = (&settled, &stops);
        let starter = tx.clone();
        s.spawn(move || {
            let stop_of = |b: Branch| &stops[b as usize];
            if !build_admission {
                br.release_admission_barrier();
            }
            if plan
                .tree
                .as_ref()
                .is_some_and(|run| run.checks.iter().any(|check| check == "test"))
            {
                br.expect_tree_index();
            }
            if let (Some(work), Some(reserved)) = (plan.build, reserved) {
                start_branch(
                    s,
                    br,
                    &starter,
                    Branch::Build,
                    stop_of(Branch::Build),
                    move |stop| Note::Built(br.build(c, reserved, p, work, stop)),
                );
                if work.bundle {
                    br.build_admitted(&|| settled.load(Ordering::SeqCst));
                    if !coordinated_admission {
                        br.release_admission_barrier();
                    }
                }
            }
            if let Some(run) = &plan.tree {
                start_branch(
                    s,
                    br,
                    &starter,
                    Branch::Tree,
                    stop_of(Branch::Tree),
                    move |stop| Note::Tree(br.verify(Step::Qualify, c, p, run, None, stop)),
                );
            }
            if coordinated_admission {
                apply_tree_admission(
                    br.tree_admitted(&|| settled.load(Ordering::SeqCst)),
                    stop_of(Branch::Tree),
                );
                br.release_admission_barrier();
            }
            if let Some(target) = plan.export {
                start_branch(
                    s,
                    br,
                    &starter,
                    Branch::Export,
                    stop_of(Branch::Export),
                    move |stop| Note::Export(br.export(c, p, target, stop)),
                );
            }
        });
        drop(tx);
        let mut started: BTreeMap<Branch, u64> = BTreeMap::new();
        // Each end is decided and told in the graph's one order, so a red's event and the stops
        // it fires read before any start or push that came after it.
        for note in rx {
            first.in_order(|| {
                let now = br.now();
                let (branch, ended) = match &note {
                    Note::Started(branch, at) => {
                        started.insert(*branch, *at);
                        return;
                    }
                    Note::Unstarted(branch) => (*branch, Ended::Stopped),
                    Note::Export(ran) => (Branch::Export, ran_ended(Branch::Export, ran)),
                    Note::Tree(ran) => (Branch::Tree, ran_ended(Branch::Tree, ran)),
                    Note::Built(built) => (Branch::Build, built_ended(built)),
                };
                // A branch ends stopped, with no verdict of its own, when a person's signal cut
                // it short (neither a red that stops the others, since the signal reaches them by
                // itself and a stop on top of it would count as a second signal, nor a pass), or
                // when another's stop reached it: unless it was red first, since a red after
                // another's is no verdict, the first red is.
                let stopped = (ended != Ended::Passed && br.interrupted())
                    || (stop_of(branch).fired()
                        && (ended != Ended::Red || red.load(Ordering::SeqCst)));
                let ended = if stopped { Ended::Stopped } else { ended };
                let secs = started.get(&branch).map_or(0, |at| now.saturating_sub(*at));
                branch_event(br, branch, ended.word(), now, secs);
                graph.ended.push((branch, ended, secs));
                let why = first_why(&note);
                match note {
                    Note::Export(ran) => graph.export = Some(ran),
                    Note::Tree(ran) => graph.tree = Some(ran),
                    Note::Built(built) => graph.built = Some(built),
                    Note::Started(..) | Note::Unstarted(_) => {}
                }
                let at = fmt_dur(now.saturating_sub(t0));
                if ended == Ended::Red && !red.swap(true, Ordering::SeqCst) {
                    let keep_build = branch == Branch::Tree
                        && started.contains_key(&Branch::Build)
                        && br.keep_build_on_test_red(c);
                    let done: Vec<Branch> = graph.ended.iter().map(|(b, ..)| *b).collect();
                    let halted: Vec<&str> = plan
                        .branches()
                        .into_iter()
                        .filter(|b| {
                            !done.contains(b)
                                && !(keep_build && *b == Branch::Build)
                                && stop_of(*b).fire()
                        })
                        .map(Branch::name)
                        .collect();
                    // A branch outside the plan never starts; its stop fires all the same.
                    for b in Branch::ALL {
                        if !(keep_build && b == Branch::Build) {
                            stop_of(b).fire();
                        }
                    }
                    let pushing =
                        stop_of(Branch::Export).pushing() && !done.contains(&Branch::Export);
                    br.say(&format!(
                        "        {}: RED at {at} — {why}; rerun: {}; stopping {}{}",
                        branch.name(),
                        rerun_of(branch, plan, c),
                        if halted.is_empty() {
                            "nothing".to_string()
                        } else {
                            halted.join(", ")
                        },
                        if pushing {
                            " (the export's staging push began, so it finishes)"
                        } else {
                            ""
                        }
                    ));
                } else {
                    br.say(&format!(
                        "        {}: {} at {at} after {}{}",
                        branch.name(),
                        ended.word().replace('-', " "),
                        fmt_dur(secs),
                        if ended == Ended::CouldNotRun {
                            format!(" — {why}; the other branches finish")
                        } else {
                            String::new()
                        }
                    ));
                }
                if matches!(branch, Branch::Build | Branch::Tree) || ended == Ended::Red {
                    settled.store(true, Ordering::SeqCst);
                }
            });
        }
    });
    graph
}

/// Start `branch`'s thread in `s`: unstarted when a red stopped it first or a person's signal
/// came, else told started (from the starter, so the graph hears the starts in their order), its
/// `work` run, and its end told to the graph. A branch that panics (a bug in the release) ends
/// red, so the others stop as they would for any red.
fn start_branch<'scope>(
    s: &'scope std::thread::Scope<'scope, '_>,
    br: &'scope dyn Branches,
    tx: &std::sync::mpsc::Sender<Note>,
    branch: Branch,
    stop: &'scope BranchStop,
    work: impl FnOnce(&BranchStop) -> Note + Send + 'scope,
) {
    let started = stop.in_order(|| {
        if stop.fired() || br.interrupted() {
            let _ = tx.send(Note::Unstarted(branch));
            return false;
        }
        let at = br.now();
        branch_event(br, branch, "started", at, 0);
        let _ = tx.send(Note::Started(branch, at));
        true
    });
    if !started {
        return;
    }
    let tx = tx.clone();
    s.spawn(move || {
        let note = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(stop)))
            .unwrap_or_else(|_| {
                let refusal = Refusal::new(
                    format!(
                        "the {} branch stopped unexpectedly (a bug in {RELEASE_COMMAND})",
                        branch.name()
                    ),
                    format!("read its log, then run {RELEASE_COMMAND} again to resume"),
                );
                match branch {
                    Branch::Tree => Note::Tree(Err(refusal)),
                    Branch::Export => Note::Export(Err(refusal)),
                    Branch::Build => Note::Built(Built {
                        artifact: Some(Err(refusal)),
                        fixtures: None,
                    }),
                }
            });
        let _ = tx.send(note);
    });
}

/// What step 7's sides answered ([`run_sides`]): each side's result (`None`: it never ran), and
/// each side's end in the order they came, with the seconds it ran.
#[derive(Debug, Default)]
struct Sides {
    source: Option<Result<Ran, Refusal>>,
    sign: Option<Ran>,
    ended: Vec<(Side, Ended, u64)>,
    /// The source's pre-promote hook took this run's gate for its push.
    pushed: bool,
}

impl Sides {
    /// How `side` ended; `None` when it was not run.
    fn of(&self, side: Side) -> Option<Ended> {
        self.ended
            .iter()
            .find(|(s, ..)| *s == side)
            .map(|(_, ended, _)| *ended)
    }
}

/// What a side's thread tells step 7.
enum SideNote {
    Started(Side, u64),
    Unstarted(Side),
    Source(Result<Ran, Refusal>),
    Sign(Ran),
}

/// How `pub publish`'s answer ends the source: any refusal of the publish is red; an interrupted
/// or unstarted child, and a refusal that could not run, decide nothing.
fn source_ended(ran: &Result<Ran, Refusal>) -> Ended {
    match ran {
        Ok(ran) if ran.code == 0 => Ended::Passed,
        Ok(ran) if ran.code == 130 => Ended::CouldNotRun,
        Ok(_) => Ended::Red,
        Err(refusal) if refusal.could_not_run => Ended::CouldNotRun,
        Err(_) => Ended::Red,
    }
}

/// How `cut --sign`'s exit ends the sign: the cutter exits 1 on any refusal; another exit (an
/// interrupt, a signal, a child that never started) decides nothing.
fn sign_ended(ran: &Ran) -> Ended {
    match ran.code {
        0 => Ended::Passed,
        1 => Ended::Red,
        _ => Ended::CouldNotRun,
    }
}

/// A side's first line of why, for its red line.
fn side_why(note: &SideNote) -> String {
    let of = |ran: &Ran| {
        let exit = format!("exit {}", ran.code);
        match ran.detail.last().filter(|l| **l != exit) {
            Some(line) => format!("{exit}: {line}"),
            None => exit,
        }
    };
    match note {
        SideNote::Source(Ok(ran)) | SideNote::Sign(ran) => of(ran),
        SideNote::Source(Err(refusal)) => refusal.why.clone(),
        SideNote::Started(..) | SideNote::Unstarted(_) => String::new(),
    }
}

/// A `publish-branch` event: one side's start, its push begun, or its end, and the file name of
/// its log ([`Branches::side_log`]), for the timeline's readers and the `ReleaseSignBesidePublish`
/// model's conformance.
fn side_event(br: &dyn Branches, side: Side, phase: &str, at: u64, seconds: u64) {
    let log = br.side_log(side);
    let log = log.file_name().map(|n| n.to_string_lossy().into_owned());
    let event = aterm_json::Value::Object(aterm_json::Map::from([
        ("schema".to_string(), 1.into()),
        ("event".to_string(), "publish-branch".into()),
        ("branch".to_string(), side.name().into()),
        ("log".to_string(), log.into()),
        ("phase".to_string(), phase.into()),
        ("at_unix_s".to_string(), at.into()),
        ("seconds".to_string(), seconds.into()),
    ]));
    br.say(&format!("release-event: {event}"));
}

/// Run step 7 on `br`: the source (when `source`) and the sign (`sign`'s arguments) at once, each
/// in a thread of its own. A source red stops the sign. A sign red takes the gate's stop
/// ([`PublishGate::stop`]): where it wins, the source is stopped and its hook never pushes, so
/// nothing is public; where the source's push began first, the source finishes, public ahead of
/// its binary. A COULD NOT RUN stops nothing. Every start, push and end is decided and told in
/// one order ([`BranchStop::in_order`]). Returns once every side it started ended.
fn run_sides(
    br: &dyn Branches,
    p: &Prover,
    source: bool,
    sign: &[String],
    early: Option<Box<dyn PendingSign>>,
) -> Sides {
    let first = BranchStop::default();
    let mut sign_stop = early
        .as_ref()
        .map_or_else(|| first.beside(), |child| child.stop());
    sign_stop.told = std::sync::Arc::clone(&first.told);
    let stops: [BranchStop; 2] = [first.beside(), sign_stop];
    let stop_of = |s: Side| &stops[s as usize];
    let gate = br.publish_gate();
    let mut sides = Sides::default();
    let (mut red, mut push_told) = (false, false);
    let t0 = br.now();
    std::thread::scope(|s| {
        let (tx, rx) = std::sync::mpsc::channel::<SideNote>();
        let gate = &gate;
        if source {
            start_side(
                s,
                br,
                &tx,
                Side::Source,
                stop_of(Side::Source),
                move |stop| SideNote::Source(br.source(p, gate, stop)),
            );
        }
        start_side(s, br, &tx, Side::Sign, stop_of(Side::Sign), move |stop| {
            if let Some(child) = early {
                let ran = child.finish();
                if ran.code != 0 {
                    return SideNote::Sign(ran);
                }
            }
            SideNote::Sign(br.sign(sign, p, stop))
        });
        drop(tx);
        let mut started: BTreeMap<Side, u64> = BTreeMap::new();
        for note in rx {
            first.in_order(|| {
                let now = br.now();
                let (side, ended) = match &note {
                    SideNote::Started(side, at) => {
                        started.insert(*side, *at);
                        return;
                    }
                    SideNote::Unstarted(side) => (*side, Ended::Stopped),
                    SideNote::Source(ran) => (Side::Source, source_ended(ran)),
                    SideNote::Sign(ran) => (Side::Sign, sign_ended(ran)),
                };
                // As in step 4's graph: a person's signal ends a side stopped, no red that takes
                // the gate or stops the other (the signal reaches it by itself); another's stop
                // ends it stopped unless its own red came first.
                let stopped = (ended != Ended::Passed && br.interrupted())
                    || (stop_of(side).fired() && (ended != Ended::Red || red));
                let ended = if stopped { Ended::Stopped } else { ended };
                if side == Side::Sign
                    && let Err(error) = gate.sign_finished(ended.word())
                {
                    br.say(&format!(
                        "        cannot record private draft writer completion: {error}"
                    ));
                }
                // The first red's gate is taken before its end is told: a push that began first is
                // told before it, as it came.
                let taken =
                    (side == Side::Sign && ended == Ended::Red && !red).then(|| gate.stop());
                let pushed = match side {
                    Side::Source => {
                        gate.word().ok().flatten().as_deref() == Some(PublishGate::PUSH)
                    }
                    Side::Sign => matches!(taken, Some(Ok(false))),
                };
                if pushed && !push_told {
                    push_told = true;
                    side_event(br, Side::Source, "push-began", now, 0);
                }
                let secs = started.get(&side).map_or(0, |at| now.saturating_sub(*at));
                side_event(br, side, ended.word(), now, secs);
                sides.ended.push((side, ended, secs));
                let why = side_why(&note);
                match note {
                    SideNote::Source(ran) => sides.source = Some(ran),
                    SideNote::Sign(ran) => sides.sign = Some(ran),
                    SideNote::Started(..) | SideNote::Unstarted(_) => {}
                }
                let at = fmt_dur(now.saturating_sub(t0));
                if ended != Ended::Red || red {
                    br.say(&format!(
                        "        {}: {} at {at} after {}{}",
                        side.name(),
                        ended.word().replace('-', " "),
                        fmt_dur(secs),
                        match ended {
                            Ended::CouldNotRun => format!(" — {why}; the other side finishes"),
                            Ended::Red => format!(" — {why}"),
                            Ended::Passed | Ended::Stopped => String::new(),
                        }
                    ));
                    return;
                }
                red = true;
                let other = match side {
                    Side::Source => Side::Sign,
                    Side::Sign => Side::Source,
                };
                let running = started.contains_key(&other) && sides.of(other).is_none();
                let stopping = match side {
                    Side::Source => {
                        let halted = stop_of(Side::Sign).fire() && running;
                        if halted { "the sign" } else { "nothing" }.to_string()
                    }
                    Side::Sign => match taken.unwrap_or(Ok(true)) {
                        Ok(true) => {
                            let halted = stop_of(Side::Source).fire() && running;
                            if halted {
                                "the source, whose hook can no longer push"
                            } else {
                                "nothing (no source push can begin)"
                            }
                            .to_string()
                        }
                        Ok(false) => {
                            "nothing: the source's push began, so it finishes, public ahead of \
                             its binary"
                                .to_string()
                        }
                        Err(e) => format!(
                            "nothing: the gate {} could not be taken ({e}), and the source \
                             finishes",
                            gate.path().display()
                        ),
                    },
                };
                br.say(&format!(
                    "        {}: RED at {at} — {why}; stopping {stopping}",
                    side.name()
                ));
            });
        }
    });
    sides.pushed = gate.word().ok().flatten().as_deref() == Some(PublishGate::PUSH);
    sides
}

/// Start `side`'s thread in `s`: unstarted when a red stopped it first or a person's signal came,
/// else told started, its `work` run, and its end told to step 7. A side that panics (a bug in the
/// release) ends red.
fn start_side<'scope>(
    s: &'scope std::thread::Scope<'scope, '_>,
    br: &'scope dyn Branches,
    tx: &std::sync::mpsc::Sender<SideNote>,
    side: Side,
    stop: &'scope BranchStop,
    work: impl FnOnce(&BranchStop) -> SideNote + Send + 'scope,
) {
    let started = stop.in_order(|| {
        if stop.fired() || br.interrupted() {
            let _ = tx.send(SideNote::Unstarted(side));
            return false;
        }
        let at = br.now();
        side_event(br, side, "started", at, 0);
        let _ = tx.send(SideNote::Started(side, at));
        true
    });
    if !started {
        return;
    }
    let tx = tx.clone();
    s.spawn(move || {
        let note = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(stop)))
            .unwrap_or_else(|_| {
                let why = format!(
                    "step 7's {} side stopped unexpectedly (a bug in {RELEASE_COMMAND})",
                    side.name()
                );
                match side {
                    Side::Source => SideNote::Source(Err(Refusal::new(
                        why,
                        format!("read its log, then run {RELEASE_COMMAND} again to resume"),
                    ))),
                    Side::Sign => SideNote::Sign(Ran {
                        code: 1,
                        detail: vec![why],
                    }),
                }
            });
        let _ = tx.send(note);
    });
}

/// `verify release` of `run`'s half as an operator runs it: `--tree` or `--bundle`, `--checks`
/// where it is fewer than its half, and `--app` with the check that reads it.
#[must_use]
pub fn half_command(run: &VerifyRun, app: Option<&str>) -> String {
    let mut words = format!("targo {} {}", VERIFY_ARGS.join(" "), run.half.flag());
    if run.strict_subset() {
        words.push_str(&format!(" --checks {}", run.checks.join(",")));
    }
    if run.reads_the_bundle()
        && let Some(app) = app
    {
        words.push_str(&format!(" --app {}", shell_word(app)));
    }
    words
}

fn fixture_generator() -> String {
    format!(
        "the in-tree generator (targo --unverified test -p aterm-gui --lib -- {})",
        crate::fixtures::generator_args().join(" ")
    )
}

fn running_refusal(h: &Holder) -> Refusal {
    if let Some(step) = &h.step {
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

fn signal_name(sig: i32) -> String {
    match sig {
        1 => "SIGHUP".into(),
        2 => "SIGINT".into(),
        15 => "SIGTERM".into(),
        n => format!("signal {n}"),
    }
}

/// A historical or calendar public product version's full three-component identity.
#[must_use]
pub fn public_version(v: &str) -> Option<(u64, u64, u64)> {
    aterm_types::release_version::ReleaseVersion::parse(v).map(|v| v.components())
}

fn newer(a: &str, b: &str) -> bool {
    matches!((public_version(a), public_version(b)), (Some(a), Some(b)) if a > b)
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;

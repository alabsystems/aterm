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
const SELECTION_FILE: &str = "candidate.toml";

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
    /// R's tree judged, its bundle built and its export checked, at once ([`Branch`]).
    Qualify,
    /// The bundle half of `verify release`, once the build passed and the tree branch ended.
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
    pub const fn could_not_run(mut self) -> Self {
        self.could_not_run = true;
        self
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
    /// `--bump`: commit the next MINOR on origin/main's tip and start a line at it.
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
    /// origin the release's own engine is cloned from ([`crate::gates::engine_copy`]).
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
    /// each path flag's value replaced by these options'; `--fix` with its commits left out,
    /// since the new image resumes the head the journal names (the hops are made); and
    /// `--engine` with its directory left out, since the exec'd image receives this release's own
    /// engine as `PUBLICATION_ENGINE` ([`crate::gates::engine_copy`]), and a cutter older than that
    /// copy would run and sync the directory `--engine` named in place.
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
            if arg.to_str() == Some("--fix") {
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
    /// The release version, `X.Y.0`.
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
/// verified bundle signed, notarized and packaged privately by `ship cut --sign`, at once, each
/// in a child of its own. Neither claims; the cut (step 8) claims once both passed.
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
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
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
    /// Return once both release slices hold memory, or `ended` says the build ended.
    /// The measured critical path gets memory before the tree's moldable compile.
    fn build_admitted(&self, ended: &dyn Fn() -> bool);
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
    /// Run `verify release` of `run`'s half in the clone at R — the checks it names, with the
    /// test binaries that passed — on `artifact` for the bundle half, then keep one record per
    /// check it finished ([`crate::check_record::write_run`]).
    fn verify(
        &self,
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

/// Everything the release asks of the world — git, the engine, the records, the children, the clock, the owner's aterm.
pub trait Backend {
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
    fn fleet_supersession(
        &mut self,
        _version: &str,
    ) -> Result<Option<crate::line::Supersession>, Refusal> {
        Ok(None)
    }
    fn supersede_line(&mut self, _line: &crate::line::Line) -> Result<String, String> {
        Err("supersession is unavailable".into())
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
    fn ledger_versions(&mut self, rev: &str) -> Vec<String>;
    fn engine_rows(&mut self, commit: Option<&str>) -> EngineRows;
    fn engine_now(&mut self, staged: &str) -> Result<EngineNow, String>;
    fn candidate_missing(&mut self) -> Result<Option<String>, String>;
    fn cut_journal(&mut self) -> Option<CutState>;
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
    fn verify_pending_line(&mut self, line: &crate::line::Line) -> Result<String, Refusal> {
        self.verify_line(line)
    }

    fn sync(&mut self) -> Result<String, Refusal>;
    fn bump(&mut self, from: &str, to: &str, prover: &Prover) -> Result<String, Refusal>;
    fn move_to(&mut self, commit: &str) -> Result<(), Refusal>;
    /// Rewind only a clean, structurally proved, locally generated pending line to its source
    /// prefix before adopting another fleet line. No owner-authored commit is discarded.
    fn rewind_selection(&mut self, _line: &crate::line::Line) -> Result<(), Refusal> {
        Ok(())
    }
    /// Provision the selected checkout's tools before staging or compiling the product.
    fn validation_tools(&mut self) -> Result<String, Refusal>;
    fn own_cutter(&mut self, commit: &str) -> Result<String, crate::cutter::NotOwn>;
    fn frozen_commit(&mut self) -> Option<String> {
        None
    }
    /// Build, freeze and prove R's tools, then replace this image with its cutter. A real
    /// success never returns; `Ok(())` is only a model backend's representation of that edge.
    fn exec_cutter(&mut self, commit: &str, prover: &Prover) -> Result<(), Refusal>;
    /// Start the verifier build beside steps 1–2; validation_tools joins and freezes it before
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
    fn commit_gates(&mut self, c: &Candidate, dry: bool) -> Vec<Refusal>;
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
    fn origin_tail(&mut self) -> Option<u64>;
    fn reserve(&mut self, c: &Candidate) -> Result<Reservation, Refusal>;
    /// Before the qualify graph starts: admit its one core claim and make the pool every branch
    /// draws from, holding the export's share of it when `export` runs, and ready the export's
    /// engine and scanner tools; its words.
    fn qualify_begin(&mut self, prover: &Prover, export: bool) -> Result<String, Refusal>;
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
    fn source_publication(&mut self, c: &Candidate) -> Result<(), Refusal>;
    fn cut(&mut self, args: &[String], prover: &Prover) -> Ran;
    /// Discard the copy step 7 signed of build `build` of `version` from the clone's `dist/`
    /// ([`crate::publish::discard_signed`]); what could not be removed.
    fn discard_signed(&mut self, version: &str, build: u64) -> Vec<String>;
    fn after_cut(&mut self, version: &str) -> Result<AfterCut, Refusal>;
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
        journal: None,
        claim: None,
        claim_refused: None,
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
        disk_held: false,
        exported_locally: false,
        early_asks: Vec::new(),
        waited: 0,
        dry_audit: None,
        last: String::new(),
    };
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
    run.b.stop_tools();
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
        self.t_start = self.b.now();
        self.cur_t0 = self.t_start;
        self.b.observe_operational();
        if self.dry {
            self.say(
                "release: DRY RUN — nothing is changed; each step says what a real run would do",
            );
        }

        if !self.dry {
            self.journal_path = selection;
            self.prepare_candidate()?;
            let prover = self.prover.clone().ok_or(Halt::Said(1))?;
            let commit = self.commit_r();
            if let Err(refusal) = self.b.warm_tools(&commit, &prover) {
                return self.refuse_hard(refusal);
            }
        }

        self.begin(Step::Checks)?;
        let note = self.checks()?;
        self.finish(&note);

        self.begin(Step::Sync)?;
        if self.dry {
            self.finish(
                "would bring the release's own engine to its origin/main (cloned from it the \
                 first time; appended audit rows kept, unpushed records delivered) and run \
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
            let _ = std::fs::remove_file(self.b.release_dir().join(SELECTION_FILE));
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

    fn halt_if_interrupted(&mut self) -> Flow {
        if let Some(why) = crate::line_claim::failure() {
            return self.refuse(
                Refusal::new(why, "resume on the fleet line's current holder").could_not_run(),
            );
        }
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

    fn refuse_hard(&mut self, r: Refusal) -> Flow {
        self.refuse(r)?;
        self.dry_final()
    }

    fn final_refusal(&mut self, r: Refusal) -> Flow {
        self.halt_if_interrupted()?;
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
            self.last = if self.opts.qualify {
                format!(
                    "DRY RUN aterm {v} from {}: --qualify would check the full export locally, build and verify, about {} of work{waits}; no staging snapshot pushed or publication — nothing was changed",
                    short(&r),
                    fmt_dur(self.plan_secs)
                )
            } else if self.opts.prepare {
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
        let at = self.b.hms(now.saturating_add(total));
        format!(
            "  · ETA {} ({} to go)",
            at.get(..5).unwrap_or(&at),
            fmt_dur(total)
        )
    }

    /// A step's estimate from this machine's `steps.tsv`, else the plan's default. Step 4's is
    /// its longest branch's (the branches run at once): each branch's own row, else its default.
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
            return plan
                .branches()
                .into_iter()
                .map(|b| self.est_branch(b))
                .max()
                .unwrap_or(0);
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
        let selected = self.b.release_dir().join(SELECTION_FILE);
        let asks = Journal::load(&selected)
            .and_then(|pending| {
                pending.map_or_else(|| Journal::load(&self.journal_path), |j| Ok(Some(j)))
            })
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
        let _ = std::fs::remove_file(self.b.release_dir().join(SELECTION_FILE));
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
            self.disk_still_needed("the qualify graph and the bundle half")?;
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
                        "release the next MINOR: {RELEASE_COMMAND} --forget, then \
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
                if !self.dry {
                    if let Err(error) = self.b.lease(&p) {
                        self.refuse_hard(Refusal::new(
                            error,
                            "restore the toolchain lease and retry",
                        ))?;
                    }
                    // Under the lease, so the seal is of the toolchain the lease holds.
                    if let Err(error) = self.b.seal_inputs() {
                        self.refuse_hard(Refusal::new(
                            format!("the release build inputs cannot be sealed: {error}"),
                            "restore the release build inputs and retry",
                        ))?;
                    }
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
            if !self.dry {
                let _ = std::fs::remove_file(self.b.release_dir().join(SELECTION_FILE));
                self.journal = Journal::load(&self.b.release_dir().join(JOURNAL_FILE))
                    .ok()
                    .flatten();
            }
            return self.refuse_hard(Refusal::new(
                "the release in progress is already published or claimed; its local fix was not recorded",
                "finish the published release, then start the next line",
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

    /// Select locally, freeze, and only then let the candidate's cutter judge the release.
    /// Selection is deliberately independent of the launching cutter's origin refresh and
    /// publication roster reader. An old local origin/main is a pinned candidate, not a claim
    /// about the current remote tip; step 1 refreshes and proves its ancestry before a product
    /// build or staging begins.
    fn prepare_candidate(&mut self) -> Flow {
        if let Some(refusal) = self.claim_refused.take() {
            return self.refuse_hard(refusal);
        }
        if !self.b.branch_is_main() {
            return self.refuse_hard(Refusal::new(
                "this clone is not on main",
                "release from a clean clone on main (git switch main)",
            ));
        }
        if let Err(refusal) = self.b.prepare_clone() {
            return self.refuse_hard(refusal);
        }
        self.tip = self.b.rev("origin/main^{commit}").ok_or_else(|| {
            self.halted(Refusal::new(
                "this clone has no local origin/main candidate",
                "git fetch origin, then run the release again",
            ))
        })?;
        self.choose_candidate(true)?;
        let r = self.commit_r();
        if self.b.head() != r
            && let Err(refusal) = self.b.move_to(&r)
        {
            return self.refuse_hard(refusal);
        }
        let journaled = self
            .journal
            .as_ref()
            .filter(|j| !j.finished())
            .map(|j| j.prover.clone());
        let prover = self.b.prover(journaled.as_deref()).map_err(|why| {
            self.halted(Refusal::new(
                format!("no Trust prover: {why}"),
                "restore the locked Trust toolchain and retry",
            ))
        })?;
        if let Err(why) = self.b.lease(&prover) {
            return self.refuse_hard(Refusal::new(why, "restore the toolchain lease and retry"));
        }
        self.prover = Some(prover);
        // A bump needs the publication engine. Dispatch to its base's content-proved cutter
        // first, so no previous line's engine discovery can hold the next line hostage.
        if self.cand.as_ref().is_some_and(|c| c.bump_from.is_some()) {
            self.own_cutter()?;
            if let Err(refusal) = self.b.sync() {
                return self.refuse_hard(refusal);
            }
        }
        self.commit(true)?;
        self.own_cutter()?;
        if self.opts.supersede {
            if !self
                .journal
                .as_ref()
                .is_some_and(|j| j.has(SUPERSEDED_LINE))
            {
                let version = self.version();
                if self.version_used(&version, &self.tip.clone()) {
                    return self.refuse_hard(Refusal::new(
                        format!("v{version} has public release state and cannot be superseded"),
                        "finish the existing release",
                    ));
                }
                if let Some(previous) = Journal::load(&self.b.release_dir().join(JOURNAL_FILE))
                    .ok()
                    .flatten()
                    .filter(|j| !j.finished())
                {
                    self.refuse_published(&previous, "--supersede")?;
                }
                let line = crate::line::Line::start(&version, &self.commit_r(), self.t_start);
                match self.b.supersede_line(&line) {
                    Ok(note) => self.say(&note),
                    Err(why) => {
                        let code = if why.starts_with("CUTTING ELSEWHERE:") {
                            4
                        } else {
                            2
                        };
                        self.say(&why);
                        self.last = why;
                        return Err(Halt::Said(code));
                    }
                }
                if let Some(journal) = self.journal.as_mut() {
                    journal.add(SUPERSEDED_LINE);
                }
                self.save_journal()?;
            }
            self.opts.supersede = false;
            self.b.supersession_completed();
        }
        // Fleet discovery also belongs to a frozen image, but must precede judgment: adopting
        // another head after checks would otherwise judge once with the wrong cutter.
        let pending = self
            .journal
            .clone()
            .filter(|j| j.has(PENDING_LINE))
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
            self.last = why;
            let last = self.last.clone();
            self.say(&last);
            return Err(Halt::Said(if last.starts_with("CUTTING ELSEWHERE:") {
                4
            } else {
                3
            }));
        }
        if selected.commit != self.commit_r() {
            if let Some(pending) = pending
                && let Err(refusal) = self.b.rewind_selection(&pending)
            {
                return self.refuse_hard(refusal);
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
        Ok(())
    }

    fn choose_candidate(&mut self, selecting: bool) -> Flow {
        let tip = self.tip.clone();
        let j = self.journal.clone();
        let unfinished = j.as_ref().filter(|j| !j.finished());
        let continued = self.claim.as_ref().is_some_and(|c| c.continued);
        let mut cand = if !selecting && !self.dry {
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
            let Some((major, minor)) = public_version(&v0) else {
                return self.refuse_hard(Refusal::new(
                    format!("origin/main's version '{v0}' is not MAJOR.MINOR.0"),
                    "fix [workspace.package] version on main",
                ));
            };
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
            let v = format!("{major}.{}.0", minor + 1);
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
                        "{} has no public MAJOR.MINOR.0 version",
                        short(&cand.commit)
                    ),
                    "select a release commit with --commit",
                ));
            }
            self.cand = Some(cand);
            return Ok(());
        }

        if !self.b.is_ancestor(&cand.commit, &tip) {
            self.cand = Some(cand.clone());
            let hop = self.heads_a_hop(&cand.commit);
            return self.refuse_hard(Refusal::new(
                format!(
                    "{} is not on origin/main — the engine stages only mainline commits",
                    short(&cand.commit)
                ),
                if hop {
                    lost_record()
                } else {
                    format!("land it on main first, then re-run {RELEASE_COMMAND}")
                },
            ));
        }
        if cand.version.is_empty() {
            cand.version = self.b.version_at(&cand.commit).unwrap_or_default();
        }
        if self.dry
            && public_version(&cand.version).is_some()
            && (!self.opts.supersede || continued)
            && let Err(refusal) = crate::line_claim::follow(self.b, &mut cand, &mut self.journal)
        {
            return self.refuse_hard(refusal);
        }
        self.cand = Some(cand.clone());
        let (r, v) = (cand.commit.clone(), cand.version.clone());
        if self.opts.supersede && !continued {
            if self.version_used(&v, &tip) {
                return self.refuse_hard(Refusal::new(
                    format!("v{v} has public release state and cannot be superseded"),
                    "finish the existing release",
                ));
            }
            if let Some(j) = unfinished {
                self.refuse_published(j, "--supersede")?;
            }
        }
        if public_version(&v).is_none() {
            let remedy = format!("release the next MINOR: {}", self.start_remedy("--bump"));
            return self.refuse_hard(Refusal::new(
                format!(
                    "{} declares version '{v}', not a public MAJOR.MINOR.0",
                    short(&r)
                ),
                remedy,
            ));
        }

        if let Some(prev) = self.prev_release(&r, &v) {
            if !newer(&v, &prev) {
                let remedy = format!("release the next MINOR: {}", self.start_remedy("--bump"));
                return self.refuse_hard(Refusal::new(
                    format!("v{v} is not newer than v{prev}, the release it would succeed"),
                    remedy,
                ));
            }
            let claim = self.b.origin_tag(&format!("v{prev}")).unwrap_or_default();
            if claim.is_empty() || !self.b.is_ancestor(&claim, &r) {
                let remedy = format!(
                    "release a commit of origin/main after that claim: {}",
                    self.start_remedy("--commit origin/main")
                );
                return self.refuse_hard(Refusal::new(
                    format!(
                        "{} does not contain v{prev}'s claim commit {}, so the release it \
                         succeeds is not in its history",
                        short(&r),
                        short(&claim)
                    ),
                    remedy,
                ));
            }
        }

        if self.b.origin_tag(&format!("v{v}")).is_some() && !self.claimed() {
            let remedy = match &cand.bump_from {
                Some(v0) => format!(
                    "origin/main still declares {v0}: find what cut v{v} (git ls-remote --tags \
                     origin v{v}) and bring main's version past it, then {RELEASE_COMMAND} --bump"
                ),
                None => format!("release the next MINOR: {}", self.start_remedy("--bump")),
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

        let rows = self.b.engine_rows(Some(&r));
        if let Some(newest) = rows.newest.clone()
            && !rows.has
            && self.b.version_at(&newest).as_deref() == Some(v.as_str())
            && self.b.origin_tag(&format!("v{v}")).is_none()
        {
            let remedy = format!(
                "finish that release: {}; or release the next MINOR: {}",
                self.start_remedy(&format!("--commit {newest}")),
                self.start_remedy("--bump")
            );
            return self.refuse_hard(Refusal::new(
                format!(
                    "v{v}'s source was published from {}, not {} — the cut releases the \
                     published commit",
                    short(&newest),
                    short(&r)
                ),
                remedy,
            ));
        }
        if let Some(r) = self.newest_row_refusal() {
            return self.refuse_hard(r);
        }
        Ok(())
    }

    /// `{RELEASE_COMMAND} {args}`, which starts a line: after `--forget` while this clone holds
    /// an unfinished line, which neither `--commit` (it refuses) nor `--bump` (it resumes the
    /// line) replaces.
    fn start_remedy(&self, args: &str) -> String {
        if self.journal.as_ref().is_some_and(|j| !j.finished()) {
            format!("{RELEASE_COMMAND} --forget, then {RELEASE_COMMAND} {args}")
        } else {
            format!("{RELEASE_COMMAND} {args}")
        }
    }

    /// `commit` heads this clone's unfinished line past its base: a hop, which origin/main
    /// holds only through the line's record merge.
    fn heads_a_hop(&self, commit: &str) -> bool {
        self.journal
            .as_ref()
            .is_some_and(|j| !j.finished() && j.commit == commit && !j.line().hops.is_empty())
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
                 MINOR: {RELEASE_COMMAND} --forget, then --bump"
            ),
        ))
    }

    /// `--fix`'s candidate: the head of this clone's unfinished line, neither published nor
    /// claimed, whose tree knows lines ([`LINE_SOURCE`]), with each commit `--fix` names
    /// admitted ([`Backend::fix_of`]) for step 3 to carry. One a hop carries already is dropped;
    /// when every one is, the run resumes the line as it is.
    fn fix_candidate(
        &mut self,
        unfinished: Option<&Journal>,
        selecting: bool,
    ) -> Result<Candidate, Halt> {
        let Some(j) = unfinished else {
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

    fn commit(&mut self, selecting: bool) -> Result<String, Halt> {
        let Some(mut cand) = self.cand.clone() else {
            return Ok(String::new());
        };
        if self.dry {
            return Ok(match &cand.bump_from {
                Some(_) => format!(
                    "would commit `release: {} — the version and the lockfile move together` on \
                     {} (pub-bump-edit, then targo update --workspace --offline, each nested \
                     lockfile's path crates following; with the harness versions its window \
                     owes, `ship harness-bump`'s writes) and push it to main",
                    cand.version,
                    short(&self.tip)
                ),
                None if !cand.carry.is_empty() => {
                    let named: Vec<&str> = cand.carry.iter().map(|f| short(&f.commit)).collect();
                    format!(
                        "would carry {} onto the line's head {} in ancestry order, one hop each \
                         (git merge-tree, then git commit-tree), push a merge that keeps main's \
                         tree onto main's tip to record the new head, and put this clone's main \
                         at it",
                        named.join(", "),
                        short(&cand.commit)
                    )
                }
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
        if self.b.head() != cand.commit
            && let Err(r) = self.b.move_to(&cand.commit)
        {
            self.refuse(r)?;
        }
        let same = self
            .journal
            .as_ref()
            .is_some_and(|j| j.commit == cand.commit);
        if !same {
            let now = self.b.now();
            let line = carried
                .unwrap_or_else(|| crate::line::Line::start(&cand.version, &cand.commit, now));
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
            });
        } else if !self.claimed()
            && let Some(j) = self.journal.as_mut()
        {
            j.prover = prover.bin.display().to_string();
            j.trustc = prover.commit.clone();
            if self.linux.declared() {
                j.linux = self.linux.clone();
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
        if self.dry {
            return Ok(());
        }
        let r = self.commit_r();
        match self.b.own_cutter(&r) {
            Ok(_) => Ok(()),
            Err(not) => {
                if self.claim.as_ref().is_some_and(|c| c.continued)
                    && self.journal.as_ref().is_some_and(|j| j.commit == r)
                    && self.b.frozen_commit().is_none_or(|commit| commit == r)
                {
                    return self.refuse(Refusal::new(
                        format!(
                            "this cutter was rebuilt at {} and is still not its own ({})",
                            short(&r),
                            not.why
                        ),
                        format!("{} (nothing was claimed)", not.fix),
                    ));
                }
                self.halt_if_interrupted()?;
                self.say(&format!(
                    "release: {} — freezing and executing {}'s own cutter ({RELEASE_COMMAND}, \
                     same arguments)",
                    not.why,
                    short(&r)
                ));
                let prover = self.prover.clone().ok_or(Halt::Said(1))?;
                match self.b.exec_cutter(&r, &prover) {
                    // A real backend cannot reach this edge: exec replaced it. The model uses
                    // it to represent the old image producing no final line of its own.
                    Ok(()) => Err(Halt::Said(0)),
                    Err(refusal) => self.refuse(refusal),
                }
            }
        }
    }

    fn refresh(&mut self) {
        let Some(c) = self.cand.clone() else {
            return;
        };
        if self.dry && (c.bump_from.is_some() || !c.carry.is_empty()) {
            let pending = if c.bump_from.is_some() {
                "the bump commit is not made yet"
            } else {
                "the line's new head is not made yet"
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

    /// What step 4's graph runs, by the records that stand, and what it skips, in words: the
    /// export unless R is staged (and always under `--qualify`, which checks it without a push),
    /// the build unless its bundle is built and its handoff fixtures staged, the tree half unless
    /// every tree check's record stands.
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
        (plan, done)
    }

    /// Why the plan counts step 4 done: the cut is claimed, or R's preflight passed and every
    /// branch's record stands. Unless the cut is claimed, step 4 still runs its preflight: the
    /// FAIL rule, a PREPARED release's second look, the disk.
    fn skip_qualify(&self) -> Option<String> {
        self.claimed_words().or_else(|| {
            let (plan, done) = self.qualify_plan();
            (plan.branches().is_empty() && self.journal_has(COMMIT_GATES)).then(|| {
                format!(
                    "{}; its preflight passed for it (this release's journal)",
                    done.join("; ")
                )
            })
        })
    }

    /// Why step 5 runs nothing: the cut is claimed, or every bundle check's record stands.
    fn skip_bundle(&self) -> Option<String> {
        self.claimed_words().or_else(|| {
            let checks = &self.facts.checks;
            checks
                .run(Half::Bundle)
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
                "release: {} runs at once: {} (about {})",
                Step::Qualify.at(),
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
    /// candidate's checks are not all recorded: step 4 takes it before its gates and its graph
    /// (the export, the build and the tree half of `verify release`), and a bundle half that runs
    /// without step 4 takes it before it runs. It ends as step 5 does ([`Run::let_disk_go`]). It
    /// is as much as
    /// the most any of this machine's recorded releases wrote, and never less than
    /// [`crate::release_disk::WARM`] where the clone's build is warm, or
    /// [`crate::release_disk::COLD`]. Nothing is walked to size it.
    fn disk_still_needed(&mut self, covers: &str) -> Flow {
        if self.disk_held || self.facts.checks.complete() {
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

    /// Step 4: R's preflight, then its reservation where the build needs one, then the qualify
    /// graph — the tree half of `verify release`, the bundle's build and the export check, at once
    /// ([`run_graph`]) — and the verdict of each branch it ran. A red refuses R at once, every
    /// other branch stopped (an export whose staging push began finishes it); a COULD NOT RUN lets
    /// the others finish, so their records survive for the next run. A peer's cut that reaches the
    /// reservation while the graph runs sets its bundle aside: one more round reserves anew and
    /// builds again, the tree's records and the staging kept.
    fn qualify(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        let mut words = vec![self.commit_gates(&c)?];
        let (plan, _) = self.qualify_plan();
        if plan.export.is_some() {
            words.extend(self.line_holds(&c)?);
        }
        if self.dry {
            if self.export_alone(&plan) {
                self.stage_claimed(|_| ())?;
            }
            words.push(self.qualify_dry(&c, &plan));
            self.finish(&words.join("; "));
            return Ok(());
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

    /// One run of the qualify graph over what still needs it, under the release's disk claim
    /// where it builds or runs the tree, else (the export alone) under a stage's own; its words.
    fn qualify_round(&mut self, c: &Candidate, p: &Prover) -> Result<Vec<String>, Halt> {
        let (plan, _) = self.qualify_plan();
        let reserved = match plan.build {
            Some(_) => {
                self.disk_still_needed("the qualify graph and the bundle half")?;
                Some(self.reserved_for_build(c)?)
            }
            None => self.reservation(),
        };
        // A new reservation sets the old bundle aside, so the plan is asked again.
        let (plan, done) = self.qualify_plan();
        if plan.branches().is_empty() {
            return Ok(done);
        }
        let graph = if self.export_alone(&plan) {
            self.stage_claimed(|run| run.qualify_graph(c, p, reserved.as_ref(), &plan))?
        } else {
            self.qualify_graph(c, p, reserved.as_ref(), &plan)
        };
        self.qualified(c, graph?, done)
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
        let pool = match self.b.qualify_begin(p, plan.export.is_some()) {
            Ok(pool) => pool,
            Err(refusal) => return Err(self.halt(refusal)),
        };
        self.say(&format!("        pool: {pool}"));
        let graph = {
            let br = self.b.branches();
            for branch in plan.branches() {
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
    /// with no bundle and no fixtures staged under it.
    fn reserved_for_build(&mut self, c: &Candidate) -> Result<Reservation, Halt> {
        if let Some(r) = self.reservation()
            && self.b.origin_tail().is_some_and(|tail| r.build > tail)
        {
            return Ok(r);
        }
        if let Some(spent) = self.spent() {
            self.say(&format!("        {} — reserving anew", spent.words()));
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

    /// A real run's refusal as the halt it is.
    fn halt(&mut self, refusal: Refusal) -> Halt {
        match self.refuse(refusal) {
            Err(halt) => halt,
            Ok(()) => Halt::Said(1),
        }
    }

    /// What a dry run's step 4 would run, branch by branch.
    fn qualify_dry(&mut self, c: &Candidate, plan: &Plan) -> String {
        let mut said = Vec::new();
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
            } else if !c.carry.is_empty() {
                "<the line's new head>"
            } else {
                c.commit.as_str()
            };
            said.push(format!(
                "the export — pub stage aterm --commit {commit} --repo-path <this clone>{}",
                if target == StageTarget::Local {
                    " --dry-run, without a staging push"
                } else {
                    ""
                }
            ));
        }
        if said.is_empty() {
            return "would run nothing: every branch's record stands".to_string();
        }
        format!(
            "would run at once, drawing from one pool of this Mac's cores: {}; a red stops the \
             rest",
            said.join("; ")
        )
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
            Some(Ok(ran)) if self.opts.qualify => {
                if ran.code == 0 {
                    self.exported_locally = true;
                    words.push(
                        "the full export qualified locally; no staging snapshot pushed".into(),
                    );
                } else {
                    refused.insert(
                        Branch::Export,
                        Refusal::new(
                            format!("private export qualification failed (exit {})", ran.code),
                            "read its log and fix the candidate before qualifying it again",
                        )
                        .detail(ran.detail),
                    );
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
            if red.is_some() {
                refusal.could_not_run = false;
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
            ),
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

    /// The meaning check before the export: [`Backend::verify_line`] on the journaled line when
    /// it has hops, its summary or its refusal. A dry run with fixes still to carry checks the
    /// line it has and says the new head is checked before its export too.
    fn line_holds(&mut self, c: &Candidate) -> Result<Option<String>, Halt> {
        let line = self
            .journal
            .as_ref()
            .map(Journal::line)
            .filter(|line| !line.hops.is_empty());
        let mut said = Vec::new();
        if let Some(line) = line {
            match self.b.verify_line(&line) {
                Ok(summary) => said.push(summary),
                Err(refusal) => self.refuse(refusal)?,
            }
        }
        if self.dry && !c.carry.is_empty() {
            said.push("would check the line's new head with line::verify before its export".into());
        }
        Ok((!said.is_empty()).then(|| said.join("; ")))
    }

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
            let mut words = String::from(
                "the preflight passes: R's commit-only gates (the changelog, the predecessor's \
                 handoff fixtures, the handoff policy, the locked metadata) and the engine's \
                 readiness",
            );
            if !c.carry.is_empty() {
                words.push_str(&format!(
                    ", asked of the line's head {} before the carry (a real run asks them of the \
                     new head step 3 makes)",
                    short(&c.commit)
                ));
            }
            return Ok(words);
        }
        let n = found.len();
        self.refuse(blocked(
            format!("the preflight BLOCKED ({n} finding(s))"),
            &found,
            &c.commit,
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
            let kind = if self.opts.qualify {
                crate::publish::CutKind::DryRun
            } else {
                crate::publish::CutKind::Real
            };
            out.extend(self.b.cut_gates(c, &creds, kind));
            // With fixes still to carry, R is the line's old head, which the cut never releases:
            // the head step 3 makes meets these in step 4's preflight and before the publish, and
            // judging the old one would refuse the very fix that repairs it.
            if c.carry.is_empty() {
                out.extend(self.b.head_gates(c));
            }
        }
        out
    }

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
                &c.commit,
            ))?;
        }
        if !unasked.is_empty() {
            let mut r = blocked(
                format!(
                    "{asked}what the cut would refuse could not be asked ({} question(s))",
                    unasked.len()
                ),
                &unasked,
                &c.commit,
            );
            r.remedy = format!("run {RELEASE_COMMAND} again once it answers (nothing changed)");
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

    /// Step 5: the bundle half of `verify release` on the bundle step 4 built — appkit and
    /// measure, both timed, so never beside the release's own load — then its records' verdict.
    fn bundle(&mut self) -> Flow {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(());
        };
        self.fail_rule()?;
        let run = self.facts.checks.run(Half::Bundle);
        let words = self.facts.checks.line().to_owned();
        if self.dry {
            self.finish(&format!(
                "would run `{}` in the clone ({words})",
                half_command(&run, Some("<the bundle step 4 built>"))
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
        let ran = self
            .b
            .branches()
            .verify(&c, &p, &run, Some(&artifact), &BranchStop::default());
        self.refresh();
        let ran = match ran {
            Ok(ran) => ran,
            Err(refusal) => return self.refuse(refusal),
        };
        if self.facts.checks.complete() {
            let words = self.facts.checks.line().to_owned();
            self.finish(&words);
            return Ok(());
        }
        if let Some(refusal) = self.facts.checks.fail_refusal(&c.commit) {
            return self.refuse(refusal.detail(ran.detail));
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
        vec![
            "cut".to_string(),
            "--sign".to_string(),
            "--release-credentials".to_string(),
            self.creds.display().to_string(),
        ]
    }

    /// Step 7: R's source published by `pub publish` and R's verified bundle signed, notarized
    /// and packaged privately by `cut --sign`, at once ([`run_sides`]); neither claims, and the
    /// cut claims once both passed. `published` says why the source is skipped (published
    /// earlier, no build claimed): the sign then runs alone, and adopts the copy an earlier run
    /// signed of the same bytes. Everything the cut would refuse is asked again before either
    /// side starts, and the engine readied for the source; the sign reads only the release's
    /// own journal.
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
                    "{why}; {line}; would ask again what the cut would refuse, then run: {then}"
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
        } else if let Err(r) = self.b.source_publication(&c) {
            return self.refuse(r);
        }
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
                        Some(why) => format!("        source: skipped — {why}"),
                    });
                    br.say(&format!(
                        "        sign: targo --unverified ship {}; log: {}",
                        sign.join(" "),
                        br.side_log(Side::Sign).display()
                    ));
                    Ok(run_sides(&*br, &p, source, &sign))
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
        let n = self.reservation().map_or(0, |r| r.build);
        let r = short(&c.commit).to_string();
        let rev = self.facts.engine.rev.clone().unwrap_or_default();
        self.finish(&format!(
            "{} {r} -> alabsystems {}; signed build {n} privately",
            if source {
                "published"
            } else {
                "published earlier:"
            },
            short(&rev)
        ));
        Ok(())
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
                        "finish what its last line names (e.g. pub sign-release aterm), then \
                         {RELEASE_COMMAND}"
                    ),
                )
                .detail(ran.detail),
            );
        }
        if let Err(r) = self.b.source_publication(c) {
            return self.refuse(r);
        }
        Ok(())
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
        let ran = self.b.cut(&args, &p);
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
            let left = self.b.discard_signed(&self.version(), build);
            let discarded = if left.is_empty() {
                "was discarded".to_string()
            } else {
                format!("was discarded but for {}", left.join("; "))
            };
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
            match self.b.after_cut(&v) {
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
        }
        if !self.facts.fixtures.yes {
            notes.push(self.land_fixtures()?);
        }
        self.finish(&notes.join("; "));
        Ok(())
    }

    fn land_fixtures(&mut self) -> Result<String, Halt> {
        let (Some(c), Some(p)) = (self.cand.clone(), self.prover.clone()) else {
            return Ok(String::new());
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

/// The way out of a red on the line whose head is `head`: the fix lands on main as its own
/// commit, and `--fix` carries it onto the line as a hop, so the release never re-selects main.
pub(crate) fn fix_remedy(head: &str) -> String {
    format!(
        "land the fix on main (based on {} when it must apply to the line's head), then \
         {RELEASE_COMMAND} --fix <the fix's own non-merge commit>",
        short(head)
    )
}

fn blocked(why: String, found: &[Refusal], head: &str) -> Refusal {
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
            "fix each (each block's fix line above) — the source's: {}; this machine's: fix \
             it, then {RELEASE_COMMAND}",
            fix_remedy(head)
        ),
    )
    .detail(detail)
}

/// What step 4's graph runs ([`Run::qualify_plan`]): each branch's work, `None` where its
/// record stands.
#[derive(Debug, Clone, Default)]
struct Plan {
    export: Option<StageTarget>,
    build: Option<BuildWork>,
    tree: Option<VerifyRun>,
}

impl Plan {
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

/// Run step 4's graph on `br`: the build first; the tree once both slices hold memory
/// ([`Branches::build_admitted`]), then the export, each in a thread of its
/// own; the ledger's memory claims decide what runs together, and every branch draws from the
/// one pool [`Backend::qualify_begin`] made. A branch's red stops the other branches at once,
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
    std::thread::scope(|s| {
        let (tx, rx) = std::sync::mpsc::channel::<Note>();
        let (settled, stops) = (&settled, &stops);
        let starter = tx.clone();
        s.spawn(move || {
            let stop_of = |b: Branch| &stops[b as usize];
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
                }
            }
            if let Some(run) = &plan.tree {
                start_branch(
                    s,
                    br,
                    &starter,
                    Branch::Tree,
                    stop_of(Branch::Tree),
                    move |stop| Note::Tree(br.verify(c, p, run, None, stop)),
                );
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
                if branch == Branch::Build || ended == Ended::Red {
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
fn run_sides(br: &dyn Branches, p: &Prover, source: bool, sign: &[String]) -> Sides {
    let first = BranchStop::default();
    let stops: [BranchStop; 2] = [first.beside(), first.beside()];
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

/// `MAJOR.MINOR.0` → (MAJOR, MINOR).
#[must_use]
pub fn public_version(v: &str) -> Option<(u64, u64)> {
    let mut parts = v.split('.');
    let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || patch != "0" {
        return None;
    }
    let number = |s: &str| -> Option<u64> {
        (!s.is_empty()
            && (s == "0" || !s.starts_with('0'))
            && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse::<u64>().ok())
        .flatten()
    };
    Some((number(major)?, number(minor)?))
}

fn newer(a: &str, b: &str) -> bool {
    let key = |v: &str| {
        let mut p = v.split('.').map(|x| {
            (!x.starts_with('+'))
                .then(|| x.parse::<u64>().ok())
                .flatten()
                .unwrap_or(0)
        });
        (p.next().unwrap_or(0), p.next().unwrap_or(0))
    };
    key(a) > key(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step 10 distinguishes an unanswered freshness read from an answered channel verdict.
    /// NEGATIVE CONTROLS: a response with no tag and one with the wrong tag remain ordinary
    /// refusals, while the expected tag has no refusal.
    #[test]
    fn latest_transport_is_not_a_channel_refusal() {
        let transport = "curl asset request failed with HTTP 504";
        let unavailable = latest_refusal("0.175.0", Err(transport.into())).unwrap();
        assert!(unavailable.could_not_run);
        assert!(
            unavailable
                .detail
                .iter()
                .any(|line| line.contains(transport)),
            "the transport's own detail is retained"
        );

        for answered in [None, Some("v0.174.0".to_string())] {
            let semantic = latest_refusal("0.175.0", Ok(answered.clone())).unwrap();
            assert!(
                !semantic.could_not_run,
                "an answered channel with {answered:?} made a semantic decision"
            );
        }
        assert!(latest_refusal("0.175.0", Ok(Some("v0.175.0".into()))).is_none());
    }

    /// An exec'd image continues the live claim of its exact pid and start, atomically replacing
    /// its logs and retaining the preceding image's different logs. The continued image's end
    /// removes the claim. NEGATIVE CONTROLS: changing either half of the process identity refuses
    /// the continuation, and appending the new body instead of replacing it leaves the old logs
    /// as the first `logs` field [`read_holder`] reads.
    #[test]
    fn an_execed_image_continues_only_its_exact_process_claim() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join(RUNNING_FILE);
        let me = (4242, "Thu Oct  9 02:00:00 2026");
        let reused_pid = (me.0, "Thu Oct  9 02:01:00 2026");
        let another_pid = (4343, me.1);
        let mut started_of = |pid: u32| {
            [me, another_pid]
                .into_iter()
                .find(|(p, _)| *p == pid)
                .map(|(_, at)| at.to_string())
        };
        let logs = Path::new("/l");
        let claim = RunClaim::take(&path, me, logs, &mut started_of).unwrap();
        assert!(claim.names_this_run());
        drop(claim);
        assert!(!path.exists(), "a claim nobody took is removed");

        let claim = RunClaim::take(&path, me, logs, &mut started_of).unwrap();
        assert!(!claim.continued && claim.previous_logs.is_none());
        for identity in [reused_pid, another_pid] {
            let refused = RunClaim::take(&path, identity, Path::new("/wrong"), &mut started_of);
            assert!(
                matches!(&refused, Err(Ok(Holder { pid, .. })) if *pid == me.0),
                "a different process identity must not continue this claim: {refused:?}"
            );
        }
        let continued_logs = Path::new("/l2");
        let continued = RunClaim::take(&path, me, continued_logs, &mut started_of).unwrap();
        assert!(continued.continued && continued.names_this_run());
        assert_eq!(
            continued.previous_logs.as_deref(),
            Some(logs),
            "the preceding image's logs"
        );
        assert_eq!(
            read_holder(&path).map(|h| h.logs),
            Some(continued_logs.display().to_string()),
            "the atomic replacement leaves one complete current body"
        );
        std::mem::forget(claim); // a successful exec does not run the preceding image's Drop
        drop(continued);
        assert!(!path.exists(), "the continued image's end removes it");
    }

    /// A cutter already running when B1 lands may have written the old `handed` marker before it
    /// starts R's newly built cutter as a child. That one live ancestor handoff is adopted and
    /// rewritten without the marker; no new code writes it. NEGATIVE CONTROLS: ancestry without
    /// the marker, and the marker on a live non-ancestor, are both ordinary competing runs.
    #[test]
    fn a_pre_b1_handoff_is_adopted_once_only_from_its_live_ancestor() {
        use std::io::Write as _;

        let parent = (4242, "Thu Oct  9 02:00:00 2026");
        let child = (4343, "Thu Oct  9 02:00:01 2026");
        let mut started_of = |pid: u32| {
            [parent, child]
                .into_iter()
                .find(|(found, _)| *found == pid)
                .map(|(_, started)| started.to_string())
        };

        for (name, mark_handed, ancestors, adopted) in [
            ("authentic", true, vec![parent.0], true),
            ("unmarked", false, vec![parent.0], false),
            ("unrelated", true, vec![9999], false),
        ] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let path = dir.path().join(RUNNING_FILE);
            let old_logs = Path::new("/old/logs");
            let old =
                RunClaim::take_locking(&path, parent, old_logs, &mut started_of, &[], &claim_lock)
                    .unwrap();
            if mark_handed {
                let mut file = crate::durable::open_append(&path, false).unwrap();
                file.write_all(b"handed\n").unwrap();
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the legacy-handoff fixture must make the old cutter's marker durable before adoption"
                )]
                file.sync_all().unwrap();
            }

            let answer = RunClaim::take_locking(
                &path,
                child,
                Path::new("/new/logs"),
                &mut started_of,
                &ancestors,
                &claim_lock,
            );
            if adopted {
                let claim = answer.unwrap();
                assert!(claim.continued && claim.legacy_handoff, "{name}");
                assert_eq!(claim.previous_logs.as_deref(), Some(old_logs), "{name}");
                assert!(claim.names_this_run(), "{name}");
                assert!(!read_holder(&path).unwrap().legacy_handed, "{name}");
                drop(claim);
                assert!(!path.exists(), "{name}: the adopted owner removes it");
            } else {
                assert!(
                    matches!(answer, Err(Ok(Holder { pid, .. })) if pid == parent.0),
                    "{name}: only the authenticated legacy handoff may be adopted"
                );
                assert!(old.names_this_run(), "{name}: the parent retains its claim");
            }
            drop(old);
        }
    }

    /// A continuation never lands between this image's read of its claim and the removal that
    /// read decided: the take, made at that moment without waiting for the claim's lock, finds it
    /// held and writes nothing; the removal goes on, and a later take claims anew. The
    /// interleaving is the test's: `remove_if_mine` (RunClaim's drop) runs the take between its
    /// read and its removal. NEGATIVE CONTROL: without the common lock, the continuation lands
    /// and the removal then deletes its rewritten claim.
    #[test]
    fn a_continuation_never_lands_between_a_claims_read_and_removal() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join(RUNNING_FILE);
        let me = (4242, "Thu Oct  9 02:00:00 2026");
        let mut started_of = |pid: u32| (pid == me.0).then(|| me.1.to_string());
        let logs = Path::new("/l");
        let at_once = |path: &Path| {
            aterm_update_core::FileLock::acquire_within(
                &path.with_extension("lock"),
                std::time::Duration::ZERO,
            )
        };
        let claim = RunClaim::take(&path, me, logs, &mut started_of).unwrap();
        let mut between = None;
        claim.remove_if_mine(|| {
            between = Some(RunClaim::take_locking(
                &path,
                me,
                Path::new("/l2"),
                &mut started_of,
                &[],
                &at_once,
            ));
        });
        let between = between.expect("the claim named this run when it was read");
        assert!(
            matches!(&between, Err(Err(why)) if why.starts_with("lock ")),
            "the take did not land between the read and the removal: {between:?}"
        );
        assert!(
            !path.exists(),
            "the claim this run read as its own is removed"
        );
        drop(claim);
        let took = RunClaim::take(&path, me, logs, &mut started_of).unwrap();
        assert!(
            !took.continued && took.names_this_run(),
            "the process claims anew after the removal"
        );
    }

    #[test]
    fn estimates_takes_upper_median_of_last_five_and_formats_durations() {
        let tsv = "\
1\t0.1.0\teven\t10\n\
2\t0.1.0\teven\t40\n\
malformed\trow\n\
3\t0.1.0\teven\tnan\n\
3\t0.1.0\teven\t+99\n\
4\t0.1.0\twindow\t1000\n\
5\t0.1.0\twindow\t5\n\
6\t0.1.0\twindow\t30\n\
7\t0.1.0\twindow\t10\n\
8\t0.1.0\twindow\t20\n\
9\t0.1.0\twindow\t40\n";
        let got = estimates(tsv);
        assert_eq!(
            got.get("even"),
            Some(&40),
            "sorted[2 / 2] is the upper median"
        );
        assert_eq!(
            got.get("window"),
            Some(&20),
            "only the last five runs (5, 30, 10, 20, 40) participate"
        );
        assert_eq!(got.len(), 2);

        assert_eq!(fmt_dur(0), "0s");
        assert_eq!(fmt_dur(59), "59s");
        assert_eq!(fmt_dur(60), "1m00s");
        assert_eq!(fmt_dur(185), "3m05s");
        assert_eq!(fmt_dur(3600), "1h00m");
        assert_eq!(fmt_dur(9480), "2h38m");

        assert_eq!(public_version("0.1.0"), Some((0, 1)));
        assert_eq!(public_version("00.1.0"), None);
        assert_eq!(public_version("0.01.0"), None);
        assert!(!newer("+2.0", "1.0"));
        assert!(!newer("1.+2", "1.1"));
    }
}

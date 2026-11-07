// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One record per release check, keyed by content; a FAIL is never re-rolled (A19).
//!
//! `verify release` ends each check with one `check-finished` line ([`aterm_verify::report`])
//! and each test binary with one `test-binary-finished` line, and writes each whole to the run's
//! own events file (`--events`), which only verify's own process writes: no child's output and no
//! other writer's bytes land inside a line there, as they can in its output. Every record of one
//! tree under one seal lives in one directory, `<git common dir>/aterm-release/verified/<tree>/<S>/`,
//! S the seal's digest ([`seal_dir`]):
//! - a tree check's at `<check>.toml`, keyed by H(check, tree, seal);
//! - the bundle check's, `measure` (the one check that reads `--app`), at `measure/<U>.toml`,
//!   keyed by H(check, tree, seal, U), where U is the executable's SHA-256. The tree stays in its
//!   key until verify can report the SHA-256 of the harness executables measure runs;
//! - the run-level record, `verify.toml`: `began` from before a run starts ([`begin_run`]) until
//!   every FAIL the run showed is kept, and FAIL when the run exited 1 and no check's FAIL was
//!   read. A `began` left behind (the cutter died mid-run, or what the run said could not be read
//!   whole) reads as a FAIL, so the FAIL rule never rests on a write that did not happen.
//!
//! Once a run's clone was still at R's tree and clean, and its build-inputs seal rechecked, the
//! cutter keeps one record per check from the events ([`write_run`]); a run that no longer judged
//! R's tree under that seal keeps nothing ([`abandon_run`]).
//!
//! The judge ([`checks`]) reads them back: an `ok` (or an owner's `accepted` measure) passes, a
//! `FAIL` refuses the tree under that seal until a new hop changes the tree, and `NOT RUN`,
//! `skip` and an absent record are missing, which a resume runs again, alone, carrying the test
//! binaries a test record lists as passed with their executables' digests ([`VerifyRun`]). A
//! record that does not parse, or whose key is not this tree's and seal's, is missing; one that
//! cannot be read leaves the judge without an answer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aterm_verify::checks::{Check, RELEASE};

use crate::ledger::GitRunner;
use crate::release::{Answer, Refusal};

/// Where the records live, in the release directory.
pub const DIR: &str = "verified";

/// The run-level record's name: a `verify release` that began and has not settled, or that
/// exited 1 (FAIL) with no check's FAIL read from its events, still refuses its tree, so the FAIL
/// rule never rests on parsing or on a write.
pub const RUN_LEVEL: &str = "verify";

const SCHEMA: u32 = 1;
const KEY_DOMAIN: &str = "aterm-release-check-v1";
const SEAL_DOMAIN: &str = "aterm-release-seal-dir-v1";
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

/// A record's outcome words: the words a check's line opens with, and an owner's acceptance.
pub const OK: &str = "ok";
pub const FAIL: &str = "FAIL";
pub const NOT_RUN: &str = "NOT RUN";
pub const SKIP: &str = "skip";
pub const ACCEPTED: &str = "accepted";
/// The run-level record's outcome while a run is under way, and after one that never settled.
pub const BEGAN: &str = "began";

/// Whether `check`'s record answers for the bundle rather than the tree.
fn bundle(check: Check) -> bool {
    check == Check::Measure
}

/// One test binary a test record lists: its spec, its executable's SHA-256 and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Binary {
    pub spec: String,
    pub sha256: String,
    pub outcome: String,
}

impl Binary {
    /// Whether a resume carries it: it passed, and its executable's digest is known. One whose
    /// digest is not would be refused by `--passed-binaries`, and can never be matched, so it runs
    /// again.
    fn carried(&self) -> bool {
        self.outcome == OK && aterm_codec::hex::is_lower(&self.sha256, 64)
    }
}

/// An owner's acceptance of a measure FAIL ([`crate::accepted_measure::record`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Acceptance {
    pub reason: String,
    /// The SHA-256 of the failed run's log the acceptance names.
    pub failed_log_sha256: String,
}

/// One check's record.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CheckRecord {
    pub schema: u32,
    pub check: String,
    /// [`key`] of the check, the tree, the seal and (a bundle check) the executable.
    pub key: String,
    pub tree: String,
    /// The commit whose release ran it.
    pub commit: String,
    /// The run's build-inputs identity ([`crate::build_inputs::current`]).
    pub seal: String,
    /// U, the executable's SHA-256, for a bundle check.
    #[serde(default)]
    pub executable: Option<String>,
    pub outcome: String,
    pub seconds: f64,
    pub detail: String,
    /// The check's stage rows, each `<word> <label>`.
    #[serde(default)]
    pub rows: Vec<String>,
    pub log: String,
    /// The log's SHA-256 when the run ended; empty when it could not be hashed (or, for a
    /// `began` record, the run has not ended).
    pub log_sha256: String,
    /// The release run that wrote it (its logs directory's name).
    pub run: String,
    /// `verify release`'s exit code (-1 for a `began` record).
    pub exit: i32,
    #[serde(default)]
    pub acceptance: Option<Acceptance>,
    #[serde(default)]
    pub binaries: Vec<Binary>,
}

/// H(check, tree, seal[, U]): what a record answers for.
#[must_use]
pub fn key(check: &str, tree: &str, seal: &str, executable: Option<&str>) -> String {
    let mut h = aterm_digest::Sha256::new();
    for part in [KEY_DOMAIN, check, tree, seal]
        .into_iter()
        .chain(executable)
    {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    aterm_codec::hex::encode_lower(&h.finalize())
}

/// S, the directory of a tree's records under `seal`: the seal's SHA-256, so a record under one
/// seal never lies where another seal's would, and none ever replaces another's.
#[must_use]
pub fn seal_dir(seal: &str) -> String {
    let mut h = aterm_digest::Sha256::new();
    h.update(SEAL_DOMAIN.as_bytes());
    h.update(b"\0");
    h.update(seal.as_bytes());
    aterm_codec::hex::encode_lower(&h.finalize())
}

/// `<root>/<tree>/<S>/<check>.toml`, or a bundle check's `<root>/<tree>/<S>/<check>/<U>.toml`:
/// one path per key, so no record of one key ever replaces another key's.
#[must_use]
pub fn path(root: &Path, tree: &str, seal: &str, check: &str, executable: Option<&str>) -> PathBuf {
    path_in_seal_dir(&root.join(tree).join(seal_dir(seal)), check, executable)
}

fn path_in_seal_dir(seal_dir: &Path, check: &str, executable: Option<&str>) -> PathBuf {
    match executable {
        Some(u) => seal_dir.join(check).join(format!("{u}.toml")),
        None => seal_dir.join(format!("{check}.toml")),
    }
}

/// `<release dir>/verified`.
///
/// # Errors
/// git cannot name the common dir.
pub fn root(git: &dyn GitRunner) -> Result<PathBuf, String> {
    Ok(crate::release::release_dir(git)?.join(DIR))
}

/// Whether `root` holds a directory of `tree`'s records. A legacy single record (a FILE at
/// `<root>/<tree>`, from before A19) stands for no check: the next run's records replace it.
fn has_records(root: &Path, tree: &str) -> Result<bool, String> {
    let dir = root.join(tree);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) => Ok(meta.is_dir()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("read {}: {e}", dir.display())),
    }
}

/// A record as read: absent, there but no record this cutter reads, or the record.
enum Read {
    Absent,
    Unusable,
    Record(Box<CheckRecord>),
}

fn read(path: &Path) -> Result<Read, String> {
    let text = match crate::durable::read_regular_text(path, MAX_RECORD_BYTES) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Read::Absent),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    Ok(match aterm_toml::from_str::<CheckRecord>(&text) {
        Ok(record) if record.schema == SCHEMA => Read::Record(Box::new(record)),
        _ => Read::Unusable,
    })
}

/// Whether `record` answers for `check` of `tree` under `seal` (on `executable`): its key is the
/// one this question computes. The key binds every part, so a record of another check, tree,
/// seal or build never answers, wherever it lies; the fields beside it are what the record says
/// to a reader.
fn answers(
    record: &CheckRecord,
    check: &str,
    tree: &str,
    seal: &str,
    executable: Option<&str>,
) -> bool {
    record.key == key(check, tree, seal, executable)
}

/// The records of bundle check `check` in `seal_dir`, one per executable: each U and
/// its record's path, in U order.
fn bundle_records(seal_dir: &Path, check: &str) -> Result<Vec<(String, PathBuf)>, String> {
    let dir = seal_dir.join(check);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", dir.display())),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let name = entry.file_name();
        let u = name
            .to_str()
            .and_then(|n| n.strip_suffix(".toml"))
            .filter(|u| aterm_codec::hex::is_lower(u, 64))
            .map(str::to_owned);
        if let Some(u) = u {
            found.push((u, entry.path()));
        }
    }
    found.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

/// Whether any seal's records of `tree` hold an owner's acceptance of a bundle check, or cannot
/// be read: the seal is taken to ask further only then.
fn holds_acceptance(root: &Path, tree: &str) -> bool {
    let Ok(seals) = std::fs::read_dir(root.join(tree)) else {
        return false;
    };
    for seal in seals {
        let Ok(seal) = seal else { return true };
        for check in RELEASE.into_iter().filter(|c| bundle(*c)) {
            let records = match std::fs::read_dir(seal.path().join(check.name())) {
                Ok(records) => records,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return true,
            };
            for record in records {
                let Ok(record) = record else { return true };
                match read(&record.path()) {
                    Ok(Read::Record(r)) if r.outcome == ACCEPTED => return true,
                    Ok(_) => {}
                    Err(_) => return true,
                }
            }
        }
    }
    false
}

/// A check whose record says FAIL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub check: String,
    pub log: String,
    /// The run-level `began` record of a run that never settled: what it found is not known, so
    /// it reads as a FAIL.
    pub unrecorded: bool,
}

impl Failed {
    /// Its name in the judge's words.
    fn name(&self) -> String {
        if self.unrecorded {
            format!("{} (it began and recorded no outcome)", self.check)
        } else {
            self.check.clone()
        }
    }
}

/// What a resumed `verify release` runs: the checks the records lack, in [`RELEASE`] order, and
/// the test binaries that passed in this tree (each spec with its executable's SHA-256).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyRun {
    pub checks: Vec<String>,
    pub passed_binaries: Vec<(String, String)>,
}

impl VerifyRun {
    /// Whether this is fewer than every check a release requires: its run then names them
    /// (`--checks a,b`).
    #[must_use]
    pub fn strict_subset(&self) -> bool {
        self.checks.len() != RELEASE.len()
            || self
                .checks
                .iter()
                .zip(RELEASE.iter())
                .any(|(a, b)| a != b.name())
    }

    /// Whether it runs a bundle check, the one kind that reads `--app`.
    #[must_use]
    pub fn reads_the_bundle(&self) -> bool {
        RELEASE
            .into_iter()
            .filter(|c| bundle(*c))
            .any(|c| self.checks.iter().any(|name| name == c.name()))
    }
}

/// [`RELEASE`]'s names, in its order.
#[must_use]
pub fn release_names() -> Vec<String> {
    RELEASE.iter().map(|c| c.name().to_owned()).collect()
}

/// `n` binaries, in words.
fn binaries(n: usize) -> String {
    if n == 1 {
        "1 binary".to_owned()
    } else {
        format!("{n} binaries")
    }
}

/// The judge's answer over one tree's records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checks {
    answer: Answer,
    pub tree: String,
    /// The checks with no passing record, in [`RELEASE`] order.
    pub missing: Vec<String>,
    /// The checks whose record says FAIL, the run-level record's included.
    pub failed: Vec<Failed>,
    /// An owner's acceptance the measure record holds, in words.
    pub accepted: Option<String>,
    /// The test binaries the test record lists as passed, with known digests, when test did not
    /// pass.
    pub resume: Vec<(String, String)>,
}

/// One check's state, as its record left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Passed,
    Accepted {
        reason: String,
        log: String,
    },
    Failed {
        log: String,
    },
    /// No passing record (NOT RUN, skip, absent, unusable), and the test binaries it lists as
    /// passed.
    Missing {
        resume: Vec<(String, String)>,
    },
}

impl Checks {
    /// No answer: the records (or the seal) cannot be read; nothing is known.
    #[must_use]
    pub fn unanswered(words: impl Into<String>) -> Self {
        Self {
            answer: Answer::unanswered(words),
            missing: release_names(),
            ..Self::default()
        }
    }

    /// No record stands, for `why` (the bump commit not made yet, a bundle that can never ship).
    #[must_use]
    pub fn no(words: impl Into<String>) -> Self {
        Self {
            answer: Answer::no(words),
            missing: release_names(),
            ..Self::default()
        }
    }

    /// These records with the bundle's set aside: its bytes can never ship (`why`), so the
    /// bundle checks are missing again, the tree's records kept.
    #[must_use]
    pub fn bundle_stale(mut self, words: impl Into<String>) -> Self {
        let missing: Vec<String> = RELEASE
            .iter()
            .filter(|c| bundle(**c) || self.missing.iter().any(|m| m == c.name()))
            .map(|c| c.name().to_owned())
            .collect();
        self.missing = missing;
        self.accepted = None;
        self.answer = Answer::no(words);
        self
    }

    /// Every check's record from `state`, `also` the FAILs no check's own record holds (the
    /// run-level record, a measure FAIL of another build of this tree): the answer for
    /// `commit`'s `tree` on `executable`.
    #[must_use]
    pub fn of(
        commit: &str,
        tree: &str,
        executable: Option<&str>,
        mut state: impl FnMut(Check) -> State,
        also: Vec<Failed>,
    ) -> Self {
        let mut checks = Self {
            tree: tree.to_owned(),
            ..Self::default()
        };
        let mut accepted = None;
        for check in RELEASE {
            match state(check) {
                State::Passed => {}
                State::Accepted { reason, log } => accepted = Some((reason, log)),
                State::Failed { log } => checks.failed.push(Failed {
                    check: check.name().to_owned(),
                    log,
                    unrecorded: false,
                }),
                State::Missing { resume } => {
                    if check == Check::Test {
                        checks.resume = resume;
                    }
                    checks.missing.push(check.name().to_owned());
                }
            }
        }
        for f in also {
            if !checks.failed.iter().any(|g| g.check == f.check) {
                checks.failed.push(f);
            }
        }
        let at = format!("{}'s tree {}", short(commit), short(tree));
        let words = if let Some(first) = checks.failed.first() {
            let names: Vec<String> = checks.failed.iter().map(Failed::name).collect();
            format!(
                "`verify release` FAILED {} for {at} under this seal (log {}) — a FAIL is never \
                 re-rolled",
                names.join(", "),
                first.log
            )
        } else if !checks.missing.is_empty() {
            let ok = RELEASE.len() - checks.missing.len();
            let mut words = format!(
                "`verify release` has no passing record of {} for {at} under this seal ({ok} of \
                 {} checks recorded ok)",
                checks.missing.join(", "),
                RELEASE.len()
            );
            if executable.is_none() && checks.missing.iter().any(|m| m == "measure") {
                words.push_str("; measure judges the bundle, not built yet (step 5)");
            }
            if !checks.resume.is_empty() {
                use std::fmt::Write as _;
                let _ = write!(
                    words,
                    "; test's record carries {} that passed",
                    binaries(checks.resume.len())
                );
            }
            words
        } else if let Some((reason, log)) = &accepted {
            format!(
                "OWNER ACCEPTED measure failure: {reason}; all other release checks passed; \
                 report {log} (not a full verification pass)"
            )
        } else {
            format!(
                "verify release: PASSED for {at} on the executable {}: every check's record is ok \
                 under this seal",
                executable.map_or("?", short)
            )
        };
        checks.accepted = accepted
            .filter(|_| checks.failed.is_empty() && checks.missing.is_empty())
            .map(|_| words.clone());
        checks.answer = if checks.failed.is_empty() && checks.missing.is_empty() {
            Answer::yes(words)
        } else {
            Answer::no(words)
        };
        checks
    }

    /// Did every check a release requires pass, on these bytes, under this seal?
    #[must_use]
    pub fn complete(&self) -> bool {
        self.answer.yes
    }

    /// The judge's answer, in its words.
    #[must_use]
    pub const fn answer(&self) -> &Answer {
        &self.answer
    }

    /// The answer's words.
    #[must_use]
    pub fn line(&self) -> &str {
        &self.answer.words
    }

    /// What a resume runs: the missing checks, with the test binaries that passed when test is
    /// among them.
    #[must_use]
    pub fn run(&self) -> VerifyRun {
        let test = self.missing.iter().any(|m| m == "test");
        VerifyRun {
            checks: self.missing.clone(),
            passed_binaries: if test {
                self.resume.clone()
            } else {
                Vec::new()
            },
        }
    }

    /// The FAIL rule: a FAIL record refuses R's tree under this seal, at once, exit 1; its remedy
    /// carries a landed fix onto the line whose head is `head` ([`crate::release::fix_remedy`]).
    #[must_use]
    pub fn fail_refusal(&self, head: &str) -> Option<Refusal> {
        let first = self.failed.first()?;
        let names: Vec<String> = self.failed.iter().map(Failed::name).collect();
        Some(
            Refusal::new(
                format!(
                    "R's tree {} FAILED {} under this seal (log {}) — a FAIL is never re-rolled",
                    short(&self.tree),
                    names.join(", "),
                    first.log
                ),
                format!(
                    "{}: a new hop changes the tree, and a FAIL refuses only its own",
                    crate::release::fix_remedy(head)
                ),
            )
            .detail(self.failed.iter().map(|f| {
                if f.unrecorded {
                    format!(
                        "{} began under this seal and recorded no outcome, so it reads as \
                         FAILED: {}",
                        f.check, f.log
                    )
                } else {
                    format!("{} FAILED: {}", f.check, f.log)
                }
            })),
        )
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// The tree of `commit`.
fn tree_of(git: &dyn GitRunner, commit: &str) -> Result<String, String> {
    crate::ledger::rev_parse(git, &format!("{commit}^{{tree}}")).map_err(|e| e.to_string())
}

/// The judge over `commit`'s tree on `executable` (None: no bundle built yet) under the seal
/// `seal` names, read only once the tree has a directory of records.
///
/// # Errors
/// git cannot name the tree or the common dir, a record cannot be read, or the seal cannot be
/// taken: no answer either way.
pub fn checks(
    git: &dyn GitRunner,
    commit: &str,
    executable: Option<&str>,
    seal: impl FnOnce() -> Result<String, String>,
) -> Result<Checks, String> {
    let tree = tree_of(git, commit)?;
    checks_in(&root(git)?, commit, &tree, executable, seal)
}

/// [`checks`] over the records under `root`.
///
/// # Errors
/// As [`checks`].
pub fn checks_in(
    root: &Path,
    commit: &str,
    tree: &str,
    executable: Option<&str>,
    seal: impl FnOnce() -> Result<String, String>,
) -> Result<Checks, String> {
    let missing = |_: Check| State::Missing { resume: Vec::new() };
    if !has_records(root, tree)? {
        return Ok(Checks::of(commit, tree, executable, missing, Vec::new()));
    }
    let seal = seal()?;
    let seal_path = root.join(tree).join(seal_dir(&seal));
    let mut states: BTreeMap<&'static str, State> = BTreeMap::new();
    for check in RELEASE {
        let u = bundle(check).then_some(executable).flatten();
        if bundle(check) && u.is_none() {
            continue;
        }
        let Read::Record(record) = read(&path_in_seal_dir(&seal_path, check.name(), u))? else {
            continue;
        };
        if !answers(&record, check.name(), tree, &seal, u) {
            continue;
        }
        let state = match record.outcome.as_str() {
            OK => State::Passed,
            ACCEPTED if bundle(check) => match record.acceptance {
                Some(a) => State::Accepted {
                    reason: a.reason,
                    log: record.log,
                },
                None => continue,
            },
            FAIL => State::Failed { log: record.log },
            _ => State::Missing {
                resume: record
                    .binaries
                    .into_iter()
                    .filter(Binary::carried)
                    .map(|b| (b.spec, b.sha256))
                    .collect(),
            },
        };
        states.insert(check.name(), state);
    }
    let mut also = Vec::new();
    // The run-level record refuses the tree when no check's own record does, and names it then.
    if !states.values().any(|s| matches!(s, State::Failed { .. }))
        && let Read::Record(record) = read(&path_in_seal_dir(&seal_path, RUN_LEVEL, None))?
        && answers(&record, RUN_LEVEL, tree, &seal, None)
        && [FAIL, BEGAN].contains(&record.outcome.as_str())
    {
        let unrecorded = record.outcome == BEGAN;
        also.push(Failed {
            check: RUN_LEVEL.to_owned(),
            log: record.log,
            unrecorded,
        });
    }
    // A FAIL of a bundle check on another build of this tree under this seal refuses it too: a
    // rebuilt U (a spent reservation built again) never re-rolls it.
    for check in RELEASE.into_iter().filter(|c| bundle(*c)) {
        for (u, at) in bundle_records(&seal_path, check.name())? {
            if Some(u.as_str()) == executable {
                continue;
            }
            if let Read::Record(record) = read(&at)?
                && record.outcome == FAIL
                && answers(&record, check.name(), tree, &seal, Some(&u))
            {
                also.push(Failed {
                    check: check.name().to_owned(),
                    log: record.log,
                    unrecorded: false,
                });
            }
        }
    }
    Ok(Checks::of(
        commit,
        tree,
        executable,
        |check| {
            states
                .remove(check.name())
                .unwrap_or(State::Missing { resume: Vec::new() })
        },
        also,
    ))
}

/// The owner's acceptance of `commit`'s measure FAIL on `executable`, in words, when one stands
/// under the seal `seal` names (taken only once an acceptance is there).
///
/// # Errors
/// git, a record that cannot be read, or the seal.
pub fn accepted(
    git: &dyn GitRunner,
    commit: &str,
    executable: &str,
    seal: impl FnOnce() -> Result<String, String>,
) -> Result<Option<String>, String> {
    let tree = tree_of(git, commit)?;
    let root = root(git)?;
    if !has_records(&root, &tree)? || !holds_acceptance(&root, &tree) {
        return Ok(None);
    }
    let seal = seal()?;
    let Read::Record(record) = read(&path(&root, &tree, &seal, "measure", Some(executable)))?
    else {
        return Ok(None);
    };
    let Some(acceptance) = record
        .acceptance
        .as_ref()
        .filter(|_| record.outcome == ACCEPTED)
    else {
        return Ok(None);
    };
    Ok(
        answers(&record, "measure", &tree, &seal, Some(executable)).then(|| {
            format!(
                "OWNER ACCEPTED measure failure: {}; report {} (not a full verification pass)",
                acceptance.reason, record.log
            )
        }),
    )
}

/// Rewrite `commit`'s measure FAIL on `executable` as the owner's acceptance of it: the FAIL's
/// key must be this tree's and `seal`'s, its log `report` and that log's SHA-256 `report_sha256`.
///
/// # Errors
/// No such FAIL record, any mismatch, or the write.
pub fn accept(
    git: &dyn GitRunner,
    commit: &str,
    executable: &str,
    seal: &str,
    report: (&Path, &str),
    reason: &str,
) -> Result<PathBuf, String> {
    let tree = tree_of(git, commit)?;
    accept_in(
        &root(git)?,
        (commit, &tree),
        executable,
        seal,
        report,
        reason,
    )
}

/// [`accept`] over the records under `root`.
///
/// # Errors
/// As [`accept`].
pub fn accept_in(
    root: &Path,
    (commit, tree): (&str, &str),
    executable: &str,
    seal: &str,
    (report, report_sha256): (&Path, &str),
    reason: &str,
) -> Result<PathBuf, String> {
    let at = path(root, tree, seal, "measure", Some(executable));
    let Read::Record(mut record) = read(&at)? else {
        return Err(format!(
            "{} holds no measure record of this tree under the current build-inputs seal to \
             accept; rerun verification",
            at.display()
        ));
    };
    if record.outcome != FAIL {
        return Err(format!(
            "the measure record of {}'s tree on {} is {}, not FAIL: only a measure FAIL is \
             accepted",
            short(commit),
            short(executable),
            record.outcome
        ));
    }
    if !answers(&record, "measure", tree, seal, Some(executable)) {
        return Err(
            "the measure FAIL is not this tree's under the current build-inputs seal; rerun \
             verification"
                .into(),
        );
    }
    let logged = std::fs::canonicalize(&record.log).ok();
    if logged.as_deref() != Some(report) || record.log_sha256 != report_sha256 {
        return Err(format!(
            "the measure FAIL was read from {} (sha256 {}), not {} as it is now",
            record.log,
            short(&record.log_sha256),
            report.display()
        ));
    }
    record.outcome = ACCEPTED.into();
    record.acceptance = Some(Acceptance {
        reason: reason.into(),
        failed_log_sha256: record.log_sha256.clone(),
    });
    write(&at, &record)?;
    Ok(at)
}

fn write(at: &Path, record: &CheckRecord) -> Result<(), String> {
    let text =
        aterm_toml::to_string(record).map_err(|e| format!("serialize {}: {e}", at.display()))?;
    crate::durable::write_atomic_durable(at, text.as_bytes(), 0o600)
        .map_err(|e| format!("write {}: {e}", at.display()))
}

/// Write `record` at `at` unless `at` holds a FAIL of the same key (or an owner's acceptance of
/// one) and `record` is no FAIL: a FAIL is never replaced by another outcome of the same check,
/// tree, seal and build, whatever ran it again. Returns whether it wrote.
fn keep(at: &Path, record: &CheckRecord) -> Result<bool, String> {
    if record.outcome != FAIL
        && let Read::Record(held) = read(at)?
        && held.key == record.key
        && [FAIL, ACCEPTED].contains(&held.outcome.as_str())
    {
        return Ok(false);
    }
    write(at, record)?;
    Ok(true)
}

fn remove(at: &Path) -> Result<(), String> {
    match std::fs::remove_file(at) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove {}: {e}", at.display())),
    }
}

/// What one `verify release` run said, read from its events file ([`events`]).
#[derive(Debug, Default)]
pub struct RunEvents {
    /// Each `check-finished` event, in order.
    pub finished: Vec<aterm_json::Value>,
    /// Each test binary's last `test-binary-finished` event: its spec, digest and outcome.
    pub binaries: Vec<Binary>,
    /// Lines too long to read: what they said is not known.
    pub too_long: usize,
    /// `verify-event:` lines whose JSON does not parse: what they said is not known.
    pub garbled: usize,
    /// The file ended inside a line: a run cut off as it wrote.
    pub partial: bool,
}

impl RunEvents {
    /// Whether every line was read whole, so nothing the run said is lost.
    #[must_use]
    pub const fn whole(&self) -> bool {
        self.too_long == 0 && self.garbled == 0 && !self.partial
    }
}

/// The events a `verify release` run wrote to its events file `at` (`--events`).
///
/// # Errors
/// The file cannot be opened or read.
pub fn events(at: &Path) -> std::io::Result<RunEvents> {
    let file = crate::durable::open_regular_file(at)?;
    let mut run = RunEvents::default();
    let mut binaries: Vec<Binary> = Vec::new();
    let partial = crate::release_status::event_lines(file, |line| {
        let json = match line {
            crate::release_status::Line::Verify(json) => json,
            crate::release_status::Line::TooLong => {
                run.too_long += 1;
                return;
            }
            crate::release_status::Line::Release(_) => return,
        };
        let Ok(event) = aterm_json::from_slice::<aterm_json::Value>(json) else {
            run.garbled += 1;
            return;
        };
        if event["schema"].as_u64() != Some(1) {
            return;
        }
        match event["event"].as_str() {
            Some("check-finished") => run.finished.push(event),
            Some("test-binary-finished") => {
                let (Some(spec), Some(outcome)) =
                    (event["test"].as_str(), event["outcome"].as_str())
                else {
                    return;
                };
                let sha256 = event["exe_sha256"]
                    .as_str()
                    .filter(|s| aterm_codec::hex::is_lower(s, 64))
                    .unwrap_or_default();
                if let Some(pos) = binaries.iter().position(|b| b.spec == spec) {
                    let mut b = binaries.remove(pos);
                    sha256.clone_into(&mut b.sha256);
                    outcome.clone_into(&mut b.outcome);
                    binaries.push(b);
                } else {
                    binaries.push(Binary {
                        spec: spec.to_owned(),
                        sha256: sha256.to_owned(),
                        outcome: outcome.to_owned(),
                    });
                }
            }
            _ => {}
        }
    })?;
    run.partial = partial;
    run.binaries = binaries;
    Ok(run)
}

/// One `verify release` run of R's tree under one seal: what every record it leaves states.
#[derive(Debug, Clone)]
pub struct RunFacts<'a> {
    pub commit: &'a str,
    pub tree: &'a str,
    pub seal: &'a str,
    /// U: the executable the run judges.
    pub executable: &'a str,
    /// Its log, which every record names.
    pub log: &'a Path,
    /// The release run (its logs directory's name).
    pub run: &'a str,
    /// The checks it was asked for.
    pub asked: &'a [String],
}

/// How one run ended: its exit code, its log's SHA-256 (empty when it could not be hashed), and
/// what its events file said, or why it could not be read.
#[derive(Debug, Clone, Copy)]
pub struct RunEnd<'a> {
    pub exit: i32,
    pub log_sha256: &'a str,
    pub events: Result<&'a RunEvents, &'a str>,
}

fn record(
    facts: &RunFacts<'_>,
    (exit, log_sha256): (i32, &str),
    check: &str,
    outcome: &str,
    executable: Option<&str>,
) -> CheckRecord {
    CheckRecord {
        schema: SCHEMA,
        check: check.to_owned(),
        key: key(check, facts.tree, facts.seal, executable),
        tree: facts.tree.to_owned(),
        commit: facts.commit.to_owned(),
        seal: facts.seal.to_owned(),
        executable: executable.map(str::to_owned),
        outcome: outcome.to_owned(),
        seconds: 0.0,
        detail: String::new(),
        rows: Vec::new(),
        log: facts.log.display().to_string(),
        log_sha256: log_sha256.to_owned(),
        run: facts.run.to_owned(),
        exit,
        acceptance: None,
        binaries: Vec::new(),
    }
}

/// Before `verify release` runs: remove a legacy single record of the tree, then record that a
/// run of R's tree under this seal began (`verify.toml`, `began`), which reads as a FAIL until
/// [`write_run`] settles it or [`abandon_run`] withdraws it. Returns its path.
///
/// # Errors
/// The legacy record cannot be removed, a FAIL or a `began` of this tree under this seal is
/// there already (the FAIL rule refuses the tree), or the record cannot be written: the run must
/// not start, since a FAIL it found could not be kept.
pub fn begin_run(root: &Path, facts: &RunFacts<'_>) -> Result<PathBuf, String> {
    let dir = root.join(facts.tree);
    if std::fs::symlink_metadata(&dir).is_ok_and(|m| !m.is_dir()) {
        std::fs::remove_file(&dir)
            .map_err(|e| format!("remove the legacy record {}: {e}", dir.display()))?;
    }
    let at = path(root, facts.tree, facts.seal, RUN_LEVEL, None);
    if let Read::Record(held) = read(&at)?
        && answers(&held, RUN_LEVEL, facts.tree, facts.seal, None)
        && [FAIL, BEGAN].contains(&held.outcome.as_str())
    {
        return Err(format!(
            "{} says {} for this tree under this seal (run {}): the FAIL rule refuses it",
            at.display(),
            held.outcome,
            held.run
        ));
    }
    let mut r = record(facts, (-1, ""), RUN_LEVEL, BEGAN, None);
    r.detail = format!(
        "`verify release` of {} began, asked for {}; until every FAIL it shows is kept, this tree \
         reads as FAILED under this seal",
        short(facts.commit),
        facts.asked.join(",")
    );
    write(&at, &r)?;
    Ok(at)
}

/// The run no longer judged R's tree under this seal (the clone moved, or the seal changed or
/// could not be read): withdraw its `began` record, keeping nothing of the run.
///
/// # Errors
/// The record cannot be read or removed.
pub fn abandon_run(root: &Path, facts: &RunFacts<'_>) -> Result<(), String> {
    let at = path(root, facts.tree, facts.seal, RUN_LEVEL, None);
    match read(&at)? {
        Read::Record(held) if held.outcome == BEGAN && held.run == facts.run => remove(&at),
        _ => Ok(()),
    }
}

/// Keep the records of a run [`begin_run`] began, once it judged R's tree under this seal: one
/// per check its events say finished (of those `facts.asked`), and a test check that never
/// finished recorded from its binaries (FAIL when one failed, else NOT RUN, each listed). Every
/// FAIL is written first, and an exit 1 that names none becomes the run-level FAIL. The `began`
/// record goes only once every FAIL the run showed is kept: for an exit 1 once its FAIL is
/// written; for an exit 0, 2 or 3, verify's own word that no check failed; for any other end (a
/// killed run) only when its events were read whole. Otherwise it stays, and the tree reads as
/// FAILED under this seal. A FAIL already recorded is never replaced ([`keep`]). Returns one line
/// per record written.
///
/// # Errors
/// A FAIL that cannot be written (the `began` record then stays), another record that cannot be
/// written (the rest are still tried), the `began` record that cannot be removed, or events that
/// could not be read.
pub fn write_run(
    root: &Path,
    facts: &RunFacts<'_>,
    end: RunEnd<'_>,
) -> Result<Vec<String>, String> {
    let ended = (end.exit, end.log_sha256);
    let seal_path = root.join(facts.tree).join(seal_dir(facts.seal));
    let mut records: Vec<(PathBuf, CheckRecord, String)> = Vec::new();
    if let Ok(events) = end.events {
        let mut test_finished = false;
        for event in &events.finished {
            let (Some(name), Some(outcome)) = (event["check"].as_str(), event["outcome"].as_str())
            else {
                continue;
            };
            let Some(check) = RELEASE.into_iter().find(|c| c.name() == name) else {
                continue;
            };
            if !facts.asked.iter().any(|a| a == name)
                || ![OK, FAIL, NOT_RUN, SKIP].contains(&outcome)
            {
                continue;
            };
            let u = bundle(check).then_some(facts.executable);
            let mut r = record(facts, ended, name, outcome, u);
            r.seconds = event["seconds"].as_f64().unwrap_or(0.0);
            event["detail"]
                .as_str()
                .unwrap_or_default()
                .clone_into(&mut r.detail);
            r.rows = event["rows"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .map(|row| {
                            format!(
                                "{} {}",
                                row["outcome"].as_str().unwrap_or("?"),
                                row["detail"].as_str().unwrap_or_default()
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            if check == Check::Test {
                test_finished = true;
                r.binaries.clone_from(&events.binaries);
            }
            records.push((
                path_in_seal_dir(&seal_path, name, u),
                r,
                format!("{name} {outcome}"),
            ));
        }
        if !test_finished && facts.asked.iter().any(|a| a == "test") && !events.binaries.is_empty()
        {
            let fail = events.binaries.iter().any(|b| b.outcome == FAIL);
            let ok = events.binaries.iter().filter(|b| b.outcome == OK).count();
            let mut r = record(
                facts,
                ended,
                "test",
                if fail { FAIL } else { NOT_RUN },
                None,
            );
            r.detail = format!(
                "the run ended before its test check finished: {ok} of the {} binaries that ended \
                 passed{}",
                events.binaries.len(),
                if fail { ", and one failed" } else { "" }
            );
            r.binaries.clone_from(&events.binaries);
            let said = format!("test {} (from its binaries)", r.outcome);
            records.push((path_in_seal_dir(&seal_path, "test", None), r, said));
        }
    }
    let failed = records.iter().any(|(_, r, _)| r.outcome == FAIL);
    let mut lines = Vec::new();
    for (at, r, said) in records.iter().filter(|(_, r, _)| r.outcome == FAIL) {
        // A FAIL that cannot be kept leaves the `began` record standing, which refuses the tree.
        write(at, r)?;
        lines.push(format!("{said} → {}", at.display()));
    }
    let began = path_in_seal_dir(&seal_path, RUN_LEVEL, None);
    let run_level = end.exit == 1 && !failed;
    if run_level {
        let mut r = record(facts, ended, RUN_LEVEL, FAIL, None);
        r.detail = match end.events {
            Ok(events) => format!(
                "`verify release` exited 1 (FAIL) and no check's FAIL was read from its events \
                 ({} line(s) too long to read, {} that did not parse)",
                events.too_long, events.garbled
            ),
            Err(why) => {
                format!("`verify release` exited 1 (FAIL) and its events could not be read: {why}")
            }
        };
        write(&began, &r)?;
        lines.push(format!(
            "{RUN_LEVEL} FAIL (exit 1, no check named) → {}",
            began.display()
        ));
    }
    let mut first_error = None;
    for (at, r, said) in records.iter().filter(|(_, r, _)| r.outcome != FAIL) {
        match keep(at, r) {
            Ok(true) => lines.push(format!("{said} → {}", at.display())),
            Ok(false) => lines.push(format!(
                "{said} not kept → {}: it holds this check's FAIL, never replaced",
                at.display()
            )),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    let settled = match end.exit {
        0..=3 => true,
        _ => end.events.is_ok_and(RunEvents::whole),
    };
    if !settled {
        lines.push(format!(
            "{RUN_LEVEL} {BEGAN} kept → {}: the run ended with no exit code of verify's own \
             ({}) and its events were not read whole, so this tree reads as FAILED under this \
             seal",
            began.display(),
            end.exit
        ));
    } else if !run_level {
        remove(&began)?;
    }
    if let Err(why) = end.events {
        return Err(why.to_owned());
    }
    first_error.map_or(Ok(lines), Err)
}

/// Test seam: the records a `verify release` of `commit` on `executable` under `seal` that exited
/// `exit` leaves when each check ended `outcome(check)` (`None`: it never finished), as
/// [`begin_run`] and [`write_run`] keep them in the clone `git` names.
#[cfg(test)]
pub fn record_run(
    git: &dyn GitRunner,
    commit: &str,
    executable: &str,
    seal: &str,
    exit: i32,
    outcome: impl Fn(&str) -> Option<&'static str>,
) -> Vec<String> {
    let tree = tree_of(git, commit).unwrap();
    let root = root(git).unwrap();
    let asked = release_names();
    let events = RunEvents {
        finished: asked
            .iter()
            .filter_map(|check| {
                outcome(check).map(|o| {
                    aterm_json::json!({"schema":1,"event":"check-finished","check":check,
                        "outcome":o,"seconds":1.0,"detail":"","rows":[]})
                })
            })
            .collect(),
        ..RunEvents::default()
    };
    let log = root.join("test-run.log");
    let facts = RunFacts {
        commit,
        tree: &tree,
        seal,
        executable,
        log: &log,
        run: "test-run",
        asked: &asked,
    };
    begin_run(&root, &facts).unwrap();
    write_run(
        &root,
        &facts,
        RunEnd {
            exit,
            log_sha256: &"00".repeat(32),
            events: Ok(&events),
        },
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = "1111111111111111111111111111111111111111";
    const COMMIT: &str = "2222222222222222222222222222222222222222";
    const RUN: &str = "20261105T120000-1";

    fn u(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    fn finished(check: &str, outcome: &str) -> aterm_json::Value {
        aterm_json::json!({"schema":1,"event":"check-finished","check":check,"outcome":outcome,
            "seconds":2.0,"detail":"d","rows":[{"outcome":outcome,"detail":"row"}]})
    }

    fn bin(spec: &str, sha: u8, outcome: &str) -> Binary {
        Binary {
            spec: spec.into(),
            sha256: u(sha),
            outcome: outcome.into(),
        }
    }

    fn names(failed: &[Failed]) -> Vec<&str> {
        failed.iter().map(|f| f.check.as_str()).collect()
    }

    struct Fixture {
        _dir: aterm_tempfile::TempDir,
        root: PathBuf,
        log: PathBuf,
        asked: Vec<String>,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path().join("verified");
            let log = dir.path().join("06-verify.log");
            std::fs::write(&log, "log\n").unwrap();
            Self {
                _dir: dir,
                root,
                log,
                asked: release_names(),
            }
        }

        fn facts<'a>(&'a self, seal: &'a str, executable: &'a str) -> RunFacts<'a> {
            RunFacts {
                commit: COMMIT,
                tree: TREE,
                seal,
                executable,
                log: &self.log,
                run: RUN,
                asked: &self.asked,
            }
        }

        /// The run's end, as `write_run` takes it.
        fn end(
            &self,
            seal: &str,
            executable: &str,
            exit: i32,
            events: Result<&RunEvents, &str>,
        ) -> Result<Vec<String>, String> {
            write_run(
                &self.root,
                &self.facts(seal, executable),
                RunEnd {
                    exit,
                    log_sha256: &u(9),
                    events,
                },
            )
        }

        fn begin(&self, seal: &str, executable: &str) -> Result<PathBuf, String> {
            begin_run(&self.root, &self.facts(seal, executable))
        }

        /// One whole run: begun, then ended.
        fn write(
            &self,
            seal: &str,
            executable: &str,
            exit: i32,
            events: &RunEvents,
        ) -> Vec<String> {
            self.begin(seal, executable).unwrap();
            self.end(seal, executable, exit, Ok(events)).unwrap()
        }

        fn judge(&self, seal: &str, executable: Option<&str>) -> Checks {
            checks_in(&self.root, COMMIT, TREE, executable, || Ok(seal.to_owned())).unwrap()
        }
    }

    fn all_ok() -> RunEvents {
        RunEvents {
            finished: release_names().iter().map(|c| finished(c, OK)).collect(),
            ..RunEvents::default()
        }
    }

    /// Every check ok on these bytes under this seal is complete; another seal, another
    /// executable or a garbled record is missing, never an error, and so is a record of another
    /// seal found where this seal's lies; a record that cannot be read leaves no answer; with no
    /// records of the tree the seal is not taken. Negative control: a judge that skips the key
    /// comparison passes the record moved under the other seal.
    #[test]
    fn a_tree_passes_only_on_its_own_records_under_its_seal() {
        let f = Fixture::new();
        let untaken = checks_in(&f.root, COMMIT, TREE, Some(&u(1)), || {
            Err("the seal is not taken with no records".into())
        })
        .unwrap();
        assert_eq!(untaken.missing, release_names());
        f.write("s1", &u(1), 0, &all_ok());
        let judged = f.judge("s1", Some(&u(1)));
        assert!(judged.complete(), "{}", judged.line());
        let other = f.judge("s2", Some(&u(1)));
        assert_eq!(other.missing, release_names(), "{}", other.line());
        let moved = path(&f.root, TREE, "s2", "fmt", None);
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::copy(path(&f.root, TREE, "s1", "fmt", None), &moved).unwrap();
        assert_eq!(
            f.judge("s2", Some(&u(1))).missing,
            release_names(),
            "a record answers only for its own key, wherever it lies"
        );
        let rebuilt = f.judge("s1", Some(&u(2)));
        assert_eq!(
            rebuilt.missing,
            ["measure"],
            "tree records stand for another build"
        );
        assert_eq!(f.judge("s1", None).missing, ["measure"]);
        let lint = path(&f.root, TREE, "s1", "lint", None);
        std::fs::write(&lint, "not a record").unwrap();
        assert_eq!(f.judge("s1", Some(&u(1))).missing, ["lint"]);
        std::fs::remove_file(&lint).unwrap();
        std::fs::create_dir(&lint).unwrap();
        assert!(
            checks_in(&f.root, COMMIT, TREE, Some(&u(1)), || Ok("s1".into())).is_err(),
            "an unreadable record is no answer"
        );
    }

    /// A FAIL record refuses the tree under its seal, and a measure FAIL of another build of the
    /// tree refuses it too; NOT RUN and skip are missing, and a resume runs only those, carrying
    /// the test binaries that passed with a known digest. Negative controls: dropping the bundle
    /// scan lets a rebuilt executable re-roll the measure FAIL; dropping the digest filter
    /// carries a binary with none, which `--passed-binaries` refuses.
    #[test]
    fn a_fail_refuses_its_tree_and_a_resume_runs_only_what_is_missing() {
        let f = Fixture::new();
        let mut run = all_ok();
        run.finished = vec![
            finished("fmt", OK),
            finished("lint", OK),
            finished("doc", SKIP),
            finished("census", OK),
            finished("measure", FAIL),
            finished("cells", FAIL),
        ];
        run.binaries = vec![
            bin("-p a --lib", 1, OK),
            bin("-p b --lib", 2, NOT_RUN),
            Binary {
                sha256: String::new(),
                ..bin("-p c --lib", 3, OK)
            },
        ];
        let lines = f.write("s1", &u(1), 1, &run);
        assert_eq!(
            lines.len(),
            6,
            "five checks and the unfinished test: {lines:?}"
        );
        let judged = f.judge("s1", Some(&u(1)));
        assert_eq!(names(&judged.failed), ["measure"]);
        let refusal = judged.fail_refusal(COMMIT).expect("refused");
        assert!(!refusal.could_not_run);
        assert!(
            refusal.remedy.contains("a new hop changes the tree"),
            "{}",
            refusal.remedy
        );
        assert_eq!(
            names(&f.judge("s1", Some(&u(2))).failed),
            ["measure"],
            "a rebuilt executable never re-rolls a measure FAIL"
        );
        assert!(
            f.judge("s2", Some(&u(2))).fail_refusal(COMMIT).is_none(),
            "another seal"
        );

        let f = Fixture::new();
        run.finished
            .retain(|e| e["check"].as_str() != Some("measure"));
        let lines = f.write("s1", &u(1), 3, &run);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("test NOT RUN (from its binaries)")),
            "{lines:?}"
        );
        let judged = f.judge("s1", Some(&u(1)));
        assert!(judged.failed.is_empty() && !judged.complete());
        let want: Vec<String> = release_names()
            .into_iter()
            .filter(|c| !["fmt", "lint", "census"].contains(&c.as_str()))
            .collect();
        assert_eq!(judged.missing, want);
        let resume = judged.run();
        assert_eq!(resume.checks, judged.missing);
        assert_eq!(
            resume.passed_binaries,
            [("-p a --lib".to_owned(), u(1))],
            "a binary whose digest is not known runs again"
        );
        assert!(resume.strict_subset() && resume.reads_the_bundle());
        let all = VerifyRun {
            checks: release_names(),
            passed_binaries: Vec::new(),
        };
        assert!(!all.strict_subset());
        let tree_only = VerifyRun {
            checks: vec!["lint".into(), "test".into()],
            passed_binaries: Vec::new(),
        };
        assert!(!tree_only.reads_the_bundle());
    }

    /// A test check killed before it finished is recorded from its binaries (FAIL when one
    /// failed), and an exit 1 whose events name no FAIL, or could not be read, is a run-level FAIL
    /// of its own seal alone, so the FAIL rule never rests on parsing; a run cannot begin over it.
    /// Negative controls: without the run-level record the exit-1 tree reads as merely missing; a
    /// run-level judge that skips the key refuses the tree under the seal the record was moved to.
    #[test]
    fn a_fail_is_recorded_whatever_its_events_let_the_cutter_read() {
        let f = Fixture::new();
        let run = RunEvents {
            binaries: vec![bin("-p a --lib", 1, OK), bin("-p b --lib", 2, FAIL)],
            ..RunEvents::default()
        };
        f.write("s1", &u(1), 1, &run);
        assert_eq!(names(&f.judge("s1", Some(&u(1))).failed), ["test"]);

        let f = Fixture::new();
        let lines = f.write("s1", &u(1), 1, &RunEvents::default());
        assert_eq!(lines.len(), 1, "{lines:?}");
        let judged = f.judge("s1", Some(&u(1)));
        assert_eq!(names(&judged.failed), [RUN_LEVEL]);
        assert!(!judged.failed[0].unrecorded);
        let other = f.judge("s2", Some(&u(1)));
        assert!(other.failed.is_empty(), "{}", other.line());
        assert_eq!(other.missing, release_names());
        let moved = path(&f.root, TREE, "s2", RUN_LEVEL, None);
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::copy(path(&f.root, TREE, "s1", RUN_LEVEL, None), &moved).unwrap();
        assert!(
            f.judge("s2", Some(&u(1))).failed.is_empty(),
            "a run-level FAIL answers only for its own seal"
        );
        assert!(
            f.begin("s1", &u(1)).is_err(),
            "no run begins over a FAIL of its tree and seal"
        );

        let f = Fixture::new();
        f.begin("s1", &u(1)).unwrap();
        assert!(
            f.end("s1", &u(1), 1, Err("its events file is gone"))
                .is_err()
        );
        assert_eq!(
            names(&f.judge("s1", Some(&u(1))).failed),
            [RUN_LEVEL],
            "an exit 1 whose events could not be read still refuses"
        );
        let f = Fixture::new();
        assert!(f.write("s1", &u(1), 3, &RunEvents::default()).is_empty());
        assert!(f.judge("s1", Some(&u(1))).failed.is_empty());
    }

    /// The FAIL rule never rests on a write: a run reads as FAILED from before it starts until
    /// every FAIL it showed is kept. A FAIL whose record cannot be written leaves the `began`
    /// record, which refuses the tree; FAILs are written before any other record, so one other
    /// record that cannot be written loses no FAIL; a killed run whose events were not read whole
    /// keeps `began`, and one read whole settles to what they say; an exit 3 settles on verify's
    /// own word; a run that no longer judged the tree withdraws its `began`; and no later run
    /// replaces a FAIL. Negative controls: without the `began` record a FAIL that could not be
    /// written leaves the tree merely missing; writing a FAIL after the others loses it to the
    /// first that fails; a plain write in place of `keep` replaces the FAIL with an ok.
    #[test]
    fn a_fail_that_cannot_be_written_still_refuses_its_tree() {
        let block = |at: &Path| std::fs::create_dir_all(at).unwrap();
        let unblock = |at: &Path| std::fs::remove_dir(at).unwrap();
        let test_fails = RunEvents {
            finished: vec![finished("lint", OK), finished("test", FAIL)],
            ..RunEvents::default()
        };

        let f = Fixture::new();
        f.begin("s1", &u(1)).unwrap();
        let judged = f.judge("s1", Some(&u(1)));
        assert_eq!(names(&judged.failed), [RUN_LEVEL], "a run under way");
        assert!(judged.failed[0].unrecorded);
        let test = path(&f.root, TREE, "s1", "test", None);
        block(&test);
        assert!(f.end("s1", &u(1), 1, Ok(&test_fails)).is_err());
        unblock(&test);
        let judged = f.judge("s1", Some(&u(1)));
        assert_eq!(names(&judged.failed), [RUN_LEVEL], "{}", judged.line());
        assert!(judged.fail_refusal(COMMIT).is_some());

        let f = Fixture::new();
        f.begin("s1", &u(1)).unwrap();
        let lint = path(&f.root, TREE, "s1", "lint", None);
        block(&lint);
        assert!(f.end("s1", &u(1), 1, Ok(&test_fails)).is_err());
        unblock(&lint);
        let judged = f.judge("s1", Some(&u(1)));
        assert_eq!(
            names(&judged.failed),
            ["test"],
            "the FAIL was written first: {}",
            judged.line()
        );

        let f = Fixture::new();
        let killed = RunEvents {
            binaries: vec![bin("-p a --lib", 1, OK)],
            partial: true,
            ..RunEvents::default()
        };
        f.begin("s1", &u(1)).unwrap();
        f.end("s1", &u(1), -1, Ok(&killed)).unwrap();
        assert_eq!(names(&f.judge("s1", Some(&u(1))).failed), [RUN_LEVEL]);
        let f = Fixture::new();
        f.begin("s1", &u(1)).unwrap();
        assert!(f.end("s1", &u(1), 137, Err("unreadable")).is_err());
        assert_eq!(names(&f.judge("s1", Some(&u(1))).failed), [RUN_LEVEL]);
        let f = Fixture::new();
        let whole = RunEvents {
            partial: false,
            ..killed
        };
        f.write("s1", &u(1), -1, &whole);
        let judged = f.judge("s1", Some(&u(1)));
        assert!(judged.failed.is_empty(), "{}", judged.line());
        assert_eq!(
            judged.run().passed_binaries,
            [("-p a --lib".to_owned(), u(1))]
        );

        let f = Fixture::new();
        f.begin("s1", &u(1)).unwrap();
        let lint = path(&f.root, TREE, "s1", "lint", None);
        block(&lint);
        assert!(f.end("s1", &u(1), 3, Ok(&all_ok())).is_err());
        unblock(&lint);
        let judged = f.judge("s1", Some(&u(1)));
        assert!(judged.failed.is_empty(), "{}", judged.line());
        assert_eq!(judged.missing, ["lint"], "an exit 3 found no FAIL");

        let f = Fixture::new();
        f.begin("s1", &u(1)).unwrap();
        abandon_run(&f.root, &f.facts("s1", &u(1))).unwrap();
        assert_eq!(f.judge("s1", Some(&u(1))).missing, release_names());

        let f = Fixture::new();
        f.write("s1", &u(1), 1, &test_fails);
        let lines = f.write(
            "s1",
            &u(1),
            0,
            &RunEvents {
                finished: vec![finished("test", OK)],
                ..RunEvents::default()
            },
        );
        assert!(
            lines.iter().any(|l| l.starts_with("test ok not kept")),
            "{lines:?}"
        );
        assert_eq!(names(&f.judge("s1", Some(&u(1))).failed), ["test"]);
    }

    /// A legacy single record (a file at `verified/<tree>`) stands for nothing and is replaced
    /// by the next run's directory of records. Negative control: without the removal the run
    /// cannot begin on the file in its path.
    #[test]
    fn a_legacy_single_record_is_missing_and_replaced() {
        let f = Fixture::new();
        std::fs::create_dir_all(&f.root).unwrap();
        std::fs::write(
            f.root.join(TREE),
            "commit = \"x\"\nexecutable = \"y\"\ninputs = \"z\"\n",
        )
        .unwrap();
        let judged = f.judge("s1", Some(&u(1)));
        assert_eq!(judged.missing, release_names(), "{}", judged.line());
        f.write("s1", &u(1), 0, &all_ok());
        assert!(f.root.join(TREE).is_dir());
        assert!(f.judge("s1", Some(&u(1))).complete());
    }

    /// An owner's acceptance rewrites only a measure FAIL of this tree under this seal, read
    /// from the named report as it still is, and completes the tree only beside every other
    /// check's ok record. Negative control: an acceptance that skips the seal comparison accepts
    /// the FAIL of another seal.
    #[test]
    fn an_accepted_measure_passes_only_beside_every_other_ok() {
        let f = Fixture::new();
        let mut run = all_ok();
        run.finished.retain(|e| e["check"].as_str() != Some("test"));
        let report = std::fs::canonicalize(&f.log).unwrap();
        let accept = |seal: &str, report: &Path, sha: &str| {
            accept_in(
                &f.root,
                (COMMIT, TREE),
                &u(1),
                seal,
                (report, sha),
                "the owner shipped it",
            )
        };
        f.write("s1", &u(1), 0, &run);
        assert!(
            accept("s1", &report, &u(9)).is_err(),
            "an ok measure is not accepted"
        );
        run.finished
            .retain(|e| e["check"].as_str() != Some("measure"));
        run.finished.push(finished("measure", FAIL));
        f.write("s1", &u(1), 1, &run);
        assert!(f.judge("s1", Some(&u(1))).fail_refusal(COMMIT).is_some());
        assert!(accept("s2", &report, &u(9)).is_err(), "another seal");
        assert!(accept("s1", &report, &u(8)).is_err(), "a changed report");
        assert!(
            accept("s1", Path::new("/elsewhere/06-verify.log"), &u(9)).is_err(),
            "another report"
        );
        accept("s1", &report, &u(9)).unwrap();
        let judged = f.judge("s1", Some(&u(1)));
        assert_eq!(judged.missing, ["test"]);
        assert!(judged.accepted.is_none());
        f.write(
            "s1",
            &u(1),
            0,
            &RunEvents {
                finished: vec![finished("test", OK)],
                ..RunEvents::default()
            },
        );
        let judged = f.judge("s1", Some(&u(1)));
        assert!(judged.complete(), "{}", judged.line());
        assert!(judged.accepted.is_some());
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tree-local release check records with content receipts; a FAIL is never
//! re-rolled (A19).
//!
//! `verify release` ends each check with one `check-finished` line ([`aterm_verify::report`])
//! each test binary with one `test-binary-finished` line, and each lint package with one
//! `lint-package-finished` line. It writes each whole to the run's own events file (`--events`),
//! which only verify's own process writes: no child's output and no other writer's bytes land
//! inside a line there, as they can in its output. Every record of one
//! tree under one seal lives in one directory, `<git common dir>/aterm-release/.ubd/verified/<tree>/<S>/`,
//! S the seal's digest ([`seal_dir`]):
//! - a tree check's at `<check>.toml`, keyed by H(check, tree, seal);
//! - the bundle check's, `measure` (the one check that reads `--app`), at `measure/<U>.toml`,
//!   indexed by H(check, tree, seal, executable SHA-256). A separate content receipt under
//!   `product-measure/` binds normalized bundle U, the linked measuring benches, the verifier
//!   executable and the seal. Only that receipt can supply a measure verdict across trees;
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
//! binaries, lint packages and AppKit executable groups a missing record lists as passed with
//! their complete bounded content proofs ([`VerifyRun`]). A record that does not parse, or whose
//! key is not this tree's and seal's, is missing; one that cannot be read leaves the judge without
//! an answer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aterm_verify::checks::{Check, RELEASE, RELEASE_BUNDLE, RELEASE_TREE};
use aterm_verify::stages::MEASURING_TARGETS;

use crate::ledger::GitRunner;
use crate::release::{Answer, Refusal};

/// Where the records live, in the release directory.
pub const DIR: &str = ".ubd/verified";

/// The run-level record's name: a `verify release` that began and has not settled, or that
/// exited 1 (FAIL) with no check's FAIL read from its events, still refuses its tree, so the FAIL
/// rule never rests on parsing or on a write.
pub const RUN_LEVEL: &str = "verify";

const SCHEMA: u32 = 1;
const KEY_DOMAIN: &str = "aterm-release-check-v1";
const SEAL_DOMAIN: &str = "aterm-release-seal-dir-v1";
/// The largest record written or read. A record lists what its check read (its input items),
/// which for a whole-tree check is every file of the tree: an archive merge that puts the whole
/// workspace in the release input made a 1.3 MB `fmt` record a 1 MiB cap refused to read back.
/// The bound stops a runaway file, not a large tree; [`write`] holds a record to it too.
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;
/// A record made only to hand the immediately preceding hop's item receipts to its verifier.
/// Its rows were not re-keyed on the tree the record names and cannot seed another hop.
const CANDIDATE_RUN_PREFIX: &str = "candidates-from-";
/// The run a whole-check record names when the hop carry copied it from the line's preceding
/// head ([`carry_hop`]): no run of the record's own tree executed that check.
const CARRIED_RUN_PREFIX: &str = "carried-from-";

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

/// Tree-local verdicts eligible for adjacent-hop content carry. AppKit is invoked with the
/// bundle half's canonical CLI context, but its debug examples read the tree rather than `--app`.
fn hop_checks() -> impl Iterator<Item = Check> {
    RELEASE.into_iter().filter(|check| !bundle(*check))
}

/// One test binary a test record lists: its outcome and the complete content proof verify
/// observed. Legacy records had only `spec`, `sha256` and `outcome`; serde defaults make those
/// rows readable (especially a standing FAIL) but unbounded, so they can never cross a hop.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Binary {
    pub spec: String,
    #[serde(default)]
    pub sha256: String,
    pub outcome: String,
    #[serde(default)]
    pub content_key: Option<String>,
    #[serde(default)]
    pub inputs_bounded: bool,
    #[serde(default)]
    pub input_reason: Option<String>,
    /// The prior run's global libtest JSON decision. Legacy rows omit it and fail closed.
    #[serde(default)]
    pub json: Option<bool>,
    #[serde(default)]
    pub execution_s: f64,
    /// The content key's named parts ([`aterm_verify::test_content::KeyParts`]): what a later
    /// hop that refuses this row names as moved. Diagnostic; never compared for a carry.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub key_parts: BTreeMap<String, String>,
    /// The release run whose execution this row is: this record's own run unless the row was
    /// carried, and then the run it was carried from, however many hops it crossed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_run: Option<String>,
}

impl Binary {
    /// Whether a resume may offer it as a candidate. Verify still recomputes its current proof.
    fn carried(&self) -> bool {
        self.outcome == OK
            && self.inputs_bounded
            && aterm_codec::hex::is_lower(&self.sha256, 64)
            && self
                .content_key
                .as_deref()
                .is_some_and(|key| aterm_codec::hex::is_lower(key, 64))
            && self.input_reason.is_none()
            && self.json.is_some()
            && self.execution_s.is_finite()
            && self.execution_s >= 0.0
    }

    /// The verifier's versioned JSONL proof for this candidate.
    pub(crate) fn proof(&self) -> Option<aterm_verify::testrun::CarryProof> {
        self.carried().then(|| aterm_verify::testrun::CarryProof {
            schema: aterm_verify::test_content::SCHEMA,
            spec: self.spec.clone(),
            outcome: OK.into(),
            exe_sha256: Some(self.sha256.clone()),
            content_key: self.content_key.clone(),
            inputs_bounded: true,
            input_reason: None,
            json: self.json.expect("a carried binary has its JSON decision"),
            execution_s: self.execution_s,
            key_parts: self.key_parts.clone(),
            origin_run: self.origin_run.clone(),
        })
    }
}

/// `rows` as a record of `run` keeps them: a row that ran and names no origin executed in
/// that run. A carried row keeps the run it names, and a row that did not run names none.
fn executed_in(rows: &[Binary], run: &str) -> Vec<Binary> {
    rows.iter()
        .cloned()
        .map(|mut row| {
            if row.outcome != NOT_RUN {
                row.origin_run.get_or_insert_with(|| run.to_owned());
            }
            row
        })
        .collect()
}

/// One workspace package's lint outcome and complete compile-time content proof. Legacy records
/// omit these rows and therefore rerun every package. A row is only a carry candidate when it is
/// an explicitly bounded OK with a canonical key; the verifier still recomputes that key on the
/// current hop before it skips the package.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LintPackage {
    pub spec: String,
    pub outcome: String,
    #[serde(default)]
    pub content_key: Option<String>,
    #[serde(default)]
    pub inputs_bounded: bool,
    #[serde(default)]
    pub input_reason: Option<String>,
    #[serde(default)]
    pub execution_s: f64,
    /// This cut revalidated an earlier verdict rather than executing Tippy for this package.
    #[serde(default)]
    pub reused: bool,
}

impl LintPackage {
    /// Whether a later qualification may offer this row to the verifier. FAIL and unbounded rows
    /// are tombstones, never evidence.
    fn carried(&self) -> bool {
        self.outcome == OK
            && self.inputs_bounded
            && self
                .content_key
                .as_deref()
                .is_some_and(|key| aterm_codec::hex::is_lower(key, 64))
            && self.input_reason.is_none()
            && self.execution_s.is_finite()
            && self.execution_s >= 0.0
    }

    /// The verifier's versioned proof for this candidate.
    pub(crate) fn proof(&self) -> Option<aterm_verify::lint_content::CarryProof> {
        self.carried()
            .then(|| aterm_verify::lint_content::CarryProof {
                schema: aterm_verify::lint_content::SCHEMA,
                spec: self.spec.clone(),
                outcome: OK.into(),
                content_key: self.content_key.clone(),
                inputs_bounded: true,
                input_reason: None,
                execution_s: self.execution_s,
            })
    }
}

/// Whether one AppKit group is a canonical proof the verifier can independently re-key after it
/// rebuilds the current executable group. Validation lives with the wire owner; records which
/// predate the schema, were hand-edited, or hold a partial/malformed proof fail closed.
fn appkit_group_carried(proof: &aterm_verify::appkit_content::GroupProof) -> bool {
    let passed = BTreeMap::from([(proof.group.clone(), proof.clone())]);
    aterm_verify::appkit_content::encode_passed(&passed).is_ok()
}

/// Canonical candidates by group. One malformed or repeated group invalidates that group alone;
/// the other independently built group may still avoid opening its windows.
fn appkit_group_candidates(
    proofs: impl IntoIterator<Item = aterm_verify::appkit_content::GroupProof>,
) -> Vec<aterm_verify::appkit_content::GroupProof> {
    let mut groups: BTreeMap<String, Option<aterm_verify::appkit_content::GroupProof>> =
        BTreeMap::new();
    for proof in proofs {
        let group = proof.group.clone();
        match groups.entry(group) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(appkit_group_carried(&proof).then_some(proof));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
    groups.into_values().flatten().collect()
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
    /// The run's build-inputs identity ([`crate::build_inputs::current`]); a release-input
    /// namespace additionally binds its selection and finalized execution context.
    pub seal: String,
    /// The separately declared content this check read.  The authoritative key above remains
    /// tree-local; this evidence can only seed the immediately following line hop.
    #[serde(default)]
    pub content: Option<crate::check_content::Input>,
    /// The stamped executable's SHA-256, for a bundle check's tree-local index.
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
    /// Per-workspace-package lint evidence. The aggregate lint record remains tree-local; these
    /// rows are finer content receipts which a later qualification must re-key before reuse.
    #[serde(default)]
    pub lint_packages: Vec<LintPackage>,
    /// AppKit target groups whose exact executable bytes passed (or were re-keyed from an exact
    /// prior proof) in this run. The verifier rebuilds each group before it may reuse one.
    #[serde(default)]
    pub appkit_groups: Vec<aterm_verify::appkit_content::GroupProof>,
    /// Deterministic source-reader input snapshots, validated again by the next preflight.
    #[serde(default)]
    pub input_items: Vec<aterm_verify::input_content::Proof>,
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

/// `<release dir>/.ubd/verified`.
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

/// Why a preflight selection cannot run: a standing failure or unreadable evidence.
#[derive(Debug)]
pub enum InputError {
    Failed(String),
    Unavailable(String),
}

/// Missing preflight rows, or the distinction between a recorded red and unreadable evidence.
pub fn input_missing(root: &Path, tree: &str, seal: &str) -> Result<Vec<String>, InputError> {
    let mut missing = Vec::new();
    for name in std::iter::once(RUN_LEVEL).chain(
        aterm_verify::release_input::CHECKS
            .into_iter()
            .map(Check::name),
    ) {
        match read(&path(root, tree, seal, name, None)).map_err(InputError::Unavailable)? {
            Read::Record(record) if answers(&record, name, tree, seal, None) => {
                if [FAIL, BEGAN].contains(&record.outcome.as_str()) {
                    return Err(InputError::Failed(format!(
                        "{name}: {} (recorded in {})",
                        record.outcome, record.log
                    )));
                }
                if name != RUN_LEVEL && record.outcome != OK {
                    missing.push(name.to_owned());
                }
            }
            _ if name != RUN_LEVEL => missing.push(name.to_owned()),
            _ => {}
        }
    }
    Ok(missing)
}

/// The runs whose passing records answer the release-input checks of `tree` under `seal`, each
/// once, in name order: what a run that reuses them says it carried.
pub fn input_judged_by(root: &Path, tree: &str, seal: &str) -> Vec<String> {
    let mut runs: Vec<String> = aterm_verify::release_input::CHECKS
        .into_iter()
        .map(Check::name)
        .filter_map(|name| match read(&path(root, tree, seal, name, None)) {
            Ok(Read::Record(record))
                if answers(&record, name, tree, seal, None) && record.outcome == OK =>
            {
                Some(record.run)
            }
            _ => None,
        })
        .collect();
    runs.sort();
    runs.dedup();
    runs
}

/// Successful source items from a completed A19 check; every reader validates its own key.
pub fn input_item_candidates(
    root: &Path,
    tree: &str,
    seal: &str,
) -> Result<Vec<aterm_verify::input_content::Proof>, String> {
    let mut items = Vec::new();
    for check in aterm_verify::release_input::CHECKS {
        if let Read::Record(record) = read(&path(root, tree, seal, check.name(), None))?
            && answers(&record, check.name(), tree, seal, None)
            && record.outcome == OK
        {
            items.extend(record.input_items);
        }
    }
    Ok(items)
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

/// The CLI half of `verify release` one run judges: R's tree ([`RELEASE_TREE`], step 4's tree
/// branch beside the build), or [`RELEASE_BUNDLE`]. AppKit retains the latter CLI context but
/// runs first in step 4 because it reads only R's tree; step 5 runs the remaining check that
/// reads the built bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    Tree,
    Bundle,
}

impl Half {
    /// Its checks, in [`RELEASE`] order.
    #[must_use]
    pub const fn checks(self) -> &'static [Check] {
        match self {
            Self::Tree => &RELEASE_TREE,
            Self::Bundle => &RELEASE_BUNDLE,
        }
    }

    /// The flag that narrows `verify release` to it.
    #[must_use]
    pub const fn flag(self) -> &'static str {
        match self {
            Self::Tree => "--tree",
            Self::Bundle => "--bundle",
        }
    }
}

/// What one `verify release` of a [`Half`] runs: its checks the records lack, in [`RELEASE`]
/// order, plus the test binaries and lint packages whose prior bounded OKs are candidates. The
/// verifier recomputes every candidate's complete key before skipping it.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifyRun {
    pub half: Half,
    pub checks: Vec<String>,
    pub passed_binaries: Vec<Binary>,
    pub passed_lint_packages: Vec<LintPackage>,
    pub passed_appkit_groups: Vec<aterm_verify::appkit_content::GroupProof>,
}

impl VerifyRun {
    /// Whether this is fewer than every check of its half: its run then names them
    /// (`--checks a,b`).
    #[must_use]
    pub fn strict_subset(&self) -> bool {
        let half = self.half.checks();
        self.checks.len() != half.len()
            || self
                .checks
                .iter()
                .zip(half.iter())
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

fn lint_packages(n: usize) -> String {
    if n == 1 {
        "1 lint package".to_owned()
    } else {
        format!("{n} lint packages")
    }
}

/// The judge's answer over one tree's records.
#[derive(Debug, Clone, Default, PartialEq)]
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
    pub resume: Vec<Binary>,
    /// Bounded lint-package OKs which the verifier must re-key before carrying.
    pub lint_resume: Vec<LintPackage>,
    /// AppKit executable groups which the current verifier must rebuild and re-key before reuse.
    pub appkit_resume: Vec<aterm_verify::appkit_content::GroupProof>,
}

/// One check's state, as its record left it.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Passed,
    Accepted {
        reason: String,
        log: String,
    },
    Failed {
        log: String,
    },
    /// No passing record (NOT RUN, skip, absent, unusable), and its fine-grained successful
    /// candidates.
    Missing {
        resume: Vec<Binary>,
        lint_resume: Vec<LintPackage>,
        appkit_resume: Vec<aterm_verify::appkit_content::GroupProof>,
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

    /// A passing answer says what its records hold that no run of this tree executed
    /// ([`carried_note`]): each was proved by content equal to what a run did execute, and the
    /// sentence names it rather than letting "every check's record is ok" stand for a run.
    #[must_use]
    fn carrying(mut self, carried: &[String]) -> Self {
        if self.answer.yes && !carried.is_empty() {
            use std::fmt::Write as _;
            let _ = write!(
                self.answer.words,
                "; carried on equal content, not run on this tree: {}",
                carried.join("; ")
            );
            if let Some(accepted) = &mut self.accepted {
                accepted.clone_from(&self.answer.words);
            }
        }
        self
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
                State::Missing {
                    resume,
                    lint_resume,
                    appkit_resume,
                } => {
                    if check == Check::Test {
                        checks.resume = resume;
                    }
                    if check == Check::Lint {
                        checks.lint_resume = lint_resume;
                    }
                    if check == Check::Appkit {
                        checks.appkit_resume = appkit_resume;
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
            if !checks.lint_resume.is_empty() {
                use std::fmt::Write as _;
                let _ = write!(
                    words,
                    "; lint's record carries {}",
                    lint_packages(checks.lint_resume.len())
                );
            }
            if !checks.appkit_resume.is_empty() {
                use std::fmt::Write as _;
                let _ = write!(
                    words,
                    "; appkit's record carries {} executable group(s)",
                    checks.appkit_resume.len()
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

    /// What a run of `half` runs: its missing checks, with the test binaries and lint packages
    /// eligible to be re-keyed. Empty when every check of `half` has a passing record.
    #[must_use]
    pub fn run(&self, half: Half) -> VerifyRun {
        self.run_only(half, half.checks())
    }

    /// What a run of `half` runs, narrowed to `only`. This retains the half's canonical CLI
    /// context while allowing the cutter to schedule one independent check before its siblings.
    #[must_use]
    pub fn run_only(&self, half: Half, only: &[Check]) -> VerifyRun {
        let checks: Vec<String> = self
            .missing
            .iter()
            .filter(|m| {
                half.checks().iter().any(|c| c.name() == m.as_str())
                    && only.iter().any(|c| c.name() == m.as_str())
            })
            .cloned()
            .collect();
        let test = checks.iter().any(|m| m == "test");
        let appkit = checks.iter().any(|m| m == "appkit");
        VerifyRun {
            half,
            checks,
            passed_binaries: if test {
                self.resume.clone()
            } else {
                Vec::new()
            },
            passed_lint_packages: if self.missing.iter().any(|m| m == "lint") && half == Half::Tree
            {
                self.lint_resume.clone()
            } else {
                Vec::new()
            },
            passed_appkit_groups: if appkit {
                self.appkit_resume.clone()
            } else {
                Vec::new()
            },
        }
    }

    /// The FAIL rule: a FAIL record refuses R's tree under this seal, at once, exit 1; its remedy
    /// carries a landed fix onto the line whose head is `head` ([`crate::release::fix_remedy`]),
    /// and it is the red of `verify release`'s failed checks ([`Refusal::failing`]).
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
            }))
            .failing(format!("verify release: {}", names.join(", "))),
        )
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// The tree of `commit`.
pub(crate) fn tree_of(git: &dyn GitRunner, commit: &str) -> Result<String, String> {
    crate::ledger::rev_parse(git, &format!("{commit}^{{tree}}")).map_err(|e| e.to_string())
}

/// A failed test executable proves the compile finished. Keep an already running product
/// build only when that is the tree's sole kind of red; missing evidence keeps the stop.
pub fn keep_build_on_test_failure(
    git: &dyn GitRunner,
    commit: &str,
    seal: &str,
) -> Result<bool, String> {
    let root = root(git)?;
    let tree = tree_of(git, commit)?;
    keep_build_for_records(&root, commit, &tree, seal)
}

fn keep_build_for_records(
    root: &Path,
    commit: &str,
    tree: &str,
    seal: &str,
) -> Result<bool, String> {
    let checks = checks_in(root, commit, tree, None, || Ok(seal.to_owned()))?;
    if checks.failed.is_empty() || checks.failed.iter().any(|f| f.check != "test") {
        return Ok(false);
    }
    let Read::Record(record) = read(&path(root, tree, seal, "test", None))? else {
        return Ok(false);
    };
    Ok(record.outcome == FAIL
        && answers(&record, "test", tree, seal, None)
        && record.binaries.iter().any(|b| b.outcome == FAIL))
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
    let root = root(git)?;
    let mut current_seal = None;
    let mut checks = checks_in(&root, commit, &tree, executable, || {
        let value = seal()?;
        current_seal = Some(value.clone());
        Ok(value)
    })?;
    if checks.missing.iter().any(|check| check == "lint") {
        // This adds candidates, never a passing aggregate verdict. Current-tree rows win,
        // including failures and unbounded tombstones. The verifier alone proves equality.
        if let Ok(candidates) = ancestor_lint_candidates(git, &root, commit, &tree) {
            let mut current: BTreeSet<_> =
                checks.lint_resume.iter().map(|p| p.spec.clone()).collect();
            if let Some(seal) = current_seal.as_deref()
                && let Read::Record(record) = read(&path(&root, &tree, seal, "lint", None))?
                && answers(&record, "lint", &tree, seal, None)
            {
                current.extend(record.lint_packages.iter().map(|p| p.spec.clone()));
            }
            checks.lint_resume.extend(
                candidates
                    .into_iter()
                    .filter(|p| !current.contains(&p.spec)),
            );
        }
    }
    if checks.missing.iter().any(|check| check == "test") {
        // As for lint: candidates only, and every row this tree already has wins, a FAIL, a
        // NOT RUN and an unbounded row included. The verifier recomputes each offered row's
        // whole content key on this tree before it carries one.
        if let Ok(candidates) = ancestor_test_candidates(git, &root, commit, &tree) {
            let mut current: BTreeSet<_> = checks.resume.iter().map(|b| b.spec.clone()).collect();
            if let Some(seal) = current_seal.as_deref()
                && let Read::Record(record) = read(&path(&root, &tree, seal, "test", None))?
                && answers(&record, "test", &tree, seal, None)
            {
                current.extend(record.binaries.iter().map(|b| b.spec.clone()));
            }
            checks.resume.extend(
                candidates
                    .into_iter()
                    .filter(|b| !current.contains(&b.spec)),
            );
        }
    }
    Ok(checks)
}

/// The records of `check` that runs of `commit`'s ancestors left, nearest ancestor first, each
/// under whatever seal it ran: bounded local history searched for qualification candidates
/// across hops and release cuts. A record made only of candidates was keyed on no tree and is
/// skipped. An absent ancestor or unreadable record supplies no fact; nothing here changes a
/// tree's verdict.
fn ancestor_records(
    git: &dyn GitRunner,
    root: &Path,
    commit: &str,
    current_tree: &str,
    check: &str,
) -> Result<Vec<CheckRecord>, String> {
    let history = git
        .git_read(&[
            "log",
            "--date-order",
            "--max-count=4096",
            "--format=%T",
            &format!("{commit}^"),
        ])
        .map_err(|e| e.to_string())?;
    if !history.success() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for tree in history.stdout_utf8().lines() {
        if tree == current_tree || !aterm_codec::hex::is_lower(tree, 40) {
            continue;
        }
        let directory = root.join(tree);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        let mut paths = entries
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        paths.sort();
        for path in paths {
            let Read::Record(record) = read(&path.join(format!("{check}.toml")))? else {
                continue;
            };
            if record.tree != tree
                || !answers(&record, check, tree, &record.seal, None)
                || record.run.starts_with(CANDIDATE_RUN_PREFIX)
            {
                continue;
            }
            records.push(*record);
        }
    }
    Ok(records)
}

fn ancestor_lint_candidates(
    git: &dyn GitRunner,
    root: &Path,
    commit: &str,
    current_tree: &str,
) -> Result<Vec<LintPackage>, String> {
    let mut candidates = BTreeMap::new();
    for record in ancestor_records(git, root, commit, current_tree, "lint")? {
        for package in record.lint_packages {
            // The first observed row owns this spec even if it is not reusable: do not
            // resurrect an older success over a newer failure or unbounded reader.
            candidates.entry(package.spec.clone()).or_insert(package);
        }
    }
    Ok(candidates
        .into_values()
        .filter(LintPackage::carried)
        .collect())
}

/// The test binaries an ancestor's run passed, each under the run that executed it. The nearest
/// ancestor's row owns its spec, so a newer FAIL or unbounded row is never passed over for an
/// older success.
fn ancestor_test_candidates(
    git: &dyn GitRunner,
    root: &Path,
    commit: &str,
    current_tree: &str,
) -> Result<Vec<Binary>, String> {
    let mut candidates = BTreeMap::new();
    for record in ancestor_records(git, root, commit, current_tree, "test")? {
        for binary in executed_in(&record.binaries, &record.run) {
            candidates.entry(binary.spec.clone()).or_insert(binary);
        }
    }
    Ok(candidates.into_values().filter(Binary::carried).collect())
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
    let missing = |_: Check| State::Missing {
        resume: Vec::new(),
        lint_resume: Vec::new(),
        appkit_resume: Vec::new(),
    };
    if !has_records(root, tree)? {
        return Ok(Checks::of(commit, tree, executable, missing, Vec::new()));
    }
    let seal = seal()?;
    let seal_path = root.join(tree).join(seal_dir(&seal));
    let mut states: BTreeMap<&'static str, State> = BTreeMap::new();
    let mut carried = Vec::new();
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
            OK => {
                carried.extend(carried_note(&record));
                State::Passed
            }
            ACCEPTED if bundle(check) => match record.acceptance {
                Some(a) => State::Accepted {
                    reason: a.reason,
                    log: record.log,
                },
                None => continue,
            },
            FAIL => State::Failed { log: record.log },
            _ => State::Missing {
                resume: executed_in(&record.binaries, &record.run)
                    .into_iter()
                    .filter(Binary::carried)
                    .collect(),
                lint_resume: record
                    .lint_packages
                    .into_iter()
                    .filter(LintPackage::carried)
                    .collect(),
                appkit_resume: appkit_group_candidates(record.appkit_groups),
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
            states.remove(check.name()).unwrap_or(State::Missing {
                resume: Vec::new(),
                lint_resume: Vec::new(),
                appkit_resume: Vec::new(),
            })
        },
        also,
    )
    .carrying(&carried))
}

/// What of an `ok` record no run of its own tree executed: the whole check where the hop
/// carry copied it ([`carry_hop`]), the test binaries another run executed, the lint packages
/// reused. `None` for a record whose every row ran in its own run.
fn carried_note(record: &CheckRecord) -> Option<String> {
    if let Some(head) = record.run.strip_prefix(CARRIED_RUN_PREFIX) {
        return Some(format!("{} whole from line head {head}", record.check));
    }
    let elsewhere: Vec<&str> = record
        .binaries
        .iter()
        .filter(|row| row.outcome == OK)
        .filter_map(|row| row.origin_run.as_deref())
        .filter(|run| *run != record.run)
        .collect();
    if !elsewhere.is_empty() {
        let runs: std::collections::BTreeSet<&str> = elsewhere.iter().copied().collect();
        return Some(format!(
            "{} of {} of test from {}",
            elsewhere.len(),
            binaries(record.binaries.len()),
            runs.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    let reused = record.lint_packages.iter().filter(|row| row.reused).count();
    (reused != 0).then(|| {
        format!(
            "{reused} of {} of lint",
            lint_packages(record.lint_packages.len())
        )
    })
}

/// What content seeding from the line's immediately preceding head established.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HopCarry {
    /// Checks copied whole because their declared input keys are equal.
    pub carried_checks: Vec<String>,
    /// Checks for which no equal successful content record stood.
    pub rerun_checks: Vec<String>,
    /// Stable report lines, including why unbounded readers rerun.
    pub lines: Vec<String>,
}

/// Seed `tree` only from `previous_tree`, the line's immediately preceding head.  The old
/// tree-local records are never moved or reinterpreted: an equal successful content record is
/// copied into a new authoritative OK record whose key names `tree`.  A prior FAIL is never
/// copied.  Its successful test-binary rows may be offered as candidates, but the verifier must
/// recompute each binary's full key before carrying it.
///
/// # Errors
/// A record cannot be read or a synthetic current-tree record cannot be kept durably.
pub fn carry_hop(
    root: &Path,
    commit: &str,
    tree: &str,
    previous_commit: &str,
    previous_tree: &str,
    seal: &str,
    plan: &crate::check_content::Plan,
) -> Result<HopCarry, String> {
    let mut result = HopCarry::default();
    if tree == previous_tree || !has_records(root, previous_tree)? {
        result.rerun_checks = hop_checks().map(|check| check.name().to_owned()).collect();
        result
            .lines
            .push("no record of the immediately preceding hop can be carried".into());
        return Ok(result);
    }

    let previous_seal = root.join(previous_tree).join(seal_dir(seal));
    let current_seal = root.join(tree).join(seal_dir(seal));
    for check in hop_checks() {
        let name = check.name();
        let current_at = path_in_seal_dir(&current_seal, name, None);
        let current = match read(&current_at)? {
            Read::Record(held) if answers(&held, name, tree, seal, None) => Some(held),
            _ => None,
        };
        if current
            .as_ref()
            .is_some_and(|held| matches!(held.outcome.as_str(), OK | FAIL | ACCEPTED))
        {
            continue;
        }
        let input = plan.input(name);
        let previous = match read(&path_in_seal_dir(&previous_seal, name, None))? {
            Read::Record(record) if answers(&record, name, previous_tree, seal, None) => {
                Some(record)
            }
            _ => None,
        };

        let equal_ok = previous.as_ref().is_some_and(|record| {
            check != Check::Appkit
                && record.outcome == OK
                && input.bounded()
                && record
                    .content
                    .as_ref()
                    .is_some_and(|old| old.key == input.key)
        });
        if equal_ok {
            let mut carried = (*previous.expect("equal record")).clone();
            carried.key = key(name, tree, seal, None);
            carried.tree = tree.to_owned();
            carried.commit = commit.to_owned();
            carried.content = Some(input);
            carried.outcome = OK.into();
            carried.seconds = 0.0;
            carried.detail = format!(
                "carried from immediately preceding line head {}: declared content is equal",
                short(previous_commit)
            );
            carried.rows.clear();
            carried.run = format!("{CARRIED_RUN_PREFIX}{}", short(previous_commit));
            carried.exit = 0;
            carried.acceptance = None;
            if keep(&current_at, &carried)? {
                result.carried_checks.push(name.to_owned());
            }
            result.lines.push(format!(
                "{name}: carried from {} (content {})",
                short(previous_commit),
                carried
                    .content
                    .as_ref()
                    .and_then(|content| content.key.as_deref())
                    .map(short)
                    .unwrap_or("undeclared")
            ));
            continue;
        }

        result.rerun_checks.push(name.to_owned());
        let prior_context = previous
            .as_ref()
            .and_then(|record| record.content.as_ref())
            .map(|content| content.context.as_str());
        let reason = match (&input.key, previous.as_ref()) {
            (_, Some(_))
                if check == Check::Appkit && prior_context != Some(input.context.as_str()) =>
            {
                "the verifier, harness, environment or toolchain identity changed".into()
            }
            (None, _) => input.declaration.clone(),
            (Some(_), None) => "the preceding hop has no usable record".into(),
            (Some(_), Some(record)) if record.outcome != OK => {
                format!("the preceding hop ended {}", record.outcome)
            }
            (Some(_), Some(record)) if record.content.is_none() => {
                "the preceding record has no declared content key".into()
            }
            (Some(_), Some(_)) if prior_context != Some(input.context.as_str()) => {
                "the verifier, harness, environment or toolchain identity changed".into()
            }
            _ => "declared content changed".into(),
        };
        result.lines.push(format!("{name}: rerun ({reason})"));

        // AppKit is never copied as a whole-check verdict. An immediately preceding OK may offer
        // its canonical executable-group proofs only when the canonical bundle-half context is
        // unchanged. The current verifier rebuilds each exact group, re-hashes it, and alone may
        // turn these candidates into a current-tree OK. Candidate records cannot relay again.
        if check == Check::Appkit
            && let Some(previous) = previous.clone()
        {
            let same_context = previous
                .content
                .as_ref()
                .is_some_and(|held| held.context == input.context);
            let rows_rekeyed_here = !previous.run.starts_with(CANDIDATE_RUN_PREFIX);
            let previous_candidates = if previous.outcome == OK && same_context && rows_rekeyed_here
            {
                appkit_group_candidates(previous.appkit_groups)
            } else {
                Vec::new()
            };
            let current_groups = appkit_group_candidates(
                current
                    .clone()
                    .into_iter()
                    .flat_map(|record| record.appkit_groups),
            );
            let mut groups: BTreeMap<String, aterm_verify::appkit_content::GroupProof> =
                previous_candidates
                    .into_iter()
                    .map(|proof| (proof.group.clone(), proof))
                    .collect();
            for proof in current_groups {
                groups.insert(proof.group.clone(), proof);
            }
            if !groups.is_empty() {
                let candidate_count = groups.len();
                let candidate = CheckRecord {
                    schema: SCHEMA,
                    check: name.to_owned(),
                    key: key(name, tree, seal, None),
                    tree: tree.to_owned(),
                    commit: commit.to_owned(),
                    seal: seal.to_owned(),
                    content: Some(input.clone()),
                    executable: None,
                    outcome: NOT_RUN.into(),
                    seconds: 0.0,
                    detail: format!(
                        "{candidate_count} successful AppKit executable group(s) from {} are candidates; each current group is rebuilt and re-keyed",
                        short(previous_commit)
                    ),
                    rows: Vec::new(),
                    log: previous.log,
                    log_sha256: previous.log_sha256,
                    run: format!("{CANDIDATE_RUN_PREFIX}{}", short(previous_commit)),
                    exit: 3,
                    acceptance: None,
                    binaries: Vec::new(),
                    lint_packages: Vec::new(),
                    appkit_groups: groups.into_values().collect(),
                    input_items: Vec::new(),
                };
                let _ = keep(&current_at, &candidate)?;
            }
        }

        // The check itself is not carried. Keep successful binary candidates plus any current
        // tombstones under a NOT RUN current-tree record. The ordinary resume wire supplies only
        // the successes, whose executable, environment and runtime-input keys verify recomputes.
        if check == Check::Test
            && let Some(previous) = previous.clone()
        {
            let rows_rekeyed_here = !previous.run.starts_with(CANDIDATE_RUN_PREFIX);
            let previous_candidates = executed_in(&previous.binaries, &previous.run)
                .into_iter()
                .filter(move |binary| rows_rekeyed_here && binary.carried());
            let current_binaries = current
                .clone()
                .into_iter()
                .flat_map(|record| record.binaries);
            let mut rows: BTreeMap<String, Binary> = previous_candidates
                .map(|binary| (binary.spec.clone(), binary))
                .collect();
            // A prior facts pass may already have judged binaries on this hop. Every current row
            // wins. Keep NOT RUN, FAIL and unbounded rows as tombstones: the ordinary resume wire
            // filters them, while a second facts pass still sees them and cannot resurrect an
            // older preceding-hop OK for the same spec.
            for binary in current_binaries {
                rows.insert(binary.spec.clone(), binary);
            }
            let candidate_count = rows.values().filter(|binary| binary.carried()).count();
            if candidate_count != 0 {
                let candidates: Vec<Binary> = rows.into_values().collect();
                let mut candidate = CheckRecord {
                    schema: SCHEMA,
                    check: name.to_owned(),
                    key: key(name, tree, seal, None),
                    tree: tree.to_owned(),
                    commit: commit.to_owned(),
                    seal: seal.to_owned(),
                    content: Some(input.clone()),
                    executable: None,
                    outcome: NOT_RUN.into(),
                    seconds: 0.0,
                    detail: format!(
                        "{} successful binaries from {} are candidates; each full content key is recomputed",
                        candidate_count,
                        short(previous_commit)
                    ),
                    rows: Vec::new(),
                    log: previous.log,
                    log_sha256: previous.log_sha256,
                    run: format!("{CANDIDATE_RUN_PREFIX}{}", short(previous_commit)),
                    exit: 3,
                    acceptance: None,
                    binaries: candidates,
                    lint_packages: Vec::new(),
                    appkit_groups: Vec::new(),
                    input_items: Vec::new(),
                };
                candidate.binaries.sort_by(|a, b| a.spec.cmp(&b.spec));
                let _ = keep(&current_at, &candidate)?;
            }
        }
        // Lint's aggregate verdict is deliberately not carried: its compiler closure is split
        // per workspace package. Offer only bounded package OKs to the next hop. Current rows,
        // including FAIL and unbounded rows, replace older candidates as tombstones so repeated
        // facts passes cannot resurrect evidence this hop already rejected.
        if check == Check::Lint
            && let Some(previous) = previous
        {
            let rows_rekeyed_here = !previous.run.starts_with(CANDIDATE_RUN_PREFIX);
            let previous_candidates = previous
                .lint_packages
                .into_iter()
                .filter(move |package| rows_rekeyed_here && package.carried());
            let current_packages = current.into_iter().flat_map(|record| record.lint_packages);
            let mut rows: BTreeMap<String, LintPackage> = previous_candidates
                .map(|package| (package.spec.clone(), package))
                .collect();
            for package in current_packages {
                rows.insert(package.spec.clone(), package);
            }
            let candidate_count = rows.values().filter(|package| package.carried()).count();
            if candidate_count != 0 {
                let mut packages: Vec<LintPackage> = rows.into_values().collect();
                packages.sort_by(|a, b| a.spec.cmp(&b.spec));
                let candidate = CheckRecord {
                    schema: SCHEMA,
                    check: name.to_owned(),
                    key: key(name, tree, seal, None),
                    tree: tree.to_owned(),
                    commit: commit.to_owned(),
                    seal: seal.to_owned(),
                    content: Some(input),
                    executable: None,
                    outcome: NOT_RUN.into(),
                    seconds: 0.0,
                    detail: format!(
                        "{candidate_count} successful lint packages from {} are candidates; each complete compile-time content key is recomputed",
                        short(previous_commit)
                    ),
                    rows: Vec::new(),
                    log: previous.log,
                    log_sha256: previous.log_sha256,
                    run: format!("{CANDIDATE_RUN_PREFIX}{}", short(previous_commit)),
                    exit: 3,
                    acceptance: None,
                    binaries: Vec::new(),
                    lint_packages: packages,
                    appkit_groups: Vec::new(),
                    input_items: Vec::new(),
                };
                let _ = keep(&current_at, &candidate)?;
            }
        }
    }
    result.carried_checks.sort();
    result.rerun_checks.sort();
    Ok(result)
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
    write_within(at, record, MAX_RECORD_BYTES)
}

/// [`write`] under `max`: a record [`read`] could not read back is never written.
fn write_within(at: &Path, record: &CheckRecord, max: u64) -> Result<(), String> {
    let text =
        aterm_toml::to_string(record).map_err(|e| format!("serialize {}: {e}", at.display()))?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > max {
        return Err(format!(
            "write {}: the record is {} bytes, over the {max}-byte bound its reader holds",
            at.display(),
            text.len()
        ));
    }
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
    /// The verifier image captured before compilation, distinct from the next on-disk image.
    pub running_driver: Option<String>,
    /// Linked measuring executables reported by the compiler, never inferred from source paths.
    pub harnesses: BTreeMap<String, String>,
    /// Each `check-finished` event, in order.
    pub finished: Vec<aterm_json::Value>,
    /// Each test binary's last `test-binary-finished` event: its spec, digest and outcome.
    pub binaries: Vec<Binary>,
    /// Each workspace package's last `lint-package-finished` event.
    pub lint_packages: Vec<LintPackage>,
    /// Each canonical AppKit executable group's last successful proof event.
    pub appkit_groups: Vec<aterm_verify::appkit_content::GroupProof>,
    pub input_items: Vec<aterm_verify::input_content::Proof>,
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
    let mut lint_packages: Vec<LintPackage> = Vec::new();
    let mut input_items = aterm_verify::input_content::Collector::default();
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
        match input_items.observe(&event) {
            Ok(Some(proof)) => {
                run.input_items
                    .retain(|item| item.check != proof.check || item.item != proof.item);
                run.input_items.push(proof);
            }
            Err(_) => run.garbled += 1,
            Ok(None) => {}
        }
        match event["event"].as_str() {
            Some("measure-driver") => {
                let digest = event["sha256"]
                    .as_str()
                    .filter(|d| aterm_codec::hex::is_lower(d, 64))
                    .unwrap_or_default();
                run.running_driver = Some(match run.running_driver.as_deref() {
                    None => digest.to_owned(),
                    Some(held) if held == digest => held.to_owned(),
                    Some(_) => String::new(),
                });
            }
            Some("compile-unit-finished") => {
                if let Some(name) = event["target"]["name"].as_str()
                    && MEASURING_TARGETS.contains(&name)
                    && event["manifest_path"]
                        .as_str()
                        .is_some_and(|p| p.ends_with("/crates/aterm-conformance/Cargo.toml"))
                    && event["target"]["kind"]
                        .as_array()
                        .is_some_and(|k| k.len() == 1 && k[0].as_str() == Some("bench"))
                {
                    let digest = event["measure_harness_sha256"]
                        .as_str()
                        .filter(|d| aterm_codec::hex::is_lower(d, 64))
                        .unwrap_or_default();
                    run.harnesses.insert(name.to_owned(), digest.to_owned());
                }
            }
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
                let binary = Binary {
                    spec: spec.to_owned(),
                    sha256: sha256.to_owned(),
                    outcome: outcome.to_owned(),
                    content_key: event["content_key"].as_str().map(str::to_owned),
                    inputs_bounded: event["inputs_bounded"].as_bool().unwrap_or(false),
                    input_reason: event["input_reason"].as_str().map(str::to_owned),
                    json: event["json"].as_bool(),
                    execution_s: event["recorded_execution_s"]
                        .as_f64()
                        .or_else(|| event["execution_s"].as_f64())
                        .unwrap_or(0.0),
                    key_parts: event["key_parts"]
                        .as_object()
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|(name, digest)| {
                                    Some((name.clone(), digest.as_str()?.to_owned()))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    origin_run: event["carried_from"].as_str().map(str::to_owned),
                };
                if let Some(pos) = binaries.iter().position(|b| b.spec == spec) {
                    binaries.remove(pos);
                    binaries.push(binary);
                } else {
                    binaries.push(binary);
                }
            }
            Some("lint-package-finished") => {
                let (Some(spec), Some(outcome)) =
                    (event["spec"].as_str(), event["outcome"].as_str())
                else {
                    return;
                };
                let content_key = event["content_key"].as_str().map(str::to_owned);
                let claimed_bounded = event["inputs_bounded"].as_bool().unwrap_or(false);
                let canonical_key = content_key
                    .as_deref()
                    .is_some_and(|key| aterm_codec::hex::is_lower(key, 64));
                let inputs_bounded = claimed_bounded && canonical_key;
                let input_reason = if claimed_bounded && !canonical_key {
                    Some("lint package reported a non-canonical content key".to_owned())
                } else {
                    event["input_reason"].as_str().map(str::to_owned)
                };
                let package = LintPackage {
                    spec: spec.to_owned(),
                    outcome: outcome.to_owned(),
                    content_key,
                    inputs_bounded,
                    input_reason,
                    execution_s: event["execution_s"].as_f64().unwrap_or(0.0),
                    reused: event["carried"].as_bool().unwrap_or(false),
                };
                if let Some(pos) = lint_packages.iter().position(|row| row.spec == spec) {
                    lint_packages.remove(pos);
                }
                lint_packages.push(package);
            }
            Some("appkit-group-finished") => {
                let proof = (|| {
                    let _reused = event["reused"].as_bool()?;
                    let group = event["group"].as_str()?.to_owned();
                    let content_key = event["content_key"].as_str()?.to_owned();
                    let executables = aterm_json::from_value(event["executables"].clone()).ok()?;
                    let proof = aterm_verify::appkit_content::GroupProof {
                        schema: aterm_verify::appkit_content::SCHEMA,
                        group,
                        content_key,
                        executables,
                    };
                    appkit_group_carried(&proof).then_some(proof)
                })();
                match proof {
                    Some(proof) => {
                        run.appkit_groups.retain(|held| held.group != proof.group);
                        run.appkit_groups.push(proof);
                    }
                    None => run.garbled += 1,
                }
            }
            _ => {}
        }
    })?;
    run.partial = partial;
    run.binaries = binaries;
    run.lint_packages = lint_packages;
    run.appkit_groups
        .sort_by(|left, right| left.group.cmp(&right.group));
    Ok(run)
}

/// Keep the linked measuring harnesses under their tree and seal. Missing or partial evidence
/// never supplies a cross-tree measure key; an existing same-tree receipt still stands.
pub fn keep_harnesses(
    root: &Path,
    tree: &str,
    seal: &str,
    events: &RunEvents,
) -> Result<(), String> {
    let at = root
        .join(tree)
        .join(seal_dir(seal))
        .join("measure-harnesses.toml");
    if !events.whole() {
        return remove(&at);
    }
    let mut harnesses = events.harnesses.clone();
    if events
        .finished
        .iter()
        .any(|e| e["check"].as_str() == Some("measure"))
        && harnesses.keys().all(|name| name == "verify")
    {
        harnesses = match crate::durable::read_regular_text(&at, 4096) {
            Ok(text) => aterm_toml::from_str(&text).unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("read measuring harnesses: {e}")),
        };
    }
    if !harnesses.is_empty() {
        harnesses.insert(
            "verify".into(),
            events.running_driver.clone().unwrap_or_default(),
        );
        let text = aterm_toml::to_string(&harnesses).map_err(|e| e.to_string())?;
        crate::durable::write_atomic_durable(&at, text.as_bytes(), 0o600)
            .map_err(|e| format!("write harness evidence: {e}"))?;
    }
    Ok(())
}

/// The receipt namespace is H(U, four linked benches, verifier executable, seal). No tree check
/// crosses trees, and historical check records and release journals retain their format.
pub fn product_key(
    root: &Path,
    tree: &str,
    seal: &str,
    content: &str,
) -> Result<Option<String>, String> {
    let at = root
        .join(tree)
        .join(seal_dir(seal))
        .join("measure-harnesses.toml");
    let text = match crate::durable::read_regular_text(&at, 4096) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read harness evidence: {e}")),
    };
    let Ok(harnesses) = aterm_toml::from_str::<BTreeMap<String, String>>(&text) else {
        return Ok(None);
    };
    if harnesses.len() != MEASURING_TARGETS.len() + 1
        || MEASURING_TARGETS.into_iter().chain(["verify"]).any(|name| {
            !harnesses
                .get(name)
                .is_some_and(|h| aterm_codec::hex::is_lower(h, 64))
        })
    {
        return Ok(None);
    }
    let mut h = aterm_digest::Sha256::new();
    for part in ["aterm-product-measure-v1", content, seal]
        .into_iter()
        .chain(harnesses.values().map(String::as_str))
    {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    Ok(Some(aterm_codec::hex::encode_lower(&h.finalize())))
}

/// Archive a measure's verdict by product and harness content. A FAIL remains authoritative.
pub fn remember_product(root: &Path, facts: &RunFacts<'_>, product: &str) -> Result<(), String> {
    if !facts.asked.iter().any(|c| c == "measure") {
        return Ok(());
    }
    let Some(executable) = facts.executable else {
        return Ok(());
    };
    let Read::Record(mut record) = read(&path(
        root,
        facts.tree,
        facts.seal,
        "measure",
        Some(executable),
    ))?
    else {
        return Ok(());
    };
    if record.commit != facts.commit
        || record.run != facts.run
        || record.log != facts.log.display().to_string()
        || !answers(&record, "measure", facts.tree, facts.seal, Some(executable))
        || ![OK, FAIL].contains(&record.outcome.as_str())
    {
        return Ok(());
    }
    record.key = product.to_owned();
    keep(
        &root.join("product-measure").join(format!("{product}.toml")),
        &record,
    )?;
    Ok(())
}

/// Install the exact product verdict as this tree's measure receipt; identity still has to
/// pass on its newly stamped bundle. A same-tree FAIL can never be overwritten by a pass.
pub fn reuse_product(root: &Path, facts: &RunFacts<'_>, product: &str) -> Result<bool, String> {
    let Some(executable) = facts.executable else {
        return Ok(false);
    };
    let Read::Record(mut record) =
        read(&root.join("product-measure").join(format!("{product}.toml")))?
    else {
        return Ok(false);
    };
    if record.check != "measure"
        || record.key != product
        || record.seal != facts.seal
        || ![OK, FAIL].contains(&record.outcome.as_str())
    {
        return Ok(false);
    }
    record.key = key("measure", facts.tree, facts.seal, Some(executable));
    record.tree = facts.tree.to_owned();
    record.executable = Some(executable.to_owned());
    let at = path(root, facts.tree, facts.seal, "measure", Some(executable));
    if let Read::Record(held) = read(&at)?
        && held.key == record.key
        && (held.outcome == record.outcome || [FAIL, ACCEPTED].contains(&held.outcome.as_str()))
    {
        return Ok(false);
    }
    keep(&at, &record)
}

/// One `verify release` run of R's tree under one seal: what every record it leaves states.
#[derive(Debug, Clone)]
pub struct RunFacts<'a> {
    pub commit: &'a str,
    pub tree: &'a str,
    pub seal: &'a str,
    /// U: the executable a bundle half judges; a tree half judges none.
    pub executable: Option<&'a str>,
    /// Its log, which every record names.
    pub log: &'a Path,
    /// The release run (its logs directory's name).
    pub run: &'a str,
    /// The checks it was asked for.
    pub asked: &'a [String],
    /// This tree's declared check-content identities.  A run without this evidence remains a
    /// valid tree-local verdict but can never cross to a later hop.
    pub content: Option<&'a crate::check_content::Plan>,
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
        content: facts.content.map(|plan| plan.input(check)),
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
        lint_packages: Vec::new(),
        appkit_groups: Vec::new(),
        input_items: Vec::new(),
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
        let mut lint_finished = false;
        for event in &events.finished {
            let (Some(name), Some(outcome)) = (event["check"].as_str(), event["outcome"].as_str())
            else {
                continue;
            };
            let Some(check) = RELEASE
                .into_iter()
                .chain(aterm_verify::release_input::CHECKS)
                .find(|c| c.name() == name)
            else {
                continue;
            };
            if !facts.asked.iter().any(|a| a == name)
                || ![OK, FAIL, NOT_RUN, SKIP].contains(&outcome)
            {
                continue;
            };
            // A bundle check's record names the executable it judged; with none, it judged none.
            let u = match (bundle(check), facts.executable) {
                (false, _) => None,
                (true, Some(u)) => Some(u),
                (true, None) => continue,
            };
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
                r.binaries = executed_in(&events.binaries, facts.run);
            }
            if check == Check::Lint {
                lint_finished = true;
                r.lint_packages.clone_from(&events.lint_packages);
                let reused = r.lint_packages.iter().filter(|p| p.reused).count();
                if !r.detail.is_empty() {
                    r.detail.push_str("; ");
                }
                r.detail.push_str(&format!(
                    "{reused} packages reused, {} judged",
                    r.lint_packages.len() - reused
                ));
            }
            if check == Check::Appkit && outcome == OK {
                r.appkit_groups.clone_from(&events.appkit_groups);
            }
            r.input_items = events
                .input_items
                .iter()
                .filter(|item| item.check == name)
                .cloned()
                .collect();
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
            r.binaries = executed_in(&events.binaries, facts.run);
            let said = format!("test {} (from its binaries)", r.outcome);
            records.push((path_in_seal_dir(&seal_path, "test", None), r, said));
        }
        if !lint_finished
            && facts.asked.iter().any(|a| a == "lint")
            && !events.lint_packages.is_empty()
        {
            let fail = events
                .lint_packages
                .iter()
                .any(|package| package.outcome == FAIL);
            let ok = events
                .lint_packages
                .iter()
                .filter(|package| package.outcome == OK)
                .count();
            let mut r = record(
                facts,
                ended,
                "lint",
                if fail { FAIL } else { NOT_RUN },
                None,
            );
            r.detail = format!(
                "the run ended before its lint check finished: {ok} of the {} package rows that ended passed{}",
                events.lint_packages.len(),
                if fail { ", and one failed" } else { "" }
            );
            r.lint_packages.clone_from(&events.lint_packages);
            let said = format!("lint {} (from its packages)", r.outcome);
            records.push((path_in_seal_dir(&seal_path, "lint", None), r, said));
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
        executable: Some(executable),
        log: &log,
        run: "test-run",
        asked: &asked,
        content: None,
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
#[path = "check_record_tests.rs"]
mod tests;

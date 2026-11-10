// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship release`'s step machine ([`crate::release`]) driven against a fake world.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::check_record::{Binary, Checks, Failed, Half, LintPackage, State, VerifyRun};
use crate::line::{Admitted, Fix, Hop, Line};
use crate::provision::{AskWords, Audited, Finding};
use crate::release::{
    AfterCut, Answer, Artifact, Backend, Branch, BranchStop, Branches, BuildWork, Built, Candidate,
    CutState, EngineNow, EngineRows, Facts, JOURNAL_FILE, Journal, Prover, PublishGate,
    RUNNING_CHILD_FILE, RUNNING_FILE, Ran, Refusal, ReleaseOptions, Reservation, SELECTION_FILE,
    Side, StageTarget, Step, run_with,
};
use crate::release_steps::Real;

mod qualify_model;
mod sign_model;

const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TIP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BUMP: &str = "cccccccccccccccccccccccccccccccccccccccc";
const NEXT: &str = "dddddddddddddddddddddddddddddddddddddddd";
const BUMP2: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
/// Landed, non-merge fixes on main, after the line's base.
const FIX: &str = "1111111111111111111111111111111111111111";
const FIX2: &str = "2222222222222222222222222222222222222222";
/// Main's movement past the base that is no fix of the line's.
const N2: &str = "3333333333333333333333333333333333333333";
const N3: &str = "4444444444444444444444444444444444444444";

fn passed_binary(spec: &str, executable: &str) -> Binary {
    Binary {
        spec: spec.into(),
        sha256: executable.into(),
        outcome: "ok".into(),
        content_key: Some("cc".repeat(32)),
        inputs_bounded: true,
        input_reason: None,
        json: Some(false),
        execution_s: 1.25,
    }
}

fn passed_lint_package(spec: &str) -> LintPackage {
    LintPackage {
        spec: spec.into(),
        outcome: "ok".into(),
        content_key: Some("dd".repeat(32)),
        inputs_bounded: true,
        input_reason: None,
        execution_s: 0.75,
    }
}

/// Offline authority/control seam for the launch instrument. Stop at the boundary:
/// no qualify branch, signature, stage or publication is started. Real freezing,
/// bumping and sealing are timed separately by release_steps' opt-in fixture.
pub(crate) fn prequalify_fixture() -> std::time::Duration {
    let mut world = World::new();
    world.versions.insert(TIP.into(), "0.6.0".into());
    world.stop_at_qualify = true;
    let start = std::time::Instant::now();
    let result = world.run(ReleaseOptions {
        commit: Some(TIP.into()),
        qualify: true,
        ..mac()
    });
    assert_ne!(
        result.0, 0,
        "the fixture must stop at qualify, not finish its branches"
    );
    world
        .qualify_boundary
        .expect("the controller reached qualify")
        .duration_since(start)
}

#[test]
#[ignore = "explicit credential-free launch measurement"]
fn measure_bump_launch_to_qualify() {
    crate::release_steps::measure_bump_launch_to_qualify(prequalify_fixture);
}

#[test]
fn offline_launch_stops_at_the_qualification_boundary() {
    let _ = prequalify_fixture();
}

/// The hop the fake makes of `fix`.
fn hop_of(fix: &str) -> String {
    format!("9{}", &fix[1..])
}

struct World {
    input_refusal: Option<Refusal>,
    fleet_refusal: Option<String>,
    supersede_preflight_refusal: Option<Refusal>,
    supersede_unknown_once: bool,
    supersede_unknown_absent_once: bool,
    supersede_classify_unknown: bool,
    supersede_final_save_error: bool,
    prepare_clone_refusal: Option<Refusal>,
    missing_origin_tip: bool,
    fleet_selected: Option<Line>,
    existing_fleet_line: Option<Line>,
    supersession: Option<crate::line::Supersession>,
    fleet_audit_refusal: bool,
    fleet_audits: usize,
    dir: aterm_tempfile::TempDir,
    now: u64,
    out: Vec<String>,
    calls: Vec<String>,
    observations: Vec<&'static str>,
    pid: u32,
    started: String,
    alive: BTreeMap<u32, String>,
    /// A refusal before exec, or the successful exec edge (which a real backend never returns).
    exec_result: Result<(), Refusal>,
    /// R the last exec was asked to replace this image with.
    exec_commit: Option<String>,
    times: String,
    history: Vec<String>,
    versions: BTreeMap<String, String>,
    head: String,
    candidate_before_tip_move: bool,
    on_main: bool,
    origin_tags: BTreeMap<String, String>,
    ledger: Vec<String>,
    published: BTreeMap<String, String>,
    newest: Option<String>,
    cut: Option<CutState>,
    attested: Option<String>,
    /// Each check's record, by R's commit (its tree here), the executable for the bundle's
    /// `measure` (`None` for a tree check), and the check: its outcome word.
    records: BTreeMap<(String, Option<String>, String), &'static str>,
    /// The test binaries a test record that did not pass lists as passed.
    resume: Vec<Binary>,
    /// What the fake `verify release` records for a check it is asked to run, over
    /// `verify_passes` (every asked check ok, else nothing).
    verify_outcomes: BTreeMap<&'static str, &'static str>,
    /// What each `verify release` was asked to run.
    verify_runs: Vec<VerifyRun>,
    /// origin's ledger tail.
    tail: u64,
    /// The release cache: key → (its build number, its executable digest, its app).
    artifacts: BTreeMap<String, (u64, String, PathBuf)>,
    /// A peer's release claims past the reservation as the cut starts — after the
    /// release's last look, before the cut's own claim.
    overtaken_at_cut: bool,
    /// The cut refuses at a pre-claim gate, before any claim (and so before its own discard).
    cut_refuses_before_its_claim: bool,
    /// `pub publish` pushes, then dies before the engine records R's row.
    source_dies_after_push: bool,
    /// The cut's claim lands (main takes the release commit, the ledger its line) and the
    /// cut then fails before it journals anything.
    claim_lands_unjournaled: bool,
    machine_refusals: Vec<Refusal>,
    /// The numbered origin refresh that refuses, and its structured refusal.
    fetch_refusal: Option<(usize, Refusal)>,
    fetches: usize,
    validation_tools: Result<String, Refusal>,
    /// The preflight's own blocks: R's commit-only gates and the engine's readiness.
    blocks: Vec<Refusal>,
    audits: VecDeque<Vec<Finding>>,
    /// Each audit's `dry`, in order.
    audited_dry: Vec<bool>,
    /// The explicit profile handed to each audit, rather than an unrelated default.
    audited_profiles: Vec<Option<PathBuf>>,
    verify_code: i32,
    verify_passes: bool,
    stage_stages: bool,
    /// The publish records R before failing at sign-release.
    publish_code: i32,
    /// Commits whose source release carries the signed manifest pair.
    source_attested: BTreeSet<String>,
    cut_code: i32,
    cut_finishes: bool,
    /// The cut claims a build (else it refused before its claim).
    cut_claims: bool,
    /// The channel's `latest` still names the release before.
    latest_lags: bool,
    /// The stage child's backend panics (a bug in the release itself).
    stage_panics: bool,
    /// The shape check's answer.
    after_cut: Result<String, Refusal>,
    /// How `ship site --latest` ends once the shape passed, read as the real backend
    /// reads it (`release_steps::site_outcome`).
    site: Result<crate::site::Outcome, crate::site::Failure>,
    own_cutter: bool,
    stop_at_qualify: bool,
    qualify_boundary: Option<std::time::Instant>,
    cutter_recovery: crate::release_tools::Recovery,
    warming_active: bool,
    unrecorded_hops: BTreeSet<String>,
    asks: Vec<String>,
    resolved: Vec<String>,
    withdrawn: Vec<String>,
    /// How many asks the owner's aterm refuses before it takes one.
    undelivered: usize,
    /// Free GiB on the clone's disk.
    free: u64,
    /// What this machine's last releases wrote, as the release's record keeps it.
    disk_record: aterm_capacity::Footprints,
    /// The release's disk claim is refused with this.
    claim_refusal: Option<Refusal>,
    /// The cut's read-only pre-claim gates' refusals.
    cut_gate_refusals: Vec<Refusal>,
    /// The cut's gates of R's own tree, by R: their refusals.
    head_gate_refusals: BTreeMap<String, Vec<Refusal>>,
    /// Each R the cut's gates of R's own tree were asked of, in order.
    head_gated: Vec<String>,
    publisher_lease_held: bool,
    /// The receipt judges cannot answer.
    judge_down: bool,
    /// The engine sync after the stage fails.
    stage_sync_fails: bool,
    /// The signal a step's child was ended by, once one arrived.
    signal: Option<i32>,
    /// The step whose child a signal ends.
    signal_at: Option<&'static str>,
    /// What the world does once `verify release` ends.
    after_verify: Option<fn(&mut World)>,
    /// What the world does once a stage to the engine ends.
    after_stage: Option<fn(&mut World)>,
    /// What the world does while the release next sleeps (an owner wait).
    after_sleep: Option<fn(&mut World)>,
    /// Tags in this clone only (a leftover).
    local_tags: BTreeSet<String>,
    /// The real backend's spawn path and interrupt, for the verify step's child: that
    /// child is `/bin/sh -c <script> sh <dir>`, run as a step's child really is.
    real: Option<(Real, String, PathBuf)>,
    /// The build whose handoff fixtures the release cache holds staged.
    fixtures_staged: Option<u64>,
    /// origin/main holds the release's handoff fixtures.
    fixtures_on_main: bool,
    /// The in-tree generator refuses.
    generator_refuses: bool,
    /// The landing's push fails (not a lost race).
    landing_fails: bool,
    /// The prover the world resolves (TRUST_STAGE2_BIN, the journal's, the store's).
    prover: Prover,
    prover_error: Option<String>,
    lease_error: Option<String>,
    /// Why step 1's build-inputs seal cannot be taken.
    seal_error: Option<String>,
    /// The engine's code now, and the engine that staged the attestation in publish/.out.
    engine: String,
    staged_by: Option<String>,
    /// Whether the run claim was still on disk when the PREPARED line was said.
    running_at_prepared: Option<bool>,
    /// The stage leaves a step child's note behind, its process gone.
    child_note_at_stage: bool,
    /// Origin does not answer `git ls-remote`.
    ls_remote_down: bool,
    /// A peer's `pub stage aterm` moved the staging tier's main since R was attested; a
    /// stage of R attests it as it is again.
    candidate_missing: bool,
    /// The commits `--fix` cannot carry ([`crate::line::admit`]'s refusals).
    unfit: BTreeMap<String, Refusal>,
    /// Commits whose tree has no `crates/aterm-release/src/line.rs`.
    pre_line: BTreeSet<String>,
    /// git cannot read the line's head's tree.
    tree_unread: bool,
    /// Why carrying fails (a conflict, a record push that cannot land).
    carry_refusal: Option<Refusal>,
    /// Exercise the production Git carry while keeping publication in this fake world.
    carry_repo: Option<PathBuf>,
    /// The meaning check's answer ([`crate::line::verify`]).
    line_verify: Result<String, Refusal>,
    /// The qualify branches whose fake runs on until its stop fires, then ends stopped.
    holds: BTreeSet<Branch>,
    /// The half of `verify release` that `verify_passes` and `verify_code` judge (both when
    /// none); the other passes.
    verify_half: Option<Half>,
    /// The export's staging push begins (its stop then never fires), and it finishes once a
    /// red was said; the tree's run waits for that push.
    export_pushes: bool,
    /// The build fails before its slices claim memory; later branches wait for its end.
    build_claims_late: bool,
    build_admission_waits: usize,
    /// The build and the export do their work once the tree's end is told, and not when their
    /// stop fired by then.
    after_tree: bool,
    keep_build_on_test_red: bool,
    /// How `ship cut --sign` (step 7's sign side) exits.
    sign_code: i32,
    /// The build whose copy `cut --sign` signed privately and kept the record of.
    signed: Option<u64>,
    prepared_signature: Option<u64>,
    prepare_code: i32,
    early_overlap: bool,
    /// How step 7's two sides meet.
    sides: SideOrder,
    /// Step 7 runs the source this run ([`Backend::publish_begin`] readied it): the sign's order
    /// waits only on a source that runs.
    source_runs: bool,
    /// Fresh cuts that adopted the signed copy, and fresh cuts that signed their own.
    adopted: usize,
    resigned: usize,
}

/// How step 7's fake sides meet, so each test's order is the one it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SideOrder {
    /// The sign does its work once the source's end is told.
    SourceFirst,
    /// The source does its work once the sign's end is told.
    SignFirst,
    /// Each enters, then waits for the other to have entered, so both run at once.
    Overlap,
    /// The source's hook takes the gate (its push begins); the sign then ends, and the source
    /// finishes once a red was said.
    PushThenSign,
}

impl World {
    fn new() -> Self {
        let mut versions = BTreeMap::new();
        versions.insert(OLD.into(), "0.5.0".into());
        versions.insert(TIP.into(), "0.5.0".into());
        let mut origin_tags = BTreeMap::new();
        origin_tags.insert("v0.5.0".into(), OLD.into());
        Self {
            input_refusal: None,
            fleet_refusal: None,
            supersede_preflight_refusal: None,
            supersede_unknown_once: false,
            supersede_unknown_absent_once: false,
            supersede_classify_unknown: false,
            supersede_final_save_error: false,
            prepare_clone_refusal: None,
            missing_origin_tip: false,
            fleet_selected: None,
            existing_fleet_line: None,
            supersession: None,
            fleet_audit_refusal: false,
            fleet_audits: 0,
            dir: aterm_tempfile::tempdir().unwrap(),
            now: 1_000_000,
            out: Vec::new(),
            calls: Vec::new(),
            observations: Vec::new(),
            pid: 4242,
            started: "Thu Oct  9 02:00:00 2026".into(),
            alive: BTreeMap::new(),
            exec_result: Err(Refusal::new(
                "the fake could not exec R's frozen cutter",
                "nothing",
            )),
            exec_commit: None,
            times: String::new(),
            history: vec![OLD.into(), TIP.into()],
            versions,
            head: TIP.into(),
            candidate_before_tip_move: false,
            on_main: true,
            origin_tags,
            ledger: vec!["0.4.0".into(), "0.5.0".into()],
            published: BTreeMap::new(),
            newest: Some(OLD.into()),
            cut: None,
            attested: None,
            records: BTreeMap::new(),
            resume: Vec::new(),
            verify_outcomes: BTreeMap::new(),
            verify_runs: Vec::new(),
            tail: 1_791_500_000,
            artifacts: BTreeMap::new(),
            overtaken_at_cut: false,
            cut_refuses_before_its_claim: false,
            source_dies_after_push: false,
            claim_lands_unjournaled: false,
            machine_refusals: Vec::new(),
            fetch_refusal: None,
            fetches: 0,
            validation_tools: Ok("pinned tools ready".into()),
            blocks: Vec::new(),
            audits: VecDeque::new(),
            audited_dry: Vec::new(),
            audited_profiles: Vec::new(),
            verify_code: 0,
            verify_passes: true,
            stage_stages: true,
            publish_code: 0,
            source_attested: BTreeSet::new(),
            cut_code: 0,
            cut_finishes: true,
            cut_claims: true,
            latest_lags: false,
            stage_panics: false,
            after_cut: Ok("the release's shape checked".into()),
            site: Ok(crate::site::Outcome::Synced { deployed: true }),
            own_cutter: true,
            stop_at_qualify: false,
            qualify_boundary: None,
            cutter_recovery: crate::release_tools::Recovery::Initial,
            warming_active: false,
            unrecorded_hops: BTreeSet::new(),
            asks: Vec::new(),
            resolved: Vec::new(),
            withdrawn: Vec::new(),
            undelivered: 0,
            free: 500,
            disk_record: aterm_capacity::Footprints::default(),
            claim_refusal: None,
            cut_gate_refusals: Vec::new(),
            head_gate_refusals: BTreeMap::new(),
            head_gated: Vec::new(),
            publisher_lease_held: false,
            judge_down: false,
            stage_sync_fails: false,
            signal: None,
            signal_at: None,
            after_verify: None,
            after_stage: None,
            after_sleep: None,
            local_tags: BTreeSet::new(),
            real: None,
            fixtures_staged: None,
            fixtures_on_main: false,
            generator_refuses: false,
            landing_fails: false,
            prover: prover(),
            prover_error: None,
            lease_error: None,
            seal_error: None,
            engine: "e7e7e7e7e7".into(),
            staged_by: None,
            running_at_prepared: None,
            child_note_at_stage: false,
            ls_remote_down: false,
            candidate_missing: false,
            unfit: BTreeMap::new(),
            pre_line: BTreeSet::new(),
            tree_unread: false,
            carry_refusal: None,
            carry_repo: None,
            line_verify: Ok("the line verifies".into()),
            holds: BTreeSet::new(),
            verify_half: None,
            export_pushes: false,
            build_claims_late: false,
            build_admission_waits: 0,
            after_tree: false,
            keep_build_on_test_red: false,
            sign_code: 0,
            signed: None,
            prepared_signature: None,
            prepare_code: 0,
            early_overlap: false,
            sides: SideOrder::SourceFirst,
            source_runs: false,
            adopted: 0,
            resigned: 0,
        }
    }

    fn interrupt_at(&mut self, step: &str) -> bool {
        if self.signal_at == Some(step) {
            self.signal = Some(15);
            return true;
        }
        false
    }

    fn release_dir(&self) -> PathBuf {
        self.dir.path().join("aterm-release")
    }

    fn journal(&self) -> Option<Journal> {
        Journal::load(&self.release_dir().join(JOURNAL_FILE)).unwrap()
    }

    fn run(&mut self, args: ReleaseOptions) -> (i32, String) {
        self.out.clear();
        self.source_runs = false;
        let _ = std::fs::remove_file(self.dir.path().join("publish-entered"));
        let home = self.dir.path().to_path_buf();
        let outcome = run_with(self, &args, &home);
        (outcome.code, outcome.last)
    }

    fn count(&self, call: &str) -> usize {
        self.calls.iter().filter(|c| c.as_str() == call).count()
    }

    fn pos(&self, rev: &str) -> Option<usize> {
        let rev = rev.trim_end_matches("^{commit}");
        let rev = if rev == "origin/main" {
            self.history.last().map(String::as_str).unwrap_or("")
        } else {
            rev
        };
        self.history.iter().position(|c| c == rev)
    }

    fn tip(&self) -> String {
        self.history.last().cloned().unwrap_or_default()
    }

    fn macho(&self) -> Option<String> {
        self.journal()?.artifact.map(|a| a.macho_sha256)
    }

    /// The records of `commit`'s tree on `executable`, as the real judge folds them.
    fn checks_of(&self, commit: &str, executable: Option<&str>) -> Checks {
        let record = |check: &str, u: Option<&str>| {
            self.records
                .get(&(commit.to_owned(), u.map(str::to_owned), check.to_owned()))
                .copied()
        };
        let log = "/l/06-verify.log".to_owned();
        let mut also = Vec::new();
        if record(crate::check_record::RUN_LEVEL, None) == Some("FAIL") {
            also.push(Failed {
                check: crate::check_record::RUN_LEVEL.into(),
                log: log.clone(),
                unrecorded: false,
            });
        }
        for ((c, u, check), outcome) in &self.records {
            if c == commit && check == "measure" && u.as_deref() != executable && *outcome == "FAIL"
            {
                also.push(Failed {
                    check: "measure".into(),
                    log: log.clone(),
                    unrecorded: false,
                });
            }
        }
        Checks::of(
            commit,
            commit,
            executable,
            |check| {
                let measure = check == aterm_verify::checks::Check::Measure;
                if measure && executable.is_none() {
                    return State::Missing {
                        resume: Vec::new(),
                        lint_resume: Vec::new(),
                    };
                }
                match record(check.name(), executable.filter(|_| measure)) {
                    Some("ok") => State::Passed,
                    Some("FAIL") => State::Failed { log: log.clone() },
                    _ => State::Missing {
                        resume: self.resume.clone(),
                        lint_resume: Vec::new(),
                    },
                }
            },
            also,
        )
    }

    /// Every check of the journal's R passed on the journal's bundle.
    fn complete(&self) -> bool {
        let Some(j) = self.journal() else {
            return false;
        };
        self.checks_of(&j.commit, self.macho().as_deref())
            .complete()
    }
}

fn mac() -> ReleaseOptions {
    let mut o = ReleaseOptions::default();
    o.linux.mac_only = true;
    o
}

fn bump() -> ReleaseOptions {
    ReleaseOptions {
        bump: true,
        ..mac()
    }
}

fn prover() -> Prover {
    Prover {
        bin: PathBuf::from("/store/trust/9192/bin"),
        commit: "321aaeda7000000000000000000000000000000".into(),
        source: "the store".into(),
    }
}

fn ran(code: i32) -> Ran {
    Ran {
        code,
        detail: vec![format!("exit {code}")],
    }
}

/// The graph's branches over the world. The build marks its slice admission before the
/// tree starts. Work then runs past the other branch starts, except an early build failure.
/// A branch in [`World::holds`] runs unlocked until its stop fires.
struct WorldBranches<'a> {
    w: std::sync::Mutex<&'a mut World>,
    /// The branches whose work entered, the plan's branches and each start told, and the reds
    /// said, in order.
    entered: std::sync::Mutex<Vec<&'static str>>,
    moved: std::sync::Condvar,
}

/// `branch`'s marks: the plan runs it (its line names its log), and its start, or its stop where
/// it never started, was told.
fn marks(branch: Branch) -> (&'static str, &'static str) {
    match branch {
        Branch::Tree => ("tree-planned", "tree-told"),
        Branch::Build => ("build-planned", "build-told"),
        Branch::Export => ("export-planned", "export-told"),
    }
}

impl<'a> WorldBranches<'a> {
    fn world(&self) -> std::sync::MutexGuard<'_, &'a mut World> {
        self.w
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn mark(&self, what: &'static str) {
        self.entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(what);
        self.moved.notify_all();
    }

    /// Wait until `done` holds of what entered, or `ended` says so: a one-minute hang detector.
    fn wait(&self, what: &str, done: impl Fn(&[&'static str]) -> bool, ended: &dyn Fn() -> bool) {
        let bound = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut seen = self
            .entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !done(&seen) && !ended() {
            assert!(std::time::Instant::now() < bound, "hung waiting for {what}");
            seen = self
                .moved
                .wait_timeout(seen, std::time::Duration::from_millis(5))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Where the world says so, wait for the tree's end to be told: whether `stop` fired by
    /// then.
    fn stopped_after_tree(&self, stop: &BranchStop) -> bool {
        if !self.world().after_tree {
            return false;
        }
        self.wait(
            "the tree's end",
            |seen| seen.contains(&"tree-ended"),
            &|| false,
        );
        stop.fired()
    }

    /// Whether `branch` is held; if so, run on until `stop` fires (a one-minute hang detector).
    fn held(&self, branch: Branch, stop: &BranchStop) -> bool {
        if !self.world().holds.contains(&branch) {
            return false;
        }
        self.wait(branch.name(), |_| false, &|| stop.fired());
        true
    }

    /// Wait until the start of each of `later` the plan runs is told, or its stop where it never
    /// started (a one-minute hang detector).
    fn after_starts(&self, later: &[Branch]) {
        self.wait(
            "the later branches' starts",
            |seen| {
                later.iter().all(|b| {
                    let (planned, told) = marks(*b);
                    !seen.contains(&planned) || seen.contains(&told)
                })
            },
            &|| false,
        );
    }
}

impl Branches for WorldBranches<'_> {
    fn keep_build_on_test_red(&self, _c: &Candidate) -> bool {
        self.world().keep_build_on_test_red
    }
    fn say(&self, line: &str) {
        Backend::say(&mut **self.world(), line);
        if line.starts_with("        tree: ") && !line.contains("; log: ") {
            self.mark("tree-ended");
        }
        if line.starts_with("        source: ") && !line.contains("; log: ") {
            self.mark("source-ended");
        }
        if line.starts_with("        sign: ") && !line.contains("; log: ") {
            self.mark("sign-ended");
        }
        if line.starts_with("        build: ") && !line.contains("; log: ") {
            self.mark("build-ended");
        }
        if line.contains(": RED at ") {
            self.mark("red");
        }
        for branch in Branch::ALL {
            if line.starts_with(&format!("        {}: ", branch.name())) && line.contains("; log: ")
            {
                self.mark(marks(branch).0);
            }
        }
        if let Some(event) = line.strip_prefix("release-event: ") {
            let event: aterm_json::Value = aterm_json::from_str(event).unwrap();
            if event["event"].as_str() == Some("qualify-branch")
                && let Some(branch) = Branch::ALL
                    .into_iter()
                    .find(|b| event["branch"].as_str() == Some(b.name()))
            {
                self.mark(marks(branch).1);
            }
        }
    }
    fn now(&self) -> u64 {
        Backend::now(&mut **self.world())
    }
    fn interrupted(&self) -> bool {
        Backend::interrupted(&mut **self.world()).is_some()
    }
    fn branch_log(&self, branch: Branch) -> PathBuf {
        let w = self.world();
        w.dir
            .path()
            .join("logs")
            .join(format!("04-{}.log", branch.row()))
    }
    fn build_admitted(&self, ended: &dyn Fn() -> bool) {
        self.world().build_admission_waits += 1;
        let late = self.world().build_claims_late;
        self.wait(
            "the slices' claims",
            |seen| !late && seen.contains(&"slices"),
            ended,
        );
    }
    fn export(
        &self,
        c: &Candidate,
        _prover: &Prover,
        target: StageTarget,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        if self.stopped_after_tree(stop) {
            return Ok(ran_code(130));
        }
        if self.world().signal_at == Some("build") {
            // A person's signal at `ship build` comes minutes into the build, before the export's
            // work is done: the export's work ends once the build's end is told, and a stop that
            // end fired ends it, as its group's SIGTERM ends the real export's child.
            self.wait(
                "the build's end",
                |seen| seen.contains(&"build-ended"),
                &|| false,
            );
            if stop.fired() {
                return Ok(ran_code(130));
            }
        }
        let pushes = self.world().export_pushes;
        if pushes {
            stop.in_order(|| {
                assert!(stop.push(), "the export's push begins before any red");
                let event = aterm_json::json!({"schema": 1, "event": "qualify-branch",
                    "branch": "export", "phase": "push-began", "at_unix_s": 0, "seconds": 0});
                self.say(&format!("release-event: {event}"));
            });
            self.mark("push");
            self.wait("a red", |seen| seen.contains(&"red"), &|| false);
        }
        let ran = self.world().export(c, target);
        self.mark("export");
        if self.held(Branch::Export, stop) {
            return Ok(ran_code(130));
        }
        ran
    }
    fn build(
        &self,
        c: &Candidate,
        reserved: &Reservation,
        p: &Prover,
        work: BuildWork,
        stop: &BranchStop,
    ) -> Built {
        if self.world().build_claims_late {
            let mut world = self.world();
            world.calls.push("build".into());
            world.interrupt_at("build");
            return Built {
                artifact: Some(Err(Refusal::new("early build failure", "fix the build"))),
                fixtures: None,
            };
        }
        self.mark("slices");
        self.after_starts(&[Branch::Tree, Branch::Export]);
        if self.stopped_after_tree(stop) {
            return Built {
                artifact: Some(Err(
                    Refusal::new("the build was stopped", "nothing").could_not_run()
                )),
                fixtures: None,
            };
        }
        let built = self.world().build(c, reserved, p, work);
        self.mark("build");
        if self.held(Branch::Build, stop) {
            return Built {
                artifact: Some(Err(
                    Refusal::new("the build was stopped", "nothing").could_not_run()
                )),
                fixtures: None,
            };
        }
        built
    }
    fn verify(
        &self,
        c: &Candidate,
        _prover: &Prover,
        run: &VerifyRun,
        artifact: Option<&Artifact>,
        stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        let tree = run.half == Half::Tree;
        self.world().verify_called(run);
        if !tree && self.world().early_overlap {
            assert!(
                self.world().prepared_signature.is_some(),
                "preparation started before bundle"
            );
        }
        if tree {
            self.mark("tree");
            self.after_starts(&[Branch::Build, Branch::Export]);
            if self.world().export_pushes {
                self.wait("the export's push", |seen| seen.contains(&"push"), &|| {
                    false
                });
            }
        }
        if tree && self.world().holds.contains(&Branch::Tree) {
            // A held tree finishes no check: it records nothing before its stop ends it.
            self.held(Branch::Tree, stop);
            return Ok(ran_code(143));
        }
        self.world().verify(c, run, artifact)
    }
    fn side_log(&self, side: Side) -> PathBuf {
        let w = self.world();
        w.dir
            .path()
            .join("logs")
            .join(format!("07-{}.log", side.row()))
    }
    fn publish_gate(&self) -> PublishGate {
        PublishGate::at(self.world().dir.path().join("logs").join("07-publish.gate"))
    }
    fn source(
        &self,
        _prover: &Prover,
        gate: &PublishGate,
        _stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        let order = self.world().sides;
        self.mark("source");
        match order {
            SideOrder::SignFirst => {
                self.wait(
                    "the sign's end",
                    |seen| seen.contains(&"sign-ended"),
                    &|| false,
                );
            }
            SideOrder::Overlap => self.wait("the sign", |seen| seen.contains(&"sign"), &|| false),
            SideOrder::SourceFirst | SideOrder::PushThenSign => {}
        }
        // `pub publish`'s gates passed: its pre-promote hook takes the gate, as the real one.
        if !gate.push().unwrap() {
            self.world().calls.push("publish: nothing pushed".into());
            return Ok(ran_code(1));
        }
        if order == SideOrder::PushThenSign {
            self.mark("push");
            self.wait("a red", |seen| seen.contains(&"red"), &|| false);
        }
        if self.world().source_dies_after_push {
            return Ok(ran_code(1));
        }
        Ok(self.world().publish_source())
    }
    fn sign(&self, args: &[String], _prover: &Prover, stop: &BranchStop) -> Ran {
        let (order, source_runs) = {
            let w = self.world();
            (w.sides, w.source_runs)
        };
        self.mark("sign");
        match order {
            _ if !source_runs => {}
            SideOrder::SourceFirst => {
                self.wait(
                    "the source's end",
                    |seen| seen.contains(&"source-ended"),
                    &|| false,
                );
            }
            SideOrder::Overlap => {
                self.wait("the source", |seen| seen.contains(&"source"), &|| false);
            }
            SideOrder::PushThenSign => {
                self.wait("the push", |seen| seen.contains(&"push"), &|| false);
            }
            SideOrder::SignFirst => {}
        }
        if stop.fired() {
            return ran_code(143);
        }
        let mut w = self.world();
        assert_eq!(args[..2], ["cut", "--sign"], "{args:?}");
        w.calls.push("cut --sign".into());
        // A cutter a person's signal cut short, whose exit says only 1.
        if w.interrupt_at("cut --sign") {
            return ran(1);
        }
        if w.sign_code == 0 {
            w.signed = w.journal().and_then(|j| j.reserved).map(|r| r.build);
        }
        ran(w.sign_code)
    }
}

fn ran_code(code: i32) -> Ran {
    ran(code)
}

impl World {
    /// `pub publish` of R, its push begun: R recorded as the engine's newest row, and its source
    /// release attested unless it fails at sign-release.
    fn publish_source(&mut self) -> Ran {
        self.calls.push("publish".into());
        let r = self.head.clone();
        self.published.insert(r.clone(), "e1e1e1e1e1".into());
        self.newest = Some(r.clone());
        if self.publish_code == 0 {
            self.source_attested.insert(r);
        }
        ran(self.publish_code)
    }
}

impl World {
    /// Count of `verify release` runs of either half.
    fn verified(&self) -> usize {
        self.count("verify --tree") + self.count("verify --bundle")
    }

    fn stage_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        _p: &Prover,
    ) -> Result<String, Refusal> {
        if self.fixtures_staged == Some(reserved.build) {
            return Ok(format!("v{}'s handoff fixtures staged already", c.version));
        }
        self.calls.push("stage-fixtures".into());
        if self.generator_refuses {
            return Err(Refusal::new(
                "the release's handoff fixture generator refuses: 1 test(s) failed: \
                 seamless::carry_tests::fixture_generator::generate_handoff_fixtures",
                "fix it on main",
            ));
        }
        self.fixtures_staged = Some(reserved.build);
        Ok(format!(
            "v{}'s handoff fixtures written by its own producer for build {}",
            c.version, reserved.build
        ))
    }

    fn build(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        p: &Prover,
        work: BuildWork,
    ) -> Built {
        if work.bundle && self.interrupt_at("build") {
            // A signal cuts `ship build` short; it exits nonzero, as a red build does.
            self.calls.push("build".into());
            return Built {
                artifact: Some(Err(Refusal::new(
                    "the build did not finish (ship build exited 130)",
                    "read its log",
                ))),
                fixtures: None,
            };
        }
        let artifact = work.bundle.then(|| {
            self.calls.push("build".into());
            let key = format!("{:0>64}", reserved.build);
            let macho = format!("m{}", reserved.build);
            let app = PathBuf::from(format!("/cache/{key}/aterm.app"));
            self.artifacts
                .insert(key.clone(), (reserved.build, macho.clone(), app.clone()));
            Ok(Artifact {
                key,
                app,
                macho_sha256: macho,
                build: reserved.build,
            })
        });
        Built {
            artifact,
            fixtures: work.fixtures.then(|| self.stage_fixtures(c, reserved, p)),
        }
    }

    /// `verify release` of `run`'s half began: its call and what it was asked to run, recorded
    /// as its child starts (the tree's before its claim).
    fn verify_called(&mut self, run: &VerifyRun) {
        self.calls.push(format!("verify {}", run.half.flag()));
        self.verify_runs.push(run.clone());
    }

    /// `verify release` of `run`'s half, its call recorded ([`World::verify_called`]): the
    /// bundle half on a bundle the build made.
    fn verify(
        &mut self,
        c: &Candidate,
        run: &VerifyRun,
        artifact: Option<&Artifact>,
    ) -> Result<Ran, Refusal> {
        match (run.half, artifact) {
            (Half::Tree, None) => {}
            (Half::Bundle, Some(artifact)) => assert!(
                self.artifacts
                    .values()
                    .any(|(_, macho, app)| *app == artifact.app && *macho == artifact.macho_sha256),
                "`verify release --bundle` is handed a bundle the build made"
            ),
            (half, artifact) => panic!("the {half:?} half was handed {artifact:?}"),
        }
        if self.interrupt_at(if run.half == Half::Tree {
            "verify --tree"
        } else {
            "verify --bundle"
        }) {
            return Ok(ran(-1));
        }
        if let Some((real, script, dir)) = &mut self.real {
            let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
            cmd.args(["-c", script.as_str(), "sh"]).arg(dir.as_path());
            let step = if run.half == Half::Tree {
                Step::Qualify
            } else {
                Step::Bundle
            };
            return Ok(real.logged(step, &mut cmd));
        }
        let judged = self.verify_half.is_none_or(|half| half == run.half);
        let (passes, code) = if judged {
            (self.verify_passes, self.verify_code)
        } else {
            (true, 0)
        };
        let mut failed = false;
        for check in &run.checks {
            let outcome = self
                .verify_outcomes
                .get(check.as_str())
                .copied()
                .or(passes.then_some("ok"));
            if let Some(outcome) = outcome {
                failed |= outcome == "FAIL";
                let u = (check == "measure")
                    .then(|| artifact.map(|a| a.macho_sha256.clone()))
                    .flatten();
                self.records
                    .insert((c.commit.clone(), u, check.clone()), outcome);
            }
        }
        // As the real cutter does, an exit 1 that named no FAIL is a run-level FAIL.
        if code == 1 && !failed {
            self.records.insert(
                (
                    c.commit.clone(),
                    None,
                    crate::check_record::RUN_LEVEL.into(),
                ),
                "FAIL",
            );
        }
        if run.half == Half::Bundle
            && let Some(then) = self.after_verify
        {
            then(self);
        }
        Ok(ran(code))
    }

    /// `pub stage` of R: to the staging tier, or (`Local`, under `--qualify`) without its push.
    fn export(&mut self, c: &Candidate, target: StageTarget) -> Result<Ran, Refusal> {
        self.calls.push(
            if target == StageTarget::Local {
                "stage --dry-run"
            } else {
                "stage"
            }
            .into(),
        );
        assert!(!self.stage_panics, "a bug in the release");
        if self.stage_stages {
            if target == StageTarget::Local {
                if let Some(then) = self.after_stage {
                    then(self);
                }
                return Ok(ran(0));
            }
            self.attested = Some(c.commit.clone());
            self.staged_by = Some(self.engine.clone());
            self.candidate_missing = false;
            if self.child_note_at_stage {
                std::fs::write(
                    World::release_dir(self).join(RUNNING_CHILD_FILE),
                    "pid 999\nstarted Thu Oct  9 01:00:00 2026\nstep stage\n",
                )
                .unwrap();
            }
            if self.stage_sync_fails {
                return Err(Refusal::new(
                    "the engine is not a fast-forward of its origin/main (its audit rows are \
                     kept in /logs/audit-rows.jsonl)",
                    "reconcile it by hand",
                ));
            }
            if let Some(then) = self.after_stage {
                then(self);
            }
            return Ok(ran(0));
        }
        Ok(ran(1))
    }
}

struct FakePreparingSign {
    code: i32,
    stop: BranchStop,
    gate: PathBuf,
}

impl crate::release::PendingSign for FakePreparingSign {
    fn stop(&self) -> BranchStop {
        self.stop.clone()
    }
    fn finish(self: Box<Self>) -> Ran {
        assert!(
            self.stop.fired() || self.gate.exists(),
            "the early child is joined only at step 7, after READY, or during cancellation"
        );
        ran(self.code)
    }
}

impl Backend for World {
    fn main_lane(&mut self, base: &str) -> Option<String> {
        Some(format!(
            "main lane: base {base}: red, age_s=23 (advisory fixture)"
        ))
    }
    fn start_prefreeze(&mut self) {
        std::fs::write(
            self.dir.path().join("prefreeze-started"),
            format!(
                "ready={} publish_entered={}",
                self.complete(),
                self.dir.path().join("publish-entered").exists(),
            ),
        )
        .unwrap();
    }
    fn prepare_sign(
        &mut self,
        args: &[String],
        _prover: &Prover,
    ) -> Result<Box<dyn crate::release::PendingSign>, Refusal> {
        assert_eq!(args[..2], ["cut", "--sign-prepare"]);
        self.calls.push("cut --sign-prepare".into());
        let code = if self.interrupt_at("cut --sign-prepare") {
            1
        } else {
            self.prepare_code
        };
        if code == 0 {
            self.prepared_signature = self.journal().and_then(|j| j.reserved).map(|r| r.build);
        }
        Ok(Box::new(FakePreparingSign {
            code,
            stop: BranchStop::default(),
            gate: self.dir.path().join("publish-entered"),
        }))
    }
    fn entered(&mut self, step: Step) {
        if step == Step::Publish {
            std::fs::write(self.dir.path().join("publish-entered"), b"READY passed").unwrap();
        }
    }
    fn fleet_supersession(
        &mut self,
        _version: &str,
    ) -> Result<Option<crate::line::Supersession>, Refusal> {
        Ok(self.supersession.clone())
    }
    fn preflight_supersede(
        &mut self,
        line: &Line,
        expected_previous: Option<&Line>,
    ) -> Result<Line, Refusal> {
        self.calls.push("supersede-preflight".into());
        let previous = self
            .existing_fleet_line
            .clone()
            .ok_or_else(|| Refusal::new("no existing fleet line", "drop --supersede"))?;
        if expected_previous.is_some_and(|expected| *expected != previous) {
            return Err(Refusal::new(
                "the fleet line advanced",
                "resume the current line",
            ));
        }
        if self.supersession.is_some() {
            return Err(Refusal::new(
                "the line already has a durable supersession",
                "resume the current line",
            ));
        }
        if let Some(refusal) = self.supersede_preflight_refusal.clone() {
            return Err(refusal);
        }
        let tagged = self.origin_tags.contains_key(&format!("v{}", line.version));
        let ledgered = self.ledger.iter().any(|version| version == &line.version)
            || self
                .cut
                .as_ref()
                .is_some_and(|cut| cut.version == line.version);
        let published = self
            .newest
            .as_deref()
            .is_some_and(|commit| self.versions.get(commit) == Some(&line.version))
            || std::iter::once(previous.base.as_str())
                .chain(previous.hops.iter().map(|hop| hop.commit.as_str()))
                .any(|commit| self.published.contains_key(commit));
        crate::line::require_unpublished([tagged, ledgered, published, false, false])
            .map_err(|why| Refusal::new(why, crate::release::fix_remedy(previous.head())))?;
        Ok(previous)
    }
    fn classify_supersede(
        &mut self,
        previous: &Line,
        line: &Line,
    ) -> crate::release::SupersedeResult {
        if self.supersede_classify_unknown {
            return crate::release::SupersedeResult::Unknown(
                "planted authoritative reread failure".into(),
            );
        }
        match self.supersession.clone() {
            Some(record)
                if record.previous == *previous
                    && record.replacement == *line
                    && record.reason == "explicit request" =>
            {
                crate::release::SupersedeResult::Committed(
                    "the exact earlier supersession is durable".into(),
                )
            }
            Some(record) => crate::release::SupersedeResult::Refused {
                why: "a different durable supersession won".into(),
                retry: false,
                winner: Some(record.replacement),
            },
            None => crate::release::SupersedeResult::Refused {
                why: "the earlier source write is absent".into(),
                retry: true,
                winner: None,
            },
        }
    }
    fn supersede_line(&mut self, previous: &Line, line: &Line) -> crate::release::SupersedeResult {
        self.calls.push("supersede".into());
        if let Some(why) = &self.fleet_refusal {
            return crate::release::SupersedeResult::Refused {
                why: format!("{why} — {}", crate::release::fix_remedy(previous.head())),
                retry: false,
                winner: None,
            };
        }
        let record = crate::line::Supersession {
            previous: previous.clone(),
            replacement: line.clone(),
            reason: "explicit request".into(),
        };
        if std::mem::take(&mut self.supersede_unknown_absent_once) {
            return crate::release::SupersedeResult::Unknown(
                "planted response loss before an absent source write".into(),
            );
        }
        self.supersession = Some(record);
        self.existing_fleet_line = Some(line.clone());
        self.fleet_selected = Some(line.clone());
        if std::mem::take(&mut self.supersede_unknown_once) {
            crate::release::SupersedeResult::Unknown(
                "planted response loss after the source write".into(),
            )
        } else {
            crate::release::SupersedeResult::Committed("SUPERSEDED".into())
        }
    }
    fn save_supersede_journal(&mut self, journal: &Journal, path: &Path) -> Result<(), String> {
        if std::mem::take(&mut self.supersede_final_save_error) {
            Err("planted final candidate write refusal".into())
        } else {
            journal.save(path)
        }
    }
    fn fleet_existing(&mut self, version: &str) -> Result<Option<Line>, Refusal> {
        let line = self
            .existing_fleet_line
            .clone()
            .filter(|line| line.version == version);
        Ok(line)
    }
    fn fleet_line(
        &mut self,
        line: &crate::line::Line,
        _handed: bool,
    ) -> Result<crate::line::Line, String> {
        self.fleet_refusal.clone().map_or_else(
            || Ok(self.fleet_selected.clone().unwrap_or_else(|| line.clone())),
            Err,
        )
    }
    fn fleet_audit(&mut self) -> Result<(), String> {
        self.fleet_audits += 1;
        if self.fleet_audit_refusal {
            Err("fleet line was adopted".into())
        } else {
            Ok(())
        }
    }
    fn observe_operational(&mut self) {
        self.observations.push("observe-operational");
    }
    fn now(&mut self) -> u64 {
        self.now += 1;
        self.now
    }
    fn hms(&mut self, epoch: u64) -> String {
        format!(
            "{:02}:{:02}:{:02}",
            epoch / 3600 % 24,
            epoch / 60 % 60,
            epoch % 60
        )
    }
    fn sleep(&mut self, secs: u64) {
        self.calls.push(format!("sleep {secs}"));
        self.now += secs;
        if let Some(then) = self.after_sleep.take() {
            then(self);
        }
    }
    fn say(&mut self, line: &str) {
        if line.starts_with("PREPARED") {
            self.running_at_prepared = Some(World::release_dir(self).join(RUNNING_FILE).exists());
        }
        self.out.push(line.to_string());
    }
    fn interrupted(&mut self) -> Option<i32> {
        match &mut self.real {
            Some((real, ..)) => real.interrupted(),
            None => self.signal,
        }
    }
    fn log_path(&mut self, step: Step) -> PathBuf {
        self.dir
            .path()
            .join("logs")
            .join(format!("{:02}-{}.log", step.number(), step.name()))
    }
    fn me(&mut self) -> (u32, String) {
        (self.pid, self.started.clone())
    }
    fn started_of(&mut self, pid: u32) -> Option<String> {
        if pid == self.pid {
            return Some(self.started.clone());
        }
        self.alive.get(&pid).cloned()
    }
    fn release_dir(&mut self) -> PathBuf {
        World::release_dir(self)
    }
    fn times(&mut self) -> String {
        self.times.clone()
    }
    fn record_time(&mut self, version: &str, row: &str, secs: u64) {
        self.times
            .push_str(&format!("{}\t{version}\t{row}\t{secs}\n", self.now));
    }
    fn prover(&mut self, _journaled: Option<&str>) -> Result<Prover, String> {
        self.observations.push("prover");
        self.prover_error
            .clone()
            .map_or_else(|| Ok(self.prover.clone()), Err)
    }
    fn lease(&mut self, _prover: &Prover) -> Result<(), String> {
        self.calls.push("lease".into());
        self.lease_error.clone().map_or(Ok(()), Err)
    }
    fn seal_inputs(&mut self) -> Result<(), String> {
        self.calls.push("seal-inputs".into());
        self.seal_error.clone().map_or(Ok(()), Err)
    }
    fn cleared_env(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn machine(&mut self, _creds: &Path, _explicit: bool) -> (String, Vec<Refusal>) {
        self.observations.push("machine");
        ("m3".into(), self.machine_refusals.clone())
    }
    fn fetch(&mut self) -> Result<String, Refusal> {
        self.observations.push("fetch");
        self.fetches += 1;
        if let Some((at, refusal)) = &self.fetch_refusal
            && *at == self.fetches
        {
            return Err(refusal.clone());
        }
        Ok(self.tip())
    }
    fn head(&mut self) -> String {
        self.head.clone()
    }
    fn branch_is_main(&mut self) -> bool {
        self.on_main
    }
    fn prepare_clone(&mut self) -> Result<(), Refusal> {
        self.prepare_clone_refusal.clone().map_or(Ok(()), Err)
    }
    fn rev(&mut self, rev: &str) -> Option<String> {
        if self.missing_origin_tip && rev == "origin/main^{commit}" {
            return None;
        }
        self.pos(rev).map(|i| self.history[i].clone())
    }
    fn version_at(&mut self, rev: &str) -> Option<String> {
        let full = self.rev(rev)?;
        self.versions.get(&full).cloned()
    }
    fn is_ancestor(&mut self, a: &str, b: &str) -> bool {
        if (b == "origin/main" || b == self.tip()) && self.unrecorded_hops.contains(a) {
            return false;
        }
        matches!((self.pos(a), self.pos(b)), (Some(a), Some(b)) if a <= b)
    }
    fn origin_tag(&mut self, tag: &str) -> Option<String> {
        self.origin_tags.get(tag).cloned()
    }
    fn origin_tag_now(&mut self, tag: &str) -> Result<Option<String>, String> {
        self.calls.push(format!("ls-remote {tag}"));
        if self.ls_remote_down {
            return Err("origin did not answer".into());
        }
        if let Some(cut) = self.cut.as_ref().filter(|cut| cut.finished)
            && tag == format!("v{}", cut.version)
        {
            return Ok(Some(format!("e{}", &BUMP[1..])));
        }
        Ok(self.origin_tags.get(tag).cloned())
    }
    fn local_tag(&mut self, tag: &str) -> bool {
        self.origin_tags.contains_key(tag) || self.local_tags.contains(tag)
    }
    fn input_checks(&mut self, base: &str, tip: &str, _prover: &Prover) -> Result<String, Refusal> {
        self.calls.push(format!("input-checks {base} {tip}"));
        self.input_refusal
            .clone()
            .map_or_else(|| Ok("input clean".into()), Err)
    }
    fn ledger_versions(&mut self, _rev: &str) -> Vec<String> {
        self.ledger.clone()
    }
    fn engine_rows(&mut self, commit: Option<&str>) -> EngineRows {
        let mine = commit.and_then(|c| self.published.get(c));
        EngineRows {
            newest: self.newest.clone(),
            has: mine.is_some(),
            rev: mine.cloned(),
        }
    }
    fn engine_now(&mut self, staged: &str) -> Result<EngineNow, String> {
        self.calls.push("engine-now".into());
        let now = if staged == self.engine {
            EngineNow::Same(format!("the engine is the one that staged ({staged})"))
        } else {
            EngineNow::Moved(format!(
                "the engine's code moved since it staged: {staged}..{}",
                self.engine
            ))
        };
        Ok(now)
    }
    fn candidate_missing(&mut self) -> Result<Option<String>, String> {
        self.calls.push("candidate-now".into());
        Ok(self
            .candidate_missing
            .then(|| "the qualified candidate was lost".into()))
    }
    fn cut_journal(&mut self) -> Option<CutState> {
        self.cut.clone()
    }
    fn is_dir(&mut self, _path: &Path) -> bool {
        true
    }
    fn tree_has(&mut self, rev: &str, path: &str) -> Result<bool, Refusal> {
        assert_eq!(path, crate::release::LINE_SOURCE);
        if self.tree_unread {
            return Err(Refusal::new(
                format!("this clone lacks commit {}", &rev[..4]),
                "fetch origin, then run the release again",
            )
            .could_not_run());
        }
        Ok(!self.pre_line.contains(rev))
    }
    fn fix_of(&mut self, line: &Line, rev: &str) -> Result<Admitted, Refusal> {
        let named = || {
            Refusal::new(
                format!("--fix {rev:?} names no commit of this clone"),
                "fetch",
            )
        };
        let commit = self.rev(rev).ok_or_else(named)?;
        if line.carried(&commit) {
            return Ok(Admitted::Carried);
        }
        if let Some(refusal) = self.unfit.get(&commit) {
            return Err(refusal.clone());
        }
        if self.is_ancestor(&commit, line.head()) {
            return Err(Refusal::new(
                format!("{} is already in the line", &commit[..4]),
                "drop it from --fix",
            ));
        }
        Ok(Admitted::Fix(Fix {
            subject: format!("fix {}", &commit[..4]),
            commit,
            paths: vec!["src/fixed.rs".into()],
        }))
    }
    /// Each hop and then the record merge join the (linear) history, so the head is on main.
    fn carry(&mut self, line: &Line, fixes: &[Fix]) -> Result<(Vec<Hop>, String), Refusal> {
        self.calls.push("carry".into());
        if let Some(refusal) = self.carry_refusal.clone() {
            return Err(refusal);
        }
        let main = self.versions.get(&self.tip()).cloned().unwrap_or_default();
        let hops: Vec<Hop> = if let Some(repo) = &self.carry_repo {
            let git = crate::ledger::testing::Git::new(repo);
            let fixes = crate::line::order(&git, fixes.to_vec())?;
            crate::line::carry(&git, line, &fixes)?
        } else {
            fixes
                .iter()
                .map(|fix| Hop {
                    commit: hop_of(&fix.commit),
                    carries: fix.commit.clone(),
                    why: fix.subject.clone(),
                    paths: fix.paths.clone(),
                })
                .collect()
        };
        for hop in &hops {
            self.unrecorded_hops.insert(hop.commit.clone());
            self.history.push(hop.commit.clone());
            self.versions
                .insert(hop.commit.clone(), line.version.clone());
        }
        let head = hops.last().map_or(line.head(), |hop| hop.commit.as_str());
        let merge = format!("8{}", &head[1..]);
        self.history.push(merge.clone());
        self.versions.insert(merge.clone(), main);
        Ok((
            hops,
            format!("recorded on main by the merge {}", &merge[..4]),
        ))
    }
    fn verify_line(&mut self, _line: &Line) -> Result<String, Refusal> {
        self.calls.push("verify-line".into());
        self.line_verify.clone()
    }
    fn failure_hints(&mut self, line: &Line, _evidence: &str, _logs: &[String]) -> Vec<String> {
        self.calls.push(format!("failure-hints {}", line.head()));
        vec![format!("fixture main fix hint for {}", line.head())]
    }
    fn sync(&mut self) -> Result<String, Refusal> {
        self.calls.push("sync".into());
        Ok("engine synced".into())
    }
    fn bump(&mut self, _from: &str, to: &str, _prover: &Prover) -> Result<String, Refusal> {
        self.calls.push("bump".into());
        let made = if self.history.iter().any(|c| c == BUMP) {
            BUMP2
        } else {
            BUMP
        };
        self.history.push(made.into());
        self.versions.insert(made.into(), to.into());
        Ok(made.into())
    }
    fn validation_tools(&mut self) -> Result<String, Refusal> {
        self.warming_active = false;
        self.observations.push("validation-tools");
        self.calls.push("validation-tools".into());
        self.validation_tools.clone()
    }

    fn move_to(&mut self, commit: &str) -> Result<(), Refusal> {
        self.calls.push(format!("move {}", &commit[..4]));
        if commit == TIP && self.release_dir().join(SELECTION_FILE).exists() {
            self.candidate_before_tip_move = true;
        }
        self.head = commit.to_string();
        self.interrupt_at("move");
        Ok(())
    }
    fn own_cutter(&mut self, _commit: &str) -> Result<String, crate::cutter::NotOwn> {
        if self.own_cutter {
            Ok("its own".into())
        } else {
            Err(crate::cutter::NotOwn {
                why: "built from another commit".into(),
                fix: "the verdict's own remedy".into(),
            })
        }
    }
    fn cutter_recovery(&mut self, _commit: &str) -> crate::release_tools::Recovery {
        self.cutter_recovery
    }
    fn exec_cutter(&mut self, commit: &str, _prover: &Prover) -> Result<(), Refusal> {
        self.calls.push("exec-cutter".into());
        self.exec_commit = Some(commit.to_owned());
        self.exec_result.clone()
    }
    fn warm_tools(&mut self, _commit: &str, _prover: &Prover) -> Result<(), Refusal> {
        self.observations.push("warm-tools");
        self.warming_active = true;
        Ok(())
    }
    fn stop_tools(&mut self) {
        if self.warming_active {
            assert!(
                self.release_dir().join(RUNNING_FILE).exists(),
                "the clone remains claimed until its compiler is settled"
            );
        }
        self.warming_active = false;
    }
    fn record_line(&mut self, line: &Line) -> Result<String, Refusal> {
        self.calls.push("record-line".into());
        for hop in &line.hops {
            self.unrecorded_hops.remove(&hop.commit);
        }
        Ok("recorded on main".into())
    }
    fn rewind_selection(&mut self, line: &Line) -> Result<(), Refusal> {
        self.calls.push("rewind-selection".into());
        self.head = line.base.clone();
        Ok(())
    }
    fn facts(&mut self, c: &Candidate, _prover: &Prover, artifact: Option<&Artifact>) -> Facts {
        let down = self.judge_down;
        let built = match artifact {
            None => Answer::no("no bundle is built for it yet"),
            Some(a)
                if self
                    .artifacts
                    .get(&a.key)
                    .is_some_and(|(_, macho, _)| *macho == a.macho_sha256) =>
            {
                Answer::yes(format!("built once: {}", a.app.display()))
            }
            Some(_) => Answer::no("the release cache holds no COMPLETE bundle for it"),
        };
        let checks = if down {
            Checks::unanswered("the check records' judge did not answer: store unreadable")
        } else {
            let executable = artifact
                .filter(|_| built.yes)
                .map(|a| a.macho_sha256.as_str());
            self.checks_of(&c.commit, executable)
        };
        let attested =
            self.attested.as_deref() == Some(c.commit.as_str()) && self.staged_by.is_some();
        Facts {
            cut: self.cut.clone(),
            engine: self.engine_rows(Some(&c.commit)),
            attested,
            staged_by: self.staged_by.clone().filter(|_| attested),
            built,
            checks,
            fixtures: if self.fixtures_on_main {
                Answer::yes(format!("v{}'s handoff fixtures are on main", c.version))
            } else {
                Answer::no(format!("v{}'s handoff fixtures are not on main", c.version))
            },
        }
    }
    fn disk(&mut self) -> (Option<u64>, bool) {
        (Some(self.free), true)
    }
    fn disk_record(&mut self) -> aterm_capacity::Footprints {
        self.disk_record.clone()
    }
    fn claim_disk(&mut self, bytes: u64, _why: &str) -> Result<(), Refusal> {
        self.calls.push(format!("disk-claim {}", bytes >> 30));
        self.claim_refusal.clone().map_or(Ok(()), Err)
    }
    fn let_disk_go(&mut self, passed: bool) {
        self.calls.push(format!("disk-let-go {passed}"));
    }
    fn commit_gates(&mut self, _c: &Candidate, dry: bool) -> Vec<Refusal> {
        self.calls.push(
            if dry {
                "commit-gates-dry"
            } else {
                "commit-gates"
            }
            .into(),
        );
        self.blocks.clone()
    }
    fn cut_gates(
        &mut self,
        _c: &Candidate,
        _creds: &Path,
        kind: crate::publish::CutKind,
    ) -> Vec<Refusal> {
        self.calls.push("cut-gates".into());
        let mut refusals = self.cut_gate_refusals.clone();
        if self.publisher_lease_held && kind == crate::publish::CutKind::Real {
            refusals.push(lease_held());
        }
        refusals
    }
    fn head_gates(&mut self, c: &Candidate) -> Vec<Refusal> {
        self.head_gated.push(c.commit.clone());
        self.head_gate_refusals
            .get(&c.commit)
            .cloned()
            .unwrap_or_default()
    }
    fn land_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        p: &Prover,
    ) -> Result<String, Refusal> {
        self.calls.push("land-fixtures".into());
        if self.fixtures_on_main {
            return Ok(format!(
                "v{}'s handoff fixtures are on main already",
                c.version
            ));
        }
        if self.fixtures_staged != Some(reserved.build) {
            self.stage_fixtures(c, reserved, p)?;
        }
        if self.landing_fails {
            return Err(Refusal::new(
                "handoff fixtures: git push origin main failed (not a fast-forward rejection, \
                 so retrying cannot help): remote: denied",
                "run it again",
            ));
        }
        self.fixtures_on_main = true;
        Ok(format!("v{}'s handoff fixtures landed on main", c.version))
    }
    fn audit(&mut self, _id: &str, profile: Option<&Path>, dry: bool) -> Vec<Finding> {
        self.calls.push("audit".into());
        self.audited_dry.push(dry);
        self.audited_profiles.push(profile.map(Path::to_path_buf));
        if self.audits.len() > 1 {
            self.audits.pop_front().unwrap_or_default()
        } else {
            self.audits.front().cloned().unwrap_or_default()
        }
    }
    fn ask(&mut self, words: &AskWords) -> Result<String, String> {
        self.calls.push(format!("ask {}", words.key));
        if self.undelivered > 0 {
            self.undelivered -= 1;
            return Err("no aterm window answered".into());
        }
        self.asks.push(words.key.clone());
        Ok("OK message=7".into())
    }
    fn resolve(&mut self, key: &str) -> Result<String, String> {
        self.resolved.push(key.to_string());
        Ok("OK done=7 how=resolved-ok".into())
    }
    fn withdraw(&mut self, key: &str) -> Result<String, String> {
        self.withdrawn.push(key.to_string());
        Ok("OK done=7 how=withdrawn".into())
    }
    fn origin_tail(&mut self) -> Option<u64> {
        Some(self.tail)
    }
    fn reserve(&mut self, c: &Candidate) -> Result<Reservation, Refusal> {
        self.calls.push("reserve".into());
        let build = (self.tail + 1).max(1_791_600_000);
        Ok(Reservation {
            build,
            commit: format!("e{}", &c.commit[1..]),
            line: format!("{build} {}", c.version),
        })
    }
    fn qualify_begin(&mut self, _prover: &Prover, export: bool) -> Result<String, Refusal> {
        if self.stop_at_qualify {
            self.qualify_boundary = Some(std::time::Instant::now());
            return Err(Refusal::new(
                "offline fixture reached qualify",
                "fixture complete",
            ));
        }
        Ok(format!(
            "8 slots of this Mac's 8 cores{}",
            if export {
                ", 2 held for the export"
            } else {
                ""
            }
        ))
    }
    fn qualify_end(&mut self) {}
    fn branches(&mut self) -> Box<dyn Branches + '_> {
        Box::new(WorldBranches {
            w: std::sync::Mutex::new(self),
            entered: std::sync::Mutex::new(Vec::new()),
            moved: std::sync::Condvar::new(),
        })
    }
    fn stage(&mut self, c: &Candidate, _p: &Prover, target: StageTarget) -> Result<Ran, Refusal> {
        self.export(c, target)
    }
    fn publish_begin(&mut self, _p: &Prover) -> Result<(), Refusal> {
        self.source_runs = true;
        // A signal that came during step 7's checks, after the step's own entry.
        self.interrupt_at("publish-begin");
        Ok(())
    }
    fn source_publication(&mut self, c: &Candidate) -> Result<(), Refusal> {
        self.calls.push("source-publication".into());
        if self.source_attested.contains(&c.commit) {
            Ok(())
        } else {
            Err(Refusal::new(
                "the source publication is incomplete: SHA256SUMS.sig is missing",
                "finish pub sign-release aterm, then run the release again",
            ))
        }
    }
    fn cut(
        &mut self,
        args: &[String],
        _p: &Prover,
        _candidate: &Candidate,
        _reserved: Option<&Reservation>,
    ) -> Ran {
        self.calls.push(format!("cut {}", args[3..].join(" ")));
        let resuming = args.iter().any(|a| a == "--resume");
        let Some(reserved) = self.journal().and_then(|j| j.reserved) else {
            if resuming && let Some(cut) = self.cut.as_mut() {
                cut.finished = self.cut_finishes;
                return ran(self.cut_code);
            }
            return ran(1);
        };
        if self.overtaken_at_cut {
            self.overtaken_at_cut = false;
            self.tail = reserved.build;
        }
        if self.cut_refuses_before_its_claim {
            // A pre-claim gate refused (another release's lease): no claim, so no discard here.
            return ran(1);
        }
        if !resuming && reserved.build <= self.tail {
            // The claim lost the ledger's race: the privately signed copy is discarded.
            if self.signed == Some(reserved.build) {
                self.signed = None;
            }
            return ran(1);
        }
        if self.claim_lands_unjournaled {
            self.claim_lands_unjournaled = false;
            self.tail = reserved.build;
            self.history.push(reserved.commit.clone());
            return ran(1);
        }
        let version = self.versions.get(&self.head).cloned().unwrap_or_default();
        if self.interrupt_at("cut") {
            self.tail = reserved.build;
            self.cut = Some(CutState {
                version,
                build: reserved.build,
                finished: false,
            });
            return ran(-1);
        }
        if !self.cut_claims {
            return ran(self.cut_code);
        }
        if !resuming {
            if self.signed == Some(reserved.build) {
                self.adopted += 1;
            } else {
                self.resigned += 1;
            }
        }
        self.tail = reserved.build;
        self.cut = Some(CutState {
            version,
            build: reserved.build,
            finished: self.cut_finishes,
        });
        ran(self.cut_code)
    }
    fn discard_signed(&mut self, version: &str, build: u64) -> Vec<String> {
        self.calls.push(format!("discard-signed {version} {build}"));
        if self.signed == Some(build) {
            self.signed = None;
        }
        if self.prepared_signature == Some(build) {
            self.prepared_signature = None;
        }
        Vec::new()
    }
    fn after_cut(&mut self, version: &str) -> Result<AfterCut, Refusal> {
        self.calls.push(format!("after-cut {version}"));
        let shape = self.after_cut.clone()?;
        let site = crate::release_steps::site_outcome(&self.site, vec!["== site".into()])?;
        Ok(AfterCut {
            note: format!("{shape}; {}", site.note),
        })
    }
    fn after_cut_with_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> (Result<AfterCut, Refusal>, Result<String, Refusal>) {
        let distribution = self.after_cut(&c.version);
        let fixtures = self.land_fixtures(c, reserved, prover);
        (distribution, fixtures)
    }
    fn latest(&mut self) -> Result<Option<String>, String> {
        match &self.cut {
            Some(c) if c.finished && !self.latest_lags => Ok(Some(format!("v{}", c.version))),
            _ => Ok(Some("v0.5.0".into())),
        }
    }
}

#[test]
fn automatic_prefreeze_starts_after_ready_before_publish_only_for_a_cut() {
    let mut w = World::new();
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}");
    assert_eq!(
        std::fs::read_to_string(w.dir.path().join("prefreeze-started")).unwrap(),
        "ready=true publish_entered=false"
    );
    let mut private = World::new();
    let (code, last) = private.run(ReleaseOptions {
        qualify: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}");
    assert!(!private.dir.path().join("prefreeze-started").exists());
}

fn owner_gap() -> Vec<Finding> {
    vec![Finding {
        label: "notary",
        state: Audited::Gap {
            what: "Apple's developer agreement for team X is missing or expired".into(),
            fix: "the team's Account Holder accepts the current agreements".into(),
            owner: true,
        },
    }]
}

fn passes() -> Vec<Finding> {
    vec![Finding {
        label: "notary",
        state: Audited::Pass,
    }]
}

fn machine_gap() -> Vec<Finding> {
    vec![Finding {
        label: "x86 slice",
        state: Audited::Gap {
            what: "no x86_64 std".into(),
            fix: "add it".into(),
            owner: false,
        },
    }]
}

fn lease_held() -> Refusal {
    Refusal::new(
        "release lease — the release lock is held by claim abcdef; nothing was claimed",
        "on the machine that cut it: targo --unverified ship release",
    )
}

#[test]
fn fleet_line_excludes_a_second_cut_and_rechecks_before_publication() {
    let mut live = World::new();
    live.fleet_refusal = Some("CUTTING ELSEWHERE: live progressing peer".into());
    let (code, _) = live.run(bump());
    assert_eq!(code, 4);
    assert_eq!(live.count("stage"), 0);
    assert_eq!(live.count("build"), 0);

    let mut adopted = World::new();
    adopted.fleet_audit_refusal = true;
    let (code, _) = adopted.run(bump());
    assert_ne!(code, 0);
    assert!(adopted.fleet_audits > 0);
    assert_eq!(adopted.count("publish"), 0);
    assert_eq!(adopted.count("cut"), 0);

    // Negative control: the same candidate publishes when it still owns the line.
    let mut owner = World::new();
    let (code, last) = owner.run(bump());
    assert_eq!(code, 0, "{last}");
    assert!(owner.fleet_audits >= 2);
}

#[test]
fn supersession_is_explicit_and_refuses_public_or_live_lines() {
    let setup = || {
        let mut world = World::new();
        world.versions.insert(TIP.into(), "0.6.0".into());
        world.versions.insert(NEXT.into(), "0.6.0".into());
        world.history.insert(1, NEXT.into());
        world.existing_fleet_line = Some(Line::start("0.6.0", NEXT, 100));
        world
    };
    let options = || ReleaseOptions {
        commit: Some(TIP.into()),
        supersede: true,
        ..mac()
    };
    let mut old = setup();
    let (code, last) = old.run(ReleaseOptions {
        commit: Some(TIP.into()),
        ..mac()
    });
    assert_eq!(code, 0, "{last}");
    assert_eq!(old.journal().unwrap().commit, NEXT);
    let mut replacement = setup();
    let (code, last) = replacement.run(options());
    assert_eq!(code, 0, "{last}\n{:?}", replacement.out);
    assert_eq!(replacement.journal().unwrap().commit, TIP);
    assert!(replacement.supersession.is_some());

    let mut live = setup();
    live.fleet_refusal = Some("CUTTING ELSEWHERE: live progressing peer".into());
    let (code, _) = live.run(options());
    assert_eq!(code, 4);
    assert!(live.supersession.is_none());
    assert_eq!(live.count("stage"), 0);

    let mut public = setup();
    public
        .published
        .insert(NEXT.into(), "public-revision".into());
    public.newest = Some(NEXT.into());
    let (code, _) = public.run(options());
    assert_ne!(code, 0);
    assert!(public.supersession.is_none());
    assert_eq!(public.count("stage"), 0);
}

/// Supersession preflight is read-only in both real and dry paths, the candidate's cutter
/// repeats it, and only an entered source write may retain candidate.toml for exact recovery.
#[test]
fn supersession_preflight_and_unknown_write_use_one_candidate_transaction() {
    fn setup() -> World {
        let mut world = World::new();
        world.history = vec![OLD.into(), NEXT.into(), FIX.into(), TIP.into()];
        for commit in [NEXT, FIX, TIP] {
            world.versions.insert(commit.into(), "0.6.0".into());
        }
        world.head = NEXT.into();
        let line = Line::start("0.6.0", NEXT, 100);
        world.existing_fleet_line = Some(line.clone());
        std::fs::create_dir_all(world.release_dir()).unwrap();
        Journal {
            version: line.version.clone(),
            commit: line.head().into(),
            prover: world.prover.bin.display().to_string(),
            trustc: world.prover.commit.clone(),
            started: line.started,
            line: Some(line),
            ..Journal::default()
        }
        .save(&world.release_dir().join(JOURNAL_FILE))
        .unwrap();
        world
    }
    let options = |dry_run| ReleaseOptions {
        commit: Some(TIP.into()),
        supersede: true,
        dry_run,
        ..mac()
    };
    let staged = || {
        Refusal::new(
            "the line has a staged publication; cannot supersede",
            crate::release::fix_remedy(NEXT),
        )
    };

    for dry_run in [false, true] {
        let mut world = setup();
        world.supersede_preflight_refusal = Some(staged());
        let journal_path = world.release_dir().join(JOURNAL_FILE);
        let before = std::fs::read(&journal_path).unwrap();
        let fleet = world.existing_fleet_line.clone();
        let (code, last) = world.run(options(dry_run));
        assert_ne!(code, 0, "{last}");
        assert!(world.out.join("\n").contains("--fix"), "{:#?}", world.out);
        assert_eq!(std::fs::read(&journal_path).unwrap(), before);
        assert!(!world.release_dir().join(SELECTION_FILE).exists());
        assert_eq!(world.head, NEXT);
        assert_eq!(world.existing_fleet_line, fleet);
        assert!(world.supersession.is_none());
        assert_eq!(world.count("supersede-preflight"), 1);
        assert_eq!(world.count("supersede"), 0);
        if dry_run {
            assert_eq!(world.fetches, 0, "dry preflight precedes step 1 fetch");
        } else {
            world.supersede_preflight_refusal = None;
            let (code, last) = world.run(fix(&[FIX]));
            assert_eq!(code, 0, "{last}\n{:#?}", world.out);
            let line = world.journal().unwrap().line();
            assert_eq!(line.base, NEXT);
            assert_eq!(line.hops.len(), 1);
            assert_eq!(line.hops[0].carries, FIX);
        }
    }

    // The replacement cutter repeats preflight. A surface that turns red after launcher
    // dispatch restores the exact checkout and leaves release.toml untouched.
    let mut selected = setup();
    selected.own_cutter = false;
    selected.exec_result = Ok(());
    let journal_path = selected.release_dir().join(JOURNAL_FILE);
    let before = std::fs::read(&journal_path).unwrap();
    assert_eq!(selected.run(options(false)).0, 0);
    assert_eq!(selected.head, TIP);
    assert!(
        selected.candidate_before_tip_move,
        "candidate.toml must hold the exact rollback transaction before HEAD moves"
    );
    assert!(selected.release_dir().join(SELECTION_FILE).exists());
    selected.own_cutter = true;
    selected.supersede_preflight_refusal = Some(staged());
    let (code, last) = selected.run(options(false));
    assert_ne!(code, 0, "{last}");
    assert_eq!(selected.head, NEXT);
    assert_eq!(std::fs::read(&journal_path).unwrap(), before);
    assert!(!selected.release_dir().join(SELECTION_FILE).exists());
    assert_eq!(selected.count("supersede"), 0);

    // The replacement image can fail its own toolchain gates before repeated preflight. Those
    // failures still restore the launcher's exact pre-selection state.
    type Gate = fn(&mut World);
    let gates: [(&str, Gate); 5] = [
        ("branch", |world| world.on_main = false),
        ("clone", |world| {
            world.prepare_clone_refusal = Some(Refusal::new(
                "planted selected clone refusal",
                "restore the clone",
            ));
        }),
        ("origin", |world| world.missing_origin_tip = true),
        ("prover", |world| {
            world.prover_error = Some("planted selected prover refusal".into());
        }),
        ("lease", |world| {
            world.lease_error = Some("planted selected lease refusal".into());
        }),
    ];
    for (gate, block) in gates {
        let mut early = setup();
        early.own_cutter = false;
        early.exec_result = Ok(());
        let journal_path = early.release_dir().join(JOURNAL_FILE);
        let before = std::fs::read(&journal_path).unwrap();
        assert_eq!(early.run(options(false)).0, 0);
        assert_eq!(early.head, TIP);
        assert!(early.release_dir().join(SELECTION_FILE).exists());
        early.own_cutter = true;
        block(&mut early);
        let (code, last) = early.run(options(false));
        assert_ne!(code, 0, "{gate}: {last}");
        assert_eq!(early.head, NEXT, "{gate}");
        assert_eq!(std::fs::read(&journal_path).unwrap(), before, "{gate}");
        assert!(!early.release_dir().join(SELECTION_FILE).exists(), "{gate}");
        assert_eq!(early.count("supersede"), 0, "{gate}");
    }

    // The under-claim recheck can also refuse after candidate selection and gets the same exact
    // rollback.
    let mut late = setup();
    late.fleet_refusal = Some("planted under-claim refusal".into());
    let journal_path = late.release_dir().join(JOURNAL_FILE);
    let before = std::fs::read(&journal_path).unwrap();
    let (code, last) = late.run(options(false));
    assert_ne!(code, 0, "{last}");
    assert!(late.out.join("\n").contains("--fix"), "{:#?}", late.out);
    assert_eq!(late.head, NEXT);
    assert_eq!(std::fs::read(journal_path).unwrap(), before);
    assert!(!late.release_dir().join(SELECTION_FILE).exists());

    // An unreadable candidate may be the only durable identity of an entered source write.
    // --forget therefore preserves its exact bytes instead of treating parse failure as absence.
    let mut malformed = setup();
    let selection = malformed.release_dir().join(SELECTION_FILE);
    let malformed_bytes = b"supersede = [this is torn\n";
    std::fs::write(&selection, malformed_bytes).unwrap();
    let release_before = std::fs::read(malformed.release_dir().join(JOURNAL_FILE)).unwrap();
    let (code, last) = malformed.run(ReleaseOptions {
        forget: true,
        ..mac()
    });
    assert_eq!(code, 3, "{last}");
    assert_eq!(std::fs::read(&selection).unwrap(), malformed_bytes);
    assert_eq!(
        std::fs::read(malformed.release_dir().join(JOURNAL_FILE)).unwrap(),
        release_before
    );

    // Once the source writer is entered, Unknown retains the sole transaction. The exact retry
    // classifies the full durable record before public preflight and never writes it twice.
    let mut unknown = setup();
    unknown.supersede_unknown_once = true;
    let (code, last) = unknown.run(options(false));
    assert_eq!(code, 3, "{last}");
    assert_eq!(unknown.head, TIP);
    let candidate = Journal::load(&unknown.release_dir().join(SELECTION_FILE))
        .unwrap()
        .unwrap();
    assert!(
        candidate
            .supersede
            .as_ref()
            .is_some_and(|txn| txn.cas_unknown)
    );
    let candidate_before = std::fs::read(unknown.release_dir().join(SELECTION_FILE)).unwrap();
    assert_eq!(unknown.count("supersede"), 1);
    let preflights = unknown.count("supersede-preflight");
    let (code, last) = unknown.run(ReleaseOptions {
        forget: true,
        ..mac()
    });
    assert_eq!(code, 3, "{last}");
    assert_eq!(
        std::fs::read(unknown.release_dir().join(SELECTION_FILE)).unwrap(),
        candidate_before,
        "--forget cannot erase an unknown source write"
    );

    unknown.supersede_classify_unknown = true;
    unknown.on_main = false;
    let (code, last) = unknown.run(options(false));
    assert_eq!(code, 3, "{last}");
    assert_eq!(
        std::fs::read(unknown.release_dir().join(SELECTION_FILE)).unwrap(),
        candidate_before,
        "an unavailable authoritative reread retains the exact transaction"
    );
    assert_eq!(unknown.count("supersede"), 1);
    assert_eq!(unknown.count("supersede-preflight"), preflights);
    unknown.supersede_classify_unknown = false;
    unknown.on_main = true;

    // Dry recovery performs the same read-only classification first. A newly red staging
    // surface cannot make it contradict the exact durable record, and dry writes nothing.
    unknown.supersede_preflight_refusal = Some(staged());
    let (code, last) = unknown.run(options(true));
    assert_eq!(code, 0, "{last}\n{:#?}", unknown.out);
    assert_eq!(
        std::fs::read(unknown.release_dir().join(SELECTION_FILE)).unwrap(),
        candidate_before
    );
    assert_eq!(unknown.count("supersede-preflight"), preflights);
    unknown.supersede_preflight_refusal = None;
    let (code, last) = unknown.run(options(false));
    assert_eq!(code, 0, "{last}\n{:#?}", unknown.out);
    assert_eq!(unknown.count("supersede"), 1);
    assert_eq!(unknown.count("supersede-preflight"), preflights);
    assert!(!unknown.release_dir().join(SELECTION_FILE).exists());
    let journal = unknown.journal().unwrap();
    assert_eq!(journal.commit, TIP);
    assert!(journal.supersede.is_none());

    // An authoritative absence clears ambiguity, repeats preflight, and enters the writer once
    // more. The successful second entry is the only retry.
    let mut absent = setup();
    absent.supersede_unknown_absent_once = true;
    assert_eq!(absent.run(options(false)).0, 3);
    assert!(absent.supersession.is_none());
    let preflights = absent.count("supersede-preflight");
    let (code, last) = absent.run(options(false));
    assert_eq!(code, 0, "{last}\n{:#?}", absent.out);
    assert_eq!(absent.count("supersede"), 2);
    assert_eq!(absent.count("supersede-preflight"), preflights + 1);
    assert!(!absent.release_dir().join(SELECTION_FILE).exists());

    // A different durable winner is authoritative: the retry adopts it in HEAD and release.toml
    // instead of restoring the line it retired.
    let mut raced = setup();
    raced.supersede_unknown_once = true;
    assert_eq!(raced.run(options(false)).0, 3);
    let transaction = Journal::load(&raced.release_dir().join(SELECTION_FILE))
        .unwrap()
        .unwrap()
        .supersede
        .unwrap();
    let winner = Line::start("0.6.0", FIX, 300);
    raced.supersession = Some(crate::line::Supersession {
        previous: transaction.previous,
        replacement: winner.clone(),
        reason: "another cutter won".into(),
    });
    raced.existing_fleet_line = Some(winner.clone());
    raced.fleet_selected = Some(winner.clone());
    let preflights = raced.count("supersede-preflight");
    let writes = raced.count("supersede");
    let (code, last) = raced.run(options(false));
    assert_eq!(code, 2, "{last}");
    assert_eq!(raced.head, FIX);
    assert_eq!(raced.journal().unwrap().line(), winner);
    assert!(!raced.release_dir().join(SELECTION_FILE).exists());
    assert_eq!(raced.count("supersede-preflight"), preflights);
    assert_eq!(raced.count("supersede"), writes);

    // Once the source CAS committed, a failed final candidate write reports that the exact
    // cas_unknown transaction remains recoverable; its retry classifies instead of writing twice.
    let mut final_save = setup();
    final_save.supersede_final_save_error = true;
    let (code, last) = final_save.run(options(false));
    assert_eq!(code, 3, "{last}");
    let retained = Journal::load(&final_save.release_dir().join(SELECTION_FILE))
        .unwrap()
        .unwrap();
    assert!(retained.supersede.is_some_and(|txn| txn.cas_unknown));
    let writes = final_save.count("supersede");
    let (code, last) = final_save.run(options(false));
    assert_eq!(code, 0, "{last}\n{:#?}", final_save.out);
    assert_eq!(final_save.count("supersede"), writes);
}

#[test]
fn superseded_journal_refuses_but_plain_resume_adopts_replacement() {
    let mut world = World::new();
    let old = Line::start("0.6.0", NEXT, 100);
    let new = Line::start("0.6.0", TIP, 200);
    world.existing_fleet_line = Some(new.clone());
    world.supersession = Some(crate::line::Supersession {
        previous: old.clone(),
        replacement: new.clone(),
        reason: "broken cutter".into(),
    });
    let candidate = || Candidate {
        commit: NEXT.into(),
        version: "0.6.0".into(),
        why: String::new(),
        bump_from: None,
        carry: Vec::new(),
    };
    let mut c = candidate();
    let mut journal = Some(Journal {
        commit: NEXT.into(),
        version: "0.6.0".into(),
        line: Some(old),
        ..Journal::default()
    });
    assert!(crate::line_claim::follow(&mut world, &mut c, &mut journal).is_err());
    assert_eq!(c.commit, NEXT);
    // Negative control: a clone without the retired journal resumes the new line.
    let mut journal = None;
    crate::line_claim::follow(&mut world, &mut c, &mut journal).unwrap();
    assert_eq!(c.commit, TIP);
    assert_eq!(journal.unwrap().line(), new);
}

#[test]
fn fleet_line_adoption_keeps_the_candidates_head_and_drops_other_trees_records() {
    let mut world = World::new();
    let line = Line::start("0.6.0", NEXT, 123);
    world.fleet_selected = Some(line.clone());
    let mut candidate = Candidate {
        version: "0.6.0".into(),
        commit: BUMP.into(),
        why: "moving main".into(),
        bump_from: None,
        carry: Vec::new(),
    };
    let previous = Journal {
        version: "0.6.0".into(),
        commit: BUMP.into(),
        done: vec!["commit-gates".into()],
        ..Journal::default()
    };
    let mut journal = Some(previous.clone());
    crate::line_claim::select(&mut world, false, &mut candidate, &mut journal, 1000).unwrap();
    assert_eq!(candidate.commit, NEXT);
    let adopted = journal.unwrap();
    assert_eq!(adopted.line(), line);
    assert!(adopted.done.is_empty());
    assert!(adopted.reserved.is_none());
    assert!(adopted.artifact.is_none());

    // Negative control: selecting the existing line preserves its same-tree records.
    world.fleet_selected = None;
    let mut same = Some(previous);
    candidate.commit = BUMP.into();
    crate::line_claim::select(&mut world, false, &mut candidate, &mut same, 1000).unwrap();
    assert_eq!(same.unwrap().done, ["commit-gates"]);
}

#[test]
fn fleet_line_is_read_before_refusing_a_newer_main_tip_than_the_published_source() {
    let setup = || {
        let mut world = World::new();
        world.history.insert(1, NEXT.into());
        world.versions.insert(TIP.into(), "0.6.0".into());
        world.versions.insert(NEXT.into(), "0.6.0".into());
        world.published.insert(NEXT.into(), "e1e1e1e1e1e1".into());
        world.source_attested.insert(NEXT.into());
        world.newest = Some(NEXT.into());
        world
    };
    let mut negative = setup();
    let (code, _) = negative.run(mac());
    assert_ne!(code, 0);
    assert_eq!(negative.count("build"), 0);
    let mut adopter = setup();
    adopter.existing_fleet_line = Some(Line::start("0.6.0", NEXT, 123));
    let (code, last) = adopter.run(mac());
    assert_eq!(code, 0, "{last}\n{:?}", adopter.out);
    assert_eq!(adopter.journal().unwrap().commit, NEXT);
    assert_eq!(adopter.count("bump"), 0);
}

#[test]
fn fleet_line_read_does_not_erase_a_carried_hop_a_failed_write_did_not_publish() {
    let mut world = World::new();
    let base = Line::start("0.6.0", BUMP, 123);
    world.existing_fleet_line = Some(base.clone());
    let mut advanced = base;
    advanced.hops.push(Hop {
        commit: NEXT.into(),
        carries: FIX.into(),
        why: "landed fix".into(),
        paths: vec!["src/lib.rs".into()],
    });
    let mut candidate = Candidate {
        commit: NEXT.into(),
        version: "0.6.0".into(),
        why: "carried fix".into(),
        bump_from: None,
        carry: Vec::new(),
    };
    let mut journal = Some(Journal {
        commit: NEXT.into(),
        version: "0.6.0".into(),
        line: Some(advanced.clone()),
        done: vec!["commit-gates".into()],
        ..Journal::default()
    });
    crate::line_claim::follow(&mut world, &mut candidate, &mut journal).unwrap();
    assert_eq!(candidate.commit, NEXT);
    let journal = journal.unwrap();
    assert_eq!(journal.line(), advanced);
    assert_eq!(journal.done, ["commit-gates"]);
}

#[test]
fn a_toolchain_lease_failure_stops_before_any_release_work() {
    let mut world = World::new();
    world.lease_error = Some("lease IO refused".into());
    let (code, last) = world.run(bump());
    assert_ne!(code, 0);
    assert!(last.contains("lease IO refused"), "{last}");
    assert_eq!(world.calls, ["lease"]);
    assert_eq!(world.observations, ["observe-operational", "prover"]);
}

/// Step 1 seals the build inputs under the lease it just took, and a seal that cannot be taken
/// stops the release there with the build inputs' remedy, not the lease's. CONTROL: a lease
/// that fails stops before the seal (`a_toolchain_lease_failure_stops_…`), and a dry run takes
/// neither.
#[test]
fn a_build_inputs_seal_failure_stops_step_one_with_its_own_remedy() {
    let mut world = World::new();
    world.seal_error = Some("build input changed while it was hashed: /sdk/usr".into());
    let (code, last) = world.run(bump());
    assert_ne!(code, 0);
    assert!(
        last.contains("build input changed while it was hashed"),
        "{last}"
    );
    assert!(
        last.ends_with(". restore the release build inputs and retry"),
        "{last}"
    );
    assert_eq!(world.count("seal-inputs"), 1);
    assert_eq!(world.count("validation-tools"), 0);
    let mut dry = World::new();
    dry.seal_error = Some("never asked".into());
    let (code, last) = dry.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}");
    assert_eq!((dry.count("lease"), dry.count("seal-inputs")), (0, 0));
}

#[test]
fn operational_observation_precedes_prover_network_and_provisioning() {
    let mut world = World::new();
    let (code, _) = world.run(bump());
    assert_eq!(code, 0);
    let observed = world
        .observations
        .iter()
        .position(|&c| c == "observe-operational")
        .unwrap();
    for call in ["prover", "machine", "fetch", "validation-tools"] {
        let position = world.observations.iter().position(|&c| c == call).unwrap();
        assert!(observed < position, "{call} ran before observation");
    }
}

/// Every step runs in order. Step 4's branches run at once, so their calls interleave: the
/// tree's work follows slice admission, and the build's bundle precedes its fixtures.
#[test]
fn a_bump_runs_every_step_in_order_and_ends_released() {
    let mut w = World::new();
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{:#?}", w.out);
    let starts: Vec<_> = w
        .out
        .iter()
        .filter_map(|line| line.strip_prefix("release-event: "))
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|event| {
            event["event"].as_str() == Some("qualify-branch")
                && event["phase"].as_str() == Some("started")
        })
        .filter_map(|event| event["branch"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        starts,
        ["build", "tree", "export"],
        "slices claim before tree admission"
    );
    assert!(
        last.starts_with("RELEASED aterm 0.6.0 (build 1791600000)"),
        "{last}"
    );
    let mut order: Vec<&str> = w
        .calls
        .iter()
        .map(String::as_str)
        .filter(|c| {
            !matches!(
                *c,
                "audit" | "lease" | "seal-inputs" | "cut-gates" | "cut --sign-prepare"
            ) && !c.starts_with("ls-remote")
        })
        .collect();
    let graph = order
        .iter()
        .position(|c| matches!(*c, "verify --tree" | "build" | "stage"))
        .expect("the graph ran");
    let at = |order: &[&str], call: &str| order.iter().position(|c| *c == call);
    assert!(
        at(&order, "build") < at(&order, "stage-fixtures"),
        "the build's bundle, then its fixtures: {order:?}"
    );
    let mut branches: Vec<&str> = order.drain(graph..graph + 4).collect();
    branches.sort_unstable();
    assert_eq!(
        branches,
        ["build", "stage", "stage-fixtures", "verify --tree"],
        "{:?}",
        w.calls
    );
    // Step 7's two sides run at once: their calls come in their threads' order.
    let seven = order
        .iter()
        .position(|c| matches!(*c, "publish" | "cut --sign"))
        .expect("step 7 ran");
    order[seven..seven + 2].sort_unstable();
    let input = format!("input-checks {OLD} {BUMP}");
    assert_eq!(
        order,
        [
            "sync",
            "bump",
            "move cccc",
            input.as_str(),
            "sync",
            "validation-tools",
            "disk-claim 30",
            "commit-gates",
            "reserve",
            "verify --bundle",
            "disk-let-go true",
            "sync",
            "engine-now",
            "candidate-now",
            "cut --sign",
            "publish",
            "source-publication",
            "sync",
            "cut --mac-only",
            "after-cut 0.6.0",
            "land-fixtures",
        ]
    );
    assert_eq!(w.count("audit"), 2, "{:?}", w.calls);
    assert_eq!(w.audited_profiles, [None, None]);
    assert_eq!(
        w.audited_dry,
        [false, false],
        "a real run's audit may test-sign"
    );
    let j = w.journal().unwrap();
    assert_eq!((j.version.as_str(), j.commit.as_str()), ("0.6.0", BUMP));
    for fact in [
        crate::release::COMMIT_GATES,
        crate::release::FIXTURES_STAGED,
        "after-cut",
    ] {
        assert!(j.has(fact), "{fact}: {j:?}");
    }
    assert!(j.bumped && j.finished());
    assert!(
        !w.release_dir().join(RUNNING_FILE).exists(),
        "the claim goes with the run"
    );
}

/// A RELEASE CLAIMS ITS DISK BEFORE ANYTHING OF IT WRITES, UNTIL ITS VERIFY ENDS, AND A STAGE
/// AFTER THAT CLAIMS ITS OWN: one claim, sized from what this machine's recorded releases wrote
/// (45 GiB, above the 30 GiB warm floor) or, with none recorded, the floor, taken before the stage
/// and the build and ended as verify ends, kept where it passed; a verify that cannot run ends it
/// unrecorded. A stage outside it (again before the publish where the engine moved after verify,
/// or of a candidate verified earlier) claims the 10 GiB a release that verified is held to, and
/// ends that claim unrecorded as the stage ends: the 0.157.0 cut's unclaimed re-stage walked the
/// clone twice. No claim is held through an owner's wait. A build and verify that a stage's
/// refresh brings on (R's reservation spent while it staged) take the release's claim before they
/// run. A verified candidate that stages nothing claims nothing; a dry run claims nothing and
/// says each claim a real run would take. NEGATIVE CONTROLS: holding the claim through the
/// publish, claiming a stage outside it as a whole release or not at all, a dry stage that says
/// no claim, a rebuild that claims nothing, a second release claim where one is held, or sizing
/// the claim without the record, fails here.
#[test]
fn a_release_claims_its_disk_until_verify_ends_and_a_later_stage_claims_its_own() {
    let gib = 1u64 << 30;
    let mut w = World::new();
    w.disk_record.push(aterm_capacity::Footprint {
        peak: 45 * gib,
        grew: 45 * gib,
        at_ms: 1,
        dirs: None,
        scope: atpkg::admit_disk::scope_tag(crate::release_disk::SCOPE),
    });
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}");
    let at = |w: &World, call: &str| w.calls.iter().position(|c| c == call);
    let claims = |w: &World| {
        w.calls
            .iter()
            .filter(|c| c.starts_with("disk-claim"))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(claims(&w), ["disk-claim 45"], "{:?}", w.calls);
    assert!(
        ["reserve", "stage", "build", "verify --tree"]
            .iter()
            .all(|call| at(&w, "disk-claim 45") < at(&w, call)),
        "{:?}",
        w.calls
    );
    let go = at(&w, "disk-let-go true").expect("the claim ends once verify passed");
    assert!(
        at(&w, "verify --bundle") < Some(go) && Some(go) < at(&w, "publish"),
        "{:?}",
        w.calls
    );

    // The engine moved after verify: the publish stages R again, under a claim of its own.
    let mut w = World::new();
    w.after_verify = Some(|w| w.engine = "f3f3f3f3f3".into());
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}");
    let staged: Vec<usize> = w
        .calls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.as_str() == "stage")
        .map(|(i, _)| i)
        .collect();
    let (released, restage, ended) = (
        at(&w, "disk-let-go true"),
        at(&w, "disk-claim 10"),
        at(&w, "disk-let-go false"),
    );
    assert!(
        claims(&w) == ["disk-claim 30", "disk-claim 10"]
            && staged.len() == 2
            && at(&w, "verify --bundle") < released
            && released < restage
            && restage < Some(staged[1])
            && Some(staged[1]) < ended
            && ended < at(&w, "publish"),
        "the re-stage runs under a stage's claim, ended before the publish: {:?}",
        w.calls
    );

    w.calls.clear();
    assert_eq!(w.run(mac()).0, 0, "a verified candidate");
    assert!(claims(&w).is_empty(), "{:?}", w.calls);

    let mut w = World::new();
    w.verify_passes = false;
    w.verify_code = 3;
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert_eq!(
        (w.count("disk-claim 30"), w.count("disk-let-go false")),
        (1, 1),
        "nothing recorded: the warm floor, and a verify that could not run ends it unpassed: {:?}",
        w.calls
    );

    // An owner's wait before the publish holds no claim; the re-stage after it claims again.
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap(), owner_gap(), passes()]);
    w.after_sleep = Some(|w| w.engine = "f5f5f5f5f5".into());
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let wait = at(&w, "sleep 600");
    assert!(
        at(&w, "disk-let-go true") < wait
            && wait < at(&w, "disk-claim 10")
            && at(&w, "disk-claim 10") < at(&w, "disk-let-go false")
            && at(&w, "disk-let-go false") < at(&w, "publish"),
        "{:?}",
        w.calls
    );

    let mut w = World::new();
    let (code, last) = w.run(ReleaseOptions {
        prepare: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}");
    assert_eq!(
        (claims(&w), w.count("disk-let-go true")),
        (vec!["disk-claim 30".to_owned()], 1),
        "a prepare's claim ends with its verify: {:?}",
        w.calls
    );
    // publish/.out no longer attests R: its resume stages again, and only stages.
    w.attested = None;
    w.calls.clear();
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..mac()
    });
    assert_eq!(code, 0, "{last}");
    assert!(claims(&w).is_empty(), "{:?}", w.calls);
    assert!(
        w.out.iter().any(|l| l.contains("disk: would claim 10 GiB"))
            && !w.out.iter().any(|l| l.contains("disk: would claim 30 GiB")),
        "a dry run of a stage again says the stage's claim: {:#?}",
        w.out
    );
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert!(
        w.verified() == 0
            && claims(&w) == ["disk-claim 10"]
            && at(&w, "disk-claim 10") < at(&w, "stage")
            && at(&w, "stage") < at(&w, "disk-let-go false")
            && at(&w, "disk-let-go false") < at(&w, "publish"),
        "{:?}",
        w.calls
    );

    // Another release claims past R's reservation while the stage again runs: the build and
    // verify that brings on run under the release's claim, which keeps what they wrote.
    let mut w = World::new();
    let (code, last) = w.run(ReleaseOptions {
        prepare: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}");
    w.attested = None;
    w.after_stage = Some(|w| w.tail = w.journal().unwrap().reserved.unwrap().build);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        claims(&w) == ["disk-claim 10", "disk-claim 30"]
            && at(&w, "stage") < at(&w, "disk-let-go false")
            && at(&w, "disk-let-go false") < at(&w, "disk-claim 30")
            && at(&w, "disk-claim 30") < at(&w, "build")
            && at(&w, "verify --bundle") < at(&w, "disk-let-go true")
            && at(&w, "disk-let-go true") < at(&w, "publish"),
        "{:?}",
        w.calls
    );

    let mut w = World::new();
    w.after_verify = Some(|w| {
        w.head_gate_refusals.insert(
            BUMP.into(),
            vec![Refusal::new(
                "a gate of R's own tree refuses it",
                "land the fix",
            )],
        );
    });
    let (code, last) = w.run(bump());
    assert!(
        last.starts_with("REFUSED at 7/10 publish"),
        "{code}: {last}"
    );
    assert!(
        w.count("publish") == 0 && at(&w, "verify --bundle") < at(&w, "disk-let-go true"),
        "the claim ended with verify, what the run wrote kept: {:?}",
        w.calls
    );
    w.after_verify = None;
    w.head_gate_refusals.clear();
    w.engine = "f1f1f1f1f1".into();
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    let claim = at(&w, "disk-claim 10").expect("the publish's re-stage claims first");
    assert!(
        ["stage", "build", "verify --tree", "verify --bundle"].map(|c| w.count(c)) == [1, 0, 0, 0]
            && claims(&w).len() == 1
            && at(&w, "engine-now") < Some(claim)
            && Some(claim) < at(&w, "stage")
            && at(&w, "stage") < at(&w, "disk-let-go false")
            && at(&w, "disk-let-go false") < at(&w, "publish"),
        "{:?}",
        w.calls
    );

    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap()]);
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}");
    assert!(claims(&w).is_empty(), "{:?}", w.calls);
    assert!(
        w.out.iter().any(|l| l.contains("disk: would claim 30 GiB"))
            && !w.out.iter().any(|l| l.contains("disk: would claim 10 GiB")),
        "{:#?}",
        w.out
    );

    let mut w = World::new();
    w.claim_refusal = Some(Refusal::new(
        "the release's 30 GiB can never fit on /c",
        "free space, then re-run",
    ));
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 4/10 qualify"), "{last}");
    assert_eq!(
        (w.count("stage"), w.count("build"), w.verified()),
        (0, 0, 0),
        "{:?}",
        w.calls
    );
}

#[test]
fn a_second_run_finds_everything_done_and_only_checks() {
    let mut w = World::new();
    assert_eq!(w.run(bump()).0, 0);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{:#?}", w.out);
    assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");
    for never in [
        "bump",
        "commit-gates",
        "build",
        "verify --tree",
        "verify --bundle",
        "stage",
        "publish",
        "stage-fixtures",
        "land-fixtures",
    ] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert!(
        !w.calls
            .iter()
            .any(|c| c.starts_with("cut ") || c.starts_with("after-cut")),
        "{:?}",
        w.calls
    );
}

/// A generator that refuses is the build branch's red: step 4 refuses R, the bundle is kept, and
/// the tree's records, made beside it, stand, so the next run stages the fixtures alone.
#[test]
fn a_generator_that_refuses_refuses_the_release_before_its_claim() {
    let mut w = World::new();
    w.generator_refuses = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{:#?}", w.out);
    assert!(last.starts_with("REFUSED at 4/10 qualify"), "{last}");
    assert!(last.contains("fixture generator refuses"), "{last}");
    for call in ["verify --bundle", "publish", "land-fixtures"] {
        assert_eq!(w.count(call), 0, "{call}: {:?}", w.calls);
    }
    assert!(w.cut.is_none(), "nothing claimed");
    assert!(
        w.journal().unwrap().artifact.is_some(),
        "the bundle is kept"
    );

    w.generator_refuses = false;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        (
            w.count("stage-fixtures"),
            w.count("reserve"),
            w.count("build"),
            w.count("verify --tree")
        ),
        (1, 0, 0, 0),
        "{:?}",
        w.calls
    );
    assert!(w.fixtures_on_main);
}

#[test]
fn fixtures_staged_and_not_landed_are_landed_by_the_next_run() {
    let mut w = World::new();
    w.landing_fails = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{:#?}", w.out);
    assert!(last.starts_with("REFUSED at 9/10 after-cut"), "{last}");
    assert!(w.cut.as_ref().is_some_and(|cut| cut.finished));
    assert!(
        w.journal().unwrap().has("after-cut"),
        "the shape half is done"
    );
    assert!(!w.fixtures_on_main);

    w.landing_fails = false;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");
    assert_eq!(
        (
            w.count("land-fixtures"),
            w.count("stage-fixtures"),
            w.count("after-cut 0.6.0")
        ),
        (1, 0, 0),
        "{:?}",
        w.calls
    );
    assert!(w.fixtures_on_main);
}

/// PUBLIC-BEFORE-READER and READER-INDEPENDENT-OF-DISTRIBUTION.
/// NEGATIVE CONTROL: the old site-first early return leaves fixtures off main.
#[test]
fn distribution_refusal_does_not_block_the_independent_main_reader() {
    let mut w = World::new();
    w.site = Ok(crate::site::Outcome::NoDeploy);
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(w.cut.as_ref().is_some_and(|cut| cut.finished));
    assert!(w.fixtures_on_main);
    assert!(!w.journal().unwrap().has("after-cut"));
    w.site = Ok(crate::site::Outcome::Synced { deployed: true });
    w.calls.clear();
    assert_eq!(w.run(mac()).0, 0);
    assert_eq!(w.count("land-fixtures"), 0);
    assert_eq!(w.count("after-cut 0.6.0"), 1);
}

#[test]
fn a_run_stopped_at_any_step_resumes_there_and_repeats_nothing_done() {
    type Break = fn(&mut World);
    type Mend = fn(&mut World);
    let unverified: Mend = |w| {
        w.verify_half = None;
        w.verify_passes = true;
        w.verify_code = 0;
    };
    let cases: [(Step, i32, Break, Mend); 8] = [
        (
            Step::Qualify,
            3,
            |w| {
                w.verify_half = Some(Half::Tree);
                w.verify_passes = false;
                w.verify_code = 3;
            },
            unverified,
        ),
        (
            Step::Bundle,
            3,
            |w| {
                w.verify_half = Some(Half::Bundle);
                w.verify_passes = false;
                w.verify_code = 3;
            },
            unverified,
        ),
        (
            Step::Qualify,
            1,
            |w| w.stage_stages = false,
            |w| w.stage_stages = true,
        ),
        (
            Step::Qualify,
            1,
            |w| w.stage_panics = true,
            |w| w.stage_panics = false,
        ),
        (
            Step::Qualify,
            1,
            |w| w.stage_sync_fails = true,
            |w| w.stage_sync_fails = false,
        ),
        (
            Step::Cut,
            1,
            |w| {
                w.cut_finishes = false;
                w.cut_code = 1;
            },
            |w| {
                w.cut_finishes = true;
                w.cut_code = 0;
            },
        ),
        (
            Step::AfterCut,
            1,
            |w| w.after_cut = Err(Refusal::new("the site hook exited 4", "again later")),
            |w| w.after_cut = Ok("synced".into()),
        ),
        (
            Step::Check,
            1,
            |w| w.latest_lags = true,
            |w| w.latest_lags = false,
        ),
    ];
    for ((step, exit, broken, mend), again) in cases
        .into_iter()
        .flat_map(|case| [(case, mac()), (case, bump())])
    {
        let mut w = World::new();
        broken(&mut w);
        let (code, last) = w.run(bump());
        assert_eq!(code, exit, "{step:?}: {last}");
        assert!(
            last.starts_with(&format!("REFUSED at {}: ", step.at())),
            "{step:?}: {last}"
        );
        assert_eq!(w.out.last(), Some(&last), "the final line is the last line");
        assert!(
            !w.release_dir().join(RUNNING_FILE).exists(),
            "{step:?}: the claim goes with the run"
        );
        let first: Vec<String> = w.calls.clone();
        mend(&mut w);
        w.calls.clear();
        let (code, last) = w.run(again);
        assert_eq!(code, 0, "{step:?}: {last}\n{:#?}", w.out);
        assert!(last.starts_with("RELEASED aterm 0.6.0"), "{step:?}: {last}");
        for (call, done_by) in [
            ("bump", Step::Commit),
            ("reserve", Step::Qualify),
            ("build", Step::Qualify),
            ("verify --tree", Step::Qualify),
            ("stage", Step::Qualify),
            ("verify --bundle", Step::Bundle),
            ("publish", Step::Publish),
        ] {
            if done_by < step && first.iter().any(|c| c == call) {
                assert_eq!(w.count(call), 0, "{step:?} re-ran {call}: {:?}", w.calls);
            }
        }
        if step == Step::Cut {
            assert!(w.calls.iter().any(|c| c == "cut --resume"), "{:?}", w.calls);
            assert_eq!(
                w.count("commit-gates") + w.count("cut-gates"),
                0,
                "{:?}",
                w.calls
            );
        }
        assert_eq!(w.count("bump"), 0, "--bump never bumps twice");
        let j = w.journal().unwrap();
        assert_eq!(j.commit, BUMP, "{step:?}: the candidate is kept");
    }
}

#[test]
fn a_dry_run_changes_nothing_and_asks_nobody() {
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap()]);
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(w.journal().is_none(), "a dry run writes no journal");
    assert!(!w.release_dir().join(RUNNING_FILE).exists());
    for call in [
        "sync",
        "bump",
        "commit-gates",
        "stage",
        "reserve",
        "build",
        "stage-fixtures",
        "verify --tree",
        "verify --bundle",
        "publish",
        "land-fixtures",
    ] {
        assert_eq!(w.count(call), 0, "{call}: {:?}", w.calls);
    }
    assert!(
        !w.audited_dry.is_empty() && w.audited_dry.iter().all(|dry| *dry),
        "a dry run's audit writes nothing: {:?}",
        w.audited_dry
    );
    assert!(
        !w.calls
            .iter()
            .any(|c| { c.starts_with("cut ") || c.starts_with("move") || c.starts_with("ask ") }),
        "{:?}",
        w.calls
    );
}

/// Both entry paths judge the input before qualify; the negative control reaches its dry
/// success summary only after that same check passes.
#[test]
fn unchecked_input_refuses_at_step_one_including_a_dry_run() {
    for dry in [true, false] {
        let mut w = World::new();
        w.input_refusal = Some(Refusal::new(
            "export-scan: planted forbidden content",
            "land the fix",
        ));
        let (code, last) = w.run(ReleaseOptions {
            dry_run: dry,
            ..bump()
        });
        assert_ne!(code, 0, "{last}\n{:?}", w.out);
        assert!(
            w.out
                .iter()
                .any(|line| line.contains("export-scan: planted forbidden content")),
            "{:?}",
            w.out
        );
        assert_eq!(w.count("build"), 0);
        assert_eq!(w.count("verify --tree"), 0);
        assert!(w.calls.iter().any(|call| call.starts_with("input-checks ")));
    }
    let mut w = World::new();
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}");
    assert!(last.contains("would go ahead"), "{last}");
    assert!(w.calls.iter().any(|call| call.starts_with("input-checks ")));
}

#[test]
fn selection_states_the_bases_lane_red_without_refusing_or_reselecting() {
    let mut w = World::new();
    let tip = w.history.last().unwrap().clone();
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(w.out.iter().any(|line| line == &format!("main lane: base {tip}: red, age_s=23 (advisory fixture)")));
    assert!(w.journal().is_none());
    assert_eq!(w.count("build"), 0);
    assert_eq!(w.count("bump"), 0);
}

/// A refusal before step 1 starts a child still creates the log its verdict names, with the
/// native detail that explains why it could not run. NEGATIVE CONTROL: without the refusal-log
/// append in `Run::final_refusal`, `01-checks.log` does not exist and this read fails.
#[test]
fn a_step_one_refusal_keeps_its_detail_in_the_named_log() {
    const DETAIL: &str = "native route made no byte or CPU progress while waiting for git fetch";
    let mut w = World::new();
    w.fetch_refusal = Some((
        1,
        Refusal::new(
            "cannot refresh origin through agh",
            "check the reported native routing or source-state refusal, then re-run",
        )
        .detail([DETAIL.to_string()])
        .could_not_run(),
    ));
    let log = Backend::log_path(&mut w, Step::Checks);
    let (code, last) = w.run(bump());

    assert_eq!(code, 3, "{last}");
    assert!(
        last.starts_with(
            "REFUSED at 1/10 checks: COULD NOT RUN — cannot refresh origin through agh"
        ),
        "{last}"
    );
    assert_eq!(w.fetches, 1);
    let kept = std::fs::read_to_string(&log).expect("the named step log");
    assert!(kept.contains(DETAIL), "{kept:?}");
}

/// An existing local fleet line adds no refresh. The final cutter's single refresh preserves
/// structured evidence. NEGATIVE CONTROLS: duplicate it, or convert its refusal to `r.why`,
/// and either the count changes or the log loses DETAIL.
#[test]
fn an_existing_fleet_line_refreshes_once_and_keeps_its_refusal_detail() {
    const DETAIL: &str =
        "git fetch-batch fixture/app made no progress while waiting on its native fetch step";
    const REMEDY: &str = "check the reported native routing or source-state refusal, then re-run";
    let mut w = World::new();
    w.existing_fleet_line = Some(Line::start("0.6.0", TIP, 999_000));
    w.fetch_refusal = Some((
        1,
        Refusal::new("cannot refresh origin through agh", REMEDY)
            .detail([DETAIL.to_string()])
            .could_not_run(),
    ));
    let log = Backend::log_path(&mut w, Step::Checks);

    let (code, last) = w.run(bump());

    assert_eq!(code, 3, "{last}");
    assert!(
        last.starts_with(
            "REFUSED at 1/10 checks: COULD NOT RUN — cannot refresh origin through agh"
        ),
        "{last}"
    );
    assert!(last.contains(REMEDY), "{last}");
    assert_eq!(
        w.fetches, 1,
        "fleet discovery must not duplicate step 1's refresh"
    );
    let kept = std::fs::read_to_string(&log).expect("the named step log");
    assert!(kept.contains(DETAIL), "{kept:?}");
}

#[test]
fn a_refused_candidate_never_stages_or_publishes() {
    type Change = fn(&mut World);
    let cases: [(&str, Change, &str); 9] = [
        (
            "the clone not on main",
            |w| w.on_main = false,
            "1/10 checks",
        ),
        (
            "a machine refusal",
            |w| w.machine_refusals = vec![Refusal::new("only 4 GiB free on /c", "free space")],
            "1/10 checks",
        ),
        (
            "a machine gap",
            |w| w.audits = VecDeque::from([machine_gap()]),
            "1/10 checks",
        ),
        (
            "the release lease held",
            |w| w.cut_gate_refusals = vec![lease_held()],
            "1/10 checks",
        ),
        (
            "cut on origin",
            |w| {
                w.origin_tags.insert("v0.6.0".into(), NEXT.into());
            },
            "1/10 checks",
        ),
        (
            "a leftover local tag",
            |w| {
                w.local_tags.insert("v0.6.0".into());
            },
            "1/10 checks",
        ),
        (
            "the predecessor's claim not on main",
            |w| {
                w.origin_tags.insert("v0.5.0".into(), NEXT.into());
            },
            "1/10 checks",
        ),
        (
            "main declares a version no release has used: a bump would skip it",
            |w| {
                w.versions.insert(TIP.into(), "0.6.0".into());
            },
            "1/10 checks",
        ),
        (
            "a preflight block",
            |w| {
                w.blocks = vec![Refusal::new(
                    "handoff fixtures — the handoff fixtures of v0.5.0 are not checked in",
                    "generate them at tag v0.5.0",
                )];
            },
            "4/10 qualify",
        ),
    ];
    for (case, change, at) in cases {
        let mut w = World::new();
        change(&mut w);
        let (code, last) = w.run(bump());
        assert_eq!(code, 1, "{case}: {last}");
        assert!(
            last.starts_with(&format!("REFUSED at {at}")),
            "{case}: {last}"
        );
        assert_eq!(w.count("stage"), 0, "{case}: {:?}", w.calls);
        assert!(
            !w.journal()
                .is_some_and(|j| j.has(crate::release::COMMIT_GATES)),
            "{case}: a refused preflight is no fact the cut could read"
        );
        if at == "1/10 checks" {
            assert!(
                w.journal().is_none(),
                "{case}: no judged candidate journaled"
            );
            assert_eq!(
                w.count("publish"),
                0,
                "{case}: no publication before judgment"
            );
        }

        let mut w = World::new();
        change(&mut w);
        let (code, dry) = w.run(ReleaseOptions {
            dry_run: true,
            ..bump()
        });
        assert_eq!(code, 1, "{case}: {dry}");
        assert!(
            dry.contains(&format!("would refuse at {at}")),
            "{case}: {dry}"
        );
    }
}

#[test]
fn an_owner_gap_is_asked_once_the_long_steps_run_and_the_release_waits_then_goes_on() {
    for undelivered in [0, 1] {
        let mut w = World::new();
        w.undelivered = undelivered;
        w.audits = VecDeque::from([owner_gap(), owner_gap(), passes()]);
        let (code, last) = w.run(bump());
        assert_eq!(code, 0, "{last}\n{:#?}", w.out);
        assert_eq!(
            w.count("ask release.notary"),
            1 + undelivered,
            "{:?}",
            w.calls
        );
        assert_eq!(w.asks, ["release.notary"], "delivered exactly once");
        assert_eq!(w.resolved, ["release.notary"], "resolved when it closed");
        assert_eq!(
            w.count("audit"),
            3,
            "two audits and one re-audit: {:?}",
            w.calls
        );
        let at = |call: &str| w.calls.iter().position(|c| c == call).unwrap();
        assert!(
            at("ask release.notary") < at("stage")
                && at("verify --bundle") < at("sleep 600")
                && at("sleep 600") < at("publish"),
            "{:?}",
            w.calls
        );
        assert!(w.journal().unwrap().asks.is_empty());
    }
}

#[test]
fn an_audit_that_could_not_ask_is_a_resumable_stop_where_it_is_needed() {
    let mut w = World::new();
    w.audits = VecDeque::from([vec![Finding {
        label: "notary",
        state: Audited::CouldNotRun("Apple's notary service did not answer".into()),
    }]]);
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert!(
        last.starts_with("REFUSED at 7/10 publish: COULD NOT RUN"),
        "{last}"
    );
    assert_eq!(
        (w.count("verify --tree"), w.count("verify --bundle")),
        (1, 1),
        "the long steps ran before it was needed"
    );
    assert_eq!(w.count("publish"), 0, "{:?}", w.calls);
}

#[test]
fn a_bare_rerun_keeps_the_ready_candidate() {
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap(), owner_gap(), machine_gap()]);
    let (code, _) = w.run(bump());
    assert_eq!(code, 1);
    let j = w.journal().unwrap();
    assert_eq!(j.commit, BUMP);
    assert!(w.complete(), "READY");
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.audits = VecDeque::from([passes()]);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert_eq!(w.journal().unwrap().commit, BUMP, "never chases main");
    assert_eq!(w.verified(), 0);
}

/// A published line, or one whose build is claimed, is the cut's: neither `--commit` nor
/// `--fix` changes it, and nothing is carried. NEGATIVE CONTROL: drop `refuse_published` from
/// `--fix`'s candidate, and the published line takes a hop.
#[test]
fn a_published_candidate_cannot_be_replaced() {
    let mut w = World::new();
    w.cut_finishes = false;
    w.cut_code = 1;
    let _ = w.run(bump());
    w.cut = None;
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    let (code, last) = w.run(ReleaseOptions {
        commit: Some(NEXT.into()),
        ..mac()
    });
    assert_eq!(code, 1, "{last}");
    assert_eq!(
        w.journal().unwrap().commit,
        BUMP,
        "the published candidate stays"
    );

    let fix = ReleaseOptions {
        fix: vec![NEXT.into()],
        ..mac()
    };
    let (code, last) = w.run(fix.clone());
    assert!(
        code == 1 && last.contains("is already published"),
        "published: {last}"
    );
    w.published.clear();
    w.newest = Some(OLD.into());
    w.cut = Some(CutState {
        version: "0.6.0".into(),
        build: 1_791_600_000,
        finished: false,
    });
    let (code, last) = w.run(fix);
    assert!(
        code == 1 && last.contains("is already published"),
        "claimed: {last}"
    );
    assert_eq!(w.count("record-line"), 0, "{:?}", w.calls);
    assert_eq!(w.journal().unwrap().commit, BUMP, "the cut's line stays");
}

/// A line started at G (the bump) went red; main then moved by three commits and the fix
/// landed. `--fix` carries the fix onto G as hop 1 and the run resumes at the new head: the
/// release never re-selects origin/main's tip. THE INSTRUMENT (A18's): re-selections from main
/// after the line started, counted as the commits the clone was moved to that are not the
/// line's own — 0 here, where `--commit origin/main` (the remedy before lines) moved it to the
/// tip, one re-selection absorbing main's three commits. `verify release` runs once per head.
/// NEGATIVE CONTROLS: carry onto a line started at origin/main's tip (a re-selection), and the
/// base is the tip; say `--commit origin/main` after a red, and the remedy names no `--fix`.
#[test]
fn a_fix_after_a_red_carries_a_landed_commit_and_never_reselects_main() {
    let mut w = World::new();
    w.verify_code = 1;
    w.verify_passes = false;
    let (code, last) = w.run(bump());
    assert!(
        code == 1 && last.starts_with("REFUSED at 4/10 qualify"),
        "{last}"
    );
    assert!(
        last.contains("--fix") && !last.contains("--commit origin/main"),
        "a red's way out is a hop: {last}"
    );
    let started = w.journal().unwrap();
    assert_eq!(started.line(), Line::start("0.6.0", BUMP, started.started));
    assert_eq!(started.line.as_ref(), Some(&started.line()), "journaled");

    for moved in [NEXT, N2, N3, FIX] {
        w.history.push(moved.into());
        w.versions.insert(moved.into(), "0.6.0".into());
    }
    w.verify_code = 0;
    w.verify_passes = true;
    let (code, last) = w.run(ReleaseOptions {
        fix: vec![FIX.into()],
        ..mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let hop = hop_of(FIX);
    assert!(
        last.starts_with(&format!(
            "RELEASED aterm 0.6.0 (build 1791600000) from {}",
            &hop[..9]
        )),
        "{last}"
    );
    let j = w.journal().unwrap();
    let line = j.line.clone().expect("the journal keeps the line");
    assert_eq!(line.base, BUMP, "the line never rebases");
    assert_eq!(line.started, started.started, "one line, started once");
    let carried: Vec<&str> = line.hops.iter().map(|h| h.carries.as_str()).collect();
    assert_eq!(carried, [FIX]);
    assert_eq!(
        (j.commit.as_str(), line.head()),
        (hop.as_str(), hop.as_str())
    );

    let own: Vec<String> = std::iter::once(line.base.clone())
        .chain(line.hops.iter().map(|h| h.commit.clone()))
        .collect();
    let moves: Vec<&str> = w
        .calls
        .iter()
        .filter_map(|c| c.strip_prefix("move "))
        .collect();
    let reselections = moves
        .iter()
        .filter(|m| !own.iter().any(|c| c.starts_with(**m)))
        .count();
    assert_eq!(reselections, 0, "moves {moves:?}, line {own:?}");
    assert_eq!(moves, [&BUMP[..4], &hop[..4]]);
    assert_eq!(
        (
            w.count("bump"),
            w.count("carry"),
            w.count("verify --tree"),
            w.count("verify --bundle")
        ),
        (1, 1, 2, 1),
        "the base's tree, then the hop's tree and bundle: {:?}",
        w.calls
    );
}

/// `--fix` naming a commit a hop carries already carries nothing and resumes the line as it
/// is, journal and all. The hop's verify could not run (exit 3), which leaves it resumable; a
/// FAIL would refuse its tree (rf-fail-never-rerolled). NEGATIVE CONTROL: refuse a carried
/// commit instead of dropping it, and the run stops at step 1.
#[test]
fn a_fix_the_line_carries_resumes_it_as_it_is() {
    let mut w = World::new();
    w.verify_code = 1;
    w.verify_passes = false;
    let _ = w.run(bump());
    w.history.push(FIX.into());
    w.versions.insert(FIX.into(), "0.6.0".into());
    let fix = ReleaseOptions {
        fix: vec![FIX.into()],
        ..mac()
    };
    w.verify_code = 3;
    let (code, last) = w.run(fix.clone());
    assert!(
        code == 3 && last.starts_with("REFUSED at 4/10 qualify"),
        "the hop's verify could not run: {last}"
    );
    let before = w.journal().unwrap();
    assert_eq!(before.commit, hop_of(FIX));

    w.verify_code = 0;
    w.verify_passes = true;
    w.calls.clear();
    let (code, last) = w.run(fix);
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("carry"), 0, "{:?}", w.calls);
    assert!(
        w.out
            .iter()
            .any(|l| l.contains("every commit --fix names is carried already")),
        "{:#?}",
        w.out
    );
    let after = w.journal().unwrap();
    assert_eq!(
        (after.commit.as_str(), after.line.as_ref()),
        (before.commit.as_str(), before.line.as_ref())
    );
}

/// With an unpublished line in the clone, `--commit` naming another commit (main's tip, the
/// remedy before lines) refuses and names `--fix` and `--forget`; the journal is untouched.
/// `--commit` naming the line's head resumes it (its verify could not run, exit 3, so its tree
/// holds no FAIL). NEGATIVE CONTROL: remove the refusal, and `--commit origin/main` takes main's
/// tip as the candidate again (a re-selection).
#[test]
fn commit_on_an_unpublished_line_refuses_naming_fix_and_forget() {
    let mut w = World::new();
    w.verify_code = 3;
    w.verify_passes = false;
    let _ = w.run(bump());
    let before = w.journal().unwrap();
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    for rev in ["origin/main", NEXT] {
        let (code, last) = w.run(ReleaseOptions {
            commit: Some(rev.into()),
            ..mac()
        });
        assert!(
            code == 1 && last.starts_with("REFUSED at 1/10 checks"),
            "{rev}: {last}"
        );
        assert!(
            last.contains(&format!("line v0.6.0 at {}", &BUMP[..9]))
                && last.contains("--fix <its commit>")
                && last.contains("--forget"),
            "{rev}: {last}"
        );
        assert_eq!(w.journal().unwrap(), before, "{rev}");
    }
    assert!(
        !w.calls.iter().any(|c| c.starts_with("move dddd")),
        "{:?}",
        w.calls
    );

    w.verify_code = 0;
    w.verify_passes = true;
    let (code, last) = w.run(ReleaseOptions {
        commit: Some(BUMP.into()),
        ..mac()
    });
    assert_eq!(code, 0, "the head resumes: {last}");
    assert_eq!(w.journal().unwrap().commit, BUMP);
}

/// `--fix` needs this clone's unfinished line: with no journal it refuses at step 1 and
/// changes nothing, and a line whose head predates lines (its tree has no line.rs, so the
/// cutter its run executes as keeps no line) refuses too; a tree git cannot read could not run,
/// and names no `--forget`. NEGATIVE CONTROL: drop the `tree_has` test, and the old head takes
/// a hop.
#[test]
fn fix_needs_this_clones_unfinished_line_whose_head_knows_lines() {
    let mut w = World::new();
    w.history.push(FIX.into());
    let fix = ReleaseOptions {
        fix: vec![FIX.into()],
        ..mac()
    };
    let (code, last) = w.run(fix.clone());
    assert!(
        code == 1 && last.starts_with("REFUSED at 1/10 checks"),
        "{last}"
    );
    assert!(
        last.contains("has none") && last.contains("--bump"),
        "{last}"
    );
    assert!(w.journal().is_none());

    let mut w = World::new();
    w.verify_code = 1;
    w.verify_passes = false;
    let _ = w.run(bump());
    w.history.push(FIX.into());
    w.versions.insert(FIX.into(), "0.6.0".into());
    w.pre_line.insert(BUMP.into());
    let (code, last) = w.run(fix.clone());
    assert!(
        code == 1 && last.contains("predates release lines"),
        "{last}"
    );
    assert_eq!(w.count("carry"), 0, "{:?}", w.calls);
    assert_eq!(w.journal().unwrap().commit, BUMP);

    w.pre_line.clear();
    w.tree_unread = true;
    let (code, last) = w.run(fix);
    assert!(
        code == 3
            && last.starts_with("REFUSED at 1/10 checks: COULD NOT RUN — this clone lacks commit")
            && !last.contains("--forget"),
        "a tree git cannot read is no line to drop: {last}"
    );
    assert_eq!(w.count("carry"), 0, "{:?}", w.calls);
    assert_eq!(w.journal().unwrap().commit, BUMP);
}

/// A pending line whose meaning check fails is refused by its frozen head before recording
/// on source main or starting either export. NEGATIVE CONTROL: omit the pending-line proof
/// and the source record and export can run.
#[test]
fn a_line_that_fails_its_meaning_check_is_never_exported() {
    for qualify in [false, true] {
        let mut w = World::new();
        w.versions.insert(TIP.into(), "0.6.0".into());
        w.verify_code = 1;
        w.verify_passes = false;
        let start = ReleaseOptions {
            qualify,
            commit: Some(TIP.into()),
            ..mac()
        };
        let _ = w.run(start);
        w.history.push(FIX.into());
        w.versions.insert(FIX.into(), "0.6.0".into());
        w.line_verify = Err(Refusal::new(
            "hop 1's tree is not 1111's patch onto the previous head",
            "drop the line",
        ));
        w.calls.clear();
        let (code, last) = w.run(ReleaseOptions {
            qualify,
            fix: vec![FIX.into()],
            ..mac()
        });
        assert!(
            code == 1 && last.starts_with("REFUSED at 1/10 checks"),
            "qualify {qualify}: {last}"
        );
        assert!(last.contains("is not 1111's patch"), "{last}");
        assert_eq!(
            (w.count("carry"), w.count("verify-line")),
            (1, 1),
            "{:?}",
            w.calls
        );
        assert_eq!(
            (w.count("stage"), w.count("stage --dry-run")),
            (0, 0),
            "qualify {qualify}: {:?}",
            w.calls
        );
    }
}

/// A line started by `--bump` whose `verify release` went red, then FIX landed on main and
/// `verify release` turns green.
fn red_line() -> World {
    let mut w = World::new();
    w.verify_code = 1;
    w.verify_passes = false;
    let (code, last) = w.run(bump());
    assert!(
        code == 1 && last.starts_with("REFUSED at 4/10 qualify"),
        "{last}"
    );
    w.history.push(FIX.into());
    w.versions.insert(FIX.into(), "0.6.0".into());
    w.verify_code = 0;
    w.verify_passes = true;
    w
}

fn fix(commits: &[&str]) -> ReleaseOptions {
    ReleaseOptions {
        fix: commits.iter().map(|c| (*c).to_string()).collect(),
        ..mac()
    }
}

/// Run the release flow against real carry plumbing, with every external service faked.
pub(super) fn real_carry_fixture(
    repo: &Path,
    line: &Line,
    fixes: &[Fix],
    dry_run: bool,
) -> (i32, String, Vec<String>) {
    let mut w = World::new();
    w.carry_repo = Some(repo.to_path_buf());
    w.verify_code = 0;
    w.verify_passes = true;
    w.head = line.head().into();
    for commit in std::iter::once(line.base.as_str())
        .chain(std::iter::once(line.head()))
        .chain(fixes.iter().map(|fix| fix.commit.as_str()))
    {
        w.history.push(commit.into());
        w.versions.insert(commit.into(), line.version.clone());
    }
    std::fs::create_dir_all(w.release_dir()).unwrap();
    Journal {
        version: line.version.clone(),
        commit: line.head().into(),
        prover: w.prover.bin.display().to_string(),
        trustc: w.prover.commit.clone(),
        started: w.now,
        line: Some(line.clone()),
        ..Journal::default()
    }
    .save(&w.release_dir().join(JOURNAL_FILE))
    .unwrap();
    let before = w.journal().unwrap();
    let names: Vec<&str> = fixes.iter().map(|fix| fix.commit.as_str()).collect();
    let (code, last) = w.run(ReleaseOptions {
        dry_run,
        ..fix(&names)
    });
    if dry_run || code != 0 {
        assert_eq!(w.journal().unwrap(), before);
    }
    (code, last, w.out)
}

/// A dry `--fix` constructs the head step 3 would make, but moves and journals nothing, and
/// reads none of the old head's journaled facts (its preflight, its reserved build) as the new
/// head's, so it agrees with the real run, which journals the new head afresh — going ahead,
/// and refusing at the preflight. NEGATIVE CONTROL: let the journal's facts stand for R while
/// fixes are still to carry, and the dry run reads the preflight off the journal and says the
/// red head's build is reserved, going ahead where the real run refuses.
#[test]
fn a_dry_fix_plans_the_new_head_and_agrees_with_the_real_run() {
    let dry = ReleaseOptions {
        dry_run: true,
        ..fix(&[FIX])
    };

    let mut w = red_line();
    let before = w.journal().unwrap();
    assert!(
        before.has(crate::release::COMMIT_GATES) && before.reserved.is_some(),
        "the red head's own facts: {before:?}"
    );
    w.calls.clear();
    let (code, last) = w.run(dry.clone());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.contains("a real run would go ahead"), "{last}");
    assert_eq!(w.journal().unwrap(), before, "a dry run journals nothing");
    assert_eq!(w.count("carry"), 1);
    for call in [
        "stage",
        "reserve",
        "build",
        "verify --tree",
        "verify --bundle",
        "verify-line",
    ] {
        assert_eq!(w.count(call), 0, "{call}: {:?}", w.calls);
    }
    assert!(
        !w.calls.iter().any(|c| c.starts_with("move")),
        "{:?}",
        w.calls
    );
    assert_eq!(
        w.count("commit-gates-dry"),
        1,
        "the preflight is asked, not read off the old head's journal: {:?}",
        w.calls
    );
    let said = |needle: &str| w.out.iter().any(|l| l.contains(needle));
    for needle in [
        "111111111 carries cleanly as hop 1",
        "asked of the line's head ccccccccc before the carry",
        "would reserve build n",
        "the line's new head is not made yet",
        "pub stage aterm --commit <the line's new head>",
        "would check the line's new head with line::verify",
    ] {
        assert!(said(needle), "{needle}: {:#?}", w.out);
    }
    assert!(
        !said("(this release's journal)"),
        "no fact of the old head is the new head's: {:#?}",
        w.out
    );
    let (code, last) = w.run(fix(&[FIX]));
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");

    let mut w = red_line();
    w.blocks = vec![Refusal::new(
        "changelog: CHANGELOG.md has no 0.6.0 section",
        "add it on main",
    )];
    let before = w.journal().unwrap();
    let (code, dry_last) = w.run(dry);
    assert_eq!(code, 1, "{dry_last}\n{:#?}", w.out);
    assert!(
        dry_last.contains("would refuse at 4/10 qualify: the preflight BLOCKED"),
        "{dry_last}"
    );
    assert_eq!(w.journal().unwrap(), before);
    assert_eq!(w.count("carry"), 1, "{:?}", w.calls);
    let (code, last) = w.run(fix(&[FIX]));
    assert!(
        code == 1 && last.starts_with("REFUSED at 4/10 qualify: the preflight BLOCKED"),
        "the real run refuses where the dry one said: {last}"
    );
}

/// The cut's gates of R's own tree (its handoff fixtures and policy) are asked at step 1 of the
/// head the release goes on with: a bare re-run of a head that fails them refuses there, naming
/// `--fix`, and that `--fix` is not refused by the head it replaces: preparation carries it, and the
/// new head meets those gates. NEGATIVE CONTROL: ask them of the old head while its fixes are
/// still to carry, and `--fix` refuses at step 1 with the remedy that named it.
#[test]
fn a_fix_is_never_judged_by_the_head_it_replaces() {
    let mut w = red_line();
    w.verify_code = 1;
    w.verify_passes = false;
    let (code, last) = w.run(fix(&[FIX]));
    assert!(
        code == 1 && last.starts_with("REFUSED at 4/10 qualify"),
        "{last}"
    );
    let hop1 = hop_of(FIX);
    w.head_gate_refusals.insert(
        hop1.clone(),
        vec![Refusal::new(
            "handoff policy: v0.6.0 moves the handoff policy without its fixture",
            "land the fixture on main",
        )],
    );
    let (code, last) = w.run(mac());
    assert!(
        code == 1
            && last.starts_with("REFUSED at 1/10 checks: the cut would refuse")
            && last.contains("--fix <the fix's own non-merge commit>"),
        "{last}"
    );
    assert!(
        w.out.iter().any(|l| l.contains("handoff policy")),
        "{:#?}",
        w.out
    );

    w.history.push(FIX2.into());
    w.versions.insert(FIX2.into(), "0.6.0".into());
    w.verify_code = 0;
    w.verify_passes = true;
    w.head_gated.clear();
    let (code, last) = w.run(fix(&[FIX2]));
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let hop2 = hop_of(FIX2);
    assert_eq!(w.count("carry"), 2, "{:?}", w.calls);
    assert_eq!(w.journal().unwrap().commit, hop2);
    assert!(
        !w.head_gated.is_empty() && w.head_gated.iter().all(|c| *c == hop2),
        "asked of the new head alone: {:?}",
        w.head_gated
    );
}

/// A carry that cannot be made refuses during preparation with
/// its own words: the journal keeps the line as it was, the clone is moved nowhere, and nothing
/// is staged or verified. NEGATIVE CONTROL: swallow the carry's refusal, and the red head is
/// staged and verified again.
#[test]
fn a_carry_that_cannot_be_made_refuses_before_judgment_and_keeps_the_line() {
    let mut w = red_line();
    let before = w.journal().unwrap();
    w.carry_refusal = Some(Refusal::new(
        "111111111111 (fix 1111) does not carry onto the line's head: it conflicts in \
         src/fixed.rs",
        "author it on ccccccccc and land it",
    ));
    w.calls.clear();
    let (code, last) = w.run(fix(&[FIX]));
    assert!(
        code == 1
            && last.starts_with("REFUSED at 1/10 checks")
            && last.contains("it conflicts in src/fixed.rs"),
        "{last}"
    );
    assert_eq!(w.journal().unwrap(), before, "the line is kept as it was");
    assert_eq!(w.count("carry"), 1, "{:?}", w.calls);
    assert!(
        !w.calls.iter().any(|c| c.starts_with("move")),
        "{:?}",
        w.calls
    );
    for call in [
        "commit-gates",
        "verify-line",
        "stage",
        "verify --tree",
        "verify --bundle",
    ] {
        assert_eq!(w.count(call), 0, "{call}: {:?}", w.calls);
    }
}

/// Every commit `--fix` names is admitted before anything is carried: one that cannot be (a
/// merge, off main, a frozen path: `line::admit`'s refusals), or a name that is no commit,
/// refuses at step 1 and carries none of them, the admissible ones included. NEGATIVE CONTROL:
/// skip a commit admission refuses, and the run carries the rest.
#[test]
fn a_fix_admission_refuses_at_step_one_and_carries_nothing() {
    let mut w = red_line();
    w.history.push(FIX2.into());
    w.versions.insert(FIX2.into(), "0.6.0".into());
    let before = w.journal().unwrap();
    w.unfit.insert(
        FIX.into(),
        Refusal::new(
            "111111111111 is a merge: a hop carries one non-merge commit",
            "name the landing's own commits",
        ),
    );
    for named in [vec![FIX], vec![FIX2, FIX], vec!["no-such-commit"]] {
        let (code, last) = w.run(fix(&named));
        assert!(
            code == 1 && last.starts_with("REFUSED at 1/10 checks"),
            "{named:?}: {last}"
        );
        let why = if named.contains(&FIX) {
            "is a merge"
        } else {
            "names no commit of this clone"
        };
        assert!(last.contains(why), "{named:?}: {last}");
        assert_eq!(w.count("carry"), 0, "{named:?}: {:?}", w.calls);
        assert_eq!(w.journal().unwrap(), before, "{named:?}");
    }
}

/// A hop origin/main no longer holds (main rewritten without the line's record merge) cannot
/// ship: step 1 says so and names `--forget`, whether the clone's main is still at the hop or
/// was reset to origin/main. A commit with no hop that main lacks is to be landed instead.
/// NEGATIVE CONTROL: name no line, and the hop's clone is told to push its main.
#[test]
fn a_hop_main_no_longer_holds_drops_the_line() {
    let mut w = red_line();
    w.verify_code = 1;
    w.verify_passes = false;
    let _ = w.run(fix(&[FIX]));
    let hop = hop_of(FIX);
    let merge = format!("8{}", &hop[1..]);
    assert_eq!(w.journal().unwrap().commit, hop);
    w.history.retain(|c| *c != hop && *c != merge);
    let before = w.journal().unwrap();
    for at in ["its hop", "origin/main"] {
        if at == "origin/main" {
            w.head = w.tip();
        }
        assert_eq!(w.head == hop, at == "its hop");
        let (code, last) = w.run(mac());
        assert!(
            code == 1
                && last.starts_with("REFUSED at 1/10 checks")
                && last.contains("no longer holds the line's record merge")
                && last.contains("--forget"),
            "{at}: {last}"
        );
        assert!(!last.contains("git push origin main"), "{at}: {last}");
        assert_eq!(w.journal().unwrap(), before, "{at}");
    }

    let mut w = World::new();
    w.verify_code = 1;
    w.verify_passes = false;
    let _ = w.run(bump());
    w.history.retain(|c| c != BUMP);
    w.head = w.tip();
    let (code, last) = w.run(mac());
    assert!(
        code == 1
            && last.contains("is not on origin/main")
            && last.contains("land it on main first"),
        "{last}"
    );
}

/// A remedy that starts a line (`--commit`, `--bump`) names `--forget` first while this clone
/// holds an unfinished line, which `--commit` refuses to replace and `--bump` resumes: the
/// predecessor's claim missing from the line's history, an unfinished cut of another version.
/// The remedy then goes through; a pending selection is held until forgotten too. NEGATIVE CONTROL:
/// drop the `--forget`, and the remedy's `--commit` refuses with "only starts a line".
#[test]
fn a_remedy_that_starts_a_line_drops_the_held_line_first() {
    let mut w = red_line();
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.origin_tags.insert("v0.5.0".into(), NEXT.into());
    let before = w.journal().unwrap();
    let (code, last) = w.run(mac());
    assert!(
        code == 1 && last.contains("does not contain v0.5.0's claim commit"),
        "{last}"
    );
    let remedy = format!(
        "{} --forget, then {} --commit origin/main",
        crate::ledger::RELEASE_COMMAND,
        crate::ledger::RELEASE_COMMAND
    );
    assert!(last.contains(&remedy), "{last}");
    assert_eq!(w.journal().unwrap(), before);
    let (code, last) = w.run(ReleaseOptions {
        commit: Some("origin/main".into()),
        ..mac()
    });
    assert!(
        code == 1 && last.contains("only starts a line"),
        "the remedy without --forget is a dead end: {last}"
    );
    let (code, last) = w.run(ReleaseOptions {
        forget: true,
        ..mac()
    });
    assert_eq!(code, 0, "{last}");
    let (code, last) = w.run(ReleaseOptions {
        commit: Some("origin/main".into()),
        ..mac()
    });
    assert_eq!(code, 0, "the remedy goes through: {last}\n{:#?}", w.out);

    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.origin_tags.insert("v0.5.0".into(), NEXT.into());
    let (code, last) = w.run(mac());
    assert!(
        code == 1
            && last.contains("does not contain v0.5.0's claim commit")
            && last.contains("--forget"),
        "the selected candidate must be forgotten before replacing it: {last}"
    );

    let mut w = red_line();
    w.cut = Some(CutState {
        version: "0.5.0".into(),
        build: 1_791_400_000,
        finished: false,
    });
    let (code, last) = w.run(mac());
    assert!(
        code == 1
            && last.contains("holds an unfinished cut of v0.5.0")
            && last.contains(&format!(
                "finish it: {} --forget, then {} --commit <its published commit>",
                crate::ledger::RELEASE_COMMAND,
                crate::ledger::RELEASE_COMMAND
            )),
        "{last}"
    );
}

#[test]
fn a_second_run_in_the_same_clone_is_refused_naming_the_first() {
    let mut w = World::new();
    std::fs::create_dir_all(w.release_dir()).unwrap();
    std::fs::write(
        w.release_dir().join(RUNNING_FILE),
        "pid 777\nstarted Thu Oct  9 01:00:00 2026\nlogs /l\n",
    )
    .unwrap();
    w.alive.insert(777, "Thu Oct  9 01:00:00 2026".into());
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with("REFUSED at 1/10 checks") && last.contains("pid 777"),
        "{last}"
    );
    assert!(w.calls.is_empty(), "{:?}", w.calls);
    w.alive.insert(777, "Fri Oct 10 01:00:00 2026".into());
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
    assert!(
        w.release_dir()
            .join(format!("{RUNNING_FILE}.stale"))
            .exists()
    );
}

/// A cutter built from another commit asks the backend to replace this image exactly once with
/// R's frozen cutter. A pre-exec refusal ends preparation before step 1; the successful exec edge
/// returns no final line because the real backend never returns from it.
/// NEGATIVE CONTROLS: exec a commit other than R, call exec twice, or continue into validation
/// work in the old image, and these assertions fail.
#[test]
fn a_foreign_cutter_execs_once_at_r_and_only_a_refusal_returns() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.own_cutter = false;
    let tip = ReleaseOptions {
        commit: Some("origin/main".into()),
        ..mac()
    };
    let claim = w.release_dir().join(RUNNING_FILE);

    let (code, last) = w.run(tip.clone());
    assert_eq!(w.count("exec-cutter"), 1, "{:?}", w.calls);
    assert_eq!(w.exec_commit.as_deref(), Some(TIP));
    assert!(
        code == 1 && last.starts_with("REFUSED at 1/10 checks"),
        "an exec that could not begin: {last}"
    );
    assert!(
        !claim.exists(),
        "a failed exec returned, so this image removes its claim"
    );

    w.exec_result = Ok(());
    w.exec_commit = None;
    w.calls.clear();
    let (code, last) = w.run(tip);
    assert_eq!(
        (code, last.as_str()),
        (0, ""),
        "the successfully exec'd image, not this one, owns the final line"
    );
    assert_eq!(w.count("exec-cutter"), 1, "{:?}", w.calls);
    assert_eq!(w.exec_commit.as_deref(), Some(TIP));
    assert_eq!(
        w.count("validation-tools"),
        0,
        "the old image does no work after exec: {:?}",
        w.calls
    );
    assert!(
        !claim.exists(),
        "the fake success edge returns and drops it; a real exec retains the process"
    );
}

/// NEGATIVE CONTROL: put checks before prepare_candidate and the launcher's planted network
/// failure prevents exec; judge them again after exec and the final count exceeds one.
#[test]
fn only_the_frozen_candidate_judges_step_one() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.own_cutter = false;
    w.exec_result = Ok(());
    w.fetch_refusal = Some((
        1,
        Refusal::new("planted refresh defect", "use the candidate cutter"),
    ));
    let options = ReleaseOptions {
        commit: Some(TIP.into()),
        qualify: true,
        ..mac()
    };
    assert_eq!(w.run(options.clone()).0, 0);
    assert_eq!(w.exec_commit.as_deref(), Some(TIP));
    assert_eq!(w.fetches, 0);
    assert_eq!(w.count("seal-inputs"), 0);
    assert!(!w.observations.contains(&"machine"));
    assert_eq!(w.count("record-line"), 0);

    // Model the replacement image: its fixed refresh answers, and its own step 1 alone runs.
    w.own_cutter = true;
    w.fetch_refusal = None;
    assert_eq!(w.run(options).0, 0, "{:#?}", w.out);
    assert_eq!(w.fetches, 1);
    assert_eq!(w.count("seal-inputs"), 1);
    assert_eq!(
        w.observations
            .iter()
            .filter(|&&item| item == "machine")
            .count(),
        1
    );
    assert!(
        w.observations.iter().position(|&item| item == "warm-tools")
            < w.observations.iter().position(|&item| item == "machine")
    );
}

/// NEGATIVE CONTROL: invoking sync or bump before dispatch lets the previous line's engine
/// code decide whether a new line can start.
#[test]
fn a_bump_dispatches_to_its_base_before_touching_the_engine() {
    let mut w = World::new();
    w.own_cutter = false;
    w.exec_result = Ok(());
    assert_eq!(w.run(bump()).0, 0);
    assert_eq!(w.exec_commit.as_deref(), Some(TIP));
    assert_eq!(w.count("sync"), 0);
    assert_eq!(w.count("bump"), 0);
    assert_eq!(w.fetches, 0);
    assert_eq!(w.count("seal-inputs"), 0);
}

#[test]
fn supersession_is_performed_by_its_replacement_cutter_once() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.history.insert(1, NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.existing_fleet_line = Some(Line::start("0.6.0", NEXT, 100));
    let options = ReleaseOptions {
        commit: Some(TIP.into()),
        supersede: true,
        ..mac()
    };
    let args = vec!["release".into(), "--supersede".into()];
    assert!(
        options
            .dispatch_args(args.clone())
            .iter()
            .any(|arg| arg == "--supersede")
    );
    assert!(
        !options
            .exec_args(args)
            .iter()
            .any(|arg| arg == "--supersede")
    );
    w.own_cutter = false;
    w.exec_result = Ok(());
    assert_eq!(w.run(options.clone()).0, 0);
    assert_eq!(w.exec_commit.as_deref(), Some(TIP));
    assert_eq!(
        w.count("supersede"),
        0,
        "the launcher must not retire a line"
    );
    assert_eq!(w.fetches, 0);
    w.own_cutter = true;
    let (code, last) = w.run(options.clone());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("supersede-preflight"), 2);
    assert_eq!(w.count("supersede"), 1);
    let (code, last) = w.run(options);
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        w.count("supersede"),
        1,
        "a completed request is not repeated"
    );
}

#[test]
fn a_fresh_proposal_adopts_a_durable_replacement_without_becoming_a_retired_journal() {
    let mut w = World::new();
    for commit in [TIP, NEXT] {
        w.versions.insert(commit.into(), "0.6.0".into());
    }
    w.history.insert(1, NEXT.into());
    let previous = Line::start("0.6.0", OLD, 50);
    let replacement = Line::start("0.6.0", NEXT, 100);
    w.supersession = Some(crate::line::Supersession {
        previous,
        replacement: replacement.clone(),
        reason: "explicit retirement".into(),
    });
    w.existing_fleet_line = Some(replacement);
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.journal().unwrap().commit, NEXT);
    assert_eq!(w.count("supersede"), 0);
}

/// NEGATIVE CONTROL: releasing the Run before stop_tools either leaves the compiler active,
/// or fails the fake's assertion that cleanup still owns the clone claim.
#[test]
fn a_step_one_refusal_settles_tools_before_releasing_the_clone() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.machine_refusals = vec![Refusal::new("planted machine refusal", "retry")];
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}");
    assert!(w.observations.contains(&"warm-tools"));
    assert!(!w.warming_active);
    assert!(!w.release_dir().join(RUNNING_FILE).exists());
}

/// NEGATIVE CONTROLS: checking the old roster, refreshing before carrying, recording the hop
/// from the launcher, or executing the old head each breaks a different assertion here.
#[test]
fn a_fix_reaches_its_own_step_one_before_any_remote_read_or_record() {
    let mut w = red_line();
    w.calls.clear();
    w.observations.clear();
    let fetches = w.fetches;
    w.own_cutter = false;
    w.exec_result = Ok(());
    w.fetch_refusal = Some((
        fetches + 1,
        Refusal::new("old step 1 cannot refresh", "carry its fix"),
    ));
    assert_eq!(w.run(fix(&[FIX])).0, 0);
    assert_eq!(w.exec_commit.as_deref(), Some(hop_of(FIX).as_str()));
    assert_eq!(w.fetches, fetches);
    assert_eq!(w.count("carry"), 1);
    assert_eq!(w.count("record-line"), 0);
    assert!(!w.observations.contains(&"machine"));
    assert_eq!(
        w.journal().unwrap().commit,
        BUMP,
        "the judged line stays until the new head passes"
    );

    w.own_cutter = true;
    w.fetch_refusal = None;
    w.verify_passes = true;
    w.verify_code = 0;
    assert_eq!(w.run(mac()).0, 0, "{:#?}", w.out);
    assert_eq!(w.count("record-line"), 1);
    assert_eq!(w.fetches, fetches + 1);
    assert_eq!(w.journal().unwrap().commit, hop_of(FIX));
}

#[test]
fn a_pending_fix_survives_step_one_failure_without_a_fleet_record_deadlock() {
    let mut w = red_line();
    w.machine_refusals = vec![Refusal::new("planted step-one refusal", "retry")];
    let (code, last) = w.run(fix(&[FIX]));
    assert_eq!(code, 1, "{last}");
    assert!(w.unrecorded_hops.contains(&hop_of(FIX)));
    assert_eq!(w.journal().unwrap().commit, BUMP);
    w.existing_fleet_line = Some(w.journal().unwrap().line());
    w.machine_refusals.clear();
    w.verify_passes = true;
    w.verify_code = 0;
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(!w.unrecorded_hops.contains(&hop_of(FIX)));
    assert_eq!(w.journal().unwrap().commit, hop_of(FIX));
}

#[test]
fn fleet_adoption_recarries_the_pending_fixes_on_its_selected_base() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.verify_passes = false;
    w.verify_code = 1;
    assert_eq!(
        w.run(ReleaseOptions {
            commit: Some(TIP.into()),
            ..mac()
        })
        .0,
        1
    );
    for commit in [N2, FIX] {
        w.history.push(commit.into());
        w.versions.insert(commit.into(), "0.6.0".into());
    }
    w.fleet_selected = Some(Line::start("0.6.0", N2, 999_000));
    w.verify_passes = true;
    w.verify_code = 0;
    let (code, last) = w.run(fix(&[FIX]));
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let line = w.journal().unwrap().line();
    assert_eq!(line.base, N2, "the fleet's exact base is retained");
    assert_eq!(line.hops.len(), 1);
    assert_eq!(
        line.hops[0].carries, FIX,
        "the requested fix is not lost at adoption"
    );
    assert_eq!(w.count("carry"), 2);
    assert_eq!(w.count("rewind-selection"), 1);
}

/// A world at the boundary after the old image chose R and exec'd R's cutter. The claim names
/// the same process, while origin/main has moved beyond the journaled R.
fn world_after_cutter_exec(same_process_lifetime: bool) -> World {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    std::fs::create_dir_all(w.release_dir()).unwrap();
    Journal {
        version: "0.6.0".into(),
        commit: TIP.into(),
        prover: w.prover.bin.display().to_string(),
        trustc: w.prover.commit.clone(),
        started: w.now,
        line: Some(Line::start("0.6.0", TIP, w.now)),
        ..Journal::default()
    }
    .save(&w.release_dir().join(JOURNAL_FILE))
    .unwrap();
    let claim_started = if same_process_lifetime {
        w.started.clone()
    } else {
        "Thu Oct  9 01:59:59 2026".into()
    };
    std::fs::write(
        w.release_dir().join(RUNNING_FILE),
        format!(
            "pid {}\nstarted {}\nlogs /previous-image\n",
            w.pid, claim_started
        ),
    )
    .unwrap();
    w
}

/// The image exec'd into the exact same pid and process start continues the claim, records the
/// preceding image's logs, and keeps the journaled R even when the repeated command line names
/// a newer origin/main. The same pid with another start is stale, not a continuation.
/// NEGATIVE CONTROLS: compare only the pid or reselect the command-line commit after exec, and
/// the second half or the released commit fails.
#[test]
fn an_execed_cutter_continues_only_the_exact_process_and_keeps_r() {
    let options = ReleaseOptions {
        commit: Some("origin/main".into()),
        ..mac()
    };
    let mut w = world_after_cutter_exec(true);
    w.audits = VecDeque::from([passes()]);
    let (code, last) = w.run(options.clone());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with(&format!(
            "RELEASED aterm 0.6.0 (build 1791600000) from {}",
            &TIP[..9]
        )),
        "{last}"
    );
    assert_eq!(w.journal().unwrap().commit, TIP, "never chases main");
    assert_eq!(w.count("exec-cutter"), 0, "{:?}", w.calls);
    let continued_from: Vec<_> = w
        .out
        .iter()
        .filter_map(|line| line.strip_prefix("release-event: "))
        .filter_map(|json| aterm_json::from_str::<aterm_json::Value>(json).ok())
        .filter(|event| event["event"].as_str() == Some("continued"))
        .map(|event| event["from"].as_str().unwrap_or("?").to_owned())
        .collect();
    assert_eq!(continued_from, ["/previous-image"]);

    let mut stale = world_after_cutter_exec(false);
    let (code, last) = stale.run(options);
    assert_ne!(code, 0, "{last}");
    assert!(
        stale
            .release_dir()
            .join(format!("{RUNNING_FILE}.stale"))
            .exists(),
        "the same pid from another process lifetime is stale"
    );
    assert!(
        !stale.out.iter().any(|line| line.contains("\"continued\"")),
        "a stale lifetime is no exec continuation: {:#?}",
        stale.out
    );
    assert_eq!(
        stale.journal().unwrap().commit,
        TIP,
        "the conflicting command-line commit never replaces the journal"
    );
}

/// If the continued image still fails R's own-cutter proof, it refuses instead of execing again.
/// NEGATIVE CONTROL: drop the continued guard and this calls exec-cutter in a loop.
#[test]
fn a_continued_cutter_that_is_still_foreign_never_execs_again() {
    let mut w = world_after_cutter_exec(true);
    w.own_cutter = false;
    let (code, last) = w.run(ReleaseOptions {
        commit: Some("origin/main".into()),
        ..mac()
    });
    assert_eq!(code, 1, "{last}");
    assert!(
        last.starts_with("REFUSED at 1/10 checks") && last.contains("this cutter was rebuilt at"),
        "{last}"
    );
    assert_eq!(w.count("exec-cutter"), 0, "{:?}", w.calls);
}

/// NEGATIVE CONTROLS: retaining the continued-claim veto prevents the repair; ignoring the
/// retry marker repeats exec; treating the repair hint as a proof reaches step 1 too early.
#[test]
fn an_old_contract_refreezes_once_and_only_the_proved_image_continues() {
    use crate::release_tools::Recovery;
    let options = ReleaseOptions {
        commit: Some(TIP.into()),
        ..mac()
    };
    let mut w = world_after_cutter_exec(true);
    w.own_cutter = false;
    w.cutter_recovery = Recovery::Refreeze;
    w.exec_result = Ok(());
    assert_eq!(w.run(options.clone()).0, 0);
    assert_eq!(w.count("exec-cutter"), 1);
    assert_eq!(w.exec_commit.as_deref(), Some(TIP));
    assert_eq!(w.count("validation-tools"), 0);

    // A failed replacement must not retry, even when the continued claim was lost.
    let mut invalid = World::new();
    invalid.own_cutter = false;
    invalid.cutter_recovery = Recovery::Refuse;
    assert_eq!(invalid.run(options.clone()).0, 1);
    assert_eq!(invalid.count("exec-cutter"), 0);
    assert_eq!(invalid.count("validation-tools"), 0);

    w.own_cutter = true;
    w.cutter_recovery = Recovery::Refuse;
    w.audits = VecDeque::from([passes()]);
    assert_eq!(w.run(options).0, 0, "{:#?}", w.out);
    assert_eq!(
        w.count("exec-cutter"),
        1,
        "the proved replacement did not exec again"
    );
    assert_eq!(w.count("validation-tools"), 1);
}
#[test]
fn a_judge_that_cannot_answer_stops_at_minute_one() {
    let mut w = World::new();
    w.verify_passes = false;
    w.verify_code = 3;
    assert_eq!(w.run(bump()).0, 3, "{:#?}", w.out);
    assert!(w.journal().unwrap().has(crate::release::COMMIT_GATES));
    w.judge_down = true;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 3, "{last}");
    assert!(
        last.starts_with("REFUSED at 4/10 qualify: COULD NOT RUN"),
        "{last}"
    );
    assert_eq!(w.verified(), 0, "{:?}", w.calls);
}

#[test]
fn a_publish_that_recorded_source_but_failed_signing_cannot_be_skipped_into_a_cut() {
    let mut w = World::new();
    w.publish_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 7/10 publish"), "{last}");
    assert!(w.published.contains_key(BUMP) && w.cut.is_none());

    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}");
    assert!(last.contains("source publication is incomplete"), "{last}");
    assert_eq!(w.count("publish"), 0, "{:?}", w.calls);
    assert_eq!(w.count("source-publication"), 1, "{:?}", w.calls);
    assert!(
        !w.calls.iter().any(|call| call.starts_with("cut ")),
        "{:?}",
        w.calls
    );
    assert!(w.cut.is_none(), "nothing was claimed");

    w.source_attested.insert(BUMP.into());
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    for never in [
        "build",
        "verify --tree",
        "verify --bundle",
        "stage",
        "publish",
    ] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert_eq!(w.count("source-publication"), 1, "{:?}", w.calls);
    assert!(w.cut.as_ref().is_some_and(|cut| cut.finished));
}

#[test]
fn a_fresh_cut_whose_publish_is_skipped_is_audited_and_waits_before_its_claim() {
    let mut w = World::new();
    w.cut_claims = false;
    w.cut_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 8/10 cut"), "{last}");
    assert!(w.published.contains_key(BUMP) && w.cut.is_none());

    w.cut_claims = true;
    w.cut_code = 0;
    w.audits = VecDeque::from([owner_gap(), owner_gap(), passes()]);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("publish"), 0, "{:?}", w.calls);
    assert_eq!(w.count("audit"), 3, "{:?}", w.calls);
    let sleep = w.calls.iter().position(|c| c == "sleep 600").unwrap();
    let gates = w.calls.iter().rposition(|c| c == "cut-gates").unwrap();
    let cut = w.calls.iter().position(|c| c.starts_with("cut ")).unwrap();
    assert!(sleep < gates && gates < cut, "{:?}", w.calls);
}

/// The events of `side` (`source` or `sign`) step 7 said, in order: each phase.
fn side_phases(w: &World, side: &str) -> Vec<String> {
    w.out
        .iter()
        .filter_map(|l| l.strip_prefix("release-event: "))
        .map(|json| aterm_json::from_str::<aterm_json::Value>(json).unwrap())
        .filter(|e| {
            e["event"].as_str() == Some("publish-branch") && e["branch"].as_str() == Some(side)
        })
        .map(|e| e["phase"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn step_seven_runs_the_source_and_the_sign_at_once_and_the_claim_adopts_the_copy() {
    let mut w = World::new();
    // Each side waits, inside its work, for the other's work to have begun: a step 7 that ran
    // them one after the other would hang here (the fake's one-minute hang detector).
    w.sides = SideOrder::Overlap;
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");
    assert!(
        w.out
            .iter()
            .any(|l| l.starts_with("release: 7/10 publish runs at once: source sign")),
        "{:#?}",
        w.out
    );
    assert_eq!(
        side_phases(&w, "source"),
        ["started", "push-began", "passed"]
    );
    assert_eq!(side_phases(&w, "sign"), ["started", "passed"]);
    let at = |call: &str| w.calls.iter().position(|c| c == call).unwrap();
    assert!(
        at("cut --sign") < at("cut --mac-only") && at("publish") < at("cut --mac-only"),
        "the claim only after both: {:?}",
        w.calls
    );
    assert_eq!(
        (w.adopted, w.resigned),
        (1, 0),
        "the claim adopted the copy step 7 signed"
    );
    let rows = w.times.clone();
    for row in ["publish-source", "publish-sign"] {
        assert!(rows.contains(row), "{row}: {rows}");
    }
}

#[test]
fn a_source_red_stops_the_sign_and_nothing_is_claimed() {
    let mut w = World::new();
    w.publish_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 7/10 publish"), "{last}");
    assert!(last.contains("pub publish recorded"), "{last}");
    assert_eq!(side_phases(&w, "sign"), ["started", "stopped"]);
    assert_eq!(w.count("cut --sign"), 0, "{:?}", w.calls);
    assert!(
        !w.calls
            .iter()
            .any(|c| c.starts_with("cut ") && c != "cut --sign-prepare"),
        "nothing claimed: {:?}",
        w.calls
    );
    assert!(w.cut.is_none() && w.signed.is_none());
}

#[test]
fn a_sign_red_before_the_push_stops_the_source_and_nothing_is_public() {
    let mut w = World::new();
    w.sides = SideOrder::SignFirst;
    w.sign_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 7/10 publish"), "{last}");
    assert!(
        last.contains("the source's push never began") && last.contains("nothing public"),
        "{last}"
    );
    assert_eq!(side_phases(&w, "sign"), ["started", "red"]);
    assert_eq!(side_phases(&w, "source"), ["started", "stopped"]);
    assert_eq!(
        w.count("publish: nothing pushed"),
        1,
        "the source's hook found the release's stop: {:?}",
        w.calls
    );
    assert_eq!(w.count("publish"), 0, "{:?}", w.calls);
    assert!(w.published.is_empty() && w.cut.is_none());

    w.sign_code = 0;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert_eq!((w.count("publish"), w.count("cut --sign")), (1, 1));
    assert!(w.cut.as_ref().is_some_and(|cut| cut.finished));
}

#[test]
fn a_sign_red_after_the_push_leaves_the_source_public_ahead_of_its_binary() {
    let mut w = World::new();
    w.sides = SideOrder::PushThenSign;
    w.sign_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 7/10 publish"), "{last}");
    assert!(last.contains("ahead of its binary"), "{last}");
    assert_eq!(
        side_phases(&w, "source"),
        ["started", "push-began", "passed"]
    );
    assert!(
        w.out
            .iter()
            .any(|l| l.contains("sign: RED at") && l.contains("the source's push began")),
        "{:#?}",
        w.out
    );
    assert!(w.published.contains_key(BUMP) && w.cut.is_none());

    w.sign_code = 0;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert!(
        w.out
            .iter()
            .any(|l| l.starts_with("release: 7/10 publish runs alone: sign")),
        "{:#?}",
        w.out
    );
    assert_eq!((w.count("publish"), w.count("cut --sign")), (0, 1));
    assert!(side_phases(&w, "source").is_empty());
    assert_eq!((w.adopted, w.resigned), (1, 0));
}

/// A PERSON'S SIGNAL AT STEP 7 IS NO RED: a cutter the signal cut short exits 1, and the sign
/// ends stopped, taking no gate and stopping no source (the signal reaches the engine by itself);
/// step 7 ends INTERRUPTED with no verdict, and the next run signs alone. A signal during step
/// 7's checks starts neither side. NEGATIVE CONTROLS: judging that exit as the sign's red takes the
/// gate and stops the source behind it; starting a side after the signal publishes R.
#[test]
fn a_persons_signal_at_step_seven_is_no_red_and_stops_no_side() {
    let mut w = World::new();
    w.sides = SideOrder::SignFirst;
    w.signal_at = Some("cut --sign");
    let (code, last) = w.run(bump());
    assert_eq!(code, 130, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with("INTERRUPTED at 7/10 publish (by SIGTERM)"),
        "{last}"
    );
    assert!(
        !w.out.iter().any(|l| l.contains(": RED at ")),
        "{:#?}",
        w.out
    );
    assert_eq!(side_phases(&w, "sign"), ["started", "stopped"]);
    assert!(
        !side_phases(&w, "source").contains(&"stopped".to_string()),
        "{:#?}",
        w.out
    );
    assert_eq!(w.count("publish: nothing pushed"), 0, "{:?}", w.calls);
    assert!(w.cut.is_none());

    w.signal_at = None;
    w.signal = None;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        (w.count("publish"), w.count("cut --sign")),
        (0, 1),
        "{:?}",
        w.calls
    );
    assert!(w.cut.as_ref().is_some_and(|cut| cut.finished));

    // A signal during step 7's checks starts neither side.
    let mut w = World::new();
    w.signal_at = Some("publish-begin");
    let (code, last) = w.run(bump());
    assert_eq!(code, 130, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with("INTERRUPTED at 7/10 publish (by SIGTERM)"),
        "{last}"
    );
    assert_eq!(side_phases(&w, "source"), ["stopped"], "{:#?}", w.out);
    assert_eq!(side_phases(&w, "sign"), ["stopped"], "{:#?}", w.out);
    assert_eq!(
        (w.count("publish"), w.count("cut --sign")),
        (0, 0),
        "{:?}",
        w.calls
    );
}

/// A SIGN RED AFTER THE SOURCE'S PUSH BEGAN, AND A SOURCE THAT THEN DIES BEFORE ITS LEDGER ROW,
/// SAYS THE SOURCE MAY BE PUBLIC: the engine records no row of R, and this run's gate says the
/// push began; the source's own failure is named under the sign's. NEGATIVE CONTROL: judging what
/// is public by the engine's rows alone says the push never began.
#[test]
fn a_push_that_began_is_named_though_the_source_recorded_no_row() {
    let mut w = World::new();
    w.sides = SideOrder::PushThenSign;
    w.sign_code = 1;
    w.source_dies_after_push = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 7/10 publish"), "{last}");
    assert!(
        last.contains("may be public ahead of its binary")
            && !last.contains("never began")
            && !last.contains("nothing public"),
        "{last}"
    );
    assert!(
        w.out
            .iter()
            .any(|l| l.contains("| the source failed after it: exit 1")),
        "{:#?}",
        w.out
    );
    assert!(w.cut.is_none() && !w.published.contains_key(BUMP));
}

#[test]
fn a_sign_that_could_not_run_is_resumed_by_the_sign_alone() {
    let mut w = World::new();
    w.sign_code = 130;
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "a resumable stop: {last}");
    assert!(
        last.starts_with("REFUSED at 7/10 publish: COULD NOT RUN — cut --sign"),
        "{last}"
    );
    assert_eq!(side_phases(&w, "sign"), ["started", "could-not-run"]);
    assert_eq!(
        side_phases(&w, "source"),
        ["started", "push-began", "passed"]
    );
    assert!(w.published.contains_key(BUMP) && w.cut.is_none());

    w.sign_code = 0;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert_eq!((w.count("publish"), w.count("cut --sign")), (0, 1));
    assert!(w.cut.as_ref().is_some_and(|cut| cut.finished));
}

/// The fake child starts before bundle and joins in step 7. NEGATIVE CONTROL: preparing
/// after bundle fails its assertion; promoting before READY leaves signed state
/// after the red. Neither defect can be hidden by a fast fake signer.
#[test]
fn early_signature_overlaps_bundle_and_a_bundle_red_discards_it() {
    for red in [false, true] {
        let mut w = World::new();
        w.early_overlap = true;
        if red {
            w.verify_half = Some(Half::Bundle);
            w.verify_code = 1;
            w.verify_passes = false;
        }
        let (code, last) = w.run(bump());
        assert_eq!(code, i32::from(red), "{last}");
        assert_eq!(w.count("cut --sign-prepare"), 1);
        if red {
            assert!(last.starts_with("REFUSED at 5/10 bundle"), "{last}");
            assert!(w.prepared_signature.is_none() && w.signed.is_none());
            assert!(w.published.is_empty() && w.cut.is_none());
            assert_eq!(w.count("cut --sign"), 0);
            assert_eq!(w.adopted, 0);
        } else {
            assert_eq!((w.adopted, w.resigned), (1, 0));
        }
    }
}

#[test]
fn early_sign_failure_and_interruption_never_publish_or_adopt() {
    for interrupted in [false, true] {
        let mut w = World::new();
        w.sides = SideOrder::SignFirst;
        if interrupted {
            w.signal_at = Some("cut --sign-prepare");
        } else {
            w.prepare_code = 1;
        }
        let (code, last) = w.run(bump());
        assert_eq!(code, if interrupted { 130 } else { 1 }, "{last}");
        assert!(w.prepared_signature.is_none() && w.signed.is_none());
        assert!(w.published.is_empty() && w.cut.is_none());
        assert_eq!(w.adopted, 0);
    }
}

#[test]
fn a_claim_that_loses_the_race_discards_the_signed_copy() {
    let mut w = World::new();
    w.overtaken_at_cut = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 8/10 cut"), "{last}");
    assert!(
        last.contains("lost the ledger's race") && last.contains("was discarded"),
        "{last}"
    );
    assert!(w.signed.is_none(), "the privately signed copy is gone");
    assert!(w.cut.is_none() && w.published.contains_key(BUMP));
    assert_eq!((w.adopted, w.resigned), (0, 0), "nothing was claimed");

    // The cut refuses before its claim while another release takes the number: the release
    // discards the copy itself, so what it says is what happened.
    let mut w = World::new();
    w.overtaken_at_cut = true;
    w.cut_refuses_before_its_claim = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.contains("was discarded"), "{last}");
    assert!(
        w.signed.is_none(),
        "the privately signed copy is gone: {:?}",
        w.calls
    );
    assert_eq!(
        w.calls
            .iter()
            .filter(|c| c.starts_with("discard-signed "))
            .count(),
        1,
        "{:?}",
        w.calls
    );
}

#[test]
fn a_resumed_cut_never_signs_beside_a_publish_again() {
    let mut w = World::new();
    w.cut_finishes = false;
    w.cut_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 8/10 cut"), "{last}");
    assert_eq!(w.count("cut --sign"), 1, "{:?}", w.calls);
    assert!(w.cut.as_ref().is_some_and(|cut| !cut.finished));

    w.cut_finishes = true;
    w.cut_code = 0;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert_eq!(
        (w.count("publish"), w.count("cut --sign")),
        (0, 0),
        "a claimed cut signs its own: {:?}",
        w.calls
    );
    assert!(w.calls.iter().any(|c| c == "cut --resume"), "{:?}", w.calls);
}

#[test]
fn what_changes_while_the_publish_waits_is_asked_again_right_before_it() {
    type Change = fn(&mut World);
    let changes: [(&str, Change); 2] = [
        ("v0.6.0 cut by a peer", |w| {
            w.origin_tags.insert("v0.6.0".into(), NEXT.into());
        }),
        ("the disk filled", |w| w.free = 4),
    ];
    for (case, change) in changes {
        let mut w = World::new();
        w.audits = VecDeque::from([passes(), owner_gap(), passes()]);
        w.after_sleep = Some(change);
        let (code, last) = w.run(bump());
        assert_eq!(code, 1, "{case}: {last}");
        assert!(
            last.starts_with("REFUSED at 7/10 publish"),
            "{case}: {last}"
        );
        assert_eq!(
            (w.count("sleep 600"), w.count("publish")),
            (1, 0),
            "{case}: {:?}",
            w.calls
        );
    }
}

#[test]
fn an_interrupt_ends_the_run_in_one_line_and_it_resumes() {
    type Setup = fn(&mut World);
    let cases: [(Setup, &str, &str); 5] = [
        (
            |w| w.signal_at = Some("verify --tree"),
            "INTERRUPTED at 4/10 qualify (by SIGTERM)",
            "verify --bundle",
        ),
        (
            |w| w.signal_at = Some("verify --bundle"),
            "INTERRUPTED at 5/10 bundle (by SIGTERM)",
            "publish",
        ),
        (
            |w| w.signal_at = Some("cut"),
            "INTERRUPTED at 8/10 cut (by SIGTERM)",
            "after-cut 0.6.0",
        ),
        (
            |w| {
                w.audits = VecDeque::from([passes(), owner_gap(), passes()]);
                w.after_sleep = Some(|w| w.signal = Some(15));
            },
            "INTERRUPTED at 7/10 publish (by SIGTERM)",
            "publish",
        ),
        (
            |w| w.signal_at = Some("move"),
            "INTERRUPTED at 1/10 checks (by SIGTERM)",
            "exec-cutter",
        ),
    ];
    for (setup, line, never) in cases {
        let mut w = World::new();
        setup(&mut w);
        let (code, last) = w.run(bump());
        assert_eq!(code, 130, "{line}: {last}");
        assert!(last.starts_with(line), "{last}");
        assert_eq!(
            w.out.last().unwrap(),
            &last,
            "the final line is the last line"
        );
        let closed = w.out[w.out.len() - 2]
            .strip_prefix("release-event: ")
            .map(|event| aterm_json::from_str::<aterm_json::Value>(event).expect("an event"))
            .unwrap_or_default();
        assert_eq!(
            (closed["stage"].as_str(), closed["outcome"].as_str()),
            (line.split_whitespace().nth(3), Some("INTERRUPTED")),
            "the step's record closes before the final line: {:?}",
            w.out
        );
        assert!(!w.release_dir().join(RUNNING_FILE).exists(), "{line}");
        assert_eq!(w.count(never), 0, "{line}: {:?}", w.calls);

        w.signal = None;
        w.signal_at = None;
        w.own_cutter = true;
        w.calls.clear();
        let (code, last) = w.run(bump());
        assert_eq!(code, 0, "{line}: {last}\n{:#?}", w.out);
        assert_eq!(w.count("bump"), 0, "{line}: {:?}", w.calls);
    }
}

const INTERRUPT_PROBE: &str = "ATERM_RELEASE_INTERRUPT_PROBE";

const PROBE_STEP: &str = r#"d="$1"
( trap 'echo TERM > "$d/grandchild"; exit 0' TERM
  echo armed > "$d/grandchild.armed"
  i=0; while [ "$i" -lt 120 ]; do sleep 1; i=$((i + 1)); done ) &
trap 'echo TERM > "$d/child"; wait; exit 143' TERM
echo armed > "$d/child.armed"
wait
"#;

/// A REAL INTERRUPT, END TO END. The release (a process of its own: this test binary
/// re-run as the probe) runs its verify step's child through the real spawn path; a
/// SIGTERM sent to the RELEASE alone reaches every member of the step's group (the
/// child and the subshell it left running), the release waits for the step to end, and
/// its last line is `INTERRUPTED …`, exit 130.
#[cfg(unix)]
#[test]
fn a_real_sigterm_reaches_the_steps_group_and_ends_interrupted() {
    use aterm_uds::spawnfd::{Stdio, command};
    use std::time::{Duration, Instant};
    if let Some(dir) = std::env::var_os(INTERRUPT_PROBE) {
        let dir = PathBuf::from(dir);
        unsafe extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
        }
        // SAFETY: SIG_DFL (0) for SIGTERM, which the parent sends this probe, whatever this
        // test binary was started with: `install` leaves an ignored one ignored, and the step's
        // child would inherit it ignored too.
        unsafe {
            signal(crate::interrupt::SIGTERM, 0);
        }
        crate::interrupt::install();
        let mut w = World::new();
        let real = Real::new(w.dir.path(), None, true, false).expect("a backend");
        w.real = Some((real, PROBE_STEP.to_string(), dir));
        let (code, _) = w.run(bump());
        for line in &w.out {
            println!("{line}");
        }
        std::process::exit(code);
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut probe = command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "release_flow::a_real_sigterm_reaches_the_steps_group_and_ends_interrupted",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(INTERRUPT_PROBE, scratch.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let bound = Instant::now() + Duration::from_secs(60);
    let armed = |who: &str| scratch.path().join(format!("{who}.armed")).exists();
    while !(armed("child") && armed("grandchild")) {
        if Instant::now() > bound || probe.try_wait().unwrap().is_some() {
            let _ = probe.kill();
            let out = probe.wait_with_output().unwrap();
            panic!(
                "the step's child never armed: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let sent = command("kill")
        .args(["-TERM", &probe.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    let bound = Instant::now() + Duration::from_secs(60);
    while probe.try_wait().unwrap().is_none() {
        if Instant::now() > bound {
            let _ = probe.kill();
            panic!("the release did not end after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = probe.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(130), "{said}");
    let read = |who: &str| std::fs::read_to_string(scratch.path().join(who)).unwrap_or_default();
    assert_eq!(read("child").trim(), "TERM", "the step's child got it");
    assert_eq!(read("grandchild").trim(), "TERM", "and so did its group");
    let last = said
        .lines()
        .rfind(|l| l.starts_with("INTERRUPTED"))
        .unwrap_or_else(|| panic!("no INTERRUPTED line: {said}"));
    assert!(
        last.starts_with("INTERRUPTED at 4/10 qualify (by SIGTERM)"),
        "{last}"
    );
}

#[test]
fn every_child_a_killed_step_left_running_holds_the_clone() {
    let mut w = World::new();
    let dir = w.release_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let note = |path: PathBuf, pid: u32| {
        std::fs::write(
            path,
            format!(
                "pid {pid}\nstarted Thu Oct  9 01:30:0{} 2026\nstep stage\n",
                pid % 10
            ),
        )
        .unwrap();
    };
    note(dir.join(RUNNING_CHILD_FILE), 801);
    note(crate::release::child_note(&dir, 802), 802);
    std::fs::write(dir.join(format!("{RUNNING_CHILD_FILE}.x")), "pid 803\n").unwrap();
    w.alive.insert(802, "Thu Oct  9 01:30:02 2026".into());
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with("REFUSED at 1/10 checks") && last.contains("pid 802"),
        "{last}"
    );
    assert!(!last.contains("-- -802"), "never its group: {last}");
    assert_eq!(crate::release::child_notes(&dir).len(), 2);
    w.alive.clear();
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
    assert!(
        crate::release::child_notes(&dir).is_empty(),
        "the dead children's notes are cleared"
    );
}

#[test]
fn a_check_that_could_not_run_keeps_the_ask_open() {
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap()]);
    // A check that could not run (exit 3) is resumed; a FAIL never would be (A19).
    w.verify_passes = false;
    w.verify_code = 3;
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert_eq!(w.journal().unwrap().asks, ["release.notary"], "journaled");

    let unanswered = vec![Finding {
        label: "notary",
        state: Audited::CouldNotRun("Apple's notary service did not answer".into()),
    }];
    w.audits = VecDeque::from([unanswered, owner_gap(), passes()]);
    w.verify_passes = true;
    w.verify_code = 0;
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.asks, ["release.notary"], "one ask");
    assert_eq!(
        w.resolved,
        ["release.notary"],
        "resolved once, when it passed"
    );
}

#[test]
fn asks_are_carried_over_each_new_head_and_withdrawn_by_forget() {
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap(), owner_gap(), machine_gap()]);
    let _ = w.run(bump());
    assert_eq!(w.journal().unwrap().asks, ["release.notary"]);
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    let (code, _) = w.run(ReleaseOptions {
        commit: Some(NEXT.into()),
        ..mac()
    });
    assert_eq!(code, 1, "--commit only starts a line");
    assert_eq!(w.journal().unwrap().commit, BUMP);
    w.history.push(FIX.into());
    w.versions.insert(FIX.into(), "0.6.0".into());
    w.audits = VecDeque::from([owner_gap(), machine_gap()]);
    let (code, _) = w.run(ReleaseOptions {
        fix: vec![FIX.into()],
        ..mac()
    });
    assert_eq!(code, 1);
    let j = w.journal().unwrap();
    assert_eq!(j.commit, hop_of(FIX));
    assert_eq!(j.asks, ["release.notary"], "carried, not asked again");
    assert_eq!(w.asks, ["release.notary"]);
    let forget = ReleaseOptions {
        forget: true,
        ..ReleaseOptions::default()
    };
    std::fs::write(
        w.release_dir().join(RUNNING_FILE),
        "pid 9\nstarted then\nlogs /l\n",
    )
    .unwrap();
    w.alive.insert(9, "then".into());
    let (code, last) = w.run(forget.clone());
    assert_eq!(code, 1, "{last}");
    assert!(w.journal().is_some() && w.withdrawn.is_empty(), "{last}");
    w.alive.clear();
    let (code, last) = w.run(forget);
    assert_eq!(code, 0, "{last}");
    assert_eq!(w.withdrawn, ["release.notary"]);
    assert!(w.journal().is_none());
}

#[test]
fn an_ask_before_judgment_is_retained_by_selection_and_forgotten_once() {
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap()]);
    w.cut_gate_refusals = vec![lease_held()];
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 1/10 checks"), "{last}");
    assert!(w.journal().is_none(), "nothing journaled");
    assert_eq!(w.asks, ["release.notary"]);
    assert!(w.withdrawn.is_empty());
    let selection = Journal::load(&w.release_dir().join("candidate.toml"))
        .unwrap()
        .unwrap();
    assert_eq!(selection.asks, ["release.notary"]);
    assert_eq!(
        w.run(ReleaseOptions {
            forget: true,
            ..mac()
        })
        .0,
        0
    );
    assert_eq!(w.withdrawn, ["release.notary"]);
}

#[test]
fn a_claim_forgotten_and_resumed_is_never_built_again() {
    let mut w = World::new();
    w.cut_finishes = false;
    w.cut_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    let claimed = w.cut.clone().expect("claimed");
    assert!(!claimed.finished);
    let forget = ReleaseOptions {
        forget: true,
        ..ReleaseOptions::default()
    };
    let (code, last) = w.run(forget);
    assert_eq!(code, 0, "{last}");
    assert!(w.journal().is_none());

    w.cut_finishes = true;
    w.cut_code = 0;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with(&format!(
            "RELEASED aterm 0.6.0 (build {}) from {}",
            claimed.build,
            &BUMP[..9]
        )),
        "{last}"
    );
    assert!(w.calls.iter().any(|c| c == "cut --resume"), "{:?}", w.calls);
    assert_eq!(
        (w.count("reserve"), w.count("build"), w.verified()),
        (0, 0, 0),
        "{:?}",
        w.calls
    );
}

#[test]
fn the_release_waits_for_a_deployed_and_verified_website_without_rebuilding() {
    use crate::site::Outcome;
    for outcome in [
        Outcome::NoCheckout("no checkout".into()),
        Outcome::NoDeploy,
        Outcome::NotDeployed,
        Outcome::Skipped,
        Outcome::Deferred,
        Outcome::Planned {
            files: vec![],
            engine: false,
        },
        Outcome::LiveUnchecked { deployed: true },
        Outcome::LiveUnchecked { deployed: false },
    ] {
        let mut w = World::new();
        w.site = Ok(outcome);
        let (code, last) = w.run(bump());
        assert_ne!(code, 0, "{last}");
        assert!(last.starts_with("REFUSED at 9/10 after-cut"), "{last}");
        assert!(!w.journal().unwrap().finished());
        assert!(!w.journal().unwrap().has("after-cut"));
        w.site = Ok(Outcome::Synced { deployed: true });
        w.calls.clear();
        assert_eq!(w.run(mac()).0, 0);
        assert_eq!(w.count("after-cut 0.6.0"), 1);
        for work in [
            "reserve",
            "build",
            "verify --tree",
            "verify --bundle",
            "publish",
            "cut",
        ] {
            assert_eq!(w.count(work), 0, "{work}: {:?}", w.calls);
        }
        assert!(w.journal().unwrap().finished());
    }
    let failed = crate::release_steps::site_outcome(
        &Err(crate::site::Failure::Child {
            child: "git push",
            said: "push to origin main failed (above)".into(),
        }),
        vec!["  remote: refused".into()],
    )
    .unwrap_err();
    assert!(
        failed
            .why
            .contains("the site sync failed: push to origin main failed"),
        "{failed:?}"
    );
    let mut w = World::new();
    w.site = Ok(crate::site::Outcome::LiveDisagrees {
        deployed: true,
        why: Vec::new(),
    });
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(last.starts_with("REFUSED at 9/10 after-cut"), "{last}");
}

#[test]
fn an_old_site_fact_cannot_skip_tap_alignment_on_resume() {
    let mut w = World::new();
    assert_eq!(w.run(bump()).0, 0);
    let mut journal = w.journal().unwrap();
    assert!(journal.has("tap-aligned"));
    journal.done.retain(|fact| fact != "tap-aligned");
    assert!(journal.has("after-cut") && journal.has("check"));
    assert!(
        !journal.finished(),
        "a historical website/check fact cannot prove tap delivery"
    );
    journal.save(&w.release_dir().join(JOURNAL_FILE)).unwrap();
    w.after_cut = Err(Refusal::new(
        "tap cohort changed; nothing changed",
        "retry after-cut",
    ));
    w.calls.clear();
    assert_eq!(w.run(mac()).0, 1);
    assert_eq!(w.count("after-cut 0.6.0"), 1);
    assert!(!w.journal().unwrap().has("tap-aligned"));
    for work in [
        "reserve",
        "build",
        "verify --tree",
        "verify --bundle",
        "publish",
        "cut",
        "land-fixtures",
    ] {
        assert_eq!(w.count(work), 0, "{work}: {:?}", w.calls);
    }
    w.after_cut = Ok("site and guarded tap align".into());
    w.calls.clear();
    assert_eq!(w.run(mac()).0, 0);
    assert_eq!(w.count("after-cut 0.6.0"), 1);
    assert!(w.journal().unwrap().has("tap-aligned"));
    assert!(w.journal().unwrap().finished());
    w.calls.clear();
    assert_eq!(w.run(mac()).0, 0);
    assert_eq!(
        w.count("after-cut 0.6.0"),
        0,
        "completed alignment is resumable"
    );
}

#[test]
fn an_old_deferred_website_is_repaired_before_the_release_is_complete() {
    let mut w = World::new();
    assert_eq!(w.run(bump()).0, 0);
    let mut journal = w.journal().unwrap();
    journal.done.push("site-deferred".into());
    assert!(!journal.finished());
    journal.save(&w.release_dir().join(JOURNAL_FILE)).unwrap();
    w.calls.clear();
    assert_eq!(w.run(mac()).0, 0);
    assert_eq!(w.count("after-cut 0.6.0"), 1);
    for work in [
        "reserve",
        "build",
        "verify --tree",
        "verify --bundle",
        "publish",
        "cut",
    ] {
        assert_eq!(w.count(work), 0, "{work}: {:?}", w.calls);
    }
    let journal = w.journal().unwrap();
    assert!(journal.finished());
    assert!(!journal.has("site-deferred"));
}

#[test]
fn a_shape_refusal_names_its_way_out_and_following_it_ships_the_successor() {
    let mut w = World::new();
    w.after_cut = Err(crate::release_steps::shape_refusal(
        "alabsystems/aterm",
        "v0.6.0",
        vec!["aterm-0.6.0.dmg is missing".into()],
    ));
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(last.starts_with("REFUSED at 9/10 after-cut"), "{last}");
    w.origin_tags.insert("v0.6.0".into(), BUMP.into());
    w.ledger.push("0.6.0".into());
    w.calls.clear();
    let (code, again) = w.run(bump());
    assert_eq!((code, again.as_str()), (1, last.as_str()));
    assert_eq!(w.count("bump"), 0, "{:?}", w.calls);

    let (code, last) = w.run(ReleaseOptions {
        forget: true,
        ..ReleaseOptions::default()
    });
    assert_eq!(code, 0, "{last}");
    w.after_cut = Ok("the release's shape checked".into());
    w.calls.clear();
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with(&format!(
            "RELEASED aterm 0.7.0 (build 1791600001) from {}",
            &BUMP2[..9]
        )),
        "{last}"
    );
    assert_eq!(w.count("bump"), 1, "{:?}", w.calls);
}

#[test]
fn forget_takes_no_flag_it_would_ignore() {
    let parse = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        crate::cli::parse(&args)
    };
    for bad in [
        &["release", "--dry-run", "--forget"][..],
        &["release", "--forget", "--bump"],
        &["release", "--forget", "--commit", "origin/main"],
        &["release", "--forget", "--mac-only"],
        &["release", "--forget", "--prepare"],
        &["release", "--forget", "--qualify"],
        &["release", "--forget", "--fix", "abc"],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
    assert!(matches!(
        parse(&["release", "--forget", "--verbose", "--engine", "/e"]),
        Ok(crate::cli::Cmd::Release(o)) if o.forget && o.verbose
    ));
}

#[test]
fn explicit_credentials_reach_preflight_and_publish_audits() {
    for dry in [false, true] {
        let mut w = World::new();
        let profile = PathBuf::from("/existing-release-profile.toml");
        let (code, last) = w.run(ReleaseOptions {
            release_credentials: Some(profile.clone()),
            dry_run: dry,
            ..bump()
        });
        assert_eq!(code, 0, "{last}\n{:#?}", w.out);
        assert!(!w.audited_profiles.is_empty());
        assert!(
            w.audited_profiles
                .iter()
                .all(|p| p.as_ref() == Some(&profile)),
            "{dry}: {:?}",
            w.audited_profiles
        );
    }
}

#[test]
fn private_qualification_preserves_a_live_publishers_lease() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.publisher_lease_held = true;
    let options = ReleaseOptions {
        commit: Some(TIP.into()),
        ..mac()
    };
    let (code, last) = w.run(ReleaseOptions {
        qualify: true,
        ..options.clone()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(w.publisher_lease_held);
    assert_eq!(w.count("stage --dry-run"), 1);
    for never in ["stage", "publish", "cut"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    w.calls.clear();
    let (code, last) = w.run(options);
    assert_ne!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        w.out
            .iter()
            .any(|line| line.contains("release lock is held")),
        "{last}\n{:#?}",
        w.out
    );
    assert_eq!(w.count("stage"), 0);
    assert!(w.publisher_lease_held);

    let mut invalid = World::new();
    invalid.versions.insert(TIP.into(), "0.6.0".into());
    invalid.publisher_lease_held = true;
    invalid.cut_gate_refusals.push(Refusal::new(
        "signing roster floor mismatch",
        "restore the current roster",
    ));
    let (code, last) = invalid.run(ReleaseOptions {
        qualify: true,
        commit: Some(TIP.into()),
        ..mac()
    });
    assert_ne!(code, 0, "{last}");
    assert!(
        invalid
            .out
            .iter()
            .any(|line| line.contains("signing roster floor mismatch")),
        "{last}\n{:#?}",
        invalid.out
    );
    assert_eq!(invalid.count("build"), 0);
    assert!(invalid.publisher_lease_held);
}

#[test]
fn private_qualification_keeps_the_verified_bundle_and_a_later_release_still_stages() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    let options = ReleaseOptions {
        qualify: true,
        commit: Some(TIP.into()),
        ..mac()
    };
    let (code, last) = w.run(options.clone());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("QUALIFIED aterm 0.6.0"), "{last}");
    for work in [
        "stage --dry-run",
        "reserve",
        "build",
        "verify --tree",
        "verify --bundle",
    ] {
        assert_eq!(w.count(work), 1, "{work}: {:?}", w.calls);
    }
    for never in ["stage", "bump", "publish", "cut"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    let journal = w.journal().unwrap();
    assert!(journal.has(crate::release::COMMIT_GATES));
    assert!(journal.artifact.is_some());
    assert!(journal.reserved.is_some());
    assert!(journal.prepared.is_none());
    assert!(w.attested.is_none());
    assert!(w.published.is_empty());
    assert!(w.cut.is_none());
    assert!(!w.release_dir().join(RUNNING_FILE).exists());

    w.calls.clear();
    let (code, last) = w.run(options);
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        w.count("stage --dry-run"),
        1,
        "recheck the full local export"
    );
    assert_eq!(w.count("build"), 0, "keep the exact verified bytes");
    assert_eq!(w.verified(), 0, "reuse only a matching verification");

    w.calls.clear();
    let (code, last) = w.run(ReleaseOptions {
        commit: Some(TIP.into()),
        ..mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        w.count("stage"),
        1,
        "local qualification cannot attest remote staging"
    );
    let stage = w.calls.iter().position(|c| c == "stage").unwrap();
    let publish = w.calls.iter().position(|c| c == "publish").unwrap();
    assert!(stage < publish);
    assert_eq!(w.count("build"), 0);
    assert_eq!(w.verified(), 0);
}

/// A failed local export is a red: it stops the tree and the build running beside it (each here
/// runs until its stop fires), so no bundle is kept, the bundle half never runs and nothing is
/// staged or published. A dry run runs nothing. Negative control: a graph that fires no stop on
/// a red leaves the held branches running, and the hang detector fails here.
#[test]
fn private_qualification_refuses_failed_exports_and_a_dry_run_builds_nothing() {
    for dry in [false, true] {
        let mut w = World::new();
        w.versions.insert(TIP.into(), "0.6.0".into());
        w.stage_stages = false;
        w.holds = BTreeSet::from([Branch::Tree, Branch::Build]);
        let (code, last) = w.run(ReleaseOptions {
            qualify: true,
            dry_run: dry,
            commit: Some(TIP.into()),
            ..mac()
        });
        let never: &[&str] = if dry {
            assert_eq!(code, 0, "{last}\n{:#?}", w.out);
            assert!(last.starts_with("DRY RUN"), "{last}");
            assert!(w.journal().is_none());
            &["stage --dry-run", "reserve", "build", "verify --tree"]
        } else {
            assert_eq!(code, 1, "{last}\n{:#?}", w.out);
            assert!(
                last.starts_with("REFUSED at 4/10 qualify: private export qualification failed"),
                "{last}"
            );
            for stopped in ["tree", "build"] {
                let line = format!("the {stopped} branch was stopped by the export branch's red");
                assert!(
                    w.out.iter().any(|l| l.contains(&line)),
                    "{line}: {:#?}",
                    w.out
                );
            }
            assert!(w.journal().unwrap().artifact.is_none());
            assert_eq!(
                ["stage --dry-run", "reserve", "build", "verify --tree"].map(|c| w.count(c)),
                [1; 4],
                "{:?}",
                w.calls
            );
            &[]
        };
        for never in never
            .iter()
            .chain(&["verify --bundle", "stage", "publish", "cut"])
        {
            assert_eq!(w.count(never), 0, "{dry}: {never}: {:?}", w.calls);
        }
    }
}

/// The `qualify-branch` event phases `branch` said, in order.
fn phases(w: &World, branch: &str) -> Vec<String> {
    w.out
        .iter()
        .filter_map(|l| l.strip_prefix("release-event: "))
        .map(|json| aterm_json::from_str::<aterm_json::Value>(json).unwrap())
        .filter(|e| {
            e["event"].as_str() == Some("qualify-branch") && e["branch"].as_str() == Some(branch)
        })
        .filter_map(|e| e["phase"].as_str().map(str::to_owned))
        .collect()
}

/// A PERSON'S SIGNAL IS NO RED AND ADDS NO STOP: a signal that cuts the build short (its `ship
/// build` exits nonzero, as a red build's does) ends the build `stopped` with no RED line and
/// fires no stop of its own (the signal reaches each branch's group by itself; a stop on top of
/// it counts as a second signal, which escalates to SIGKILL), so the export that runs beside it
/// ends by itself; a branch due to start after a signal never starts; either way step 4 ends
/// INTERRUPTED, and the run resumes. NEGATIVE CONTROLS: judging the build's end without the
/// signal makes it a red that stops the export, firing the stops on the build's signalled end
/// stops the export (its work ends after that end is told), and starting a branch without asking
/// starts the tree after the build's signal.
#[test]
fn a_persons_signal_is_no_red_and_adds_no_stop() {
    let interrupted = |w: &mut World| {
        let (code, last) = w.run(bump());
        assert_eq!(code, 130, "{last}\n{:#?}", w.out);
        assert!(
            last.starts_with("INTERRUPTED at 4/10 qualify (by SIGTERM)"),
            "{last}"
        );
        assert!(
            !w.out.iter().any(|l| l.contains(": RED at ")),
            "{:#?}",
            w.out
        );
    };
    let mut w = World::new();
    w.signal_at = Some("build");
    interrupted(&mut w);
    assert_eq!(phases(&w, "build"), ["started", "stopped"], "{:#?}", w.out);
    assert_eq!(phases(&w, "export"), ["started", "passed"], "{:#?}", w.out);

    w.signal_at = None;
    w.signal = None;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("build"), 1, "{:?}", w.calls);

    // A signal before the slices' claims leaves later branches unstarted.
    let mut w = World::new();
    w.build_claims_late = true;
    w.signal_at = Some("build");
    interrupted(&mut w);
    assert_eq!(phases(&w, "tree"), ["stopped"], "{:#?}", w.out);
    assert_eq!(phases(&w, "build"), ["started", "stopped"], "{:#?}", w.out);
    assert_eq!(phases(&w, "export"), ["stopped"], "{:#?}", w.out);
    assert_eq!(w.count("build"), 1, "{:?}", w.calls);
}

/// A RED STOPS EVERY OTHER BRANCH, AND ONE NOT STARTED NEVER STARTS (Q3): the build fails before
/// its slices claimed memory; its red stops the tree and the export
/// before either starts, and step 4 refuses R with the FAIL. Negative control: a branch that
/// starts though its stop fired runs the tree here.
#[test]
fn a_red_stops_the_other_branches_and_one_not_started_never_starts() {
    let mut w = World::new();
    w.build_claims_late = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(!w.dir.path().join("prefreeze-started").exists());
    assert!(
        last.starts_with("REFUSED at 4/10 qualify") && last.contains("early build failure"),
        "{last}"
    );
    for never in [
        "verify --tree",
        "stage-fixtures",
        "stage",
        "verify --bundle",
        "publish",
    ] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert!(
        w.out
            .iter()
            .any(|l| l.contains("build: RED at") && l.contains("stopping tree, export")),
        "{:#?}",
        w.out
    );
    assert_eq!(phases(&w, "build"), ["started", "red"]);
    for branch in ["tree", "export"] {
        assert_eq!(phases(&w, branch), ["stopped"], "{branch}");
    }
}

/// AN EXPORT WHOSE STAGING PUSH BEGAN FINISHES IT (Q4): the tree's red comes while the push runs;
/// the export is not stopped, staging finishes and is attested, and R is still refused for the
/// red. Negative control: a stop that fires on a branch whose push began ends the export stopped.
#[test]
fn a_red_leaves_an_export_whose_push_began_to_finish() {
    let mut w = World::new();
    w.verify_outcomes.insert("test", "FAIL");
    w.verify_code = 1;
    w.export_pushes = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(
        w.out
            .iter()
            .any(|row| row.contains("fixture main fix hint")),
        "{:#?}",
        w.out
    );
    assert!(
        last.starts_with("REFUSED at 4/10 qualify") && last.contains("FAILED test"),
        "{last}"
    );
    assert!(
        w.out
            .iter()
            .any(|l| l.contains("tree: RED at") && l.contains("staging push began, so it finishes")),
        "{:#?}",
        w.out
    );
    assert_eq!(phases(&w, "export"), ["started", "push-began", "passed"]);
    assert_eq!(
        w.attested.as_deref(),
        Some(BUMP),
        "the staging push finished"
    );
    assert_eq!(w.count("publish"), 0);
}

/// A COULD NOT RUN STOPS NOTHING (Q5): the tree could not run, and the build and the export,
/// which work only once its end is told, finish: the bundle and its fixtures are kept and R is
/// staged, so the next run resumes the tree alone. Negative control: a graph that stops the
/// other branches on a COULD NOT RUN ends both stopped here, nothing built or staged.
#[test]
fn a_could_not_run_stops_nothing() {
    let mut w = World::new();
    w.verify_half = Some(Half::Tree);
    w.verify_passes = false;
    w.verify_code = 3;
    w.after_tree = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with("REFUSED at 4/10 qualify: COULD NOT RUN"),
        "{last}"
    );
    assert!(
        w.out.iter().any(
            |l| l.contains("tree: could not run at") && l.contains("the other branches finish")
        ),
        "{:#?}",
        w.out
    );
    for branch in ["build", "export"] {
        assert_eq!(phases(&w, branch), ["started", "passed"], "{branch}");
    }
    assert!(
        w.journal().unwrap().artifact.is_some(),
        "the bundle is kept"
    );
    assert_eq!(w.attested.as_deref(), Some(BUMP));

    w.verify_half = None;
    w.verify_passes = true;
    w.verify_code = 0;
    w.after_tree = false;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        ["verify --tree", "build", "stage", "verify --bundle"].map(|c| w.count(c)),
        [1, 0, 0, 1],
        "{:?}",
        w.calls
    );
}

#[test]
fn private_qualification_cannot_select_a_publishing_or_withdrawal_mode() {
    for incompatible in ["--bump", "--prepare", "--forget", "--abandon"] {
        let mut args = vec![
            "release".to_string(),
            "--qualify".into(),
            incompatible.into(),
        ];
        if incompatible == "--abandon" {
            args.push("v0.6.0".into());
        }
        assert!(crate::cli::parse(&args).is_err(), "{args:?}");
    }
    let args = [
        "release",
        "--qualify",
        "--dry-run",
        "--commit",
        TIP,
        "--mac-only",
    ]
    .map(str::to_string);
    assert!(
        matches!(crate::cli::parse(&args), Ok(crate::cli::Cmd::Release(options)) if options.qualify && options.dry_run)
    );
}

#[test]
fn private_qualification_reports_unavailable_prerequisites_without_asking() {
    for dry in [false, true] {
        for state in [
            Audited::Gap {
                what: "notary unavailable".into(),
                fix: "restore it".into(),
                owner: true,
            },
            Audited::CouldNotRun("notary unavailable".into()),
        ] {
            let mut w = World::new();
            w.versions.insert(TIP.into(), "0.6.0".into());
            w.audits.push_back(vec![Finding {
                label: "notary",
                state,
            }]);
            let (code, last) = w.run(ReleaseOptions {
                qualify: true,
                dry_run: dry,
                commit: Some(TIP.into()),
                ..mac()
            });
            assert_ne!(code, 0, "{last}\n{:#?}", w.out);
            assert!(w.asks.is_empty());
            for never in [
                "stage",
                "stage --dry-run",
                "build",
                "verify --tree",
                "verify --bundle",
                "publish",
                "cut",
            ] {
                assert_eq!(w.count(never), 0, "{dry}: {never}: {:?}", w.calls);
            }
        }
    }
}

#[test]
fn private_qualification_never_restages_an_earlier_preparation() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    let options = ReleaseOptions {
        commit: Some(TIP.into()),
        ..mac()
    };
    let (code, last) = w.run(ReleaseOptions {
        prepare: true,
        ..options.clone()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    w.engine = "f1f1f1f1f1".into();
    w.candidate_missing = true;
    w.calls.clear();
    let (code, last) = w.run(ReleaseOptions {
        qualify: true,
        ..options
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("stage --dry-run"), 1);
    for never in ["stage", "publish", "cut"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
}

#[test]
fn a_prepare_stops_before_the_publish_and_the_bare_run_reasks_then_releases() {
    let mut w = World::new();
    w.child_note_at_stage = true;
    let (code, last) = w.run(ReleaseOptions {
        prepare: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with("PREPARED aterm 0.6.0 (build 1791600000)"),
        "{last}"
    );
    for heavy in ["build", "verify --tree", "verify --bundle", "stage"] {
        assert_eq!(w.count(heavy), 1, "{heavy}: {:?}", w.calls);
    }
    assert!(
        w.cut.is_none() && w.published.is_empty(),
        "nothing claimed or public"
    );
    assert_eq!(
        w.running_at_prepared,
        Some(false),
        "the claim goes before the line"
    );
    assert!(!w.release_dir().join(RUNNING_CHILD_FILE).exists());
    let prepared = w
        .journal()
        .unwrap()
        .prepared
        .expect("PREPARED is journaled");
    w.child_note_at_stage = false;

    type Change = fn(&mut World);
    let stops: [(&str, Change, Change, i32); 2] = [
        (
            "another prover",
            |w| {
                w.prover = Prover {
                    bin: PathBuf::from("/store/trust/9300/bin"),
                    commit: "fd87ed4a0060000000000000000000000000000".into(),
                    source: "TRUST_STAGE2_BIN".into(),
                };
            },
            |w| w.prover = prover(),
            1,
        ),
        (
            "origin unanswered",
            |w| w.ls_remote_down = true,
            |w| w.ls_remote_down = false,
            3,
        ),
    ];
    for (case, change, mend, exit) in stops {
        change(&mut w);
        w.calls.clear();
        let (code, last) = w.run(mac());
        assert_eq!(code, exit, "{case}: {last}\n{:#?}", w.out);
        assert!(last.starts_with("REFUSED at 1/10 checks"), "{case}: {last}");
        for never in [
            "sync",
            "build",
            "verify --tree",
            "verify --bundle",
            "stage",
            "publish",
        ] {
            assert_eq!(w.count(never), 0, "{case}: {never}: {:?}", w.calls);
        }
        assert_eq!(
            w.journal().unwrap().prepared,
            Some(prepared.clone()),
            "{case}: the prepare is kept"
        );
        mend(&mut w);
    }

    w.engine = "f1f1f1f1f1".into();
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with("RELEASED aterm 0.6.0 (build 1791600000)"),
        "{last}"
    );
    for never in [
        "bump",
        "commit-gates",
        "reserve",
        "build",
        "verify --tree",
        "verify --bundle",
    ] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    let at = |call: &str| {
        w.calls
            .iter()
            .position(|c| c == call)
            .unwrap_or_else(|| panic!("{call}: {:?}", w.calls))
    };
    for asked in ["audit", "ls-remote v0.6.0", "engine-now"] {
        assert!(at(asked) < at("stage"), "{asked}: {:?}", w.calls);
    }
    assert_eq!(w.count("stage"), 1, "{:?}", w.calls);
    assert!(at("stage") < at("publish"), "{:?}", w.calls);
}

#[test]
fn a_prepare_overtaken_after_its_publish_is_prepared_again_then_released() {
    let mut w = World::new();
    let (code, last) = w.run(ReleaseOptions {
        prepare: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let first = w.journal().unwrap().prepared.unwrap();

    w.overtaken_at_cut = true;
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("REFUSED at 8/10 cut"), "{last}");
    assert!(
        w.published.contains_key(BUMP) && w.cut.is_none(),
        "public, nothing claimed"
    );

    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("REFUSED at 4/10 qualify"), "{last}");
    for never in [
        "build",
        "verify --tree",
        "verify --bundle",
        "stage",
        "publish",
    ] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert_eq!(
        w.journal().unwrap().prepared,
        Some(first.clone()),
        "the record is kept"
    );

    w.calls.clear();
    let (code, last) = w.run(ReleaseOptions {
        prepare: true,
        ..mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let fresh = w.journal().unwrap().prepared.unwrap();
    assert!(fresh.build > first.build, "{fresh:?}");
    assert_eq!(Some(fresh.macho_sha256.clone()), w.macho());
    assert_eq!(
        [
            "reserve",
            "build",
            "verify --tree",
            "verify --bundle",
            "stage",
            "publish"
        ]
        .map(|c| w.count(c)),
        [1, 1, 0, 1, 0, 0],
        "{:?}",
        w.calls
    );

    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    for never in [
        "build",
        "verify --tree",
        "verify --bundle",
        "stage",
        "publish",
    ] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert_eq!(w.cut.as_ref().unwrap().build, fresh.build);
}

#[test]
fn what_moved_while_the_long_steps_ran_stages_again_before_the_publish() {
    type Move = fn(&mut World);
    let cases: [(&str, Move, &str); 2] = [
        (
            "the engine moved",
            |w| w.engine = "f3f3f3f3f3".into(),
            "f3f3f3f3f3",
        ),
        (
            "the retained candidate was lost",
            |w| w.candidate_missing = true,
            "e7e7e7e7e7",
        ),
    ];
    for (case, moved, by) in cases {
        let mut w = World::new();
        w.after_verify = Some(moved);
        let (code, last) = w.run(bump());
        assert_eq!(code, 0, "{case}: {last}\n{:#?}", w.out);
        let staged: Vec<usize> = w
            .calls
            .iter()
            .enumerate()
            .filter(|(_, c)| c.as_str() == "stage")
            .map(|(i, _)| i)
            .collect();
        let at = |call: &str| w.calls.iter().position(|c| c == call).unwrap();
        assert!(
            staged.len() == 2
                && at("reserve") < staged[0]
                && staged[0] < at("verify --bundle")
                && at("verify --bundle") < staged[1]
                && staged[1] < at("publish"),
            "{case}: {:?}",
            w.calls
        );
        assert_eq!(w.staged_by.as_deref(), Some(by), "{case}");
    }
}

#[test]
fn relative_path_flags_are_anchored_once_where_the_release_started() {
    let root = aterm_tempfile::tempdir().unwrap();
    let start = root.path().join("clone/sub");
    std::fs::create_dir_all(&start).unwrap();
    std::fs::create_dir_all(root.path().join("publication")).unwrap();
    let opts = ReleaseOptions {
        engine: Some(PathBuf::from("../../publication")),
        release_credentials: Some(PathBuf::from("creds.toml")),
        linux: crate::release::LinuxDecl {
            artifacts: Some(PathBuf::from("/abs/linux")),
            ..Default::default()
        },
        ..ReleaseOptions::default()
    };
    let args: Vec<std::ffi::OsString> = [
        "release",
        "--engine",
        "../../publication",
        "--release-credentials",
        "creds.toml",
        "--linux-artifacts",
        "/abs/linux",
    ]
    .iter()
    .map(Into::into)
    .collect();
    let anchored = opts.anchored(&start);
    assert_eq!(
        anchored.engine,
        Some(root.path().join("publication").canonicalize().unwrap())
    );
    // The exec is handed no `--engine`: its engine is the release's own, handed it as
    // PUBLICATION_ENGINE, which an older cutter would otherwise run in place of.
    let again = anchored.exec_args(args);
    let want: Vec<std::ffi::OsString> = vec![
        "release".into(),
        "--release-credentials".into(),
        start.join("creds.toml").into_os_string(),
        "--linux-artifacts".into(),
        "/abs/linux".into(),
    ];
    assert_eq!(again, want);
}

/// The cutter exec'd at R receives no `--fix`: it resumes the head the journal names, whose hops
/// the preceding image made, and every other flag is passed on. NEGATIVE CONTROL: keep `--fix`
/// and its commits in `exec_args`, and they reach the new image.
#[test]
fn the_execed_cutter_drops_fix_and_resumes_the_journaled_head() {
    let args = [
        "release",
        "--fix",
        FIX,
        FIX2,
        "--mac-only",
        "--fix",
        N2,
        "--verbose",
    ];
    let again = mac().exec_args(args.iter().map(Into::into).collect());
    let want: Vec<std::ffi::OsString> = ["release", "--mac-only", "--verbose"]
        .iter()
        .map(Into::into)
        .collect();
    assert_eq!(again, want);
    let again = mac().exec_args(
        ["release", "--commit", TIP, "--supersede", "--mac-only"]
            .iter()
            .map(Into::into)
            .collect(),
    );
    let want: Vec<std::ffi::OsString> = ["release", "--commit", TIP, "--mac-only"]
        .iter()
        .map(Into::into)
        .collect();
    assert_eq!(
        again, want,
        "the replacement was already recorded before exec"
    );
}

/// `--fix` takes one or more commits, may be repeated, and combines with no flag that starts a
/// line (`--bump`, `--commit`) or ends the run (`--forget`, `--abandon`). NEGATIVE CONTROL: drop
/// the `--bump`/`--commit` refusal, and `--fix a --bump` parses.
#[test]
fn fix_takes_its_commits_and_no_flag_that_starts_a_line() {
    let parse = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        crate::cli::parse(&args)
    };
    assert!(matches!(
        parse(&["release", "--fix", "a1", "b2", "--mac-only", "--fix", "c3", "--dry-run"]),
        Ok(crate::cli::Cmd::Release(o))
            if o.fix == ["a1", "b2", "c3"] && o.linux.mac_only && o.dry_run
    ));
    for bad in [
        &["release", "--fix"][..],
        &["release", "--fix", "--mac-only"],
        &["release", "--fix", "a1", "--bump"],
        &["release", "--bump", "--fix", "a1"],
        &["release", "--commit", "origin/main", "--fix", "a1"],
        &["release", "--fix", "a1", "--commit", "b2"],
        &["release", "--abandon", "v0.6.0", "--fix", "a1"],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_public_version_is_major_minor_zero() {
    use crate::release::public_version;
    assert_eq!(public_version("0.101.0"), Some((0, 101)));
    for bad in ["0.101", "0.101.1", "0.101.0-dev", "v0.1.0", "0..0", ""] {
        assert_eq!(public_version(bad), None, "{bad}");
    }
}

#[test]
fn the_verify_step_refuses_on_any_exit_but_a_recorded_pass() {
    for (code, exit) in [(1, 1), (3, 3), (2, 3), (0, 3)] {
        let mut w = World::new();
        w.verify_code = code;
        w.verify_passes = false;
        let (got, last) = w.run(bump());
        assert_eq!(got, exit, "verify exit {code}: {last}");
        assert!(
            last.starts_with("REFUSED at 4/10 qualify"),
            "verify exit {code}: {last}"
        );
        assert_eq!(w.count("publish"), 0, "nothing public: {:?}", w.calls);
    }
}

/// PLANTED RED (f), design §8: a test that fails once and would pass if run again. Its FAIL
/// record refuses R's tree at minute one of the next run's stage, exit 1, naming the check and
/// the log, before the disk is asked for room for work the tree can never pass, and nothing is
/// staged, built or verified again: only a new hop changes the tree, so the remedy is `--fix`.
/// Negative controls: without the stage's FAIL rule the next run gets past its stage; the FAIL
/// rule after the disk check refuses for room instead; reading a FAIL as missing, with no FAIL
/// rule (what the single pass record did), verifies the flaky test once more and releases; a
/// FAIL remedy that re-selects main (`--commit origin/main`) fails the remedy assertion.
#[test]
fn a_fail_is_never_rerolled_into_a_pass() {
    let mut w = World::new();
    w.verify_outcomes.insert("test", "FAIL");
    w.verify_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 4/10 qualify"), "{last}");
    assert!(last.contains("FAILED test"), "{last}");
    assert_eq!(w.verified(), 1);

    // The flaky test would pass now, and the release's disk claim would be refused: the FAIL is
    // what refuses, before any room is sought.
    w.verify_outcomes.clear();
    w.verify_code = 0;
    w.claim_refusal = Some(Refusal::new(
        "the release's 30 GiB can never fit on /c",
        "free space, then re-run",
    ));
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 4/10 qualify"), "{last}");
    assert!(
        last.contains("FAILED test") && last.contains("/l/06-verify.log"),
        "names the check and its log: {last}"
    );
    assert!(last.contains("a new hop changes the tree"), "{last}");
    assert!(
        w.calls
            .iter()
            .any(|call| call.starts_with("failure-hints ")),
        "a stored FAIL on resume also searches main: {:?}",
        w.calls
    );
    assert!(
        last.contains("--fix") && !last.contains("--commit origin/main"),
        "the FAIL's remedy carries a landed fix onto the line: {last}"
    );
    for step in [
        "verify --tree",
        "verify --bundle",
        "build",
        "stage",
        "publish",
    ] {
        assert_eq!(w.count(step), 0, "{step}: {:?}", w.calls);
    }
    assert!(
        !w.calls.iter().any(|c| c.starts_with("disk-claim")),
        "no room is claimed for a FAILed tree: {:?}",
        w.calls
    );
}

/// A FAIL refuses only its own tree (A19 with A18's line): after a test FAILs on the line's base,
/// `--fix` carries a landed fix as a hop, the FAIL rule judges the hop's tree, which has no
/// record, and the release verifies the new head once and ships it from there. Negative control:
/// preparation that carries the hop but leaves the run's candidate at the old head refuses with the
/// base's FAIL.
#[test]
fn a_fail_refuses_its_own_tree_and_a_fix_hop_is_verified_anew() {
    let mut w = World::new();
    w.verify_outcomes.insert("test", "FAIL");
    w.verify_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 4/10 qualify"), "{last}");

    for moved in [NEXT, FIX] {
        w.history.push(moved.into());
        w.versions.insert(moved.into(), "0.6.0".into());
    }
    w.verify_outcomes.clear();
    w.verify_code = 0;
    w.calls.clear();
    let (code, last) = w.run(ReleaseOptions {
        fix: vec![FIX.into()],
        ..mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let hop = hop_of(FIX);
    assert_eq!(w.journal().unwrap().commit, hop);
    assert_eq!(
        w.count("verify --tree"),
        1,
        "the hop's tree is verified once"
    );
    assert_eq!(
        w.checks_of(BUMP, w.macho().as_deref())
            .failed
            .iter()
            .map(|f| f.check.as_str())
            .collect::<Vec<_>>(),
        ["test"],
        "the base's FAIL stays its own"
    );
}

/// A run that could not run every check (exit 3) keeps the records of those that finished, and
/// the resume runs exactly the missing checks of each half, carrying exactly the test binaries
/// the test record lists as passed; once they pass the release goes on. Negative control: a
/// resume that asks every check of a half again fails the asserted set.
#[test]
fn a_resume_runs_only_the_missing_checks_and_carries_the_binaries_that_passed() {
    let mut w = World::new();
    w.verify_outcomes.insert("test", "NOT RUN");
    w.verify_code = 3;
    let carried = vec![passed_binary("-p aterm-db --lib", &"aa".repeat(32))];
    w.resume.clone_from(&carried);
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert!(last.starts_with("REFUSED at 4/10 qualify"), "{last}");
    let r = w.journal().unwrap().commit;
    assert_eq!(
        w.checks_of(&r, w.macho().as_deref()).missing,
        ["test", "appkit", "measure"],
        "the bundle half never ran"
    );
    assert_eq!(w.verify_runs.len(), 1);
    assert!(
        w.verify_runs[0].half == Half::Tree && !w.verify_runs[0].strict_subset(),
        "the first run asks every tree check"
    );

    // The tree's resume passes; the bundle half could not run appkit.
    w.verify_outcomes.clear();
    w.verify_outcomes.insert("appkit", "NOT RUN");
    w.verify_half = Some(Half::Bundle);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 3, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("REFUSED at 5/10 bundle"), "{last}");
    let [.., tree, bundle] = w.verify_runs.as_slice() else {
        panic!("{:?}", w.verify_runs);
    };
    assert_eq!(
        (tree.half, tree.checks.as_slice()),
        (Half::Tree, ["test".to_owned()].as_slice())
    );
    assert_eq!(tree.passed_binaries, carried);
    assert!(
        bundle.half == Half::Bundle && !bundle.strict_subset(),
        "{bundle:?}"
    );

    w.verify_outcomes.clear();
    w.verify_half = None;
    w.verify_code = 0;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        (w.count("verify --tree"), w.count("verify --bundle")),
        (0, 1),
        "{:?}",
        w.calls
    );
    let resumed = w.verify_runs.last().unwrap();
    assert_eq!(
        (resumed.half, resumed.checks.as_slice()),
        (Half::Bundle, ["appkit".to_owned()].as_slice())
    );
}

/// A run killed after a test binary failed (no exit code of verify's own) leaves its test FAIL
/// record, and the verify step refuses at once, exit 1, as the FAIL rule does: never a COULD NOT
/// RUN whose resume the next stage would refuse. Negative control: without Run::verify's own
/// FAIL refusal the run ends COULD NOT RUN (exit 3) and claims a resume.
#[test]
fn a_killed_run_that_failed_a_test_is_refused_never_resumed() {
    for killed in [-1, 137] {
        let mut w = World::new();
        w.verify_outcomes.insert("test", "FAIL");
        w.verify_code = killed;
        let (code, last) = w.run(bump());
        assert_eq!(code, 1, "{killed}: {last}");
        assert!(last.starts_with("REFUSED at 4/10 qualify"), "{last}");
        assert!(last.contains("a new hop changes the tree"), "{last}");
        let r = w.journal().unwrap().commit;
        let judged = w.checks_of(&r, w.macho().as_deref());
        assert_eq!(
            judged
                .failed
                .iter()
                .map(|f| f.check.as_str())
                .collect::<Vec<_>>(),
            ["test"]
        );
        assert_eq!(w.count("publish"), 0, "nothing public: {:?}", w.calls);
    }
}

/// STEP 6 AS AN AGENT RUNS IT: the real backend runs R's recorded, content-proved
/// `aterm-verify` copy directly as `release --events <file> --app <app>` in the clone (a resume
/// names its missing `--checks` and its `--passed-binaries`, and
/// `--app` only when it runs measure), and only a run from a clone at R's tree and clean, before
/// and after, under one seal, keeps one record per check its events file says finished, a FAIL
/// included, never what its output says; a moved clone or a changed seal keeps nothing, its
/// `began` record withdrawn, and could not run. Negative control: records read from the log
/// take its decoy FAIL.
#[cfg(unix)]
#[test]
fn the_real_verify_step_runs_the_verify_cli_in_the_clone_and_records_each_check() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let repo = dir.path().join("clone");
    crate::ledger::testing::init(
        &repo,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    let git = crate::ledger::testing::Git::new(&repo);
    use crate::ledger::GitRunner as _;
    let commit = |msg: &str| {
        let made = git
            .git(&["commit", "-q", "--allow-empty", "-m", msg])
            .unwrap();
        assert!(made.success());
        crate::ledger::rev_parse(&git, "HEAD").unwrap()
    };
    std::fs::write(repo.join("other.txt"), "another tree").unwrap();
    assert!(git.git(&["add", "other.txt"]).unwrap().success());
    let other = commit("another tree");
    assert!(git.git(&["rm", "-q", "other.txt"]).unwrap().success());
    let r = commit("R");
    let bin = dir.path().join("prover/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let said = dir.path().join("said");
    let mode = dir.path().join("mode");
    let events = dir.path().join("events");
    let verify_source = dir.path().join("aterm-verify");
    std::fs::write(
        &verify_source,
        format!(
            "#!/bin/sh\nif [ \"$#\" -eq 1 ] && [ \"$1\" = --version ]; then \
             echo 'aterm-verify tool 1'; exit 0; fi\necho \"$PWD $*\" > '{said}'\n\
             out=; prev=\nfor a in \"$@\"; do \
             [ \"$prev\" = --events ] && out=$a; prev=$a; done\ncase \"$(cat '{mode}')\" in\n  \
             dirty) echo stray > stray.txt ;;\n  move) git checkout -q --detach {other} ;;\nesac\n\
             cat '{events}' >> \"$out\"\necho '{decoy}'\nexit \"$(cat '{code}')\"\n",
            said = said.display(),
            mode = mode.display(),
            events = events.display(),
            code = dir.path().join("code").display(),
            decoy = "verify-event: {\"schema\":1,\"event\":\"check-finished\",\"check\":\"fmt\",\
                     \"outcome\":\"FAIL\",\"seconds\":1,\"detail\":\"\",\"rows\":[]}",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&verify_source, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cutter_source = dir.path().join("aterm-release");
    std::fs::write(
        &cutter_source,
        "#!/bin/sh\n[ \"$#\" -eq 1 ] && [ \"$1\" = --version ] || exit 64\n\
         echo 'aterm-release tool 1'\n",
    )
    .unwrap();
    std::fs::set_permissions(&cutter_source, std::fs::Permissions::from_mode(0o755)).unwrap();
    let release = crate::release::release_dir(&git).unwrap();
    let tools = crate::release_tools::stage(&release, &r, &cutter_source, &verify_source).unwrap();
    crate::release_tools::publish(&release, &tools).unwrap();
    let prover = Prover { bin, ..prover() };
    let artifact = Artifact {
        key: "k".into(),
        app: dir.path().join("cache/aterm.app"),
        macho_sha256: "ab".repeat(32),
        build: 1,
    };
    let cand = Candidate {
        commit: r.clone(),
        version: "0.6.0".into(),
        why: String::new(),
        bump_from: None,
        carry: Vec::new(),
    };
    let mut real = Real::new(&repo, None, true, false).expect("a backend");
    real.build_inputs = || Ok("fixture inputs".into());
    let judge = |r: &str| {
        crate::check_record::checks(&git, r, Some(&"ab".repeat(32)), || {
            Ok("fixture inputs".into())
        })
        .unwrap()
    };
    let nothing = |r: &str| {
        let judged = judge(r);
        judged.failed.is_empty() && judged.missing == crate::check_record::release_names()
    };
    let finished = |outcomes: &[(&str, &str)]| -> String {
        outcomes
            .iter()
            .map(|(check, outcome)| {
                format!(
                    "verify-event: {}\n",
                    aterm_json::json!({"schema":1,"event":"check-finished","check":check,
                        "outcome":outcome,"seconds":1.5,"detail":"","rows":[]})
                )
            })
            .collect()
    };
    let all_ok: Vec<(&str, &str)> = aterm_verify::checks::RELEASE
        .iter()
        .map(|c| (c.name(), "ok"))
        .collect();
    let set = |code: &str, how: &str, said_events: &str| {
        std::fs::write(dir.path().join("code"), code).unwrap();
        std::fs::write(&mode, how).unwrap();
        std::fs::write(&events, said_events).unwrap();
    };
    let every = |half: Half| crate::check_record::Checks::no("none").run(half);
    let repo_seen = std::fs::canonicalize(&repo).unwrap();
    let verify = |real: &mut Real, run: &VerifyRun| {
        Backend::branches(real).verify(&cand, &prover, run, Some(&artifact), &BranchStop::default())
    };

    for how in ["dirty", "move"] {
        set("0", how, &finished(&all_ok));
        let refused = verify(&mut real, &every(Half::Tree)).expect_err("the clone moved");
        assert!(refused.could_not_run, "{refused:?}");
        assert!(nothing(&r), "{how}: nothing is recorded");
        let _ = std::fs::remove_file(repo.join("stray.txt"));
        assert!(
            git.git(&["checkout", "-q", "--detach", &r])
                .unwrap()
                .success()
        );
    }

    std::fs::write(repo.join("stray.txt"), "before").unwrap();
    std::fs::remove_file(&said).unwrap();
    set("0", "", &finished(&all_ok));
    let refused = verify(&mut real, &every(Half::Tree)).expect_err("a dirty clone");
    assert!(refused.could_not_run, "{refused:?}");
    assert!(!said.exists(), "nothing ran");
    assert!(nothing(&r));
    std::fs::remove_file(repo.join("stray.txt")).unwrap();

    fn changing_inputs() -> Result<String, String> {
        static READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        Ok(READS
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .to_string())
    }
    real.build_inputs = changing_inputs;
    let refused = verify(&mut real, &every(Half::Bundle)).expect_err("inputs changed");
    assert!(
        refused.why.contains("compiler or SDK bytes changed"),
        "{refused:?}"
    );
    assert!(
        refused.could_not_run,
        "a changed seal decides nothing: {refused:?}"
    );
    assert!(nothing(&r), "changed inputs keep no record");
    let began =
        crate::check_record::checks(&git, &r, Some(&"ab".repeat(32)), || Ok("0".into())).unwrap();
    assert!(
        began.failed.is_empty(),
        "the run's began record is withdrawn: {}",
        began.line()
    );
    real.build_inputs = || Ok("fixture inputs".into());

    // A resume names exactly its missing checks and the binaries that passed.
    let resume = VerifyRun {
        half: Half::Tree,
        checks: vec!["lint".into(), "test".into()],
        passed_binaries: vec![passed_binary("-p a --lib", &"aa".repeat(32))],
        passed_lint_packages: vec![passed_lint_package("a@1.0.0")],
    };
    let binary = |spec: &str, sha: &str, outcome: &str| {
        format!(
            "verify-event: {}\n",
            aterm_json::json!({"schema":1,"event":"test-binary-finished","test":spec,
                "outcome":outcome,"exe_sha256":sha,"content_key":"cc".repeat(32),
                "inputs_bounded":true,"input_reason":null,"recorded_execution_s":1.25,
                "json":false,"carried":false})
        )
    };
    let lint_package = |spec: &str, key: &str, outcome: &str| {
        format!(
            "verify-event: {}\n",
            aterm_json::json!({"schema":1,"event":"lint-package-finished","spec":spec,
                "outcome":outcome,"content_key":key,"inputs_bounded":true,
                "input_reason":null,"execution_s":0.75,"carried":false})
        )
    };
    set(
        "3",
        "",
        &format!(
            "{}{}{}{}",
            finished(&[("lint", "ok")]),
            lint_package("a@1.0.0", &"dd".repeat(32), "ok"),
            binary("-p a --lib", &"aa".repeat(32), "ok"),
            binary("-p b --lib", &"bb".repeat(32), "NOT RUN")
        ),
    );
    let ran = verify(&mut real, &resume).expect("it ran");
    assert_eq!(ran.code, 3);
    let said_now = std::fs::read_to_string(&said).unwrap();
    let after = |flag: &str| {
        said_now
            .split_whitespace()
            .skip_while(|w| *w != flag)
            .nth(1)
            .unwrap_or_else(|| panic!("{flag} <path>: {said_now}"))
            .to_owned()
    };
    let (context, passed, passed_lint, events_file) = (
        after("--carry-context"),
        after("--passed-binaries"),
        after("--passed-lint-packages"),
        after("--events"),
    );
    assert_eq!(
        said_now.trim(),
        format!(
            "{} release --tree --carry-context {context} --checks lint,test \
             --passed-binaries {passed} --passed-lint-packages {passed_lint} \
             --events {events_file}",
            repo_seen.display(),
        ),
        "no measure, so no --app"
    );
    let proofs =
        aterm_verify::testrun::read_passed(&std::fs::read_to_string(&passed).unwrap()).unwrap();
    assert_eq!(proofs.len(), 1);
    assert_eq!(proofs["-p a --lib"].exe_sha256, Some("aa".repeat(32)));
    assert_eq!(proofs["-p a --lib"].content_key, Some("cc".repeat(32)));
    let lint_proofs =
        aterm_verify::lint_content::read_passed(&std::fs::read_to_string(&passed_lint).unwrap())
            .unwrap();
    assert_eq!(lint_proofs.len(), 1);
    assert_eq!(lint_proofs["a@1.0.0"].content_key, Some("dd".repeat(32)));
    let judged = judge(&r);
    assert!(
        !judged.missing.iter().any(|m| m == "lint"),
        "{}",
        judged.line()
    );
    assert_eq!(
        judged.run(Half::Tree).passed_binaries,
        [passed_binary("-p a --lib", &"aa".repeat(32))],
        "the test record of the killed run lists what passed"
    );

    set("0", "", &finished(&all_ok));
    let ran = verify(&mut real, &every(Half::Tree)).expect("it ran");
    assert_eq!(ran.code, 0);
    assert!(
        !judge(&r).complete(),
        "the bundle's checks are not the tree's"
    );
    let ran = verify(&mut real, &every(Half::Bundle)).expect("it ran");
    assert_eq!(ran.code, 0);
    let said_now = std::fs::read_to_string(&said).unwrap();
    let bundle_events = said_now
        .split_whitespace()
        .skip_while(|w| *w != "--events")
        .nth(1)
        .unwrap_or_else(|| panic!("--events <path>: {said_now}"))
        .to_owned();
    assert_ne!(bundle_events, events_file, "each half has its own events");
    assert_eq!(
        said_now.trim(),
        format!(
            "{} release --bundle --events {bundle_events} \
             --app {}",
            repo_seen.display(),
            artifact.app.display()
        ),
        "a run of a whole half names no checks"
    );
    assert!(
        judge(&r).complete(),
        "every check recorded ok for R's tree on these bytes"
    );
    assert!(!judge(&other).complete(), "another tree has no records");

    set("1", "", &finished(&[("fmt", "ok"), ("lint", "FAIL")]));
    let ran = verify(&mut real, &every(Half::Tree)).expect("it ran");
    assert_eq!(ran.code, 1);
    let judged = judge(&r);
    assert_eq!(
        judged
            .failed
            .iter()
            .map(|f| f.check.as_str())
            .collect::<Vec<_>>(),
        ["lint"],
        "a FAIL is recorded: {}",
        judged.line()
    );
    let verified = repo.join(".git/aterm-release/verified");
    std::fs::remove_dir_all(&verified).unwrap();
    set("1", "", "");
    verify(&mut real, &every(Half::Tree)).expect("it ran");
    assert_eq!(
        judge(&r)
            .failed
            .iter()
            .map(|f| f.check.as_str())
            .collect::<Vec<_>>(),
        [crate::check_record::RUN_LEVEL],
        "an exit 1 that names no FAIL still refuses the tree"
    );
}

#[test]
fn a_spent_reservation_is_built_again_never_renumbered() {
    type Spend = fn(&mut World);
    let cases: [(&str, Spend, i32, &str); 2] = [
        (
            "overtaken while verified",
            |w| {
                w.after_verify = Some(|w| {
                    let reserved = w.journal().unwrap().reserved.unwrap();
                    w.tail = reserved.build;
                });
            },
            3,
            "REFUSED at 5/10 bundle: COULD NOT RUN",
        ),
        (
            "claimed here, no cut journal",
            |w| w.claim_lands_unjournaled = true,
            1,
            "REFUSED at 8/10 cut",
        ),
    ];
    for (case, spend, exit, at) in cases {
        let mut w = World::new();
        spend(&mut w);
        let (code, last) = w.run(bump());
        assert_eq!(code, exit, "{case}: {last}");
        assert!(last.starts_with(at), "{case}: {last}");
        assert!(w.cut.is_none(), "{case}: no cut journal");
        let spent = w.journal().unwrap().reserved.unwrap();

        w.after_verify = None;
        w.build_admission_waits = 0;
        let (code, last) = w.run(mac());
        assert_eq!(code, 0, "{case}: {last}\n{:#?}", w.out);
        let fresh = w.journal().unwrap().reserved.unwrap();
        assert!(fresh.build > spent.build, "{case}");
        assert_eq!(
            (w.count("reserve"), w.count("build")),
            (2, 2),
            "{case}: {:?}",
            w.calls
        );
        assert_eq!(
            w.verify_runs.last().unwrap().checks,
            ["measure"],
            "{case}: the tree's records stand; only the new bundle is judged again"
        );
        assert!(
            w.build_admission_waits > 0,
            "{case}: a resumed build claims its slices first even when tree records stand"
        );
        assert_eq!(w.cut.as_ref().unwrap().build, fresh.build, "{case}");
    }
}

/// A RESERVATION SPENT WHILE A PRIVATE QUALIFICATION'S GRAPH RUNS IS BUILT AGAIN, AND THE LOCAL
/// EXPORT IS NOT CHECKED AGAIN: a peer's cut reaches R's reservation as `--qualify`'s export
/// check runs; step 4's second round reserves anew and builds once more, and the export that
/// passed in this run stands. NEGATIVE CONTROLS: planning the export under `--qualify` whatever
/// passed checks it twice; refusing the set-aside bundle refuses step 4 before its second round.
#[test]
fn a_reservation_spent_under_qualify_is_built_again_with_one_export_check() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.after_stage = Some(|w| {
        w.after_stage = None;
        if let Some(reserved) = w.journal().and_then(|j| j.reserved) {
            w.tail = reserved.build;
        }
    });
    let (code, last) = w.run(ReleaseOptions {
        qualify: true,
        commit: Some(TIP.into()),
        ..mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        (
            w.count("stage --dry-run"),
            w.count("reserve"),
            w.count("build")
        ),
        (1, 2, 2),
        "{:?}",
        w.calls
    );
    assert!(
        w.out
            .iter()
            .any(|l| l.contains("R's export checked locally in this run")),
        "{:#?}",
        w.out
    );
}

#[test]
fn missing_validation_tools_stop_before_staging_or_product_compilation() {
    let mut world = World::new();
    world.validation_tools =
        Err(Refusal::new("missing pinned browser", "provision it").could_not_run());
    let (code, last) = world.run(bump());
    assert_ne!(code, 0);
    assert!(last.contains("missing pinned browser"), "{last}");
    assert_eq!(world.count("validation-tools"), 1);
    for expensive in [
        "stage",
        "build",
        "verify --tree",
        "verify --bundle",
        "publish",
    ] {
        assert_eq!(world.count(expensive), 0, "{expensive}: {:?}", world.calls);
    }
}

#[test]
fn after_cut_records_a_red_hold_as_a_fact_without_refusing_the_public_release() {
    let mut world = World::new();
    let verdict = "field hold: FAIL: meta CheckpointMeta.program_state_began (listed, but no desk reaches it)";
    world.after_cut = Ok(verdict.into());
    assert_eq!(world.run(bump()).0, 0);
    assert!(
        world
            .out
            .iter()
            .any(|line| line.contains("after-cut") && line.contains(verdict))
    );
    let event = world
        .out
        .iter()
        .filter_map(|line| line.strip_prefix("release-event: "))
        .map(|json| aterm_json::from_str::<aterm_json::Value>(json).unwrap())
        .find(|event| {
            event["stage"].as_str() == Some("after-cut") && event["outcome"].as_str() == Some("ok")
        })
        .expect("the completed after-cut is journaled");
    assert!(event["detail"].as_str().unwrap().contains(verdict));
    assert!(world.fixtures_on_main);
    assert!(world.journal().unwrap().finished());
}

#[test]
fn structured_stage_events_include_source_log_and_separate_waiting_from_execution() {
    let mut world = World::new();
    assert_eq!(world.run(bump()).0, 0);
    let events: Vec<aterm_json::Value> = world
        .out
        .iter()
        .filter_map(|line| line.strip_prefix("release-event: "))
        .map(|json| aterm_json::from_str(json).unwrap())
        .collect();
    let qualify = events
        .iter()
        .find(|event| {
            event["stage"].as_str() == Some("qualify") && event["outcome"].as_str() == Some("ok")
        })
        .expect("qualify completion");
    assert_eq!(qualify["source"].as_str(), Some(BUMP));
    assert!(qualify["log"].as_str().is_some());
    assert!(qualify["waiting_s"].as_u64().is_some());
    assert!(qualify["execution_s"].as_u64().is_some());
    // Each branch of the graph says it started and how it ended, with its time.
    let branch = |name: &str, phase: &str| {
        events.iter().find(|e| {
            e["event"].as_str() == Some("qualify-branch")
                && e["branch"].as_str() == Some(name)
                && e["phase"].as_str() == Some(phase)
        })
    };
    for name in ["tree", "build", "export"] {
        assert!(branch(name, "started").is_some(), "{name}: {events:#?}");
        let passed = branch(name, "passed").unwrap_or_else(|| panic!("{name}: {events:#?}"));
        assert!(passed["seconds"].as_u64().is_some(), "{passed:?}");
    }
}

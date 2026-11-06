// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship release`'s step machine ([`crate::release`]) driven against a fake world.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::check_record::{Checks, Failed, State, VerifyRun};
use crate::line::{Admitted, Fix, Hop, Line};
use crate::provision::{AskWords, Audited, Finding};
use crate::release::{
    AfterCut, Answer, Artifact, Backend, Built, Candidate, CutState, EngineNow, EngineRows, Facts,
    JOURNAL_FILE, Journal, Prover, RUNNING_CHILD_FILE, RUNNING_FILE, Ran, Refusal, ReleaseOptions,
    Reservation, StageTarget, Step, run_with,
};
use crate::release_steps::Real;

const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TIP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BUMP: &str = "cccccccccccccccccccccccccccccccccccccccc";
const NEXT: &str = "dddddddddddddddddddddddddddddddddddddddd";
/// The re-run as R's own cutter: its pid and start, as the claim it takes over names them.
const RERUN: (u32, &str) = (4343, "Thu Oct  9 02:01:00 2026");
const BUMP2: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
/// Landed, non-merge fixes on main, after the line's base.
const FIX: &str = "1111111111111111111111111111111111111111";
const FIX2: &str = "2222222222222222222222222222222222222222";
/// Main's movement past the base that is no fix of the line's.
const N2: &str = "3333333333333333333333333333333333333333";
const N3: &str = "4444444444444444444444444444444444444444";

/// The hop the fake makes of `fix`.
fn hop_of(fix: &str) -> String {
    format!("9{}", &fix[1..])
}

struct World {
    fleet_refusal: Option<String>,
    fleet_selected: Option<Line>,
    existing_fleet_line: Option<Line>,
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
    /// This process's ancestors, its parent's pid first.
    ancestors: Vec<u32>,
    /// How the re-run as R's own cutter ends: its exit code, or why it could not start.
    reexec: Result<i32, Refusal>,
    /// The re-run took the run claim over (it names [`RERUN`], alive) before it ended.
    reexec_takes_claim: bool,
    /// The re-run's end removed the claim it took over.
    rerun_removes_claim: bool,
    times: String,
    history: Vec<String>,
    versions: BTreeMap<String, String>,
    head: String,
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
    resume: Vec<(String, String)>,
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
    /// The cut's claim lands (main takes the release commit, the ledger its line) and the
    /// cut then fails before it journals anything.
    claim_lands_unjournaled: bool,
    machine_refusals: Vec<Refusal>,
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
    lease_error: Option<String>,
    /// Why step 1's build-inputs seal cannot be taken.
    seal_error: Option<String>,
    /// The engine's code now, and the engine that staged the attestation in publish/.out.
    engine: String,
    staged_by: Option<String>,
    /// Whether the run claim was still on disk when the PREPARED line was said.
    running_at_prepared: Option<bool>,
    /// The run claim's text (empty when there was none) when an INTERRUPTED line was said.
    claim_at_interrupted: Option<String>,
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
    /// The meaning check's answer ([`crate::line::verify`]).
    line_verify: Result<String, Refusal>,
}

impl World {
    fn new() -> Self {
        let mut versions = BTreeMap::new();
        versions.insert(OLD.into(), "0.5.0".into());
        versions.insert(TIP.into(), "0.5.0".into());
        let mut origin_tags = BTreeMap::new();
        origin_tags.insert("v0.5.0".into(), OLD.into());
        Self {
            fleet_refusal: None,
            fleet_selected: None,
            existing_fleet_line: None,
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
            ancestors: Vec::new(),
            reexec: Err(Refusal::new("the fake's re-run could not start", "nothing")),
            reexec_takes_claim: false,
            rerun_removes_claim: false,
            times: String::new(),
            history: vec![OLD.into(), TIP.into()],
            versions,
            head: TIP.into(),
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
            claim_lands_unjournaled: false,
            machine_refusals: Vec::new(),
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
            lease_error: None,
            seal_error: None,
            engine: "e7e7e7e7e7".into(),
            staged_by: None,
            running_at_prepared: None,
            claim_at_interrupted: None,
            child_note_at_stage: false,
            ls_remote_down: false,
            candidate_missing: false,
            unfit: BTreeMap::new(),
            pre_line: BTreeSet::new(),
            tree_unread: false,
            carry_refusal: None,
            line_verify: Ok("the line verifies".into()),
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
                    return State::Missing { resume: Vec::new() };
                }
                match record(check.name(), executable.filter(|_| measure)) {
                    Some("ok") => State::Passed,
                    Some("FAIL") => State::Failed { log: log.clone() },
                    _ => State::Missing {
                        resume: self.resume.clone(),
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

impl Backend for World {
    fn fleet_existing(&mut self, version: &str) -> Result<Option<Line>, String> {
        Ok(self
            .existing_fleet_line
            .clone()
            .filter(|line| line.version == version))
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
        if line.starts_with("INTERRUPTED") {
            let claim = World::release_dir(self).join(RUNNING_FILE);
            self.claim_at_interrupted = Some(std::fs::read_to_string(claim).unwrap_or_default());
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
    fn ancestors(&mut self) -> Vec<u32> {
        self.ancestors.clone()
    }
    fn release_dir(&mut self) -> PathBuf {
        World::release_dir(self)
    }
    fn times(&mut self) -> String {
        self.times.clone()
    }
    fn record_time(&mut self, version: &str, step: Step, secs: u64) {
        self.times.push_str(&format!(
            "{}\t{version}\t{}\t{secs}\n",
            self.now,
            step.name()
        ));
    }
    fn prover(&mut self, _journaled: Option<&str>) -> Result<Prover, String> {
        self.observations.push("prover");
        Ok(self.prover.clone())
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
        Ok(self.tip())
    }
    fn head(&mut self) -> String {
        self.head.clone()
    }
    fn branch_is_main(&mut self) -> bool {
        self.on_main
    }
    fn rev(&mut self, rev: &str) -> Option<String> {
        self.pos(rev).map(|i| self.history[i].clone())
    }
    fn version_at(&mut self, rev: &str) -> Option<String> {
        let full = self.rev(rev)?;
        self.versions.get(&full).cloned()
    }
    fn is_ancestor(&mut self, a: &str, b: &str) -> bool {
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
        let hops: Vec<Hop> = fixes
            .iter()
            .map(|fix| Hop {
                commit: hop_of(&fix.commit),
                carries: fix.commit.clone(),
                why: fix.subject.clone(),
                paths: fix.paths.clone(),
            })
            .collect();
        for hop in &hops {
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
        self.observations.push("validation-tools");
        self.calls.push("validation-tools".into());
        self.validation_tools.clone()
    }

    fn move_to(&mut self, commit: &str) -> Result<(), Refusal> {
        self.calls.push(format!("move {}", &commit[..4]));
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
    fn reexec(&mut self, _prover: &Prover) -> Result<i32, Refusal> {
        self.calls.push("reexec".into());
        let path = World::release_dir(self).join(RUNNING_FILE);
        let claim = std::fs::read_to_string(&path);
        if claim.is_ok_and(|text| text.lines().any(|l| l == "handed")) {
            self.calls.push("reexec with the claim handed".into());
        }
        self.interrupt_at("reexec");
        if self.reexec_takes_claim {
            self.alive.insert(RERUN.0, RERUN.1.into());
            std::fs::write(
                &path,
                format!("pid {}\nstarted {}\nlogs /l\n", RERUN.0, RERUN.1),
            )
            .unwrap();
        }
        if self.rerun_removes_claim {
            std::fs::remove_file(&path).unwrap();
        }
        self.reexec.clone()
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
    fn build(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        p: &Prover,
        fixtures: bool,
    ) -> Built {
        self.calls.push("build".into());
        let key = format!("{:0>64}", reserved.build);
        let macho = format!("m{}", reserved.build);
        let app = PathBuf::from(format!("/cache/{key}/aterm.app"));
        self.artifacts
            .insert(key.clone(), (reserved.build, macho.clone(), app.clone()));
        Built {
            artifact: Ok(Artifact {
                key,
                app,
                macho_sha256: macho,
                build: reserved.build,
            }),
            fixtures: fixtures.then(|| self.stage_fixtures(c, reserved, p)),
        }
    }
    fn verify(
        &mut self,
        c: &Candidate,
        _p: &Prover,
        artifact: &Artifact,
        run: &VerifyRun,
    ) -> Result<Ran, Refusal> {
        self.calls.push("verify".into());
        self.verify_runs.push(run.clone());
        assert!(
            self.artifacts
                .values()
                .any(|(_, macho, app)| *app == artifact.app && *macho == artifact.macho_sha256),
            "`verify release` is handed a bundle the build made"
        );
        if self.interrupt_at("verify") {
            return Ok(ran(-1));
        }
        if let Some((real, script, dir)) = &mut self.real {
            let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
            cmd.args(["-c", script.as_str(), "sh"]).arg(dir.as_path());
            return Ok(real.logged(Step::Verify, &mut cmd));
        }
        let mut failed = false;
        for check in &run.checks {
            let outcome = self
                .verify_outcomes
                .get(check.as_str())
                .copied()
                .or(self.verify_passes.then_some("ok"));
            if let Some(outcome) = outcome {
                failed |= outcome == "FAIL";
                let u = (check == "measure").then(|| artifact.macho_sha256.clone());
                self.records
                    .insert((c.commit.clone(), u, check.clone()), outcome);
            }
        }
        // As the real cutter does, an exit 1 that named no FAIL is a run-level FAIL.
        if self.verify_code == 1 && !failed {
            self.records.insert(
                (
                    c.commit.clone(),
                    None,
                    crate::check_record::RUN_LEVEL.into(),
                ),
                "FAIL",
            );
        }
        if let Some(then) = self.after_verify {
            then(self);
        }
        Ok(ran(self.verify_code))
    }
    fn stage(&mut self, c: &Candidate, _p: &Prover, target: StageTarget) -> Result<Ran, Refusal> {
        self.calls.push(
            if target == StageTarget::Local {
                "qualify"
            } else {
                "stage"
            }
            .into(),
        );
        assert!(!self.stage_panics, "a bug in the release");
        if self.stage_stages {
            if target == StageTarget::Local {
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
    fn publish(&mut self, _p: &Prover) -> Result<Ran, Refusal> {
        self.calls.push("publish".into());
        let r = self.head.clone();
        self.published.insert(r.clone(), "e1e1e1e1e1".into());
        self.newest = Some(r.clone());
        if self.publish_code == 0 {
            self.source_attested.insert(r);
        }
        Ok(ran(self.publish_code))
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
    fn cut(&mut self, args: &[String], _p: &Prover) -> Ran {
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
        if !resuming && reserved.build <= self.tail {
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
        self.tail = reserved.build;
        self.cut = Some(CutState {
            version,
            build: reserved.build,
            finished: self.cut_finishes,
        });
        ran(self.cut_code)
    }
    fn after_cut(&mut self, version: &str) -> Result<AfterCut, Refusal> {
        self.calls.push(format!("after-cut {version}"));
        let shape = self.after_cut.clone()?;
        let site = crate::release_steps::site_outcome(&self.site, vec!["== site".into()])?;
        Ok(AfterCut {
            note: format!("{shape}; {}", site.note),
        })
    }
    fn latest(&mut self) -> Result<String, String> {
        match &self.cut {
            Some(c) if c.finished && !self.latest_lags => Ok(format!("v{}", c.version)),
            _ => Ok("v0.5.0".into()),
        }
    }
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
    assert_eq!(world.calls, ["lease", "seal-inputs"]);
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

#[test]
fn a_bump_runs_every_step_in_order_and_ends_released() {
    let mut w = World::new();
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{:#?}", w.out);
    assert!(
        last.starts_with("RELEASED aterm 0.6.0 (build 1791600000)"),
        "{last}"
    );
    let order: Vec<&str> = w
        .calls
        .iter()
        .map(String::as_str)
        .filter(|c| {
            !matches!(*c, "audit" | "lease" | "seal-inputs" | "cut-gates")
                && !c.starts_with("ls-remote")
        })
        .collect();
    assert_eq!(
        order,
        [
            "sync",
            "bump",
            "move cccc",
            "validation-tools",
            "disk-claim 30",
            "commit-gates",
            "stage",
            "reserve",
            "build",
            "stage-fixtures",
            "verify",
            "disk-let-go true",
            "sync",
            "engine-now",
            "candidate-now",
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
        at(&w, "disk-claim 45") < at(&w, "stage") && at(&w, "stage") < at(&w, "build"),
        "{:?}",
        w.calls
    );
    let go = at(&w, "disk-let-go true").expect("the claim ends once verify passed");
    assert!(
        at(&w, "verify") < Some(go) && Some(go) < at(&w, "publish"),
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
            && at(&w, "verify") < released
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
        w.count("verify") == 0
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
            && at(&w, "verify") < at(&w, "disk-let-go true")
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
        last.starts_with("REFUSED at 8/11 publish"),
        "{code}: {last}"
    );
    assert!(
        w.count("publish") == 0 && at(&w, "verify") < at(&w, "disk-let-go true"),
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
        ["stage", "build", "verify"].map(|c| w.count(c)) == [1, 0, 0]
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
    assert!(last.starts_with("REFUSED at 4/11 stage"), "{last}");
    assert_eq!(
        (w.count("stage"), w.count("build"), w.count("verify")),
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
        "verify",
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

#[test]
fn a_generator_that_refuses_refuses_the_release_before_its_claim() {
    let mut w = World::new();
    w.generator_refuses = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{:#?}", w.out);
    assert!(last.starts_with("REFUSED at 5/11 build"), "{last}");
    for call in ["verify", "publish", "land-fixtures"] {
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
            w.count("build")
        ),
        (1, 0, 0),
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
    assert!(last.starts_with("REFUSED at 10/11 after-cut"), "{last}");
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

#[test]
fn a_run_stopped_at_any_step_resumes_there_and_repeats_nothing_done() {
    type Break = fn(&mut World);
    type Mend = fn(&mut World);
    let cases: [(Step, Break, Mend); 7] = [
        (
            Step::Verify,
            |w| {
                w.verify_passes = false;
                w.verify_code = 3;
            },
            |w| {
                w.verify_passes = true;
                w.verify_code = 0;
            },
        ),
        (
            Step::Stage,
            |w| w.stage_stages = false,
            |w| w.stage_stages = true,
        ),
        (
            Step::Stage,
            |w| w.stage_panics = true,
            |w| w.stage_panics = false,
        ),
        (
            Step::Stage,
            |w| w.stage_sync_fails = true,
            |w| w.stage_sync_fails = false,
        ),
        (
            Step::Cut,
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
            |w| w.after_cut = Err(Refusal::new("the site hook exited 4", "again later")),
            |w| w.after_cut = Ok("synced".into()),
        ),
        (
            Step::Check,
            |w| w.latest_lags = true,
            |w| w.latest_lags = false,
        ),
    ];
    for ((step, broken, mend), again) in cases
        .into_iter()
        .flat_map(|case| [(case, mac()), (case, bump())])
    {
        let mut w = World::new();
        broken(&mut w);
        let (code, last) = w.run(bump());
        let exit = if step == Step::Verify { 3 } else { 1 };
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
            ("reserve", Step::Build),
            ("build", Step::Build),
            ("verify", Step::Verify),
            ("stage", Step::Stage),
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
        "verify",
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

#[test]
fn what_would_refuse_the_release_refuses_it_before_anything_changes() {
    type Change = fn(&mut World);
    let cases: [(&str, Change, &str); 9] = [
        (
            "the clone not on main",
            |w| w.on_main = false,
            "1/11 checks",
        ),
        (
            "a machine refusal",
            |w| w.machine_refusals = vec![Refusal::new("only 4 GiB free on /c", "free space")],
            "1/11 checks",
        ),
        (
            "a machine gap",
            |w| w.audits = VecDeque::from([machine_gap()]),
            "1/11 checks",
        ),
        (
            "the release lease held",
            |w| w.cut_gate_refusals = vec![lease_held()],
            "1/11 checks",
        ),
        (
            "cut on origin",
            |w| {
                w.origin_tags.insert("v0.6.0".into(), NEXT.into());
            },
            "1/11 checks",
        ),
        (
            "a leftover local tag",
            |w| {
                w.local_tags.insert("v0.6.0".into());
            },
            "1/11 checks",
        ),
        (
            "the predecessor's claim not on main",
            |w| {
                w.origin_tags.insert("v0.5.0".into(), NEXT.into());
            },
            "1/11 checks",
        ),
        (
            "main declares a version no release has used: a bump would skip it",
            |w| {
                w.versions.insert(TIP.into(), "0.6.0".into());
            },
            "1/11 checks",
        ),
        (
            "a preflight block",
            |w| {
                w.blocks = vec![Refusal::new(
                    "handoff fixtures — the handoff fixtures of v0.5.0 are not checked in",
                    "generate them at tag v0.5.0",
                )];
            },
            "4/11 stage",
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
        if at == "1/11 checks" {
            assert_eq!(w.count("bump"), 0, "{case}: nothing pushed: {:?}", w.calls);
            assert!(w.journal().is_none(), "{case}: nothing journaled");
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
                && at("verify") < at("sleep 600")
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
        last.starts_with("REFUSED at 8/11 publish: COULD NOT RUN"),
        "{last}"
    );
    assert_eq!(
        w.count("verify"),
        1,
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
    assert_eq!(w.count("verify"), 0);
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
    assert_eq!(w.count("carry"), 0, "{:?}", w.calls);
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
        code == 1 && last.starts_with("REFUSED at 6/11 verify"),
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
        (w.count("bump"), w.count("carry"), w.count("verify")),
        (1, 1, 2),
        "{:?}",
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
        code == 3 && last.starts_with("REFUSED at 6/11 verify"),
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
            code == 1 && last.starts_with("REFUSED at 1/11 checks"),
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
/// cutter its run re-runs as keeps no line) refuses too; a tree git cannot read could not run,
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
        code == 1 && last.starts_with("REFUSED at 1/11 checks"),
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
            && last.starts_with("REFUSED at 1/11 checks: COULD NOT RUN — this clone lacks commit")
            && !last.contains("--forget"),
        "a tree git cannot read is no line to drop: {last}"
    );
    assert_eq!(w.count("carry"), 0, "{:?}", w.calls);
    assert_eq!(w.journal().unwrap().commit, BUMP);
}

/// A line whose meaning check fails ([`crate::line::verify`]) is never exported: the stage
/// refuses before `pub stage` (and before the private export of `--qualify`). NEGATIVE
/// CONTROL: drop `line_holds` before the remote stage, and `pub stage` runs.
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
            code == 1 && last.starts_with("REFUSED at 4/11 stage"),
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
            (w.count("stage"), w.count("qualify")),
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
        code == 1 && last.starts_with("REFUSED at 6/11 verify"),
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

/// A dry `--fix` plans the head step 3 would make: it carries, moves and journals nothing, and
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
    for call in [
        "carry",
        "stage",
        "reserve",
        "build",
        "verify",
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
        "would carry 111111111 onto the line's head ccccccccc",
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
        dry_last.contains("would refuse at 4/11 stage: the preflight BLOCKED"),
        "{dry_last}"
    );
    assert_eq!(w.journal().unwrap(), before);
    assert_eq!(w.count("carry"), 0, "{:?}", w.calls);
    let (code, last) = w.run(fix(&[FIX]));
    assert!(
        code == 1 && last.starts_with("REFUSED at 4/11 stage: the preflight BLOCKED"),
        "the real run refuses where the dry one said: {last}"
    );
}

/// The cut's gates of R's own tree (its handoff fixtures and policy) are asked at step 1 of the
/// head the release goes on with: a bare re-run of a head that fails them refuses there, naming
/// `--fix`, and that `--fix` is not refused by the head it replaces: step 3 carries it, and the
/// new head meets those gates. NEGATIVE CONTROL: ask them of the old head while its fixes are
/// still to carry, and `--fix` refuses at step 1 with the remedy that named it.
#[test]
fn a_fix_is_never_judged_by_the_head_it_replaces() {
    let mut w = red_line();
    w.verify_code = 1;
    w.verify_passes = false;
    let (code, last) = w.run(fix(&[FIX]));
    assert!(
        code == 1 && last.starts_with("REFUSED at 6/11 verify"),
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
            && last.starts_with("REFUSED at 1/11 checks: the cut would refuse")
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

/// A carry that cannot be made (a conflict, a record that cannot land) refuses at step 3 with
/// its own words: the journal keeps the line as it was, the clone is moved nowhere, and nothing
/// is staged or verified. NEGATIVE CONTROL: swallow the carry's refusal, and the red head is
/// staged and verified again.
#[test]
fn a_carry_that_cannot_be_made_refuses_at_step_three_and_keeps_the_line() {
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
            && last.starts_with("REFUSED at 3/11 commit")
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
    for call in ["commit-gates", "verify-line", "stage", "verify"] {
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
            code == 1 && last.starts_with("REFUSED at 1/11 checks"),
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
                && last.starts_with("REFUSED at 1/11 checks")
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
/// The remedy then goes through; with no line held it is the bare command. NEGATIVE CONTROL:
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
            && last.contains(&format!(
                "after that claim: {} --commit origin/main",
                crate::ledger::RELEASE_COMMAND
            ))
            && !last.contains("--forget"),
        "no line is held: {last}"
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
        last.starts_with("REFUSED at 1/11 checks") && last.contains("pid 777"),
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

/// A cutter that is not R's own hands its run claim to the re-run it starts, waits for it and
/// ends with its exit code; the re-run, a descendant of it, takes the claim over, keeps the
/// candidate, and refuses rather than re-run again. A claim handed by a live process that is
/// not this one's ancestor, or one a live ancestor did not hand, refuses as another run's.
/// NEGATIVE CONTROLS: take a claim over whatever its holder (the old pid test), and the re-run's
/// own process is never its holder, so it refuses at 1/11; take it over from any live
/// ancestor, handed or not, and the unhanded claim is adopted. The re-run names the handing run's
/// logs in a `handed` record (its first steps' seal calls are there); a run that adopted nothing
/// names none.
#[test]
fn a_cutter_that_is_not_rs_own_reexecs_once_and_keeps_the_candidate() {
    let handed_from = |w: &World| -> Vec<String> {
        w.out
            .iter()
            .filter_map(|l| l.strip_prefix("release-event: "))
            .filter_map(|json| aterm_json::from_str::<aterm_json::Value>(json).ok())
            .filter(|event| event["event"].as_str() == Some("handed"))
            .map(|event| event["from"].as_str().unwrap_or("?").to_owned())
            .collect()
    };
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.own_cutter = false;
    let tip = ReleaseOptions {
        commit: Some("origin/main".into()),
        ..mac()
    };
    let claim = w.release_dir().join(RUNNING_FILE);
    let (code, last) = w.run(tip.clone());
    assert_eq!(w.count("reexec"), 1, "{:?}", w.calls);
    assert_eq!(w.count("reexec with the claim handed"), 1, "{:?}", w.calls);
    assert!(
        code == 1 && last.starts_with("REFUSED at 3/11 commit"),
        "a re-run that could not start: {last}"
    );
    assert!(handed_from(&w).is_empty(), "{:#?}", w.out);
    assert!(
        !claim.exists(),
        "the claim no re-run took is this run's to remove"
    );

    w.reexec = Ok(5);
    w.calls.clear();
    let (code, last) = w.run(tip.clone());
    assert_eq!(
        (code, last.as_str()),
        (5, ""),
        "the re-run's exit, its own final line"
    );
    assert_eq!(w.count("reexec with the claim handed"), 1, "{:?}", w.calls);
    assert!(!claim.exists());

    // The re-run: its parent (or grandparent) holds the claim and handed it on.
    let earlier = (999, "Thu Oct  9 01:59:00 2026");
    let handed = |w: &mut World, handed: bool| {
        w.alive.insert(earlier.0, earlier.1.into());
        let mark = if handed { "handed\n" } else { "" };
        std::fs::write(
            w.release_dir().join(RUNNING_FILE),
            format!("pid {}\nstarted {}\nlogs /l\n{mark}", earlier.0, earlier.1),
        )
        .unwrap();
    };
    w.ancestors = vec![998, earlier.0, 1];
    handed(&mut w, true);
    w.calls.clear();
    let (code, last) = w.run(tip.clone());
    assert_eq!((code, w.count("reexec")), (1, 0), "no loop: {last}");
    assert!(last.starts_with("REFUSED at 3/11 commit"), "{last}");
    assert_eq!(handed_from(&w), ["/l"], "{:#?}", w.out);
    assert!(
        !claim.exists(),
        "the claim it took over is its own to remove"
    );

    for (ancestors, mark, what) in [
        (
            vec![998, 1],
            true,
            "handed by a live process that is no ancestor",
        ),
        (
            vec![998, earlier.0, 1],
            false,
            "a live ancestor's, not handed",
        ),
    ] {
        w.ancestors = ancestors;
        handed(&mut w, mark);
        w.calls.clear();
        let (code, last) = w.run(tip.clone());
        assert!(
            code == 1 && last.starts_with("REFUSED at 1/11 checks") && last.contains("pid 999"),
            "{what}: {last}"
        );
        assert!(w.calls.is_empty(), "{what}: {:?}", w.calls);
        assert!(claim.exists(), "{what}: another run's claim is kept");
    }

    w.ancestors = vec![998, earlier.0, 1];
    handed(&mut w, true);
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.own_cutter = true;
    let (code, last) = w.run(tip);
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with(&format!(
            "RELEASED aterm 0.6.0 (build 1791600000) from {}",
            &TIP[..9]
        )),
        "{last}"
    );
}

/// A signal this run catches while it waits on the re-run as R's own cutter stops the release
/// at 3/11 when the claim still names this run as the re-run ends (no re-run took it over).
/// When the claim is the re-run's then, or gone with the re-run's end, the release was the
/// re-run's to stop and to say: this run ends with its exit code, no final line of its own, and
/// the re-run's claim left in place. The fake fires the signal as the re-run starts, before it
/// rewrites the claim. NEGATIVE CONTROLS: drop the interrupt check after the re-run, and the
/// first case ends with the re-run's code and no line; make `RunClaim`'s drop remove the claim
/// whatever it names, and the re-run's claim is gone.
#[test]
fn a_signal_during_the_rerun_is_this_runs_until_the_rerun_takes_the_claim() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.own_cutter = false;
    w.reexec = Ok(5);
    w.signal_at = Some("reexec");
    let tip = ReleaseOptions {
        commit: Some("origin/main".into()),
        ..mac()
    };
    let claim = w.release_dir().join(RUNNING_FILE);
    let (code, last) = w.run(tip.clone());
    assert_eq!(w.count("reexec with the claim handed"), 1, "{:?}", w.calls);
    assert!(
        code == 130 && last.starts_with("INTERRUPTED at 3/11 commit (by SIGTERM)"),
        "the claim still this run's as the re-run ends: {code} {last}"
    );
    assert!(
        !claim.exists(),
        "the claim no re-run took is this run's to remove"
    );

    w.signal = None;
    w.reexec_takes_claim = true;
    w.calls.clear();
    let (code, last) = w.run(tip.clone());
    assert_eq!(w.count("reexec with the claim handed"), 1, "{:?}", w.calls);
    assert_eq!(
        (code, last.as_str()),
        (5, ""),
        "the claim the re-run's as it ends: the re-run answers for the signal"
    );
    let holder = std::fs::read_to_string(&claim).unwrap_or_default();
    assert!(
        holder.starts_with(&format!("pid {}\n", RERUN.0)),
        "the claim the re-run took is the re-run's: {holder:?}"
    );

    // The re-run took the claim, said its own INTERRUPTED line, and its end removed the claim
    // (the claim the last case's re-run left, its end not modelled, goes first).
    std::fs::remove_file(&claim).unwrap();
    w.signal = None;
    w.reexec = Ok(130);
    w.rerun_removes_claim = true;
    w.calls.clear();
    let (code, last) = w.run(tip);
    assert_eq!(
        (code, last.as_str()),
        (130, ""),
        "one final line: the re-run's"
    );
    assert!(!claim.exists());
}

/// A re-run as R's own cutter takes over the claim the run that started it handed it before it
/// can say a final line: one that caught a signal before its checks says INTERRUPTED at 1/11 with
/// the claim naming it, and its end removes the claim, so the run that started it, finding the
/// claim no longer its own when the re-run ends, says nothing more
/// (a_signal_during_the_rerun_is_this_runs_until_the_rerun_takes_the_claim). NEGATIVE CONTROL:
/// take the claim at the checks, after begin(Checks)'s interrupt check (as before), and the
/// re-run says its line while the claim still names the run that handed it, which would say
/// INTERRUPTED at 3/11 as well.
#[test]
fn a_rerun_takes_the_handed_claim_before_it_can_say_a_final_line() {
    let mut w = World::new();
    let earlier = (999, "Thu Oct  9 01:59:00 2026");
    w.alive.insert(earlier.0, earlier.1.into());
    w.ancestors = vec![998, earlier.0, 1];
    let claim = w.release_dir().join(RUNNING_FILE);
    std::fs::create_dir_all(w.release_dir()).unwrap();
    std::fs::write(
        &claim,
        format!(
            "pid {}\nstarted {}\nlogs /l\nhanded\n",
            earlier.0, earlier.1
        ),
    )
    .unwrap();
    w.signal = Some(2);
    let (code, last) = w.run(mac());
    assert!(
        code == 130 && last.starts_with("INTERRUPTED at 1/11 checks (by SIGINT)"),
        "{code} {last}"
    );
    let said_with = w.claim_at_interrupted.clone().unwrap_or_default();
    assert!(
        said_with.starts_with(&format!("pid {}\n", w.pid)),
        "the re-run's line was said with the claim its own: {said_with:?}"
    );
    assert!(!claim.exists(), "and its end removed the claim");
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
        last.starts_with("REFUSED at 4/11 stage: COULD NOT RUN"),
        "{last}"
    );
    assert_eq!(w.count("verify"), 0, "{:?}", w.calls);
}

#[test]
fn a_publish_that_recorded_source_but_failed_signing_cannot_be_skipped_into_a_cut() {
    let mut w = World::new();
    w.publish_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 8/11 publish"), "{last}");
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
    for never in ["build", "verify", "stage", "publish"] {
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
    assert!(last.starts_with("REFUSED at 9/11 cut"), "{last}");
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
            last.starts_with("REFUSED at 8/11 publish"),
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
    let cases: [(Setup, &str, &str); 4] = [
        (
            |w| w.signal_at = Some("verify"),
            "INTERRUPTED at 6/11 verify (by SIGTERM)",
            "publish",
        ),
        (
            |w| w.signal_at = Some("cut"),
            "INTERRUPTED at 9/11 cut (by SIGTERM)",
            "after-cut 0.6.0",
        ),
        (
            |w| {
                w.audits = VecDeque::from([passes(), owner_gap(), passes()]);
                w.after_sleep = Some(|w| w.signal = Some(15));
            },
            "INTERRUPTED at 8/11 publish (by SIGTERM)",
            "publish",
        ),
        (
            |w| {
                w.own_cutter = false;
                w.signal_at = Some("move");
            },
            "INTERRUPTED at 3/11 commit (by SIGTERM)",
            "reexec",
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
        last.starts_with("INTERRUPTED at 6/11 verify (by SIGTERM)"),
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
        last.starts_with("REFUSED at 1/11 checks") && last.contains("pid 802"),
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
fn an_ask_no_journal_took_is_withdrawn_when_the_run_stops_before_step_3() {
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap()]);
    w.cut_gate_refusals = vec![lease_held()];
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 1/11 checks"), "{last}");
    assert!(w.journal().is_none(), "nothing journaled");
    assert_eq!(w.asks, ["release.notary"]);
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
        (w.count("reserve"), w.count("build"), w.count("verify")),
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
        assert!(last.starts_with("REFUSED at 10/11 after-cut"), "{last}");
        assert!(!w.journal().unwrap().finished());
        assert!(!w.journal().unwrap().has("after-cut"));
        w.site = Ok(Outcome::Synced { deployed: true });
        w.calls.clear();
        assert_eq!(w.run(mac()).0, 0);
        assert_eq!(w.count("after-cut 0.6.0"), 1);
        for work in ["reserve", "build", "verify", "publish", "cut"] {
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
    assert!(last.starts_with("REFUSED at 10/11 after-cut"), "{last}");
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
    for work in ["reserve", "build", "verify", "publish", "cut"] {
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
    assert!(last.starts_with("REFUSED at 10/11 after-cut"), "{last}");
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
    assert_eq!(w.count("qualify"), 1);
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
    for work in ["qualify", "reserve", "build", "verify"] {
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
    assert_eq!(w.count("qualify"), 1, "recheck the full local export");
    assert_eq!(w.count("build"), 0, "keep the exact verified bytes");
    assert_eq!(w.count("verify"), 0, "reuse only a matching verification");

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
    assert_eq!(w.count("verify"), 0);
}

#[test]
fn private_qualification_refuses_failed_exports_and_a_dry_run_builds_nothing() {
    for dry in [false, true] {
        let mut w = World::new();
        w.versions.insert(TIP.into(), "0.6.0".into());
        w.stage_stages = false;
        let (code, last) = w.run(ReleaseOptions {
            qualify: true,
            dry_run: dry,
            commit: Some(TIP.into()),
            ..mac()
        });
        if dry {
            assert_eq!(code, 0, "{last}\n{:#?}", w.out);
            assert!(last.starts_with("DRY RUN"), "{last}");
            assert!(w.journal().is_none());
            assert_eq!(w.count("qualify"), 0);
        } else {
            assert_eq!(code, 1, "{last}\n{:#?}", w.out);
            assert!(
                last.contains("private export qualification failed"),
                "{last}"
            );
            assert!(w.journal().unwrap().artifact.is_none());
            assert_eq!(w.count("qualify"), 1);
        }
        for never in ["reserve", "build", "verify", "stage", "publish", "cut"] {
            assert_eq!(w.count(never), 0, "{dry}: {never}: {:?}", w.calls);
        }
    }
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
            for never in ["stage", "qualify", "build", "verify", "publish", "cut"] {
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
    assert_eq!(w.count("qualify"), 1);
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
    for heavy in ["build", "verify", "stage"] {
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
        assert!(last.starts_with("REFUSED at 1/11 checks"), "{case}: {last}");
        for never in ["sync", "build", "verify", "stage", "publish"] {
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
    for never in ["bump", "commit-gates", "reserve", "build", "verify"] {
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
    assert!(last.starts_with("REFUSED at 9/11 cut"), "{last}");
    assert!(
        w.published.contains_key(BUMP) && w.cut.is_none(),
        "public, nothing claimed"
    );

    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("REFUSED at 4/11 stage"), "{last}");
    for never in ["build", "verify", "stage", "publish"] {
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
        ["reserve", "build", "verify", "stage", "publish"].map(|c| w.count(c)),
        [1, 1, 1, 0, 0],
        "{:?}",
        w.calls
    );

    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    for never in ["build", "verify", "stage", "publish"] {
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
                && staged[0] < at("build")
                && at("verify") < staged[1]
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
    let again = opts.anchored(&start).rerun_args(args);
    let want: Vec<std::ffi::OsString> = vec![
        "release".into(),
        "--engine".into(),
        root.path()
            .join("publication")
            .canonicalize()
            .unwrap()
            .into_os_string(),
        "--release-credentials".into(),
        start.join("creds.toml").into_os_string(),
        "--linux-artifacts".into(),
        "/abs/linux".into(),
    ];
    assert_eq!(again, want);
}

/// The re-run as R's own cutter is handed no `--fix`: it resumes the head the journal names,
/// whose hops this run made, and every other flag is passed on. NEGATIVE CONTROL: keep `--fix`
/// and its commits in `rerun_args`, and they reach the re-run.
#[test]
fn the_rerun_is_handed_no_fix_and_resumes_the_journaled_head() {
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
    let again = mac().rerun_args(args.iter().map(Into::into).collect());
    let want: Vec<std::ffi::OsString> = ["release", "--mac-only", "--verbose"]
        .iter()
        .map(Into::into)
        .collect();
    assert_eq!(again, want);
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
            last.starts_with("REFUSED at 6/11 verify"),
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
    assert!(last.starts_with("REFUSED at 6/11 verify"), "{last}");
    assert!(last.contains("FAILED test"), "{last}");
    assert_eq!(w.count("verify"), 1);

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
    assert!(last.starts_with("REFUSED at 4/11 stage"), "{last}");
    assert!(
        last.contains("FAILED test") && last.contains("/l/06-verify.log"),
        "names the check and its log: {last}"
    );
    assert!(last.contains("a new hop changes the tree"), "{last}");
    assert!(
        last.contains("--fix") && !last.contains("--commit origin/main"),
        "the FAIL's remedy carries a landed fix onto the line: {last}"
    );
    for step in ["verify", "build", "stage", "publish"] {
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
/// a step 3 that carries the hop but leaves the run's candidate at the old head refuses with the
/// base's FAIL.
#[test]
fn a_fail_refuses_its_own_tree_and_a_fix_hop_is_verified_anew() {
    let mut w = World::new();
    w.verify_outcomes.insert("test", "FAIL");
    w.verify_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 6/11 verify"), "{last}");

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
    assert_eq!(w.count("verify"), 1, "the hop's tree is verified once");
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
/// the resume runs exactly the missing checks, carrying exactly the test binaries the test
/// record lists as passed; once they pass the release goes on. Negative control: a resume that
/// asks every check again fails the asserted set.
#[test]
fn a_resume_runs_only_the_missing_checks_and_carries_the_binaries_that_passed() {
    let mut w = World::new();
    w.verify_outcomes.insert("test", "NOT RUN");
    w.verify_outcomes.insert("appkit", "NOT RUN");
    w.verify_code = 3;
    let carried = vec![("-p aterm-db --lib".to_owned(), "aa".repeat(32))];
    w.resume.clone_from(&carried);
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert!(last.starts_with("REFUSED at 6/11 verify"), "{last}");
    let r = w.journal().unwrap().commit;
    assert_eq!(
        w.checks_of(&r, w.macho().as_deref()).missing,
        ["test", "appkit"]
    );
    assert_eq!(w.verify_runs.len(), 1);
    assert!(
        !w.verify_runs[0].strict_subset(),
        "the first run asks every check"
    );

    w.verify_outcomes.clear();
    w.verify_code = 0;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("verify"), 1);
    let resumed = w.verify_runs.last().unwrap();
    assert_eq!(resumed.checks, ["test", "appkit"]);
    assert_eq!(resumed.passed_binaries, carried);
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
        assert!(last.starts_with("REFUSED at 6/11 verify"), "{last}");
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

/// STEP 6 AS AN AGENT RUNS IT: the real backend runs the release's own `targo` —
/// `<prover>/targo --unverified run -q -p aterm-verify -- release --events <file> --app <app>`
/// — in the clone (a resume names its missing `--checks` and its `--passed-binaries`, and
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
    std::fs::write(
        bin.join("targo"),
        format!(
            "#!/bin/sh\necho \"$PWD $*\" > '{said}'\nout=; prev=\nfor a in \"$@\"; do \
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
    std::fs::set_permissions(bin.join("targo"), std::fs::Permissions::from_mode(0o755)).unwrap();
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
    let every = crate::check_record::Checks::no("none").run();
    let repo_seen = std::fs::canonicalize(&repo).unwrap();

    for how in ["dirty", "move"] {
        set("0", how, &finished(&all_ok));
        let refused = Backend::verify(&mut real, &cand, &prover, &artifact, &every)
            .expect_err("the clone moved");
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
    let refused =
        Backend::verify(&mut real, &cand, &prover, &artifact, &every).expect_err("a dirty clone");
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
    let refused =
        Backend::verify(&mut real, &cand, &prover, &artifact, &every).expect_err("inputs changed");
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
        checks: vec!["lint".into(), "test".into()],
        passed_binaries: vec![("-p a --lib".into(), "aa".repeat(32))],
    };
    let binary = |spec: &str, sha: &str, outcome: &str| {
        format!(
            "verify-event: {}\n",
            aterm_json::json!({"schema":1,"event":"test-binary-finished","test":spec,
                "outcome":outcome,"exe_sha256":sha,"carried":false})
        )
    };
    set(
        "3",
        "",
        &format!(
            "{}{}{}",
            finished(&[("lint", "ok")]),
            binary("-p a --lib", &"aa".repeat(32), "ok"),
            binary("-p b --lib", &"bb".repeat(32), "NOT RUN")
        ),
    );
    let ran = Backend::verify(&mut real, &cand, &prover, &artifact, &resume).expect("it ran");
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
    let (passed, events_file) = (after("--passed-binaries"), after("--events"));
    assert_eq!(
        said_now.trim(),
        format!(
            "{} --unverified run -q -p aterm-verify -- release --checks lint,test \
             --passed-binaries {passed} --events {events_file}",
            repo_seen.display(),
        ),
        "no measure, so no --app"
    );
    assert_eq!(
        std::fs::read_to_string(&passed).unwrap(),
        format!("{} -p a --lib\n", "aa".repeat(32))
    );
    let judged = judge(&r);
    assert!(
        !judged.missing.iter().any(|m| m == "lint"),
        "{}",
        judged.line()
    );
    assert_eq!(
        judged.run().passed_binaries,
        [("-p a --lib".to_owned(), "aa".repeat(32))],
        "the test record of the killed run lists what passed"
    );

    set("0", "", &finished(&all_ok));
    let ran = Backend::verify(&mut real, &cand, &prover, &artifact, &every).expect("it ran");
    assert_eq!(ran.code, 0);
    assert_eq!(
        std::fs::read_to_string(&said).unwrap().trim(),
        format!(
            "{} --unverified run -q -p aterm-verify -- release --events {events_file} --app {}",
            repo_seen.display(),
            artifact.app.display()
        ),
        "a run of every check names none"
    );
    assert!(
        judge(&r).complete(),
        "every check recorded ok for R's tree on these bytes"
    );
    assert!(!judge(&other).complete(), "another tree has no records");

    set("1", "", &finished(&[("fmt", "ok"), ("lint", "FAIL")]));
    let ran = Backend::verify(&mut real, &cand, &prover, &artifact, &every).expect("it ran");
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
    Backend::verify(&mut real, &cand, &prover, &artifact, &every).expect("it ran");
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
            "REFUSED at 6/11 verify: COULD NOT RUN",
        ),
        (
            "claimed here, no cut journal",
            |w| w.claim_lands_unjournaled = true,
            1,
            "REFUSED at 9/11 cut",
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
        assert_eq!(w.cut.as_ref().unwrap().build, fresh.build, "{case}");
    }
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
    for expensive in ["stage", "build", "verify", "publish"] {
        assert_eq!(world.count(expensive), 0, "{expensive}: {:?}", world.calls);
    }
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
    let build = events
        .iter()
        .find(|event| {
            event["stage"].as_str() == Some("build") && event["outcome"].as_str() == Some("ok")
        })
        .expect("build completion");
    assert_eq!(build["source"].as_str(), Some(BUMP));
    assert!(build["log"].as_str().is_some());
    assert!(build["waiting_s"].as_u64().is_some());
    assert!(build["execution_s"].as_u64().is_some());
}

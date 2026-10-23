// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship release`'s step machine ([`crate::release`]) driven against a fake world.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::provision::{AskWords, Audited, Finding};
use crate::release::{
    AfterCut, Answer, Artifact, Backend, Built, Candidate, CutState, EngineNow, EngineRows, Facts,
    JOURNAL_FILE, Journal, Prover, RUNNING_CHILD_FILE, RUNNING_FILE, Ran, Refusal, ReleaseOptions,
    Reservation, Step, run_with,
};
use crate::release_steps::Real;

const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TIP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BUMP: &str = "cccccccccccccccccccccccccccccccccccccccc";
const NEXT: &str = "dddddddddddddddddddddddddddddddddddddddd";
const BUMP2: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

struct World {
    dir: aterm_tempfile::TempDir,
    now: u64,
    out: Vec<String>,
    calls: Vec<String>,
    pid: u32,
    started: String,
    alive: BTreeMap<u32, String>,
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
    /// The executable digests a verified record names.
    verified: BTreeSet<String>,
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
    /// The preflight's own blocks: R's commit-only gates and the engine's readiness.
    blocks: Vec<Refusal>,
    audits: VecDeque<Vec<Finding>>,
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
    /// The cut's read-only pre-claim gates' refusals.
    cut_gate_refusals: Vec<Refusal>,
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
    peer_staged: bool,
}

impl World {
    fn new() -> Self {
        let mut versions = BTreeMap::new();
        versions.insert(OLD.into(), "0.5.0".into());
        versions.insert(TIP.into(), "0.5.0".into());
        let mut origin_tags = BTreeMap::new();
        origin_tags.insert("v0.5.0".into(), OLD.into());
        Self {
            dir: aterm_tempfile::tempdir().unwrap(),
            now: 1_000_000,
            out: Vec::new(),
            calls: Vec::new(),
            pid: 4242,
            started: "Thu Oct  9 02:00:00 2026".into(),
            alive: BTreeMap::new(),
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
            verified: BTreeSet::new(),
            tail: 1_791_500_000,
            artifacts: BTreeMap::new(),
            overtaken_at_cut: false,
            claim_lands_unjournaled: false,
            machine_refusals: Vec::new(),
            blocks: Vec::new(),
            audits: VecDeque::new(),
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
            cut_gate_refusals: Vec::new(),
            judge_down: false,
            stage_sync_fails: false,
            signal: None,
            signal_at: None,
            after_verify: None,
            after_sleep: None,
            local_tags: BTreeSet::new(),
            real: None,
            fixtures_staged: None,
            fixtures_on_main: false,
            generator_refuses: false,
            landing_fails: false,
            prover: prover(),
            engine: "e7e7e7e7e7".into(),
            staged_by: None,
            running_at_prepared: None,
            child_note_at_stage: false,
            ls_remote_down: false,
            peer_staged: false,
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
    fn record_time(&mut self, version: &str, step: Step, secs: u64) {
        self.times.push_str(&format!(
            "{}\t{version}\t{}\t{secs}\n",
            self.now,
            step.name()
        ));
    }
    fn prover(&mut self, _journaled: Option<&str>) -> Result<Prover, String> {
        Ok(self.prover.clone())
    }
    fn lease(&mut self, _prover: &Prover) {
        self.calls.push("lease".into());
    }
    fn cleared_env(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn machine(&mut self, _creds: &Path, _explicit: bool) -> (String, Vec<Refusal>) {
        ("m3".into(), self.machine_refusals.clone())
    }
    fn fetch(&mut self) -> Result<String, Refusal> {
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
    fn staging_moved(&mut self) -> Result<Option<String>, String> {
        self.calls.push("staging-now".into());
        let why = "the staging tier's main moved since R was attested (555555555..666666666: a \
                   peer's pub stage), and pub promote refuses that attestation";
        Ok(self.peer_staged.then(|| why.to_string()))
    }
    fn cut_journal(&mut self) -> Option<CutState> {
        self.cut.clone()
    }
    fn is_dir(&mut self, _path: &Path) -> bool {
        true
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
    fn reexec(&mut self, _prover: &Prover) -> Refusal {
        self.calls.push("reexec".into());
        Refusal::new("exec failed in the fake", "nothing")
    }
    fn facts(&mut self, c: &Candidate, _prover: &Prover, artifact: Option<&Artifact>) -> Facts {
        let down = self.judge_down;
        let yes = |set: &BTreeSet<String>, what: &str, key: &str| {
            if down {
                Answer::unanswered(format!("the {what} judge did not answer: store unreadable"))
            } else if set.contains(key) {
                Answer::yes(format!("{what} {}", &key[..4]))
            } else {
                Answer::no(format!("no {what} receipt"))
            }
        };
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
        let verified = match artifact.filter(|_| built.yes) {
            None => Answer::no("its bundle is not built yet (step 5)"),
            Some(a) => yes(&self.verified, "PASSED", &a.macho_sha256),
        };
        let attested =
            self.attested.as_deref() == Some(c.commit.as_str()) && self.staged_by.is_some();
        Facts {
            cut: self.cut.clone(),
            engine: self.engine_rows(Some(&c.commit)),
            attested,
            staged_by: self.staged_by.clone().filter(|_| attested),
            built,
            verified,
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
    fn cut_gates(&mut self, _c: &Candidate, _creds: &Path) -> Vec<Refusal> {
        self.calls.push("cut-gates".into());
        self.cut_gate_refusals.clone()
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
    fn audit(&mut self, _id: &str) -> Vec<Finding> {
        self.calls.push("audit".into());
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
    fn verify(&mut self, _c: &Candidate, _p: &Prover, artifact: &Artifact) -> Result<Ran, Refusal> {
        self.calls.push("verify".into());
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
        if self.verify_passes {
            self.verified.insert(artifact.macho_sha256.clone());
        }
        if let Some(then) = self.after_verify {
            then(self);
        }
        Ok(ran(self.verify_code))
    }
    fn stage(&mut self, c: &Candidate, _p: &Prover) -> Result<Ran, Refusal> {
        self.calls.push("stage".into());
        assert!(!self.stage_panics, "a bug in the release");
        if self.stage_stages {
            self.attested = Some(c.commit.clone());
            self.staged_by = Some(self.engine.clone());
            self.peer_staged = false;
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
            ..site
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
        .filter(|c| !matches!(*c, "audit" | "lease" | "cut-gates") && !c.starts_with("ls-remote"))
        .collect();
    assert_eq!(
        order,
        [
            "sync",
            "bump",
            "move cccc",
            "commit-gates",
            "stage",
            "reserve",
            "build",
            "stage-fixtures",
            "verify",
            "sync",
            "engine-now",
            "staging-now",
            "publish",
            "source-publication",
            "sync",
            "cut --mac-only",
            "after-cut 0.6.0",
            "land-fixtures",
        ]
    );
    assert_eq!(w.count("audit"), 2, "{:?}", w.calls);
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
    assert!(w.macho().is_some_and(|m| w.verified.contains(&m)), "READY");
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.audits = VecDeque::from([passes()]);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert_eq!(w.journal().unwrap().commit, BUMP, "never chases main");
    assert_eq!(w.count("verify"), 0);
}

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

#[test]
fn a_cutter_that_is_not_rs_own_reexecs_once_and_keeps_the_candidate() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.own_cutter = false;
    let tip = ReleaseOptions {
        commit: Some("origin/main".into()),
        ..mac()
    };
    let (code, last) = w.run(tip.clone());
    assert_eq!(w.count("reexec"), 1, "{:?}", w.calls);
    assert_eq!(code, 1, "the fake's exec fails: {last}");
    let adopt = |w: &World| {
        std::fs::write(
            w.release_dir().join(RUNNING_FILE),
            format!("pid {}\nstarted {}\nlogs /l\n", w.pid, w.started),
        )
        .unwrap();
    };
    adopt(&w);
    w.calls.clear();
    let (code, last) = w.run(tip.clone());
    assert_eq!((code, w.count("reexec")), (1, 0), "no loop: {last}");
    assert!(last.starts_with("REFUSED at 3/11 commit"), "{last}");

    adopt(&w);
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
    w.verify_passes = false;
    w.verify_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
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
fn asks_are_carried_over_a_replaced_candidate_and_withdrawn_by_forget() {
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap(), owner_gap(), machine_gap()]);
    let _ = w.run(bump());
    assert_eq!(w.journal().unwrap().asks, ["release.notary"]);
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.audits = VecDeque::from([owner_gap(), machine_gap()]);
    let (code, _) = w.run(ReleaseOptions {
        commit: Some(NEXT.into()),
        ..mac()
    });
    assert_eq!(code, 1);
    let j = w.journal().unwrap();
    assert_eq!(j.commit, NEXT);
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
fn a_site_this_machine_cannot_sync_is_a_recorded_hand_step_and_the_release_ends() {
    let mut w = World::new();
    w.site = Ok(crate::site::Outcome::NoCheckout(
        "site checkout not found — set SITE_DIR or skip with --skip-site (looked in /s)".into(),
    ));
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with("RELEASED aterm 0.6.0 (build 1791600000)")
            && last.contains(crate::release::SITE_BY_HAND),
        "{last}"
    );
    let j = w.journal().unwrap();
    assert!(
        j.finished() && j.has("after-cut") && j.has("site-deferred"),
        "{j:?}"
    );
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert!(last.contains(crate::release::SITE_BY_HAND), "{last}");
    assert_eq!(w.count("after-cut 0.6.0"), 0, "{:?}", w.calls);
    let undeployed =
        crate::release_steps::site_outcome(&Ok(crate::site::Outcome::NoDeploy), Vec::new())
            .unwrap();
    assert!(
        undeployed.note.contains("NOT deployed") && !undeployed.note.contains("already"),
        "{undeployed:?}"
    );
    assert!(!undeployed.site_deferred);
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
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
    assert!(matches!(
        parse(&["release", "--forget", "--verbose", "--engine", "/e"]),
        Ok(crate::cli::Cmd::Release(o)) if o.forget && o.verbose
    ));
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
            "the staging tier moved",
            |w| w.peer_staged = true,
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
    let again = opts.anchored(&start).anchored_args(args);
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

/// STEP 6 AS AN AGENT RUNS IT: the real backend runs the release's own `targo` —
/// `<prover>/targo --unverified run -q -p aterm-verify -- release --app <app>` — in the
/// clone, and only its exit 0 from a clone at R's tree and clean, before and after,
/// writes the verified record of R's tree on the bundle's executable.
#[cfg(unix)]
#[test]
fn the_real_verify_step_runs_the_verify_cli_in_the_clone_and_records_only_a_pass() {
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
    std::fs::write(
        bin.join("targo"),
        format!(
            "#!/bin/sh\necho \"$PWD $*\" > '{said}'\ncase \"$(cat '{mode}')\" in\n  \
             dirty) echo stray > stray.txt ;;\n  move) git checkout -q --detach {other} ;;\nesac\n\
             exit \"$(cat '{code}')\"\n",
            said = said.display(),
            mode = mode.display(),
            code = dir.path().join("code").display(),
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
    };
    let mut real = Real::new(&repo, None, true, false).expect("a backend");
    let record = |r: &str| crate::gates::verified(&git, r, &"ab".repeat(32)).unwrap().0;
    let set = |code: &str, how: &str| {
        std::fs::write(dir.path().join("code"), code).unwrap();
        std::fs::write(&mode, how).unwrap();
    };

    set("1", "");
    let ran = Backend::verify(&mut real, &cand, &prover, &artifact).expect("it ran");
    assert_eq!(ran.code, 1);
    assert!(!record(&r), "a failure records nothing");
    let repo_seen = std::fs::canonicalize(&repo).unwrap();
    assert_eq!(
        std::fs::read_to_string(&said).unwrap().trim(),
        format!(
            "{} --unverified run -q -p aterm-verify -- release --app {}",
            repo_seen.display(),
            artifact.app.display()
        )
    );

    for how in ["dirty", "move"] {
        set("0", how);
        let refused =
            Backend::verify(&mut real, &cand, &prover, &artifact).expect_err("the clone moved");
        assert!(refused.could_not_run, "{refused:?}");
        assert!(!record(&r), "{how}: nothing is recorded");
        let _ = std::fs::remove_file(repo.join("stray.txt"));
        assert!(
            git.git(&["checkout", "-q", "--detach", &r])
                .unwrap()
                .success()
        );
    }

    std::fs::write(repo.join("stray.txt"), "before").unwrap();
    std::fs::remove_file(&said).unwrap();
    set("0", "");
    let refused = Backend::verify(&mut real, &cand, &prover, &artifact).expect_err("a dirty clone");
    assert!(refused.could_not_run, "{refused:?}");
    assert!(!said.exists(), "nothing ran");
    assert!(!record(&r));
    std::fs::remove_file(repo.join("stray.txt")).unwrap();

    let ran = Backend::verify(&mut real, &cand, &prover, &artifact).expect("it ran");
    assert_eq!(ran.code, 0);
    assert!(record(&r), "a pass is recorded for R's tree on these bytes");
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
        assert_eq!(w.cut.as_ref().unwrap().build, fresh.build, "{case}");
    }
}

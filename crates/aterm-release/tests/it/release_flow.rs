// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship release`'s step machine ([`crate::release`]) driven against a fake world: a
//! scratch release directory (the journal and the run claim are real files), and a
//! [`Backend`] whose git, engine, records, children, clock and owner's aterm are in
//! memory. What the machine owns is what these tests hold it to: the order, the
//! skip-by-facts rule, the resume after a stop at each step, the final line's shape,
//! the dry run, the owner ask while the long steps run, the READY candidate kept, one
//! run per clone and the re-exec guard, and the prepare with its visible half's re-asks.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::provision::{AskWords, Audited, Finding};
use crate::release::{
    AfterCut, Answer, Artifact, Backend, Beside, Besides, Built, Candidate, CutState, EngineNow,
    EngineRows, Facts, JOURNAL_FILE, Journal, Prover, RUNNING_CHILD_FILE, RUNNING_FILE, Ran,
    Refusal, ReleaseOptions, Reservation, Step, run_with,
};
use crate::release_steps::Real;

const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TIP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BUMP: &str = "cccccccccccccccccccccccccccccccccccccccc";
const NEXT: &str = "dddddddddddddddddddddddddddddddddddddddd";
/// The bump after BUMP's release.
const BUMP2: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// The fake world. History is one line: `history[i]` is an ancestor of every later one.
struct World {
    dir: aterm_tempfile::TempDir,
    now: u64,
    out: Vec<String>,
    calls: Vec<String>,
    pid: u32,
    started: String,
    alive: BTreeMap<u32, String>,
    times: String,
    // git
    history: Vec<String>,
    versions: BTreeMap<String, String>,
    head: String,
    on_main: bool,
    origin_tags: BTreeMap<String, String>,
    ledger: Vec<String>,
    // the engine, the cut, the receipts
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
    /// The build fails.
    build_fails: bool,
    /// The L0 censuses find something.
    census_finds: bool,
    /// A peer's release claims past the reservation as the cut starts — after the
    /// release's last look, before the cut's own claim.
    overtaken_at_cut: bool,
    /// The cut's claim LANDS (main takes the release commit, the ledger its line) and
    /// the cut then fails before it journals anything.
    claim_lands_unjournaled: bool,
    // behaviours
    machine_refusals: Vec<Refusal>,
    /// The preflight's own blocks: R's commit-only gates and the engine's readiness.
    blocks: Vec<Refusal>,
    audits: VecDeque<Vec<Finding>>,
    verify_code: i32,
    verify_passes: bool,
    /// The clone leaves R's tree while `verify release` runs: the backend records nothing.
    verify_moves: bool,
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
    /// What the world does at each engine sync, in turn (`None`: nothing).
    after_sync: VecDeque<Option<fn(&mut World)>>,
}

impl World {
    /// origin/main at TIP declaring 0.5.0, which is released (its tag on origin is
    /// OLD's claim); everything provisioned; every child succeeds.
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
            build_fails: false,
            census_finds: false,
            overtaken_at_cut: false,
            claim_lands_unjournaled: false,
            machine_refusals: Vec::new(),
            blocks: Vec::new(),
            audits: VecDeque::new(),
            verify_code: 0,
            verify_passes: true,
            verify_moves: false,
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
            after_sync: VecDeque::new(),
        }
    }

    /// The handoff fixture guard's child.
    fn guard(&mut self) -> Result<String, Refusal> {
        self.calls.push("guard".into());
        Ok("the handoff fixture guard passes (3 test(s))".into())
    }

    /// The L0 censuses' child.
    fn census(&mut self) -> Result<String, Refusal> {
        self.calls.push("census".into());
        if self.census_finds {
            return Err(Refusal::new(
                "L0 CENSUS FINDING in /clone — its diagnostics name it",
                "fix it on main",
            ));
        }
        Ok("the L0 censuses pass".into())
    }

    /// A signal arrives while `step`'s child runs: the child ends, the release sees it.
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

    fn said(&self, needle: &str) -> bool {
        self.out.iter().any(|l| l.contains(needle))
    }

    fn step_line(&self, step: Step) -> String {
        let head = format!(
            "[{:>2}/{}] {:<15}",
            step.number(),
            Step::ALL.len(),
            step.name()
        );
        self.out
            .iter()
            .rfind(|l| l.starts_with(&head) && !l.contains(" start "))
            .cloned()
            .unwrap_or_default()
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

    /// The executable digest of the bundle the journal names, if one is built.
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
        // a finished cut published its version's tag, on the release commit
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
        if let Some(Some(then)) = self.after_sync.pop_front() {
            then(self);
        }
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
        // a signal as the move's child ends: the move itself is done
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
        // promotable only naming the engine that staged it, as the real reader holds it
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
    fn stage_beside(&mut self, c: &Candidate, p: &Prover, beside: Beside) -> Besides {
        Besides {
            stage: beside.stage.then(|| self.stage(c, p)),
            guard: beside.guard.then(|| self.guard()),
            census: beside.census.then(|| self.census()),
        }
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
        let artifact = if self.build_fails {
            Err(Refusal::new(
                "the build did not finish (ship build exited 1)",
                "read its log",
            ))
        } else {
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
        };
        Built {
            artifact,
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
        if self.verify_moves {
            return Err(Refusal::new(
                "the clone moved during `verify release`, so its pass is not ccccccccc's — \
                 nothing was recorded",
                "put the clone back at ccccccccc and clean",
            )
            .could_not_run());
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
                // a step's child that ended, its note left behind
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
        // The cut claims the journal's reservation, never another number: one the
        // tail has reached refuses before the claim, nothing burned.
        let Some(reserved) = self.journal().and_then(|j| j.reserved) else {
            // a resume finishes the claim its own journal holds, the bytes it names:
            // it reads no release journal
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
            self.calls
                .push(format!("cut refused: {} is stale", reserved.build));
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

/// A gap this machine's operator closes: it refuses where the audit finds it.
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

// (a) end to end ----------------------------------------------------------------

#[test]
fn a_bump_runs_every_step_in_order_and_ends_released() {
    let mut w = World::new();
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{:#?}", w.out);
    assert_eq!(
        last,
        format!(
            "RELEASED aterm 0.6.0 (build 1791600000) from {} — v0.6.0 is the channel's latest — {}",
            &BUMP[..9],
            last.rsplit(" — ").next().unwrap()
        )
    );
    let order: Vec<&str> = w
        .calls
        .iter()
        .map(String::as_str)
        .filter(|c| !matches!(*c, "audit" | "lease" | "cut-gates") && !c.starts_with("ls-remote"))
        .collect();
    // the real stage first (the preflight, then it beside the guard and the censuses),
    // the fixture generator beside the build, and the engine asked again right before
    // the publish
    assert_eq!(
        order,
        [
            "sync",
            "bump",
            "move cccc",
            "commit-gates",
            "stage",
            "guard",
            "census",
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
    // two provisioning audits: the checks', and the publish's
    assert_eq!(w.count("audit"), 2, "{:?}", w.calls);
    for step in Step::ALL {
        assert!(w.step_line(step).contains(" ok "), "{step:?}: {:#?}", w.out);
    }
    let j = w.journal().unwrap();
    assert_eq!((j.version.as_str(), j.commit.as_str()), ("0.6.0", BUMP));
    for fact in [
        crate::release::COMMIT_GATES,
        crate::release::FIXTURE_GUARD,
        crate::release::CENSUS,
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
    // every step that ran is measured for the next ETA
    assert!(w.times.contains("\tverify\t") && w.times.contains("\tcut\t"));
}

#[test]
fn a_second_run_finds_everything_done_and_only_checks() {
    let mut w = World::new();
    assert_eq!(w.run(bump()).0, 0);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{:#?}", w.out);
    assert!(last.starts_with("RELEASED aterm 0.6.0"), "{last}");
    for step in [
        Step::Stage,
        Step::Build,
        Step::Verify,
        Step::Ready,
        Step::Publish,
        Step::Cut,
        Step::AfterCut,
    ] {
        assert!(
            w.step_line(step).contains(" skip "),
            "{step:?}: {:#?}",
            w.out
        );
    }
    for never in [
        "bump",
        "commit-gates",
        "verify",
        "stage",
        "publish",
        "guard",
        "census",
    ] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert!(!w.calls.iter().any(|c| c.starts_with("cut ")));
}

// (a') the release's own handoff fixtures ---------------------------------------------

/// A generator that refuses refuses the release at the build step, beside which it runs:
/// before the publish and the cut's claim, so nothing is public and no number burned —
/// and the bundle built beside it is kept, so the run after the fix stages the fixtures
/// alone, building nothing again.
#[test]
fn a_generator_that_refuses_refuses_the_release_before_its_claim() {
    let mut w = World::new();
    w.generator_refuses = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{:#?}", w.out);
    assert!(
        last.starts_with("REFUSED at 5/11 build: the release's handoff fixture generator refuses"),
        "{last}"
    );
    assert!(last.contains("nothing public"), "{last}");
    for call in ["verify", "publish", "land-fixtures"] {
        assert_eq!(w.count(call), 0, "{call}: {:?}", w.calls);
    }
    assert!(
        !w.calls.iter().any(|c| c.starts_with("cut ")),
        "{:?}",
        w.calls
    );
    assert!(w.cut.is_none(), "nothing claimed");
    assert!(
        w.journal().unwrap().artifact.is_some(),
        "the bundle is kept"
    );

    // fixed on main: the same reservation is staged, nothing built again, and the
    // release goes on
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

/// Fixtures staged and not landed (the landing's push failed after the cut) are landed
/// by the next run, from the staging: the release's shape is not checked again and
/// nothing is generated again.
#[test]
fn fixtures_staged_and_not_landed_are_landed_by_the_next_run() {
    let mut w = World::new();
    w.landing_fails = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{:#?}", w.out);
    assert!(
        last.starts_with(
            "REFUSED at 10/11 after-cut: handoff fixtures: git push origin main failed"
        ),
        "{last}"
    );
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
    assert!(
        w.step_line(Step::AfterCut)
            .contains("v0.6.0's handoff fixtures landed on main"),
        "{:#?}",
        w.out
    );
}

/// Fixtures already on main (landed by an earlier run, or by hand) are left alone: no
/// staging, no landing, and step 10 skips once its shape half is done too.
#[test]
fn fixtures_already_on_main_are_left_alone() {
    let mut w = World::new();
    w.fixtures_on_main = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        (w.count("stage-fixtures"), w.count("land-fixtures")),
        (0, 0),
        "{:?}",
        w.calls
    );
    assert!(
        w.step_line(Step::Build)
            .contains("v0.6.0's handoff fixtures are on main"),
        "{:#?}",
        w.out
    );
    w.calls.clear();
    let (code, _) = w.run(mac());
    assert_eq!(code, 0);
    assert!(
        w.step_line(Step::AfterCut).contains(" skip "),
        "{:#?}",
        w.out
    );
    assert_eq!(w.count("land-fixtures"), 0, "{:?}", w.calls);
}

// (b) resume after a stop at each step --------------------------------------------

#[test]
fn a_run_stopped_at_any_step_resumes_there_and_repeats_nothing_done() {
    type Break = fn(&mut World);
    type Mend = fn(&mut World);
    let cases: [(Step, Break, Mend); 5] = [
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
    // resumed bare, and by the very command that stopped (docs/RELEASING.md: "the same
    // command resumes at that step") — a `--bump` past the cut included
    for ((step, broken, mend), again) in cases
        .into_iter()
        .flat_map(|case| [(case, mac()), (case, bump())])
    {
        let mut w = World::new();
        broken(&mut w);
        let (code, last) = w.run(bump());
        assert_ne!(code, 0, "{step:?}");
        assert!(
            last.starts_with(&format!("REFUSED at {}: ", step.at())),
            "{step:?}: {last}"
        );
        let first: Vec<String> = w.calls.clone();
        mend(&mut w);
        w.calls.clear();
        let (code, last) = w.run(again);
        assert_eq!(code, 0, "{step:?}: {last}\n{:#?}", w.out);
        assert!(last.starts_with("RELEASED aterm 0.6.0"), "{step:?}: {last}");
        // nothing the first run finished is run again
        for (call, done_by) in [
            ("bump", Step::Commit),
            ("verify", Step::Verify),
            ("stage", Step::Stage),
            ("publish", Step::Publish),
        ] {
            if done_by < step && first.iter().any(|c| c == call) {
                assert_eq!(w.count(call), 0, "{step:?} re-ran {call}: {:?}", w.calls);
            }
        }
        assert_eq!(w.count("bump"), 0, "--bump never bumps twice");
        let j = w.journal().unwrap();
        assert_eq!(j.commit, BUMP, "{step:?}: the candidate is kept");
    }
}

#[test]
fn a_cut_claimed_and_stopped_resumes_with_resume() {
    let mut w = World::new();
    w.cut_finishes = false;
    w.cut_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.contains("build 1791600000 of v0.6.0 is claimed")
            && last.ends_with("it resumes build 1791600000"),
        "{last}"
    );
    w.cut_finishes = true;
    w.cut_code = 0;
    w.calls.clear();
    let (code, _) = w.run(mac());
    assert_eq!(code, 0, "{:#?}", w.out);
    assert!(w.calls.iter().any(|c| c == "cut --resume"), "{:?}", w.calls);
    // past the claim, every pre-claim gate is past too
    assert_eq!(
        w.count("commit-gates") + w.count("cut-gates"),
        0,
        "{:?}",
        w.calls
    );
}

// (c) the final line --------------------------------------------------------------

#[test]
fn a_refusal_is_one_line_naming_the_step_why_what_is_public_and_the_remedy() {
    let mut w = World::new();
    w.verify_passes = false;
    w.verify_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with("REFUSED at 6/11 verify: `verify release` failed for ")
            && last.contains(" — nothing public. fix what it names on main"),
        "{last}"
    );
    assert_eq!(
        w.out.last().unwrap(),
        &last,
        "the final line is the last line"
    );

    // after the publish, and a cut that refused before its claim, the source is public
    let mut w = World::new();
    w.cut_claims = false;
    w.cut_code = 1;
    let (_, last) = w.run(bump());
    assert!(
        last.starts_with("REFUSED at 9/11 cut: the cut did not finish (ship cut exited 1)")
            && last.contains(
                " — the source of v0.6.0 is public (alabsystems e1e1e1e1e), no build claimed. "
            )
            && last.ends_with("it starts the cut again"),
        "{last}"
    );

    // a step that decided nothing is a resumable stop, exit 3, the candidate kept
    let mut w = World::new();
    w.verify_passes = false;
    w.verify_code = 3;
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert!(
        last.starts_with(
            "REFUSED at 6/11 verify: COULD NOT RUN — `verify release` could not run every \
             check for "
        ) && last.contains("targo --unverified ship release"),
        "{last}"
    );
    assert!(
        !w.calls.iter().any(|c| c == "sleep 60"),
        "no load-sampling retry: a COULD NOT RUN is a stop"
    );
    assert_eq!(w.journal().unwrap().commit, BUMP);
}

#[test]
fn a_run_that_stops_unexpectedly_still_ends_in_one_line_and_resumes() {
    let mut w = World::new();
    w.stage_panics = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with(
            "REFUSED at 4/11 stage: targo --unverified ship release stopped unexpectedly — \
             nothing public. Read "
        ) && last.ends_with("then run it again to resume"),
        "{last}"
    );
    assert!(
        !w.release_dir().join(RUNNING_FILE).exists(),
        "the claim goes with it"
    );
    w.stage_panics = false;
    w.calls.clear();
    assert_eq!(w.run(mac()).0, 0, "{:#?}", w.out);
    // what passed before the stop is not asked again: the bump, the preflight
    assert_eq!(
        w.count("bump") + w.count("commit-gates"),
        0,
        "{:?}",
        w.calls
    );
}

// (d) the dry run ------------------------------------------------------------------

#[test]
fn a_dry_run_plans_every_step_says_every_refusal_and_changes_nothing() {
    let mut w = World::new();
    w.on_main = false;
    let dry = ReleaseOptions {
        dry_run: true,
        ..bump()
    };
    let (code, last) = w.run(dry.clone());
    assert_eq!(code, 1, "{:#?}", w.out);
    assert!(
        last.starts_with(
            "DRY RUN aterm 0.6.0 from bbbbbbbbb: a real run would refuse at 1/11 \
                          checks: this clone is not on main"
        ),
        "{last}"
    );
    assert!(w.said("WOULD REFUSE  this clone is not on main"));
    for step in &Step::ALL[1..] {
        assert!(
            w.step_line(*step).contains(" plan "),
            "{step:?}: {:#?}",
            w.out
        );
    }
    assert!(w.journal().is_none(), "a dry run writes no journal");
    assert!(!w.release_dir().join(RUNNING_FILE).exists());
    let mutating = [
        "sync",
        "bump",
        "commit-gates",
        "stage",
        "guard",
        "census",
        "reserve",
        "build",
        "stage-fixtures",
        "verify",
        "publish",
        "land-fixtures",
    ];
    for call in mutating {
        assert_eq!(w.count(call), 0, "{call}: {:?}", w.calls);
    }
    assert!(
        !w.calls
            .iter()
            .any(|c| c.starts_with("cut ") || c.starts_with("move"))
    );

    // the same world fixed: the dry run would go ahead, and says how long
    w.on_main = true;
    let (code, last) = w.run(dry);
    assert_eq!(code, 0, "{last}");
    assert!(last.contains("a real run would go ahead, about "), "{last}");
    // and plans the real stage beside the guard and the censuses, and the release's own
    // handoff fixtures: written beside the build, landed after the cut
    assert!(
        w.step_line(Step::Stage)
            .contains("would run, beside each other: pub stage aterm --commit <the bump commit>"),
        "{:#?}",
        w.out
    );
    assert!(
        w.step_line(Step::Build)
            .contains("would write v0.6.0's handoff fixtures for the build it reserves"),
        "{:#?}",
        w.out
    );
    assert!(
        w.step_line(Step::AfterCut)
            .contains("would land v0.6.0's handoff fixtures, staged before the claim, on main"),
        "{:#?}",
        w.out
    );
}

#[test]
fn a_dry_run_and_a_real_run_meet_the_same_step_one_refusals() {
    let refusals = || {
        vec![
            Refusal::new("only 4 GiB free on /c — the cut needs 10", "free space"),
            Refusal::new("no publication engine at /p (bin/pub)", "clone it there"),
        ]
    };
    let mut w = World::new();
    w.machine_refusals = refusals();
    let (_, dry) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert!(dry.contains("(2 refusal(s) above)"), "{dry}");
    assert!(
        w.said("WOULD REFUSE  only 4 GiB free") && w.said("WOULD REFUSE  no publication engine")
    );
    let mut w = World::new();
    w.machine_refusals = refusals();
    let (code, real) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        real.starts_with("REFUSED at 1/11 checks: only 4 GiB free on /c"),
        "{real}"
    );
}

#[test]
fn a_preflight_block_is_met_at_minute_one_by_a_dry_bump_and_a_real_run_alike() {
    let block = || {
        vec![Refusal::new(
            "handoff fixtures — the handoff fixtures of v0.5.0 are not checked in",
            "generate them at tag v0.5.0",
        )]
    };
    let mut w = World::new();
    w.blocks = block();
    let (code, dry) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 1, "{dry}");
    assert!(
        dry.contains("would refuse at 4/11 stage: the preflight BLOCKED (1 finding(s))"),
        "{dry}"
    );
    assert!(w.said("| handoff fixtures — the handoff fixtures of v0.5.0 are not checked in"));
    assert!(
        w.step_line(Step::Stage)
            .contains("the preflight BLOCKED (1 finding(s), above)")
    );
    let mut w = World::new();
    w.blocks = block();
    let (code, real) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        real.starts_with("REFUSED at 4/11 stage: the preflight BLOCKED (1 finding(s))"),
        "{real}"
    );
    // refused in seconds: before the stage, the guard and the censuses
    for never in ["stage", "guard", "census"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert!(
        !w.journal().unwrap().has(crate::release::COMMIT_GATES),
        "a refused preflight is no fact the cut could read"
    );
    // a preflight that passes is journaled for R, and asked once
    let mut w = World::new();
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
    assert!(
        w.step_line(Step::Stage)
            .contains("the preflight passes: R's commit-only gates"),
        "{:#?}",
        w.out
    );
    assert_eq!(w.count("commit-gates"), 1, "{:?}", w.calls);
}

// (e) the owner, asked once, while the long steps run ---------------------------------

#[test]
fn an_owner_gap_is_asked_once_the_long_steps_run_and_the_release_waits_then_goes_on() {
    let mut w = World::new();
    // open at the checks' audit and at the publish's; closed at its first re-audit
    w.audits = VecDeque::from([owner_gap(), owner_gap(), passes()]);
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.asks, ["release.notary"], "asked exactly once");
    assert_eq!(w.resolved, ["release.notary"], "resolved when it closed");
    assert_eq!(
        w.count("audit"),
        3,
        "two audits and one re-audit: {:?}",
        w.calls
    );
    // asked at the checks, before the stage; waited on at the publish, after the long
    // steps
    let at = |call: &str| w.calls.iter().position(|c| c == call).unwrap();
    assert!(
        at("ask release.notary") < at("stage")
            && at("verify") < at("sleep 600")
            && at("sleep 600") < at("publish"),
        "{:?}",
        w.calls
    );
    let waiting: Vec<&String> = w
        .out
        .iter()
        .filter(|l| l.starts_with("WAITING (owner): "))
        .collect();
    assert_eq!(waiting.len(), 1, "said once: {waiting:?}");
    assert!(
        waiting[0].contains("is READY (built once, verified); nothing public"),
        "{}",
        waiting[0]
    );
    assert!(w.journal().unwrap().asks.is_empty());
    // the owner's wait is not the step's own time
    let publish_secs: Vec<u64> = w
        .times
        .lines()
        .filter(|l| l.contains("\tpublish\t"))
        .filter_map(|l| l.rsplit('\t').next()?.parse().ok())
        .collect();
    assert!(publish_secs.iter().all(|s| *s < 600), "{publish_secs:?}");
}

#[test]
fn a_machine_gap_refuses_before_the_long_steps_and_a_dry_run_says_it_would_wait() {
    let mut w = World::new();
    w.audits = VecDeque::from([vec![Finding {
        label: "x86 slice",
        state: Audited::Gap {
            what: "no x86_64 std".into(),
            fix: "rustup +stable target add x86_64-apple-darwin".into(),
            owner: false,
        },
    }]]);
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with("REFUSED at 1/11 checks: this machine's provisioning"),
        "{last}"
    );
    for never in ["bump", "stage"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }

    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap()]);
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}");
    assert!(
        last.contains("and wait for the owner at 8/11 publish"),
        "{last}"
    );
    assert!(w.said("WOULD WAIT  for the owner: notary"));
    assert!(w.asks.is_empty(), "a dry run asks nobody");
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
        last.starts_with("REFUSED at 8/11 publish: COULD NOT RUN — this machine's provisioning")
            && last.ends_with("(the candidate is kept)"),
        "{last}"
    );
    assert_eq!(
        w.count("verify"),
        1,
        "the long steps ran before it was needed"
    );
    assert_eq!(w.count("publish"), 0, "{:?}", w.calls);
}

// (f) the READY candidate is kept --------------------------------------------------

#[test]
fn a_bare_rerun_keeps_the_ready_candidate_and_commit_replaces_it_out_loud() {
    let mut w = World::new();
    // stop it at the wait at the publish, READY: the owner's gap holds, and the fake's
    // sleep is ended by making the next audit refuse
    w.audits = VecDeque::from([owner_gap(), owner_gap(), machine_gap()]);
    let (code, _) = w.run(bump());
    assert_eq!(code, 1);
    let j = w.journal().unwrap();
    assert_eq!(j.commit, BUMP);
    assert!(w.macho().is_some_and(|m| w.verified.contains(&m)), "READY");
    // main moves on
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.audits = VecDeque::from([passes()]);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert!(w.said("its READY candidate is kept"), "{:#?}", w.out);
    assert_eq!(w.journal().unwrap().commit, BUMP, "never chases main");
    assert_eq!(w.count("verify"), 0);

    // --commit replaces an unpublished candidate, saying what it costs
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap(), owner_gap(), machine_gap()]);
    let _ = w.run(bump());
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.audits = VecDeque::from([passes()]);
    let (code, last) = w.run(ReleaseOptions {
        commit: Some(NEXT.into()),
        ..mac()
    });
    assert_eq!(code, 0, "{last}");
    assert!(
        w.out.iter().any(|l| l.starts_with(&format!(
            "DECISION: --commit {} replaces the unpublished candidate v0.6.0 from {}",
            &NEXT[..9],
            &BUMP[..9]
        )) && l.contains("costs a `verify release` run")),
        "{:#?}",
        w.out
    );
    assert_eq!(w.journal().unwrap().commit, NEXT);
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
    assert_eq!(code, 1);
    assert!(
        last.contains("is already published — the cut releases that commit")
            && last.contains("finish it: targo --unverified ship release without --commit"),
        "{last}"
    );
}

// (g) one run per clone; the re-exec guard ------------------------------------------

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
        last.starts_with(
            "REFUSED at 1/11 checks: another targo --unverified ship release is running in this \
             clone (pid 777, since Thu Oct  9 01:00:00 2026)"
        ),
        "{last}"
    );
    assert!(w.calls.is_empty(), "{:?}", w.calls);
    // a dead holder (or its pid reused by another process) is no holder
    w.alive.insert(777, "Fri Oct 10 01:00:00 2026".into());
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
    assert!(
        w.release_dir()
            .join(format!("{RUNNING_FILE}.stale"))
            .exists()
    );
}

#[test]
fn a_cutter_that_is_not_rs_own_reexecs_once_and_a_reexec_that_still_is_not_refuses() {
    let mut w = World::new();
    w.own_cutter = false;
    let (code, last) = w.run(bump());
    assert_eq!(w.count("reexec"), 1, "{:?}", w.calls);
    assert_eq!(code, 1, "the fake's exec fails: {last}");
    // after the exec: the claim is this process's own (same pid, same start time)
    std::fs::create_dir_all(w.release_dir()).unwrap();
    std::fs::write(
        w.release_dir().join(RUNNING_FILE),
        format!("pid {}\nstarted {}\nlogs /l\n", w.pid, w.started),
    )
    .unwrap();
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 1);
    assert_eq!(w.count("reexec"), 0, "no loop");
    assert!(
        last.starts_with("REFUSED at 3/11 commit: this cutter was rebuilt at")
            && last.contains("the verdict's own remedy (nothing was claimed)"),
        "{last}"
    );
}

#[test]
fn forget_drops_the_journal_unless_a_run_is_live() {
    let mut w = World::new();
    w.cut_finishes = false;
    let _ = w.run(bump());
    assert!(w.journal().is_some());
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
    assert!(w.journal().is_some());
    w.alive.clear();
    let (code, last) = w.run(forget);
    assert_eq!(code, 0, "{last}");
    assert!(w.journal().is_none());
}

// (h) what the cut would refuse: at minute one, and again right before the publish -----

/// The disk MEASURE fills, and a rostered peer cutting the version
/// meanwhile, are asked again right before the publish: the refusal arrives with nothing
/// public (R staged on the private tier only).
#[test]
fn the_disk_and_the_tag_are_asked_again_before_the_publish() {
    type Change = fn(&mut World);
    let changes: [(Change, &str); 2] = [
        (|w| w.free = 4, "only 4 GiB free — the cut needs 10"),
        (
            |w| {
                w.origin_tags.insert("v0.6.0".into(), NEXT.into());
            },
            "v0.6.0 is on origin now (at ddddddddd) — it was cut while this release ran",
        ),
    ];
    for (change, why) in changes {
        let mut w = World::new();
        w.after_verify = Some(change);
        let (code, last) = w.run(bump());
        assert_eq!(code, 1, "{last}");
        assert!(
            last.starts_with(
                "REFUSED at 8/11 publish: asked again right before the publish, the cut would \
                 refuse (1 finding(s))"
            ) && last.contains(" — nothing public. "),
            "{last}"
        );
        assert!(w.said(&format!("| {why}")), "{why}: {:#?}", w.out);
        assert_eq!(w.count("publish"), 0, "{:?}", w.calls);
        assert!(w.published.is_empty(), "nothing public");
    }
}

/// The cut's own read-only pre-claim gates (the release lease another machine holds,
/// a roster or floor the channel moved) refuse at minute one, at the checks, before
/// anything changes — and a dry run reports them there.
#[test]
fn the_cuts_read_only_gates_refuse_at_minute_one_and_a_dry_run_says_so() {
    let held = || {
        vec![Refusal::new(
            "release lease — the release lock is held by claim abcdef; nothing was claimed",
            "on the machine that cut it: targo --unverified ship release",
        )]
    };
    let mut w = World::new();
    w.cut_gate_refusals = held();
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with("REFUSED at 1/11 checks: the cut would refuse (1 finding(s))")
            && last.contains("nothing public"),
        "{last}"
    );
    assert!(w.said("| release lease — the release lock is held by claim abcdef"));
    for never in ["bump", "stage"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    let mut w = World::new();
    w.cut_gate_refusals = held();
    let (code, dry) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 1);
    assert!(
        dry.contains("would refuse at 1/11 checks: the cut would refuse (1 finding(s))"),
        "{dry}"
    );
}

/// A judge that cannot answer is no "no": it stops at step 4 as COULD NOT RUN, never
/// after the hours of the steps whose record it would then not read either.
#[test]
fn a_judge_that_cannot_answer_stops_at_minute_one() {
    // resumed with step 4 journaled already (a COULD NOT RUN at `verify release`): still
    // stopped at step 4, before the long steps
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
        last.starts_with(
            "REFUSED at 4/11 stage: COULD NOT RUN — the cutter's own judge could not answer"
        ),
        "{last}"
    );
    assert_eq!(w.count("verify"), 0, "{:?}", w.calls);
}

/// The ledger row is written before sign-release. A nonzero publish refuses once,
/// and its row must not let a re-run claim or ship before that missing work is done.
#[test]
fn a_publish_that_recorded_source_but_failed_signing_cannot_be_skipped_into_a_cut() {
    let mut w = World::new();
    w.publish_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(
        last.contains("pub publish recorded") && last.contains("exited 1"),
        "{last}"
    );
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

    // The owner finishes the signing work the refusal names. Reuse the verified
    // candidate and the already published source without another public write.
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

/// AUDIT 2 BEFORE EVERY FRESH CUT. A run that published and then stopped short of the
/// claim leaves a re-run whose publish is skipped, and whose cut claims straight after:
/// the waiting audit, and what the cut would refuse, are asked at the publish step all
/// the same — an owner gap opened since (Apple's agreement lapsed) is waited on before
/// the claim, never met after it with a build spent.
#[test]
fn a_fresh_cut_whose_publish_is_skipped_is_audited_and_waits_before_its_claim() {
    let mut w = World::new();
    w.cut_claims = false;
    w.cut_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 9/11 cut"), "{last}");
    assert!(w.published.contains_key(BUMP) && w.cut.is_none());

    // open at the checks' audit and at the one before the cut; closed at its re-audit
    w.cut_claims = true;
    w.cut_code = 0;
    w.audits = VecDeque::from([owner_gap(), owner_gap(), passes()]);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("publish"), 0, "{:?}", w.calls);
    assert_eq!(w.count("audit"), 3, "{:?}", w.calls);
    assert_eq!(w.asks, ["release.notary"], "asked exactly once");
    assert_eq!(w.resolved, ["release.notary"], "resolved when it closed");
    let sleep = w.calls.iter().position(|c| c == "sleep 600").unwrap();
    let gates = w.calls.iter().rposition(|c| c == "cut-gates").unwrap();
    let cut = w.calls.iter().position(|c| c.starts_with("cut ")).unwrap();
    assert!(sleep < gates && gates < cut, "{:?}", w.calls);
    let publish = w.step_line(Step::Publish);
    assert!(
        publish.contains(" skip ")
            && publish.contains("asked again before the cut it does not publish for"),
        "{publish}"
    );
}

/// THE RE-CHECK RIGHT BEFORE WHAT GOES PUBLIC. The publish audits the provisioning
/// first, and may wait there for the owner — hours; so what the cut would refuse is
/// asked again after that wait, the last thing before the engine's re-ask and `pub
/// publish` — and what changed during the wait refuses there, nothing public.
#[test]
fn what_changes_while_the_publish_waits_is_asked_again_right_before_it() {
    // every run: the publish's audit, then the re-check (the tag on origin now, then the
    // cut's gates), then the engine and the staging tier, then the publish
    let mut w = World::new();
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
    let at = w.calls.iter().position(|c| c == "publish").unwrap();
    let before: Vec<&str> = w.calls[..at]
        .iter()
        .rev()
        .take(6)
        .map(String::as_str)
        .collect();
    assert_eq!(
        before,
        [
            "staging-now",
            "engine-now",
            "sync",
            "cut-gates",
            "ls-remote v0.6.0",
            "audit",
        ],
        "{:?}",
        w.calls
    );

    // the owner is waited on at the publish, and meanwhile a rostered peer cuts v0.6.0 —
    // or the disk fills
    type Change = fn(&mut World);
    let changes: [(Change, &str); 2] = [
        (
            |w| {
                w.origin_tags.insert("v0.6.0".into(), NEXT.into());
            },
            "| v0.6.0 is on origin now (at ddddddddd)",
        ),
        (|w| w.free = 4, "| only 4 GiB free — the cut needs 10"),
    ];
    for (change, why) in changes {
        let mut w = World::new();
        w.audits = VecDeque::from([passes(), owner_gap(), passes()]);
        w.after_sleep = Some(change);
        let (code, last) = w.run(bump());
        assert_eq!(code, 1, "{last}");
        assert!(
            last.starts_with(
                "REFUSED at 8/11 publish: asked again right before the publish, the cut would \
                 refuse (1 finding(s))"
            ) && last.contains(" — nothing public. "),
            "{last}"
        );
        assert!(w.said(why), "{why}: {:#?}", w.out);
        assert_eq!(w.count("sleep 600"), 1, "{:?}", w.calls);
        assert_eq!(w.count("publish"), 0, "{:?}", w.calls);
    }
}

/// A `--bump` is refused BEFORE its push by what would refuse the commit it makes: the
/// next version already cut on origin, a leftover local tag of it, and a predecessor
/// whose claim commit main does not contain — for a real run and a dry run alike.
#[test]
fn a_bump_is_refused_before_its_push_by_what_would_refuse_its_commit() {
    type Change = fn(&mut World);
    let cases: [(Change, &str); 3] = [
        (
            |w| {
                w.origin_tags.insert("v0.6.0".into(), NEXT.into());
            },
            "REFUSED at 1/11 checks: v0.6.0 is already cut (its tag is on origin)",
        ),
        (
            |w| {
                w.local_tags.insert("v0.6.0".into());
            },
            "REFUSED at 1/11 checks: tag v0.6.0 exists in this clone and not on origin — a \
             leftover",
        ),
        (
            |w| {
                w.origin_tags.insert("v0.5.0".into(), NEXT.into());
            },
            "REFUSED at 1/11 checks: bbbbbbbbb does not contain v0.5.0's claim commit ddddddddd",
        ),
    ];
    for (change, want) in cases {
        let mut w = World::new();
        change(&mut w);
        let (code, last) = w.run(bump());
        assert_eq!(code, 1, "{last}");
        assert!(last.starts_with(want), "{want}\n{last}");
        assert_eq!(w.count("bump"), 0, "nothing pushed: {:?}", w.calls);
        assert!(w.journal().is_none(), "nothing journaled");

        let mut w = World::new();
        change(&mut w);
        let (code, dry) = w.run(ReleaseOptions {
            dry_run: true,
            ..bump()
        });
        assert_eq!(code, 1, "{dry}");
        let why = want.trim_start_matches("REFUSED at ");
        assert!(dry.contains(&format!("would refuse at {why}")), "{dry}");
    }
}

/// A wait for the owner that a signal stops ends the run in its INTERRUPTED line, exit
/// 130, with nothing staged.
#[test]
fn a_signal_while_waiting_for_the_owner_ends_the_run_interrupted() {
    let mut w = World::new();
    w.audits = VecDeque::from([passes(), owner_gap(), passes()]);
    w.after_sleep = Some(|w| w.signal = Some(15));
    let (code, last) = w.run(bump());
    assert_eq!(code, 130, "{last}");
    assert!(
        last.starts_with("INTERRUPTED at 8/11 publish (by SIGTERM)"),
        "{last}"
    );
    assert_eq!(w.count("publish"), 0, "{:?}", w.calls);
}

/// An exec forgets every signal this process caught, so a signal caught during step 3
/// ends the run there, in its one line, and R's cutter is never exec'd.
#[test]
fn a_signal_in_the_commit_step_ends_the_run_before_the_reexec() {
    let mut w = World::new();
    w.own_cutter = false;
    w.signal_at = Some("move");
    let (code, last) = w.run(bump());
    assert_eq!(code, 130, "{last}");
    assert!(
        last.starts_with("INTERRUPTED at 3/11 commit (by SIGTERM)"),
        "{last}"
    );
    assert_eq!(w.count("reexec"), 0, "{:?}", w.calls);
}

/// The engine sync after `pub stage` that fails is the step's refusal, never ignored.
#[test]
fn an_engine_sync_after_the_stage_that_fails_refuses_the_stage() {
    let mut w = World::new();
    w.stage_sync_fails = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with("REFUSED at 4/11 stage: the engine is not a fast-forward")
            && last.contains("audit-rows.jsonl"),
        "{last}"
    );
    assert_eq!(w.count("publish"), 0);
}

// (i) interrupted, never orphaned ------------------------------------------------------

/// A signal ends the step's child; the release ends in ONE line, exit 130, its claim
/// gone, and the same command resumes there.
#[test]
fn an_interrupt_ends_the_run_in_one_line_and_it_resumes() {
    let mut w = World::new();
    w.signal_at = Some("verify");
    let (code, last) = w.run(bump());
    assert_eq!(code, 130, "{last}");
    assert_eq!(
        last,
        "INTERRUPTED at 6/11 verify (by SIGTERM) — nothing public. Run targo --unverified ship \
         release again to resume"
    );
    assert_eq!(w.out.last().unwrap(), &last);
    assert!(!w.release_dir().join(RUNNING_FILE).exists());
    w.signal = None;
    w.signal_at = None;
    w.calls.clear();
    assert_eq!(w.run(mac()).0, 0, "{:#?}", w.out);
    assert_eq!(w.count("bump"), 0);

    // at the cut, the line says the cut resumes
    let mut w = World::new();
    w.signal_at = Some("cut");
    let (code, last) = w.run(bump());
    assert_eq!(code, 130);
    assert!(
        last.starts_with(
            "INTERRUPTED at 9/11 cut (by SIGTERM) — build 1791600000 of v0.6.0 is claimed."
        ) && last.contains("The cut stopped with this run; running the command again resumes it."),
        "{last}"
    );
}

/// The probe mode of [`a_real_sigterm_reaches_the_steps_group_and_ends_interrupted`]:
/// set, the test is the release itself (its own process, its own signal handlers).
const INTERRUPT_PROBE: &str = "ATERM_RELEASE_INTERRUPT_PROBE";

/// The verify step's child in the probe: a shell (the step's child, its group's leader)
/// and a subshell it leaves running (another member of that group), each writing what
/// it was sent once its trap is armed. Bounded, so nothing outlives a broken run long.
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
        crate::release_steps::interrupt::install();
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
    // a hang detector, never a latency budget
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
    assert_eq!(
        last,
        "INTERRUPTED at 6/11 verify (by SIGTERM) — nothing public. Run targo --unverified ship \
         release again to resume"
    );
}

/// A run killed outright (SIGKILL) leaves its step's child: while that child lives, the
/// clone is held, and the refusal names the step and how to stop it — its own process,
/// never its group, where a trustd it started may live.
#[test]
fn a_step_left_running_by_a_killed_run_holds_the_clone() {
    let mut w = World::new();
    std::fs::create_dir_all(w.release_dir()).unwrap();
    std::fs::write(
        w.release_dir().join(RUNNING_CHILD_FILE),
        "pid 888\nstarted Thu Oct  9 01:30:00 2026\nstep verify\n",
    )
    .unwrap();
    w.alive.insert(888, "Thu Oct  9 01:30:00 2026".into());
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with(
            "REFUSED at 1/11 checks: the verify step of a targo --unverified ship release that \
             was killed is still running in this clone (pid 888"
        ) && last.contains("kill -TERM 888 — the step's own process, never its group"),
        "{last}"
    );
    assert!(!last.contains("-- -888"), "{last}");
    // once it has ended, it holds nothing
    w.alive.clear();
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
}

/// A STEP RUNS SEVERAL CHILDREN BESIDE EACH OTHER (the stage, the guard, the censuses),
/// each noted on its own: any one still alive after its run was killed holds the clone,
/// named; the notes of the ones that ended hold nothing, and the next run clears them.
#[test]
fn every_child_a_killed_step_left_running_holds_the_clone() {
    let mut w = World::new();
    let dir = w.release_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let note = |pid: u32| {
        std::fs::write(
            crate::release::child_note(&dir, pid),
            format!(
                "pid {pid}\nstarted Thu Oct  9 01:30:0{} 2026\nstep stage\n",
                pid % 10
            ),
        )
        .unwrap();
    };
    note(801);
    note(802);
    // a stray file that only looks like a note is none
    std::fs::write(dir.join(format!("{RUNNING_CHILD_FILE}.x")), "pid 803\n").unwrap();
    w.alive.insert(802, "Thu Oct  9 01:30:02 2026".into());
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with(
            "REFUSED at 1/11 checks: the stage step of a targo --unverified ship release that \
             was killed is still running in this clone (pid 802"
        ),
        "{last}"
    );
    assert_eq!(crate::release::child_notes(&dir).len(), 2);
    w.alive.clear();
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
    assert!(
        crate::release::child_notes(&dir).is_empty(),
        "the dead children's notes are cleared"
    );
}

// (j) the owner's asks: delivered, carried, withdrawn

/// An ask the owner's aterm did not take is raised again at the next audit, and is
/// journaled only once delivered.
#[test]
fn an_ask_that_was_not_delivered_is_raised_again() {
    let mut w = World::new();
    w.undelivered = 1;
    // the checks' ask is not delivered; the publish's audit raises it again
    w.audits = VecDeque::from([owner_gap(), owner_gap(), passes()]);
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("ask release.notary"), 2, "{:?}", w.calls);
    assert_eq!(w.asks, ["release.notary"], "delivered once");
    assert_eq!(w.resolved, ["release.notary"]);
    assert!(w.said("could not ask the owner (release.notary): no aterm window answered"));
}

/// A check that got no answer resolves nothing: owner gap, COULD NOT RUN, owner gap is
/// ONE ask, resolved only when the check passes — here across two runs, the first
/// stopped by `verify release` after the checks asked.
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
    let resolved_at = w
        .out
        .iter()
        .position(|l| l.contains("the owner closed release.notary"))
        .unwrap();
    let published_at = w
        .out
        .iter()
        .position(|l| l.starts_with("[ 8/11] publish") && l.contains(" ok "))
        .unwrap();
    let publish_at = w
        .out
        .iter()
        .position(|l| l.starts_with("[ 8/11] publish") && l.contains(" start "))
        .unwrap();
    assert!(
        publish_at < resolved_at && resolved_at < published_at,
        "{:#?}",
        w.out
    );
}

/// A replaced candidate keeps the asks raised for the machine; `--forget` withdraws
/// them, so no standing row outlives the release it was raised for.
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
    let (code, _) = w.run(ReleaseOptions {
        forget: true,
        ..ReleaseOptions::default()
    });
    assert_eq!(code, 0);
    assert_eq!(w.withdrawn, ["release.notary"]);
    assert!(w.journal().is_none());
}

/// An ask the checks delivered before the release had a journal, on a run that then
/// stops before step 3 journals it, is withdrawn before the final line: no standing row
/// outlives a run that left nothing able to resolve or withdraw it.
#[test]
fn an_ask_no_journal_took_is_withdrawn_when_the_run_stops_before_step_3() {
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap()]);
    w.cut_gate_refusals = vec![Refusal::new(
        "release lease — the release lock is held by claim abcdef; nothing was claimed",
        "on the machine that cut it: targo --unverified ship release",
    )];
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 1/11 checks"), "{last}");
    assert!(w.journal().is_none(), "nothing journaled");
    assert_eq!(w.asks, ["release.notary"]);
    assert_eq!(w.withdrawn, ["release.notary"]);
    assert_eq!(
        w.out.last().unwrap(),
        &last,
        "the final line is the last line"
    );
}

// (k) a claim forgotten, the site, the successor, the re-exec, --forget ---------------

/// NO CLAIM IS HELD TO A PROVER, because no resume compiles. The cut claims bytes the
/// release already built and verified, and its journal (`dist/cut-state.toml`) names
/// them, so a resume signs those bytes whatever prover it runs under. `--forget` around
/// a claimed, unfinished cut therefore drops nothing its resume needs: the run after it
/// resumes the claim with `--resume`, and never reserves, builds or verifies again.
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

/// A machine with no site checkout (`ship site` touched nothing) ends the
/// release: the site is a hand step, journaled and said — in the step, the final line
/// and every later run — never a refusal a re-run here would only repeat.
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
    assert!(
        w.step_line(Step::AfterCut).contains(" ok ")
            && w.step_line(Step::AfterCut)
                .contains(crate::release::SITE_BY_HAND),
        "{:#?}",
        w.out
    );
    let j = w.journal().unwrap();
    assert!(
        j.finished() && j.has("after-cut") && j.has("site-deferred"),
        "{j:?}"
    );
    // the hand step adds no ` — `: the final line's three parts stay three
    assert_eq!(last.matches(" — ").count(), 2, "{last}");
    // a later run checks again, and says it again, at the step too: nothing is re-run
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert!(last.contains(crate::release::SITE_BY_HAND), "{last}");
    assert_eq!(w.count("after-cut 0.6.0"), 0, "{:?}", w.calls);
    let skipped = w.step_line(Step::AfterCut);
    assert!(
        skipped.contains(" skip ") && skipped.contains(crate::release::SITE_BY_HAND),
        "{skipped}"
    );
    // the step keeps the sync's own line: where it looked, or what the checkout lacks
    let lacks = "site checkout at /s has no executable deploy.sh — not a usable site checkout; \
                 set SITE_DIR or skip with --skip-site";
    let deferred = crate::release_steps::site_outcome(
        &Ok(crate::site::Outcome::NoCheckout(lacks.into())),
        Vec::new(),
    )
    .unwrap();
    assert!(deferred.site_deferred, "{deferred:?}");
    assert_eq!(deferred.note, format!("ship site said: {lacks}"));
    // asked for no deploy (SITE_NO_DEPLOY): said as not deployed, never as already shown
    let undeployed =
        crate::release_steps::site_outcome(&Ok(crate::site::Outcome::NoDeploy), Vec::new())
            .unwrap();
    assert!(
        undeployed.note.contains("NOT deployed") && !undeployed.note.contains("already"),
        "{undeployed:?}"
    );
    assert!(!undeployed.site_deferred);
    // a failed sync is a stop that names the failure and keeps the transcript's end
    let failed = crate::release_steps::site_outcome(
        &Err(crate::site::Failure(
            "push to origin main failed (above)".into(),
        )),
        vec!["  remote: refused".into()],
    )
    .unwrap_err();
    assert!(
        failed
            .why
            .contains("the site sync failed: push to origin main failed"),
        "{failed:?}"
    );
    // exit 4 (deployed, the live site not agreeing yet) is still a stop to retry
    let mut w = World::new();
    w.site = Ok(crate::site::Outcome::LiveDisagrees {
        deployed: true,
        why: Vec::new(),
    });
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with("REFUSED at 10/11 after-cut: the site is deployed"),
        "{last}"
    );
}

/// A PUBLISHED RELEASE IS NEVER RE-CUT, so a refusal of its shape after the cut is one
/// no re-run of it can end — the very command resumes it and meets it again, and
/// `--bump` never bumps twice — and the refusal names the way out instead: `--forget`,
/// then `--bump`, which ships the successor; or, where main
/// already declares a version no release has used (and `--bump` says "drop --bump"), a
/// bare run.
#[test]
fn a_shape_refusal_names_its_way_out_and_following_it_ships_the_successor() {
    let refused = || {
        let mut w = World::new();
        w.after_cut = Err(crate::release_steps::shape_refusal(
            "alabsystems/aterm",
            "v0.6.0",
            vec!["aterm-0.6.0.dmg is missing".into()],
        ));
        let (code, last) = w.run(bump());
        assert_eq!(code, 1);
        assert!(
            last.starts_with(
                "REFUSED at 10/11 after-cut: alabsystems/aterm v0.6.0 does NOT carry the \
                 one-lean-download shape (1 problem(s))"
            ) && last.contains(
                "ship its successor from current main with targo --unverified ship release \
                 --forget (this clone's journal of v0.6.0; nothing public changes), then targo \
                 --unverified ship release --bump, or a bare targo --unverified ship release \
                 where origin/main already declares a version no release has used"
            ),
            "{last}"
        );
        // v0.6.0 is cut: its tag on origin, its ledger line
        w.origin_tags.insert("v0.6.0".into(), BUMP.into());
        w.ledger.push("0.6.0".into());
        // the same command resumes it and meets the same refusal: --bump never bumps twice
        w.calls.clear();
        let (code, again) = w.run(bump());
        assert_eq!((code, again.as_str()), (1, last.as_str()));
        assert_eq!(w.count("bump"), 0, "{:?}", w.calls);
        // the way out: --forget
        let forget = ReleaseOptions {
            forget: true,
            ..ReleaseOptions::default()
        };
        let (code, last) = w.run(forget);
        assert_eq!(code, 0, "{last}");
        assert!(w.journal().is_none());
        w.after_cut = Ok("the release's shape checked".into());
        w.calls.clear();
        w
    };

    // then --bump ships the successor, under a number reserved above v0.6.0's claim
    let mut w = refused();
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

    // main already declares an unused version: --bump says "drop --bump", and a bare
    // run (the journal forgotten) releases it
    let mut w = refused();
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.7.0".into());
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.contains("origin/main already declares 0.7.0, which no release has used")
            && last.ends_with("release it as it is: drop --bump"),
        "{last}"
    );
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with(&format!(
            "RELEASED aterm 0.7.0 (build 1791600001) from {}",
            &NEXT[..9]
        )),
        "{last}"
    );
    assert_eq!(w.count("bump"), 0, "{:?}", w.calls);
}

/// THE RE-EXEC KEEPS R. `--commit origin/main` names main's tip when the run starts;
/// re-run as R's own cutter (the same pid: its claim adopted) after main moved, the run
/// keeps the R it chose and journaled, never replacing it with the new tip.
#[test]
fn a_reexec_keeps_the_candidate_when_main_moved_while_rs_cutter_was_built() {
    let mut w = World::new();
    w.versions.insert(TIP.into(), "0.6.0".into());
    w.own_cutter = false;
    let tip = ReleaseOptions {
        commit: Some("origin/main".into()),
        ..mac()
    };
    let (code, _) = w.run(tip.clone());
    assert_eq!(code, 1, "the fake's exec fails");
    assert_eq!(w.count("reexec"), 1, "{:?}", w.calls);
    assert_eq!(w.journal().unwrap().commit, TIP);
    // the exec: the same process, its claim kept; R's cutter took a while to build
    std::fs::create_dir_all(w.release_dir()).unwrap();
    std::fs::write(
        w.release_dir().join(RUNNING_FILE),
        format!("pid {}\nstarted {}\nlogs /l\n", w.pid, w.started),
    )
    .unwrap();
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.own_cutter = true;
    w.calls.clear();
    let (code, last) = w.run(tip);
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(!w.said("DECISION"), "{:#?}", w.out);
    assert!(
        last.starts_with(&format!(
            "RELEASED aterm 0.6.0 (build 1791600000) from {}",
            &TIP[..9]
        )),
        "{last}"
    );
    assert_eq!(w.journal().unwrap().commit, TIP);
}

/// `--forget` drops the journal and exits: a dry run's "nothing changes", and every
/// flag of a run, would be ignored by it, so each is refused at parse.
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
        let refused = parse(bad).unwrap_err();
        assert!(refused.starts_with("--forget drops"), "{bad:?}: {refused}");
    }
    assert!(matches!(
        parse(&["release", "--forget", "--verbose", "--engine", "/e"]),
        Ok(crate::cli::Cmd::Release(o)) if o.forget && o.verbose
    ));
}

// (p) prepare, then release -------------------------------------------------------------

/// `--prepare` runs the heavy half and stops before the publish: PREPARED in the journal
/// with the prover it held, the clone's run markers let go before its line, which names
/// the command that finishes it (a dry run plans both halves). A bare run then asks the
/// live world again before anything is public: a prover that is not the prepared build
/// refuses at step 1, nothing published or claimed; the prover back, it asks the
/// provisioning, the tag, the ledger tail and the engine — which moved since it staged,
/// so the stage runs again — and goes on from the publish, nothing heavy run again.
#[test]
fn a_prepare_stops_before_the_publish_and_the_bare_run_reasks_then_releases() {
    let mut w = World::new();
    let prepare = ReleaseOptions {
        prepare: true,
        ..bump()
    };
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..prepare.clone()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.contains("a real --prepare would go ahead and stop PREPARED after 7/11 ready")
            && last.contains("then targo --unverified ship release would finish it"),
        "{last}"
    );
    assert!(w.said("--prepare would stop here"), "{:#?}", w.out);
    assert!(w.step_line(Step::Cut).contains(" plan "), "{:#?}", w.out);
    assert!(w.journal().is_none(), "a dry run writes no journal");

    // the heavy half, then the stop
    w.calls.clear();
    w.child_note_at_stage = true;
    let (code, last) = w.run(prepare);
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with(&format!(
            "PREPARED aterm 0.6.0 (build 1791600000) from {} — built once, verified and \
             staged; nothing public — ",
            &BUMP[..9]
        )) && last.ends_with(". Finish it: targo --unverified ship release"),
        "{last}"
    );
    for heavy in ["build", "verify", "stage"] {
        assert_eq!(w.count(heavy), 1, "{heavy}: {:?}", w.calls);
    }
    assert_eq!(w.count("publish"), 0, "{:?}", w.calls);
    assert!(
        !w.calls.iter().any(|c| c.starts_with("cut ")),
        "{:?}",
        w.calls
    );
    assert!(
        w.cut.is_none() && w.published.is_empty(),
        "nothing claimed or public"
    );
    // the run's markers let go before its line (the release holds no machine lock)
    assert_eq!(
        w.running_at_prepared,
        Some(false),
        "the claim goes before the line"
    );
    assert!(!w.release_dir().join(RUNNING_CHILD_FILE).exists());
    let j = w.journal().unwrap();
    let prepared = j.prepared.clone().expect("PREPARED is journaled");
    assert_eq!(prepared.prover, "/store/trust/9192/bin");
    assert_eq!(prepared.trustc, prover().commit);
    assert_eq!(prepared.build, 1_791_600_000);
    assert_eq!(Some(prepared.macho_sha256.clone()), w.macho());
    assert_eq!(prepared.engine, w.engine);
    assert!(!j.finished());

    // another prover: refused at step 1, nothing public, nothing claimed
    w.child_note_at_stage = false;
    w.prover = Prover {
        bin: PathBuf::from("/store/trust/9300/bin"),
        commit: "fd87ed4a0060000000000000000000000000000".into(),
        source: "TRUST_STAGE2_BIN".into(),
    };
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with(
            "REFUSED at 1/11 checks: v0.6.0 was PREPARED with the prover /store/trust/9192/bin"
        ) && last.contains("this run's is /store/trust/9300/bin"),
        "{last}"
    );
    assert!(
        last.contains("--prepare (the build and `verify release` again"),
        "{last}"
    );
    for never in ["sync", "build", "verify", "stage", "publish"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert!(w.cut.is_none() && w.published.is_empty());
    assert_eq!(
        w.journal().unwrap().prepared,
        Some(prepared),
        "the prepare is kept"
    );

    // the prepared prover back, and the engine moved since it staged
    w.prover = prover();
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
        "guard",
        "census",
        "reserve",
        "build",
        "verify",
    ] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    let at = |call: &str| {
        w.calls
            .iter()
            .position(|c| c == call)
            .unwrap_or_else(|| panic!("{call}: {:?}", w.calls))
    };
    // asked first, before the stage again: the provisioning, the tag, the engine
    for asked in ["audit", "ls-remote v0.6.0", "engine-now"] {
        assert!(at(asked) < at("stage"), "{asked}: {:?}", w.calls);
    }
    // the stage again, by the engine as it is now, then the publish and the cut
    assert_eq!(w.count("stage"), 1, "{:?}", w.calls);
    assert!(
        at("stage") < at("publish") && at("publish") < at("cut --mac-only"),
        "{:?}",
        w.calls
    );
    assert!(w.said("v0.6.0 was PREPARED at "), "{:#?}", w.out);
    assert!(w.journal().unwrap().finished());
}

/// ONLY A CLAIMED CUT IS PAST ITS PREPARE. A visible half that published and then
/// stopped short of the claim — a peer's release claimed past the reservation as the cut
/// started — leaves the source public, nothing claimed, and its PREPARED record stale: the
/// next bare run refuses before anything heavy (the ledger reached the prepared build),
/// keeping the record; `--prepare` reserves anew, builds and verifies, and records
/// PREPARED again over the published source; and the bare run after it releases that
/// build, nothing heavy run again.
#[test]
fn a_prepare_overtaken_after_its_publish_is_prepared_again_then_released() {
    let mut w = World::new();
    let (code, last) = w.run(ReleaseOptions {
        prepare: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let first = w.journal().unwrap().prepared.unwrap();

    // the visible half publishes; a peer claims past the reservation as the cut starts
    w.overtaken_at_cut = true;
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(last.starts_with("REFUSED at 9/11 cut"), "{last}");
    assert!(
        w.published.contains_key(BUMP) && w.cut.is_none(),
        "public, nothing claimed"
    );

    // the bare run refuses: the ledger reached the prepared build — nothing heavy runs
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with("REFUSED at 4/11 stage: the PREPARED v0.6.0 is stale"),
        "{last}"
    );
    assert!(
        w.said(&format!(
            "origin's ledger tail {} reached the reserved build {}",
            first.build, first.build
        )),
        "{:#?}",
        w.out
    );
    for never in ["build", "verify", "stage", "publish"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert_eq!(
        w.journal().unwrap().prepared,
        Some(first.clone()),
        "the record is kept"
    );

    // `--prepare` makes it fresh over the published source: reserved anew, built, verified
    w.calls.clear();
    let (code, last) = w.run(ReleaseOptions {
        prepare: true,
        ..mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let fresh = w.journal().unwrap().prepared.unwrap();
    assert!(fresh.build > first.build, "{fresh:?}");
    let from = &BUMP[..9];
    let want = format!("PREPARED aterm 0.6.0 (build {}) from {from}", fresh.build);
    assert!(
        last.starts_with(&want) && last.contains("the source of v0.6.0 is public"),
        "{last}"
    );
    assert_eq!(
        (w.count("reserve"), w.count("build"), w.count("verify")),
        (1, 1, 1),
        "{:?}",
        w.calls
    );
    assert_eq!(
        (w.count("stage"), w.count("publish")),
        (0, 0),
        "{:?}",
        w.calls
    );
    assert_eq!(Some(fresh.macho_sha256.clone()), w.macho());
    assert!(w.cut.is_none(), "nothing claimed");

    // the bare run asks again and releases the fresh build
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with(&format!("RELEASED aterm 0.6.0 (build {})", fresh.build)),
        "{last}"
    );
    for never in ["build", "verify", "stage", "publish"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert_eq!(w.cut.as_ref().unwrap().build, fresh.build);
}

/// What the visible half could not ask is a resumable stop, the prepare kept; and the
/// engine it publishes by is asked once more right before `pub publish`, after a sync:
/// one a peer moved after the re-ask at 4/11 is no stop — R is staged again there, by
/// the engine as it is now, and released, nothing heavy run again.
#[test]
fn a_prepared_release_stops_on_what_it_cannot_ask_and_stages_again_before_the_publish() {
    let mut w = World::new();
    let (code, last) = w.run(ReleaseOptions {
        prepare: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let prepared = w.journal().unwrap().prepared.unwrap();

    // origin does not answer: COULD NOT RUN at minute one, nothing public, the prepare
    // kept
    w.ls_remote_down = true;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 3, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with(
            "REFUSED at 1/11 checks: COULD NOT RUN — what the cut would refuse could not be \
             asked (1 question(s))"
        ),
        "{last}"
    );
    assert!(
        w.said("| cannot ask origin whether v0.6.0 is cut: origin did not answer"),
        "{:#?}",
        w.out
    );
    for never in ["sync", "build", "verify", "stage", "publish"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert_eq!(w.journal().unwrap().prepared, Some(prepared.clone()));

    // origin back; a peer moves the engine after the re-ask: the sync right before the
    // publish brings it (the step-2 sync, then the publish's)
    w.ls_remote_down = false;
    let moved: fn(&mut World) = |w| w.engine = "f2f2f2f2f2".into();
    w.after_sync = VecDeque::from([None, Some(moved)]);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with("RELEASED aterm 0.6.0 (build 1791600000)"),
        "{last}"
    );
    assert!(
        w.said("is staged again by the engine as it is now, before the publish"),
        "{:#?}",
        w.out
    );
    let at = |call: &str| w.calls.iter().position(|c| c == call).unwrap();
    assert!(
        at("engine-now") < at("stage") && at("stage") < at("publish"),
        "{:?}",
        w.calls
    );
    assert_eq!(
        (w.count("stage"), w.count("publish")),
        (1, 1),
        "{:?}",
        w.calls
    );
    assert_eq!(
        w.staged_by.as_deref(),
        Some("f2f2f2f2f2"),
        "by the engine as it is"
    );
    for never in ["reserve", "build", "verify"] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
}

/// EVERY RELEASE PUBLISHES BY THE ENGINE THAT STAGED IT. The stage comes before the long
/// steps now, hours before the publish, and the engine checkout is shared: one whose code
/// moved meanwhile stages R again right before the publish — never a stop — and only
/// then publishes.
#[test]
fn an_engine_moved_while_the_long_steps_ran_stages_again_before_the_publish() {
    let mut w = World::new();
    w.after_verify = Some(|w| w.engine = "f3f3f3f3f3".into());
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("stage"), 2, "{:?}", w.calls);
    let staged: Vec<usize> = w
        .calls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.as_str() == "stage")
        .map(|(i, _)| i)
        .collect();
    let at = |call: &str| w.calls.iter().position(|c| c == call).unwrap();
    assert!(
        staged[0] < at("build") && at("verify") < staged[1] && staged[1] < at("publish"),
        "{:?}",
        w.calls
    );
    assert_eq!(w.staged_by.as_deref(), Some("f3f3f3f3f3"));
    // the same engine all along: one stage
    let mut w = World::new();
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
    assert_eq!(w.count("stage"), 1, "{:?}", w.calls);
}

/// A STAGING TIER A PEER MOVED STAGES AGAIN TOO. `pub promote` refuses an attestation
/// whose `<repo>-staging` main moved since it was made (any peer's `pub stage aterm`),
/// and the stage is hours before the publish: the staging tier is asked again right
/// before the publish, and one that moved stages R again by the same engine — never a
/// refusal at the publish that every re-run would meet again.
#[test]
fn a_staging_tier_moved_while_the_long_steps_ran_stages_again_before_the_publish() {
    let mut w = World::new();
    w.after_verify = Some(|w| w.peer_staged = true);
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("stage"), 2, "{:?}", w.calls);
    let staged: Vec<usize> = w
        .calls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.as_str() == "stage")
        .map(|(i, _)| i)
        .collect();
    let at = |call: &str| w.calls.iter().position(|c| c == call).unwrap();
    assert!(
        at("verify") < at("staging-now")
            && at("staging-now") < staged[1]
            && staged[1] < at("publish"),
        "{:?}",
        w.calls
    );
    assert!(
        w.said("the staging tier's main moved since R was attested")
            && w.said("is staged again by the engine as it is now, before the publish"),
        "{:#?}",
        w.out
    );
    // asked once more after the stage, which attested the staging tier as it is
    assert_eq!(w.count("staging-now"), 2, "{:?}", w.calls);
    assert_eq!(
        w.staged_by.as_deref(),
        Some("e7e7e7e7e7"),
        "the same engine"
    );
}

/// STEP 4'S WORK RUNS BESIDE EACH OTHER, AND WHAT PASSED IS KEPT. A census finding refuses
/// the release at 4/11, in minute one, with nothing public (R staged on the private tier
/// only) and before the build — and the stage and the guard that passed beside it
/// are journaled, so the run after the fix runs the censuses alone.
#[test]
fn a_census_finding_refuses_at_the_stage_and_keeps_what_passed_beside_it() {
    let mut w = World::new();
    w.census_finds = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}\n{:#?}", w.out);
    assert!(
        last.starts_with("REFUSED at 4/11 stage: L0 CENSUS FINDING")
            && last.contains("nothing public"),
        "{last}"
    );
    for call in ["stage", "guard", "census"] {
        assert_eq!(w.count(call), 1, "{call}: {:?}", w.calls);
    }
    assert_eq!(w.count("build"), 0, "{:?}", w.calls);
    let j = w.journal().unwrap();
    assert!(
        j.has(crate::release::COMMIT_GATES)
            && j.has(crate::release::FIXTURE_GUARD)
            && !j.has(crate::release::CENSUS),
        "{j:?}"
    );

    w.census_finds = false;
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(
        (
            w.count("commit-gates"),
            w.count("stage"),
            w.count("guard"),
            w.count("census")
        ),
        (0, 0, 0, 1),
        "{:?}",
        w.calls
    );
}

// the pure pieces -------------------------------------------------------------------

/// Relative path flags are anchored ONCE, where the release started: the steps and the
/// re-exec as R's own cutter run from the clone's root, where they would name another
/// place. An existing one is canonical; an absolute one is left as given.
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
        ..mac()
    };
    let anchored = opts.anchored(&start);
    let engine = root.path().join("publication").canonicalize().unwrap();
    assert_eq!(anchored.engine.as_deref(), Some(engine.as_path()));
    assert_eq!(
        anchored.release_credentials.as_deref(),
        Some(start.join("creds.toml").as_path()),
        "one that does not exist yet is joined, for step 1 to refuse by its full name"
    );
    assert_eq!(
        anchored.linux.artifacts.as_deref(),
        Some(Path::new("/abs/linux"))
    );
    let args: Vec<std::ffi::OsString> = [
        "release",
        "--engine",
        "../../publication",
        "--release-credentials",
        "creds.toml",
        "--mac-only",
    ]
    .iter()
    .map(Into::into)
    .collect();
    let again = anchored.anchored_args(args);
    let want: Vec<std::ffi::OsString> = vec![
        "release".into(),
        "--engine".into(),
        engine.into_os_string(),
        "--release-credentials".into(),
        start.join("creds.toml").into_os_string(),
        "--mac-only".into(),
    ];
    assert_eq!(again, want);
}

#[test]
fn estimates_are_the_median_of_the_last_five_runs() {
    let tsv = "1\t0.1.0\tverify\t100\n2\t0.1.0\tverify\t900\n3\t0.2.0\tverify\t300\n\
               4\t0.3.0\tverify\t200\n5\t0.4.0\tverify\t50\n6\t0.5.0\tverify\t400\n\
               7\t0.5.0\tcut\t60\nnot a row\n";
    let est = crate::release::estimates(tsv);
    assert_eq!(est.get("verify"), Some(&300), "of 900 300 200 50 400");
    assert_eq!(est.get("cut"), Some(&60));
    assert_eq!(crate::release::fmt_dur(42), "42s");
    assert_eq!(crate::release::fmt_dur(185), "3m05s");
    assert_eq!(crate::release::fmt_dur(9480), "2h38m");
}

#[test]
fn a_public_version_is_major_minor_zero() {
    use crate::release::public_version;
    assert_eq!(public_version("0.101.0"), Some((0, 101)));
    for bad in ["0.101", "0.101.1", "0.101.0-dev", "v0.1.0", "0..0", ""] {
        assert_eq!(public_version(bad), None, "{bad}");
    }
}

// (i) build once ---------------------------------------------------------------------

/// A RESUME NEVER BUILDS AGAIN. A release stopped after its build — at `verify release`,
/// which refused, and then at the cut, after its claim — re-runs every later step and
/// never the build or the reservation: `verify release` judges the bundle the build
/// made (the same app, the same executable digest), and the cut claims the number those
/// bytes carry.
#[test]
fn a_resumed_release_never_builds_again() {
    let mut w = World::new();
    w.verify_passes = false;
    w.verify_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 6/11 verify"), "{last}");
    let built = w.journal().unwrap().artifact.unwrap();
    assert_eq!((w.count("reserve"), w.count("build")), (1, 1));

    w.verify_passes = true;
    w.verify_code = 0;
    w.cut_finishes = false;
    w.cut_code = 1;
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}");
    assert!(w.step_line(Step::Build).contains(" skip "), "{:#?}", w.out);
    assert!(w.said(&format!(
        "        runs: {}",
        crate::release::verify_command(&built.app.display().to_string())
    )));
    assert_eq!(
        w.journal().unwrap().artifact.unwrap(),
        built,
        "the same bytes"
    );
    assert_eq!(
        w.cut.as_ref().unwrap().build,
        built.build,
        "claimed as built"
    );

    w.cut_finishes = true;
    w.cut_code = 0;
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert!(
        last.starts_with(&format!("RELEASED aterm 0.6.0 (build {})", built.build)),
        "{last}"
    );
    assert_eq!(
        (w.count("reserve"), w.count("build"), w.count("verify")),
        (1, 1, 2),
        "built once, verified again only after its refusal: {:?}",
        w.calls
    );
}

/// `verify release` decides step 6 by its exit alone: a FAIL (1) refuses naming the
/// command that reruns it; any other exit (3: a check could not run or was skipped; 2: a
/// refused command line) is a resumable stop, never a pass; and a pass whose record
/// cannot be read is a resumable stop too.
#[test]
fn the_verify_step_refuses_on_any_exit_but_a_recorded_pass() {
    for (code, passes, exit, why) in [
        (
            1,
            false,
            1,
            "`verify release` failed for ccccccccc (exit 1)",
        ),
        (
            3,
            false,
            3,
            "COULD NOT RUN — `verify release` could not run every check for ccccccccc (exit 3",
        ),
        (
            2,
            false,
            3,
            "COULD NOT RUN — `verify release` could not run every check for ccccccccc (exit 2",
        ),
        (
            0,
            false,
            3,
            "COULD NOT RUN — `verify release` passed for ccccccccc, and its record cannot be read",
        ),
    ] {
        let mut w = World::new();
        w.verify_code = code;
        w.verify_passes = passes;
        let (got, last) = w.run(bump());
        assert_eq!(got, exit, "{last}");
        assert!(
            last.starts_with(&format!("REFUSED at 6/11 verify: {why}")),
            "{last}"
        );
        if code != 0 {
            assert!(
                last.contains("targo --unverified run -q -p aterm-verify -- release --app /cache/"),
                "the rerun command is named: {last}"
            );
        }
        assert_eq!(w.count("publish"), 0, "nothing public: {:?}", w.calls);
    }
}

/// A PASS FROM A CLONE THAT LEFT R IS NO PASS: the backend records nothing, the step
/// stops resumably before anything is staged, and the next run verifies again.
#[test]
fn a_verify_run_the_clone_moved_under_records_nothing_and_runs_again() {
    let mut w = World::new();
    w.verify_moves = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert!(
        last.starts_with(
            "REFUSED at 6/11 verify: COULD NOT RUN — the clone moved during `verify release`"
        ),
        "{last}"
    );
    assert!(w.verified.is_empty(), "{:?}", w.verified);
    assert_eq!(w.count("publish"), 0, "nothing public: {:?}", w.calls);

    w.verify_moves = false;
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert_eq!(w.count("verify"), 2, "{:?}", w.calls);
}

/// STEP 7 AS AN AGENT RUNS IT: the real backend runs the release's own `targo` —
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
    // The stand-in: says how it was run, then leaves the clone as `mode` asks.
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

    // A pass that dirtied the clone, or moved it off R's tree, is no pass.
    for how in ["dirty", "move"] {
        set("0", how);
        let refused =
            Backend::verify(&mut real, &cand, &prover, &artifact).expect_err("the clone moved");
        assert!(refused.could_not_run, "{refused:?}");
        assert!(
            refused
                .why
                .starts_with("the clone moved during `verify release`"),
            "{refused:?}"
        );
        assert!(!record(&r), "{how}: nothing is recorded");
        let _ = std::fs::remove_file(repo.join("stray.txt"));
        assert!(
            git.git(&["checkout", "-q", "--detach", &r])
                .unwrap()
                .success()
        );
    }

    // A clone that is not clean as the step starts never runs the checks.
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

/// A REFUSED RELEASE BURNS NO BUILD NUMBER. The number is reserved and built before
/// anything is claimed: a release that refuses anywhere before its cut — here `verify
/// release` fails — has pushed nothing to the ledger. When another release claims past
/// the reservation meanwhile, the next run reserves anew and builds and verifies those
/// new bytes (the old ones carry a number that is gone), and the cut claims the new
/// number.
#[test]
fn a_refused_release_burns_no_build_number() {
    let mut w = World::new();
    w.verify_passes = false;
    w.verify_code = 1;
    let tail = w.tail;
    let (code, _) = w.run(bump());
    assert_eq!(code, 1);
    assert_eq!(w.tail, tail, "nothing claimed: the ledger is untouched");
    assert!(
        !w.calls.iter().any(|c| c.starts_with("cut ")),
        "{:?}",
        w.calls
    );
    let first = w.journal().unwrap().reserved.unwrap();
    assert!(first.build > tail);

    // A peer's release claims past the reservation while this one is stopped.
    w.tail = first.build;
    w.verify_passes = true;
    w.verify_code = 0;
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        w.said(&format!(
            "its reserved build {} is at or below origin's ledger tail {} — another release \
             claimed past it (nothing was claimed here, nothing burned) — reserving anew",
            first.build, first.build
        )),
        "{:#?}",
        w.out
    );
    let second = w.journal().unwrap().reserved.unwrap();
    assert!(second.build > first.build, "{second:?}");
    assert_eq!((w.count("reserve"), w.count("build")), (2, 2));
    assert_eq!(w.journal().unwrap().artifact.unwrap().build, second.build);
    assert!(
        last.starts_with(&format!("RELEASED aterm 0.6.0 (build {})", second.build)),
        "{last}"
    );
}

/// A reservation the ledger reaches while the release runs is never re-numbered — its
/// bytes carry it — and never claimed: overtaken while `verify release` judged it, the
/// release stops there saying so; overtaken as the cut starts, the cut refuses before
/// its claim. Either way nothing was burned, and the next run reserves anew, builds and
/// verifies those bytes, and the cut claims their number.
#[test]
fn an_overtaken_reservation_is_built_again_never_renumbered() {
    // overtaken while `verify release` ran
    let mut w = World::new();
    w.after_verify = Some(|w| {
        let reserved = w.journal().unwrap().reserved.unwrap();
        w.tail = reserved.build;
    });
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    let stale = w.journal().unwrap().reserved.unwrap();
    assert!(
        last.starts_with(&format!(
            "REFUSED at 6/11 verify: COULD NOT RUN — origin's ledger reached the reservation \
             while it was verified: its reserved build {} is at or below origin's ledger tail \
             {} — another release claimed past it",
            stale.build, stale.build
        )),
        "{last}"
    );
    assert!(
        !w.calls.iter().any(|c| c.starts_with("cut ")),
        "{:?}",
        w.calls
    );
    w.after_verify = None;
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    let fresh = w.journal().unwrap().reserved.unwrap();
    assert!(fresh.build > stale.build);
    assert_eq!((w.count("reserve"), w.count("build")), (2, 2));
    assert_eq!(w.cut.as_ref().unwrap().build, fresh.build);

    // overtaken as the cut starts
    let mut w = World::new();
    w.overtaken_at_cut = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 9/11 cut"), "{last}");
    let stale = w.journal().unwrap().reserved.unwrap();
    assert!(
        w.calls
            .contains(&format!("cut refused: {} is stale", stale.build)),
        "{:?}",
        w.calls
    );
    assert!(w.cut.is_none(), "nothing claimed");
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(w.step_line(Step::Build).contains(" ok "), "{:#?}", w.out);
    assert!(w.step_line(Step::Verify).contains(" ok "), "{:#?}", w.out);
    let fresh = w.journal().unwrap().reserved.unwrap();
    assert!(fresh.build > stale.build);
    assert_eq!(w.cut.as_ref().unwrap().build, fresh.build);
}

/// A CLAIM THAT LANDED WITH NO CUT JOURNAL is this clone's own, not another release's:
/// origin's tail is the reservation's line and main holds its release commit, but the
/// cut stopped between its push and its journal. The re-run says exactly that — the
/// number is spent, nothing was signed or published under it — and reserves anew,
/// builds and verifies, and the cut claims the fresh number.
#[test]
fn a_claim_that_landed_unjournaled_is_named_as_this_clones() {
    let mut w = World::new();
    w.claim_lands_unjournaled = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 9/11 cut"), "{last}");
    assert!(w.cut.is_none(), "no cut journal");
    let spent = w.journal().unwrap().reserved.unwrap();
    assert_eq!(w.tail, spent.build, "the claim landed");

    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert!(
        w.said(&format!(
            "its reserved build {} was CLAIMED from this clone — origin/main holds its release \
             commit",
            spent.build
        )) && w.said(&format!(
            "build {} is spent, a gap in the ledger — reserving anew",
            spent.build
        )),
        "{:#?}",
        w.out
    );
    assert!(
        !w.said("another release claimed past it"),
        "our own claim is not another release's: {:#?}",
        w.out
    );
    let fresh = w.journal().unwrap().reserved.unwrap();
    assert!(fresh.build > spent.build);
    assert_eq!(w.cut.as_ref().unwrap().build, fresh.build);
}

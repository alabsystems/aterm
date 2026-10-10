// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship release`'s step machine ([`crate::release`]) driven against a fake world: a
//! scratch release directory (the journal and the run claim are real files), and a
//! [`Backend`] whose git, engine, receipts, children, clock and owner's aterm are in
//! memory. What the machine owns is what these tests hold it to: the order, the
//! skip-by-facts rule, the resume after a stop at each step, the final line's shape,
//! the dry run, the owner ask while the long steps run, the READY candidate kept, one
//! run per clone and the re-exec guard.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::provision::{AskWords, Audited, Finding};
use crate::release::{
    AfterCut, Answer, Artifact, Backend, Candidate, CutState, EngineRows, Facts, JOURNAL_FILE,
    Journal, Prover, RUNNING_CHILD_FILE, RUNNING_FILE, Ran, Refusal, ReleaseOptions, Reservation,
    Step, run_with,
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
    trusted: BTreeSet<String>,
    /// The executable digests a MEASURE receipt says `measured yes` about.
    measured: BTreeSet<String>,
    /// origin's ledger tail.
    tail: u64,
    /// The release cache: key → (its build number, its executable digest, its app).
    artifacts: BTreeMap<String, (u64, String, PathBuf)>,
    /// The build fails.
    build_fails: bool,
    /// A peer's release claims past the reservation as the cut starts — after the
    /// release's last look, before the cut's own claim.
    overtaken_at_cut: bool,
    /// The cut's claim LANDS (main takes the release commit, the ledger its line) and
    /// the cut then fails before it journals anything.
    claim_lands_unjournaled: bool,
    // behaviours
    machine_refusals: Vec<Refusal>,
    /// The preflight's own blocks (the receipts are the machine's to add).
    blocks: Vec<(bool, Refusal)>,
    audits: VecDeque<Vec<Finding>>,
    trust_code: i32,
    trust_verifies: bool,
    pending: Vec<String>,
    measure_code: i32,
    measure_measures: bool,
    measure_starved: bool,
    stage_stages: bool,
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
    /// `publish/post-promote --latest`'s exit once the shape passed, read as the real
    /// backend reads it (`release_steps::site_outcome`).
    site_code: i32,
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
    /// The lock is busy when the release takes it again after the owner's wait.
    relock_busy: bool,
    /// The signal a step's child was ended by, once one arrived.
    signal: Option<i32>,
    /// The step whose child a signal ends.
    signal_at: Option<&'static str>,
    /// What the world does once the MEASURE run ends.
    after_measure: Option<fn(&mut World)>,
    /// What the world does while the release next sleeps (an owner wait).
    after_sleep: Option<fn(&mut World)>,
    /// Tags in this clone only (a leftover).
    local_tags: BTreeSet<String>,
    /// The real backend's spawn path and interrupt, for the trust step's child: that
    /// child is `/bin/sh -c <script> sh <dir>`, run as a step's child really is.
    real: Option<(Real, String, PathBuf)>,
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
            trusted: BTreeSet::new(),
            measured: BTreeSet::new(),
            tail: 1_791_500_000,
            artifacts: BTreeMap::new(),
            build_fails: false,
            overtaken_at_cut: false,
            claim_lands_unjournaled: false,
            machine_refusals: Vec::new(),
            blocks: Vec::new(),
            audits: VecDeque::new(),
            trust_code: 0,
            trust_verifies: true,
            pending: Vec::new(),
            measure_code: 0,
            measure_measures: true,
            measure_starved: false,
            stage_stages: true,
            cut_code: 0,
            cut_finishes: true,
            cut_claims: true,
            latest_lags: false,
            stage_panics: false,
            after_cut: Ok("the release's shape checked".into()),
            site_code: 0,
            own_cutter: true,
            asks: Vec::new(),
            resolved: Vec::new(),
            withdrawn: Vec::new(),
            undelivered: 0,
            free: 500,
            cut_gate_refusals: Vec::new(),
            judge_down: false,
            stage_sync_fails: false,
            relock_busy: false,
            signal: None,
            signal_at: None,
            after_measure: None,
            after_sleep: None,
            local_tags: BTreeSet::new(),
            real: None,
        }
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
        Ok(prover())
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
        // a signal as the move's child ends: the move itself is done
        self.interrupt_at("move");
        Ok(())
    }
    fn own_cutter(&mut self, _commit: &str) -> Result<String, String> {
        if self.own_cutter {
            Ok("its own".into())
        } else {
            Err("built from another commit".into())
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
        let measured = match artifact.filter(|_| built.yes) {
            None => Answer::no("its bundle is not built yet (step 7)"),
            Some(a) => yes(&self.measured, "MEASURED", &a.macho_sha256),
        };
        Facts {
            cut: self.cut.clone(),
            engine: self.engine_rows(Some(&c.commit)),
            attested: self.attested.as_deref() == Some(c.commit.as_str()),
            trusted: yes(&self.trusted, "VERIFIED", &c.commit),
            built,
            measured,
        }
    }
    fn disk(&mut self) -> (Option<u64>, bool) {
        (Some(self.free), true)
    }
    fn preflight(&mut self, _c: &Candidate, dry: bool, again: bool) -> Vec<(bool, Refusal)> {
        self.calls.push(
            match (dry, again) {
                (true, _) => "preflight-dry",
                (false, true) => "preflight-again",
                (false, false) => "preflight",
            }
            .into(),
        );
        self.blocks.clone()
    }
    fn cut_gates(&mut self, _c: &Candidate, _creds: &Path) -> Vec<Refusal> {
        self.calls.push("cut-gates".into());
        self.cut_gate_refusals.clone()
    }
    fn fixture_guard(&mut self, _p: &Prover) -> Result<String, Refusal> {
        self.calls.push("guard".into());
        Ok("the handoff fixture guard passes (3 test(s))".into())
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
    fn trust_pending(&mut self, _c: &Candidate, _p: &Prover) -> Vec<String> {
        self.pending.clone()
    }
    fn trust_lane(&mut self, _p: &Prover, remeasure: bool) -> Ran {
        self.calls.push(
            if remeasure {
                "trust --remeasure"
            } else {
                "trust"
            }
            .into(),
        );
        if self.interrupt_at("trust") {
            return ran(-1);
        }
        if let Some((real, script, dir)) = &mut self.real {
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.args(["-c", script.as_str(), "sh"]).arg(dir.as_path());
            return real.logged(Step::Trust, &mut cmd);
        }
        if self.trust_verifies {
            self.trusted.insert(self.head.clone());
        }
        ran(self.trust_code)
    }
    fn lock(&mut self, _caller: &str) -> Result<String, Refusal> {
        self.calls.push("lock".into());
        // a signal while it waits in line: the wait stops, nothing taken
        if self.interrupt_at("lock") {
            return Err(Refusal::new(
                "stopped waiting for this machine's gate after 3 s — this waiter was interrupted",
                "wait",
            )
            .could_not_run());
        }
        if self.relock_busy && self.calls.iter().any(|c| c.starts_with("sleep")) {
            return Err(
                Refusal::new("the gate lock is held by a peer's gate", "wait").could_not_run(),
            );
        }
        Ok("held".into())
    }
    fn unlock(&mut self) {
        self.calls.push("unlock".into());
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
        _c: &Candidate,
        reserved: &Reservation,
        _p: &Prover,
    ) -> Result<Artifact, Refusal> {
        self.calls.push("build".into());
        if self.build_fails {
            return Err(Refusal::new(
                "the build did not finish (ship build exited 1)",
                "read its log",
            ));
        }
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
    }
    fn measure(&mut self, _p: &Prover, app: &Path) -> (Ran, bool) {
        self.calls.push("measure".into());
        if self.measure_measures {
            let macho = self
                .artifacts
                .values()
                .find(|(_, _, a)| a == app)
                .map(|(_, macho, _)| macho.clone())
                .expect("MEASURE is handed a bundle the build made");
            self.measured.insert(macho);
        }
        if let Some(then) = self.after_measure {
            then(self);
        }
        (ran(self.measure_code), self.measure_starved)
    }
    fn stage(&mut self, c: &Candidate, _p: &Prover) -> Result<Ran, Refusal> {
        self.calls.push("stage".into());
        assert!(!self.stage_panics, "a bug in the release");
        if self.stage_stages {
            self.attested = Some(c.commit.clone());
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
        self.newest = Some(r);
        Ok(ran(0))
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
        let site = crate::release_steps::site_outcome(ran(self.site_code))?;
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
        .filter(|c| {
            !matches!(*c, "audit" | "lease" | "unlock" | "cut-gates") && !c.starts_with("ls-remote")
        })
        .collect();
    assert_eq!(
        order,
        [
            "sync",
            "bump",
            "move cccc",
            "preflight",
            "guard",
            "trust",
            "lock",
            "reserve",
            "build",
            "measure",
            "preflight-again",
            "stage",
            "publish",
            "sync",
            "cut --mac-only",
            "after-cut 0.6.0",
        ]
    );
    for step in Step::ALL {
        assert!(w.step_line(step).contains(" ok "), "{step:?}: {:#?}", w.out);
    }
    let j = w.journal().unwrap();
    assert_eq!((j.version.as_str(), j.commit.as_str()), ("0.6.0", BUMP));
    assert!(j.bumped && j.finished() && j.has("fixture-guard") && j.has("after-cut"));
    assert!(
        !w.release_dir().join(RUNNING_FILE).exists(),
        "the claim goes with the run"
    );
    // every step that ran is measured for the next ETA
    assert!(w.times.contains("\ttrust\t") && w.times.contains("\tcut\t"));
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
        Step::PreflightEarly,
        Step::Trust,
        Step::Lock,
        Step::Measure,
        Step::Preflight,
        Step::Stage,
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
        "bump", "trust", "measure", "stage", "publish", "lock", "guard",
    ] {
        assert_eq!(w.count(never), 0, "{never}: {:?}", w.calls);
    }
    assert!(!w.calls.iter().any(|c| c.starts_with("cut")));
}

// (b) resume after a stop at each step --------------------------------------------

#[test]
fn a_run_stopped_at_any_step_resumes_there_and_repeats_nothing_done() {
    type Break = fn(&mut World);
    type Mend = fn(&mut World);
    let cases: [(Step, Break, Mend); 6] = [
        (
            Step::Trust,
            |w| w.trust_verifies = false,
            |w| w.trust_verifies = true,
        ),
        (
            Step::Measure,
            |w| {
                w.measure_measures = false;
                w.measure_code = 3;
                w.measure_starved = true;
            },
            |w| {
                w.measure_measures = true;
                w.measure_code = 0;
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
            ("trust", Step::Trust),
            ("measure", Step::Measure),
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
    // past the claim, the preflights are past too
    assert_eq!(w.count("preflight"), 0);
}

// (c) the final line --------------------------------------------------------------

#[test]
fn a_refusal_is_one_line_naming_the_step_why_what_is_public_and_the_remedy() {
    let mut w = World::new();
    w.trust_verifies = false;
    w.trust_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with("REFUSED at 5/14 trust: the whole-tree trust lane did not pass (exit 1; ")
            && last.contains(" — nothing public. fix the library on main"),
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
        last.starts_with("REFUSED at 12/14 cut: the cut did not finish (ship cut exited 1)")
            && last.contains(
                " — the source of v0.6.0 is public (alabsystems e1e1e1e1e), no build claimed. "
            )
            && last.ends_with("it starts the cut again"),
        "{last}"
    );

    // a step that decided nothing is a resumable stop, exit 3, the candidate kept
    let mut w = World::new();
    w.trust_verifies = false;
    w.trust_code = 2;
    w.pending = vec!["aterm-render".into()];
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert!(
        last.starts_with(
            "REFUSED at 5/14 trust: COULD NOT RUN — the trust lane decided nothing \
                          about aterm-render"
        ) && last.contains("targo --unverified ship release again"),
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
            "REFUSED at 10/14 stage: targo --unverified ship release stopped unexpectedly — \
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
    assert_eq!(w.count("trust") + w.count("measure"), 0, "{:?}", w.calls);
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
            "DRY RUN aterm 0.6.0 from bbbbbbbbb: a real run would refuse at 1/14 \
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
        "sync", "bump", "trust", "lock", "measure", "stage", "publish", "guard",
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
        real.starts_with("REFUSED at 1/14 checks: only 4 GiB free on /c"),
        "{real}"
    );
}

#[test]
fn a_preflight_block_is_met_at_minute_one_by_a_dry_bump_and_a_real_run_alike() {
    let block = || {
        vec![(
            false,
            Refusal::new(
                "handoff fixtures — the handoff fixtures of v0.5.0 are not checked in",
                "generate them at tag v0.5.0",
            ),
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
        dry.contains("would refuse at 4/14 preflight-early: the preflight BLOCKED (1 finding(s))"),
        "{dry}"
    );
    assert!(w.said("| handoff fixtures — the handoff fixtures of v0.5.0 are not checked in"));
    assert!(
        w.step_line(Step::PreflightEarly)
            .contains("BLOCKED (1 finding(s), above)")
    );
    let mut w = World::new();
    w.blocks = block();
    let (code, real) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        real.starts_with("REFUSED at 4/14 preflight-early: the preflight BLOCKED (1 finding(s))"),
        "{real}"
    );
    assert_eq!(
        w.count("trust"),
        0,
        "refused before the hours of the trust lane"
    );
    // the receipts alone (steps 5-8 answer them) never block at minute one
    let mut w = World::new();
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
    assert!(
        w.step_line(Step::PreflightEarly)
            .contains("no block but the 2 receipt block(s) steps 5-8 answer"),
        "{:#?}",
        w.out
    );
}

// (e) the owner, asked once, while the long steps run ---------------------------------

#[test]
fn an_owner_gap_is_asked_once_the_long_steps_run_and_the_release_waits_then_goes_on() {
    let mut w = World::new();
    // open for the audits at 4, before 5, 7 and 8, at 9 and its first re-audit; closed after
    w.audits = VecDeque::from([
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        passes(),
    ]);
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.asks, ["release.notary"], "asked exactly once");
    assert_eq!(w.resolved, ["release.notary"], "resolved when it closed");
    let trust = w.calls.iter().position(|c| c == "trust").unwrap();
    let measure = w.calls.iter().position(|c| c == "measure").unwrap();
    let waited = w.calls.iter().position(|c| c == "sleep 600").unwrap();
    let stage = w.calls.iter().position(|c| c == "stage").unwrap();
    assert!(
        trust < waited && measure < waited && waited < stage,
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
        waiting[0].contains("is READY (trusted, built once, measured); nothing public"),
        "{}",
        waiting[0]
    );
    // the lock is let go while it waits, and taken again
    let unlock = w.calls.iter().position(|c| c == "unlock").unwrap();
    let relock = w.calls.iter().rposition(|c| c == "lock").unwrap();
    assert!(unlock < waited && waited < relock, "{:?}", w.calls);
    assert!(w.journal().unwrap().asks.is_empty());
    // the owner's wait is not the step's own time
    let preflight_secs: Vec<u64> = w
        .times
        .lines()
        .filter(|l| l.contains("\tpreflight\t"))
        .filter_map(|l| l.rsplit('\t').next()?.parse().ok())
        .collect();
    assert!(
        preflight_secs.iter().all(|s| *s < 600),
        "{preflight_secs:?}"
    );
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
        last.starts_with("REFUSED at 4/14 preflight-early: this machine's provisioning"),
        "{last}"
    );
    assert_eq!(w.count("trust"), 0);

    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap()]);
    let (code, last) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 0, "{last}");
    assert!(
        last.contains("and wait for the owner at 9/14 preflight"),
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
        last.starts_with("REFUSED at 9/14 preflight: COULD NOT RUN — this machine's provisioning")
            && last.ends_with("(the candidate is kept)"),
        "{last}"
    );
    assert_eq!(
        w.count("trust"),
        1,
        "the long steps ran before it was needed"
    );
}

// (f) the READY candidate is kept --------------------------------------------------

#[test]
fn a_bare_rerun_keeps_the_ready_candidate_and_commit_replaces_it_out_loud() {
    let mut w = World::new();
    w.audits = VecDeque::from([owner_gap()]);
    // stop it at the wait: a machine refusal there is not reachable, so the owner's
    // gap holds and the fake's sleep is ended by making the next audit refuse
    w.audits = VecDeque::from([
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        vec![Finding {
            label: "x86 slice",
            state: Audited::Gap {
                what: "no x86_64 std".into(),
                fix: "add it".into(),
                owner: false,
            },
        }],
    ]);
    let (code, _) = w.run(bump());
    assert_eq!(code, 1);
    let j = w.journal().unwrap();
    assert_eq!(j.commit, BUMP);
    assert!(
        w.trusted.contains(BUMP) && w.macho().is_some_and(|m| w.measured.contains(&m)),
        "READY"
    );
    // main moves on
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.audits = VecDeque::from([passes()]);
    w.calls.clear();
    let (code, last) = w.run(mac());
    assert_eq!(code, 0, "{last}");
    assert!(w.said("its READY candidate is kept"), "{:#?}", w.out);
    assert_eq!(w.journal().unwrap().commit, BUMP, "never chases main");
    assert_eq!(w.count("trust") + w.count("measure"), 0);

    // --commit replaces an unpublished candidate, saying what it costs
    let mut w = World::new();
    w.audits = VecDeque::from([
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        vec![Finding {
            label: "x86 slice",
            state: Audited::Gap {
                what: "no x86_64 std".into(),
                fix: "add it".into(),
                owner: false,
            },
        }],
    ]);
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
        )) && l.contains("costs a whole-tree Trust lane")),
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
            "REFUSED at 1/14 checks: another targo --unverified ship release is running in this \
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
        last.starts_with("REFUSED at 3/14 commit: this cutter was rebuilt at")
            && last.contains("targo clean --release -p aterm-release"),
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

// (h) what can change while the long steps run, asked again before the stage ----------

/// The disk the trust lane and MEASURE fill, and a rostered peer cutting the version
/// meanwhile, are asked again at step 9: the refusal arrives with nothing public.
#[test]
fn the_disk_and_the_tag_are_asked_again_before_the_stage() {
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
        w.after_measure = Some(change);
        let (code, last) = w.run(bump());
        assert_eq!(code, 1, "{last}");
        assert!(
            last.starts_with("REFUSED at 9/14 preflight: the preflight BLOCKED")
                && last.contains(" — nothing public. "),
            "{last}"
        );
        assert!(w.said(&format!("| {why}")), "{why}: {:#?}", w.out);
        assert_eq!(w.count("stage") + w.count("publish"), 0, "{:?}", w.calls);
    }
}

/// The cut's own read-only pre-claim gates (the release lease another machine holds,
/// a roster or floor the channel moved) refuse at step 9, before anything is public —
/// and a dry run reports them.
#[test]
fn the_cuts_read_only_gates_refuse_before_the_stage_and_a_dry_run_says_so() {
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
        last.starts_with("REFUSED at 9/14 preflight: the preflight BLOCKED (1 finding(s))")
            && last.contains("nothing public"),
        "{last}"
    );
    assert!(w.said("| release lease — the release lock is held by claim abcdef"));
    assert_eq!(w.count("stage"), 0);
    let mut w = World::new();
    w.cut_gate_refusals = held();
    let (code, dry) = w.run(ReleaseOptions {
        dry_run: true,
        ..bump()
    });
    assert_eq!(code, 1);
    assert!(
        dry.contains("would refuse at 9/14 preflight: the preflight BLOCKED"),
        "{dry}"
    );
}

/// A judge that cannot answer is no "no": it stops at minute one as COULD NOT RUN,
/// never after the hours of the lane whose receipt it would then not read either.
#[test]
fn a_judge_that_cannot_answer_stops_at_minute_one() {
    let mut w = World::new();
    w.judge_down = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert!(
        last.starts_with(
            "REFUSED at 4/14 preflight-early: COULD NOT RUN — the cutter's own judge could \
             not answer"
        ),
        "{last}"
    );
    assert_eq!(w.count("trust") + w.count("measure"), 0, "{:?}", w.calls);
}

/// THE RE-CHECK RIGHT BEFORE WHAT GOES PUBLIC. The stage and the publish may each wait
/// for the owner at their provisioning, after step 9 asked; so what the cut would
/// refuse is asked again after that wait, the last thing before `pub stage` and before
/// `pub publish` — and what changed during the wait refuses there, nothing (more) public.
#[test]
fn what_changes_while_the_stage_or_publish_waits_is_asked_again_right_before_it() {
    // every run: the re-check (the tag on origin now, then the cut's gates) is the last
    // thing asked before each
    let mut w = World::new();
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
    for step in ["stage", "publish"] {
        let at = w.calls.iter().position(|c| c == step).unwrap();
        let before: Vec<&str> = w.calls[..at]
            .iter()
            .rev()
            .take(2)
            .map(String::as_str)
            .collect();
        assert_eq!(
            before,
            ["cut-gates", "ls-remote v0.6.0"],
            "{step}: {:?}",
            w.calls
        );
    }

    // the owner is waited on at the stage, and a rostered peer cuts v0.6.0 meanwhile
    let mut w = World::new();
    w.audits = VecDeque::from([
        passes(),
        passes(),
        passes(),
        passes(),
        passes(),
        owner_gap(),
        passes(),
    ]);
    w.after_sleep = Some(|w| {
        w.origin_tags.insert("v0.6.0".into(), NEXT.into());
    });
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(
        last.starts_with(
            "REFUSED at 10/14 stage: asked again right before the stage, the cut would refuse \
             (1 finding(s))"
        ) && last.contains(" — nothing public. "),
        "{last}"
    );
    assert!(
        w.said("| v0.6.0 is on origin now (at ddddddddd)"),
        "{:#?}",
        w.out
    );
    assert_eq!(w.count("sleep 600"), 1, "{:?}", w.calls);
    assert_eq!(w.count("stage") + w.count("publish"), 0, "{:?}", w.calls);

    // the owner is waited on at the publish, and the disk fills meanwhile
    let mut w = World::new();
    w.audits = VecDeque::from([
        passes(),
        passes(),
        passes(),
        passes(),
        passes(),
        passes(),
        owner_gap(),
        passes(),
    ]);
    w.after_sleep = Some(|w| w.free = 4);
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(
        last.starts_with(
            "REFUSED at 11/14 publish: asked again right before the publish, the cut would \
             refuse (1 finding(s))"
        ) && last.contains(" — nothing public. "),
        "{last}"
    );
    assert!(
        w.said("| only 4 GiB free — the cut needs 10"),
        "{:#?}",
        w.out
    );
    assert_eq!(
        (w.count("stage"), w.count("publish")),
        (1, 0),
        "{:?}",
        w.calls
    );
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
            "REFUSED at 1/14 checks: v0.6.0 is already cut (its tag is on origin)",
        ),
        (
            |w| {
                w.local_tags.insert("v0.6.0".into());
            },
            "REFUSED at 1/14 checks: tag v0.6.0 exists in this clone and not on origin — a \
             leftover",
        ),
        (
            |w| {
                w.origin_tags.insert("v0.5.0".into(), NEXT.into());
            },
            "REFUSED at 1/14 checks: bbbbbbbbb does not contain v0.5.0's claim commit ddddddddd",
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

/// A wait for the machine's lock that a signal stops — at step 6, and taken again after
/// a wait for the owner — ends the run in its INTERRUPTED line, exit 130, never a
/// COULD NOT RUN about the lock.
#[test]
fn a_signal_while_waiting_for_the_lock_ends_the_run_interrupted() {
    let mut w = World::new();
    w.signal_at = Some("lock");
    let (code, last) = w.run(bump());
    assert_eq!(code, 130, "{last}");
    assert!(
        last.starts_with("INTERRUPTED at 6/14 lock (by SIGTERM) — nothing public."),
        "{last}"
    );
    assert_eq!(w.count("measure"), 0, "{:?}", w.calls);

    // the relock after the owner's wait at step 9
    let mut w = World::new();
    w.audits = VecDeque::from([
        passes(),
        passes(),
        passes(),
        passes(),
        owner_gap(),
        passes(),
    ]);
    w.after_sleep = Some(|w| w.signal_at = Some("lock"));
    let (code, last) = w.run(bump());
    assert_eq!(code, 130, "{last}");
    assert!(
        last.starts_with("INTERRUPTED at 9/14 preflight (by SIGTERM)"),
        "{last}"
    );
    assert_eq!(w.count("stage"), 0, "{:?}", w.calls);
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
        last.starts_with("INTERRUPTED at 3/14 commit (by SIGTERM)"),
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
        last.starts_with("REFUSED at 10/14 stage: the engine is not a fast-forward")
            && last.contains("audit-rows.jsonl"),
        "{last}"
    );
    assert_eq!(w.count("publish"), 0);
}

// (i) interrupted, never orphaned ------------------------------------------------------

/// A signal ends the step's child; the release ends in ONE line, exit 130, its lock let
/// go and its claim gone, and the same command resumes there.
#[test]
fn an_interrupt_ends_the_run_in_one_line_and_it_resumes() {
    let mut w = World::new();
    w.signal_at = Some("trust");
    let (code, last) = w.run(bump());
    assert_eq!(code, 130, "{last}");
    assert_eq!(
        last,
        "INTERRUPTED at 5/14 trust (by SIGTERM) — nothing public. Run targo --unverified ship \
         release again to resume"
    );
    assert_eq!(w.out.last().unwrap(), &last);
    assert!(!w.release_dir().join(RUNNING_FILE).exists());
    w.signal = None;
    w.signal_at = None;
    w.calls.clear();
    assert_eq!(w.run(mac()).0, 0, "{:#?}", w.out);
    assert_eq!(w.count("bump"), 0);

    // at the cut, the line says the cut resumes, and the lock is let go
    let mut w = World::new();
    w.signal_at = Some("cut");
    let (code, last) = w.run(bump());
    assert_eq!(code, 130);
    assert!(
        last.starts_with(
            "INTERRUPTED at 12/14 cut (by SIGTERM) — build 1791600000 of v0.6.0 is claimed."
        ) && last.contains("The cut stopped with this run; running the command again resumes it."),
        "{last}"
    );
    let locked = w.calls.iter().rposition(|c| c == "lock").unwrap();
    let unlocked = w.calls.iter().rposition(|c| c == "unlock").unwrap();
    assert!(locked < unlocked, "{:?}", w.calls);
}

/// The probe mode of [`a_real_sigterm_reaches_the_steps_group_and_ends_interrupted`]:
/// set, the test is the release itself (its own process, its own signal handlers).
const INTERRUPT_PROBE: &str = "ATERM_RELEASE_INTERRUPT_PROBE";

/// The trust step's child in the probe: a shell (the step's child, its group's leader)
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
/// re-run as the probe) runs its trust step's child through the real spawn path; a
/// SIGTERM sent to the RELEASE alone reaches every member of the step's group (the
/// child and the subshell it left running), the release waits for the step to end, and
/// its last line is `INTERRUPTED …`, exit 130.
#[cfg(unix)]
#[test]
fn a_real_sigterm_reaches_the_steps_group_and_ends_interrupted() {
    use std::process::{Command, Stdio};
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
    let mut probe = Command::new(std::env::current_exe().unwrap())
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
    let sent = Command::new("kill")
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
        "INTERRUPTED at 5/14 trust (by SIGTERM) — nothing public. Run targo --unverified ship \
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
        "pid 888\nstarted Thu Oct  9 01:30:00 2026\nstep trust\n",
    )
    .unwrap();
    w.alive.insert(888, "Thu Oct  9 01:30:00 2026".into());
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with(
            "REFUSED at 1/14 checks: the trust step of a targo --unverified ship release that \
             was killed is still running in this clone (pid 888"
        ) && last.contains("kill -TERM 888 — the step's own process, never its group"),
        "{last}"
    );
    assert!(!last.contains("-- -888"), "{last}");
    // once it has ended, it holds nothing
    w.alive.clear();
    assert_eq!(w.run(bump()).0, 0, "{:#?}", w.out);
}

// (j) the owner's asks: delivered, carried, withdrawn; the lock let go while he is waited on

/// An ask the owner's aterm did not take is raised again at the next audit, and is
/// journaled only once delivered.
#[test]
fn an_ask_that_was_not_delivered_is_raised_again() {
    let mut w = World::new();
    w.undelivered = 1;
    w.audits = VecDeque::from([
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        passes(),
    ]);
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}\n{:#?}", w.out);
    assert_eq!(w.count("ask release.notary"), 2, "{:?}", w.calls);
    assert_eq!(w.asks, ["release.notary"], "delivered once");
    assert_eq!(w.resolved, ["release.notary"]);
    assert!(w.said("could not ask the owner (release.notary): no aterm window answered"));
}

/// A check that got no answer resolves nothing: owner gap, COULD NOT RUN, owner gap is
/// ONE ask, resolved only when the check passes.
#[test]
fn a_check_that_could_not_run_keeps_the_ask_open() {
    let mut w = World::new();
    let unanswered = vec![Finding {
        label: "notary",
        state: Audited::CouldNotRun("Apple's notary service did not answer".into()),
    }];
    w.audits = VecDeque::from([owner_gap(), unanswered, owner_gap(), owner_gap(), passes()]);
    let (code, last) = w.run(bump());
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
    let staged_at = w
        .out
        .iter()
        .position(|l| l.starts_with("[10/14] stage"))
        .unwrap();
    let preflight_at = w
        .out
        .iter()
        .position(|l| l.starts_with("[ 9/14] preflight") && l.contains(" start "))
        .unwrap();
    assert!(
        preflight_at < resolved_at && resolved_at < staged_at,
        "{:#?}",
        w.out
    );
}

/// While the owner is waited on, the machine's lock stays let go: it is taken again
/// once, only when the audit passes — and a lock still busy then is a resumable stop.
#[test]
fn the_owner_wait_holds_no_lock_and_a_busy_lock_after_it_is_a_resumable_stop() {
    let mut w = World::new();
    w.audits = VecDeque::from([
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        passes(),
    ]);
    let (code, last) = w.run(bump());
    assert_eq!(code, 0, "{last}");
    assert_eq!(w.count("sleep 600"), 3, "{:?}", w.calls);
    let first_sleep = w.calls.iter().position(|c| c == "sleep 600").unwrap();
    let last_sleep = w.calls.iter().rposition(|c| c == "sleep 600").unwrap();
    assert!(
        !w.calls[first_sleep..last_sleep].iter().any(|c| c == "lock"),
        "no lock taken between the polls: {:?}",
        w.calls
    );
    assert_eq!(w.count("lock"), 2, "step 6, then once after the wait");

    let mut w = World::new();
    w.relock_busy = true;
    w.audits = VecDeque::from([
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        passes(),
    ]);
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    assert!(
        last.starts_with("REFUSED at 9/14 preflight: COULD NOT RUN — the gate lock is held"),
        "{last}"
    );
}

/// A replaced candidate keeps the asks raised for the machine; `--forget` withdraws
/// them, so no standing row outlives the release it was raised for.
#[test]
fn asks_are_carried_over_a_replaced_candidate_and_withdrawn_by_forget() {
    let mut w = World::new();
    w.audits = VecDeque::from([
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        owner_gap(),
        vec![Finding {
            label: "x86 slice",
            state: Audited::Gap {
                what: "no x86_64 std".into(),
                fix: "add it".into(),
                owner: false,
            },
        }],
    ]);
    let _ = w.run(bump());
    assert_eq!(w.journal().unwrap().asks, ["release.notary"]);
    w.history.push(NEXT.into());
    w.versions.insert(NEXT.into(), "0.6.0".into());
    w.audits = VecDeque::from([
        owner_gap(),
        vec![Finding {
            label: "x86 slice",
            state: Audited::Gap {
                what: "no x86_64 std".into(),
                fix: "add it".into(),
                owner: false,
            },
        }],
    ]);
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

// (k) a claim forgotten, the site, the successor, the re-exec, --forget ---------------

/// NO CLAIM IS HELD TO A PROVER, because no resume compiles. The cut claims bytes the
/// release already built and measured, and its journal (`dist/cut-state.toml`) names
/// them, so a resume signs those bytes whatever prover it runs under. `--forget` around
/// a claimed, unfinished cut therefore drops nothing its resume needs: the run after it
/// resumes the claim with `--resume`, and never reserves, builds or measures again.
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
        (w.count("reserve"), w.count("build"), w.count("measure")),
        (0, 0, 0),
        "{:?}",
        w.calls
    );
}

/// A machine with no site checkout (post-promote exit 3, nothing touched) ends the
/// release: the site is a hand step, journaled and said — in the step, the final line
/// and every later run — never a refusal a re-run here would only repeat.
#[test]
fn a_site_this_machine_cannot_sync_is_a_recorded_hand_step_and_the_release_ends() {
    let mut w = World::new();
    w.site_code = 3;
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
    // the step keeps the hook's own line: where it looked, or what the checkout lacks
    let lacks = "post-promote: site checkout at /s has no executable deploy.sh — not a usable \
                 site checkout; set SITE_DIR or skip with --skip-site";
    let deferred = crate::release_steps::site_outcome(Ran {
        code: 3,
        detail: vec![
            "$ \"publish/post-promote\" \"--latest\"".into(),
            lacks.into(),
        ],
    })
    .unwrap();
    assert!(deferred.site_deferred, "{deferred:?}");
    assert_eq!(
        deferred.note,
        format!(
            "publish/post-promote said: {}",
            &lacks["post-promote: ".len()..]
        )
    );
    // exit 4 (deployed, the live site not agreeing yet) is still a stop to retry
    let mut w = World::new();
    w.site_code = 4;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1);
    assert!(
        last.starts_with("REFUSED at 13/14 after-cut: the site is deployed"),
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
                "REFUSED at 13/14 after-cut: alabsystems/aterm v0.6.0 does NOT carry the \
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
    ] {
        let refused = parse(bad).unwrap_err();
        assert!(refused.starts_with("--forget drops"), "{bad:?}: {refused}");
    }
    assert!(matches!(
        parse(&["release", "--forget", "--verbose", "--engine", "/e"]),
        Ok(crate::cli::Cmd::Release(o)) if o.forget && o.verbose
    ));
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
    let tsv = "1\t0.1.0\ttrust\t100\n2\t0.1.0\ttrust\t900\n3\t0.2.0\ttrust\t300\n\
               4\t0.3.0\ttrust\t200\n5\t0.4.0\ttrust\t50\n6\t0.5.0\ttrust\t400\n\
               7\t0.5.0\tcut\t60\nnot a row\n";
    let est = crate::release::estimates(tsv);
    assert_eq!(est.get("trust"), Some(&300), "of 900 300 200 50 400");
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

/// A RESUME NEVER BUILDS AGAIN. A release stopped after its build — at MEASURE, which
/// refused, and then at the cut, after its claim — re-runs every later step and never
/// the build or the reservation: MEASURE judges the bundle the build made (the same
/// app, the same executable digest), and the cut claims the number those bytes carry.
#[test]
fn a_resumed_release_never_builds_again() {
    let mut w = World::new();
    w.measure_measures = false;
    w.measure_code = 1;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 8/14 measure"), "{last}");
    let built = w.journal().unwrap().artifact.unwrap();
    assert_eq!((w.count("reserve"), w.count("build")), (1, 1));

    w.measure_measures = true;
    w.measure_code = 0;
    w.cut_finishes = false;
    w.cut_code = 1;
    let (code, last) = w.run(mac());
    assert_eq!(code, 1, "{last}");
    assert!(w.step_line(Step::Build).contains(" skip "), "{:#?}", w.out);
    assert!(w.said(&format!("        judges: {}", built.app.display())));
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
        (w.count("reserve"), w.count("build"), w.count("measure")),
        (1, 1, 2),
        "built once, measured again only after its refusal: {:?}",
        w.calls
    );
}

/// A REFUSED RELEASE BURNS NO BUILD NUMBER. The number is reserved and built before
/// anything is claimed: a release that refuses anywhere before its cut — here MEASURE
/// says the bytes did not measure — has pushed nothing to the ledger. When another
/// release claims past the reservation meanwhile, the next run reserves anew and
/// builds and measures those new bytes (the old ones carry a number that is gone), and
/// the cut claims the new number.
#[test]
fn a_refused_release_burns_no_build_number() {
    let mut w = World::new();
    w.measure_measures = false;
    w.measure_code = 1;
    let tail = w.tail;
    let (code, _) = w.run(bump());
    assert_eq!(code, 1);
    assert_eq!(w.tail, tail, "nothing claimed: the ledger is untouched");
    assert!(
        !w.calls.iter().any(|c| c.starts_with("cut")),
        "{:?}",
        w.calls
    );
    let first = w.journal().unwrap().reserved.unwrap();
    assert!(first.build > tail);

    // A peer's release claims past the reservation while this one is stopped.
    w.tail = first.build;
    w.measure_measures = true;
    w.measure_code = 0;
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
/// bytes carry it — and never claimed: overtaken while MEASURE judged it, the release
/// stops there saying so; overtaken as the cut starts, the cut refuses before its
/// claim. Either way nothing was burned, and the next run reserves anew, builds and
/// measures those bytes, and the cut claims their number.
#[test]
fn an_overtaken_reservation_is_built_again_never_renumbered() {
    // overtaken while MEASURE ran
    let mut w = World::new();
    w.after_measure = Some(|w| {
        let reserved = w.journal().unwrap().reserved.unwrap();
        w.tail = reserved.build;
    });
    let (code, last) = w.run(bump());
    assert_eq!(code, 3, "{last}");
    let stale = w.journal().unwrap().reserved.unwrap();
    assert!(
        last.starts_with(&format!(
            "REFUSED at 8/14 measure: COULD NOT RUN — origin's ledger reached the reservation \
             while it was measured: its reserved build {} is at or below origin's ledger tail \
             {} — another release claimed past it",
            stale.build, stale.build
        )),
        "{last}"
    );
    assert!(
        !w.calls.iter().any(|c| c.starts_with("cut")),
        "{:?}",
        w.calls
    );
    w.after_measure = None;
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
    assert!(last.starts_with("REFUSED at 12/14 cut"), "{last}");
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
    assert!(w.step_line(Step::Measure).contains(" ok "), "{:#?}", w.out);
    let fresh = w.journal().unwrap().reserved.unwrap();
    assert!(fresh.build > stale.build);
    assert_eq!(w.cut.as_ref().unwrap().build, fresh.build);
}

/// A CLAIM THAT LANDED WITH NO CUT JOURNAL is this clone's own, not another release's:
/// origin's tail is the reservation's line and main holds its release commit, but the
/// cut stopped between its push and its journal. The re-run says exactly that — the
/// number is spent, nothing was signed or published under it — and reserves anew,
/// builds and measures, and the cut claims the fresh number.
#[test]
fn a_claim_that_landed_unjournaled_is_named_as_this_clones() {
    let mut w = World::new();
    w.claim_lands_unjournaled = true;
    let (code, last) = w.run(bump());
    assert_eq!(code, 1, "{last}");
    assert!(last.starts_with("REFUSED at 12/14 cut"), "{last}");
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

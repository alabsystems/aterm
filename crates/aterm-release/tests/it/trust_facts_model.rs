// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for `TrustFactsReuse` (`trust_facts_reuse_model`).

use crate::trust_facts::{
    self, Crate, Decision, Facts, Measured, Pace, Prover, RATCHET, TrustCmd, Update, facts_for,
    testing,
};

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use aterm_models_release::trust_facts_reuse_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;

use crate::ledger::testing::Git;

const DRV: &str = "targo 1.99.0-dev (321aaeda7 2026-09-17) (targo 0.1.0)";
const OTHER: &str = "targo 1.99.0-dev (bbbbbbbbb 2026-09-24) (targo 0.1.0)";
const TRUSTC: &str = "321aaeda75478038420d97830f14724c33c8dc39";
const REASON: &str = "the new prover proves one fewer (trust change, owner-approved)";

/// A starved run: CPU/wall 0.4 at load 30 on 14 cores.
const STARVED: Pace = Pace {
    cpu_ms: 4,
    wall_ms: 10,
    load: Some(3000),
    cores: 14,
    threads: None,
};

/// A loaded run that was not starved: CPU/wall 0.9 at load 10 on 14 cores.
const UNSTARVED: Pace = Pace {
    cpu_ms: 9,
    wall_ms: 10,
    load: Some(1000),
    cores: 14,
    threads: None,
};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = crate::ledger::testing::git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

struct Harness {
    root: PathBuf,
    work: PathBuf,
    provers: [Prover; 2],
    model: Model,
    state: State,
    src_of: BTreeMap<String, i64>,
    steps: usize,
    /// The word the judge gave the last run.
    word: &'static str,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Harness {
    fn new(label: &str) -> Harness {
        let root = std::env::temp_dir().join(format!(
            "aterm-trust-facts-model-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        crate::ledger::testing::init(
            &work,
            &[
                ("user.name", "facts model"),
                ("user.email", "facts-model@aterm.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        let provers = [
            testing::prover(&root, "a", TRUSTC),
            testing::prover(&root, "b", TRUSTC),
        ];
        assert_ne!(
            provers[0].identity, provers[1].identity,
            "two builds of one trustc commit are two provers"
        );
        let model = trust_facts_reuse_model();
        let state = model.init_state();
        let mut h = Harness {
            root,
            work,
            provers,
            model,
            state,
            src_of: BTreeMap::new(),
            steps: 0,
            word: "",
        };
        let head = h.commit(&[
            ("crates/alpha/src/lib.rs", "v0"),
            (RATCHET, &floors(0, 0, 0)),
        ]);
        h.src_of.insert(head, 0);
        h
    }

    fn cli(&self) -> Git {
        Git::new(&self.work)
    }

    fn store(&self) -> PathBuf {
        self.work.join(".git/aterm-verify/receipts")
    }

    fn commit(&self, files: &[(&str, &str)]) -> String {
        for (path, body) in files {
            let full = self.work.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, body).unwrap();
        }
        if !files.is_empty() {
            let mut add = vec!["add", "--"];
            add.extend(files.iter().map(|(path, _)| *path));
            git(&self.work, &add);
        }
        git(&self.work, &["commit", "-q", "--allow-empty", "-m", "step"]);
        git(&self.work, &["rev-parse", "HEAD"])
    }

    fn head(&self) -> String {
        git(&self.work, &["rev-parse", "HEAD"])
    }

    fn prover(&self) -> &Prover {
        &self.provers[usize::try_from(self.state["prv"]).unwrap()]
    }

    fn floor_at(&self, commit: &str) -> (i64, i64, i64) {
        let ratchet = trust_facts::ratchet_at(&self.cli(), commit)
            .unwrap()
            .unwrap();
        let row: Vec<&str> = ratchet.trim_end().split('\t').collect();
        let other = match row[3] {
            DRV => 0,
            OTHER => 1,
            p => panic!("a prover column the harness never writes: {p}"),
        };
        let why = match row.get(4) {
            None => 0,
            Some(&REASON) => 1,
            Some(w) => panic!("a reason the harness never writes: {w}"),
        };
        (row[1].parse().unwrap(), other, why)
    }

    fn with(&self, fields: &[(&'static str, i64)]) -> State {
        let mut next = self.state.clone();
        for &(name, value) in fields {
            assert!(next.contains_key(name), "no model variable {name}");
            next.insert(name, value);
        }
        next
    }

    fn admits(&mut self, action: &str, next: &State) -> (bool, String) {
        self.steps += 1;
        aterm_spec::verify::validate_transition_tiered(
            &self.model,
            &[],
            &self.state,
            next,
            Some(action),
            &format!("TrustFactsReuse step {} {action}", self.steps),
        )
    }

    fn step(&mut self, action: &str, next: State) {
        let (ok, why) = self.admits(action, &next);
        assert!(
            ok,
            "the model refuses production's {action}: {why}\nfrom {:?}\nto   {next:?}",
            self.state
        );
        self.state = next;
    }

    fn cleared(&self, fields: &[(&'static str, i64)]) -> State {
        let mut all = fields.to_vec();
        all.extend([
            ("asked", 0),
            ("found", 0),
            ("verdict", 0),
            ("used_src", 0),
            ("used_prv", 0),
            ("used_proved", 0),
        ]);
        self.with(&all)
    }

    fn edit_source(&mut self) {
        let src = self.state["src"] + 1;
        let head = self.commit(&[("crates/alpha/src/lib.rs", &format!("v{src}"))]);
        self.src_of.insert(head, src);
        let next = self.cleared(&[("src", src)]);
        self.step("EditSource", next);
    }

    fn move_floor(&mut self, action: &str, by: i64, other: i64, why: i64) {
        let floor = self.state["floor"] + by;
        let src = self.state["src"];
        let head = self.commit(&[(RATCHET, &floors(floor, other, why))]);
        self.src_of.insert(head.clone(), src);
        assert_eq!(
            self.floor_at(&head),
            (floor, other, why),
            "the commit holds the floor moved"
        );
        let next = self.cleared(&[("floor", floor), ("other", other), ("why", why)]);
        self.step(action, next);
    }

    fn raise(&mut self) {
        let other = self.state["other"];
        self.move_floor("RaiseFloor", 1, other, 0);
    }

    fn lower(&mut self) {
        let other = self.state["other"];
        self.move_floor("LowerFloor", -1, other, 0);
    }

    fn relabel(&mut self) {
        self.move_floor("RelabelFloor", 0, 1, 0);
    }

    fn rebase(&mut self) {
        self.move_floor("RebaseFloor", -1, 0, 0);
    }

    fn rebase_with_reason(&mut self) {
        self.move_floor("RebaseFloorWithReason", -1, 0, 1);
    }

    fn answer(&self) -> (i64, i64) {
        (self.state["found"], self.state["verdict"])
    }

    fn swap_prover(&mut self) {
        let next = self.cleared(&[("prv", 1 - self.state["prv"])]);
        self.step("SwapProver", next);
    }

    fn alpha(proved: i64, lapsed: u64, pace: Option<Pace>) -> Crate {
        Crate::new(
            "alpha",
            Decision::Gate(Measured {
                rc: "0".into(),
                summary: format!("Summary: {proved} proved (9 total)"),
                lapsed,
                pace,
                ..Measured::default()
            }),
        )
    }

    /// A loaded run (`Some` pace) lapses one obligation: in a starved run its shortfall is the
    /// clock's and undecided, in one that was not it is the code's. Neither is a PASS.
    fn run_as(&mut self, action: &str, loaded: Option<Pace>) {
        let head = self.head();
        let src = self.src_of[&head];
        for e in fs::read_dir(self.store()).into_iter().flatten().flatten() {
            let _ = fs::remove_file(e.path());
        }
        let alpha = match loaded {
            Some(pace) => Harness::alpha(src - 1, 1, Some(pace)),
            None => Harness::alpha(src, 0, None),
        };
        let filed = testing::file(
            &self.cli(),
            &self.store(),
            self.prover(),
            &head,
            DRV,
            vec![alpha],
        );
        let record = Facts::parse(&fs::read_to_string(&filed).unwrap()).unwrap();
        let f = self.project_record(&record);
        let ratchet = trust_facts::ratchet_at(&self.cli(), &head).unwrap();
        self.word = trust_facts::judge(
            &record,
            ratchet.as_deref(),
            None,
            RATCHET,
            trust_facts::MEMORY,
            Update::NONE,
        )
        .word;
        let pass = self.word == "PASS";
        let (f_floor, f_other, f_why) = self.floor_at(&record.provenance.clone().unwrap().head);
        let next = self.cleared(&[
            ("filed", 1),
            ("f_src", f.src),
            ("f_prv", f.prv),
            ("f_floor", f_floor),
            ("f_other", f_other),
            ("f_why", f_why),
            ("f_proved", f.proved),
            ("f_lapsed", f.lapsed),
            ("m_src", f.m_src),
            ("m_prv", f.m_prv),
            ("f_pass", i64::from(pass)),
        ]);
        self.step(action, next);
    }

    fn run(&mut self) {
        self.run_as("Run", None);
    }

    fn run_under_load(&mut self) {
        self.run_as("RunUnderLoad", Some(STARVED));
    }

    fn run_under_load_unstarved(&mut self) {
        self.run_as("RunUnderLoad", Some(UNSTARVED));
    }

    fn remeasure(&mut self) {
        let head = self.head();
        let (base, _) = facts_for(&self.cli(), &self.store(), self.prover(), &head)
            .unwrap()
            .expect("--remeasure completes the record of HEAD's key");
        let again = self.measured_on(&head, self.prover());
        let done = trust_facts::remeasured(&base, &again).expect("one key, one host, one driver");
        trust_facts::file_record(&self.store(), &done).unwrap();
        let f = self.project_record(&done);
        let next = self.cleared(&[
            ("f_proved", f.proved),
            ("f_lapsed", f.lapsed),
            ("m_src", f.m_src),
            ("m_prv", f.m_prv),
        ]);
        self.step("Remeasure", next);
    }

    fn measured_on(&self, commit: &str, prover: &Prover) -> Facts {
        let dir = self.root.join("scratch-store");
        let _ = fs::remove_dir_all(&dir);
        let src = self.src_of[commit];
        let path = testing::file(
            &self.cli(),
            &dir,
            prover,
            commit,
            DRV,
            vec![Harness::alpha(src, 0, None)],
        );
        let facts = Facts::parse(&fs::read_to_string(path).unwrap()).unwrap();
        let _ = fs::remove_dir_all(&dir);
        facts
    }

    fn project_record(&self, facts: &Facts) -> Projected {
        let p = facts.provenance.clone().unwrap();
        let prv = self
            .provers
            .iter()
            .position(|pr| pr.identity.as_ref().ok() == Some(&p.prover))
            .unwrap();
        let Some(Decision::Gate(m)) = facts.crates.first().map(|c| &c.decision) else {
            panic!("the record measures alpha");
        };
        let proved = trust_facts::parse_summary(&m.summary).0.unwrap();
        let src = self.src_of[&p.head];
        let m_src = p
            .remeasured
            .last()
            .and_then(|r| r.split_whitespace().find_map(|w| self.src_of.get(w)))
            .copied()
            .unwrap_or(src);
        let prv = i64::try_from(prv).unwrap();
        Projected {
            src,
            prv,
            proved: i64::try_from(proved).unwrap(),
            lapsed: i64::from(m.lapsed > 0),
            m_src,
            m_prv: prv,
        }
    }

    fn assembled(&self) -> State {
        let head = self.head();
        let judged = trust_facts::run_cmd(
            &self.work,
            TrustCmd::Judge {
                commit: head.clone(),
                prover: Some(self.prover().bin.clone()),
                update: false,
                reason: None,
            },
        );
        let found = facts_for(&self.cli(), &self.store(), self.prover(), &head).unwrap();
        let (found, used) = match found {
            Ok((facts, _)) => {
                let f = self.project_record(&facts);
                (1, (f.m_src, f.m_prv, f.proved))
            }
            Err(_) => (0, (0, 0, 0)),
        };
        let verdict = if judged == 0 { 1 } else { 2 };
        self.with(&[
            ("asked", 1),
            ("found", found),
            ("verdict", verdict),
            ("used_src", used.0),
            ("used_prv", used.1),
            ("used_proved", used.2),
        ])
    }

    fn assemble(&mut self) {
        let next = self.assembled();
        self.step("Assemble", next);
    }

    fn refuses_a_pass(&mut self, which: &str) {
        let head = self.head();
        let (facts, _) = facts_for(&self.cli(), &self.store(), self.prover(), &head)
            .unwrap()
            .expect("the facts are found");
        let f = self.project_record(&facts);
        let retired = self.with(&[
            ("asked", 1),
            ("found", 1),
            ("verdict", 1),
            ("used_src", f.m_src),
            ("used_prv", f.m_prv),
            ("used_proved", f.proved),
        ]);
        let (ok, why) = self.admits("Assemble", &retired);
        assert!(
            !ok,
            "NEGATIVE CONTROL passed the model — {which}'s PASS was admitted ({why}); the \
             conformance would not catch the defect it replays"
        );
    }
}

struct Projected {
    src: i64,
    prv: i64,
    proved: i64,
    lapsed: i64,
    m_src: i64,
    m_prv: i64,
}

fn floors(n: i64, other: i64, why: i64) -> String {
    let prover = if other == 1 { OTHER } else { DRV };
    if why == 1 {
        format!("alpha\t{n}\t9\t{prover}\t{REASON}\n")
    } else {
        format!("alpha\t{n}\t9\t{prover}\n")
    }
}

#[test]
fn the_real_filing_and_reader_conform_to_the_facts_contract() {
    let mut h = Harness::new("walk");
    h.run();
    h.assemble();
    assert_eq!(h.answer(), (1, 1));
    h.raise();
    h.assemble();
    assert_eq!(h.answer(), (1, 2), "reused, and refused");
    h.edit_source();
    h.assemble();
    assert_eq!(h.state["found"], 0);
    h.run();
    h.assemble();
    assert_eq!(h.state["verdict"], 1);
    h.raise();
    h.assemble();
    assert_eq!(h.answer(), (1, 2));
    h.lower();
    h.assemble();
    assert_eq!(h.answer(), (1, 1), "fail -> pass, no run");
    h.lower();
    h.assemble();
    assert_eq!(
        h.answer(),
        (1, 2),
        "below the run's floor, by hand: found, and refused"
    );
    h.raise();
    h.relabel();
    h.assemble();
    assert_eq!(
        h.answer(),
        (1, 2),
        "re-labelled away from the measuring prover"
    );
    h.run();
    h.assemble();
    assert_eq!(h.answer(), (1, 1), "another prover's 1, met");
    h.raise();
    h.run();
    h.assemble();
    assert_eq!(h.answer(), (1, 2), "another prover's 2, held: refused");
    h.rebase();
    h.assemble();
    assert_eq!(h.answer(), (1, 2), "re-based lower with no reason: refused");
    h.raise();
    h.relabel();
    h.rebase_with_reason();
    h.assemble();
    assert_eq!(h.answer(), (1, 1), "re-based to 1 with its reason: PASS");
    h.swap_prover();
    h.assemble();
    assert_eq!(h.state["found"], 0);
    h.run();
    h.assemble();
    assert_eq!(h.state["verdict"], 1);
    h.swap_prover();
    h.assemble();
    assert_eq!(h.state["found"], 0);
}

#[test]
fn the_real_remeasure_completes_the_record_of_its_key() {
    let mut h = Harness::new("clock");
    h.edit_source();
    h.raise();
    h.run_under_load();
    h.assemble();
    assert_eq!(h.answer(), (1, 2), "0 proved, 1 lapsed, floor 1: undecided");
    h.lower();
    h.raise();
    h.remeasure();
    h.assemble();
    assert_eq!(h.answer(), (1, 1), "measured again on the same key: PASS");
    assert_eq!(h.state["m_src"], h.state["f_src"]);
}

#[test]
fn the_pre_facts_receipt_reader_is_refused() {
    let mut h = Harness::new("receipt");
    h.run();
    let measured_tree = git(&h.work, &["rev-parse", "HEAD^{tree}"]);
    h.raise();
    let now_tree = git(&h.work, &["rev-parse", "HEAD^{tree}"]);
    assert_ne!(
        measured_tree, now_tree,
        "a floors-only commit is another tree"
    );
    let receipt_answer = h.with(&[
        ("asked", 1),
        ("found", 0),
        ("verdict", 2),
        ("used_src", 0),
        ("used_prv", 0),
        ("used_proved", 0),
    ]);
    let (ok, why) = h.admits("Assemble", &receipt_answer);
    assert!(
        !ok,
        "NEGATIVE CONTROL passed the model — the whole-tree receipt reader's answer was \
         admitted ({why}); the conformance would not catch the defect it replays"
    );
    h.assemble();
    assert_eq!(h.state["found"], 1);
}

#[test]
fn the_unchecked_reuse_reader_is_refused() {
    let mut h = Harness::new("unchecked");
    h.edit_source();
    h.raise();
    h.run();
    h.lower();
    let head = h.head();
    let (facts, _) = facts_for(&h.cli(), &h.store(), h.prover(), &head)
        .unwrap()
        .expect("the facts are found");
    let ratchet = trust_facts::ratchet_at(&h.cli(), &head).unwrap();
    let judged = trust_facts::judge(
        &facts,
        ratchet.as_deref(),
        None,
        RATCHET,
        trust_facts::MEMORY,
        Update::NONE,
    );
    assert_eq!(
        judged.word, "PASS",
        "1 proved against the lowered floor of 0"
    );
    h.refuses_a_pass("the unchecked reuse reader");
    h.assemble();
    assert_eq!(h.answer(), (1, 2));
}

#[test]
fn the_retired_floor_excuses_are_refused() {
    let mut h = Harness::new("new-prover");
    h.edit_source();
    h.raise();
    h.raise();
    h.relabel();
    h.run();
    h.refuses_a_pass("the lane's BELOW-FLOOR-NEW-PROVER");
    h.assemble();
    assert_eq!(h.answer(), (1, 2));

    for (label, starved) in [("timed-out", true), ("timed-out-unstarved", false)] {
        let mut h = Harness::new(label);
        h.edit_source();
        h.raise();
        if starved {
            h.run_under_load();
        } else {
            h.run_under_load_unstarved();
        }
        assert_eq!(
            h.word,
            if starved { "COULD-NOT-RUN" } else { "FAIL" },
            "{label}: the clock's only in a starved run"
        );
        h.refuses_a_pass("the lane's BELOW-FLOOR-TIMED-OUT");
        h.assemble();
        assert_eq!(h.answer(), (1, 2), "{label}");
    }

    let mut h = Harness::new("rebase");
    h.raise();
    h.relabel();
    h.run();
    h.rebase();
    let head = h.head();
    let (facts, _) = facts_for(&h.cli(), &h.store(), h.prover(), &head)
        .unwrap()
        .expect("found");
    let judged = trust_facts::judge(
        &facts,
        trust_facts::ratchet_at(&h.cli(), &head).unwrap().as_deref(),
        None,
        RATCHET,
        trust_facts::MEMORY,
        Update::NONE,
    );
    assert_eq!(
        judged.word, "PASS",
        "0 proved against the re-based floor of 0"
    );
    h.refuses_a_pass("P0's unreasoned re-base");
    h.assemble();
    assert_eq!(h.answer(), (1, 2));
}

#[test]
fn a_remeasure_across_keys_is_refused() {
    let mut h = Harness::new("across");
    h.edit_source();
    h.run_under_load();
    let base_head = h.head();
    let (base, _) = facts_for(&h.cli(), &h.store(), h.prover(), &base_head)
        .unwrap()
        .expect("found");
    h.edit_source();
    let moved = h.head();
    let again = h.measured_on(&moved, h.prover());
    let err = trust_facts::remeasured(&base, &again).expect_err("two keys are never one record");
    assert!(
        err.contains("facts of two keys are never one record"),
        "{err}"
    );
    let naive = h.cleared(&[
        ("f_proved", h.src_of[&moved]),
        ("f_lapsed", 0),
        ("m_src", h.src_of[&moved]),
        ("m_prv", h.state["prv"]),
    ]);
    let (ok, why) = h.admits("Remeasure", &naive);
    assert!(
        !ok,
        "NEGATIVE CONTROL passed the model — a fold across keys was admitted ({why})"
    );
}

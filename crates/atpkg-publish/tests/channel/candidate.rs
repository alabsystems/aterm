// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The candidate gate, answered in-process: what admits a publish (a receipt's rules),
//! which receipt governs, and the group's own [`ShipTrustCandidate`] against a stub seal,
//! a scratch aterm checkout and a receipts store. The group's handling of each answer is
//! in `group.rs`.
//!
//! Beside it, the publish's other admission: the channel head it is built over verifies
//! under the head's OWN roster pair, so a head whose pair has lapsed is refused even when
//! a newer roster on this machine would authorize it.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use atpkg_publish::api::{Baseline, Delta, IndexRequest};

use atpkg_publish::channel::candidate::{
    Aterm, CandidateGate, Checkout, GateAnswer, PROGRAM, RECEIPT_MAGIC, RETRACTION_LEDGER, Receipt,
    ShipTrustCandidate, command, newest_receipt, receipts_dir, refusals, tree_root,
};
use atpkg_publish::channel::index::publish_with;
use atpkg_publish::ctx::Ctx;
use atpkg_publish::quad::QUAD_FILES;

use super::{Doc, Rig, git, scratch, sha256_hex};

struct Repo {
    head_contains: Vec<&'static str>,
    ledgers: BTreeMap<&'static str, &'static str>,
}

impl Aterm for Repo {
    fn contains(&self, commit: &str) -> bool {
        self.head_contains.contains(&commit)
    }
    fn ledger_sha(&self, rev: &str) -> Option<String> {
        self.ledgers.get(rev).map(|s| (*s).to_string())
    }
}

const ROOT: &str = "a519d423f47ff6fb19d505012f210f13ad39f2f6f31cb3c3af5aff508551d0b2";
const SEAL: &str = "5ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea15ea1";
const AT: &str = "36b2c3df36eac4971b8189da9967ab7cc442bbf2";

/// A current PASS receipt for the toolchain `root`, measured at aterm `at` under the
/// ledger whose sha256 is `ledger`.
fn pass_for(root: &str, at: &str, ledger: &str) -> Receipt {
    let mut r = Receipt::parse(&format!(
        "{RECEIPT_MAGIC}\nverdict PASS\nexit 0\nmode run\ncandidate-tree-root {root}\n\
         candidate-prover {root}\nbase-from published (public index 68 on alabsystems/aterm)\n\
         base-prover {SEAL}\nbase-signed-tree-root {SEAL}\n\
         base-build 9192\ndate 2026-10-14T00:00:00Z\naterm-commit {at}\n\
         retractions {ledger} /r/{RETRACTION_LEDGER}\ncompare /o/compare.txt\n"
    ));
    r.fields.insert("extra".into(), "x".into());
    r
}

fn pass() -> Receipt {
    pass_for(ROOT, AT, "abc")
}

fn repo() -> Repo {
    Repo {
        head_contains: vec![AT],
        ledgers: BTreeMap::from([(AT, "abc"), ("HEAD", "abc")]),
    }
}

fn now() -> i64 {
    aterm_types::rfc3339::parse_utc("2026-10-15T00:00:00Z").expect("a stamp")
}

#[test]
fn the_same_question_from_a_shell_names_the_seal_and_the_published_pin() {
    let argv: Vec<String> = command(Path::new("/seals/stage-trust-wp"), Some(9192))
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(PROGRAM, "targo");
    assert_eq!(
        argv,
        [
            "--unverified",
            "ship",
            "trust",
            "candidate",
            "--admits",
            "/seals/stage-trust-wp",
            "--base-build",
            "9192"
        ]
    );
    let unread: Vec<String> = command(Path::new("/s"), None)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(!unread.contains(&"--base-build".to_string()));
}

/// A current PASS of a run that measured this toolchain against the published build,
/// on this aterm's history, under the ledger committed there, admits; each way a
/// receipt admits nothing refuses, one at a time.
#[test]
fn every_way_a_receipt_admits_nothing() {
    assert_eq!(
        refusals(Some(&pass()), ROOT, 9192, now(), &repo()),
        Vec::<String>::new()
    );
    let refused = |edit: &dyn Fn(&mut Receipt), pin: u64, at: i64, r: &Repo| {
        let mut receipt = pass();
        edit(&mut receipt);
        refusals(Some(&receipt), ROOT, pin, at, r).len()
    };
    let set = |k: &'static str, v: &'static str| {
        move |r: &mut Receipt| {
            r.fields.insert(k.into(), v.into());
        }
    };
    let none = |_: &mut Receipt| {};
    assert_eq!(
        refusals(None, ROOT, 9192, now(), &repo()).len(),
        1,
        "no receipt"
    );
    assert_eq!(
        refused(
            &|r| r.magic = "aterm-trust candidate-gate 1".into(),
            9192,
            now(),
            &repo()
        ),
        1
    );
    assert_eq!(refused(&set("verdict", "REFUSED"), 9192, now(), &repo()), 1);
    assert_eq!(refused(&set("exit", "1"), 9192, now(), &repo()), 1);
    assert_eq!(refused(&set("mode", "replay"), 9192, now(), &repo()), 1);
    assert_eq!(
        refused(&set("candidate-prover", "other"), 9192, now(), &repo()),
        1
    );
    assert_eq!(
        refused(&set("base-from", "explicit (--base)"), 9192, now(), &repo()),
        1
    );
    assert_eq!(
        refused(&none, 9300, now(), &repo()),
        1,
        "the channel moved on"
    );
    assert_eq!(
        refused(&set("base-prover", ROOT), 9192, now(), &repo()),
        1,
        "a base that is not the verified seal"
    );
    assert_eq!(
        refused(&set("base-signed-tree-root", "-"), 9192, now(), &repo()),
        1,
        "a base bound to no verified seal"
    );
    let late = aterm_types::rfc3339::parse_utc("2026-10-30T00:00:00Z").expect("a stamp");
    assert_eq!(refused(&none, 9192, late, &repo()), 1, "too old");
    assert_eq!(refused(&set("date", "yesterday"), 9192, now(), &repo()), 1);
    let elsewhere = Repo {
        head_contains: vec![],
        ..repo()
    };
    assert_eq!(
        refused(&none, 9192, now(), &elsewhere),
        1,
        "another history"
    );
    assert_eq!(
        refused(&set("retractions", "def /x"), 9192, now(), &repo()),
        1,
        "a ledger by hand"
    );
    let moved = Repo {
        ledgers: BTreeMap::from([(AT, "abc"), ("HEAD", "def")]),
        ..repo()
    };
    assert_eq!(
        refused(&none, 9192, now(), &moved),
        1,
        "the ledger changed since"
    );
    let unshown = Repo {
        ledgers: BTreeMap::new(),
        ..repo()
    };
    assert_eq!(refused(&none, 9192, now(), &unshown), 1);
}

#[test]
fn a_receipt_round_trips_and_the_newest_run_governs() {
    let d = scratch("receipts");
    let write = |run: &str, verdict: &str, date: &str| {
        let mut r = pass();
        r.fields.insert("verdict".into(), verdict.into());
        r.fields.insert("date".into(), date.into());
        std::fs::create_dir_all(d.path().join(run)).expect("mkdir");
        std::fs::write(d.path().join(run).join("RECEIPT"), r.render()).expect("write");
    };
    write("a", "PASS", "2026-10-13T00:00:00Z");
    write("b", "REFUSED", "2026-10-14T00:00:00Z");
    let (_, newest) = newest_receipt(d.path(), ROOT).expect("a receipt");
    assert_eq!(
        newest.get("verdict"),
        "REFUSED",
        "an older PASS never outlives a newer run"
    );
    assert_eq!(Receipt::parse(&newest.render()), newest);
    assert!(newest_receipt(d.path(), "another").is_none());
}

/// THE GROUP'S OWN GATE, in-process: a stub seal, a scratch aterm checkout with the
/// ledger committed, and a PASS receipt for the seal in the context's receipts store
/// admit; the receipt store, the checkout's history and the ledger at HEAD each decide.
/// No cutter is built and no child but git runs.
#[test]
fn the_groups_gate_answers_in_process_from_the_receipts_store_and_the_checkout() {
    let tmp = scratch("gate");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let mut ctx = Ctx::new(&home, None, None, now());
    // the aterm checkout: the retraction ledger, committed
    let aterm = tmp.path().join("aterm");
    std::fs::create_dir_all(aterm.join("crates/aterm-release/trust")).expect("mkdir");
    git(&aterm, &["init", "-q", "-b", "main"]);
    let ledger = b"# retractions\n";
    std::fs::write(aterm.join(RETRACTION_LEDGER), ledger).expect("write ledger");
    git(&aterm, &["add", "."]);
    git(&aterm, &["commit", "-q", "-m", "ledger"]);
    let at = git(&aterm, &["rev-parse", "HEAD"]);
    ctx.repo_root = aterm.clone();
    // the seal: a stub toolchain directory
    let seal = tmp.path().join("stage-trust-wp");
    std::fs::create_dir_all(seal.join("bin")).expect("mkdir seal");
    std::fs::write(seal.join("bin/targo"), b"#!/bin/sh\n").expect("write targo");
    let root = tree_root(&seal).expect("tree_root");
    // its receipt
    let receipts = receipts_dir(&ctx.state_dir);
    let r = pass_for(&root, &at, &sha256_hex(ledger));
    let run = receipts.join("run-1");
    std::fs::create_dir_all(&run).expect("mkdir run");
    std::fs::write(run.join("RECEIPT"), r.render()).expect("write receipt");

    let started = Instant::now();
    let answer = ShipTrustCandidate.admits(&ctx, &seal, Some(9192));
    let took = started.elapsed();
    println!("in-process candidate-gate answer: {took:?}");
    match &answer {
        GateAnswer::Admitted(said) => assert!(said.contains("run-1/RECEIPT"), "{said}"),
        other => panic!("a current PASS for the seal admits: {other:?}"),
    }
    // the bin/ named for the toolchain is the same toolchain
    assert_eq!(
        ShipTrustCandidate.admits(&ctx, &seal.join("bin"), Some(9192)),
        answer
    );

    // the channel moved on
    match ShipTrustCandidate.admits(&ctx, &seal, Some(9300)) {
        GateAnswer::Refused(why) => assert!(why[0].contains("now pins 9300"), "{why:?}"),
        other => panic!("{other:?}"),
    }
    // no pin read: nothing to hold the receipt to
    assert!(matches!(
        ShipTrustCandidate.admits(&ctx, &seal, None),
        GateAnswer::CannotCheck(_)
    ));
    // no such seal
    assert!(matches!(
        ShipTrustCandidate.admits(&ctx, &tmp.path().join("absent"), Some(9192)),
        GateAnswer::CannotCheck(_)
    ));
    // the ledger changed at HEAD since the run: its excuses no longer stand
    std::fs::write(aterm.join(RETRACTION_LEDGER), b"# retractions\nrow\n").expect("write");
    git(&aterm, &["commit", "-q", "-am", "a retraction"]);
    match ShipTrustCandidate.admits(&ctx, &seal, Some(9192)) {
        GateAnswer::Refused(why) => {
            assert_eq!(why.len(), 1, "{why:?}");
            assert!(why[0].contains("changed since the run"), "{why:?}");
        }
        other => panic!("{other:?}"),
    }
    // another history: the measured commit is not HEAD's
    let other = tmp.path().join("other");
    std::fs::create_dir_all(other.join("crates/aterm-release/trust")).expect("mkdir");
    git(&other, &["init", "-q", "-b", "main"]);
    std::fs::write(other.join(RETRACTION_LEDGER), ledger).expect("write ledger");
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "elsewhere"]);
    let elsewhere = Checkout(&other);
    assert!(!elsewhere.contains(&at));
    assert_eq!(elsewhere.ledger_sha("HEAD"), Some(sha256_hex(ledger)));
    ctx.repo_root = other;
    match ShipTrustCandidate.admits(&ctx, &seal, Some(9192)) {
        GateAnswer::Refused(why) => assert!(
            why.iter().any(|w| w.contains("does not contain")),
            "{why:?}"
        ),
        other => panic!("{other:?}"),
    }
    // no receipt for it at all
    std::fs::remove_dir_all(&receipts).expect("rm receipts");
    match ShipTrustCandidate.admits(&ctx, &seal, Some(9192)) {
        GateAnswer::Refused(why) => {
            assert!(why[0].contains("no candidate-gate receipt"), "{why:?}")
        }
        other => panic!("{other:?}"),
    }
}

/// A head whose OWN roster pair has lapsed is refused, even beside a newer roster the
/// paper master signed that this machine holds: the publish verifies the head under the
/// pair it carries and never re-binds it to a local one (decided: keep refusing a lapsed
/// roster). The same head under a current pair is built over, and the newer local roster
/// is then the one shipped — so the local roster is read, and the lapse alone refused.
#[test]
fn a_head_whose_own_roster_pair_lapsed_is_refused_under_a_newer_local_roster() {
    let rig = Rig::new("lapsed-roster");
    // the newest local roster: a later generation, current, master-signed, installed
    let mut newer = rig.keys.roster.clone();
    newer.roster_seq += 1;
    let newer_bytes = newer.to_toml().expect("the roster emits").into_bytes();
    let installed = &rig.ctx.identity.roster;
    std::fs::create_dir_all(installed.parent().expect("a roster dir")).expect("mkdir roster");
    std::fs::write(installed, &newer_bytes).expect("write local roster");
    std::fs::write(
        format!("{}.sig", installed.display()),
        rig.keys.master_sign(&newer_bytes),
    )
    .expect("write local roster sig");

    // the head's own pair: the same machine, a roster whose valid_until has passed
    let mut lapsed = rig.keys.roster.clone();
    lapsed.valid_until = "2020-01-01T00:00:00Z".into();
    let lapsed_bytes = lapsed.to_toml().expect("the roster emits").into_bytes();
    let head = rig.path("head");
    let doc = Doc::new(41, &[("ay", 5)]);
    rig.quad_dir(&head, &doc);
    std::fs::write(head.join(QUAD_FILES[2]), &lapsed_bytes).expect("write roster");
    std::fs::write(
        head.join(QUAD_FILES[3]),
        rig.keys.master_sign(&lapsed_bytes),
    )
    .expect("write roster sig");
    let over_head = |out: &str| IndexRequest {
        baseline: Baseline::Dir(head.clone()),
        delta: Delta::default(),
        out: rig.path(out),
        upload: false,
        pack_dir: None,
    };

    let refused = over_head("refused");
    let err = publish_with(&rig.ctx, &refused, &mut Rig::log())
        .expect_err("a head whose roster pair lapsed is refused");
    assert!(
        err.to_string().contains("the roster does not verify"),
        "{err}"
    );
    assert!(
        !refused.out.join("index.toml").exists(),
        "nothing was built"
    );

    rig.quad_dir(&head, &doc);
    let built = over_head("built");
    let mut log = Rig::log();
    let o = publish_with(&rig.ctx, &built, &mut log)
        .unwrap_or_else(|e| panic!("a current pair is built over: {e}\n{}", log.text()));
    assert_eq!(
        o.plan.roster_seq, newer.roster_seq,
        "the newest roster ships"
    );
}

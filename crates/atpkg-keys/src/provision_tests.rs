// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::master::parse_master;

const PAPER: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";
const OTHER_PAPER: &str = "zyxwvtsrqpnmkjhgfedcba9876543210zyxwvtsrqpnmkjhgfed0";

const NOW: u64 = 1_785_801_600;

fn scratch(name: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-keys-provision-{name}-"))
}

const PINS_FIXTURE: &str = "// Copyright 2026 Andrew Yates\n\
    // SPDX-License-Identifier: Apache-2.0\n\
    \n\
    /// The paper master. Empty here, and therefore INERT.\n\
    pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n";

fn paths_in(dir: &std::path::Path) -> Paths {
    let s = |n: &str| {
        dir.join(n)
            .to_str()
            .expect("utf-8 scratch path")
            .to_string()
    };
    Paths {
        pins: s("pins.rs"),
        roster: s("aterm-machines.toml"),
        key: s("machine.key"),
        machine_pub: s("machine.toml"),
        pins_explicit: true,
    }
}

fn write_fixture(paths: &Paths) {
    std::fs::write(&paths.pins, PINS_FIXTURE).expect("fixture pins.rs");
}

fn seed_of(paper: &str) -> MasterSeed {
    parse_master(paper).expect("synthetic phrase").seed()
}

fn run_setup(paths: &Paths, id: &str) -> Result<Report, String> {
    let pre = preflight(Verb::Setup, id, paths)?;
    let seed = seed_of(PAPER);
    let planned = plan(pre, &seed, NOW)?;
    write_pins(&planned)?;
    write_rest(planned)
}

fn run_join(paths: &Paths, id: &str, paper: &str) -> Result<Report, String> {
    let pre = preflight(Verb::Join, id, paths)?;
    let seed = seed_of(paper);
    verify_master(&pre, &seed)?;
    let planned = plan(pre, &seed, NOW)?;
    write_pins(&planned)?;
    write_rest(planned)
}

#[test]
fn setup_arms_the_anchor_mints_the_machine_and_signs_the_roster() {
    let dir = scratch("setup-happy");
    let paths = paths_in(&dir);
    write_fixture(&paths);

    let report = run_setup(&paths, "m3").expect("setup completes");

    let src = std::fs::read_to_string(&paths.pins).unwrap();
    let master = read_anchor(&src, MASTER_ANCHOR).unwrap();
    assert_eq!(master.members, vec![report.master_pubkey.clone()]);
    assert_eq!(
        master.members[0],
        seed_of(PAPER).pubkey_b64().unwrap(),
        "the armed anchor is the master the phrase derives"
    );
    assert!(
        !src.contains(report.machine_pubkey.as_str()),
        "the minted key must appear nowhere in the anchor file"
    );

    let bytes = std::fs::read(&paths.roster).unwrap();
    let sig = std::fs::read(concat(&[&paths.roster, ".sig"])).unwrap();
    let verified =
        aterm_update_core::roster::verify_roster(&[report.master_pubkey.as_str()], bytes, &sig)
            .expect("the roster verifies under the armed master");
    let roster = Roster::parse(&verified).unwrap();
    assert_eq!(
        roster.machines.len(),
        1,
        "the first roster names this machine alone"
    );
    assert_eq!(roster.machines[0].id, "m3");
    assert_eq!(roster.machines[0].pubkey, report.machine_pubkey);
    assert_eq!(roster.roster_seq, 1, "one bump per machine added");

    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(&paths.key).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "the machine key must be owner-only");
    let pkcs8 = std::fs::read(&paths.key).unwrap();
    assert_eq!(crate::pubkey_b64(&pkcs8).unwrap(), report.machine_pubkey);
}

#[test]
fn the_planned_public_snapshot_matches_the_successfully_published_pair() {
    let dir = scratch("planned-public-pair");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
    let seed = seed_of(PAPER);
    let planned = plan(pre, &seed, NOW).unwrap();
    let expected = planned.public_roster();
    write_pins(&planned).unwrap();
    write_rest(planned).unwrap();
    assert_eq!(std::fs::read(&paths.roster).unwrap(), expected.raw);
    assert_eq!(
        std::fs::read(concat(&[&paths.roster, ".sig"])).unwrap(),
        expected.sig
    );
}

#[test]
fn setup_refuses_when_a_master_is_already_committed() {
    let dir = scratch("setup-refuses");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("the first setup succeeds");

    let before = std::fs::read_to_string(&paths.pins).unwrap();
    let err = preflight(Verb::Setup, "m11", &paths).unwrap_err();
    assert!(err.contains("ALREADY committed"), "{err}");
    assert!(err.contains("join"), "{err}");
    assert!(err.contains("strand"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&paths.pins).unwrap(),
        before,
        "a refused setup writes nothing"
    );

    let fresh = scratch("setup-refuses-control");
    let fresh_paths = paths_in(&fresh);
    write_fixture(&fresh_paths);
    assert!(preflight(Verb::Setup, "m11", &fresh_paths).is_ok());
}

#[test]
fn join_proves_the_master_and_adds_a_second_machine() {
    let dir = scratch("join-happy");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    let first = run_setup(&paths, "m3").expect("setup");

    let mut second = paths.clone();
    second.key = dir.join("m11.key").to_str().unwrap().to_string();
    second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();

    let pre = preflight(Verb::Join, "m11", &second).expect("join preflight");
    let seed = seed_of(PAPER);
    verify_master(&pre, &seed).expect("the phrase matches the committed anchor");
    let planned = plan(pre, &seed, NOW).expect("join plans");
    write_pins(&planned).expect("pins written");
    let report = write_rest(planned).expect("join completes");

    let src = std::fs::read_to_string(&paths.pins).unwrap();
    assert_eq!(
        read_anchor(&src, MASTER_ANCHOR).unwrap().members,
        vec![first.master_pubkey.clone()],
        "join never touches the master anchor"
    );
    assert!(
        !src.contains(first.machine_pubkey.as_str())
            && !src.contains(report.machine_pubkey.as_str()),
        "neither minted key appears in the anchor file"
    );
    assert_eq!(report.roster_seq, 2, "the roster advanced");
    assert_eq!(report.roster_machines, vec!["m3", "m11"]);
    assert!(!report.roster_was_fresh, "join edited the existing roster");
}

#[test]
fn join_refuses_a_missing_roster_instead_of_forking_a_second_one() {
    let dir = scratch("join-no-roster");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("setup, on the first machine");

    let dir2 = scratch("join-no-roster-second");
    let second = paths_in(&dir2);
    std::fs::copy(&paths.pins, &second.pins).expect("the anchor travels with the repo");
    assert!(
        !std::path::Path::new(&second.roster).exists(),
        "the roster does NOT travel with the repo; that is the premise"
    );

    let err = preflight(Verb::Join, "m11", &second).unwrap_err();
    assert!(err.contains("no roster at"), "{err}");
    assert!(err.contains("COPY"), "{err}");
    assert!(err.contains("will not start a second roster"), "{err}");
    assert!(
        !std::path::Path::new(&second.roster).exists(),
        "a refused join creates nothing"
    );
    assert!(!std::path::Path::new(&second.key).exists());

    std::os::unix::fs::symlink(&paths.roster, &second.roster).unwrap();
    let err = preflight(Verb::Join, "m11", &second).unwrap_err();
    assert!(
        err.contains("no roster at"),
        "a symlinked roster is refused at preflight: {err}"
    );
    std::fs::remove_file(&second.roster).unwrap();

    std::fs::copy(&paths.roster, &second.roster).unwrap();
    std::fs::copy(
        concat(&[&paths.roster, ".sig"]),
        concat(&[&second.roster, ".sig"]),
    )
    .unwrap();
    let report = run_join(&second, "m11", PAPER).expect("join, with the roster in hand");
    assert_eq!(
        report.roster_machines,
        vec!["m3", "m11"],
        "the roster was EXTENDED: every machine already on it survives"
    );
    assert!(!report.roster_was_fresh);
}

#[test]
fn join_refuses_a_roster_that_vanished_between_preflight_and_plan() {
    let dir = scratch("join-vanished-roster");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("setup");

    let mut second = paths.clone();
    second.key = dir.join("m11.key").to_str().unwrap().to_string();
    second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();

    let pre = preflight(Verb::Join, "m11", &second).expect("roster is present");
    let saved_body = std::fs::read(&second.roster).unwrap();
    let saved_sig = std::fs::read(concat(&[&second.roster, ".sig"])).unwrap();
    std::fs::remove_file(&second.roster).unwrap();
    std::fs::remove_file(concat(&[&second.roster, ".sig"])).unwrap();

    let seed = seed_of(PAPER);
    verify_master(&pre, &seed).expect("the phrase itself is right");
    let err = plan(pre, &seed, NOW).unwrap_err();
    assert!(err.contains("no roster at"), "{err}");
    assert!(err.contains("second roster"), "{err}");
    assert!(
        !std::path::Path::new(&second.roster).exists()
            && !std::path::Path::new(&second.key).exists(),
        "a refused plan writes nothing and forks nothing"
    );

    std::fs::write(&second.roster, &saved_body).unwrap();
    std::fs::write(concat(&[&second.roster, ".sig"]), &saved_sig).unwrap();
    let report = run_join(&second, "m11", PAPER).expect("join with the pair in place");
    assert_eq!(report.roster_machines, vec!["m3", "m11"]);
}

#[test]
fn write_rest_refuses_a_roster_that_changed_since_the_plan_read_it() {
    let dir = scratch("roster-premise");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("setup");

    let mut second = paths.clone();
    second.key = dir.join("m11.key").to_str().unwrap().to_string();
    second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();

    let pre = preflight(Verb::Join, "m11", &second).expect("preflight");
    let seed = seed_of(PAPER);
    let planned = plan(pre, &seed, NOW).expect("plan against the current pair");

    let mut moved = std::fs::read(&paths.roster).unwrap();
    moved.extend_from_slice(b"\n# concurrently rewritten\n");
    std::fs::write(&paths.roster, &moved).unwrap();

    let err = write_rest(planned).unwrap_err();
    assert!(err.contains("CHANGED ON DISK"), "{err}");
    assert!(err.contains("NOTHING HAS BEEN WRITTEN"), "{err}");
    assert!(
        !std::path::Path::new(&second.key).exists(),
        "refused before the key write — the premise check must run FIRST"
    );
    assert_eq!(
        std::fs::read(&paths.roster).unwrap(),
        moved,
        "the concurrent actor's pair survives untouched"
    );

    let fresh_dir = scratch("roster-premise-fresh");
    let fresh_paths = paths_in(&fresh_dir);
    write_fixture(&fresh_paths);
    let pre = preflight(Verb::Setup, "m3", &fresh_paths).unwrap();
    let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
    write_pins(&planned).unwrap();
    std::fs::write(&fresh_paths.roster, b"someone else's first roster").unwrap();
    let err = write_rest(planned).unwrap_err();
    assert!(err.contains("APPEARED"), "{err}");
    assert!(
        !std::path::Path::new(&fresh_paths.key).exists(),
        "nothing written on the fresh-collision refusal either"
    );

    let control_dir = scratch("roster-premise-control");
    let control = paths_in(&control_dir);
    write_fixture(&control);
    run_setup(&control, "m3").expect("the same pipeline, unraced, completes");
}

#[test]
fn a_broken_signature_target_leaves_the_roster_document_unchanged() {
    let dir = scratch("pair-rollback");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("setup publishes the first pair");
    let sig_path = concat(&[&paths.roster, ".sig"]);
    let old_body = std::fs::read(&paths.roster).unwrap();
    let old_sig = std::fs::read(&sig_path).unwrap();
    let snapshot = RosterSnapshot {
        raw: old_body.clone(),
        sig: old_sig.clone(),
    };

    std::fs::remove_file(&sig_path).unwrap();
    std::fs::create_dir(&sig_path).unwrap();
    let lock = lock_roster(&paths.roster).unwrap();
    let err = publish_roster_locked(
        &lock,
        &paths.roster,
        Some(&snapshot),
        b"NEW BODY",
        b"NEW SIG",
    )
    .unwrap_err();
    assert!(err.contains("CHANGED ON DISK"), "{err}");
    assert!(!std::path::Path::new(&roster_txn_path(&paths.roster)).exists());
    drop(lock);
    assert_eq!(
        std::fs::read(&paths.roster).unwrap(),
        old_body,
        "the document must still be the pair's previous bytes"
    );
    std::fs::remove_dir(&sig_path).unwrap();
    std::fs::write(&sig_path, &old_sig).unwrap();

    let master_pub = seed_of(PAPER).pubkey_b64().unwrap();
    assert!(
        load_roster(
            &paths.roster,
            &master_pub,
            NOW,
            RosterExpectation::MustExist
        )
        .is_ok(),
        "the pair left behind by a failed publish must be the consistent old one"
    );

    let fresh = scratch("pair-rollback-fresh");
    let fresh_roster = fresh
        .join("aterm-machines.toml")
        .to_str()
        .unwrap()
        .to_string();
    let fresh_sig = concat(&[&fresh_roster, ".sig"]);
    std::fs::create_dir(&fresh_sig).unwrap();
    let fresh_lock = lock_roster(&fresh_roster).unwrap();
    let err = publish_roster_locked(&fresh_lock, &fresh_roster, None, b"BODY", b"SIG").unwrap_err();
    assert!(err.contains("APPEARED"), "{err}");
    assert!(!std::path::Path::new(&roster_txn_path(&fresh_roster)).exists());
    assert!(
        !std::path::Path::new(&fresh_roster).exists(),
        "a first publish that failed leaves nothing, not a signatureless document"
    );

    std::fs::remove_dir(&fresh_sig).unwrap();
    publish_roster_locked(&fresh_lock, &fresh_roster, None, b"BODY", b"SIG")
        .expect("publish with nothing in the way");
    drop(fresh_lock);
    assert_eq!(std::fs::read(&fresh_roster).unwrap(), b"BODY");
    assert_eq!(std::fs::read(&fresh_sig).unwrap(), b"SIG");

    let (guarded, other) = (fresh.join("dist/a.toml"), fresh.join("b.toml"));
    let guarded = guarded.to_str().unwrap();
    let lock = lock_roster(guarded).unwrap();
    assert!(publish_roster_locked(&lock, other.to_str().unwrap(), None, b"B", b"S").is_err());
    assert!(!other.exists());
    std::fs::create_dir(roster_txn_path(guarded)).unwrap();
    assert!(publish_roster_locked(&lock, guarded, None, b"A", b"S").is_err());
    assert!(!std::path::Path::new(guarded).exists());
}

#[test]
fn a_committed_roster_transaction_recovers_forward_after_a_body_only_crash() {
    let dir = scratch("pair-redo-recovery");
    let roster = dir.join("aterm-machines.toml");
    let roster = roster.to_str().unwrap();
    let sig_path = concat(&[roster, ".sig"]);
    std::fs::write(roster, b"old body").unwrap();
    std::fs::write(&sig_path, b"old sig").unwrap();

    let predecessor = RosterSnapshot {
        raw: b"old body".to_vec(),
        sig: b"old sig".to_vec(),
    };
    let recording = aterm_durable::seam::Recording::start();
    let transaction =
        begin_roster_transaction(roster, Some(&predecessor), b"new body", b"new sig").unwrap();
    let begun = recording.take();
    let (commit, rest) = begun.split_last().expect("flushes");
    let (staged, members) = rest.split_last().expect("the staged directory's flush");
    assert_eq!(*commit, aterm_durable::Flush::Durable, "{begun:?}");
    assert_eq!(*staged, aterm_durable::Flush::Ordered, "{begun:?}");
    assert!(
        !members.is_empty() && members.iter().all(|f| *f == aterm_durable::Flush::Durable),
        "{begun:?}"
    );
    write_bytes_atomic(roster, b"new body").unwrap();
    assert_eq!(
        std::fs::read(&sig_path).unwrap(),
        b"old sig",
        "the premise: a torn pair, which is what a verifier refuses"
    );

    for (half, old) in [("", &b"old body"[..]), (".sig", &b"old sig"[..])] {
        let lost = crate::fsio::window::with(
            "roster pair replayed",
            move |body| std::fs::write(concat(&[body, half]), old).unwrap(),
            || lock_roster(roster).map(drop),
        );
        assert!(lost.is_err(), "a replay that lost {half:?} is not retired");
        assert!(std::path::Path::new(&transaction).exists());
    }

    let _ = recording.take();
    let _lock = lock_roster(roster).expect("lock acquisition replays the committed redo");
    let replayed = recording.take();
    assert!(
        replayed.ends_with(&[aterm_durable::Flush::Ordered, aterm_durable::Flush::Ordered]),
        "the retirement and its collection are only ordered: {replayed:?}"
    );
    assert!(
        replayed.contains(&aterm_durable::Flush::Durable),
        "the replayed pair is durable: {replayed:?}"
    );
    assert_eq!(std::fs::read(roster).unwrap(), b"new body");
    assert_eq!(std::fs::read(&sig_path).unwrap(), b"new sig");
    assert!(
        !std::path::Path::new(&transaction).exists(),
        "a completed transaction is retired, so it cannot replay a second time"
    );
    let mut left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "aterm-machines.toml",
            "aterm-machines.toml.lock",
            "aterm-machines.toml.sig"
        ]
    );
}

#[test]
fn redo_recovery_never_overwrites_a_newer_or_unrelated_pair() {
    let dir = scratch("pair-redo-cas");
    let roster = dir.join("aterm-machines.toml");
    let roster = roster.to_str().unwrap();
    let sig_path = concat(&[roster, ".sig"]);
    std::fs::write(roster, b"old body").unwrap();
    std::fs::write(&sig_path, b"old sig").unwrap();
    drop(lock_roster(roster).expect("establish the writer rendezvous"));

    let predecessor = RosterSnapshot {
        raw: b"old body".to_vec(),
        sig: b"old sig".to_vec(),
    };
    let transaction = begin_roster_transaction(
        roster,
        Some(&predecessor),
        b"interrupted body",
        b"interrupted sig",
    )
    .unwrap();
    std::fs::write(roster, b"newer body").unwrap();
    std::fs::write(&sig_path, b"newer sig").unwrap();

    let err = lock_roster(roster)
        .err()
        .expect("replay over an unrelated pair is refused");
    assert!(err.contains("newer or unrelated"), "{err}");
    assert_eq!(std::fs::read(roster).unwrap(), b"newer body");
    assert_eq!(std::fs::read(&sig_path).unwrap(), b"newer sig");
    assert!(
        std::path::Path::new(&transaction).exists(),
        "the evidence stays on disk for the operator to reconcile"
    );

    std::fs::write(
        std::path::Path::new(&transaction).join("predecessor-state"),
        b"torn",
    )
    .unwrap();
    std::fs::remove_file(roster).unwrap();
    std::fs::remove_file(&sig_path).unwrap();
    assert!(lock_roster(roster).is_err());
    assert!(!std::path::Path::new(roster).exists());
}

#[test]
fn check_only_lock_never_creates_or_recovers_roster_state() {
    let dir = scratch("read-only-roster-lock");
    let roster = dir.join("aterm-machines.toml");
    let roster = roster.to_str().unwrap();
    let lock_path = concat(&[roster, ".lock"]);
    let sig_path = concat(&[roster, ".sig"]);

    let err = lock_roster_read_only(roster)
        .err()
        .expect("an absent rendezvous is refused, not created");
    assert!(err.contains("without creating it"), "{err}");
    assert!(
        !std::path::Path::new(&lock_path).exists(),
        "an observer left no rendezvous file behind"
    );

    let lock_target = dir.join("lock-target");
    std::fs::write(&lock_target, b"").unwrap();
    std::os::unix::fs::symlink(&lock_target, &lock_path).unwrap();
    let err = lock_roster_read_only(roster)
        .err()
        .expect("a symlink at .lock is refused");
    assert!(err.contains("without creating it"), "{err}");
    std::fs::remove_file(&lock_path).unwrap();

    std::fs::create_dir(&lock_path).unwrap();
    let err = lock_roster_read_only(roster)
        .err()
        .expect("a directory at .lock is refused");
    assert!(err.contains("without creating it"), "{err}");
    std::fs::remove_dir(&lock_path).unwrap();

    std::fs::write(roster, b"old body").unwrap();
    std::fs::write(&sig_path, b"old sig").unwrap();
    drop(lock_roster(roster).expect("establish the writer rendezvous"));
    drop(lock_roster_read_only(roster).expect("an existing clean rendezvous is readable"));
    let swapped = crate::fsio::window::with(
        "read-only claim locked",
        |lock| {
            let fresh = concat(&[lock, ".new"]);
            std::fs::write(&fresh, b"").unwrap();
            std::fs::rename(&fresh, lock).unwrap();
        },
        || lock_roster_read_only(roster).map(drop),
    );
    assert!(swapped.is_err(), "a replaced rendezvous is refused");

    let predecessor = RosterSnapshot {
        raw: b"old body".to_vec(),
        sig: b"old sig".to_vec(),
    };
    let transaction =
        begin_roster_transaction(roster, Some(&predecessor), b"new body", b"new sig").unwrap();
    let err = lock_roster_read_only(roster)
        .err()
        .expect("a pending transaction is reported, not replayed");
    assert!(err.contains("never replays"), "{err}");
    assert_eq!(std::fs::read(roster).unwrap(), b"old body");
    assert_eq!(std::fs::read(&sig_path).unwrap(), b"old sig");
    assert!(std::path::Path::new(&transaction).exists());
}

#[test]
fn a_pending_roster_transaction_is_named_instead_of_a_mistyped_phrase() {
    let dir = scratch("pending-txn-read");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("setup publishes the first pair");
    let master_pub = seed_of(PAPER).pubkey_b64().unwrap();
    let sig_path = concat(&[&paths.roster, ".sig"]);

    let predecessor = RosterSnapshot {
        raw: std::fs::read(&paths.roster).unwrap(),
        sig: std::fs::read(&sig_path).unwrap(),
    };
    begin_roster_transaction(&paths.roster, Some(&predecessor), b"new body", b"new sig").unwrap();
    write_bytes_atomic(&paths.roster, b"new body").unwrap();

    let err = load_roster(
        &paths.roster,
        &master_pub,
        NOW,
        RosterExpectation::MustExist,
    )
    .unwrap_err();
    assert!(err.contains("interrupted roster transaction"), "{err}");
    assert!(err.contains("is not what is wrong"), "{err}");
    assert!(
        !err.contains("does not verify under the master you typed"),
        "the phrase must not be blamed for a torn pair: {err}"
    );

    drop(lock_roster(&paths.roster).expect("the writer lock recovers the pair"));
    assert_eq!(std::fs::read(&paths.roster).unwrap(), b"new body");
    assert_eq!(std::fs::read(&sig_path).unwrap(), b"new sig");
    let err = load_roster(
        &paths.roster,
        &master_pub,
        NOW,
        RosterExpectation::MustExist,
    )
    .unwrap_err();
    assert!(
        err.contains("does not verify under the master you typed"),
        "the recovered pair is read normally — and this fixture's bytes are not a \
         real roster, so the ordinary signature refusal is what should surface: {err}"
    );
}

#[test]
fn plan_reproves_the_anchor_even_when_the_orchestrator_skips_verify_master() {
    let dir = scratch("plan-anchor-reproof");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("setup");

    let attacker_dir = scratch("plan-anchor-reproof-attacker");
    let attacker_paths = paths_in(&attacker_dir);
    write_fixture(&attacker_paths);
    let pre = preflight(Verb::Setup, "mx", &attacker_paths).unwrap();
    let wrong = seed_of(OTHER_PAPER);
    let planned = plan(pre, &wrong, NOW).unwrap();
    write_pins(&planned).unwrap();
    write_rest(planned).expect("the attacker's own lineage sets up fine");

    let mut second = paths.clone();
    second.roster = attacker_paths.roster.clone();
    second.key = dir.join("m11.key").to_str().unwrap().to_string();
    second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();
    let pre = preflight(Verb::Join, "m11", &second).expect("roster file exists");
    let err = plan(pre, &wrong, NOW).unwrap_err();
    assert!(
        err.contains("is NOT the master committed"),
        "the anchor containment must refuse inside plan itself: {err}"
    );
    assert!(!std::path::Path::new(&second.key).exists());

    let mut honest = paths.clone();
    honest.key = dir.join("m11b.key").to_str().unwrap().to_string();
    honest.machine_pub = dir.join("m11b.toml").to_str().unwrap().to_string();
    let pre = preflight(Verb::Join, "m11", &honest).unwrap();
    assert!(plan(pre, &seed_of(PAPER), NOW).is_ok());
}

#[test]
fn a_run_creates_the_directories_its_key_and_roster_need() {
    let dir = scratch("missing-dirs");
    let paths = Paths {
        pins: dir.join("pins.rs").to_str().unwrap().to_string(),
        roster: dir
            .join("dist/aterm-machines.toml")
            .to_str()
            .unwrap()
            .to_string(),
        key: dir
            .join("home/.aterm/release-ring/machine.key")
            .to_str()
            .unwrap()
            .to_string(),
        machine_pub: dir
            .join("home/.aterm/release-ring/machine.toml")
            .to_str()
            .unwrap()
            .to_string(),
        pins_explicit: true,
    };
    write_fixture(&paths);
    assert!(
        !dir.join("home/.aterm").exists(),
        "the premise: no $HOME/.aterm"
    );

    let report = run_setup(&paths, "m3").expect("setup completes on a fresh machine");

    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(&paths.key).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        crate::pubkey_b64(&std::fs::read(&paths.key).unwrap()).unwrap(),
        report.machine_pubkey,
        "the roster names a key whose private half exists"
    );
    assert!(std::path::Path::new(&paths.roster).exists());
    assert!(std::path::Path::new(&paths.machine_pub).exists());
}

#[test]
fn the_key_is_written_before_the_roster_that_names_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("key-before-roster");
    let mut paths = paths_in(&dir);
    let roster_dir = dir.join("dist");
    paths.roster = roster_dir.join("r.toml").to_str().unwrap().to_string();
    write_fixture(&paths);

    let pre = preflight(Verb::Setup, "m3", &paths).expect("preflight");
    let planned = plan(pre, &seed_of(PAPER), NOW).expect("plan");
    write_pins(&planned).expect("the anchor is written");
    std::fs::write(concat(&[&paths.roster, ".lock"]), b"").unwrap();
    std::fs::set_permissions(&roster_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = write_rest(planned);
    std::fs::set_permissions(&roster_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    let err = result.unwrap_err();
    assert!(err.contains("Nothing authorizes this machine yet"), "{err}");
    assert!(
        err.contains("git checkout --"),
        "the recovery names both files: {err}"
    );
    assert!(err.contains("rm "), "{err}");
    assert!(
        std::path::Path::new(&paths.key).exists(),
        "the key was written first, which is what makes the recovery two commands"
    );
    assert!(
        !std::path::Path::new(&paths.roster).exists()
            && !std::path::Path::new(&concat(&[&paths.roster, ".sig"])).exists(),
        "neither half of a pair that could not publish exists on disk"
    );
}

#[test]
fn a_record_that_cannot_be_written_fails_the_run_before_the_roster_is_published() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("record-write-fails");
    let mut paths = paths_in(&dir);
    let record_dir = dir.join("home");
    paths.machine_pub = record_dir
        .join("machine.toml")
        .to_str()
        .unwrap()
        .to_string();
    write_fixture(&paths);

    let pre = preflight(Verb::Setup, "m3", &paths).expect("preflight");
    let planned = plan(pre, &seed_of(PAPER), NOW).expect("plan");
    write_pins(&planned).expect("the anchor is written");
    assert!(
        record_dir.is_dir(),
        "the premise: preflight created the record's directory"
    );
    std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = write_rest(planned);
    std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    let err = result.unwrap_err();
    assert!(
        err.contains(&paths.machine_pub),
        "the message names the file that could not be written: {err}"
    );
    assert!(err.contains("Nothing authorizes this machine yet"), "{err}");
    assert!(err.contains("rm "), "the recovery undoes the key: {err}");
    assert!(
        err.contains(
            "destroy the paper you just wrote, fix the problem above, and run \
             `atpkg-keys setup` again."
        ),
        "setup armed the anchor, so its retry starts over: {err}"
    );
    assert!(
        std::path::Path::new(&paths.key).exists(),
        "the key was written first, which is what makes the recovery the two commands \
         the message names"
    );
    assert!(
        !std::path::Path::new(&paths.roster).exists()
            && !std::path::Path::new(&concat(&[&paths.roster, ".sig"])).exists(),
        "NO signed document names a machine whose record could not be written"
    );
    assert!(
        !std::path::Path::new(&paths.machine_pub).exists(),
        "and no half-written record is left behind"
    );
}

#[test]
fn a_failure_that_authorized_nothing_undoes_only_what_its_verb_did() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("retry-names-the-verb");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("setup");

    let mut second = paths.clone();
    second.key = dir.join("m11.key").to_str().unwrap().to_string();
    let record_dir = dir.join("m11-home");
    second.machine_pub = record_dir
        .join("machine.toml")
        .to_str()
        .unwrap()
        .to_string();
    let pre = preflight(Verb::Join, "m11", &second).expect("preflight");
    let planned = plan(pre, &seed_of(PAPER), NOW).expect("plan");
    write_pins(&planned).expect("a join leaves the anchor as it is");
    std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = write_rest(planned);
    std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let err = result.unwrap_err();
    assert!(err.contains("Nothing authorizes this machine yet"), "{err}");
    assert!(
        err.ends_with(&concat(&["`rm ", &second.key, "`."])),
        "a join undoes its key and retries: {err}"
    );
    assert!(
        !err.contains("git checkout") && !err.contains("destroy the paper"),
        "a join never armed the anchor: {err}"
    );

    let dir = scratch("retry-setup-premise");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    let pre = preflight(Verb::Setup, "m3", &paths).expect("preflight");
    let planned = plan(pre, &seed_of(PAPER), NOW).expect("plan");
    write_pins(&planned).expect("the anchor is written");
    std::fs::write(&paths.roster, b"# appeared after the plan\n").unwrap();
    let err = write_rest(planned).unwrap_err();
    assert!(
        err.contains("The anchor is armed but nothing else was written: `git checkout -- "),
        "{err}"
    );
    assert!(!err.contains("NOTHING HAS BEEN WRITTEN"), "{err}");
    assert!(
        !std::path::Path::new(&paths.key).exists(),
        "refused before the key write"
    );
}

#[test]
fn write_pins_refuses_a_file_that_changed_under_it() {
    let dir = scratch("stale-snapshot");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    let pre = preflight(Verb::Setup, "m3", &paths).unwrap();

    const REPLACEMENT: &str = "Edited by a reviewed commit that landed in flight.";
    let concurrent = PINS_FIXTURE.replace("Empty here, and therefore INERT.", REPLACEMENT);
    assert_ne!(concurrent, PINS_FIXTURE, "the fixture must actually differ");
    std::fs::write(&paths.pins, &concurrent).unwrap();

    let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
    let err = write_pins(&planned).unwrap_err();
    assert!(err.message.contains("CHANGED ON DISK"), "{err}");
    assert!(err.message.contains("NOTHING HAS BEEN WRITTEN"), "{err}");
    assert!(!err.landed, "a refused write leaves the anchor unarmed");
    assert_eq!(
        std::fs::read_to_string(&paths.pins).unwrap(),
        concurrent,
        "the concurrent edit survives"
    );

    let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
    let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
    write_pins(&planned).expect("a plan built from the current bytes writes");
    let after = std::fs::read_to_string(&paths.pins).unwrap();
    assert!(
        after.contains(REPLACEMENT),
        "the concurrent edit was not reverted: {after}"
    );
}

#[test]
fn write_pins_says_whether_a_failed_write_reached_the_anchor() {
    let dir = scratch("failed-write");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
    let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
    let saved = dir.join("pins-before-rename.rs");
    let (pins, backup) = (paths.pins.clone(), saved.clone());
    let err = crate::fsio::window::with(
        "temp name drawn",
        move |_| {
            std::fs::rename(&pins, &backup).unwrap();
            std::fs::create_dir(&pins).unwrap();
        },
        || write_pins(&planned),
    )
    .unwrap_err();
    std::fs::remove_dir(&paths.pins).unwrap();
    std::fs::rename(&saved, &paths.pins).unwrap();
    assert!(
        !err.landed,
        "a refused rename did not publish this run's bytes: {err}"
    );
    assert_eq!(std::fs::read_to_string(&paths.pins).unwrap(), PINS_FIXTURE);

    for restore_original in [false, true] {
        write_fixture(&paths);
        let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
        let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
        let expected = planned.pins_text.as_deref().unwrap();
        let err = write_pins_with_sync(&planned, |pins| {
            assert_eq!(std::fs::read_to_string(pins).unwrap(), expected);
            if restore_original {
                std::fs::write(pins, PINS_FIXTURE).unwrap();
            }
            Err(std::io::Error::other("injected parent sync failure"))
        })
        .unwrap_err();
        assert!(
            err.landed,
            "an actual rename is reported even after restoration: {err}"
        );
        assert!(
            err.message.contains("injected parent sync failure"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(&paths.pins).unwrap(),
            if restore_original {
                PINS_FIXTURE
            } else {
                expected
            },
        );
    }
    assert!(!std::path::Path::new(&paths.roster).exists());
    assert!(!std::path::Path::new(&paths.key).exists());
    assert!(!std::path::Path::new(&paths.machine_pub).exists());
}

#[test]
fn join_refuses_a_phrase_that_does_not_match_the_committed_anchor() {
    let dir = scratch("join-wrong-paper");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("setup");

    let mut second = paths.clone();
    second.key = dir.join("m11.key").to_str().unwrap().to_string();
    second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();

    let before = std::fs::read_to_string(&paths.pins).unwrap();
    let roster_before = std::fs::read(&paths.roster).unwrap();

    let pre = preflight(Verb::Join, "m11", &second).expect("preflight is fine");
    let wrong = seed_of(OTHER_PAPER);
    let err = verify_master(&pre, &wrong).unwrap_err();
    assert!(err.contains("is NOT the master committed"), "{err}");
    assert!(err.contains("Nothing has been written"), "{err}");

    let err = plan(pre, &wrong, NOW).unwrap_err();
    assert!(err.contains("is NOT the master committed"), "{err}");

    let err = load_roster(
        &paths.roster,
        &wrong.pubkey_b64().unwrap(),
        NOW,
        RosterExpectation::MustExist,
    )
    .unwrap_err();
    assert!(
        err.contains("does not verify under the master you typed"),
        "{err}"
    );

    assert_eq!(std::fs::read_to_string(&paths.pins).unwrap(), before);
    assert_eq!(std::fs::read(&paths.roster).unwrap(), roster_before);
    assert!(
        !std::path::Path::new(&second.key).exists(),
        "no key was minted"
    );

    let pre = preflight(Verb::Join, "m11", &second).unwrap();
    assert!(verify_master(&pre, &seed_of(PAPER)).is_ok());
}

#[test]
fn join_refuses_when_no_master_is_committed() {
    let dir = scratch("join-unarmed");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    let err = preflight(Verb::Join, "m3", &paths).unwrap_err();
    assert!(err.contains("no paper master is committed"), "{err}");
    assert!(err.contains("setup"), "{err}");
    assert!(preflight(Verb::Setup, "m3", &paths).is_ok());
}

#[test]
fn a_second_pass_over_an_already_listed_key_adds_no_second_entry() {
    let dir = scratch("idempotent-verb");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    let report = run_setup(&paths, "m3").expect("setup");

    let src = std::fs::read_to_string(&paths.pins).unwrap();
    let edit = append_member(
        &src,
        MASTER_ANCHOR,
        &report.master_pubkey,
        &["a re-run"],
        MAX_MASTER_MEMBERS,
    )
    .unwrap();
    assert!(
        matches!(edit, Edit::AlreadyPresent { .. }),
        "the master is already there; a re-run must be a no-op"
    );
    assert_eq!(
        src.matches(report.master_pubkey.as_str()).count(),
        1,
        "exactly one entry for this master"
    );
    assert!(!src.contains(report.machine_pubkey.as_str()));
}

#[test]
fn an_existing_machine_key_is_refused_before_anything_is_generated() {
    let dir = scratch("existing-key");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("setup");
    let err = preflight(Verb::Join, "m3", &paths).unwrap_err();
    assert!(err.contains("already exists"), "{err}");
    assert!(
        err.contains("never overwrite a key the roster still names"),
        "{err}"
    );

    let mut dangling = paths.clone();
    dangling.key = dir.join("dangling.key").to_str().unwrap().to_string();
    std::os::unix::fs::symlink(dir.join("absent.key"), &dangling.key).unwrap();
    let err = preflight(Verb::Join, "m4", &dangling).unwrap_err();
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn setup_refuses_to_start_over_an_existing_roster() {
    let dir = scratch("setup-roster-exists");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    std::fs::write(&paths.roster, b"schema = 1\n").unwrap();
    let err = preflight(Verb::Setup, "m3", &paths).unwrap_err();
    assert!(err.contains("roster already exists"), "{err}");

    std::fs::remove_file(&paths.roster).unwrap();
    std::os::unix::fs::symlink(dir.join("absent.toml"), &paths.roster).unwrap();
    let err = preflight(Verb::Setup, "m3", &paths).unwrap_err();
    assert!(err.contains("roster already exists"), "{err}");
    std::fs::remove_file(&paths.roster).unwrap();

    std::fs::write(concat(&[&paths.roster, ".sig"]), b"sig").unwrap();
    let master = seed_of(PAPER).pubkey_b64().unwrap();
    let expect = RosterExpectation::MayCreateFresh;
    assert!(load_roster(&paths.roster, &master, NOW, expect).is_err());

    std::fs::remove_file(concat(&[&paths.roster, ".sig"])).unwrap();
    std::os::unix::fs::symlink(dir.join("absent.sig"), concat(&[&paths.roster, ".sig"])).unwrap();
    assert!(load_roster(&paths.roster, &master, NOW, expect).is_err());
}

#[test]
fn a_machine_id_is_vetted_at_its_bounds() {
    let (longest, too_long) = ("a".repeat(32), "a".repeat(33));
    for (id, ok) in [
        ("m3-2026b_x", true),
        (longest.as_str(), true),
        (too_long.as_str(), false),
        ("", false),
        ("m 3", false),
        ("m3\u{e9}", false),
    ] {
        assert_eq!(vet_machine_id(id).is_ok(), ok, "{id:?}");
    }
}

#[test]
fn the_pins_file_is_discovered_from_inside_the_checkout() {
    let package = aterm_tempfile::manifest_dir();
    let found = discover_pins_path_from(package.clone()).unwrap();
    let shipped = package.join("../aterm-update-core/src/pins.rs");
    assert_eq!(
        std::fs::canonicalize(found).unwrap(),
        std::fs::canonicalize(shipped).unwrap()
    );

    let dir = scratch("discover-pins-symlink");
    let fake_pins = dir.join(PINS_REL);
    std::fs::create_dir_all(fake_pins.parent().unwrap()).unwrap();
    let target = dir.join("real-pins.rs");
    std::fs::write(&target, PINS_FIXTURE).unwrap();
    std::os::unix::fs::symlink(&target, &fake_pins).unwrap();
    assert!(
        discover_pins_path_from(dir.to_path_buf()).is_err(),
        "a symlinked pins.rs is not auto-discovered"
    );
}

#[test]
fn an_unrecognised_pins_file_stops_the_run_at_preflight() {
    let dir = scratch("bad-pins");
    let paths = paths_in(&dir);
    std::fs::write(&paths.pins, "// nothing resembling an anchor\n").unwrap();
    let err = preflight(Verb::Setup, "m3", &paths).unwrap_err();
    assert!(err.contains("will not guess"), "{err}");
}

#[test]
fn write_pins_reports_a_write_that_did_not_land() {
    let dir = scratch("verify-write");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
    let seed = seed_of(PAPER);
    let planned = plan(pre, &seed, NOW).unwrap();

    let landed = crate::fsio::window::with(
        "anchor file written",
        |pins| {
            let ours = std::fs::read_to_string(pins).unwrap();
            let theirs = ours.replace("therefore INERT.", "edited in the gap.");
            assert_ne!(theirs, ours, "the edit must change the file");
            std::fs::write(pins, theirs).unwrap();
        },
        || write_pins(&planned),
    );
    assert!(
        landed.is_err_and(|e| e.landed),
        "a foreign edit after the write is reported, and this run's bytes landed"
    );
    std::fs::write(&paths.pins, PINS_FIXTURE).unwrap();

    write_pins(&planned).expect("the ordinary write verifies");

    std::fs::write(&paths.pins, PINS_FIXTURE).unwrap();
    let mut no_write = {
        let pre = preflight(
            Verb::Setup,
            "m3-b",
            &Paths {
                key: dir.join("m3b.key").to_str().unwrap().to_string(),
                ..paths.clone()
            },
        )
        .unwrap();
        plan(pre, &seed, NOW).unwrap()
    };
    no_write.pins_text = None;
    let err = write_pins(&no_write).unwrap_err();
    assert!(
        err.message.contains("holds 0 keys but 1 were intended"),
        "{err}"
    );
    assert!(!err.landed, "a run with nothing to write wrote nothing");
}

#[test]
fn the_report_carries_no_secret_material() {
    let dir = scratch("report");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    let report = run_setup(&paths, "m3").expect("setup");
    let text = render_report(&report).join("\n");
    assert!(text.contains(&report.machine_pubkey), "{text}");
    assert!(!text.contains(PAPER), "{text}");
    assert!(!text.contains(&PAPER[..16]), "{text}");
}

#[test]
fn debug_renderings_carry_no_secret_material() {
    let dir = scratch("debug-redaction");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
    assert_eq!(format!("{pre:?}"), "Preflight(setup, m3)");
    let seed = seed_of(PAPER);
    let planned = plan(pre, &seed, NOW).unwrap();
    let rendered = format!("{planned:?}");
    assert_eq!(rendered, "Planned(setup, m3, secret=<redacted>)");
    assert!(!rendered.contains(PAPER), "{rendered}");
    assert_eq!(format!("{seed:?}"), "MasterSeed(<redacted>)");
}

#[test]
fn no_file_written_by_a_run_contains_the_phrase_or_its_seed() {
    let dir = scratch("no-phrase-on-disk");
    let paths = paths_in(&dir);
    write_fixture(&paths);
    run_setup(&paths, "m3").expect("setup");

    const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut raw = Vec::new();
    let mut acc: u32 = 0;
    let mut nbits = 0u32;
    for &c in PAPER.as_bytes() {
        let v = ALPHABET
            .iter()
            .position(|a| *a == c)
            .expect("fixture in alphabet") as u32;
        acc = (acc << 5) | v;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            raw.push(((acc >> nbits) & 0xff) as u8);
        }
    }
    assert_eq!(raw.len(), 32);

    let mut checked = 0usize;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        checked += 1;
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(PAPER), "{path:?} contains the phrase");
        for start in 0..(PAPER.len() - 16) {
            assert!(
                !text.contains(&PAPER[start..start + 16]),
                "{path:?} contains a 16-character run of the phrase"
            );
        }
        assert!(
            !bytes.windows(32).any(|w| w == raw.as_slice()),
            "{path:?} contains the raw master seed"
        );
    }
    assert!(
        checked >= 4,
        "the run must have written pins.rs, the roster, its .sig and the key; saw {checked}"
    );
}

#[test]
fn home_path_from_rejects_unset_empty_or_root_only_home_and_trims_trailing_slashes() {
    assert_eq!(
        home_path_from(None, ".aterm/release-ring/m3.key"),
        Err("HOME is not set".to_string())
    );
    assert_eq!(
        home_path_from(Some(""), ".aterm/release-ring/m3.key"),
        Err("HOME is not set".to_string())
    );
    assert_eq!(
        home_path_from(Some("/"), ".aterm/release-ring/m3.key"),
        Err("HOME is not set".to_string())
    );
    assert_eq!(
        home_path_from(Some("///"), ".aterm/release-ring/m3.key"),
        Err("HOME is not set".to_string())
    );
    assert_eq!(
        home_path_from(Some("/Users//owner"), ".aterm/release-ring/m3.key"),
        Ok("/Users//owner/.aterm/release-ring/m3.key".to_string())
    );
    assert_eq!(
        home_path_from(Some("/Users//owner/"), ".aterm/release-ring/m3.key"),
        Ok("/Users//owner/.aterm/release-ring/m3.key".to_string())
    );
}

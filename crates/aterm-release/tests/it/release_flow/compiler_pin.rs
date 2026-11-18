// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::release::CompilerPin;

fn pin(root: char) -> CompilerPin {
    CompilerPin {
        build: 9192,
        tree_root: root.to_string().repeat(64),
        compiler_commit: prover().commit,
        receipt: "SEED-MINT sha256:00".into(),
        stock_toolchain: "1.99.1".into(),
    }
}

fn prepared() -> World {
    let mut world = World::new();
    world.versions.insert(TIP.into(), "0.6.0".into());
    world.compiler_pins.insert(TIP.into(), pin('a'));
    let (code, last) = world.run(ReleaseOptions {
        commit: Some(TIP.into()),
        prepare: true,
        ..mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", world.out);
    assert_eq!(world.journal().unwrap().compiler, Some(pin('a')));
    assert_eq!(
        world.journal().unwrap().prepared.unwrap().compiler,
        Some(pin('a'))
    );
    world
}

#[test]
fn exact_selection_hands_its_compiler_to_the_cutter_including_dry_judgments() {
    for dry_run in [false, true] {
        let mut world = World::new();
        world.versions.insert(TIP.into(), "0.6.0".into());
        world.prover.compiler = Some(pin('a'));
        world.compiler_pins.insert(TIP.into(), pin('b'));
        world.own_cutter = false;
        world.exec_result = Ok(());
        let (code, last) = world.run(ReleaseOptions {
            commit: Some(TIP.into()),
            dry_run,
            ..mac()
        });
        assert_eq!(code, 0, "dry={dry_run}: {last}\n{:#?}", world.out);
        assert_eq!(world.exec_commit.as_deref(), Some(TIP));
        assert_eq!(world.exec_prover.unwrap().compiler, Some(pin('b')));
    }
}

#[test]
fn same_compiler_commit_with_another_root_cannot_relabel_prepared_work() {
    let mut world = prepared();
    let journal = world.release_dir().join(JOURNAL_FILE);
    let before = std::fs::read(&journal).unwrap();
    world.compiler_pins.insert(TIP.into(), pin('b'));
    world.calls.clear();
    let (code, last) = world.run(mac());
    assert_ne!(code, 0, "{last}");
    assert!(last.contains("compiler declaration"), "{last}");
    assert_eq!(std::fs::read(&journal).unwrap(), before);
    for never in ["exec-cutter", "build", "publish", "cut"] {
        assert_eq!(world.count(never), 0, "{never}: {:?}", world.calls);
    }
}

#[test]
fn a_historical_preparation_without_a_pin_has_no_reuse_authority() {
    let mut world = prepared();
    let path = world.release_dir().join(JOURNAL_FILE);
    let mut journal = world.journal().unwrap();
    journal.compiler = None;
    journal.prepared.as_mut().unwrap().compiler = None;
    journal.save(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    world.calls.clear();
    let (code, last) = world.run(mac());
    assert_ne!(code, 0, "{last}");
    assert!(last.contains("historical journal"), "{last}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(world.count("build"), 0);
    assert_eq!(world.count("cut"), 0);
}

#[test]
fn the_same_candidate_and_complete_pin_resume_prepared_work_without_rebuilding() {
    let mut world = prepared();
    let artifact = world.journal().unwrap().artifact.unwrap();
    world.calls.clear();
    let (code, last) = world.run(mac());
    assert_eq!(code, 0, "{last}\n{:#?}", world.out);
    let journal = world.journal().unwrap();
    assert!(journal.finished(), "{journal:?}");
    assert_eq!(journal.compiler, Some(pin('a')));
    assert_eq!(journal.artifact, Some(artifact));
    for reused in ["build", "verify --tree", "verify --bundle"] {
        assert_eq!(world.count(reused), 0, "{reused}: {:?}", world.calls);
    }
    assert!(world.cut.as_ref().is_some_and(|cut| cut.finished));
}

#[test]
fn a_finished_foreign_version_does_not_select_the_next_candidates_compiler() {
    let mut world = prepared();
    let mut journal = world.journal().unwrap();
    journal.done.extend(["check".into(), "tap-aligned".into()]);
    journal
        .save(&world.release_dir().join(JOURNAL_FILE))
        .unwrap();
    assert!(journal.finished());
    world.history.push(NEXT.into());
    world.versions.insert(NEXT.into(), "0.7.0".into());
    world.compiler_pins.insert(NEXT.into(), pin('b'));
    world.own_cutter = false;
    world.exec_result = Ok(());
    let (code, last) = world.run(ReleaseOptions {
        commit: Some(NEXT.into()),
        ..mac()
    });
    assert_eq!(code, 0, "{last}\n{:#?}", world.out);
    assert_eq!(world.exec_commit.as_deref(), Some(NEXT));
    assert_eq!(world.exec_prover.unwrap().compiler, Some(pin('b')));
}

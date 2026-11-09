// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{fixture, opts};
use crate::harness::{
    native_act::{Decision, ImagePhase, Reply, Request},
    native_record, native_restart,
    upgrade::Phase,
    upgrade_drive::{self, St},
};
use aterm_spec::interp::State;

#[path = "native_record_identity_tests.rs"]
mod identity;

fn level(value: i64, phase: ImagePhase) -> St {
    if value == 0 {
        return St::default();
    }
    let (mut st, _) = fixture(phase);
    st.native_retired_unissued = false;
    if value == 1 {
        st.native = None;
    }
    st
}

fn read_level(st: Option<&St>) -> i64 {
    match st {
        Some(st) if st.native.is_some() => 2,
        Some(st) if !st.native_prepare.is_empty() => 1,
        _ => 0,
    }
}

fn expected_level(st: &St, phase: ImagePhase) -> i64 {
    (0..=2)
        .find(|value| native_record::expected_matches(st, &model_level(*value, phase)))
        .expect("a baseline in the model's domain")
}

fn model_level(value: i64, phase: ImagePhase) -> St {
    let mut st = level(value, phase);
    st.native_coordination = true;
    st
}

fn model_id(generation: i64) -> Option<String> {
    (generation != 0).then(|| format!("{generation:032x}"))
}

#[test]
fn stale_clone_cannot_erase_admission_phase_or_coordination() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let root = aterm_tempfile::tempdir().unwrap();
        let opts = opts(root.path());
        let mut current = level(1, phase);
        upgrade_drive::save_checked(&opts, "session", &current).unwrap();
        let mut stale = current.clone();
        current.native = level(2, phase).native;
        upgrade_drive::save_checked(&opts, "session", &current).unwrap();
        stale.wait = "a late status update".into();
        assert_eq!(
            upgrade_drive::save_checked(&opts, "session", &stale)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        upgrade_drive::restore(&opts, "session", Some(&stale));
        upgrade_drive::restore(&opts, "session", None);
        upgrade_drive::remove(&opts, "session");
        let kept = upgrade_drive::load(&opts, "session").unwrap();
        assert_eq!(kept.native, current.native);
        assert_eq!(kept.native_prepare, current.native_prepare);
        assert!(kept.native_coordination);
    }
}

#[test]
fn independent_new_preparations_have_one_winner() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let plain = St::default();
    upgrade_drive::save_checked(&opts, "session", &plain).unwrap();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (1..=2)
        .map(|id| {
            let mut st = level(1, ImagePhase::Forward);
            let Some(Request::Prepare(mut request)) = Request::parse(&st.native_prepare) else {
                panic!("prepare")
            };
            request.id = format!("{id:032x}");
            st.native_prepare = Request::Prepare(request).encode().unwrap();
            st.native_record = plain.native_record.clone();
            let opts = opts.clone();
            let gate = std::sync::Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.wait();
                upgrade_drive::save_checked(&opts, "session", &st).is_ok()
            })
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
}

#[test]
fn a_lost_barrier_retries_exactly_without_refreshing_older_clones() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let mut current = level(1, ImagePhase::Forward);
    let stale = current.clone();
    let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert!(upgrade_drive::save_checked(&opts, "session", &current).is_err());
    assert!(!native_record::expected_matches(
        &current,
        &level(1, ImagePhase::Forward)
    ));
    drop(fault);
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    assert!(native_record::expected_matches(
        &current,
        &level(1, ImagePhase::Forward)
    ));
    assert!(!native_record::expected_matches(
        &stale,
        &level(1, ImagePhase::Forward)
    ));
    current.native = level(2, ImagePhase::Forward).native;
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    assert!(upgrade_drive::save_checked(&opts, "session", &stale).is_err());
}

#[test]
fn positive_cancellation_can_close_the_current_snapshot_and_blocks_its_replay() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let mut current = level(2, ImagePhase::Forward);
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    let stale = current.clone();
    assert_eq!(
        native_restart::terminal_for_test(
            &mut current,
            &Reply::Decision(Decision::ProvenNotCommittedRollbackSafe)
        ),
        Some(native_restart::Recovery::Cancelled)
    );
    native_record::restore_inactive(&opts, "session", &mut current, Some(&St::default())).unwrap();
    assert!(upgrade_drive::save_checked(&opts, "session", &stale).is_err());
    upgrade_drive::remove(&opts, "session");
    let kept = upgrade_drive::load(&opts, "session").unwrap();
    assert!(kept.native.is_none() && kept.native_prepare.is_empty());
    assert!(kept.native_coordination);
}

#[test]
fn restore_keeps_newer_move_history_and_a_sticky_native_marker() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let mut current = St {
        native_coordination: true,
        phase: Phase::Done,
        moved_at: 7,
        ..St::default()
    };
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    let prior = current.clone();
    current.moved_at = 9;
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    upgrade_drive::restore(&opts, "session", Some(&prior));
    let kept = upgrade_drive::load(&opts, "session").unwrap();
    assert_eq!(kept.moved_at, 9);
    upgrade_drive::remove(&opts, "session");
    assert!(
        upgrade_drive::load(&opts, "session")
            .unwrap()
            .native_coordination
    );
}

#[test]
fn equal_native_fields_do_not_refresh_a_stale_writer() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let mut current = model_level(0, ImagePhase::Forward);
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    let mut stale = current.clone();
    current.wait = "newer status".into();
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    stale.wait = "older status".into();
    assert!(upgrade_drive::save_checked(&opts, "session", &stale).is_err());
    assert_eq!(
        upgrade_drive::load(&opts, "session").unwrap().wait,
        "newer status"
    );
    assert!(native_record::generation(&stale) < native_record::generation(&current));
}

#[test]
fn a_failed_attempt_belongs_only_to_its_original_writer() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let current = model_level(1, ImagePhase::Forward);
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    let before = native_record::generation(&current);
    let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert!(upgrade_drive::save_checked(&opts, "session", &current).is_err());
    let clone = current.clone();
    drop(fault);
    assert!(upgrade_drive::save_checked(&opts, "session", &clone).is_err());
    assert_eq!(native_record::generation(&clone), before);
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    assert_eq!(native_record::generation(&current), before + 1);
}

#[test]
fn a_pre_rename_failure_cannot_claim_another_writers_identical_native_state() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let initial = model_level(0, ImagePhase::Forward);
    upgrade_drive::save_checked(&opts, "session", &initial).unwrap();
    let generation = native_record::generation(&initial);
    let mut first = initial.clone();
    let mut second = initial.clone();
    first.wait = "first writer".into();
    second.wait = "second writer".into();
    let fault = aterm_uds::atomic_file::publication_seam::FailBeforeRename::with_kind(
        std::io::ErrorKind::StorageFull,
    );
    assert!(upgrade_drive::save_checked(&opts, "session", &first).is_err());
    drop(fault);
    let unchanged = upgrade_drive::load(&opts, "session").unwrap();
    assert_eq!(native_record::generation(&unchanged), generation);
    assert_eq!(unchanged.wait, initial.wait);
    let attempted = native_record::attempted_id(&first).unwrap();
    upgrade_drive::save_checked(&opts, "session", &second).unwrap();
    assert_eq!(native_record::generation(&second), generation + 1);
    assert_ne!(native_record::write_id(&second).as_ref(), Some(&attempted));
    assert_eq!(
        upgrade_drive::save_checked(&opts, "session", &first)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        upgrade_drive::load(&opts, "session").unwrap().wait,
        second.wait
    );
}

#[test]
fn failed_publications_keep_their_identity_and_fresh_writes_replace_it() {
    for before_rename in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let opts = opts(root.path());
        let mut current = model_level(0, ImagePhase::Forward);
        upgrade_drive::save_checked(&opts, "session", &current).unwrap();
        let original_id = native_record::write_id(&current).unwrap();
        let original_generation = native_record::generation(&current);
        current.wait = "next status".into();
        let early = before_rename.then(|| {
            aterm_uds::atomic_file::publication_seam::FailBeforeRename::with_kind(
                std::io::ErrorKind::StorageFull,
            )
        });
        let late = (!before_rename)
            .then(|| aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported));
        assert!(upgrade_drive::save_checked(&opts, "session", &current).is_err());
        drop((early, late));
        let pending_id = native_record::attempted_id(&current).unwrap();
        assert_ne!(pending_id, original_id);
        assert_eq!(native_record::generation(&current), original_generation);
        let disk = upgrade_drive::load(&opts, "session").unwrap();
        assert_eq!(
            native_record::generation(&disk),
            original_generation + u64::from(!before_rename)
        );
        upgrade_drive::save_checked(&opts, "session", &current).unwrap();
        assert_eq!(native_record::write_id(&current), Some(pending_id.clone()));
        assert_eq!(native_record::generation(&current), original_generation + 1);
        assert!(native_record::attempted_id(&current).is_none());
        upgrade_drive::save_checked(&opts, "session", &current).unwrap();
        assert_ne!(native_record::write_id(&current), Some(pending_id));
        assert_eq!(native_record::generation(&current), original_generation + 2);
    }
}

#[test]
fn legacy_generations_migrate_without_inventing_retry_identity() {
    for generation in [0, 7, u64::MAX] {
        let root = aterm_tempfile::tempdir().unwrap();
        let opts = opts(root.path());
        let legacy = model_level(0, ImagePhase::Forward);
        native_record::loaded(&legacy, generation, None);
        upgrade_drive::native_writer_for_test(&opts, "session", &legacy, None, false).unwrap();
        let loaded = upgrade_drive::load(&opts, "session").unwrap();
        assert_eq!(native_record::generation(&loaded), generation);
        assert!(native_record::write_id(&loaded).is_none());
        let result = upgrade_drive::save_checked(&opts, "session", &loaded);
        if generation == u64::MAX {
            assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
            assert!(native_record::attempted_id(&loaded).is_none());
            assert!(
                native_record::write_id(&upgrade_drive::load(&opts, "session").unwrap()).is_none()
            );
        } else {
            result.unwrap();
            assert_eq!(native_record::generation(&loaded), generation + 1);
            assert!(native_record::write_id(&loaded).is_some());
        }
    }
}

#[test]
fn malformed_write_identities_are_not_read_as_legacy_records() {
    let current = model_level(0, ImagePhase::Forward);
    let base: aterm_json::Value = aterm_json::from_str(&current.to_json()).unwrap();
    for id in [
        aterm_json::Value::Null,
        "".into(),
        "G".repeat(32).into(),
        "a".repeat(31).into(),
    ] {
        let mut wire = base.clone();
        let fields = wire.as_object_mut().unwrap();
        fields.insert("native_generation".into(), 1.into());
        fields.insert("native_write_id".into(), id);
        assert!(St::from_json(&aterm_json::to_string(&wire).unwrap()).is_none());
    }
    let mut wire = base;
    let fields = wire.as_object_mut().unwrap();
    fields.remove("native_generation");
    fields.insert("native_write_id".into(), "a".repeat(32).into());
    assert!(St::from_json(&aterm_json::to_string(&wire).unwrap()).is_none());
}

#[test]
fn restored_noexec_fences_a_writer_from_before_the_attempt() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let current = model_level(0, ImagePhase::Forward);
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    let stale = current.clone();
    let mut active = model_level(2, ImagePhase::Forward);
    active.native_record = current.native_record.clone();
    upgrade_drive::save_checked(&opts, "session", &active).unwrap();
    native_restart::terminal_for_test(
        &mut active,
        &Reply::Decision(Decision::ProvenNotCommittedRollbackSafe),
    )
    .unwrap();
    native_record::restore_inactive(&opts, "session", &mut active, Some(&current)).unwrap();
    assert!(active.native_noexec_prepare.is_some());
    assert!(native_record::generation(&active) > native_record::generation(&stale));
    assert!(upgrade_drive::save_checked(&opts, "session", &stale).is_err());
}

#[test]
fn absent_prior_restores_a_closed_record_without_losing_identity() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let mut current = model_level(0, ImagePhase::Forward);
    current.phase = Phase::Exiting { at_s: 7 };
    current.tab = "the original tab".into();
    current.from = "1.0.0".into();
    current.to = "1.0.1".into();
    current.source = "native".into();
    upgrade_drive::save_checked(&opts, "session", &current).unwrap();
    native_record::restore_inactive(&opts, "session", &mut current, None).unwrap();
    assert_eq!(current.phase, Phase::Done);
    assert_eq!(current.tab, "the original tab");
    assert_eq!(current.to, "1.0.1");
    let before = current.clone();
    let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert!(native_record::restore_inactive(&opts, "session", &mut current, None).is_err());
    assert_eq!(current, before);
    drop(fault);
}

#[test]
fn inactive_restore_retries_its_own_publication_without_acknowledging_failure() {
    for restore_prior in [false, true] {
        for before_rename in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let opts = opts(root.path());
            let mut current = model_level(0, ImagePhase::Forward);
            current.phase = Phase::Exiting { at_s: 7 };
            current.wait = "current metadata".into();
            upgrade_drive::save_checked(&opts, "session", &current).unwrap();
            let prior = restore_prior.then(|| {
                let mut prior = current.clone();
                prior.phase = Phase::Done;
                prior.wait = "prior metadata".into();
                prior
            });
            let original = current.clone();
            let original_id = native_record::write_id(&current);
            let early = before_rename.then(|| {
                aterm_uds::atomic_file::publication_seam::FailBeforeRename::with_kind(
                    std::io::ErrorKind::StorageFull,
                )
            });
            let late = (!before_rename).then(|| {
                aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported)
            });
            assert!(
                native_record::restore_inactive(&opts, "session", &mut current, prior.as_ref())
                    .is_err()
            );
            drop((early, late));
            assert_eq!(current, original);
            assert_eq!(native_record::write_id(&current), original_id);
            let attempted = native_record::attempted_id(&current).expect("retained retry identity");
            let disk = upgrade_drive::load(&opts, "session").unwrap();
            assert_eq!(
                native_record::write_id(&disk),
                if before_rename {
                    original_id
                } else {
                    Some(attempted.clone())
                }
            );
            native_record::restore_inactive(&opts, "session", &mut current, prior.as_ref())
                .unwrap();
            assert_eq!(native_record::write_id(&current), Some(attempted));
            assert_eq!(native_record::attempted_id(&current), None);
            assert_eq!(current.phase, Phase::Done);
            assert_eq!(current.wait, prior.as_ref().unwrap_or(&original).wait);
        }
    }
}

#[test]
fn unroundtrippable_native_records_are_not_acknowledged() {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let mut invalid = model_level(2, ImagePhase::Forward);
    invalid.native.as_mut().unwrap().capability = "not a capability".into();
    assert!(upgrade_drive::save_checked(&opts, "session", &invalid).is_err());
    assert!(!opts.state.join("upgrade/session.json").exists());
    assert_eq!(native_record::generation(&invalid), 0);
}

fn step(state: &State, action: &str, phase: ImagePhase, fault: u8) -> State {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let disk = model_level(state["disk"], phase);
    native_record::loaded(&disk, state["dg"] as u64, model_id(state["dg"]));
    upgrade_drive::native_writer_for_test(&opts, "session", &disk, None, false).unwrap();
    let baseline = model_level(state["expected"], phase);
    native_record::loaded(&baseline, state["eg"] as u64, model_id(state["eg"]));
    let mut caller = model_level(state["target"], phase);
    caller.native_record = baseline.native_record.clone();
    if state["attempted"] < 3 {
        native_record::seed_attempt(
            &caller,
            &model_level(state["attempted"], phase),
            state["ag"] as u64,
            model_id(state["ag"]),
        );
    }
    let eligible = (state["expected"] == state["disk"] && state["eg"] == state["dg"])
        || (state["attempted"] == state["disk"]
            && state["ag"] == state["dg"]
            && state["target"] == state["disk"]);
    let mut next = state.clone();
    match action {
        "Advance" => {
            let mut advancing = model_level(2, phase);
            advancing.native_record = disk.native_record.clone();
            upgrade_drive::save_checked(&opts, "session", &advancing).unwrap();
            if fault == 6 {
                native_record::loaded(&advancing, state["dg"] as u64, model_id(state["dg"]));
                upgrade_drive::native_writer_for_test(&opts, "session", &advancing, None, false)
                    .unwrap();
            }
            if fault == 3 {
                native_record::acknowledge(&caller, &advancing);
            }
            next.insert("advanced", 1);
            next.insert("expected", expected_level(&caller, phase));
            next.insert("eg", native_record::generation(&caller) as i64);
            if native_record::generation(&advancing) as i64 <= state["dg"] {
                next.insert("reused", 1);
            }
        }
        "ABA" => {
            let mut current = upgrade_drive::load(&opts, "session").unwrap();
            native_restart::terminal_for_test(
                &mut current,
                &Reply::Decision(Decision::ProvenNotCommittedRollbackSafe),
            )
            .unwrap();
            native_record::restore_inactive(
                &opts,
                "session",
                &mut current,
                Some(&model_level(0, phase)),
            )
            .unwrap();
            if fault == 6 {
                native_record::loaded(&current, state["dg"] as u64, model_id(state["dg"]));
                upgrade_drive::native_writer_for_test(&opts, "session", &current, None, false)
                    .unwrap();
            }
            next.insert("aba", 1);
            if native_record::generation(&current) as i64 <= state["dg"] {
                next.insert("reused", 1);
            }
        }
        "Write" => {
            let refusal = (state["failed"] == 1).then(|| {
                aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported)
            });
            if fault == 1 && state["target"] == state["disk"] {
                native_record::loaded(&caller, state["dg"] as u64, model_id(state["dg"]));
            }
            let result = if fault == 4 {
                Err(std::io::Error::other(
                    "negative control: reject every write",
                ))
            } else {
                upgrade_drive::save_checked(&opts, "session", &caller)
            };
            drop(refusal);
            let written = upgrade_drive::load(&opts, "session").unwrap();
            if fault == 5 && state["failed"] == 1 && eligible {
                native_record::acknowledge(&caller, &written);
            }
            if result.is_ok()
                && ((!eligible)
                    || (state["disk"] == 2 && state["target"] == 1 && state["reloaded"] == 0))
            {
                next.insert("regressed", 1);
            }
            if state["failed"] == 0 && eligible && result.is_err() {
                next.insert("blocked", 1);
            }
            if state["failed"] == 1
                && (expected_level(&caller, phase) != state["expected"]
                    || native_record::generation(&caller) as i64 != state["eg"])
            {
                next.insert("premature", 1);
            }
            next.insert("attempts", state["attempts"] + 1);
            next.insert("expected", expected_level(&caller, phase));
            next.insert("eg", native_record::generation(&caller) as i64);
            let attempted = (0..=2).find_map(|value| {
                native_record::attempted_matches(&caller, &model_level(value, phase))
                    .map(|generation| (value, generation))
            });
            next.insert("attempted", attempted.map_or(3, |(value, _)| value));
            next.insert(
                "ag",
                attempted.map_or(0, |(_, generation)| generation as i64),
            );
        }
        "Reload" => {
            caller = upgrade_drive::load(&opts, "session").unwrap();
            next.insert("reloaded", 1);
            next.insert("expected", expected_level(&caller, phase));
            next.insert("eg", native_record::generation(&caller) as i64);
            next.insert("target", read_level(Some(&caller)));
            next.insert("attempted", 3);
            next.insert("ag", 0);
        }
        "Remove" => {
            if fault == 2 {
                std::fs::remove_file(opts.state.join("upgrade/session.json")).unwrap();
            } else {
                upgrade_drive::remove(&opts, "session");
            }
            next.insert("removed", 1);
            if upgrade_drive::load(&opts, "session").is_none() {
                next.insert("forgotten", 1);
            }
        }
        "FailBarrier" => {
            next.insert("failed", 1);
        }
        "RepairBarrier" => {
            next.insert("failed", 0);
        }
        _ => unreachable!(),
    }
    let written = upgrade_drive::load(&opts, "session");
    next.insert("disk", read_level(written.as_ref()));
    next.insert(
        "dg",
        written
            .as_ref()
            .map_or(state["dg"], |st| native_record::generation(st) as i64),
    );
    next
}

pub(super) fn transition(state: &State, action: &str) -> State {
    let forward = step(state, action, ImagePhase::Forward, 0);
    assert_eq!(forward, step(state, action, ImagePhase::Rollback, 0));
    forward
}

#[test]
fn native_record_refines_stale_writer_model_with_negative_controls() {
    let model = aterm_models_harness::native_record_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                assert!(
                    model
                        .successors(action.name, &state)
                        .contains(&transition(&state, action.name)),
                    "{}: {state:?}",
                    action.name
                );
            }
        }
    }
    let phase = ImagePhase::Forward;
    let advanced = step(&model.init_state(), "Advance", phase, 0);
    let removed = step(&advanced, "Remove", phase, 2);
    assert!(!model.check_invariant("CoordinationIsSticky", &removed));
    let refreshed = step(&model.init_state(), "Advance", phase, 3);
    assert!(!model.check_invariant("NoStaleRegression", &step(&refreshed, "Write", phase, 0)));
    let rejected = step(&model.init_state(), "Write", phase, 4);
    assert!(!model.check_invariant("ValidWritesProgress", &rejected));
    let failed = step(&model.init_state(), "FailBarrier", phase, 0);
    assert!(!model.check_invariant("FailedWritesDoNotAck", &step(&failed, "Write", phase, 5)));
    let reused = step(&model.init_state(), "Advance", phase, 6);
    assert!(!model.check_invariant("GenerationsNeverReuse", &reused));
    let mut old = step(&model.init_state(), "Reload", phase, 0);
    old = step(&old, "Advance", phase, 0);
    old = step(&old, "ABA", phase, 0);
    assert!(!model.check_invariant("NoStaleRegression", &step(&old, "Write", phase, 1)));
}

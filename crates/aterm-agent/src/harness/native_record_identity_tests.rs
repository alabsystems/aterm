// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ImagePhase, St, State, model_level, native_record, opts, upgrade_drive};

fn id(value: usize) -> String {
    format!("{:032x}", value + 100)
}

fn generation(value: i64) -> u64 {
    match value {
        0 => 1,
        1 | 2 => 2,
        3 => 3,
        _ => unreachable!(),
    }
}

fn record(metadata: i64, phase: ImagePhase, generation: u64, id: &str) -> St {
    let mut st = model_level(0, phase);
    st.wait = format!("status-{metadata}");
    native_record::loaded(&st, generation, Some(id.to_owned()));
    st
}

#[derive(Clone, Copy)]
enum WritePath {
    Save,
    Restore,
}

impl WritePath {
    fn write(self, opts: &upgrade_drive::Opts, st: &mut St) -> std::io::Result<()> {
        match self {
            Self::Save => upgrade_drive::save_checked(opts, "session", st),
            Self::Restore => {
                let prior = st.clone();
                native_record::restore_inactive(opts, "session", st, Some(&prior))
            }
        }
    }
}

fn step(state: &State, action: &str, phase: ImagePhase, path: WritePath, fault: u8) -> State {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let mut ids: [String; 4] = std::array::from_fn(id);
    let disk = record(
        state["meta"],
        phase,
        generation(state["disk"]),
        &ids[state["disk"] as usize],
    );
    upgrade_drive::native_writer_for_test(&opts, "session", &disk, None, false).unwrap();
    let baseline = record(
        state["ack"],
        phase,
        generation(state["ack"]),
        &ids[state["ack"] as usize],
    );
    let mut caller = record(1, phase, 1, &ids[0]);
    caller.native_record = baseline.native_record.clone();
    if state["begun"] == 1 && state["ack"] == 0 {
        native_record::seed_attempt(&caller, &caller, 2, Some(ids[1].clone()));
    }
    let mut next = state.clone();
    match action {
        "AttemptBefore" | "AttemptAfter" => {
            let early = (action == "AttemptBefore").then(|| {
                aterm_uds::atomic_file::publication_seam::FailBeforeRename::with_kind(
                    std::io::ErrorKind::StorageFull,
                )
            });
            let late = (action == "AttemptAfter").then(|| {
                aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported)
            });
            assert!(path.write(&opts, &mut caller).is_err());
            drop((early, late));
            ids[1] = native_record::attempted_id(&caller).unwrap();
            assert_ne!(ids[1], ids[0]);
            next.insert("begun", 1);
        }
        "Rival" => {
            let mut rival = record(2, phase, 1, &ids[0]);
            if path.write(&opts, &mut rival).is_ok() {
                ids[2] = native_record::write_id(&rival).unwrap();
                assert_ne!(ids[2], ids[0]);
                assert_ne!(ids[2], ids[1]);
            }
            next.insert("rival", 1);
        }
        "Retry" | "LateRetry" => {
            if fault == 1 && action == "Retry" && state["disk"] == 2 {
                // Negative control: comparing only generation and native state
                // treats another writer's identity as the pending identity.
                native_record::seed_attempt(&caller, &caller, 2, Some(ids[2].clone()));
            }
            if fault == 5 {
                // Negative control: discard this writer's pending ownership,
                // as the restoration wrapper did before moving its baseline.
                caller.native_record = caller.native_record.clone();
            }
            let late = (action == "LateRetry").then(|| {
                aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported)
            });
            let result = if fault == 3 {
                Err(std::io::Error::other(
                    "negative control: refuse valid retries",
                ))
            } else {
                path.write(&opts, &mut caller)
            };
            drop(late);
            let written = upgrade_drive::load(&opts, "session").unwrap();
            if fault == 4 && action == "LateRetry" && state["disk"] <= 1 {
                native_record::acknowledge(&caller, &written);
            }
            if state["disk"] == 2 && written.wait != disk.wait {
                next.insert("clobbered", 1);
            }
            if action == "Retry" && state["disk"] <= 1 && result.is_err() {
                next.insert("blocked", 1);
            }
            if action == "LateRetry" && native_record::write_id(&caller) != Some(ids[0].clone()) {
                next.insert("premature", 1);
            }
            next.insert("retries", state["retries"] + 1);
        }
        "Fresh" => {
            let previous = native_record::write_id(&caller).unwrap();
            caller.wait = "status-3".into();
            path.write(&opts, &mut caller).unwrap();
            if fault == 2 {
                native_record::loaded(&caller, 3, Some(previous.clone()));
                upgrade_drive::native_writer_for_test(&opts, "session", &caller, None, false)
                    .unwrap();
            }
            let fresh = native_record::write_id(&caller).unwrap();
            if fresh == previous {
                next.insert("reused", 1);
            } else {
                ids[3] = fresh;
            }
            next.insert("fresh", 1);
        }
        _ => unreachable!(),
    }
    let written = upgrade_drive::load(&opts, "session").unwrap();
    let written_id = native_record::write_id(&written).unwrap();
    let index = ids.iter().position(|id| id == &written_id).unwrap();
    next.insert("disk", index as i64);
    next.insert(
        "meta",
        written
            .wait
            .strip_prefix("status-")
            .unwrap()
            .parse()
            .unwrap(),
    );
    next.insert(
        "ack",
        i64::from(native_record::write_id(&caller) != Some(ids[0].clone())),
    );
    next
}

pub(super) fn transition(state: &State, action: &str) -> State {
    let forward = step(state, action, ImagePhase::Forward, WritePath::Save, 0);
    for (phase, path) in [
        (ImagePhase::Rollback, WritePath::Save),
        (ImagePhase::Forward, WritePath::Restore),
        (ImagePhase::Rollback, WritePath::Restore),
    ] {
        assert_eq!(forward, step(state, action, phase, path, 0));
    }
    forward
}

#[test]
fn write_identity_refines_real_before_and_after_rename_failures() {
    let model = aterm_models_harness::native_write_identity_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
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
    for path in [WritePath::Save, WritePath::Restore] {
        let before = step(&model.init_state(), "AttemptBefore", phase, path, 0);
        let rival = step(&before, "Rival", phase, path, 0);
        assert!(!model.check_invariant(
            "PreserveRivalMetadata",
            &step(&rival, "Retry", phase, path, 1)
        ));
        assert!(!model.check_invariant(
            "ValidRetryProgress",
            &step(&before, "Retry", phase, path, 3)
        ));
        assert!(!model.check_invariant(
            "NoFailedAcknowledgement",
            &step(&before, "LateRetry", phase, path, 4)
        ));
        let completed = step(&before, "Retry", phase, path, 0);
        assert!(!model.check_invariant(
            "FreshWriteIdentity",
            &step(&completed, "Fresh", phase, path, 2)
        ));
    }
    let after = step(
        &model.init_state(),
        "AttemptAfter",
        phase,
        WritePath::Restore,
        0,
    );
    assert!(!model.check_invariant(
        "ValidRetryProgress",
        &step(&after, "Retry", phase, WritePath::Restore, 5)
    ));
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn limits() -> Limits {
    Limits {
        records: 8,
        bytes: 8192,
        record_bytes: 1024,
    }
}

fn open(dir: &Path) -> io::Result<Log> {
    Log::open(dir, "test", b"pinned", false, limits())
}

fn identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    match action {
        "Missing" => {
            next.insert("present", 0);
        }
        "Corrupt" => {
            next.insert("valid", 0);
        }
        "Unenroll" => {
            next.insert("fresh", 1);
        }
        "FailBarrier" => {
            next.insert("barrier", 0);
        }
        "FailEarly" => {
            next.insert("barrier", 0);
            next.insert("early", 1);
        }
        "Open" => {
            let dir = aterm_tempfile::tempdir().unwrap();
            let mut initial = open(dir.path()).unwrap();
            initial.finish_open().unwrap();
            drop(initial);
            let checkpoint = dir.path().join("test.checkpoint");
            if state["present"] == 0 {
                std::fs::remove_file(&checkpoint).unwrap();
            } else if state["valid"] == 0 {
                let mut bytes = std::fs::read(&checkpoint).unwrap();
                bytes[8] ^= 1;
                write_private_durable(dir.path(), "test.checkpoint", &bytes).unwrap();
            }
            if state["fresh"] == 1 {
                std::fs::remove_file(dir.path().join("test.anchor")).unwrap();
            }
            let before = identity(&checkpoint);
            // An early fault stops admission before any checkpoint publication. A late
            // one lets admission flush the parent, then fails an open's later barrier.
            let fault = (state["barrier"] == 0).then(|| {
                aterm_durable::seam::FailingDirs::after(
                    u32::from(state["early"] == 0),
                    aterm_durable::seam::EIO,
                )
            });
            let opened = Log::open(dir.path(), "test", b"pinned", state["fresh"] == 0, limits());
            next.insert("admitted", i64::from(opened.is_ok()));
            drop(opened);
            drop(fault);
            next.insert("rewritten", i64::from(identity(&checkpoint) != before));
            next.insert("phase", 1);
        }
        _ => panic!("unknown journal open action {action}"),
    }
    next
}

#[test]
fn journal_open_matches_model_and_rejects_an_unflushed_success() {
    let model = aterm_models_swarm::journal_open_model();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{action:?}: {state:?} -> {actual:?}"
                );
            }
        }
    }
    let mut failed = model.init_state();
    failed.insert("barrier", 0);
    let mut unsafe_success = transition(&failed, "Open");
    assert_eq!(unsafe_success["admitted"], 0);
    unsafe_success.insert("admitted", 1);
    assert!(!model.check_invariant("DurableBeforeAdmission", &unsafe_success));
    assert!(!model.successors("Open", &failed).contains(&unsafe_success));
    failed.insert("early", 1);
    let mut unsafe_publication = transition(&failed, "Open");
    unsafe_publication.insert("rewritten", 1);
    assert!(!model.check_invariant("NoPublicationAfterEarlyFailure", &unsafe_publication));
    assert!(
        !model
            .successors("Open", &failed)
            .contains(&unsafe_publication)
    );
    let mut unavailable = transition(&model.init_state(), "Open");
    assert_eq!(unavailable["admitted"], 1);
    unavailable.insert("admitted", 0);
    assert!(!model.check_invariant("Available", &unavailable));
}

#[test]
fn early_fresh_directory_failures_leave_no_checkpoint_and_can_recover() {
    for successful_barriers in 0..3 {
        let dir = aterm_tempfile::tempdir().unwrap();
        let fault =
            aterm_durable::seam::FailingDirs::after(successful_barriers, aterm_durable::seam::EIO);
        assert!(open(dir.path()).is_err());
        assert!(identity(&dir.path().join("test.checkpoint")).is_none());
        drop(fault);
        let mut recovered = open(dir.path()).unwrap();
        recovered.finish_open().unwrap();
        recovered.append("app/1", vec![9]).unwrap();
    }
}

#[test]
fn a_visible_uncertain_checkpoint_is_flushed_without_replacement_on_reopen() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut initial = open(dir.path()).unwrap();
    initial.finish_open().unwrap();
    let fault = aterm_durable::seam::FailingDirs::with(aterm_durable::seam::EIO);
    assert!(initial.append("app/1", vec![7]).is_err());
    assert!(initial.is_poisoned());
    drop(initial);
    drop(fault);
    let checkpoint = dir.path().join("test.checkpoint");
    let before = identity(&checkpoint);
    let recovered = open(dir.path()).unwrap();
    assert_eq!(before, identity(&checkpoint));
    assert_eq!(recovered.read_range(Offset::ZERO, 8).unwrap().len(), 1);
    // The fault-injected model case above proves this open refuses if its final
    // barrier fails. Here it succeeds after that same uncertain rename.
}

#[test]
fn an_unenrolled_nonempty_journal_cannot_invent_a_missing_checkpoint() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut initial = open(dir.path()).unwrap();
    initial.finish_open().unwrap();
    initial.append("app/1", vec![7]).unwrap();
    drop(initial);
    std::fs::remove_file(dir.path().join("test.anchor")).unwrap();
    std::fs::remove_file(dir.path().join("test.checkpoint")).unwrap();
    assert!(open(dir.path()).is_err());
    assert!(identity(&dir.path().join("test.checkpoint")).is_none());
}

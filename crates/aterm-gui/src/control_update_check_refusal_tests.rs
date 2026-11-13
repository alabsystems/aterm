// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::control_media::MainHopError;

#[test]
fn a_refused_settings_read_is_answered_err_in_its_own_words() {
    let dialog = MainHopError::Reason("aterm is showing a dialog; answer it, then retry");
    assert_eq!(
        super::update_check_refused(&dialog),
        "ERR update check: aterm is showing a dialog; answer it, then retry\n"
    );
}

#[test]
fn a_per_process_candidate_publishes_only_after_the_commit() {
    assert!(super::await_commit_to_publish(None), "an ordinary launch");
    let gate = crate::spawn::DeferredReaderGate::closed();
    let waiter = {
        let gate = gate.clone();
        std::thread::spawn(move || super::await_commit_to_publish(Some(&gate)))
    };
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the candidate's publication parks on the closed gate",
        || (gate.parked() == 1 || waiter.is_finished()).then_some(()),
    );
    assert!(
        !waiter.is_finished(),
        "nothing is published before the Commit"
    );
    gate.release();
    assert!(
        waiter.join().unwrap(),
        "the Commit releases the publication"
    );
}

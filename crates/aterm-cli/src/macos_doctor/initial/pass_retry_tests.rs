// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_pause_grows_to_an_hour_and_never_stops() {
    let mut retry = PassRetry::default();
    let mut minutes = Vec::new();
    for _ in 0..9 {
        retry = retry.after_another(retry.due(retry.at));
        minutes.push((retry.due(retry.at) - retry.at) / 60);
    }
    assert_eq!(minutes, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    assert_eq!(PassRetry::default().due(0), 0, "none tried: at once");
}

/// A pass a wake brought before the pause ran out keeps the pause's length, from its own
/// time; only a pass the pause let come lengthens it.
#[test]
fn a_pass_a_wake_brought_early_keeps_the_pause_and_never_lengthens_it() {
    let paced = PassRetry::default()
        .after_another(1_000)
        .after_another(1_060);
    assert_eq!((paced.tries, paced.due(1_060)), (2, 1_060 + 120));
    let mut early = paced;
    for at in [1_070, 1_090, 1_150] {
        early = early.after_another(at);
        assert_eq!(
            (early.tries, early.due(at)),
            (2, at + 120),
            "a wake at {at}"
        );
    }
    let ran_out = early.after_another(1_150 + 120);
    assert_eq!(ran_out.tries, 3, "the pause ran out: the next is longer");
    assert_eq!(ran_out.due(ran_out.at), 1_150 + 120 + 240);
}

/// A last pass recorded ahead of the clock (the clock set back since) is due at once, never
/// at a time the clock takes longer than the schedule's longest pause to reach.
#[test]
fn a_clock_set_back_makes_the_next_pass_due_at_once() {
    let ahead = PassRetry {
        tries: 7,
        at: 10_000 + 86_400,
    };
    assert_eq!(ahead.due(10_000), 10_000);
    let next = ahead.after_another(10_000);
    assert_eq!(
        (next.tries, next.at),
        (8, 10_000),
        "counted from the clock now"
    );
    assert_eq!(next.due(10_000), 10_000 + 3_600);
}

/// The count is the record's: a fresh read of it, as a relaunch or a reboot makes, goes on
/// from it; another run's record, or none, starts from nothing.
#[test]
fn a_relaunch_reads_the_same_count_and_deadline() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    assert_eq!(load(root, PassWorker::Run, 3), PassRetry::default());
    let retry = PassRetry::default()
        .after_another(1_000)
        .after_another(1_060);
    save(root, PassWorker::Run, 3, retry).unwrap();
    assert_eq!(load(root, PassWorker::Run, 3), retry);
    assert_eq!(load(root, PassWorker::Run, 3).due(1_060), 1_060 + 120);
    assert_eq!(
        load(root, PassWorker::Run, 4),
        PassRetry::default(),
        "another run's"
    );
    save(root, PassWorker::Run, 3, PassRetry::default()).unwrap();
    assert!(
        !root.join(PassWorker::Run.record()).exists(),
        "no try leaves no record"
    );
    std::fs::write(root.join(PassWorker::Run.record()), b"not json").unwrap();
    assert_eq!(
        load(root, PassWorker::Run, 3),
        PassRetry::default(),
        "unreadable"
    );
    clear(root, PassWorker::Run).unwrap();
    clear(root, PassWorker::Run).unwrap();
}

/// The retired Caps Lock worker's record, which nothing reads since the login job took its
/// place, goes as the run's worker saves its pace, whatever it saves. Before, it stayed on
/// disk for good.
#[test]
fn the_retired_input_workers_record_goes_at_the_run_workers_save() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    for retry in [
        PassRetry::default(),
        PassRetry::default().after_another(1_000),
    ] {
        std::fs::write(root.join(RETIRED), b"{}").unwrap();
        save(root, PassWorker::Run, 3, retry).unwrap();
        assert!(!root.join(RETIRED).exists(), "saved {retry:?}");
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::VecDeque;

const WINDOW: Duration = Duration::from_secs(60);

fn original() -> Identity {
    Identity {
        pid: 123,
        birth: Birth(456),
        idversion: crate::process::KEEPS_PID_VERSIONS.then_some(7),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    Identity,
    Presence,
    Progress,
    Pause,
}

struct Script {
    at: Instant,
    identities: VecDeque<Option<Identity>>,
    presences: VecDeque<ProcessPresence>,
    work: VecDeque<Option<u64>>,
    advances: VecDeque<Duration>,
    reads: Vec<Read>,
}

impl Script {
    fn new(
        identities: impl IntoIterator<Item = Option<Identity>>,
        presences: impl IntoIterator<Item = ProcessPresence>,
        work: impl IntoIterator<Item = Option<u64>>,
        advances: impl IntoIterator<Item = Duration>,
    ) -> Self {
        Self {
            at: Instant::now(),
            identities: identities.into_iter().collect(),
            presences: presences.into_iter().collect(),
            work: work.into_iter().collect(),
            advances: advances.into_iter().collect(),
            reads: Vec::new(),
        }
    }

    fn consumed(&self) {
        assert!(
            self.identities.is_empty(),
            "unused native identity observations"
        );
        assert!(
            self.presences.is_empty(),
            "unused native presence observations"
        );
        assert!(self.work.is_empty(), "unused native progress observations");
        assert!(
            self.advances.is_empty(),
            "retirement returned before the scripted wait"
        );
    }
}

impl View for Script {
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        assert_eq!(pid, original().pid);
        self.reads.push(Read::Identity);
        self.identities
            .pop_front()
            .expect("unexpected native identity read")
    }

    fn presence(&mut self, pid: i32) -> ProcessPresence {
        assert_eq!(pid, original().pid);
        self.reads.push(Read::Presence);
        self.presences
            .pop_front()
            .expect("unexpected native presence read")
    }

    fn progress(&mut self, pid: i32) -> Option<u64> {
        assert_eq!(pid, original().pid);
        self.reads.push(Read::Progress);
        self.work
            .pop_front()
            .expect("unexpected native progress read")
    }

    fn now(&mut self) -> Instant {
        self.at
    }

    fn pause(&mut self, duration: Duration) {
        assert!(!duration.is_zero());
        assert!(duration <= Duration::from_millis(25));
        self.reads.push(Read::Pause);
        self.at += self
            .advances
            .pop_front()
            .expect("unexpected retirement wait");
    }
}

#[test]
fn watcher_frames_require_one_complete_native_identity_in_this_platforms_shape() {
    let identity = original();
    let frame = encode(identity);
    assert_eq!(decode(&frame).unwrap(), identity);
    for end in 0..FRAME_LEN {
        assert!(decode(&frame[..end]).is_err(), "accepted only {end} bytes");
    }
    let mut extra = frame.to_vec();
    extra.push(0);
    assert!(decode(&extra).is_err());
    let mut wrong_magic = frame;
    wrong_magic[0] ^= 1;
    assert!(decode(&wrong_magic).is_err());
    for pid in [-1, 0, 1] {
        assert!(decode(&encode(Identity { pid, ..identity })).is_err());
    }
    assert!(
        decode(&encode(Identity {
            birth: Birth(0),
            ..identity
        }))
        .is_err()
    );
    let opposite_shape = Identity {
        idversion: if crate::process::KEEPS_PID_VERSIONS {
            None
        } else {
            Some(7)
        },
        ..identity
    };
    assert!(decode(&encode(opposite_shape)).is_err());
    let mut invalid_shape = frame;
    invalid_shape[20..24].copy_from_slice(&2_u32.to_ne_bytes());
    assert!(decode(&invalid_shape).is_err());
}

#[test]
fn missing_and_failed_receipt_observations_never_report_native_retirement() {
    let pending = Receipt::default();
    let unrelated = Receipt::default();
    let observe = pending.clone();
    assert!(
        pending
            .finish(WINDOW, || panic!("unobserved receipt reached native wait"))
            .is_err()
    );
    observe.record(Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "truncated watcher frame",
    )));
    // A later observation cannot erase the original failure or bind another child to it.
    observe.record(Ok(original()));
    let failed = pending
        .finish(WINDOW, || panic!("failed receipt reached native wait"))
        .unwrap_err();
    assert!(failed.to_string().contains("truncated watcher frame"));
    assert!(
        unrelated
            .finish(WINDOW, || panic!("another child inherited this receipt"))
            .is_err()
    );
}

#[test]
fn only_positive_absence_or_different_birth_closes_the_original_watcher() {
    let token = original();
    let mut absent = Script::new([None], [ProcessPresence::Absent], [], []);
    finish_using(token, Duration::ZERO, &mut absent, || Ok(())).unwrap();
    absent.consumed();
    assert_eq!(absent.reads, [Read::Identity, Read::Presence]);

    let reused = Identity {
        birth: Birth(token.birth.0 + 1),
        ..token
    };
    let mut new_process = Script::new([Some(reused)], [], [], []);
    finish_using(token, Duration::ZERO, &mut new_process, || Ok(())).unwrap();
    new_process.consumed();
    assert_eq!(new_process.reads, [Read::Identity]);

    for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
        let mut unread = Script::new([None], [presence], [], []);
        assert!(finish_using(token, WINDOW, &mut unread, || Ok(())).is_err());
        unread.consumed();
        assert_eq!(unread.reads, [Read::Identity, Read::Presence]);
    }
    let changed = Identity {
        idversion: Some(token.idversion.unwrap_or(0) + 1),
        ..token
    };
    let mut execed = Script::new([Some(changed)], [], [], []);
    assert!(finish_using(token, WINDOW, &mut execed, || Ok(())).is_err());
    execed.consumed();
    assert_eq!(execed.reads, [Read::Identity]);
}

#[test]
fn an_exited_unreaped_watcher_waits_for_positive_native_absence() {
    let mut view = Script::new(
        [None, None, None],
        [
            ProcessPresence::Exited,
            ProcessPresence::Exited,
            ProcessPresence::Absent,
        ],
        [None],
        [WINDOW * 2],
    );
    finish_using(original(), WINDOW, &mut view, || Ok(())).unwrap();
    view.consumed();
    assert_eq!(view.reads.iter().filter(|r| **r == Read::Pause).count(), 1);
    assert_eq!(view.reads.last(), Some(&Read::Presence));
}

#[test]
fn native_identity_is_rechecked_after_the_progress_observation() {
    let token = original();
    let mut ended = Script::new(
        [Some(token), None],
        [ProcessPresence::Absent],
        [Some(9)],
        [],
    );
    finish_using(token, Duration::ZERO, &mut ended, || Ok(())).unwrap();
    ended.consumed();
    assert_eq!(
        ended.reads,
        [
            Read::Identity,
            Read::Progress,
            Read::Identity,
            Read::Presence
        ]
    );
    for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
        let mut unread = Script::new([Some(token), None], [presence], [Some(9)], []);
        assert!(finish_using(token, WINDOW, &mut unread, || Ok(())).is_err());
        unread.consumed();
        assert_eq!(
            unread.reads,
            [
                Read::Identity,
                Read::Progress,
                Read::Identity,
                Read::Presence
            ]
        );
    }
    let changed = Identity {
        idversion: Some(token.idversion.unwrap_or(0) + 1),
        ..token
    };
    let mut changed_after_work = Script::new([Some(token), Some(changed)], [], [Some(9)], []);
    assert!(finish_using(token, WINDOW, &mut changed_after_work, || Ok(())).is_err());
    changed_after_work.consumed();
    assert_eq!(
        changed_after_work.reads,
        [Read::Identity, Read::Progress, Read::Identity]
    );
}

#[test]
fn comparable_native_work_can_advance_for_longer_than_one_inactivity_window() {
    let token = original();
    let mut identities = vec![Some(token); 6];
    identities.push(None);
    let mut view = Script::new(
        identities,
        [ProcessPresence::Absent],
        [Some(10), Some(11), Some(12)],
        [WINDOW + Duration::from_secs(1); 3],
    );
    let began = view.at;
    finish_using(token, WINDOW, &mut view, || Ok(())).unwrap();
    view.consumed();
    assert!(view.at.duration_since(began) > WINDOW * 3);
    assert_eq!(
        view.reads.iter().filter(|r| **r == Read::Progress).count(),
        3
    );
}

#[test]
fn a_live_idle_watcher_is_not_retirement_and_times_out_on_native_inactivity() {
    let token = original();
    let mut view = Script::new(
        [Some(token); 4],
        [],
        [Some(10), Some(10)],
        [WINDOW + Duration::from_secs(1)],
    );
    let error = finish_using(token, WINDOW, &mut view, || Ok(())).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    view.consumed();
}

#[test]
fn incomparable_native_work_disarms_then_starts_a_fresh_comparable_window() {
    let token = original();
    let mut view = Script::new(
        [Some(token); 8],
        [],
        [Some(10), None, Some(10), Some(10)],
        [WINDOW + Duration::from_secs(1); 3],
    );
    let began = view.at;
    let error = finish_using(token, WINDOW, &mut view, || Ok(())).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    view.consumed();
    assert!(view.at.duration_since(began) > WINDOW * 3);
    assert_eq!(
        view.reads.iter().filter(|r| **r == Read::Progress).count(),
        4
    );
}

#[test]
fn cancellation_precedes_native_reads_and_can_stop_an_unarmed_progress_window() {
    let mut untouched = Script::new([], [], [], []);
    let error = finish_using(original(), WINDOW, &mut untouched, || {
        Err(io::Error::new(io::ErrorKind::Interrupted, "canceled"))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    untouched.consumed();
    assert!(untouched.reads.is_empty());

    let token = original();
    let mut pending = Script::new([Some(token); 2], [], [None], [WINDOW * 2]);
    let mut checks = 0;
    let error = finish_using(token, WINDOW, &mut pending, || {
        checks += 1;
        if checks == 1 {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::Interrupted, "canceled"))
        }
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(checks, 2);
    pending.consumed();
}

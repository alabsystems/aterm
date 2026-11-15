// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::InitialSetupWithheldStopTest;
use aterm_secrets::converge::FIRST_RETRY_MS;
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Owned(Coordinator);

impl std::ops::Deref for Owned {
    type Target = Coordinator;
    fn deref(&self) -> &Coordinator {
        &self.0
    }
}

impl std::ops::DerefMut for Owned {
    fn deref_mut(&mut self) -> &mut Coordinator {
        &mut self.0
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if let Some((_, worker)) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Release(Option<mpsc::Sender<()>>);

impl Drop for Release {
    fn drop(&mut self) {
        if let Some(send) = self.0.take() {
            let _ = send.send(());
        }
    }
}

fn finished(coordinator: &Coordinator) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while coordinator
        .worker
        .as_ref()
        .is_some_and(|(_, worker)| !worker.is_finished())
    {
        assert!(Instant::now() < deadline, "finite follow-up worker hung");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn start(fixture: InitialSetupWithheldStopTest) -> std::io::Result<JoinHandle<Attempted>> {
    start_at(fixture, InitialSetupWithheldStopTest::NOW)
}

fn start_at(
    mut fixture: InitialSetupWithheldStopTest,
    now: u64,
) -> std::io::Result<JoinHandle<Attempted>> {
    std::thread::Builder::new()
        .name("aterm-test-follow-up".into())
        .spawn(move || {
            super::super::publish_withheld_attempt(
                fixture.attempt_at(now, || Some(()), |_| false),
                |_, _| {},
                |why, _| panic!("unexpected publication refusal: {why}"),
            )
        })
}

/// A follow-up whose only unfinished change waits out its own pause (a try of it that did
/// not take, its row put off) keeps the grant's request, starts nothing until that pause
/// ends, then makes the change and lets the request go. NEGATIVE CONTROL: read the put-off
/// result as ended (it needs no attention) and the request is dropped after the first
/// attempt, the change never made though the page says aterm tries again by itself.
#[test]
fn a_follow_up_put_off_for_its_pause_keeps_the_request_until_the_change_is_made() {
    let root = aterm_tempfile::tempdir().unwrap();
    let fixture = InitialSetupWithheldStopTest::unfinished(root.path()).unwrap();
    let tried = InitialSetupWithheldStopTest::NOW - 60;
    fixture.try_did_not_take(tried).unwrap();
    let writes = fixture.notification_center_writes();
    let requests = Requests::new();
    let mut coordinator = Owned(Coordinator::default());
    requests.request();
    coordinator
        .round_with(requests.current(), 0, || start(fixture.clone()))
        .unwrap();
    finished(&coordinator);
    let mut again = false;
    let reaped = 1;
    coordinator
        .round_with(requests.current(), reaped, || {
            again = true;
            start(fixture.clone())
        })
        .unwrap();
    assert!(!again, "nothing starts before the put-off change comes due");
    assert_eq!(
        coordinator.wanted,
        Some(requests.current()),
        "the put-off change keeps the request"
    );
    assert_eq!(fixture.notification_center_writes(), writes, "put off");
    // Its first pause is ten minutes after the try; the attempt ran a minute after it, and
    // the keeper counts what is left from the round that reaped it.
    let due = reaped + 9 * 60 * 1000;
    coordinator
        .round_with(requests.current(), due - 1, || {
            panic!("still within the change's own pause")
        })
        .unwrap();
    coordinator
        .round_with(requests.current(), due, || {
            start_at(fixture.clone(), tried + 10 * 60)
        })
        .unwrap();
    finished(&coordinator);
    coordinator
        .round_with(requests.current(), due + 1, || {
            panic!("the change was made: nothing is owed")
        })
        .unwrap();
    assert!(coordinator.wanted.is_none(), "made: the request goes");
    assert_eq!(fixture.notification_center_writes(), writes + 1);
}

#[test]
fn the_grants_real_follow_up_survives_failed_start_and_late_unlock_and_keeps_a_new_request() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut fixture = InitialSetupWithheldStopTest::unfinished(root.path()).unwrap();
    let publish_restore = fixture.prepare_restore().unwrap();
    let writes = fixture.notification_center_writes();
    assert!(
        !fixture.owed().unwrap(),
        "the Restore has not published its debt"
    );
    let before = std::fs::read(root.path().join("withheld-preferences.json")).ok();
    let requests = Requests::new();
    let mut coordinator = Owned(Coordinator::default());
    coordinator
        .round_with(requests.current(), 0, || panic!("no request"))
        .unwrap();
    requests.request();
    assert!(
        coordinator
            .round_with(requests.current(), 0, || {
                Err(std::io::Error::other(
                    "injected finite-worker spawn refusal",
                ))
            })
            .is_err()
    );
    assert!(coordinator.worker.is_none());
    assert_eq!(coordinator.wanted, Some(requests.current()));
    coordinator
        .round_with(requests.current(), FIRST_RETRY_MS - 1, || {
            panic!("a refused start keeps its existing backoff")
        })
        .unwrap();
    coordinator
        .round_with(requests.current(), FIRST_RETRY_MS, || {
            start(fixture.clone())
        })
        .unwrap();
    finished(&coordinator);
    coordinator
        .round_with(requests.current(), FIRST_RETRY_MS, || {
            panic!("held leases wait for the next keeper round")
        })
        .unwrap();
    assert!(coordinator.worker.is_none());
    assert_eq!(coordinator.wanted, Some(requests.current()));
    assert_eq!(fixture.notification_center_writes(), writes);
    assert_eq!(
        std::fs::read(root.path().join("withheld-preferences.json")).ok(),
        before
    );

    // No new grant: the same retained request survives well beyond the removed lock
    // budget, and no held attempt consumed a preference's effect-failure budget.
    publish_restore().unwrap();
    assert!(fixture.owed().unwrap());
    let later = 20 * 60 * 1000;
    let (send, receive) = mpsc::channel();
    coordinator
        .round_with(requests.current(), later, || {
            let mut fixture = fixture.clone();
            std::thread::Builder::new()
                .name("aterm-test-late-follow-up".into())
                .spawn(move || {
                    receive
                        .recv_timeout(Duration::from_secs(60))
                        .expect("release the finite worker");
                    super::super::publish_withheld_attempt(
                        fixture.attempt(|| Some(()), |_| false),
                        |_, _| {},
                        |why, _| panic!("unexpected publication refusal: {why}"),
                    )
                })
        })
        .unwrap();
    let release = Release(Some(send));
    requests.request();
    coordinator
        .round_with(requests.current(), later, || panic!("only one worker"))
        .unwrap();
    assert_eq!(coordinator.wanted, Some(requests.current()));
    drop(release);
    finished(&coordinator);
    // The earlier worker's successful retirement cannot consume the newer request.
    let mut started = false;
    coordinator
        .round_with(requests.current(), later, || {
            started = true;
            start(fixture.clone())
        })
        .unwrap();
    assert!(started);
    finished(&coordinator);
    coordinator
        .round_with(requests.current(), later, || panic!("settled intent"))
        .unwrap();
    assert!(coordinator.worker.is_none());
    assert!(coordinator.wanted.is_none());
    assert!(!fixture.owed().unwrap());
    assert_eq!(fixture.notification_center_writes(), writes + 1);
}

/// What the keeper's wait is told a follow-up owes: nothing with no request, a round at once
/// for a request no round has seen, the retry pause after a start that failed, `Running` from
/// the start of an attempt until a round reaps it, the later of the put-off and the retry
/// pause after an attempt that left changes put off, and nothing once the change is made.
/// NEGATIVE CONTROL: with the put-off left out, the put-off request reads `In(10s)`, its retry
/// pause, where its 40 s put-off is owed.
#[test]
fn a_follow_up_says_when_its_next_round_is_owed() {
    let millis = Duration::from_millis;
    let mut coordinator = Owned(Coordinator::default());
    assert_eq!(coordinator.owed_with(0, 0), Owed::Idle);
    assert_eq!(coordinator.owed_with(1, 0), Owed::In(Duration::ZERO));

    assert!(
        coordinator
            .round_with(1, 100, || Err(std::io::Error::other("no thread")))
            .is_err()
    );
    assert_eq!(
        coordinator.owed_with(1, 100),
        Owed::In(millis(FIRST_RETRY_MS))
    );
    let retry = 100 + FIRST_RETRY_MS;
    assert_eq!(coordinator.owed_with(1, retry - 1), Owed::In(millis(1)));
    assert_eq!(coordinator.owed_with(1, retry), Owed::In(Duration::ZERO));

    let (release, held) = mpsc::channel::<()>();
    coordinator
        .round_with(1, retry, || {
            std::thread::Builder::new().spawn(move || {
                let _ = held.recv();
                Attempted {
                    last: Last::Unfinished,
                    put_off_for: Some(Duration::from_secs(40)),
                }
            })
        })
        .unwrap();
    assert_eq!(coordinator.owed_with(1, retry), Owed::Running);
    release.send(()).unwrap();
    finished(&coordinator);
    assert_eq!(
        coordinator.owed_with(1, retry),
        Owed::Running,
        "an ended attempt runs until a round reaps it"
    );
    let reaped = retry + 10;
    coordinator
        .round_with(1, reaped, || panic!("put off"))
        .unwrap();
    // The second pause of the retry schedule is ten seconds; the put-off is forty.
    assert_eq!(
        coordinator.owed_with(1, reaped),
        Owed::In(Duration::from_secs(40))
    );
    assert_eq!(
        coordinator.owed_with(1, reaped + 15_000),
        Owed::In(Duration::from_secs(25))
    );
    assert_eq!(
        coordinator.owed_with(1, reaped + 40_000),
        Owed::In(Duration::ZERO)
    );
    assert_eq!(
        coordinator.owed_with(2, reaped),
        Owed::In(Duration::ZERO),
        "a new request is owed a round at once"
    );

    coordinator
        .round_with(1, reaped + 40_000, || {
            std::thread::Builder::new().spawn(|| Attempted::plain(Last::Ended))
        })
        .unwrap();
    finished(&coordinator);
    coordinator
        .round_with(1, reaped + 40_001, || panic!("made"))
        .unwrap();
    assert_eq!(coordinator.owed_with(1, reaped + 40_001), Owed::Idle);
}

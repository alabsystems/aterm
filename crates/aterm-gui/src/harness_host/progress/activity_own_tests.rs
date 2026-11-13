// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::accounts_sync::{LoginActivity, LoginIdentity};

const BORN: u64 = 100;
const NOW: i64 = 200;
const SID: &str = "s-progress";

pub(in crate::harness_host) struct Fixture {
    _root: aterm_tempfile::TempDir,
    pub(in crate::harness_host) host: HostHandle,
    pub(in crate::harness_host) kept: Arc<Mutex<Kept>>,
    pub(in crate::harness_host) seat: AccountSeat,
    pub(in crate::harness_host) live_seat: Arc<Mutex<Option<AccountSeat>>>,
    seat_reads: Arc<std::sync::atomic::AtomicUsize>,
    notes: Arc<std::sync::atomic::AtomicUsize>,
}

impl Fixture {
    pub(in crate::harness_host) fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let roster = root.path().join("accounts.toml");
        let row = accounts::Row {
            label: "alternate".into(),
            email: Some("expected@example.test".into()),
            dir: Some(root.path().join("login-a").to_str().unwrap().into()),
            ..Default::default()
        };
        std::fs::write(
            &roster,
            accounts::append_rows_for(accounts::Cli::Claude, "", &[row]).unwrap(),
        )
        .unwrap();
        let seat = AccountSeat {
            roster: Some(roster),
            state: root.path().join("state"),
            home: root.path().join("home"),
            now: NOW,
            ladder: &[],
        };
        let live_seat = Arc::new(Mutex::new(Some(seat.clone())));
        let seat_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let notes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (mut host, kept) = lifecycle_tests::fixture();
        {
            let mut held = kept.lock().unwrap();
            held.account.progress = None;
            held.account.execution_progress = None;
        }
        let current = Arc::clone(&live_seat);
        let reads = Arc::clone(&seat_reads);
        host.hooks.acts.account_seat = Arc::new(move || {
            reads.fetch_add(1, Ordering::SeqCst);
            current.lock().unwrap().clone()
        });
        let noted = Arc::clone(&notes);
        host.hooks.note = Arc::new(move |note| {
            assert!(matches!(note, HostNote::AccountProgress));
            noted.fetch_add(1, Ordering::SeqCst);
        });
        Self {
            _root: root,
            host,
            kept,
            seat,
            live_seat,
            seat_reads,
            notes,
        }
    }

    pub(in crate::harness_host) fn refresh(&self, full_wake: bool) {
        refresh_kept_with_birth(&self.host.shared, &self.host.hooks, full_wake, &|_| {
            Some(BORN)
        });
    }

    pub(in crate::harness_host) fn row(&self) -> accounts::Row {
        self.kept
            .lock()
            .unwrap()
            .account
            .activity_context
            .as_ref()
            .unwrap()
            .rows[0]
            .clone()
    }

    fn read(&self, row: &accounts::Row) -> Option<AccountProgress> {
        self.host
            .account_progress_with_activity(
                SID,
                4242,
                Some(BORN),
                Some("conversation-a"),
                || (Instant::now(), Some(NOW)),
                |_, _, _| {
                    Some(vec![LoginActivity {
                        queued: None,
                        label: row.label.clone(),
                        route: Some("mailbox".into()),
                        active: true,
                        observed_at: NOW,
                        next_at: None,
                        diagnostic: None,
                        identity: Some(LoginIdentity::for_test(row)),
                    }])
                },
            )
            .unwrap()
    }
}

#[test]
fn retained_unworked_snapshots_share_one_metadata_image_and_cache_it() {
    let fixture = Fixture::new();
    let mut second_snapshot = lifecycle_tests::snapshot();
    second_snapshot.tab = "s-unworked".into();
    let second = Arc::new(Mutex::new(Kept {
        snapshot: Some(second_snapshot),
        ..Default::default()
    }));
    fixture
        .host
        .shared
        .kept
        .lock()
        .unwrap()
        .insert("s-unworked".into(), Arc::clone(&second));
    fixture.refresh(false);
    let first = fixture
        .kept
        .lock()
        .unwrap()
        .account
        .activity_context
        .clone()
        .unwrap();
    let other = second
        .lock()
        .unwrap()
        .account
        .activity_context
        .clone()
        .unwrap();
    assert!(
        Arc::ptr_eq(&first.rows, &other.rows),
        "one coherent store image supplies the batch"
    );
    assert_eq!(fixture.notes.load(Ordering::SeqCst), 1);
    let reads = fixture.seat_reads.load(Ordering::SeqCst);
    // A frame or settle/timer pass cannot turn this unreadable file into an account read.
    std::fs::write(fixture.seat.roster.as_ref().unwrap(), "not a roster = [").unwrap();
    for _ in 0..3 {
        fixture.refresh(false);
    }
    assert_eq!(fixture.seat_reads.load(Ordering::SeqCst), reads);
    assert_eq!(fixture.notes.load(Ordering::SeqCst), 1);
    fixture.refresh(true);
    assert!(
        fixture
            .kept
            .lock()
            .unwrap()
            .account
            .activity_context
            .is_none()
    );
    assert!(second.lock().unwrap().account.activity_context.is_none());
    assert_eq!(fixture.notes.load(Ordering::SeqCst), 2);
}

#[test]
fn delayed_metadata_cannot_cross_process_generation_policy_namespace_or_birth() {
    for change in 0..8 {
        let fixture = Fixture::new();
        let capture = ActivityCapture::capture(&fixture.kept.lock().unwrap()).unwrap();
        let observed = capture.read_for_test(&fixture.seat, BORN);
        match change {
            0 => fixture.kept.lock().unwrap().snapshot.as_mut().unwrap().pid += 1,
            1 => {
                fixture
                    .kept
                    .lock()
                    .unwrap()
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .session = Some("conversation-b".into())
            }
            2 => fixture.kept.lock().unwrap().account.landing = Arc::new(Mutex::new(())),
            3 => fixture.kept.lock().unwrap().account.progress_policy = Arc::new(()),
            4 => fixture
                .live_seat
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .state
                .push("new"),
            5 => fixture
                .live_seat
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .home
                .push("new"),
            6 => fixture.live_seat.lock().unwrap().as_mut().unwrap().roster = None,
            7 => {}
            _ => unreachable!(),
        }
        assert!(!capture.publish_for_test(
            &fixture.kept,
            &fixture.host.hooks,
            &fixture.seat,
            observed,
            if change == 7 { None } else { Some(BORN) },
        ));
        let held = fixture.kept.lock().unwrap();
        assert!(
            held.account.activity_context.is_none(),
            "change {change} cannot authorize activity"
        );
        assert!(held.account.progress.is_none());
        assert!(held.account.execution_progress.is_none());
    }
}

#[test]
fn failed_or_unavailable_metadata_withdraws_only_its_context() {
    let fixture = Fixture::new();
    fixture.refresh(false);
    let progress = fixture.read(&fixture.row()).unwrap();
    assert!(matches!(progress.phase, ProgressPhase::SigningIn { .. }));
    assert_eq!(progress.eligible, None);
    assert_eq!(progress.known, None);
    assert_eq!(
        progress.observed_at, 0,
        "metadata is not an account judgment"
    );
    *fixture.live_seat.lock().unwrap() = None;
    fixture.refresh(true);
    let held = fixture.kept.lock().unwrap();
    assert!(held.account.activity_context.is_none());
    assert!(held.account.progress.is_none());
    assert!(held.account.execution_progress.is_none());
}

#[test]
fn an_activity_wake_sees_a_new_binding_before_held_upkeep_finishes() {
    let fixture = Fixture::new();
    fixture.refresh(false);
    let old = fixture.row();
    let seat = fixture.seat.clone();
    let new_dir = fixture
        ._root
        .path()
        .join("login-b")
        .to_str()
        .unwrap()
        .to_owned();
    let (introduced, introduced_rx) = std::sync::mpsc::sync_channel(1);
    let (release, release_rx) = std::sync::mpsc::sync_channel(1);
    let (finished, finished_rx) = std::sync::mpsc::sync_channel(1);
    let upkeep = std::thread::spawn(move || {
        accounts::Store::at(&seat.state)
            .transact_logins_within(&[], NOW, &mut None, |_, logins| {
                logins.labels.entry("alternate".into()).or_default().dir = Some(new_dir);
                Ok(Vec::new())
            })
            .unwrap();
        introduced.send(()).unwrap();
        release_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        finished.send(()).unwrap();
    });
    introduced_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    let new = accounts::Store::at(&fixture.seat.state)
        .read_account_records(fixture.seat.roster.as_deref(), NOW)
        .unwrap()
        .roster
        .rows[0]
        .clone();
    assert_ne!(old, new);
    assert_eq!(fixture.read(&new), None);
    // The activity observer's full wake refreshes independently of this upkeep's completion.
    fixture.refresh(true);
    assert!(matches!(
        fixture.read(&new).unwrap().phase,
        ProgressPhase::SigningIn { .. }
    ));
    assert_eq!(fixture.read(&old), None);
    assert!(matches!(
        finished_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    release.send(()).unwrap();
    finished_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    upkeep.join().unwrap();
}

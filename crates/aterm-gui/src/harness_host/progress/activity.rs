// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's bounded account metadata read, independent of recovery decisions.

use super::*;

/// The owner of a background metadata read, captured without process or store I/O.
pub(in crate::harness_host) struct ActivityCapture {
    snapshot: Snapshot,
    generation: Arc<Mutex<()>>,
    policy: Arc<()>,
}

struct ActivityRows {
    state: PathBuf,
    labels: BTreeSet<String>,
    rows: Arc<[accounts::Row]>,
}

impl ActivityRows {
    fn read(seat: &AccountSeat) -> Result<Self, String> {
        let records = accounts::Store::at(&seat.state)
            .bounded_by(seat.ladder)
            .read_account_records(seat.roster.as_deref(), seat.now)
            .map_err(|_| "account activity metadata is unavailable".to_owned())?;
        if !records.notes.is_empty() {
            return Err("account activity metadata is incomplete".into());
        }
        Ok(Self {
            state: seat.state.clone(),
            labels: records
                .roster
                .rows
                .iter()
                .map(|row| row.label.clone())
                .collect(),
            rows: records.roster.rows.into(),
        })
    }
}

impl ActivityCapture {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountExecutionVisibility",
            action = "ReplaceContext",
            reason = "The environment can replace the retained process or account generation while metadata is read."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountExecutionVisibility",
            action = "WorkStarts",
            reason = "The existing upkeep worker independently begins attributed sign-in work."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountExecutionVisibility",
            action = "ActivityBusy",
            reason = "The activity guard may be held by its existing owner during an observation."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountExecutionVisibility",
            action = "CaptureContext",
            project = "harness_host::tests::account_advice::execution_transition"
        )
    )]
    pub(in crate::harness_host) fn capture(kept: &Kept) -> Option<Self> {
        let snapshot = kept.snapshot.as_ref()?;
        if !observes(Some(snapshot.agent().program())) {
            return None;
        }
        Some(Self {
            snapshot: snapshot.clone(),
            generation: Arc::clone(&kept.account.landing),
            policy: Arc::clone(&kept.account.progress_policy),
        })
    }

    fn current(&self, kept: &Kept) -> bool {
        progress_context_current(&self.snapshot, &self.generation, kept)
            && Arc::ptr_eq(&self.policy, &kept.account.progress_policy)
    }

    fn context(&self, metadata: &ActivityRows, born: u64) -> AccountActivityContext {
        AccountActivityContext {
            snapshot: self.snapshot.clone(),
            generation: Arc::clone(&self.generation),
            born,
            state: metadata.state.clone(),
            labels: metadata.labels.clone(),
            rows: Arc::clone(&metadata.rows),
            policy: Arc::clone(&self.policy),
        }
    }

    #[cfg(test)]
    fn read_with_birth(
        &self,
        seat: &AccountSeat,
        birth: &dyn Fn(&Snapshot) -> Option<u64>,
    ) -> Result<AccountActivityContext, String> {
        let born = birth(&self.snapshot).ok_or("account activity process changed")?;
        Ok(self.context(&ActivityRows::read(seat)?, born))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountExecutionVisibility",
            action = "PublishContext",
            project = "harness_host::tests::account_advice::execution_transition"
        )
    )]
    fn publish_with_birth(
        &self,
        kept: &Mutex<Kept>,
        hooks: &Hooks,
        seat: &AccountSeat,
        observed: Result<AccountActivityContext, String>,
        birth: &dyn Fn(&Snapshot) -> Option<u64>,
    ) -> bool {
        let context = observed.ok().filter(|context| {
            context.snapshot == self.snapshot
                && Arc::ptr_eq(&context.generation, &self.generation)
                && Arc::ptr_eq(&context.policy, &self.policy)
                && context.state == seat.state
                && current_account_seat(hooks, seat).is_ok()
                && birth(&self.snapshot) == Some(context.born)
        });
        let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.current(&kept) {
            return false;
        }
        let changed = match (&kept.account.activity_context, &context) {
            (Some(old), Some(new)) => {
                !old.current(&kept)
                    || !Arc::ptr_eq(&old.policy, &new.policy)
                    || old.born != new.born
                    || old.state != new.state
                    || old.labels != new.labels
                    || old.rows != new.rows
            }
            (None, None) => false,
            _ => true,
        };
        kept.account.activity_context = context;
        changed
    }

    #[cfg(test)]
    pub(in crate::harness_host) fn read_for_test(
        &self,
        seat: &AccountSeat,
        born: u64,
    ) -> Result<AccountActivityContext, String> {
        self.read_with_birth(seat, &|_| Some(born))
    }

    #[cfg(test)]
    pub(in crate::harness_host) fn publish_for_test(
        &self,
        kept: &Mutex<Kept>,
        hooks: &Hooks,
        seat: &AccountSeat,
        observed: Result<AccountActivityContext, String>,
        born: Option<u64>,
    ) -> bool {
        self.publish_with_birth(kept, hooks, seat, observed, &|_| born)
    }
}

/// A generic wake refreshes metadata once; normal frame/settle timers reuse current context.
/// All retained Claude snapshots participate, including those whose harness is switched Off.
pub(in crate::harness_host) fn refresh_kept(shared: &Shared, hooks: &Hooks, full_wake: bool) {
    refresh_kept_with_birth(shared, hooks, full_wake, &Snapshot::current_start_seconds);
}

fn refresh_kept_with_birth(
    shared: &Shared,
    hooks: &Hooks,
    full_wake: bool,
    birth: &dyn Fn(&Snapshot) -> Option<u64>,
) {
    let retained: Vec<_> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .cloned()
        .collect();
    let mut changed = false;
    let mut pending = Vec::new();
    for kept in retained {
        let capture = {
            let mut held = kept.lock().unwrap_or_else(PoisonError::into_inner);
            match ActivityCapture::capture(&held) {
                None => {
                    changed |= held.account.activity_context.take().is_some();
                    None
                }
                Some(capture) => {
                    let cached = held
                        .account
                        .activity_context
                        .as_ref()
                        .is_some_and(|context| {
                            context.current(&held)
                                && Arc::ptr_eq(&context.policy, &held.account.progress_policy)
                        });
                    (full_wake || !cached).then_some(capture)
                }
            }
        };
        if let Some(capture) = capture {
            let born = birth(&capture.snapshot);
            pending.push((kept, capture, born));
        }
    }
    if !pending.is_empty() {
        // A single coherent store image supplies every snapshot in this pass.
        let seat = (hooks.acts.account_seat)();
        let metadata = seat.as_ref().map(ActivityRows::read);
        for (kept, capture, born) in pending {
            let observed = metadata
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .zip(born)
                .map(|(metadata, born)| capture.context(metadata, born));
            if let Some(seat) = &seat {
                changed |= capture.publish_with_birth(
                    &kept,
                    hooks,
                    seat,
                    observed.ok_or_else(|| "account activity metadata is unavailable".into()),
                    birth,
                );
            } else {
                let mut held = kept.lock().unwrap_or_else(PoisonError::into_inner);
                if capture.current(&held) {
                    changed |= held.account.activity_context.take().is_some();
                }
            }
        }
    }
    if changed {
        (hooks.note)(HostNote::AccountProgress);
    }
}

#[cfg(test)]
pub(super) mod tests {
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
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::cli::{RenewalExit, RenewalObservation};

fn read_activity(
    bed: &Bed,
    guard: &Arc<Mutex<AutoRenewGuard>>,
    clock: Instant,
) -> Vec<LoginActivity> {
    login_activity_of(
        &guard.lock().unwrap(),
        &bed.store,
        Cli::Claude,
        NOW + 1,
        clock,
    )
}

#[cfg(unix)]
#[test]
fn a_running_shared_login_keeps_its_captured_row_after_roster_replacement() {
    for change_email in [false, true] {
        let bed = Bed::new(None);
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let records = bed
            .store
            .read_account_records(Some(&bed.roster), NOW)
            .unwrap();
        let prior_binding = records.logins.get("shared").unwrap();
        let original = records.roster.row("shared").unwrap().clone();
        let mut replacement = original.clone();
        if change_email {
            replacement.email = Some("replacement@example.test".into());
        } else {
            replacement.dir = Some(bed.home.join("replacement-login").to_str().unwrap().into());
        }
        let queued = Arc::new(Mutex::new(Vec::new()));
        let queued_by_worker = queued.clone();
        let queue = move |work| {
            queued_by_worker.lock().unwrap().push(work);
            Ok(())
        };
        std::thread::scope(|scope| {
            let (entered, reached) = std::sync::mpsc::sync_channel(1);
            let (resume, resumed) = std::sync::mpsc::sync_channel(1);
            *bed.provider.flow_effect.lock().unwrap() = Some(Box::new(move || {
                let _ = entered.send(());
                match resumed.recv_timeout(aterm_spec::wait::MUST_HAPPEN) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        panic!("provider release hung")
                    }
                }
            }));
            bed.upkeep_observed(Some(&bed.factory()), &queue, true, &guard, Arc::new(|_| {}));
            let queued_rows = read_activity(&bed, &guard, Instant::now());
            let queued_row = queued_rows
                .iter()
                .find(|activity| activity.matches_row(&original))
                .unwrap();
            assert!(!queued_row.active);
            assert!(queued_row.next_at.is_some());
            assert!(
                queued_rows
                    .iter()
                    .all(|activity| !activity.matches_row(&replacement))
            );
            let work = queued.lock().unwrap().pop().expect("shared login queued");
            let running = scope.spawn(work);
            reached
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("provider entered");

            fs::write(
                &bed.roster,
                accounts::append_rows_for(Cli::Claude, "", &[replacement.clone()]).unwrap(),
            )
            .unwrap();
            let read =
                Roster::parse_for(Cli::Claude, &fs::read_to_string(&bed.roster).unwrap()).unwrap();
            let replacement = accounts::bound(&read, &records.logins)
                .row("shared")
                .unwrap()
                .clone();
            let rows = read_activity(&bed, &guard, Instant::now());
            let active = rows
                .iter()
                .find(|activity| activity.matches_row(&original))
                .unwrap();
            assert!(active.active);
            assert_eq!(active.route.as_deref(), Some("mailbox"));
            assert!(
                rows.iter()
                    .all(|activity| !activity.matches_row(&replacement))
            );
            let debug = format!("{:?}", active.identity);
            assert!(!debug.contains(EMAIL));
            assert!(!debug.contains(original.dir.as_deref().unwrap()));
            resume.send(()).expect("provider remains blocked");
            running.join().unwrap();
        });
        assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 1);
        assert!(
            read_activity(&bed, &guard, Instant::now())
                .iter()
                .all(|activity| !activity.active)
        );
        let logins = bed.store.load_logins(NOW + 1).unwrap().0;
        let binding = logins.get("shared").unwrap();
        let attempt = logins.shared.latest(SOURCE).unwrap();
        assert_eq!(attempt.outcome, accounts::shared::Outcome::Unknown);
        assert_eq!(attempt.intent.generation, 2);
        // Preparation advances the generation fence without publishing a new login.
        assert!(binding.renewal_generation > prior_binding.renewal_generation);
        assert_eq!(
            attempt.intent.before_generation,
            Some(binding.renewal_generation)
        );
        assert_eq!(binding.dir, prior_binding.dir);
        assert_eq!(binding.renewed_at, prior_binding.renewed_at);
        assert_eq!(
            binding.last_success_generation,
            prior_binding.last_success_generation
        );
        assert_eq!(binding.proved_by, None);
        assert_eq!(binding.proved_email, None);
        assert_eq!(binding.proved_dir, None);
        assert_eq!(binding.sign_in_attempt, prior_binding.sign_in_attempt);
    }
}

#[test]
fn activity_retry_and_active_owners_merge_only_for_the_same_captured_row() {
    let bed = Bed::new(None);
    let records = bed
        .store
        .read_account_records(Some(&bed.roster), NOW)
        .unwrap();
    let original = records.roster.row("shared").unwrap().clone();
    let mut retry_row = original.clone();
    retry_row.email = Some("retry@example.test".into());
    let mut active_row = original.clone();
    active_row.dir = Some("synthetic-active-home".into());
    let clock = Instant::now();
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let diagnostic = LoginDiagnostic::from_exit_for_test(&original, NOW, 1, None);
    {
        let mut state = guard.lock().unwrap();
        state.last_attempt.insert(original.label.clone(), clock);
        let mut owner =
            LoginOwner::new(&bed.store, Cli::Claude, &original.label, NOW).for_row(&original);
        owner.diagnostic = Some(diagnostic.clone());
        state.activity_owners.insert(original.label.clone(), owner);
        state
            .ladder
            .failed(&original.label, "observed route", clock);
        state.ladder_owners.insert(
            original.label.clone(),
            LoginOwner::new(&bed.store, Cli::Claude, &original.label, NOW).for_row(&retry_row),
        );
    }
    let active = LoginActivityGuard::begin_row(
        &guard,
        &bed.store,
        Cli::Claude,
        &active_row,
        Some("profile"),
    );
    let rows = read_activity(&bed, &guard, clock);
    assert_eq!(rows.len(), 3);
    let original_activity = rows.iter().find(|row| row.matches_row(&original)).unwrap();
    assert!(!original_activity.active);
    assert_eq!(original_activity.next_at, Some(NOW + 1 + 900));
    assert_eq!(original_activity.diagnostic, Some(diagnostic));
    let retry = rows.iter().find(|row| row.matches_row(&retry_row)).unwrap();
    assert!(!retry.active && retry.next_at.is_some() && retry.diagnostic.is_none());
    let current = rows
        .iter()
        .find(|row| row.matches_row(&active_row))
        .unwrap();
    assert!(current.active && current.next_at.is_none() && current.diagnostic.is_none());
    assert_eq!(current.route.as_deref(), Some("profile"));
    drop(active);

    {
        let mut state = guard.lock().unwrap();
        state
            .ladder_owners
            .get_mut(&original.label)
            .unwrap()
            .identity = Some(LoginIdentity::of(&original));
    }
    let active =
        LoginActivityGuard::begin_row(&guard, &bed.store, Cli::Claude, &original, Some("mailbox"));
    let rows = read_activity(&bed, &guard, clock);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].matches_row(&original) && rows[0].active);
    assert_eq!(rows[0].route.as_deref(), Some("mailbox"));
    assert!(rows[0].next_at.is_some_and(|next| next <= NOW + 1 + 900));
    drop(active);

    guard
        .lock()
        .unwrap()
        .activity_owners
        .get_mut(&original.label)
        .unwrap()
        .identity = None;
    let rows = read_activity(&bed, &guard, clock);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().filter(|row| row.matches_row(&original)).count(),
        1
    );
    assert!(
        rows.iter()
            .find(|row| row.identity.is_none())
            .is_some_and(|row| !row.matches_row(&original))
    );
}

#[test]
fn an_active_replacement_cannot_clear_or_complete_another_rows_owner() {
    let bed = Bed::new(None);
    let records = bed
        .store
        .read_account_records(Some(&bed.roster), NOW)
        .unwrap();
    let original = records.roster.row("shared").unwrap();
    let mut replacement = original.clone();
    replacement.email = Some("replacement@example.test".into());
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let clock = Instant::now();
    let permit = AutoRenewPermit::claim(&guard, &original.label, clock, true).unwrap();
    permit.attribute_row(&bed.store, Cli::Claude, original, NOW, &records.logins);
    let diagnostic = LoginDiagnostic::from_exit_for_test(original, NOW, 1, None);
    guard
        .lock()
        .unwrap()
        .activity_owners
        .get_mut(&original.label)
        .unwrap()
        .diagnostic = Some(diagnostic.clone());
    let active = LoginActivityGuard::begin_row(
        &guard,
        &bed.store,
        Cli::Claude,
        &replacement,
        Some("mailbox"),
    );
    assert!(active.owner_token.is_none());
    active.complete(
        &replacement,
        NOW + 1,
        RenewalObservation::from_outcome(&Ok(RenewalExit::from(1))),
    );
    let rows = read_activity(&bed, &guard, clock);
    assert_eq!(
        rows.iter()
            .find(|row| row.matches_row(original))
            .unwrap()
            .diagnostic,
        Some(diagnostic)
    );
    assert!(
        rows.iter()
            .find(|row| row.matches_row(&replacement))
            .unwrap()
            .diagnostic
            .is_none()
    );
    drop(active);
    let active =
        LoginActivityGuard::begin_row(&guard, &bed.store, Cli::Claude, original, Some("mailbox"));
    assert!(active.owner_token.is_some());
    active.complete(
        original,
        NOW + 1,
        RenewalObservation::from_outcome(&Ok(RenewalExit::from(1))),
    );
    assert_eq!(
        read_activity(&bed, &guard, clock)
            .iter()
            .find(|row| row.matches_row(original))
            .unwrap()
            .diagnostic
            .as_ref()
            .unwrap()
            .completed_at,
        NOW + 1
    );
}

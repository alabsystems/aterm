// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use agh::journal::{IntentDraft, IntentStore as _, JournalRecord, Settled};
use agh::policy::{RouteRequest, RouteTarget};
use agh::write::WriteOp;
use std::sync::{
    Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};

#[test]
fn native_preparation_has_no_intent_and_the_receive_has_one() {
    let world = World::canonical("native-late-intent");
    let (remote, source, before, after) = incremental_source(&world);
    let effects = Arc::new(OnceLock::<Weak<Fx>>::new());
    let observed_effects = Arc::clone(&effects);
    let store = Arc::new(OnceLock::<Arc<agh::Journal>>::new());
    let observed_store = Arc::clone(&store);
    let received = Arc::new(AtomicBool::new(false));
    let observed_receive = Arc::clone(&received);
    let fx = Arc::new(
        Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
            let effects = observed_effects.get().unwrap().upgrade().unwrap();
            let child = &effects.children_run()[span.ordinal];
            let journal = observed_store.get().unwrap();
            if child.args.iter().any(|arg| arg == "push")
                && !child.args.iter().any(|arg| arg == "--dry-run")
            {
                assert_eq!(
                    journal.open().unwrap().len(),
                    1,
                    "receive must already be fenced"
                );
                observed_receive.store(true, Ordering::SeqCst);
            } else if !observed_receive.load(Ordering::SeqCst) {
                assert!(
                    journal.read().unwrap().0.is_empty(),
                    "preparation must create no intent"
                );
            }
        }),
    );
    assert!(effects.set(Arc::downgrade(&fx)).is_ok());
    let gh = world
        .client(git_env())
        .fx(fx)
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let slug = common::slug("fixture-dev/app");
    let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
    let journal = Arc::new(gh.journal(&slug).unwrap());
    assert!(store.set(Arc::clone(&journal)).is_ok());
    let batch = push_batch(&[("refs/heads/main", &before, &after)]);
    let error = gh
        .run_git(&GitRun::push_refs(&slug, &source, batch.clone(), false))
        .unwrap_err();
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::Usage, SideEffect::None)
    );
    assert!(
        gh.fx().children_run().is_empty(),
        "the unjournaled entry must refuse before children"
    );
    gh.refs(&slug).check_push(&permit, &source, &batch).unwrap();
    assert!(journal.read().unwrap().0.is_empty());
    gh.refs(&slug)
        .push_atomic(&permit, &source, &batch, journal.as_ref())
        .unwrap()
        .into_result()
        .unwrap();
    assert!(received.load(Ordering::SeqCst));
    let records = journal.read().unwrap().0;
    assert_eq!(
        records
            .iter()
            .filter(|(_, row)| matches!(row, JournalRecord::Intent(_)))
            .count(),
        1
    );
    assert!(journal.open().unwrap().is_empty());
    assert_eq!(host_git(&remote, &["rev-parse", "main"]), after);
}

#[test]
fn a_failed_or_abandoned_native_preparation_creates_no_intent() {
    for abandoned in [false, true] {
        let world = World::canonical("native-preparation-no-intent");
        let (remote, source, before, after) = incremental_source(&world);
        let slug = common::slug("fixture-dev/app");
        let batch = push_batch(&[("refs/heads/main", &before, &after)]);
        let effects = Arc::new(OnceLock::<Weak<Fx>>::new());
        let observed_effects = Arc::clone(&effects);
        let interrupted = Arc::new(AtomicBool::new(false));
        let observed_interruption = Arc::clone(&interrupted);
        let fx = Arc::new(
            Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
                let effects = observed_effects.get().unwrap().upgrade().unwrap();
                let child = &effects.children_run()[span.ordinal];
                if child.args.iter().any(|arg| arg == "pack-objects")
                    && !observed_interruption.swap(true, Ordering::SeqCst)
                {
                    if abandoned {
                        panic!("the owner abandoned preparation before receive");
                    }
                    let prefix = Path::new(child.args.last().unwrap());
                    let packs: Vec<_> = std::fs::read_dir(prefix.parent().unwrap())
                        .unwrap()
                        .map(|entry| entry.unwrap().path())
                        .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
                        .collect();
                    assert_eq!(packs.len(), 1, "the first private metadata pack");
                    std::fs::remove_file(&packs[0]).unwrap();
                }
            }),
        );
        assert!(effects.set(Arc::downgrade(&fx)).is_ok());
        let gh = world
            .client(git_env())
            .fx(fx)
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
        let journal = gh.journal(&slug).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gh.refs(&slug)
                .push_atomic(&permit, &source, &batch, &journal)
        }));
        assert!(interrupted.load(Ordering::SeqCst));
        if abandoned {
            assert!(result.is_err());
        } else {
            let error = result.unwrap().unwrap_err();
            assert_eq!(
                (error.kind, error.side_effect),
                (Kind::CouldNotRun, SideEffect::None)
            );
        }
        assert!(
            journal.read().unwrap().0.is_empty(),
            "not even a never-sent intent is owed"
        );
        assert!(
            !gh.fx()
                .children_run()
                .iter()
                .any(|child| child.args.iter().any(|arg| arg == "push"))
        );
        assert_eq!(host_git(&remote, &["rev-parse", "main"]), before);
    }
}

#[test]
fn a_competing_open_or_closed_intent_during_preparation_prevents_receive() {
    for closed in [false, true] {
        let world = World::canonical("native-preparation-intent-race");
        let (remote, source, before, after) = incremental_source(&world);
        let effects = Arc::new(OnceLock::<Weak<Fx>>::new());
        let observed_effects = Arc::clone(&effects);
        let held = AtomicBool::new(false);
        let (ready, waiting) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let resume = Mutex::new(resume);
        let fx = Arc::new(
            Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
                let effects = observed_effects.get().unwrap().upgrade().unwrap();
                let child = &effects.children_run()[span.ordinal];
                if child.args.iter().any(|arg| arg == "ls-remote")
                    && !held.swap(true, Ordering::SeqCst)
                {
                    ready.send(()).unwrap();
                    resume
                        .lock()
                        .unwrap()
                        .recv_timeout(common::HANG)
                        .expect("preparation released");
                }
            }),
        );
        assert!(effects.set(Arc::downgrade(&fx)).is_ok());
        let gh = world
            .client(git_env())
            .fx(Arc::clone(&fx))
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        let slug = common::slug("fixture-dev/app");
        let request = RouteRequest::write(RouteTarget::Repo(slug.clone()));
        let permit = common::permit(&gh, &request);
        let rival = IntentDraft::new(
            WriteOp::RefUpdateAtomic,
            &slug,
            &permit,
            &agh::identity::fingerprint(agh_fake::token("dev").as_bytes()),
            "atomic refs",
        );
        let journal = Arc::new(gh.journal(&slug).unwrap());
        let owned_store = Arc::clone(&journal);
        let batch = push_batch(&[("refs/heads/main", &before, &after)]);
        let runner = std::thread::spawn(move || {
            gh.refs(&slug)
                .push_atomic(&permit, &source, &batch, owned_store.as_ref())
        });
        waiting
            .recv_timeout(common::HANG)
            .expect("the first remote read held preparation");
        assert!(journal.read().unwrap().0.is_empty());
        let winner = journal.begin(&rival).unwrap();
        if closed {
            journal.settle(&winner, &Settled::NeverSent).unwrap();
        }
        release.send(()).unwrap();
        let error = runner.join().unwrap().unwrap_err();
        assert_eq!(
            (error.kind, error.side_effect),
            (Kind::OutcomeUnknown, SideEffect::None)
        );
        assert!(matches!(error.sub, Sub::IntentOpen { .. }));
        assert_eq!(
            journal
                .read()
                .unwrap()
                .0
                .iter()
                .filter(|(_, row)| matches!(row, JournalRecord::Intent(_)))
                .count(),
            1
        );
        assert!(
            !fx.children_run()
                .iter()
                .any(|child| child.args.iter().any(|arg| arg == "push"))
        );
        assert_eq!(host_git(&remote, &["rev-parse", "main"]), before);
    }
}

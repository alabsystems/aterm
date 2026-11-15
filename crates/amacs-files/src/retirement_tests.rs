// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use amacs::editor::PreparedRecovery;
use amacs::store::PreparedText;
use std::sync::{Arc, mpsc};

#[test]
fn canceled_result_and_history_keep_the_host_draining_after_registry_release() {
    let registry = Arc::new(Mutex::new(None));
    let worker_registry = registry.clone();
    let (results, receiver) = mpsc::channel();
    let (receipts, observations) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let producer = std::thread::spawn(move || {
        let base = PreparedText::prepare_reserved(
            Arc::new("disk é\n".repeat(4096)),
            None,
            reservation_on(&worker_registry).unwrap(),
        );
        let recovered = PreparedRecovery::new(&base, &"draft 新\n".repeat(4096))
            .unwrap()
            .unwrap();
        receipts.send(recovered.retirement_receipts()).unwrap();
        wait.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
        // A canceled UI receiver destroys this actual result on its producer.
        assert!(results.send((base, recovered)).is_err());
    });
    let (text, history) = observations
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    let host = registry.lock().unwrap().take().unwrap();
    assert!(host.service.upgrade().is_some());
    drop(receiver);
    drop(registry);
    release.send(()).unwrap();
    producer.join().unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "reserved retirement drain exit",
        || host.worker.is_finished().then_some(()),
    );
    host.worker.join().unwrap();
    assert!(host.service.upgrade().is_none());
    let text = text.poll().unwrap();
    let history = history.poll().unwrap();
    use std::hash::{Hash, Hasher};
    let mut ui = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut ui);
    assert_ne!(text.executor, ui.finish());
    assert_eq!(text.executor, history.executor);
    assert!(text.work.retired_rope_nodes > 0);
    assert!(history.work.retired_history_edits > 0);
}

#[test]
fn host_reaps_a_drained_executor_before_replacing_it() {
    let registry = Mutex::new(None);
    drop(reservation_on(&registry).unwrap());
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "empty retirement executor exit",
        || {
            registry
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .worker
                .is_finished()
                .then_some(())
        },
    );
    let next = reservation_on(&registry).unwrap();
    assert!(
        registry
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .service
            .upgrade()
            .is_some()
    );
    drop(next);
    let host = registry.lock().unwrap().take().unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "replacement executor exit",
        || host.worker.is_finished().then_some(()),
    );
    host.worker.join().unwrap();
}

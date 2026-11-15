// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::store::DocumentStore;
use std::sync::mpsc;

/// Explicit test host: the engine only receives queue ownership and identities.
pub(crate) struct TestPool {
    service: Option<RetirementService>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl std::ops::Deref for TestPool {
    type Target = RetirementService;
    fn deref(&self) -> &Self::Target {
        self.service.as_ref().unwrap()
    }
}
impl TestPool {
    pub(crate) fn start(concurrency: usize) -> Self {
        let (service, driver) =
            RetirementService::channel(std::num::NonZeroUsize::new(concurrency).unwrap());
        let worker = std::thread::spawn(move || driver.run(RetirementExecutor { identity: 1 }));
        Self {
            service: Some(service),
            worker: Some(worker),
        }
    }
    pub(crate) fn shutdown(mut self) {
        drop(self.service.take());
        self.worker.take().unwrap().join().unwrap();
    }
}

impl Drop for TestPool {
    fn drop(&mut self) {
        drop(self.service.take());
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
fn supplied_drainer_owns_deterministic_retirement_and_sync_values_need_no_executor() {
    let (service, mut drainer) =
        RetirementService::channel(std::num::NonZeroUsize::new(1).unwrap());
    let prepared =
        PreparedText::prepare_reserved(Arc::new("é\n".repeat(4096)), None, service.reserve());
    let receipt = prepared.retirement_receipt();
    let weak = service.downgrade();
    drop(service);
    assert!(weak.upgrade().is_some());
    take_preparation_work();
    drop(prepared);
    assert!(weak.upgrade().is_none());
    assert!(drainer.0.queue.lock().unwrap().stopped);
    let transfer = take_preparation_work();
    assert_eq!(transfer.retirement_transfers, 1);
    assert_eq!(transfer.retired_rope_nodes, 0);
    assert!(receipt.poll().is_none());
    assert!(drainer.drain_one(RetirementExecutor { identity: 73 }));
    let observation = receipt.poll().unwrap();
    assert_eq!(observation.executor, 73);
    assert!(observation.work.retired_rope_nodes > 0);
    assert!(!drainer.drain_one(RetirementExecutor { identity: 73 }));
    take_preparation_work();
    drop(PreparedText::new("é\n".repeat(4096)));
    assert!(take_preparation_work().retired_rope_nodes > 0);
}

fn released(receipt: &RetirementReceipt) -> RetirementObservation {
    receipt.wait()
}

#[test]
fn unused_graphs_and_provisional_cancellation_release_nodes_only_on_the_reserved_worker() {
    let mut previous = 0;
    for repeats in [1, 131_072] {
        let pool = TestPool::start(1);
        let worker_pool = pool.clone();
        let text = std::thread::spawn(move || {
            PreparedText::prepare_reserved(
                Arc::new("é line\n".repeat(repeats)),
                None,
                worker_pool.reserve(),
            )
        })
        .join()
        .unwrap();
        let receipt = text.retirement_receipt();
        let clone = text.clone();
        take_preparation_work();
        drop(text);
        assert_eq!(take_preparation_work(), PreparationWork::default());
        drop(clone);
        assert_eq!(
            take_preparation_work(),
            PreparationWork {
                retirement_transfers: 1,
                ..Default::default()
            }
        );
        let observed = released(&receipt);
        eprintln!("unused-final-release repeats={repeats} observation={observed:?}");
        assert_eq!(observed.executor, 1);
        assert!(observed.work.retired_rope_nodes > previous);
        previous = observed.work.retired_rope_nodes;
        pool.shutdown();

        let pool = TestPool::start(1);
        let worker_pool = pool.clone();
        let prepared = std::thread::spawn(move || {
            PreparedText::prepare_reserved(
                Arc::new("é line\n".repeat(repeats)),
                None,
                worker_pool.reserve(),
            )
        })
        .join()
        .unwrap();
        let receipt = prepared.retirement_receipt();
        let mut store = DocumentStore::for_test();
        let document = store
            .open_prepared("mem://cancel".into(), prepared)
            .unwrap();
        take_preparation_work();
        store.remove_if_unattached(document).unwrap();
        assert_eq!(
            take_preparation_work(),
            PreparationWork {
                retirement_transfers: 1,
                ..Default::default()
            }
        );
        assert_eq!(released(&receipt).executor, 1);
        assert!(released(&receipt).work.retired_rope_nodes > 0);
        pool.shutdown();

        // Negative control: final synchronous storage release really visits nodes
        // on this thread; zero byte-copy counters alone would miss that work.
        let immediate = PreparedText::new("é line\n".repeat(repeats));
        take_preparation_work();
        drop(immediate);
        let control = take_preparation_work();
        eprintln!("unused-final-release-negative repeats={repeats} ui={control:?}");
        assert!(control.retired_rope_nodes > 0);
    }
}

#[test]
fn complete_unpublished_transactions_progress_with_one_constructor_and_retire_without_ui_waits() {
    for recovered in [false, true] {
        let pool = TestPool::start(1);
        let mut store = DocumentStore::for_test();
        let mut workspace = crate::editor::EditorWorkspace::new();
        let mut documents = Vec::new();
        let mut receipts = Vec::new();
        // No leaf is published while any other leaf is still being prepared.
        // A one-constructor pool must not wait on this retained prefix.
        for leaf in 0..2 {
            let worker_pool = pool.clone();
            let (send, receive) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let text = PreparedText::prepare_reserved(
                    Arc::new("old é\n".repeat(2048)),
                    None,
                    worker_pool.reserve(),
                );
                let recovery = if recovered {
                    crate::editor::PreparedRecovery::new(&text, &"new 新\n".repeat(2048)).unwrap()
                } else {
                    text.discard_recovery_reservation();
                    None
                };
                send.send((text, recovery)).unwrap();
            });
            let (text, recovery) = receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
            worker.join().unwrap();
            let receipt = text.retirement_receipt();
            let document = store
                .open_prepared(format!("mem://leaf-{leaf}"), text)
                .unwrap();
            if let Some(recovery) = recovery {
                let (target, history) = recovery.retirement_receipts();
                receipts.push(target);
                receipts.push(history);
                let base = store.snapshot(document).unwrap().seq;
                workspace
                    .adopt_recovery(&mut store, document, base, recovery)
                    .unwrap();
            }
            receipts.push(receipt);
            documents.push(document);
        }
        assert_eq!(store.document_ids().len(), 2);
        assert!(documents.iter().all(|id| store.view_count(*id) == Some(0)));
        let blocker_pool = pool.clone();
        let blocker = std::thread::spawn(move || {
            let PreparationReservation {
                initial,
                recovered: _,
                history: _,
                work: _work,
            } = blocker_pool.reserve();
            initial
        })
        .join()
        .unwrap();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        blocker.retire(Evidence::default(), move || {
            entered.send(()).unwrap();
            release_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
        });
        entered_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        take_preparation_work();
        for document in documents {
            store.remove_if_unattached(document).unwrap();
            workspace.forget(document);
        }
        let ui = take_preparation_work();
        assert_eq!(ui.retired_rope_nodes, 0);
        assert_eq!(ui.retired_history_edits, 0);
        {
            let queue = pool.service.as_ref().unwrap().0.queue.lock().unwrap();
            assert!(queue.retiring);
            assert_eq!(queue.constructing, 0);
            assert_eq!(queue.pending.len(), if recovered { 4 } else { 2 });
        }
        let waiting_pool = pool.clone();
        let (waiting, waiting_rx) = mpsc::channel();
        let (finished, finished_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            waiting.send(()).unwrap();
            let reservation = waiting_pool.reserve();
            finished.send(reservation).unwrap();
        });
        waiting_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert!(matches!(
            finished_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        release.send(()).unwrap();
        drop(
            finished_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap(),
        );
        worker.join().unwrap();
        for receipt in receipts {
            assert_eq!(released(&receipt).executor, 1);
        }
        pool.shutdown();
    }
}

#[test]
fn one_shot_adoption_reuses_a_live_canonical_head_but_refuses_after_its_identity_is_gone() {
    let prepared = PreparedText::new("disk".into());
    let mut store = DocumentStore::for_test();
    let id = store
        .open_prepared("mem://once".into(), prepared.clone())
        .unwrap();
    assert_eq!(
        store
            .open_prepared("mem://once".into(), prepared.clone())
            .unwrap(),
        id
    );
    store.remove_if_unattached(id).unwrap();
    assert_eq!(
        store.open_prepared("mem://once".into(), prepared),
        Err(crate::store::DocumentError::PreparedConsumed)
    );
}

#[test]
fn directory_navigation_final_release_is_on_the_reserved_worker() {
    for entries in [1, 512] {
        let pool = TestPool::start(1);
        let worker_pool = pool.clone();
        let prepared = std::thread::spawn(move || {
            let reservation = worker_pool.reserve();
            let listing = Arc::new(DirectoryListing {
                text: "entry\n".repeat(entries),
                parent: None,
                entries: (0..entries)
                    .map(|i| (i * 6..(i + 1) * 6, format!("file:///entry-{i}")))
                    .collect(),
            });
            PreparedText::prepare_reserved(
                Arc::new(listing.text.clone()),
                Some(listing),
                reservation,
            )
        })
        .join()
        .unwrap();
        let receipt = prepared.retirement_receipt();
        take_preparation_work();
        drop(prepared);
        let ui = take_preparation_work();
        assert_eq!(ui.retired_directory_entries, 0);
        let observed = released(&receipt);
        assert_eq!(observed.work.retired_directory_entries, entries);
        assert_eq!(observed.executor, 1);
        pool.shutdown();
    }
}

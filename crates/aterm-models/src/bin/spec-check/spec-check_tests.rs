// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_one_slot_pool_keeps_progress_without_an_extra_cpu() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Arc::new(aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 1).unwrap());
    let implicit = pool.acquire(&|| false).unwrap();
    let executor = worker_executor(pool.clone());
    let began = std::time::Instant::now();
    let bounded: Arc<aterm_spec::check::WorkerExecutor> = Arc::new(move |worker, spent, run| {
        executor(
            worker,
            &|| spent() || began.elapsed() >= std::time::Duration::from_secs(60),
            run,
        );
    });
    let answers = aterm_spec::check::with_worker_executor(bounded, || {
        aterm_spec::check::each_on(4, &[1, 2, 3, 4], |value| value * value)
    });
    assert_eq!(answers, [1, 4, 9, 16]);
    assert!(pool.try_acquire().unwrap().is_none());
    drop(implicit);
    assert!(pool.try_acquire().unwrap().is_some());
}

#[test]
fn an_auxiliary_holds_a_real_token_while_its_queue_runs() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Arc::new(aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 2).unwrap());
    let _implicit = pool.acquire(&|| false).unwrap();
    let executor = worker_executor(pool.clone());
    let mut ran = false;
    executor(1, &|| false, &mut || {
        ran = true;
        assert!(pool.try_acquire().unwrap().is_none());
    });
    assert!(ran);
    executor(0, &|| false, &mut || {
        assert!(
            pool.try_acquire().unwrap().is_some(),
            "the inherited worker borrows no second token"
        );
    });
}

#[test]
fn a_panicked_inherited_worker_ends_auxiliary_waits() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Arc::new(aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 1).unwrap());
    let _implicit = pool.acquire(&|| false).unwrap();
    let executor = worker_executor(pool.clone());
    let stopped_by_scope = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = stopped_by_scope.clone();
    let began = std::time::Instant::now();
    let bounded: Arc<aterm_spec::check::WorkerExecutor> = Arc::new(move |worker, stopped, run| {
        executor(
            worker,
            &|| stopped() || began.elapsed() >= std::time::Duration::from_secs(60),
            run,
        );
        if worker != 0 {
            observed.store(stopped(), std::sync::atomic::Ordering::SeqCst);
        }
    });
    let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        aterm_spec::check::with_worker_executor(bounded, || {
            aterm_spec::check::each_on(2, &[0, 1], |value| {
                assert_ne!(*value, 0, "inherited worker panic control");
                *value
            })
        })
    }));
    assert!(answer.is_err());
    assert!(
        stopped_by_scope.load(std::sync::atomic::Ordering::SeqCst),
        "the scope, rather than the hang detector, ended the empty-pool wait"
    );
}

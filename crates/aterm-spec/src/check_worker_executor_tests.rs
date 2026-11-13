// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn declined_auxiliaries_keep_every_item_and_nested_admission() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let executor: Arc<WorkerExecutor> = Arc::new(move |worker, _, run| {
        observed.fetch_add(1, Ordering::SeqCst);
        if worker == 0 {
            run();
        }
    });
    let answers = with_worker_executor(executor, || {
        each_on(3, &[1, 2, 3], |value| {
            each_on(2, &[*value, value + 1], |v| *v).iter().sum::<i32>()
        })
    });
    assert_eq!(answers, [3, 5, 7]);
    assert_eq!(calls.load(Ordering::SeqCst), 9);
    assert!(WORKER_EXECUTOR.with_borrow(Option::is_none));
}

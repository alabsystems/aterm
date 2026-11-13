// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/app_state.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::PendingHandlerWork;

#[test]
fn pending_handler_work_waits_out_a_nested_handler_and_keeps_fifo_order() {
    let mut pending = PendingHandlerWork::new();
    pending.push(11);
    pending.push(22);

    assert!(pending.arm());
    assert!(!pending.arm(), "only one run-loop block is armed");
    assert_eq!(pending.begin_batch(true), 0, "a nested handler consumes nothing");
    assert!(!pending.queue.is_empty());

    assert!(pending.arm(), "the outer handler can rearm after it returns");
    assert_eq!(pending.begin_batch(false), 2);
    assert_eq!(pending.pop_batch_item(), 11);
    pending.push(33);
    assert!(!pending.can_run_inline(), "callbacks cannot overtake active handler work");
    assert!(!pending.arm(), "active handler work cannot arm a nested delivery");
    assert_eq!(pending.pop_batch_item(), 22);
    pending.finish_batch();
    assert!(pending.arm(), "work appended by a batch waits for the next block");
    assert_eq!(pending.begin_batch(false), 1);
    assert_eq!(pending.pop_batch_item(), 33);
    assert!(!pending.can_run_inline(), "the last dequeued item remains active until it finishes");
    pending.finish_batch();
    assert!(!pending.arm(), "an empty FIFO arms no block");
    assert!(pending.can_run_inline());

    pending.push(44);
    pending.push(55);
    assert!(pending.arm());
    assert_eq!(pending.begin_batch(false), 2);
    assert_eq!(pending.pop_batch_item(), 44);
    pending.push(66);
    let discarded = pending.finish_batch_and_take_tail();
    assert_eq!(discarded.into_iter().collect::<Vec<_>>(), [55, 66]);
    assert!(!pending.arm(), "an exit barrier leaves no tail to rearm");
    assert!(pending.can_run_inline(), "window destruction can run inline after the barrier");
}

#[test]
fn an_owed_batch_takes_the_queue_armed_or_not_but_never_inside_a_batch() {
    let mut pending = PendingHandlerWork::new();
    pending.push(1);
    pending.push(2);
    assert!(pending.arm());
    assert!(pending.begin_owed(), "an armed block's work is owed too");
    assert!(!pending.begin_owed(), "a batch in progress owns its items");
    assert!(!pending.arm(), "nothing arms during an owed batch");
    assert_eq!(pending.pop_owed(), Some(1));
    pending.push(3);
    assert_eq!(pending.pop_owed(), Some(2));
    assert_eq!(pending.pop_owed(), Some(3), "work queued while the batch runs joins it");
    assert_eq!(pending.pop_owed(), None);
    pending.push(4);
    let given_up = pending.finish_batch_and_take_tail();
    assert_eq!(given_up.into_iter().collect::<Vec<_>>(), [4]);
    assert_eq!(pending.begin_batch(false), 0, "the armed block finds nothing to run");
    assert!(pending.can_run_inline());

    assert!(pending.begin_owed(), "an empty queue still begins a batch");
    assert!(!pending.can_run_inline(), "nothing runs inline beside an owed batch");
    pending.finish_batch();
    assert!(pending.can_run_inline());
}

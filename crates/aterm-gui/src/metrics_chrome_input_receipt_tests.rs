// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ChromeInputToken, INPUT_EDGE_RING, INPUT_SLICE_CAP_NS, Metrics, PendingChromeInputStamp,
    PresentClock, PresentInput,
};
use std::sync::atomic::Ordering;

fn arm(m: &Metrics, pending: &mut PendingChromeInputStamp, at: u64) -> ChromeInputToken {
    m.arm_chrome_input_at(pending, at, at)
        .expect("fresh receipt")
}

fn present(
    m: &Metrics,
    pending: &mut PendingChromeInputStamp,
    token: ChromeInputToken,
    now: u64,
    epoch: u64,
) -> usize {
    m.record_chrome_input_at(
        pending,
        token,
        PresentClock::at(false, now, epoch, 0, 0),
        PresentInput::default(),
    )
}

#[test]
fn only_the_latest_painted_receipt_books_a_contiguous_burst_once() {
    let m = Metrics::new();
    let mut pending = PendingChromeInputStamp::default();
    let first = arm(&m, &mut pending, 10);
    let latest = arm(&m, &mut pending, 20);
    assert_ne!(first, latest);
    assert_eq!(present(&m, &mut pending, first, 100, 0), 0, "stale paint");
    assert_eq!(pending.token(), Some(latest));
    assert_eq!(present(&m, &mut pending, latest, 110, 0), 2);
    assert_eq!(
        present(&m, &mut pending, latest, 120, 0),
        0,
        "repeat present"
    );
    assert_eq!(m.window.input.input.copy_out().n(), 2);
    assert_eq!(pending.token(), None);
}

#[test]
fn retired_foreign_and_restored_receipts_do_not_alias() {
    let m = Metrics::new();
    let mut a = PendingChromeInputStamp::default();
    let mut b = PendingChromeInputStamp::default();
    let old = arm(&m, &mut a, 10);
    let saved = a;
    a.retire();
    assert_eq!(present(&m, &mut a, old, 20, 0), 0);
    let new = arm(&m, &mut a, 30);
    let foreign = arm(&m, &mut b, 40);
    assert_ne!(old, new);
    assert_ne!(new, foreign);
    assert_eq!(present(&m, &mut a, old, 50, 0), 0, "retire/reopen ABA");
    assert_eq!(present(&m, &mut a, foreign, 50, 0), 0, "another window");
    assert_eq!(present(&m, &mut b, new, 50, 0), 0, "another window");
    a = saved;
    let restored = arm(&m, &mut a, 60);
    assert!([old, new, foreign].iter().all(|token| *token != restored));
    assert_eq!(
        present(&m, &mut a, new, 70, 0),
        0,
        "snapshot cannot rewind allocator"
    );
    assert_eq!(present(&m, &mut a, restored, 80, 0), 2);
    assert_eq!(present(&m, &mut b, foreign, 80, 0), 1);
}

#[test]
fn epoch_and_age_limits_apply_without_recycling_receipts() {
    let m = Metrics::new();
    let mut pending = PendingChromeInputStamp::default();
    let old = arm(&m, &mut pending, 10);
    m.epoch.store(11, Ordering::Relaxed);
    assert_eq!(present(&m, &mut pending, old, 100, 11), 0);
    let current = arm(&m, &mut pending, 11);
    assert_ne!(old, current);
    assert_eq!(present(&m, &mut pending, current, 100, 11), 1);
    let aged = arm(&m, &mut pending, 1);
    assert_eq!(
        present(&m, &mut pending, aged, INPUT_SLICE_CAP_NS + 2, 0),
        0
    );
    assert_eq!(m.window.input.input.copy_out().n(), 1);
}

#[test]
fn full_edge_ring_advances_receipts_and_aged_edges_make_room() {
    let m = Metrics::new();
    let mut pending = PendingChromeInputStamp::default();
    let mut token = arm(&m, &mut pending, 1);
    for at in 2..=INPUT_EDGE_RING as u64 {
        token = arm(&m, &mut pending, at);
    }
    let refused_edge = arm(&m, &mut pending, 100);
    assert_ne!(refused_edge, token);
    assert_eq!(m.window.input.edges_dropped.get(), 1);
    assert_eq!(present(&m, &mut pending, token, 1_000, 0), 0);
    assert_eq!(
        present(&m, &mut pending, refused_edge, 1_000, 0),
        INPUT_EDGE_RING
    );
    for at in 1..=INPUT_EDGE_RING as u64 {
        arm(&m, &mut pending, at);
    }
    let fresh_at = INPUT_SLICE_CAP_NS + INPUT_EDGE_RING as u64 + 1;
    let fresh = arm(&m, &mut pending, fresh_at);
    assert_eq!(present(&m, &mut pending, fresh, fresh_at + 7, 0), 1);
    assert_eq!(
        m.window.input.edges_dropped.get(),
        1,
        "aged edges do not refuse a key"
    );
}

#[test]
fn receipt_exhaustion_fails_closed_instead_of_wrapping() {
    let m = Metrics::new();
    let mut pending = PendingChromeInputStamp::default();
    m.chrome_generation.store(u64::MAX - 1, Ordering::Relaxed);
    let last = arm(&m, &mut pending, 10);
    assert_eq!(m.arm_chrome_input_at(&mut pending, 20, 20), None);
    assert_eq!(pending.token(), None);
    assert_eq!(present(&m, &mut pending, last, 30, 0), 0);
    assert_eq!(m.arm_chrome_input_at(&mut pending, 40, 40), None);
    assert_eq!(m.window.input.edges_dropped.get(), 2);
    assert_eq!(m.window.input.input.copy_out().n(), 0);
}

#[test]
fn capture_and_taint_retire_edges_before_any_later_clean_present() {
    let m = Metrics::new();
    for clock in [
        PresentClock::at(true, 100, 0, 0, 0),
        PresentClock::at(false, 100, 0, 101, 0),
        PresentClock::at(false, 100, 0, 0, 1),
    ] {
        let mut pending = PendingChromeInputStamp::default();
        let stale = arm(&m, &mut pending, 10);
        let current = arm(&m, &mut pending, 20);
        assert_eq!(
            m.record_chrome_input_at(&mut pending, stale, clock, PresentInput::default()),
            0
        );
        assert_eq!(
            pending.token(),
            None,
            "capture retires even if its receipt was stale"
        );
        assert_eq!(present(&m, &mut pending, current, 120, 0), 0);
    }
    assert_eq!(m.window.input.input.copy_out().n(), 0);
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn capacity_and_failed_start_never_admit_unowned_work() {
    let budget = Arc::new(Slots::new(2));
    let first = budget.try_take().unwrap();
    let second = budget.try_take().unwrap();
    assert!(
        Monitor::reserve_using(&budget, |_| panic!("full budget must not start work")).is_none()
    );
    drop(first);
    assert!(
        Monitor::reserve_using(&budget, |_| Err(std::io::Error::other("start refused"))).is_none()
    );
    assert_eq!(budget.used(), 1, "failed spawn returns its reservation");
    drop(second);
    assert_eq!(budget.used(), 0);
}

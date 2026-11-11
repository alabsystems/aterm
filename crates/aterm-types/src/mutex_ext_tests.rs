// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use std::sync::Arc;

#[test]
fn test_lock_or_recover_after_poison() {
    let m = Arc::new(Mutex::new(0));
    let m2 = Arc::clone(&m);
    let _ = std::thread::spawn(move || {
        let mut g = m2.lock().expect("lock for poison");
        *g = 7;
        panic!("intentional poison");
    })
    .join();
    let guard = m.lock_or_recover();
    assert_eq!(*guard, 7);
}

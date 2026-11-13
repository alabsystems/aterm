// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn original_gnu_has_zero_supplement_and_refuses_foreign_or_swapped_rights() {
    let original = RootGnu::new().unwrap();
    let copied = original
        .pipe
        .iter()
        .map(|fd| fd.try_clone().unwrap())
        .collect::<Vec<_>>();
    assert!(original.matches(&copied).unwrap());
    assert!(
        !original
            .matches(&[
                copied[1].try_clone().unwrap(),
                copied[0].try_clone().unwrap()
            ])
            .unwrap()
    );
    let foreign = RootGnu::new().unwrap();
    assert!(!original.matches(&foreign.pipe).unwrap());
    let mut queued: libc::c_int = -1;
    // SAFETY: FIONREAD only observes the held original GNU read end.
    assert_eq!(
        unsafe { libc::ioctl(original.pipe[0].as_raw_fd(), libc::FIONREAD, &mut queued) },
        0
    );
    assert_eq!(
        queued, 0,
        "one original implicit CPU never creates a supplemental GNU token"
    );
}

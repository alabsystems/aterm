// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{LicenceFence, owed};
use std::sync::Arc;

#[test]
fn a_window_owes_each_fence_of_the_session_it_presented_once() {
    let a = Arc::new(LicenceFence::default());
    let b = Arc::new(LicenceFence::default());
    let fence_of = |session| match session {
        1 => Some(a.clone()),
        2 => Some(b.clone()),
        _ => None,
    };
    let mut seen = None;
    a.fence(false);
    assert_eq!(owed(&mut seen, Some(1), fence_of), None);
    assert_eq!(owed(&mut seen, Some(1), fence_of), None);
    a.fence(true);
    assert_eq!(owed(&mut seen, Some(1), fence_of), Some(true));
    assert_eq!(
        owed(&mut seen, Some(1), fence_of),
        None,
        "a fence is owed once"
    );
    a.fence(true);
    a.fence(false);
    a.fence(true);
    assert_eq!(
        owed(&mut seen, Some(1), fence_of),
        Some(false),
        "one hand-off among the fences takes the hand off"
    );
    a.fence(true);
    assert_eq!(owed(&mut seen, None, fence_of), Some(true));
    b.fence(false);
    assert_eq!(owed(&mut seen, Some(2), fence_of), None);
    b.fence(true);
    assert_eq!(owed(&mut seen, Some(2), fence_of), Some(true));
    a.fence(false);
    assert_eq!(owed(&mut seen, Some(2), fence_of), None);
    let mut turned = None;
    assert_eq!(owed(&mut turned, Some(1), fence_of), None);
    a.fence(false);
    assert_eq!(owed(&mut turned, Some(2), fence_of), Some(false));
    assert_eq!(owed(&mut turned, Some(9), fence_of), None);
    assert!(turned.is_none());
}

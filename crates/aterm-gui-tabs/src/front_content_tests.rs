// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn lifecycle_is_forward_only_and_suspend_is_reversible() {
    let mut state = ViewLifecycle::Created;
    assert!(state.transition(ViewLifecycle::Mounted));
    assert!(state.is_active());
    assert!(state.transition(ViewLifecycle::Suspended));
    assert!(state.transition(ViewLifecycle::Mounted));
    assert!(state.transition(ViewLifecycle::Closing));
    assert!(!state.transition(ViewLifecycle::Mounted));
    assert!(state.transition(ViewLifecycle::Closed));
    assert!(state.is_terminal_state());
    assert!(!state.transition(ViewLifecycle::Created));
    let pair = encode_tab_view_pair_le(11, 22);
    assert_eq!(decode_tab_view_pair_le(pair), (11, 22));
}

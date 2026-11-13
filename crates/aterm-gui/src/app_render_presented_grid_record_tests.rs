// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{HostVisualState, SuccessfulPresentRoute};
use crate::WindowId;

/// A mixed frame after a terminal one clears the record: the leaves the terminal frame staged
/// are still there, but the mixed frame did not draw them.
#[test]
fn a_frame_off_the_terminal_route_clears_the_record() {
    let wid = WindowId(0);
    let mut app = crate::scripted_redraw_fixture::scripted_app();
    app.redraw_window(wid);
    assert_eq!(
        app.windows[&wid].presented_grids.len(),
        1,
        "the terminal frame drew its grid"
    );
    let plan = app.active_visible_leaf_plan(wid).expect("visible plan");
    app.finalize_successful_present(
        wid,
        crate::metrics::StartupPresentTiming::collapsed(std::time::Instant::now()),
        0,
        None,
        SuccessfulPresentRoute::Heterogeneous,
        crate::VisibleContentRoute::Heterogeneous,
        HostVisualState::default(),
        None,
        &plan,
    );
    assert!(
        app.windows[&wid].presented_grids.is_empty(),
        "a mixed frame names no terminal grid"
    );
}

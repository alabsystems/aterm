// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::SessionWindowRow;
use crate::{App, WindowId};

#[test]
fn rows_follow_the_tab_tree_and_the_front_window() {
    let mut app = App::headless_for_test();
    assert_eq!(
        app.session_window_rows(),
        vec![SessionWindowRow {
            local: 0,
            window: 0,
            active_tab: true,
            window_focused: true,
        }]
    );

    let sid1 = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(sid1));
    let rows = app.session_window_rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].local, rows[0].window, rows[0].active_tab),
        (0, 0, false)
    );
    assert_eq!(
        (rows[1].local, rows[1].window, rows[1].active_tab),
        (sid1, 0, true)
    );
    assert!(rows.iter().all(|r| r.window_focused));

    let sid2 = app.next_session_id;
    let wid2 = app.insert_logical_window(crate::stub_session(sid2), 24, 80);
    let rows = app.session_window_rows();
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .filter(|r| r.window == 0)
            .all(|r| !r.window_focused)
    );
    assert_eq!(
        rows[2],
        SessionWindowRow {
            local: sid2,
            window: wid2.0,
            active_tab: true,
            window_focused: true,
        }
    );
    assert!(rows.windows(2).all(|w| w[0].local < w[1].local));
    assert!(
        rows.iter()
            .all(|r| app.windows.contains_key(&WindowId(r.window)))
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::WindowId;
use crate::scripted_redraw_fixture::{feed, scripted_app};

#[test]
fn a_single_pane_with_no_rain_engine_keeps_the_completion_latch_observing() {
    let wid = WindowId(0);
    let mut app = scripted_app();
    app.config.serious_mode = Some(false);
    app.config.matrix_rain = Some(crate::app_config::MatrixRainConfig {
        enabled: Some(true),
        ..Default::default()
    });
    app.recompute_matrix_rain();
    app.redraw_window(wid);
    let front = app.front_terminal(wid).expect("front terminal");
    let (session, term) = (front.session, front.term.clone());
    assert!(
        app.windows[&wid].fx.rain().is_some(),
        "REACH: the rain engine was built"
    );
    assert_eq!(
        app.windows[&wid].fx.rain_latches().last_cmd,
        Some((session, 0))
    );
    assert!(app.set_serious_mode(true));
    feed(
        &app,
        b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07\x1b]133;D;1\x07",
    );
    app.redraw_window(wid);
    assert!(
        app.windows[&wid].fx.rain().is_none(),
        "REACH: no rain engine runs under Serious Mode"
    );
    let seq = crate::term_lock(&term).completed_command_seq();
    assert!(seq > 0, "fixture: the shell reported a completion");
    assert_eq!(
        app.windows[&wid].fx.rain_latches().last_cmd,
        Some((session, seq)),
        "the rain-less frame baselined the completion; before the fix the \
         engine the lift rebuilds read it as news"
    );
}

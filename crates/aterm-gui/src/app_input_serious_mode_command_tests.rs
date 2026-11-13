// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::App;

#[test]
fn unavailable_config_lane_leaves_runtime_and_service_unchanged_and_reports_failure() {
    let mut app = App::headless_for_test();
    let before = app.native_config_service.snapshot();
    let rows_before = app.messages.live_rows().count();
    assert!(!app.serious_mode_enabled());
    assert_eq!(app.config.serious_mode, None);
    assert!(
        app.proxy.is_none(),
        "negative control needs no event-loop host"
    );

    app.user_toggle_serious_mode();

    assert!(
        !app.serious_mode_enabled(),
        "the live suppression policy must not move before durability"
    );
    assert_eq!(app.config.serious_mode, None);
    let after = app.native_config_service.snapshot();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.text, before.text);
    assert!(
        app.native_config_pending.is_empty(),
        "a failed preflight must not leave a command that can apply later"
    );
    assert!(
        app.messages.live_rows().count() > rows_before,
        "Finder-launched users need visible failure feedback"
    );
}

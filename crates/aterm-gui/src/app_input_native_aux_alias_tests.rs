// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::native_settings_route_for_aux;
use crate::app_introspect::AuxTarget;
use crate::native_app::AppViewState;
use crate::native_settings::SettingsRoute;
use crate::{App, WindowId};

#[test]
fn update_spellings_navigate_native_settings_routes() {
    for (spelling, route) in [
        ("update", SettingsRoute::SoftwareUpdate),
        ("software-update", SettingsRoute::SoftwareUpdate),
    ] {
        let target = AuxTarget::parse(spelling).expect("compatibility spelling");
        assert_eq!(native_settings_route_for_aux(target), Some(route));

        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(route));
        let (_, view) = app
            .active_native_view(WindowId(0))
            .expect("native Settings view");
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(AppViewState::Settings(state)) if state.route == route
        ));
        assert!(app.windows[&WindowId(0)].overlay.is_none());
        assert!(app.close_settings_tabs());
        assert!(app.active_native_view(WindowId(0)).is_none());
    }
}

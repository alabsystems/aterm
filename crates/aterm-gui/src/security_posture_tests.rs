// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_verb_reads_lists_stages_and_refuses_junk() {
    assert_eq!(parse("", false), Ok(Asked::Read));
    assert_eq!(parse("list", false), Ok(Asked::List));
    assert_eq!(
        parse("denied-folders", true),
        Ok(Asked::Stage(Some("denied-folders".to_string())))
    );
    assert_eq!(parse("off", true), Ok(Asked::Stage(None)));
    assert!(
        parse("denied", false)
            .unwrap_err()
            .contains("development build")
    );
    assert!(parse("off", false).is_err());
    assert!(parse("nonsense", true).unwrap_err().contains("no posture"));
    assert!(parse("denied extra", true).is_err());
}

/// A STAGED POSTURE DRAWS THE SECURITY BLOCK ON A HEADLESS INSTANCE (s-24): a
/// headless instance publishes no posture, so the MACOS ACCESS block had never
/// been seen on a running aterm. Each catalog posture staged through the verb's
/// main-thread half draws the block with its actions on Security, runs no
/// probe, and `off` takes it away. The DEVELOPER TOOLS card is drawn too, and says
/// the checks are switched off only under the postures that switch them off (every
/// posture once said so beside a checked block). NEGATIVE CONTROLS: before any stage
/// the block is absent, and a windowed instance refuses the stage.
#[test]
fn a_staged_posture_draws_the_security_block_on_a_headless_instance() {
    use crate::app_control::InspectRequest;
    use crate::native_app::AppViewState;
    use crate::native_settings::{MACOS_ACCESS_OPEN_FDA, SettingsRoute};
    use aterm_types::app_inspection::InspectionProjection;
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    if let Some(window) = app.windows.get_mut(&wid) {
        window.cols = 160;
        window.rows = 60;
    }
    assert!(app.open_settings_tab(SettingsRoute::Security));
    let (_, view) = app.active_native_view(wid).expect("Settings is in front");
    let carries = |app: &crate::App| {
        matches!(
            app.native_runtime.view_state(view),
            Some(AppViewState::Settings(state)) if state.carries_macos_access()
        )
    };
    app.sync_settings_consent_posture();
    assert!(!carries(&app), "headless publishes no posture by itself");
    assert_eq!(
        app.control_security_posture(&Asked::Read).as_deref(),
        Ok("posture=off views=0")
    );
    for name in crate::security_fixtures::NAMES {
        let line = app
            .control_security_posture(&Asked::Stage(Some((*name).to_string())))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(line, format!("posture={name} views=1"));
        assert!(carries(&app), "{name}: the view carries the staged posture");
        let controls = app
            .inspect_app(InspectRequest::View {
                view,
                projection: InspectionProjection::Controls,
            })
            .expect("the Security page is inspected");
        assert!(
            controls
                .iter()
                .any(|line| line.contains(&format!("action={MACOS_ACCESS_OPEN_FDA}"))),
            "{name}: Open Privacy & Security is drawn: {controls:#?}"
        );
        let text = app
            .inspect_app(InspectRequest::View {
                view,
                projection: InspectionProjection::Text,
            })
            .expect("the Security page is inspected");
        assert!(
            text.iter()
                .any(|line| line.contains("key=\"settings/macos-access/heading\"")),
            "{name}: the block's heading is drawn"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("key=\"settings/macos-access/devtools/state\"")),
            "{name}: the DEVELOPER TOOLS card is drawn"
        );
        let says_off = text.iter().any(|line| {
            line.contains("key=\"settings/macos-access/devtools/")
                && line.contains("switched off in aterm.toml")
        });
        assert_eq!(
            says_off,
            crate::security_fixtures::CHECKS_OFF.contains(name),
            "{name}: the DEVELOPER TOOLS card says the checks are off only where they are"
        );
    }
    assert_eq!(
        app.control_security_posture(&Asked::Stage(None)).as_deref(),
        Ok("posture=off views=0")
    );
    assert!(!carries(&app), "off takes the block away");
    app.headless = false;
    assert!(
        app.control_security_posture(&Asked::Stage(Some("granted".to_string())))
            .unwrap_err()
            .contains("headless")
    );
    assert!(!carries(&app));
}

#[test]
fn a_launch_reads_the_host_only_where_there_is_one() {
    assert_eq!(
        SecurityPostureSource::at_launch(true, false),
        SecurityPostureSource::Host
    );
    for (macos, headless) in [(true, true), (false, false), (false, true)] {
        assert_eq!(
            SecurityPostureSource::at_launch(macos, headless),
            SecurityPostureSource::Absent
        );
    }
}

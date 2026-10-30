// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The yield at the host's routes that lay the glow's own under stream again
//! (`docs/design/PRISM-GLASS.md` §4, §15.2 1.9), driven on the trail-diff rig at the glass level:
//! a windowed composed capture re-projects the step's uncut glass and publishes the window's
//! words over it, and the heterogeneous route (a terminal beside a native view) projects it with
//! no word pass at all. Each must cut the glass off an app's red words as the frame's own
//! publish does. `LIVE` is still `Live::Off`, so each run raises the level with the rig's
//! `live glass`.

use crate::app_render::ComposedCursorFxClock;
use crate::trail_rig::TrailRig;
use crate::{App, WindowId};
use aterm_effects::cursor_glow::Live;
use aterm_render::{GlowQuad, GlowRamps};
use std::time::{Duration, Instant};

/// The font the rig draws, as the dump draws it.
const FIXTURE_FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
);

/// The config every run admits.
const CONFIG: &str = "trail_sounds = false\ncursor_trail_style = \"rainbow kitty\"\nmotion = \
                      \"full\"\ntheme = \"Default Light\"\n";

/// An app's red letter, typed and echoed: Default Light's own red, which keeps its bar over no
/// glass the yield keeps (§4 Measured).
const RED_W: &[u8] = b"\x1b[38;2;196;38;46mw\x1b[39m";

/// A rig on Default Light, gridded, before its first frame.
fn rig() -> TrailRig {
    let mut rig = TrailRig::new(std::path::Path::new(FIXTURE_FONT)).expect("the rig");
    let t0 = rig.t0();
    rig.configure(CONFIG, t0).expect("the config admits");
    rig.grid(8, 80, t0).expect("the grid");
    rig
}

/// The window's under stream as its frame left it, and the glow's own (uncut) one.
fn streams(app: &App, wid: WindowId) -> ((Vec<GlowQuad>, GlowRamps), (Vec<GlowQuad>, GlowRamps)) {
    let ws = &app.windows[&wid];
    let glow = &ws.fx.cursor().glow;
    (
        (
            ws.input_scratch.glow_under.clone(),
            ws.input_scratch.glow_under_ramps.clone(),
        ),
        (glow.under_quads().to_vec(), glow.under_ramps().clone()),
    )
}

/// The light arms the window's last frame reports, by name.
fn arms(app: &App, wid: WindowId) -> Vec<&'static str> {
    app.windows[&wid]
        .fx
        .light_arms_drawn()
        .iter()
        .map(|(arm, _)| arm)
        .collect()
}

/// A windowed composed capture (§15.2 1.9's "captures"): with an OS window the capture retains
/// the cursor's clock, re-projects the glow's own under stream into the window's frame and
/// publishes the window's words over it (`capture_words_sampled`'s branch that owns no cursor
/// step). That stream is the step's uncut glass, and the capture's publish must cut it off the
/// red words as the frame's own did. The negative control is the premise: the glow's own
/// stream, which the projection lays, still holds the glass the frame cut.
#[test]
fn a_windowed_composed_capture_yields_its_glass() {
    let mut rig = rig();
    let t0 = rig.t0();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    rig.split(t0).expect("the split");
    rig.focus_pane(0, t0)
        .expect("the first pane takes the focus");
    rig.live(Some(Live::Glass));
    rig.pty(b"\x1b[4;1H$ ", t0);
    let mut last = t0;
    for k in 1..=10u64 {
        rig.key("w", at(100 * k)).expect("the key");
        rig.pty(RED_W, at(100 * k + 4));
        for f in 0..6u64 {
            last = at(100 * k + 4 + 16 * f);
            rig.frame(last).expect("a presented frame");
        }
    }
    let (presented, uncut) = rig.with_app(last, |app, wid| {
        let names = arms(app, wid);
        assert!(
            names.contains(&"glass") && names.contains(&"yield"),
            "premise: the frame laid the glass and cut it off the red words: {names:?}"
        );
        streams(app, wid)
    });
    assert_ne!(presented, uncut, "premise: the frame's glass is cut");
    let captured = rig.with_app(last, |app, wid| {
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Retain { observed_at: last },
        )
        .expect("the composed capture grid");
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.fx.publish_words(&mut ws.input_scratch);
        streams(app, wid).0
    });
    assert_eq!(
        captured, presented,
        "the capture's publish cuts the re-projected glass as the frame's did"
    );
}

/// The heterogeneous route (a terminal pane beside Settings) steps the trail and projects the
/// glow's streams, and runs no word pass: the yield runs after its projection, so red letters
/// typed under the glass keep their colour. The negative control is the premise: the glow's
/// own stream still holds the glass the route cut.
#[test]
fn the_heterogeneous_route_yields_its_glass() {
    let mut rig = rig();
    let t0 = rig.t0();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let sid = rig.with_app(t0, |app, wid| {
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (sid, _) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert_eq!(
            app.focused_session_id(wid),
            Some(sid),
            "the terminal has the focus"
        );
        sid
    });
    rig.with_app(t0, |app, _| {
        let (tx, rx) = std::sync::mpsc::channel();
        app.theme_control(
            crate::control_theme::ThemeCtlOp::SessionSet {
                selector: "Default Light".to_string(),
            },
            Some(sid),
            t0,
            tx,
        );
        let answer = rx.try_recv().expect("answered at once");
        assert!(answer.is_ok(), "theme session set: {answer:?}");
    });
    rig.live(Some(Live::Glass));
    let echo = |rig: &mut TrailRig, bytes: &[u8], when: Instant| {
        rig.with_app(when, |app, _| {
            let term = app
                .pool
                .get(sid)
                .expect("the terminal's session")
                .term
                .clone();
            crate::term_lock(&term).process_at(
                bytes,
                aterm_core::terminal::ClockReading {
                    monotonic: when,
                    wall_ms: None,
                },
            );
            app.on_output_wake(sid, when);
        });
    };
    echo(&mut rig, b"\x1b[4;1H$ ", t0);
    let mut last = t0;
    for k in 1..=10u64 {
        rig.with_app(at(100 * k), |app, wid| {
            let fx = app
                .windows
                .get_mut(&wid)
                .expect("the window")
                .fx
                .cursor_mut();
            fx.glow.note_synthetic_typed(at(100 * k), 1);
            fx.trail.note_synthetic_typed(at(100 * k));
        });
        echo(&mut rig, RED_W, at(100 * k + 4));
        for f in 0..6u64 {
            last = at(100 * k + 4 + 16 * f);
            rig.with_app(last, |app, wid| {
                assert!(
                    app.prepare_heterogeneous_input_scratch_with_cursor_fx(
                        wid,
                        Some(ComposedCursorFxClock::Advance(last)),
                    )
                    .is_some(),
                    "the heterogeneous route draws"
                );
            });
        }
    }
    rig.with_app(last, |app, wid| {
        let names = arms(app, wid);
        assert!(
            names.contains(&"glass"),
            "premise: the route laid the glass: {names:?}"
        );
        let (drawn, uncut) = streams(app, wid);
        assert!(
            names.contains(&"yield") && drawn != uncut,
            "the route cuts its glass off the red letters: {names:?}"
        );
    });
}

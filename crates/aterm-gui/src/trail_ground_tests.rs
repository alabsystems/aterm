// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's light latch (`docs/design/PRISM-GLASS.md` §15.2 1.3), driven through the app's own
//! frame on the trail-diff rig: the latch runs on every frame the window draws, keyed by the
//! front session's configured scheme, which DECSCNM and OSC 10/11 never move and a reload moves
//! only when it installs colours; a reader outside the frame drops the light the moment that
//! scheme reads dark; a composed frame lights only its light-scheme panes. `LIVE` is still
//! `Live::Off`, so a lit frame carries the latched bit alone and draws main's frame.

use crate::app_render::terminal_configured_colors;
use crate::cursor_glow::{GlowConfig, Light};
use crate::light_fork_pin_tests::config_breaks;
use crate::trail_rig::TrailRig;
use crate::{App, WindowId};
use aterm_effects::host::light_latch::LatchOut;
use aterm_effects::rainbow_kitty::prism_glass::LIVE;
use std::time::{Duration, Instant};

/// The font the rig draws, as the dump draws it.
const FIXTURE_FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-render/tests/fixtures/jetbrains-mono.ttf"
);

/// The config every run admits, before its theme line.
const BASE: &str =
    "trail_sounds = false\ncursor_trail_style = \"rainbow kitty\"\nmotion = \"full\"\n";

/// The light a lit frame carries at [`LIVE`].
const LIT: Light = Light::admitted(true, LIVE);

/// A rig on `theme`, gridded and prompted, before its first frame.
fn rig_on(theme: &str) -> TrailRig {
    rig_with(theme, "")
}

/// [`rig_on`] with `extra` config lines.
fn rig_with(theme: &str, extra: &str) -> TrailRig {
    let mut rig = TrailRig::new(std::path::Path::new(FIXTURE_FONT)).expect("the rig");
    let t0 = rig.t0();
    rig.configure(&format!("{BASE}theme = {theme:?}\n{extra}"), t0)
        .expect("the config admits");
    rig.grid(8, 40, t0).expect("the grid");
    rig.pty(b"\x1b[4;1H$ ", t0);
    rig
}

/// What a frame left: the latch's verdict, the glow's config, the window's light read outside
/// the frame, and the focused terminal's live page `(fg, bg)` with DECSCNM folded.
struct After {
    out: LatchOut,
    cfg: GlowConfig,
    window_light: Light,
    page: (u32, u32),
}

fn after(app: &App, wid: WindowId) -> After {
    let ws = &app.windows[&wid];
    let mirror = app.front_terminal(wid).expect("a front terminal");
    let term = crate::term_lock(&mirror.term);
    let pack = |c: aterm_types::Rgb| aterm_render::rgb_to_u32([c.r, c.g, c.b]);
    let (fg, bg) = (
        pack(term.default_foreground()),
        pack(term.default_background()),
    );
    let page = if term.modes().reverse_video() {
        (bg, fg)
    } else {
        (fg, bg)
    };
    drop(term);
    After {
        out: ws.trail_ground.last(),
        cfg: *ws.fx.cursor().glow.last_cfg().expect("the glow ticked"),
        window_light: app.window_light(wid),
        page,
    }
}

/// Draw the rig's frame at `now` and read what it left.
fn frame(rig: &mut TrailRig, now: Instant) -> After {
    rig.frame(now).expect("a presented frame");
    rig.with_app(now, |app, wid| after(app, wid))
}

/// The session the window's focus holds.
fn focused(rig: &mut TrailRig, at: Instant) -> u64 {
    rig.with_app(at, |app, wid| {
        app.focused_session_id(wid).expect("a focused session")
    })
}

/// Set session `sid`'s own theme to `selector` at `at`, as `theme session set` does.
fn session_set(rig: &mut TrailRig, sid: u64, selector: &str, at: Instant) {
    rig.with_app(at, |app, _| {
        let (tx, rx) = std::sync::mpsc::channel();
        app.theme_control(
            crate::control_theme::ThemeCtlOp::SessionSet {
                selector: selector.to_string(),
            },
            Some(sid),
            at,
            tx,
        );
        let answer = rx.try_recv().expect("answered at once");
        assert!(answer.is_ok(), "theme session set {selector}: {answer:?}");
    });
}

/// A light scheme's frames are latched; a DECSCNM flash over it draws by main's dark rule for
/// its length, light off, and the latch comes back on the first frame after it.
#[test]
fn a_light_scheme_decscnm_flash_draws_mains_dark_rules_during_the_flash() {
    let mut rig = rig_on("Default Light");
    let t0 = rig.t0();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    let lit = frame(&mut rig, at(1));
    assert!(lit.out.on && lit.out.scheme_changed, "{:?}", lit.out);
    assert_eq!(lit.cfg.light, LIT);
    assert!(lit.cfg.light.on(), "the latched bit, at `LIVE` off");
    assert!(!lit.cfg.dark_theme);
    assert_eq!(config_breaks(&lit.cfg, lit.page), Vec::<&str>::new());
    assert_eq!(lit.window_light, LIT);

    rig.pty(b"\x1b[?5h", at(100));
    let flash = frame(&mut rig, at(116));
    assert_ne!(flash.page, lit.page, "the page swapped");
    assert!(
        !flash.out.on && flash.out.fell && !flash.out.scheme_changed,
        "{:?}",
        flash.out
    );
    assert_eq!(flash.cfg.light, Light::OFF);
    assert!(flash.cfg.dark_theme, "main reads the swapped page as dark");
    assert_eq!(
        config_breaks(&flash.cfg, flash.page),
        Vec::<&str>::new(),
        "the flash draws by main's rule"
    );
    assert_eq!(flash.window_light, Light::OFF);
    let held = frame(&mut rig, at(150));
    assert!(!held.out.on && !held.out.fell, "{:?}", held.out);

    rig.pty(b"\x1b[?5l", at(200));
    let back = frame(&mut rig, at(216));
    assert_eq!(back.page, lit.page);
    assert!(back.out.on && !back.out.scheme_changed, "{:?}", back.out);
    assert_eq!(back.cfg.light, LIT);
    assert_eq!(config_breaks(&back.cfg, back.page), Vec::<&str>::new());
}

/// The host's page (§15.2 1.7, D1): a translucent window keeps the latch's verdict but lights a
/// sheer light, which draws no glass at any level, so the frame draws main's paper arm, and the
/// light read outside the frame is the same; an opaque window's light is not sheer and draws the
/// glass at the glass level (negative control); a dark scheme's frame is [`Light::OFF`]
/// translucent too.
#[test]
fn a_translucent_window_lights_a_sheer_light() {
    use aterm_effects::cursor_glow::Live;
    for (extra, sheer) in [("", false), ("background_opacity = 0.8\n", true)] {
        let mut rig = rig_with("Default Light", extra);
        let t0 = rig.t0();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let page = rig.with_app(at(0), |app, _| app.page_facts());
        assert_eq!(page.opaque, !sheer, "{extra:?}");
        assert!(!page.wallpaper, "{extra:?}: no wallpaper configured");
        let lit = frame(&mut rig, at(1));
        assert!(lit.out.on, "{extra:?}: {:?}", lit.out);
        assert_eq!(lit.cfg.light, LIT.on_page(!sheer), "{extra:?}");
        assert_eq!(lit.cfg.light.sheer(), sheer, "{extra:?}");
        assert_eq!(lit.window_light, lit.cfg.light, "{extra:?}");
        assert_eq!(config_breaks(&lit.cfg, lit.page), Vec::<&str>::new());
        rig.with_app(at(2), |app, wid| {
            app.windows
                .get_mut(&wid)
                .expect("the window")
                .trail_ground
                .force_live(Some(Live::Glass));
        });
        let forced = frame(&mut rig, at(20));
        assert!(forced.cfg.light.on(), "{extra:?}");
        assert_eq!(forced.cfg.light.glass(), !sheer, "{extra:?}");

        let mut dark_rig = rig_with("Dracula", extra);
        let d0 = dark_rig.t0();
        let dark = frame(&mut dark_rig, d0 + Duration::from_millis(1));
        assert_eq!(dark.cfg.light, Light::OFF, "{extra:?}: a dark scheme");
        assert_eq!(dark.window_light, Light::OFF, "{extra:?}");
    }
}

/// A window that stops drawing keeps its last frame's latch, but a reader outside the frame
/// reads light off the moment its front session's scheme reads dark, without a frame; the next
/// frame drops the latch on the new key.
#[test]
fn an_unfocused_window_flipped_to_dark_reads_light_on_false() {
    let mut rig = rig_on("Default Light");
    let t0 = rig.t0();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    assert_eq!(frame(&mut rig, at(1)).window_light, LIT);
    rig.focus(false, at(50));
    let sid = focused(&mut rig, at(60));
    session_set(&mut rig, sid, "Dracula", at(60));
    let (latched, read, line) = rig.with_app(at(60), |app, wid| {
        (
            app.windows[&wid].trail_ground.last().on,
            app.window_light(wid),
            app.trail_light(),
        )
    });
    assert!(latched, "no frame ran: the latch keeps its last verdict");
    assert_eq!(read, Light::OFF, "the scheme reads dark now");
    let line = line.expect("a front window");
    for word in [
        "on=1",
        "window_light=0",
        "front_scheme_light=0",
        "raw=1",
        "live=off",
    ] {
        assert!(line.contains(word), "{word:?} in {line}");
    }
    let dark = frame(&mut rig, at(100));
    assert!(
        !dark.out.on && dark.out.scheme_changed && dark.out.fell,
        "{:?}",
        dark.out
    );
    assert_eq!(dark.cfg.light, Light::OFF);
    assert_eq!(dark.window_light, Light::OFF);
}

/// A reload moves the latch's key only when it installs a scheme: a rebuild reload (a new font
/// size) onto a dark theme changes the key and nothing admits; a reload back onto the light one
/// changes it and admits at once; a reload that keeps the theme changes nothing. The rollback
/// writer's pin is `app_config`'s `a_rollback_writes_the_scheme_back_and_raises_scheme_changed`.
#[test]
fn a_reload_moves_the_latch_key_only_when_it_installs_a_scheme() {
    let mut rig = rig_on("Default Light");
    let t0 = rig.t0();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    assert!(frame(&mut rig, at(1)).out.on);
    let steady = frame(&mut rig, at(40));
    assert!(steady.out.on && !steady.out.scheme_changed);

    rig.configure(
        &format!("{BASE}font_px = 17.0\ntheme = \"Dracula\"\n"),
        at(100),
    )
    .expect("the reload admits");
    let dark = frame(&mut rig, at(116));
    assert!(
        dark.out.scheme_changed && !dark.out.on && dark.out.fell,
        "{:?}",
        dark.out
    );
    assert_eq!(dark.cfg.light, Light::OFF);
    assert!(dark.cfg.dark_theme);

    rig.configure(&format!("{BASE}theme = \"Default Light\"\n"), at(200))
        .expect("the reload admits");
    let light = frame(&mut rig, at(216));
    assert!(light.out.scheme_changed && light.out.on, "{:?}", light.out);

    rig.configure(
        &format!("{BASE}cursor_trail_ms = 300\ntheme = \"Default Light\"\n"),
        at(300),
    )
    .expect("the reload admits");
    let kept = frame(&mut rig, at(316));
    assert!(kept.out.on && !kept.out.scheme_changed, "{:?}", kept.out);
}

/// A light instance's split: the focused light pane takes the frame's light, the pane beside it
/// wearing a dark theme takes none; with the dark theme focused, nothing is lit. (The rig's split
/// stands its new session on a stub terminal, so the dark theme goes on that one and the
/// instance's own session keeps the light one.)
#[test]
fn a_splits_dark_session_pane_is_off() {
    let mut rig = rig_on("Default Light");
    let t0 = rig.t0();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    rig.split(t0).expect("the split");
    rig.focus_pane(0, t0)
        .expect("the instance's pane takes the focus");
    let light_sid = focused(&mut rig, t0);
    let dark_sid = rig.with_app(t0, |app, _| {
        app.pool
            .ids()
            .into_iter()
            .find(|&sid| sid != light_sid)
            .expect("the pane beside")
    });
    session_set(&mut rig, dark_sid, "Dracula", t0);
    let lights = |rig: &mut TrailRig, now: Instant| {
        rig.frame(now).expect("a presented frame");
        rig.with_app(now, |app, wid| {
            let fx = &app.windows[&wid].fx;
            (
                fx.frame_light(now),
                fx.pane_word_light(now, light_sid),
                fx.pane_word_light(now, dark_sid),
            )
        })
    };
    assert_eq!(
        lights(&mut rig, at(1)),
        (LIT, LIT, Light::OFF),
        "the light pane takes the frame's light, the dark one none"
    );
    rig.focus_pane(1, at(100))
        .expect("the dark pane takes the focus");
    assert_eq!(
        lights(&mut rig, at(116)),
        (Light::OFF, Light::OFF, Light::OFF),
        "a dark focused session lights nothing"
    );
}

/// The lock-free read of a session's scheme is the pair its terminal holds under the lock, for
/// the instance's theme and a session's own, and neither moves under DECSCNM or OSC 10/11.
#[test]
fn the_lock_free_session_scheme_is_the_terminals_configured_pair() {
    let mut rig = rig_on("Default Light");
    let t0 = rig.t0();
    rig.split(t0).expect("the split");
    let own = focused(&mut rig, t0);
    session_set(&mut rig, own, "Dracula", t0);
    let pairs = |rig: &mut TrailRig| {
        rig.with_app(t0, |app, _| {
            app.pool
                .ids()
                .into_iter()
                .map(|sid| {
                    let session = app.pool.get(sid).expect("a live session");
                    let locked = terminal_configured_colors(&crate::term_lock(&session.term));
                    (sid, app.session_scheme(sid), locked)
                })
                .collect::<Vec<_>>()
        })
    };
    let before = pairs(&mut rig);
    assert_eq!(before.len(), 2);
    for &(sid, free, locked) in &before {
        assert_eq!(free, locked, "session {sid}");
    }
    let schemes: Vec<_> = before.iter().map(|&(_, free, _)| free).collect();
    assert_ne!(schemes[0], schemes[1], "the two sessions wear two schemes");
    rig.pty(
        b"\x1b[?5h\x1b]10;#123456\x1b\\\x1b]11;#fedcba\x1b\\",
        t0 + Duration::from_millis(1),
    );
    assert_eq!(
        pairs(&mut rig),
        before,
        "the page moved, the schemes did not"
    );
}

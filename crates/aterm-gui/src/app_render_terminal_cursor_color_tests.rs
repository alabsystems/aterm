// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn osc21_dynamic_cursor_tracks_osc10_in_main_frame_pixels_and_wake() {
    let mut term = Terminal::new(2, 4);
    term.set_default_cursor_color(Some(aterm_core::terminal::Rgb::new(0x50, 0xfa, 0x7b)));
    term.process(b"\x1b]21;cursor=\x07\x1b]10;#FE017F\x07\x1b[2 q");
    assert_eq!(
        term.cursor_color(),
        None,
        "OSC 21 empty cursor selects dynamic foreground fallback"
    );

    let (_, _, cursor) = terminal_frame_colors(&term);
    assert_eq!(cursor, 0x00FE_017F);
    assert_eq!(terminal_cursor_rgb(&term), [0xfe, 0x01, 0x7f]);

    let Some(mut renderer) = aterm_render::Renderer::from_system(16.0, Theme::default()) else {
        crate::logging::stderr_line!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = renderer.cell_size();
    let mut input = term.cell_frame(2, 4);
    input.cursor_color = cursor;
    let frame = renderer.render_input(&input);
    assert_eq!(
        frame
            .pixels
            .iter()
            .filter(|&&pixel| pixel == 0x00FE_017F)
            .count(),
        cw * ch,
        "the steady blank block cursor uses the live OSC 10 fallback, not the static theme cursor"
    );

    let mut app = App::headless_for_test();
    app.config.cursor_trail_color = None;
    let wid = WindowId(0);
    let fx = app
        .cursor_frame(
            wid,
            CursorFxInputs {
                default_fg: aterm_render::rgb_to_u32(terminal_blank_cell(&term).fg),
                rows: 2,
                cols: 4,
                live_cursor_rgb: terminal_cursor_rgb(&term),
                ..CursorFxInputs::sample_for_test(Instant::now())
            },
        )
        .expect("fixture window");
    assert_eq!(
        fx.trail_color, 0x00FE_017F,
        "the automatic single-pane wake follows the same effective cursor"
    );
}

/// The default cursor wears the cursor colour too: `draw_cursor` (and its GPU twin) paints
/// `cursor_fill_override` instead of `frame_cursor(input)` whenever the host sets one, so a
/// body effect must bloom from the cursor colour or it eats OSC 12 and `cursor_color`.
#[test]
fn osc12_recolours_the_effect_owned_block_cursor_not_only_the_plain_one() {
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    // The shipped default's block-owning half (the `pet`/`kitty` words add
    // companions, not a different fill).
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.trail_sounds = Some(false);
    // No pinned trail colour: the cursor colour is what the effects follow.
    app.config.cursor_trail_color = None;
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("fixture window").focused = true;

    let t0 = Instant::now();
    // A settled caret (no keystrokes) freezes the hue phase, so only the base can move
    // the fill between ticks.
    let mut fill_at = |rgb: [u8; 3], now| {
        app.cursor_frame(
            wid,
            CursorFxInputs {
                live_cursor_rgb: rgb,
                // A pinned colour (OSC 12 or a configured `cursor_color`).
                cursor_color_pinned: true,
                ..CursorFxInputs::sample_for_test(now)
            },
        )
        .expect("fixture window")
        .block_fill
        .filter(|owned| owned.owner == crate::cursor_glow::BlockFillOwner::Rainbow)
        .expect("the default cursor style owns the canonical block fill")
        .fill
    };
    let red = fill_at([0xFF, 0x00, 0x00], t0);
    let blue = fill_at([0x00, 0x00, 0xFF], t0 + Duration::from_millis(16));
    let theme = fill_at([0x50, 0xFA, 0x7B], t0 + Duration::from_millis(32));

    let chans = |c: u32| {
        (
            ((c >> 16) & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            (c & 0xff) as u8,
        )
    };
    let (rr, rg, rb) = chans(red);
    assert!(
        rr > 200 && rg < 110 && rb < 110,
        "OSC 12 #FF0000 must reach the effect-owned block, got {red:#08x}"
    );
    let (br, bg, bb) = chans(blue);
    assert!(
        bb > 200 && br < 110 && bg < 110,
        "OSC 12 #0000FF must reach the effect-owned block, got {blue:#08x}"
    );
    // The theme cursor arrives through the same channel when no OSC 12 is in force.
    let (tr, tg, tb) = chans(theme);
    assert!(
        tg > 200 && tg > tr && tg > tb,
        "the theme cursor colour must reach it too, got {theme:#08x}"
    );
    assert_ne!(
        red, blue,
        "two different cursor colours must not paint one identical block"
    );
}

/// The same law for the `phaser` emitter block, which blooms its beam hue
/// over the same `cursor_fill_override` seam. A settled emitter with a
/// frozen beam hue, so only the base can move the fill between ticks.
#[test]
fn osc12_recolours_the_phaser_emitter_block_too() {
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("phaser".into());
    app.config.trail_sounds = Some(false);
    app.config.cursor_trail_color = None;
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("fixture window").focused = true;

    let t0 = Instant::now();
    let mut fill_at = |rgb: [u8; 3], now| {
        app.cursor_frame(
            wid,
            CursorFxInputs {
                live_cursor_rgb: rgb,
                cursor_color_pinned: true,
                ..CursorFxInputs::sample_for_test(now)
            },
        )
        .expect("fixture window")
        .block_fill
        .filter(|owned| owned.owner == crate::cursor_glow::BlockFillOwner::Phaser)
        .expect("the phaser style owns the canonical block fill")
        .fill
    };
    let red = fill_at([0xFF, 0x00, 0x00], t0);
    let blue = fill_at([0x00, 0x00, 0xFF], t0 + Duration::from_millis(16));
    let theme = fill_at([0x50, 0xFA, 0x7B], t0 + Duration::from_millis(32));

    let chans = |c: u32| {
        (
            ((c >> 16) & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            (c & 0xff) as u8,
        )
    };
    let (rr, rg, rb) = chans(red);
    assert!(
        rr > 180 && rg < 130 && rb < 130,
        "OSC 12 #FF0000 must reach the phaser's emitter block, got {red:#08x}"
    );
    let (br, bg, bb) = chans(blue);
    assert!(
        bb > 180 && br < 130 && bg < 130,
        "OSC 12 #0000FF must reach the phaser's emitter block, got {blue:#08x}"
    );
    let (tr, tg, tb) = chans(theme);
    assert!(
        tg > 180 && tg > tr && tg > tb,
        "the theme cursor colour must reach it too, got {theme:#08x}"
    );
    assert_ne!(
        red, blue,
        "two different cursor colours must not paint one identical emitter"
    );
}

/// `trail status` reports who owns the block fill and from what base: the
/// owner named is the style that just painted the caret, and the base is
/// the cursor colour that tick was fed, against the real host seam.
#[test]
fn trail_status_names_the_style_that_owns_the_block_fill() {
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.trail_sounds = Some(false);
    app.config.cursor_trail_color = None;
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("fixture window").focused = true;
    app.frontmost_window = Some(wid);

    let t0 = Instant::now();
    let status_after_projection = |app: &mut App, style: &str, rgb: [u8; 3], now| {
        app.config.cursor_trail_style = Some(style.into());
        let candidate = app
            .cursor_frame(
                wid,
                CursorFxInputs {
                    live_cursor_rgb: rgb,
                    // Pinned iff the config has a `cursor_color`; the row's provenance
                    // must track it.
                    cursor_color_pinned: app.config.cursor_color.is_some(),
                    ..CursorFxInputs::sample_for_test(now)
                },
            )
            .expect("fixture window")
            .block_fill;
        let cursor_override = app.windows.get(&wid).and_then(|window| {
            (!window.focused && window.os_window.is_some()).then_some(CursorStyle::HollowBlock)
        });
        let projected_fill = window_cursor_fill(cursor_override, candidate.map(|owned| owned.fill));
        app.windows
            .get_mut(&wid)
            .expect("fixture window")
            .block_fill = presented_block_fill(cursor_override, projected_fill, candidate);
        app.trail_status().expect("a front window exists")
    };

    // The default, unpinned: the rainbow block owns the caret and, with no `cursor_color`
    // configured, builds from white (`base=none from=white`).
    let line = status_after_projection(&mut app, "rainbow kitty", [0xFF, 0x00, 0x00], t0);
    assert!(
        line.contains(" block_fill=rainbow")
            && line.contains(" block_fill_base=none")
            && line.contains(" block_fill_base_from=white"),
        "an unpinned caret builds from white and the row must say so: {line}"
    );
    let painted = line
        .split(" block_fill_rgb=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        .expect("the row carries the painted colour");
    assert!(
        (painted >> 16) & 0xff > 200 && (painted >> 8) & 0xff > 200 && painted & 0xff > 200,
        "the unpinned caret at rest is white, not the theme's cursor colour: \
         got {painted:#08x}"
    );

    // A pinned colour still reaches the caret: the user wrote one, so the
    // block blooms from it.
    app.config.cursor_color = Some("#ff0000".into());
    let line = status_after_projection(&mut app, "rainbow kitty", [0xFF, 0x00, 0x00], t0);
    assert!(
        line.contains(" block_fill=rainbow")
            && line.contains(" block_fill_base=ff0000")
            && line.contains(" block_fill_base_from=cursor_color"),
        "a pinned cursor colour reaches the caret and the row must say so: {line}"
    );
    // The painted colour (`block_fill_rgb`) tracks the base.
    let painted = line
        .split(" block_fill_rgb=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        .expect("the row carries the painted colour");
    assert!(
        (painted >> 16) & 0xff > 200 && (painted >> 8) & 0xff < 130 && painted & 0xff < 130,
        "the painted caret must be the red it was told to bloom from, got {painted:#08x}"
    );

    // The phaser: the same provenance from a different cursor colour.
    let line = status_after_projection(
        &mut app,
        "phaser",
        [0x00, 0x00, 0xFF],
        t0 + Duration::from_millis(16),
    );
    assert!(
        line.contains(" block_fill=phaser")
            && line.contains(" block_fill_base=0000ff")
            && line.contains(" block_fill_base_from=cursor_color"),
        "the phaser owns the caret and blooms from the cursor colour: {line}"
    );

    // Fire owns its body colour by design (warm metal); the row says
    // `style_identity` so a deliberate owner reads differently from a leak.
    let line = status_after_projection(
        &mut app,
        "fire",
        [0x00, 0x00, 0xFF],
        t0 + Duration::from_millis(32),
    );
    assert!(
        line.contains(" block_fill=forge")
            && line.contains(" block_fill_base=none")
            && line.contains(" block_fill_base_from=style_identity"),
        "fire owns its body colour deliberately: {line}"
    );

    // An unfocused window paints no body: the caret takes the terminal's own cursor
    // colour, and the row stops naming last frame's owner.
    app.windows.get_mut(&wid).expect("fixture window").focused = false;
    let line = status_after_projection(
        &mut app,
        "rainbow kitty",
        [0xFF, 0x00, 0x00],
        t0 + Duration::from_millis(48),
    );
    assert!(
        line.contains(" block_fill=none")
            && line.contains(" block_fill_base=none")
            && line.contains(" block_fill_base_from=none"),
        "nobody owns an unfocused caret: {line}"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_messages::{GLYPH_COL, Glyph, Intent, Message, Severity, tags};

use crate::{App, WindowId, term_lock};

/// Fill the scratch from the engine, splice the band and partition the
/// frame, as the compose does on the way to every present and capture.
fn compose(app: &mut App, wid: WindowId, theme: aterm_render::Theme) {
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (ws.rows as usize, ws.cols as usize)
    };
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        let mut term = term_lock(&terminal);
        term.cell_frame_into(&mut ws.input_scratch, rows, cols);
    }
    app.splice_message_band(wid, theme);
    app.set_scroll_band(wid);
}

/// The characters of composed row `row`, one per cell.
fn row_chars(app: &App, wid: WindowId, row: usize) -> Vec<char> {
    app.windows[&wid].input_scratch.cells[row]
        .iter()
        .map(|c| c.ch)
        .collect()
}

/// `0xRRGGBB`, as the CPU frame packs a pixel.
fn packed(rgb: [u8; 3]) -> u32 {
    (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])
}

/// Row 0 of the capture holds the row's title whole at 80 columns with its
/// capsule where the width law placed it, the grid starts below the chrome
/// (`grid_top_row == chrome_rows`), and the terminal's first line follows
/// the band intact.
#[test]
fn introspection_capture_paints_the_message_band() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"$ echo hello\r\nhello\r\n");
    app.post_message(
        Message::new(tags::CRASH, Severity::Error, "aterm crashed last time")
            .line("/var/log/aterm/crash-signal-1-1.log.seen")
            .action(Intent::OpenPath {
                path: "/var/log/aterm/crash-signal-1-1.log.seen".into(),
            }),
    );
    assert_eq!(app.message_band_rows, 1);
    assert_eq!(app.windows[&wid].cols, 80, "the fixture's width");
    let theme = aterm_render::Theme::default();
    compose(&mut app, wid, theme);
    let row0 = row_chars(&app, wid, 0);
    let text: String = row0.iter().collect();
    assert!(
        text.contains("aterm crashed last time"),
        "the title, whole: {text:?}"
    );
    let layout = app.band_presentation(80).rows[0].clone();
    assert_eq!(layout.full_title, layout.title.1, "nothing elided at 80");
    let open_log = layout
        .capsules
        .iter()
        .find(|c| !c.action.is_details())
        .expect("the Open log capsule");
    let painted: String = row0[open_log.col + 1..open_log.col + open_log.width - 1]
        .iter()
        .collect();
    assert_eq!(
        painted, open_log.text,
        "the capsule is painted where it is laid out"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.grid_top_row,
        usize::from(app.chrome_rows(wid)),
        "the grid starts below the chrome"
    );
    assert_eq!(app.chrome_rows(wid), 1);
    let row1: String = row_chars(&app, wid, 1).iter().collect();
    assert!(
        row1.starts_with("$ echo hello"),
        "the terminal's first line follows the band: {row1:?}"
    );
    assert_eq!(app.windows[&wid].input_scratch.cells.len(), 25);
}

/// The band's cells are a pure function of the center's state and the
/// prepared instant: the same instant composes the same cells, and another
/// instant moves only the meter's surface, the spinner and the time slots.
#[test]
fn a_capture_at_an_injected_instant_is_deterministic() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let theme = aterm_render::Theme::default();
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    let epoch = app
        .messages
        .on_glass()
        .find_map(|l| l.motion_since)
        .expect("the row is on the glass");
    let at = epoch + std::time::Duration::from_millis(1500);
    let shoot = |app: &mut App, at: std::time::Instant| {
        app.prepare_band_motion_with(wid, at, aterm_messages::Look::MOVING);
        compose(app, wid, theme);
        app.windows[&wid].input_scratch.cells[0].clone()
    };
    let first = shoot(&mut app, at);
    let again = shoot(&mut app, at);
    assert_eq!(first, again, "the same instant is the same frame");
    let later = shoot(&mut app, at + std::time::Duration::from_millis(400));
    assert_ne!(first, later, "another frame instant moves the comet");
    // The meter spans the row, so another instant may move any cell's
    // ground; characters change only in the glyph cell and the time slots.
    let layout = app.band_presentation(80).rows[0].clone();
    assert_eq!(layout.track, Some((0, 80)), "the comet's track is the row");
    let words_may_move = |x: usize| {
        x == aterm_messages::GLYPH_COL
            || layout
                .elapsed
                .is_some_and(|c| (c..c + layout.elapsed_width()).contains(&x))
            || layout
                .eta
                .is_some_and(|c| (c..c + layout.eta_width()).contains(&x))
    };
    for (x, (a, b)) in first.iter().zip(&later).enumerate() {
        if a.ch != b.ch {
            assert!(
                words_may_move(x),
                "col {x}: a word moved outside the time slots"
            );
        }
    }
}

/// Every admitted glyph (and the `!` fallback) paints ink in the glyph cell
/// through the headless CPU renderer; skipped with no system monospace font.
///
/// Two rows are posted so the probed row is not the band's last, whose seam
/// underline would read as ink; the blank cell left of the glyph is the
/// control. No retry: `Renderer::render_input` resolves a fresh renderer's
/// provisional `.notdef`, and a retry would hide a regression there.
#[test]
fn every_admitted_glyph_paints_a_non_blank_cell_headless() {
    let theme = aterm_render::Theme::default();
    let Some(mut cpu) = aterm_render::Renderer::from_system(20.0, theme) else {
        aterm_spec::host::could_not_run("no system monospace font here");
    };
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (cw, ch) = cpu.cell_size();
    let (x_pad, y0) = (cpu.pad(), cpu.grid_top());
    let mut glyphs: Vec<char> = Glyph::ALLOWED.to_vec();
    glyphs.push(Glyph::or_fallback('\u{2699}').ch());
    // No spinner frames: a moving comet keeps the row's own glyph, and each
    // admitted glyph is drawn as a band icon through the row's chrome raster.
    for ch_ in glyphs {
        app.clear_messages_for_test();
        let admitted = Glyph::new(ch_).expect("admitted");
        app.post_message(Message::new(tags::SYSTEM, Severity::Info, "glyph").glyph(admitted));
        app.post_message(Message::new(tags::SYSTEM, Severity::Info, "the seam row"));
        assert_eq!(app.message_band_rows, 2);
        compose(&mut app, wid, theme);
        let scratch = &app.windows[&wid].input_scratch;
        let cells = &scratch.cells;
        let cell = cells[0][GLYPH_COL];
        assert_eq!(cell.ch, ch_);
        assert!(cell.text_presentation, "{ch_:?}");
        // Drawn as underlines or as rails, as this cell height allows.
        let (divider, seam) = (
            crate::message_band::closing_line(scratch, 0),
            crate::message_band::closing_line(scratch, 1),
        );
        assert!(
            divider.is_some() && seam.is_some() && divider != seam,
            "the seam is on row 1, not on the probed row (row 0 carries the divider): \
             {divider:?} over {seam:?}"
        );
        assert_eq!(
            cells[0][0].bg, cell.bg,
            "the control cell shares the glyph's ground"
        );
        let ground = packed(cell.bg);
        let frame = cpu.render_input(&app.windows[&wid].input_scratch);
        assert!(
            frame.width >= x_pad + (GLYPH_COL + 1) * cw && frame.height >= y0 + ch,
            "{ch_:?}"
        );
        // The row's divider is a rule, not ink.
        let colors = crate::chrome_band::band_colors(theme);
        let divider = packed(crate::chrome_band::mix3(
            colors.bar_bg,
            colors.label,
            crate::message_band::DIVIDER_ALPHA,
        ));
        let ink = |px: std::ops::Range<usize>| {
            (y0..y0 + ch)
                .flat_map(|y| px.clone().map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let p = frame.pixels[y * frame.width + x];
                    p != ground && p != divider
                })
                .count()
        };
        // The icon may spill into the blank cells beside it, never as far as
        // the middle of either.
        assert_eq!(
            ink(x_pad..x_pad + cw / 2),
            0,
            "{ch_:?}: control — the blank band cell's far half has no ink"
        );
        let g0 = x_pad + GLYPH_COL * cw;
        assert!(ink(g0..g0 + cw) > 0, "{ch_:?} painted a blank glyph cell");
    }
}

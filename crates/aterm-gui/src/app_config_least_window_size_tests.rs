// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::native_appearance::{AppearancePreferences, current_preferences, install_preferences};
use crate::native_settings::{SettingsRoute, least_view};
use aterm_window::dpi::PhysicalSize;

fn at_text_scale(scale: f32) {
    install_preferences(AppearancePreferences {
        text_scale: scale,
        ..current_preferences()
    });
}

/// Window `wid` asked for one pixel, past the live-resize throttle: it stands at its least.
fn shrink(app: &mut crate::App, wid: crate::WindowId) {
    app.windows
        .get_mut(&wid)
        .expect("the window")
        .last_resize_at = None;
    let _ = app.resize_window_to_px(wid, PhysicalSize::new(1, 1));
}

/// The view window `wid` gives a native tab, as wide and as tall.
fn view(app: &crate::App, wid: crate::WindowId) -> (f32, f32) {
    let view = app.native_ui_full_viewport(wid).expect("the window's view");
    (view.width, view.height)
}

/// A WINDOW HOLDING SETTINGS IS NEVER SMALLER THAN SETTINGS' LEAST VIEW, AT EVERY TEXT SCALE:
/// at 1×, 1.25×, 1.5×, 1.75× and 2× text, a headless window holding Settings, asked for one
/// pixel as a window manager is asked for less than a window's least, stands at its least,
/// whose view holds the least view Settings takes at that scale; a terminal's window keeps
/// the 164x98 least at every scale. The least was a fixed 164x98 window: 2× text had a 164x96
/// view, where a control and a line of its name shared no screen.
#[test]
fn a_window_holding_settings_gives_it_its_least_view_at_every_text_scale() {
    for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0] {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        at_text_scale(1.0);
        let terminal = app.least_window_size(wid);
        at_text_scale(scale);
        assert_eq!(
            app.least_window_size(wid),
            terminal,
            "{scale}x: a terminal's window keeps its least"
        );
        assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Home));
        shrink(&mut app, wid);
        let (width, height) = view(&app, wid);
        let (least_w, least_h) = least_view(scale);
        assert!(
            width + 0.01 >= least_w && height + 0.01 >= least_h,
            "{scale}x: a {width}x{height} view under Settings' least {least_w}x{least_h}"
        );
    }
}

/// A TEXT SCALE THAT GROWS GROWS A WINDOW HOLDING SETTINGS TO ITS NEW LEAST: a window at its
/// least for 1× text holds Settings' least view for 2× once the text goes to 2×, and keeps it
/// when asked for less. It kept the view it had, Settings squeezed into the 1× least.
#[test]
fn a_text_scale_that_grows_grows_a_window_holding_settings_to_its_new_least() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    at_text_scale(1.0);
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Home));
    app.follow_least_window_scale();
    shrink(&mut app, wid);
    let before = view(&app, wid);
    let (least_w, least_h) = least_view(2.0);
    assert!(
        before.0 < least_w || before.1 < least_h,
        "precondition: the 1x least {before:?} is under the 2x least view"
    );
    at_text_scale(2.0);
    app.windows
        .get_mut(&wid)
        .expect("the window")
        .last_resize_at = None;
    app.follow_least_window_scale();
    let (width, height) = view(&app, wid);
    assert!(
        width + 0.01 >= least_w && height + 0.01 >= least_h,
        "the window grew to {width}x{height}, under the 2x least view {least_w}x{least_h}"
    );
    shrink(&mut app, wid);
    let (width, height) = view(&app, wid);
    assert!(
        width + 0.01 >= least_w && height + 0.01 >= least_h,
        "asked for less, it keeps {width}x{height} over {least_w}x{least_h}"
    );
}

/// BOTH FORMS OF `resize` HOLD A WINDOW HOLDING SETTINGS TO ITS LEAST, AND SAY SO, AT 1x AND
/// 2x TEXT: the grid form asked for a grid under Settings' least view (on both axes, on its
/// rows alone, on its columns alone) gives the window the least on each held axis and what
/// was asked on the other, and answers with the grid given, `held=least` and the grid asked;
/// a grid over the least is given as asked; the pixel form holds and answers the same in
/// pixels. The grid form resized the grid and the window as asked (a 164x96 Settings view at
/// 1x, 1564x180 at 2x) and answered a bare `OK`, until a page opening grew it back. The grids
/// asked are counted in the window's own cells from the least view, since a face's cell size
/// moves the least grid: the 9x20 headless cells of a Linux host's face hold the 1x least
/// view's 208pt in 11 rows, so a fixed 12-row grid asked as under it was given as asked.
#[test]
fn both_forms_of_resize_hold_a_window_holding_settings_to_its_least_and_say_so() {
    use crate::input::{InputEvent, InputOutcome, Source, WindowResize};
    for scale in [1.0_f32, 2.0] {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        at_text_scale(scale);
        assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Home));
        app.follow_least_window_scale();
        let (least_w, least_h) = least_view(scale);
        let holds_least = |app: &crate::App, what: &str| {
            let (width, height) = view(app, wid);
            assert!(
                width + 0.01 >= least_w && height + 0.01 >= least_h,
                "{scale}x {what}: a {width}x{height} view under Settings' least \
                 {least_w}x{least_h}"
            );
        };
        let grid = |app: &crate::App| (app.windows[&wid].rows, app.windows[&wid].cols);
        // The fewest rows and columns whose view holds Settings' least view, in this window's
        // cells: a grid's view is its cells and pads ([`crate::App::native_ui_full_viewport`]).
        let (cw, ch) = app.win_cell_size(wid);
        let (pad, device) = (app.win_pad(wid), app.windows[&wid].scale as f32);
        let fewest = |least: f32, cell: usize, pads: usize| -> u16 {
            (1..=u16::MAX)
                .find(|&n| (usize::from(n) * cell + pads) as f32 / device + 0.01 >= least)
                .expect("some grid holds the least view")
        };
        let least = (fewest(least_h, ch, pad), fewest(least_w, cw, 2 * pad));
        assert!(
            least.0 > 1 && least.1 > 1,
            "precondition: a {cw}x{ch} cell is under the {least_w}x{least_h} least view"
        );
        let (under, roomy) = ((least.0 - 1, least.1 - 1), (2 * least.0, 2 * least.1));
        for asked in [under, (under.0, roomy.1), (roomy.0, under.1)] {
            let reply = app.input(
                wid,
                InputEvent::Resize {
                    rows: asked.0,
                    cols: asked.1,
                    echo_to_window: true,
                },
                Source::Controller,
            );
            holds_least(&app, &format!("resize {} {}", asked.0, asked.1));
            let given = grid(&app);
            assert_eq!(
                reply,
                InputOutcome::Resized(WindowResize::Grid { asked, given }),
                "{scale}x: the reply names the grid given"
            );
            assert_eq!(
                given,
                (asked.0.max(least.0), asked.1.max(least.1)),
                "{scale}x: {asked:?}, under the least {least:?} on an axis, is held to it there \
                 and given as asked on the other"
            );
        }
        let roomy = (grid(&app).0 + 3, grid(&app).1 + 5);
        let reply = app.input(
            wid,
            InputEvent::Resize {
                rows: roomy.0,
                cols: roomy.1,
                echo_to_window: true,
            },
            Source::Controller,
        );
        assert_eq!(
            reply,
            InputOutcome::Resized(WindowResize::Grid {
                asked: roomy,
                given: roomy
            }),
            "{scale}x: a grid over the least is given as asked"
        );
        assert_eq!(grid(&app), roomy);
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .last_resize_at = None;
        let reply = app.input(
            wid,
            InputEvent::ResizeWindowPx {
                width: 200,
                height: 150,
            },
            Source::Controller,
        );
        let least = app.least_window_size(wid);
        assert_eq!(
            reply,
            InputOutcome::Resized(WindowResize::Px {
                asked: (200, 150),
                given: (least.width.max(200), least.height.max(150)),
            }),
            "{scale}x: the pixel form names the size it gave"
        );
        holds_least(&app, "resize px 200 150");
    }
    at_text_scale(1.0);
}

/// A FONT CHANGE KEEPS A HEADLESS WINDOW'S SIZE, AS A PLATFORM WINDOW KEEPS ITS INNER SIZE:
/// the window re-grids inside the pixels it held, so a page lays out at the size a person's
/// window keeps through the change, and a window holding Settings still holds its least. It
/// kept its grid instead, its frame growing with the font, and Settings laid out in a window
/// no person has (a Text & Fonts press's status bar stood mid-page in it).
#[test]
fn a_font_change_keeps_a_headless_windows_size() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let frame = |app: &crate::App| {
        let ws = &app.windows[&wid];
        app.window_frame_px_for(wid, ws.rows, ws.cols)
    };
    let grid = |app: &crate::App| (app.windows[&wid].rows, app.windows[&wid].cols);
    let (before, grid_before) = (frame(&app), grid(&app));
    let base = app.font_size;
    app.set_font_size(base + 8.0);
    let (cw, ch) = app.win_cell_size(wid);
    let after = frame(&app);
    assert!(
        grid(&app).0 < grid_before.0 && grid(&app).1 < grid_before.1,
        "bigger cells, fewer of them: {grid_before:?} -> {:?}",
        grid(&app)
    );
    assert!(
        after.width <= before.width
            && before.width - after.width < cw as u32
            && after.height <= before.height
            && before.height - after.height < ch as u32,
        "the window keeps its {before:?} (its frame {after:?} the whole cells inside it)"
    );
    app.set_font_size(base);
    assert_eq!(grid(&app), grid_before, "zoomed back into the same pixels");
    assert_eq!(frame(&app), before);

    at_text_scale(1.0);
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Home));
    shrink(&mut app, wid);
    app.set_font_size(base + 8.0);
    let (width, height) = view(&app, wid);
    let (least_w, least_h) = least_view(1.0);
    assert!(
        width + 0.01 >= least_w && height + 0.01 >= least_h,
        "after the font change, a {width}x{height} view under Settings' least \
         {least_w}x{least_h}"
    );
}

/// A terminal's window takes the grid it is asked for: the least that holds Settings holds
/// nothing else.
#[test]
fn a_terminals_window_takes_the_grid_it_is_asked_for() {
    use crate::input::{InputEvent, InputOutcome, Source, WindowResize};
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    at_text_scale(2.0);
    let reply = app.input(
        wid,
        InputEvent::Resize {
            rows: 6,
            cols: 20,
            echo_to_window: true,
        },
        Source::Controller,
    );
    at_text_scale(1.0);
    assert_eq!(
        reply,
        InputOutcome::Resized(WindowResize::Grid {
            asked: (6, 20),
            given: (6, 20)
        })
    );
    assert_eq!((app.windows[&wid].rows, app.windows[&wid].cols), (6, 20));
}

#[test]
fn min_size_sits_on_the_whole_cell_frame_lattice() {
    let app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let min = app.least_window_size(wid);
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let base_h =
        app.win_head(wid) + app.win_pad_top(wid) + pad + usize::from(app.tab_strip_rows) * ch;

    assert_eq!(
        (min.width as usize - 2 * pad) % cw,
        0,
        "min width must be an exact whole-cell frame"
    );
    assert_eq!(
        (min.height as usize - base_h) % ch,
        0,
        "min height must be an exact whole-cell frame"
    );
    assert!(min.width >= 164, "min width keeps the 164 logical floor");
    assert!(min.height >= 98, "min height keeps the 98 logical floor");
    assert!((min.width as usize) < 164 + cw + 2 * pad + cw);
    assert!((min.height as usize) < 98 + ch + base_h + ch);
}

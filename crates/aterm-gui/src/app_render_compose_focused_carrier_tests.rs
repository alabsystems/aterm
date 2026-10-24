// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `composed_focus_scratch` damage-scoped retention and host-write disown in split views.

use crate::{App, WindowId, term_lock};
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;
const DT: Duration = Duration::from_millis(16);
/// The fixture writes rows 0..=5 and parks the caret on row 6.
const CARET_ROW: usize = 6;
/// A row the caret never touches: the retention witness.
const QUIET_ROW: usize = 2;

/// Creates a focused 2-pane split window with rows `0..CARET_ROW` populated and the caret on `CARET_ROW`.
fn split_app() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.split_active_stub_tab(wid);
    app.windows.get_mut(&wid).expect("window").focused = true;
    app.config.cursor_trail = Some(true);
    let bg = aterm_render::Theme::default().bg;
    for s in [0u64, sid] {
        let term = app.pool.get(s).expect("pane session").term.clone();
        let mut t = term_lock(&term);
        t.set_default_background(aterm_core::terminal::Rgb {
            r: (bg >> 16) as u8,
            g: (bg >> 8) as u8,
            b: bg as u8,
        });
        for i in 0..CARET_ROW {
            t.process(format!("row{i} abcdefgh\r\n").as_bytes());
        }
        t.process(b"tail\r");
    }
    (app, wid)
}

fn compose(app: &mut App, wid: WindowId, now: Instant) -> bool {
    app.redraw_compose(wid, ROWS, COLS, false, false, None, 0, now)
        .is_some()
}

#[test]
fn subtab_title_changes_keep_undamaged_content_retainable() {
    let (mut app, wid) = split_app();
    let now = Instant::now();
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let focus = focus_rect(&app, wid).session;
    let mut previous = 0;
    for (step, title) in ["Build", "Review", "Tests"].iter().enumerate() {
        assert_eq!(
            app.pool
                .get(focus)
                .unwrap()
                .ctx
                .meta
                .lock()
                .unwrap()
                .set("title", Some((*title).into())),
            Some(true)
        );
        let fp = app.refresh_pane_headers(wid, &plan);
        assert_ne!(fp, previous, "title-only changes invalidate the frame");
        previous = fp;
        assert!(
            app.redraw_compose(
                wid,
                ROWS,
                COLS,
                false,
                false,
                None,
                fp,
                now + DT * step as u32
            )
            .is_some()
        );
        let ws = &app.windows[&wid];
        assert!(ws.input_scratch.composed_fill_intact());
        let header = plan.leaf(plan.focused).unwrap().header.unwrap();
        let text: String = ws.input_scratch.cells[header.origin.y as usize]
            [header.origin.x as usize..]
            .iter()
            .map(|cell| cell.ch)
            .collect();
        assert!(text.contains(title), "the latest title is painted: {text}");
        assert_eq!(composed_char(&app, wid, QUIET_ROW, 0), 'r');
        if step > 0 {
            assert!(
                ws.composed_retain.retained_rows() > 0,
                "header chrome must not disable terminal row retention"
            );
        }
    }
}

/// The focused leaf's rectangle in the presented composite.
fn focus_rect(app: &App, wid: WindowId) -> crate::pane::PaneRect {
    let ws = app.windows.get(&wid).expect("window");
    let tree = &ws.layouts[ws.tabs.active];
    let focus = tree.focus();
    *tree
        .compute_layout(ws.rows, ws.cols)
        .iter()
        .find(|r| r.session == focus)
        .expect("the focused leaf is laid out")
}

/// The background leaf's session id.
fn other_session(app: &App, wid: WindowId) -> u64 {
    let ws = app.windows.get(&wid).expect("window");
    let tree = &ws.layouts[ws.tabs.active];
    let focus = tree.focus();
    tree.compute_layout(ws.rows, ws.cols)
        .iter()
        .map(|r| r.session)
        .find(|s| *s != focus)
        .expect("the background leaf is laid out")
}

fn feed(app: &App, sid: u64, bytes: &[u8]) {
    let term = app.pool.get(sid).expect("pane session").term.clone();
    term_lock(&term).process(bytes);
}

/// The character the presented composite carries at a focused-pane-local cell.
fn composed_char(app: &App, wid: WindowId, pane_row: usize, pane_col: usize) -> char {
    let r = focus_rect(app, wid);
    let ws = app.windows.get(&wid).expect("window");
    ws.input_scratch
        .cells
        .get(usize::from(r.row_off) + pane_row)
        .and_then(|row| row.get(usize::from(r.col_off) + pane_col))
        .map_or('\u{0}', |c| c.ch)
}

/// Bumps `snapshot_seq` on `composed_focus_scratch` to force the next extraction onto the full arm.
fn disown(app: &mut App, wid: WindowId) {
    let ws = app.windows.get_mut(&wid).expect("window");
    ws.composed_focus_scratch.snapshot_seq = ws.composed_focus_scratch.snapshot_seq.wrapping_add(1);
}

/// Writes an undeclared marker character into `composed_focus_scratch` with composed retention off.
fn plant_marker(app: &mut App, wid: WindowId, pane_row: usize) {
    super::COMPOSE_RETAIN_OFF.with(|off| off.set(true));
    let ws = app.windows.get_mut(&wid).expect("window");
    ws.composed_focus_scratch
        .cells
        .get_mut(pane_row)
        .and_then(|row| row.first_mut())
        .expect("the focused pane's resident buffer has that row materialized")
        .ch = 'Q';
}

/// A settled split's focused pane takes the damage-scoped arm and retains undamaged rows.
#[test]
fn the_focused_pane_retains_undamaged_rows_across_composed_frames() {
    let (mut app, wid) = split_app();
    let mut now = Instant::now();
    assert!(
        compose(&mut app, wid, now),
        "the establishing frame presents"
    );
    now += DT;
    assert!(compose(&mut app, wid, now), "the second frame presents");
    assert_eq!(
        composed_char(&app, wid, QUIET_ROW, 0),
        'r',
        "the quiet row starts with its own text, before the marker"
    );
    plant_marker(&mut app, wid, QUIET_ROW);
    let focus = focus_rect(&app, wid).session;
    feed(&app, focus, b"x");
    now += DT;
    assert!(compose(&mut app, wid, now), "the echo frame presents");
    assert_eq!(
        composed_char(&app, wid, QUIET_ROW, 0),
        'Q',
        "the focused pane's UNDAMAGED row was re-resolved — the composed \
         present's focused extraction is not taking the damage-scoped arm, \
         and this change buys nothing"
    );
    assert_eq!(
        composed_char(&app, wid, CARET_ROW, 0),
        'x',
        "…while the DAMAGED row really did refill (otherwise the marker \
         above proves nothing but a frozen frame)"
    );
}

/// [`split_app`] with default matrix rain enabled.
fn split_app_raining() -> (App, WindowId) {
    let (mut app, wid) = split_app();
    app.config.matrix_rain = Some(crate::app_config::MatrixRainConfig {
        enabled: Some(true),
        ..Default::default()
    });
    app.recompute_matrix_rain();
    (app, wid)
}

/// Last damage epoch consumed by the rain engine's Tier-A occupancy scan.
fn rain_scan(app: &App, wid: WindowId) -> Option<u64> {
    app.windows[&wid]
        .fx
        .rain()
        .and_then(crate::matrix_rain::MatrixRain::scanned_epoch)
}

/// When Serious Mode drops the rain engine, a failed command is still baselined so rebuilding rain does not light an ember.
#[test]
fn a_composed_window_with_no_rain_engine_keeps_the_completion_latch_observing() {
    let (mut app, wid) = split_app_raining();
    let mut now = Instant::now();
    assert!(
        compose(&mut app, wid, now),
        "the establishing frame presents"
    );
    let focus = focus_rect(&app, wid).session;
    assert!(
        app.windows[&wid].fx.rain().is_some(),
        "REACH: the rain engine was built"
    );
    assert!(app.set_serious_mode(true));
    feed(
        &app,
        focus,
        b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07\x1b]133;D;1\x07",
    );
    now += DT;
    let _ = compose(&mut app, wid, now);
    assert!(
        app.windows[&wid].fx.rain().is_none(),
        "REACH: no rain engine runs under Serious Mode"
    );
    let term = app.pool.get(focus).expect("pane session").term.clone();
    let seq = term_lock(&term).completed_command_seq();
    assert!(seq > 0, "fixture: the shell reported a completion");
    assert_eq!(
        app.windows[&wid].fx.rain_latches().last_cmd,
        Some((focus, seq)),
        "the rain-less frame baselined the completion"
    );
}

/// Matrix rain's Tier-A rescan reuses the focused extraction instead of taking damage a second time.
#[test]
fn rain_does_not_cost_the_focused_pane_its_damage_scoped_arm() {
    let (mut app, wid) = split_app_raining();
    let mut now = Instant::now();
    assert!(
        compose(&mut app, wid, now),
        "the establishing frame presents"
    );
    let focus = focus_rect(&app, wid).session;
    let scan0 = rain_scan(&app, wid)
        .expect("REACH: rain must be live and scanning, or this test pins nothing about rain");
    feed(&app, focus, b"\rx");
    now += DT;
    assert!(compose(&mut app, wid, now), "the first echo frame presents");
    let scan1 = rain_scan(&app, wid);
    assert_ne!(
        scan1,
        Some(scan0),
        "REACH: the first echo frame ran no fresh Tier-A rescan, so it did \
         not arm the condition this test is about"
    );
    plant_marker(&mut app, wid, QUIET_ROW);
    feed(&app, focus, b"\ry");
    now += DT;
    assert!(
        compose(&mut app, wid, now),
        "the second echo frame presents"
    );
    assert_ne!(
        rain_scan(&app, wid),
        scan1,
        "REACH: the second echo frame ran no fresh Tier-A rescan"
    );
    assert_eq!(
        composed_char(&app, wid, QUIET_ROW, 0),
        'Q',
        "a RAINING focused pane re-resolved its UNDAMAGED row — the rain \
         refresh is taking the pane's damage-scoped arm away again"
    );
    assert_eq!(
        composed_char(&app, wid, CARET_ROW, 0),
        'y',
        "…while the DAMAGED row really did refill (otherwise the marker \
         above proves nothing but a frozen frame)"
    );
}

/// Disowning the carrier forces a full re-resolve that overwrites the planted marker.
#[test]
fn a_disowned_focused_carrier_re_resolves_every_row() {
    let (mut app, wid) = split_app();
    let mut now = Instant::now();
    assert!(
        compose(&mut app, wid, now),
        "the establishing frame presents"
    );
    now += DT;
    assert!(compose(&mut app, wid, now), "the second frame presents");
    plant_marker(&mut app, wid, QUIET_ROW);
    disown(&mut app, wid);
    let focus = focus_rect(&app, wid).session;
    feed(&app, focus, b"x");
    now += DT;
    assert!(compose(&mut app, wid, now), "the echo frame presents");
    assert_eq!(
        composed_char(&app, wid, QUIET_ROW, 0),
        'r',
        "a disowned carrier must fall back to the full re-resolve, which \
         overwrites the planted marker"
    );
}

/// Painting an inline IME composition into the focused pane's scratch buffer disowns the carrier so cancelling leaves no stale cells.
#[test]
fn a_cancelled_composition_leaves_no_cells_behind_in_a_split() {
    let (mut app, wid) = split_app();
    let mut now = Instant::now();
    assert!(
        compose(&mut app, wid, now),
        "the establishing frame presents"
    );
    assert_eq!(
        composed_char(&app, wid, CARET_ROW, 0),
        't',
        "the caret row starts as its own text"
    );
    app.on_ime_preedit(wid, "ZZ".to_string(), Some((2, 2)));
    assert_eq!(
        app.preedit_owner(wid),
        crate::app_input::PreeditOwner::Grid,
        "no field is open, so the GRID owns the composition"
    );
    now += DT;
    assert!(
        compose(&mut app, wid, now),
        "the composition frame presents"
    );
    assert_eq!(
        composed_char(&app, wid, CARET_ROW, 0),
        'Z',
        "the composition really did paint into the focused leaf — otherwise \
         the cancel below proves nothing"
    );
    app.on_ime_preedit(wid, String::new(), None);
    let other = other_session(&app, wid);
    feed(&app, other, b"y");
    now += DT;
    assert!(compose(&mut app, wid, now), "the cancel frame presents");
    assert_eq!(
        composed_char(&app, wid, CARET_ROW, 0),
        't',
        "the cancelled composition is still on the grid — the focused pane's \
         carrier retained HOST-written cells as if the engine had vouched \
         for them"
    );
}

/// Painting a predictive-echo ghost into the focused pane's scratch buffer disowns the carrier so an expired guess leaves no stale cells.
#[test]
fn an_expired_prediction_ghost_leaves_no_cells_behind_in_a_split() {
    let (mut app, wid) = split_app();
    app.config.predictive_echo = Some("always".to_string());
    app.predict_mode_cache = None;
    let focus = focus_rect(&app, wid).session;
    feed(&app, focus, b"tail");
    let mut now = Instant::now();
    assert!(
        compose(&mut app, wid, now),
        "the establishing frame presents"
    );
    let cols = focus_rect(&app, wid).cols;
    let coords = {
        let term = app.pool.get(focus).expect("pane session").term.clone();
        term_lock(&term)
            .content_scroll_state()
            .coordinate_generation()
    };
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.predictor.set_mode(crate::predict::PredictMode::Always);
        assert!(
            ws.predictor.predict_char(
                'Z',
                (u16::try_from(CARET_ROW).expect("small"), 4),
                coords,
                cols,
                now
            ),
            "the predictor accepted the guess"
        );
    }
    now += DT;
    assert!(compose(&mut app, wid, now), "the ghost frame presents");
    assert_eq!(
        composed_char(&app, wid, CARET_ROW, 4),
        'Z',
        "the ghost really did paint into the focused leaf — otherwise the \
         expiry below proves nothing"
    );
    app.windows.get_mut(&wid).expect("window").predictor.reset();
    let other = other_session(&app, wid);
    feed(&app, other, b"y");
    now += DT;
    assert!(compose(&mut app, wid, now), "the expiry frame presents");
    assert_ne!(
        composed_char(&app, wid, CARET_ROW, 4),
        'Z',
        "the expired ghost is still on the grid — the focused pane's carrier \
         retained HOST-written cells as if the engine had vouched for them"
    );
}

/// Compares all pane-extraction channels of two [`aterm_core::render::RenderInput`] frames (the companions by value, the rain by its rows, the wallpaper by presence).
fn extraction_diff(
    a: &aterm_core::render::RenderInput,
    b: &aterm_core::render::RenderInput,
) -> Vec<String> {
    let mut out = Vec::new();
    if (a.rows, a.cols) != (b.rows, b.cols) {
        out.push(format!(
            "dims {}x{} vs {}x{}",
            a.rows, a.cols, b.rows, b.cols
        ));
    }
    for r in 0..a.cells.len().min(b.cells.len()) {
        if a.cells[r] != b.cells[r] {
            let ra: String = a.cells[r].iter().map(|c| c.ch).collect();
            let rb: String = b.cells[r].iter().map(|c| c.ch).collect();
            out.push(format!(
                "row {r}{}:\n  scoped {ra:?}\n  full   {rb:?}",
                if ra == rb {
                    " (same text, attrs differ)"
                } else {
                    ""
                }
            ));
        }
    }
    if a.cells.len() != b.cells.len() {
        out.push(format!("row count {} vs {}", a.cells.len(), b.cells.len()));
    }
    macro_rules! same {
        ($($f:ident),* $(,)?) => {
            $( if a.$f != b.$f { out.push(stringify!($f).to_string()); } )*
        };
    }
    same!(
        cursor_row,
        cursor_col,
        cursor_visible,
        cursor_style,
        display_offset,
        clusters,
        combining,
        images,
        line_sizes,
        line_size_spans,
        default_bg_spans,
        default_bg,
        default_fg,
        cursor_color,
        selection,
        selections,
        selection_clip,
        selection_bg,
        selection_fg,
        cursor_trail,
        cursor_trail_color,
        cursor_glow_add,
        glow_halo,
        glow_under,
        fire_patch,
        cursor_fill_override,
        word_decorations,
        ink,
        char_fg,
        fire_halo,
        nova_add,
        rain_add,
    );
    use aterm_render::sprites::published;
    if published(&a.fx_layers) != published(&b.fx_layers) {
        out.push("companion sprite layers".to_string());
    }
    let rain_rows = |i: &aterm_core::render::RenderInput| {
        let layers = i.fx_layers.iter();
        layers
            .filter(|l| aterm_effects::matrix_rain::is_rain_row(l))
            .count()
    };
    if rain_rows(a) != rain_rows(b) {
        out.push(format!("rain rows {} vs {}", rain_rows(a), rain_rows(b)));
    }
    if a.wallpaper.is_some() != b.wallpaper.is_some() {
        out.push(format!(
            "wallpaper presence {} vs {}",
            a.wallpaper.is_some(),
            b.wallpaper.is_some()
        ));
    }
    out
}

/// Differential oracle comparing damage-scoped and full focused-pane extraction across a split-window script.
#[test]
fn the_scoped_focused_extraction_composes_what_the_full_one_does() {
    #[allow(clippy::type_complexity)]
    let script: Vec<(&str, &dyn Fn(&App, WindowId))> = vec![
        ("settle", &|_, _| {}),
        ("echo", &|a: &App, w| {
            feed(a, focus_rect(a, w).session, b"x")
        }),
        ("echo again", &|a: &App, w| {
            feed(a, focus_rect(a, w).session, b"\ry");
        }),
        ("background write", &|a: &App, w| {
            feed(a, other_session(a, w), b"\rbg");
        }),
        ("sgr run", &|a: &App, w| {
            feed(a, focus_rect(a, w).session, b"\r\x1b[1;31mRED\x1b[0m");
        }),
        ("wide + combining", &|a: &App, w| {
            feed(a, focus_rect(a, w).session, "\r\u{65e5}e\u{301}".as_bytes());
        }),
        ("newline scroll", &|a: &App, w| {
            feed(a, focus_rect(a, w).session, b"\r\n\r\nscrolled");
        }),
        ("flood", &|a: &App, w| {
            let s = focus_rect(a, w).session;
            for i in 0..30u8 {
                feed(a, s, format!("flood {i}\r\n").as_bytes());
            }
            feed(a, s, b"prompt $ ");
        }),
        ("alt screen", &|a: &App, w| {
            feed(a, focus_rect(a, w).session, b"\x1b[?1049h alt content ");
        }),
        ("alt echo", &|a: &App, w| {
            feed(a, focus_rect(a, w).session, b"\rmore");
        }),
        ("leave alt", &|a: &App, w| {
            feed(a, focus_rect(a, w).session, b"\x1b[?1049l");
        }),
        ("erase display", &|a: &App, w| {
            feed(a, focus_rect(a, w).session, b"\x1b[2J\x1b[Hfresh");
        }),
        ("settle again", &|_, _| {}),
        ("settle once more", &|_, _| {}),
    ];

    let (mut scoped_app, wid) = split_app();
    let (mut full_app, _) = split_app();
    let mut now = Instant::now();
    for (label, step) in script {
        step(&scoped_app, wid);
        step(&full_app, wid);
        now += DT;
        disown(&mut full_app, wid);
        let a = compose(&mut scoped_app, wid, now);
        let b = compose(&mut full_app, wid, now);
        assert_eq!(a, b, "[{label}] the two arms disagreed about PRESENTING");
        if !a {
            continue;
        }
        let sa = &scoped_app.windows.get(&wid).expect("window").input_scratch;
        let sb = &full_app.windows.get(&wid).expect("window").input_scratch;
        let diff = extraction_diff(sa, sb);
        assert!(
            diff.is_empty(),
            "[{label}] the damage-scoped focused extraction composed a \
             DIFFERENT frame from the full one:\n{}",
            diff.join("\n")
        );
    }
}

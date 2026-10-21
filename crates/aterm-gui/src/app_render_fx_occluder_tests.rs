// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Engine occluders at the host (`docs/DESIGN-card-engine-2026-10-19.md`
//! §4.3, §7 `fx_occluders_compose`): every chrome surface the host splices
//! over the grid (a row band through `scrub_overlay_row_band`, a card through
//! `scrub_overlay_card`) pushes its window-pixel rectangle into
//! `RenderInput::fx_occluders`, the renderer subtracts every one of them from
//! every fx layer, and so any number of surfaces compose: each keeps the
//! pixels it has without the light, while the light keeps drawing around
//! them. The Autopilot menu's case is in `autopilot_menu_tests`, beside its
//! fixture.

use std::time::Instant;

use aterm_core::render::{
    FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, PxRect, RenderInput,
};

use super::{App, ComposedCursorFxClock, WindowId};
use crate::term_lock;

/// How a chrome surface claims the frame: whole rows (a band, cut full
/// width) or a box of cells (a card).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Shape {
    Band,
    Card,
}

/// What one chrome splice painted, as window pixels: the rows (a band) or
/// the cell box (a card) of every frame cell whose rendered pixels the
/// splice changed. Read off the glass, so it never consults the occluder it
/// is checked against, and a cell a splice only materialized (a trimmed
/// blank padded out to its ground) is not counted.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChromeBox {
    pub(crate) name: &'static str,
    pub(crate) px: PxRect,
}

/// The window's CPU renderer, set to the window's geometry.
fn renderer(app: &mut App, wid: WindowId) -> &mut aterm_render::Renderer {
    let (pad, pad_top, head, cell) = (
        app.win_pad(wid),
        app.win_pad_top(wid),
        app.win_head(wid),
        app.win_cell_size(wid),
    );
    let crate::Backend::Cpu(r) = app.backend.ready_mut() else {
        panic!("the headless backend is the CPU renderer");
    };
    r.set_pad(pad);
    r.set_pad_top(pad_top);
    r.set_head(head);
    assert_eq!(r.cell_size(), cell, "the renderer draws the window's cells");
    r
}

/// Run `splice` over the window's composed frame and return what it painted.
pub(crate) fn trace_splice(
    app: &mut App,
    wid: WindowId,
    name: &'static str,
    shape: Shape,
    splice: impl FnOnce(&mut App),
) -> ChromeBox {
    let before = app.windows[&wid].input_scratch.clone();
    splice(app);
    chrome_written(app, wid, name, shape, &before)
}

/// The box of the frame cells whose pixels differ from `before`'s, in window
/// pixels (`grid_top + row·ch`, `pad + col·cw`; a band spans every column and
/// the pads beside them, as its ground does).
pub(crate) fn chrome_written(
    app: &mut App,
    wid: WindowId,
    name: &'static str,
    shape: Shape,
    before: &RenderInput,
) -> ChromeBox {
    let after = app.windows[&wid].input_scratch.clone();
    assert_eq!(
        before.cells.len(),
        after.cells.len(),
        "{name}: a splice over the frame adds no row"
    );
    let (cw, ch) = app.win_cell_size(wid);
    let (pad, grid_top) = (app.win_pad(wid), app.win_pad_top(wid) + app.win_head(wid));
    let r = renderer(app, wid);
    let (was, now) = (r.render_input(before), r.render_input(&after));
    let painted = |row: usize, col: usize| {
        (grid_top + row * ch..grid_top + (row + 1) * ch).any(|y| {
            (pad + col * cw..pad + (col + 1) * cw).any(|x| {
                let i = y * now.width + x;
                was.pixels[i] != now.pixels[i]
            })
        })
    };
    let (mut rows, mut cols) = ((usize::MAX, 0), (usize::MAX, 0));
    for row in 0..after.cells.len() {
        for col in 0..after.cols {
            if painted(row, col) {
                rows = (rows.0.min(row), rows.1.max(row + 1));
                cols = (cols.0.min(col), cols.1.max(col + 1));
            }
        }
    }
    assert!(rows.0 < rows.1, "precondition: {name} is up on this frame");
    let px = |v: usize| i32::try_from(v).expect("a test frame is small");
    let (x0, x1) = match shape {
        Shape::Band => (i32::MIN, i32::MAX),
        Shape::Card => (px(pad + cols.0 * cw), px(pad + cols.1 * cw)),
    };
    ChromeBox {
        name,
        px: PxRect::new(
            x0,
            px(grid_top + rows.0 * ch),
            x1,
            px(grid_top + rows.1 * ch),
        ),
    }
}

/// An fx layer of one rect far larger than any test frame.
fn sheet(id: u64, slot: FxSlot, space: FxSpace, ink: FxInk) -> FxLayer {
    FxLayer::new(
        id,
        std::sync::Arc::new(FxContent::new(
            vec![FxItem::Rect(FxRect {
                x: -4096,
                y: -4096,
                w: u16::MAX,
                h: u16::MAX,
                ink: 0,
            })],
            // A rect's two ends, one flat paint.
            vec![ink, ink],
        )),
        slot,
        space,
        FxBlend::Exact,
    )
}

/// Light over the whole frame in every slot: a Grid-space wash under the
/// glyphs, Window-space additive glow and a translucent sheet over them, the
/// shapes a decorated card's border, bloom and drawer take.
fn light() -> Vec<FxLayer> {
    vec![
        sheet(
            1,
            FxSlot::Under,
            FxSpace::Grid,
            FxInk::over(0x0030_C060, 0x90),
        ),
        sheet(2, FxSlot::Glow, FxSpace::Window, FxInk::add(0x0020_1008)),
        sheet(
            3,
            FxSlot::Over,
            FxSpace::Window,
            FxInk::over(0x00C0_3070, 0xA0),
        ),
    ]
}

/// How many pixels inside `px` differ between two frames of one size.
fn diff_in(a: &aterm_render::Frame, b: &aterm_render::Frame, px: PxRect) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let clamp = |v: i32, hi: usize| usize::try_from(v.max(0)).map_or(0, |v| v.min(hi));
    let (x0, x1) = (clamp(px.x0, a.width), clamp(px.x1, a.width));
    let (y0, y1) = (clamp(px.y0, a.height), clamp(px.y1, a.height));
    (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| y * a.width + x))
        .filter(|&i| a.pixels[i] != b.pixels[i])
        .count()
}

/// THE LAW: over the window's composed frame, [`light`] in every slot leaves
/// each chrome box byte-identical to the frame without it, while it draws
/// around them. `boxes` are in splice order.
///
/// CONTROLS, each rendered here: with no occluder (the per-stream scrubs
/// alone) the light lands on every box; under the single-cut model (one
/// `cut`, the last writer's) it lands on every box but the last.
pub(crate) fn assert_chrome_cuts_the_light(app: &mut App, wid: WindowId, boxes: &[ChromeBox]) {
    let input: RenderInput = app.windows[&wid].input_scratch.clone();
    assert!(
        input.fx_layers.is_empty(),
        "the fixture frame carries no light of its own"
    );
    let cuts = input.fx_occluders.clone();
    assert!(
        cuts.len() >= boxes.len(),
        "every surface pushed its occluder: {cuts:?} for {boxes:?}"
    );
    let r = renderer(app, wid);
    let mut render = |layers: &[FxLayer], occluders: &[PxRect]| {
        let mut frame = input.clone();
        frame.fx_layers = layers.to_vec();
        frame.fx_occluders = occluders.to_vec();
        r.render_input(&frame)
    };
    let layers = light();
    let bare = render(&[], &cuts);
    let lit = render(&layers, &cuts);
    for b in boxes {
        assert_eq!(
            diff_in(&lit, &bare, b.px),
            0,
            "{}: the chrome keeps the pixels it has without the light ({:?})",
            b.name,
            b.px
        );
    }
    let everywhere = PxRect::EVERYWHERE;
    let around: usize = diff_in(&lit, &bare, everywhere);
    assert!(
        around > 0,
        "the light draws around the chrome (the law is not vacuous)"
    );

    let uncut = render(&layers, &[]);
    for b in boxes {
        assert!(
            diff_in(&uncut, &bare, b.px) > 0,
            "CONTROL: with no occluder the light lands on {}",
            b.name
        );
    }
    let last = *cuts.last().expect("at least one occluder");
    let single = render(&layers, &[last]);
    let (kept, earlier) = boxes.split_last().expect("at least one surface");
    assert_eq!(
        diff_in(&single, &bare, kept.px),
        0,
        "the last surface's own cut keeps {} clear",
        kept.name
    );
    for b in earlier {
        assert!(
            diff_in(&single, &bare, b.px) > 0,
            "CONTROL: a single cut (the last writer's) leaves light on {}",
            b.name
        );
    }
}

/// OSC 8 link: the visible run reads `google.com`, the URI another site.
const LINK: &[u8] =
    b"\x1b[9;1HVisit \x1b]8;;https://evil.example/steal\x1b\\google.com\x1b]8;;\x1b\\ now";
/// The link's terminal row and the column of its first linked cell.
const LINK_CELL: (u16, u16) = (8, 6);

fn feed(app: &App, wid: WindowId, bytes: &[u8]) {
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(bytes);
}

/// Hover the centre of terminal cell `(row, col)` through the real motion path.
fn hover(app: &mut App, wid: WindowId, (row, col): (u16, u16)) {
    let (cw, ch) = app.win_cell_size(wid);
    let strip_px = f64::from(app.tab_strip_rows) * ch as f64;
    let x = app.win_pad(wid) as f64 + cw as f64 * (f64::from(col) + 0.5);
    let y = (app.win_pad_top(wid) + app.win_head(wid)) as f64
        + strip_px
        + ch as f64 * (f64::from(row) + 0.5);
    app.on_cursor_moved(wid, x, y);
}

fn ask_paste(app: &mut App, wid: WindowId) {
    let session = app.front_terminal(wid).map(|t| t.session);
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        session,
        "line one\nline two\nline three\n".to_string(),
        crate::input::Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));
}

/// The capture route's frame up to the chrome: the engine fill, the strip
/// and the find bar (a no-op when closed).
fn fill(app: &mut App, wid: WindowId) {
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Advance(Instant::now()),
        )
        .is_some(),
        "the headless capture produces a frame"
    );
    app.splice_tab_strip(wid);
    app.splice_find_bar(wid);
}

/// Two row bands, the paste question over the top rows and the link caption
/// on the bottom one, each cut from the light; a single cut would leave it on
/// the question.
#[test]
fn fx_occluders_compose_over_the_paste_banner_and_the_link_caption() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    feed(&app, wid, LINK);
    hover(&mut app, wid, LINK_CELL);
    ask_paste(&mut app, wid);
    fill(&mut app, wid);
    let banner = trace_splice(&mut app, wid, "paste banner", Shape::Band, |app| {
        app.splice_paste_banner(wid);
    });
    let caption = trace_splice(&mut app, wid, "link caption", Shape::Band, |app| {
        app.splice_link_target(wid);
    });
    assert!(
        banner.px.y1 <= caption.px.y0,
        "two separate bands: {banner:?} over {caption:?}"
    );
    assert_chrome_cuts_the_light(&mut app, wid, &[banner, caption]);
}

/// A band and a card: the paste question, then the tab menu hanging under
/// the strip over it (the caption yields to an open menu).
#[test]
fn fx_occluders_compose_over_the_paste_banner_and_the_tab_menu() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_tab_context_menu(wid, 0, 3, false));
    ask_paste(&mut app, wid);
    fill(&mut app, wid);
    let banner = trace_splice(&mut app, wid, "paste banner", Shape::Band, |app| {
        app.splice_paste_banner(wid);
    });
    let menu = trace_splice(&mut app, wid, "tab menu", Shape::Card, |app| {
        app.splice_tab_menu(wid);
    });
    assert!(
        app.windows[&wid].tab_menu_rect.is_some(),
        "the menu painted"
    );
    assert_chrome_cuts_the_light(&mut app, wid, &[banner, menu]);
}

/// A closed surface stops cutting: the capture's fill drops the previous
/// frame's occluders (`cell_frame_into` keeps host overlays), so the frame
/// after the question is answered carries none, and the light it would have
/// kept off the top rows draws there again.
#[test]
fn an_answered_question_stops_cutting_the_light() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    ask_paste(&mut app, wid);
    fill(&mut app, wid);
    app.splice_paste_banner(wid);
    assert_eq!(
        app.windows[&wid].input_scratch.fx_occluders.len(),
        1,
        "the question cuts"
    );
    app.paste_banner = None;
    fill(&mut app, wid);
    app.splice_paste_banner(wid);
    assert_eq!(
        app.windows[&wid].input_scratch.fx_occluders,
        Vec::<PxRect>::new(),
        "an answered question leaves no cut behind"
    );
}

/// The same on the live single-pane path (`redraw_window`, a scripted CPU
/// surface), whose scratch keeps host writes from frame to frame: the frame
/// after the answer clears the question's cut with the other host writes it
/// undoes, and the question's frame carries exactly one.
#[test]
fn the_live_frame_after_the_answer_carries_no_cut() {
    use crate::scripted_redraw_fixture::{scripted_app, surface};
    let mut app = scripted_app();
    let wid = WindowId(0);
    app.redraw_window(wid);
    ask_paste(&mut app, wid);
    app.redraw_window(wid);
    // Output under the question: a second frame that refills the scratch.
    let commits = surface(&mut app).commits();
    crate::scripted_redraw_fixture::feed(&app, b"\x1b[20;1Hmore output");
    app.redraw_window(wid);
    assert!(
        surface(&mut app).commits() > commits,
        "the second frame ran"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.fx_occluders.len(),
        1,
        "the question cuts, once however many frames it stands"
    );
    // The answer, and output after it: the frame that refills the scratch.
    app.paste_banner = None;
    let commits = surface(&mut app).commits();
    crate::scripted_redraw_fixture::feed(&app, b"\x1b[21;1Hafter the answer");
    app.redraw_window(wid);
    assert!(surface(&mut app).commits() > commits, "the frame ran");
    assert_eq!(
        app.windows[&wid].input_scratch.fx_occluders,
        Vec::<PxRect>::new(),
        "an answered question leaves no cut behind"
    );
}

/// The scrubs' occluders, exactly: a band is its frame rows full width (the
/// pads beside it included) at `grid_top + row·ch`; a card is its cell box at
/// `(pad, grid_top)`; an empty band pushes nothing.
#[test]
fn the_scrubs_push_their_window_pixel_occluders() {
    let mut input = RenderInput::empty();
    let bands = aterm_render::RowBands::new(11, 17);
    super::scrub_overlay_row_band(&mut input, 2..4, bands);
    super::scrub_overlay_row_band(&mut input, 3..3, bands);
    super::scrub_overlay_card(
        &mut input,
        crate::tab_menu::MenuRect {
            row: 1,
            col: 2,
            w: 3,
            h: 2,
        },
        (9, 17),
        (5, 11),
    );
    assert_eq!(
        input.fx_occluders,
        vec![
            PxRect::new(i32::MIN, 11 + 2 * 17, i32::MAX, 11 + 4 * 17),
            PxRect::new(5 + 2 * 9, 11 + 17, 5 + 5 * 9, 11 + 3 * 17),
        ]
    );
}

/// A compose frame starts free of fx: the fill clears the layers (each
/// pane's producers re-install theirs) and the occluders (the chrome spliced
/// after pushes its own again).
#[test]
fn a_compose_fill_clears_the_fx_layers_and_occluders() {
    let mut input = RenderInput::empty();
    input.fx_layers = light();
    input.fx_occluders = vec![PxRect::new(0, 0, 9, 9)];
    super::clear_effect_overlays_for_compose(&mut input);
    assert!(input.fx_layers.is_empty() && input.fx_occluders.is_empty());
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness card on the glass: a real engine, a real frame through the
//! capture route's splices, the real splice — and the layout's rules on
//! hand-made cells.

use super::*;

use std::sync::Arc;
use std::time::Duration;

use aterm_agent::harness::footer::FooterFacts;
use aterm_agent::supervise::Autopilot;
use aterm_core::render::{FxInk, FxItem, FxLayer, FxRamps, FxRect, FxSlot, FxSpace, PxRect};
use aterm_core::terminal::{CutKind, HolderCut};
use aterm_effects::harness_card::{EPISODE_MS, KITTY_W};

/// The build the screens' card names, and the process the facts are for.
const VERSION: &str = "2.1.283";
const PGID: i32 = 4242;

/// The model row of the screens' card: short, so the kitty has room.
const MODEL: &str = "Opus 5.5 \u{b7} Claude Max";

/// [`MODEL`] with its last glyph wide: as many chars (so the card's place
/// reads the same), a cell longer on the glass.
const MODEL_WIDE_END: &str = "Opus 5.5 \u{b7} Claude Ma\u{4E2D}";

/// The card's working directory is drawn in this gray, as Claude draws it.
const GRAY: [u8; 3] = [130, 130, 130];

/// The vendor's banner text (anchor `launch.banner`), from the one table.
fn banner() -> &'static str {
    aterm_phase::anchor_text("launch.banner")
}

/// A Claude Code screen `rows` × `cols`: a blank row, the wide launch card
/// (its text at column 11, its build [`VERSION`] or `version`), blank rows,
/// then the composer between its rules and the mode row at the bottom.
fn card_screen(rows: usize, cols: usize, model: &str, version: &str) -> Vec<String> {
    let rule = "\u{2500}".repeat(cols);
    let mut screen = vec![
        String::new(),
        format!(
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {}{version}",
            banner()
        ),
        format!(
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  {model}"
        ),
        " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/aterm".to_string(),
    ];
    while screen.len() < rows - 4 {
        screen.push(String::new());
    }
    screen.extend([
        rule.clone(),
        "\u{276F} ".to_string(),
        rule,
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".to_string(),
    ]);
    screen
}

/// Paint `rows` into `session`'s terminal: the working directory row in
/// [`GRAY`], the rest in the default ink, the caret on the composer.
fn feed(app: &App, session: u64, rows: &[String]) {
    let mut out = String::from("\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        let sgr = if row.ends_with("~/aterm") {
            format!("\x1b[38;2;{};{};{}m", GRAY[0], GRAY[1], GRAY[2])
        } else {
            String::new()
        };
        out.push_str(&format!("\x1b[{};1H{sgr}{row}\x1b[0m", i + 1));
    }
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('\u{276F}'))
        .unwrap_or(0);
    out.push_str(&format!("\x1b[{};3H", caret + 1));
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).process(out.as_bytes());
}

/// The image of a Claude Code of build `version` leading group `pgid`,
/// started at `start`: a managed-store image, whose banner prints its build
/// (the gate's Exact policy, card engine §3.8).
fn claude_image(pgid: i32, version: &str, start: u64) -> crate::holder_image::HolderImage {
    use atpkg::image::{ImageBuild, ImageSource, ProgramTag};
    let build = atpkg::vendor_direct::Version::parse(version)
        .expect("a vendor build")
        .build_id();
    crate::holder_image::HolderImage::of(
        pgid,
        Some(start),
        Some(std::path::PathBuf::from(format!(
            "/p/pkg/store/claude/{build}/bin/claude"
        ))),
        ImageBuild {
            program: ProgramTag::new("claude"),
            build,
            source: ImageSource::Store,
        },
        1,
    )
}

/// The host's report of a change of `session`'s foreground holder to the
/// drawer `to` (`None`: a shell, another command), as the PTY reader cuts the
/// engine's holder fence (`spawn::note_cut`, card engine §3.7).
fn cut(app: &App, session: u64, to: Option<u64>, kind: CutKind, from_alive: bool) {
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).note_holder_cut(HolderCut {
        to,
        kind,
        from_alive,
    });
}

/// The footer facts of a Claude Code process of build `version`, in front.
fn publish(app: &App, session: u64, version: &str) {
    publish_as(app, session, version, PGID);
}

/// [`publish`] for the process group `pgid`: its image published (a group
/// edge, or an `exec` where the group's last image was another), the fence
/// cut to it — what it paints after this is its own — and its facts.
fn publish_as(app: &App, session: u64, version: &str, pgid: i32) {
    launch(app, session, version, pgid, 1);
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.set_claude_footer(
        pgid,
        None,
        Some(FooterFacts {
            model: Some("Opus 5.5".into()),
            version: Some(version.into()),
            ..FooterFacts::default()
        }),
    );
}

/// A Claude Code of build `version` started at `start` takes the front as
/// group `pgid`: its image published and the fence cut to it, no facts. The
/// drawer's token.
fn launch(app: &App, session: u64, version: &str, pgid: i32, start: u64) -> u64 {
    use crate::holder_image::EdgeKind;
    let image = claude_image(pgid, version, start);
    let token = image.token.expect("a token");
    let exec = {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let exec = timeline
            .holder_image()
            .is_some_and(|last| last.pgid == pgid && last.token != Some(token));
        timeline.note_foreground_group(pgid);
        let kind = if exec {
            EdgeKind::Exec
        } else {
            EdgeKind::Group
        };
        timeline.offer_holder_image(Arc::new(image), kind);
        exec
    };
    let kind = if exec { CutKind::Exec } else { CutKind::Group };
    cut(app, session, Some(token), kind, false);
    token
}

/// This window's host, holding `session`'s live supervisor claim: the
/// session is SUPERVISED (`App::session_autopilot`). The caller shuts the
/// host down.
fn supervise(app: &mut App, session: u64) -> crate::harness_host::HostHandle {
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("the claim");
    host
}

/// One frame through the capture route's order (`App::render_image`'s
/// terminal arm): extract with the cursor effects, publish the word streams,
/// prepend the strip, then the footer's splice.
fn frame(app: &mut App, wid: WindowId) {
    app.capture_words(wid, Instant::now());
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    app.splice_tab_strip(wid);
    app.splice_claude_footer(wid, &plan, route);
}

/// The group of the shell the tests' Claude Code runs under.
const SHELL_PGID: i32 = 4000;

/// Claude Code leaves the front of `session` — stopped (`from_alive`) or
/// gone: its facts dropped, the shell's group in front, the shell's image
/// (no drawer) published and the fence cut to it.
fn leave(app: &App, session: u64, from_alive: bool) {
    use atpkg::image::{ImageBuild, ProgramTag};
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let pgid = timeline.agent().program_pgid;
        timeline.set_claude_footer(pgid, None, None);
        timeline.note_foreground_group(SHELL_PGID);
        timeline.offer_holder_image(
            Arc::new(crate::holder_image::HolderImage::of(
                SHELL_PGID,
                Some(1),
                Some(std::path::PathBuf::from("/bin/zsh")),
                ImageBuild::unknown(ProgramTag::new("zsh")),
                1,
            )),
            crate::holder_image::EdgeKind::Group,
        );
    }
    cut(app, session, None, CutKind::Group, from_alive);
}

/// A headless window `cols` wide and 24 rows tall, its front session, the
/// facts of its process published and its card on the screen — painted by
/// it, after it took the front.
fn glass(cols: u16) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    if cols != 80 {
        assert!(app.apply_term_resize(wid, 24, cols));
    }
    publish(&app, session, VERSION);
    feed(
        &app,
        session,
        &card_screen(24, usize::from(cols), MODEL, VERSION),
    );
    (app, wid, session)
}

/// The chrome rows above the terminal grid on `wid`'s frame.
fn strip_rows(app: &App, wid: WindowId) -> usize {
    app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows)
}

/// The text of frame row `row`, columns `cols`.
fn glass_text(app: &App, wid: WindowId, row: usize, cols: std::ops::Range<usize>) -> String {
    app.windows[&wid].input_scratch.cells[row]
        .iter()
        .skip(cols.start)
        .take(cols.len())
        .map(|c| c.ch)
        .collect()
}

/// The card's record of what it put on the frame.
fn painted(app: &App, wid: WindowId) -> &CardPainted {
    &app.windows[&wid].claude_card.painted
}

/// The card's layer of `slot` as the last splice put it on the frame.
fn card_layer(app: &App, wid: WindowId, slot: FxSlot) -> Option<FxLayer> {
    painted(app, wid)
        .layers
        .iter()
        .find(|l| l.slot == slot)
        .cloned()
}

/// The card's layers as the last splice put them on the frame: compared as
/// layers are, by IDENTITY (the same content `Arc`s at the same place: a
/// replayed frame), never by their bytes ([`card_pic`] for those).
fn card_layers(app: &App, wid: WindowId) -> Vec<FxLayer> {
    painted(app, wid).layers.clone()
}

/// The grid's origin in `wid`'s window pixels: the padding and the head,
/// the renderers' shift of a grid-space layer.
fn grid_origin(app: &App, wid: WindowId) -> (i32, i32) {
    (
        pixel(app.win_pad(wid)),
        pixel(app.win_pad_top(wid) + app.win_head(wid)),
    )
}

/// One item of a card's layer where it lands, in WINDOW pixels: its bounds
/// at the layer's offset within the layer's clip (a grid-space layer's
/// moved by the grid's origin), the palette entries it paints, and a
/// sprite's art (its content id and width).
#[derive(Clone, Debug, PartialEq)]
struct Mark {
    at: PxRect,
    inks: Vec<FxInk>,
    art: Option<(u128, u16)>,
}

impl Mark {
    /// The mark moved by `(dx, dy)`.
    fn moved(&self, dx: i32, dy: i32) -> Self {
        Self {
            at: self.at.translate(dx, dy),
            ..self.clone()
        }
    }
}

/// The marks of the card's `slot` layer on `wid`'s last frame, in item
/// order; none where the layer holds nothing or is not on the frame.
fn marks(app: &App, wid: WindowId, slot: FxSlot) -> Vec<Mark> {
    let grid = grid_origin(app, wid);
    let mut out = Vec::new();
    for layer in painted(app, wid).layers.iter().filter(|l| l.slot == slot) {
        let shift = match layer.space {
            FxSpace::Window => (0, 0),
            FxSpace::Grid => grid,
        };
        let palette = layer.content.palette();
        for item in layer.content.items() {
            let at = item
                .bounds()
                .translate(layer.offset.0, layer.offset.1)
                .intersect(layer.clip)
                .translate(shift.0, shift.1);
            if at.is_empty() {
                continue;
            }
            let (base, n, art) = match item {
                FxItem::Rect(r) => (r.ink, 2, None),
                FxItem::Ramps(r) => (r.ink, 2 * usize::from(r.h), None),
                FxItem::Sprite(s) => (
                    s.ink,
                    usize::from(s.art.max_index()),
                    Some((s.art.id(), s.art.w())),
                ),
            };
            let base = base as usize;
            out.push(Mark {
                at,
                inks: palette[base..base + n].to_vec(),
                art,
            });
        }
    }
    out
}

/// The card's under marks (the border and the trail) and over marks (the
/// kitty, its rim and the stars), as they stand.
fn card_marks(app: &App, wid: WindowId) -> (Vec<Mark>, Vec<Mark>) {
    (
        marks(app, wid, FxSlot::Under),
        marks(app, wid, FxSlot::Over),
    )
}

/// Whether `m` is the kitty: a sprite of the kitty's art width, every
/// colour opaque.
fn is_kitty(m: &Mark) -> bool {
    m.art.is_some_and(|(_, w)| usize::from(w) == KITTY_W) && m.inks.iter().all(|i| i.alpha == 255)
}

/// The kitty's mark in a card's marks.
fn kitty_mark((_, over): &(Vec<Mark>, Vec<Mark>)) -> Option<&Mark> {
    over.iter().find(|m| is_kitty(m))
}

/// The kitty's left edge in a card's marks (window pixels).
fn kitty_x(card: &(Vec<Mark>, Vec<Mark>)) -> Option<i32> {
    kitty_mark(card).map(|m| m.at.x0)
}

/// The span the trail covers in a card's marks: its `under` marks clear of
/// the border's stroke (which holds the layer's outermost pixels), left to
/// right.
fn trail_span((under, _): &(Vec<Mark>, Vec<Mark>)) -> Option<(i32, i32)> {
    let x0 = under.iter().map(|m| m.at.x0).min()?;
    let y0 = under.iter().map(|m| m.at.y0).min()?;
    let x1 = under.iter().map(|m| m.at.x1).max()?;
    let y1 = under.iter().map(|m| m.at.y1).max()?;
    let inside = under
        .iter()
        .filter(|m| m.at.x0 > x0 && m.at.y0 > y0 && m.at.x1 < x1 && m.at.y1 < y1);
    let a = inside.clone().map(|m| m.at.x0).min()?;
    let b = inside.map(|m| m.at.x1).max()?;
    Some((a, b))
}

/// The smallest window-pixel rectangle holding `marks`.
fn hull(marks: &[Mark]) -> PxRect {
    marks.iter().fold(PxRect::EMPTY, |h, m| h.hull(m.at))
}

/// A card layer compared by VALUE: where it lies and every item and
/// palette entry, a sprite's art by its content id — what a card drawn
/// anew, or another window's card, must equal bit for bit.
#[derive(Clone, Debug, PartialEq)]
struct LayerPic {
    slot: FxSlot,
    space: FxSpace,
    offset: (i32, i32),
    clip: PxRect,
    items: Vec<ItemPic>,
    palette: Vec<FxInk>,
}

/// An fx item by value ([`LayerPic`]).
#[derive(Clone, Debug, PartialEq)]
enum ItemPic {
    Rect(FxRect),
    Ramps(FxRamps),
    Sprite {
        at: (i32, i32, u16, u16),
        flip_x: bool,
        art: u128,
        ink: u32,
        clip: PxRect,
    },
}

fn layer_pic(layer: &FxLayer) -> LayerPic {
    LayerPic {
        slot: layer.slot,
        space: layer.space,
        offset: layer.offset,
        clip: layer.clip,
        items: layer
            .content
            .items()
            .iter()
            .map(|item| match item {
                FxItem::Rect(r) => ItemPic::Rect(*r),
                FxItem::Ramps(r) => ItemPic::Ramps(*r),
                FxItem::Sprite(s) => ItemPic::Sprite {
                    at: (s.x, s.y, s.dw, s.dh),
                    flip_x: s.flip_x,
                    art: s.art.id(),
                    ink: s.ink,
                    clip: s.clip,
                },
            })
            .collect(),
        palette: layer.content.palette().to_vec(),
    }
}

/// The card's layers by value ([`LayerPic`]), in the order the splice put
/// them on.
fn card_pic(app: &App, wid: WindowId) -> Vec<LayerPic> {
    painted(app, wid).layers.iter().map(layer_pic).collect()
}

/// The card's layers as the CPU renderer paints them over a black window
/// (`aterm_render::fx::paint_layers`): through the frame's fx occluders
/// (`occluded`) or not. Window pixels, row-major, and the window's width.
fn card_pixels(app: &App, wid: WindowId, occluded: bool) -> (Vec<u32>, usize) {
    let (cw, ch) = app.win_cell_size(wid);
    let scratch = &app.windows[&wid].input_scratch;
    let (pad, grid_top) = (app.win_pad(wid), app.win_pad_top(wid) + app.win_head(wid));
    let cols = scratch.cells.first().map_or(0, Vec::len);
    let fx = aterm_render::FxFrame {
        w: 2 * pad + cols * cw,
        h: 2 * grid_top + scratch.cells.len() * ch,
        pad,
        grid_top,
        cell_h: ch,
        rows: scratch.cells.len(),
    };
    let mut px = vec![0u32; fx.w * fx.h];
    let occluders: &[PxRect] = if occluded { &scratch.fx_occluders } else { &[] };
    aterm_render::fx::paint_layers(&mut px, &fx, &painted(app, wid).layers, occluders);
    (px, fx.w)
}

/// A menu's rectangle in `wid`'s window pixels.
fn menu_px(app: &App, wid: WindowId, rect: crate::tab_menu::MenuRect) -> PxRect {
    let (cw, ch) = app.win_cell_size(wid);
    let (x, y) = grid_origin(app, wid);
    PxRect::new(
        x + pixel(rect.col * cw),
        y + pixel(rect.row * ch),
        x + pixel((rect.col + rect.w) * cw),
        y + pixel((rect.row + rect.h) * ch),
    )
}

/// The line the card writes for `view` in a pane `cols` wide with its star at
/// column `at`: the widest of [`line_texts`] that ends inside the border.
fn expected_line(view: AutopilotView, at: usize, cols: usize) -> String {
    line_texts(view, LineEnd::Mode)
        .iter()
        .find(|t| at + LINE_LEAD + text_cells(t) < cols)
        .expect("a line fits")
        .clone()
}

/// THE WRAPPED CARD (§3): Cruise under this host's live claim — the border
/// and the trail in the card's under layer (window space, additive on a
/// dark ground, every item on one of the box's frame rows or the blank rows
/// over and under it, the border standing half a row into each: its air,
/// §10.6.8), the kitty and the stars in its over layer (grid space at the
/// same origin, the same pane; the kitty opaque art), both on the frame's
/// layers, and the line `✦ enhanced by aterm harness <version> · autopilot
/// ∞ cruise` written on the glass under the card at its text column, in the
/// footer's value colour, inked, behind a drawn `Star4`.
#[test]
fn the_wrapped_card_wears_its_border_kitty_stars_and_line() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    assert_eq!(
        app.session_autopilot(session).1,
        AutopilotView::Cruise,
        "the default Autopilot"
    );
    frame(&mut app, wid);
    let strip = strip_rows(&app, wid);
    // The card's banner is pane row 1: the box is rows 1..=4 (the line's),
    // blank rows 0 and 5 its air.
    let (top, text_col) = (1usize, 11usize);
    let (under, over) = card_marks(&app, wid);
    assert!(!under.is_empty(), "the border and the trail");
    assert!(!over.is_empty(), "the kitty and the stars");
    let layers = card_layers(&app, wid);
    let scratch = &app.windows[&wid].input_scratch;
    assert_eq!(layers.len(), 2, "two layers a card");
    for l in &layers {
        assert_eq!(
            scratch.fx_layers.iter().filter(|o| *o == l).count(),
            1,
            "on the frame's layers, once"
        );
    }
    let (_, ch) = app.win_cell_size(wid);
    let grid = grid_origin(&app, wid);
    let grid_top = usize::try_from(grid.1).unwrap();
    let y_px = |row: usize| pixel(grid_top + row * ch);
    for m in &under {
        assert!(
            m.inks.iter().all(|i| i.alpha == 0),
            "additive light on a dark ground"
        );
        assert!(
            m.at.y0 >= y_px(strip + top - 1) && m.at.y1 <= y_px(strip + top + 5),
            "{m:?}: on one of the box's frame rows, or its air's"
        );
    }
    assert!(over.iter().any(is_kitty), "the kitty: opaque art");
    // One origin and one pane, in the two spaces: the under layer in window
    // pixels, the over layer in grid-interior pixels the renderers offset
    // by the grid's origin.
    let (u, o) = (
        card_layer(&app, wid, FxSlot::Under).expect("the under layer"),
        card_layer(&app, wid, FxSlot::Over).expect("the over layer"),
    );
    assert_eq!((u.space, o.space), (FxSpace::Window, FxSpace::Grid));
    assert_eq!((o.offset.0 + grid.0, o.offset.1 + grid.1), u.offset);
    assert_eq!(o.clip.translate(grid.0, grid.1), u.clip);
    // The border half a row into the blank rows over and under the box.
    let border = hull(&under);
    assert_eq!(
        (border.y0, border.y1),
        (
            y_px(strip + top) - pixel(ch / 2),
            y_px(strip + top + 4) + pixel(ch / 2)
        ),
        "the border stands in the box's air"
    );
    // The line.
    let line_row = strip + top + 3;
    let want = expected_line(AutopilotView::Cruise, text_col, 120);
    assert!(
        want.ends_with("\u{b7} autopilot \u{221e} cruise"),
        "the whole line fits at 120 columns: {want:?}"
    );
    let start = text_col + LINE_LEAD;
    assert_eq!(
        glass_text(&app, wid, line_row, start..start + want.chars().count()),
        want
    );
    assert_eq!(
        glass_text(&app, wid, line_row, text_col..start),
        "  ",
        "the star's cell and the gap stay blank cells"
    );
    let scratch = &app.windows[&wid].input_scratch;
    let blank = scratch.implicit_blank;
    let e = &scratch.cells[line_row][start];
    assert_eq!(e.ch, 'e');
    assert_eq!(
        e.fg,
        crate::claude_footer::value_fg(GRAY, blank.fg),
        "the footer's value colour under the ink"
    );
    let inked: Vec<u16> = scratch
        .ink
        .iter()
        .filter(|c| usize::from(c.row) == line_row)
        .map(|c| c.col)
        .collect();
    assert_eq!(
        inked,
        (start..start + want.chars().count())
            .map(|c| c as u16)
            .collect::<Vec<_>>(),
        "every letter inked"
    );
    assert!(
        scratch
            .ink
            .windows(2)
            .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col)),
        "the ink stays sorted and unique"
    );
    let star = scratch
        .word_decorations
        .iter()
        .find(|d| usize::from(d.row) == line_row)
        .expect("the star");
    assert_eq!(
        (usize::from(star.col), star.glyph, star.blend),
        (text_col, DecoGlyph::Star4, DecoBlend::Add)
    );
    host.shutdown_and_join();
}

/// THE CARD'S GLASS IS THE RESIDENT PET'S OCCUPIED GROUND (owner-eye-6,
/// 2026-10-20). The border, the trail, the kitty and the stars are glass over
/// cells the grid reads as blank, so the pet's world could not see them: sent
/// to clean ground off Claude's composer, the cat took the blank corner under
/// the mascot, inside the border. The footer's splice now reserves the card's
/// box — its rows, its air on either side and every column to the border's —
/// for the pet (`crate::pet_chrome`), and nothing past it: the border's own
/// column is reserved on every row of the box and its air, the column after
/// it on none.
///
/// NEGATIVE CONTROLS (as run, each failing this test): the same card drawn
/// bare (no claim) reserves nothing there (in-test); the box one column
/// narrow (`lay.cols` columns, the border's left out), the border's column
/// is the pet's; one column wide, the column past the border is reserved.
#[test]
fn the_cards_glass_is_the_pets_occupied_ground() {
    let covered = |app: &App, wid: WindowId, row: usize, col: usize| {
        let (r, c) = (row as f32 + 0.5, col as f32 + 0.5);
        app.windows[&wid].pet_chrome.held().iter().any(|rect| {
            rect.row <= r && r < rect.row + rect.rows && rect.col <= c && c < rect.col + rect.cols
        })
    };
    // The footer's splice and the menu's, which ends the pet's pass.
    let pass = |app: &mut App, wid: WindowId| {
        frame(app, wid);
        app.splice_autopilot_menu(wid);
    };
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    pass(&mut app, wid);
    assert!(
        app.windows[&wid]
            .claude_card
            .entries
            .iter()
            .any(|e| e.shown),
        "fixture: the wrapped card is on the glass"
    );
    // The banner is pane row 1: the box is rows 1..=4, its air rows 0 and 5.
    for row in 0..=5 {
        for col in [0, 5, 11, 40] {
            assert!(
                covered(&app, wid, row, col),
                "pane cell ({row}, {col}) is under the card's glass"
            );
        }
    }
    assert!(
        !covered(&app, wid, 6, 40),
        "the row under the card's air is the pane's own"
    );
    // The border is drawn in the box's last column (`CardScene::cols`).
    let border = app.windows[&wid]
        .claude_card
        .entries
        .iter()
        .find(|e| e.shown)
        .map(|e| usize::from(e.place.3))
        .expect("the card on show");
    assert!(
        (41..119).contains(&border),
        "fixture: the border stands inside the pane, past column 40: {border}"
    );
    for row in 0..=5 {
        assert!(
            covered(&app, wid, row, border),
            "pane cell ({row}, {border}), the border's column, is under the card's glass"
        );
        assert!(
            !covered(&app, wid, row, border + 1),
            "pane cell ({row}, {}), past the border, is the pane's own",
            border + 1
        );
    }
    // Negative control: drawn bare, the card's cells are the pane's.
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    );
    pass(&mut app, wid);
    assert!(
        !app.windows[&wid]
            .claude_card
            .entries
            .iter()
            .any(|e| e.shown),
        "fixture: the card is bare"
    );
    assert!(
        !covered(&app, wid, 0, 40) && !covered(&app, wid, 3, 5),
        "a bare card reserves nothing"
    );
    host.shutdown_and_join();
}

/// THE WORKSPACE CARD (Claude Code 2.1.302 on `--effort workspace`, the
/// measured screen): the mascot holds its wand `╱✦` between its blocks and
/// the banner, and the card's text sits at column 13 — it wears the border,
/// the kitty and the line all the same, the line under the card at ITS text
/// column, the vendor's glyphs untouched. CONTROL (mutation-checked): a
/// reader that takes the mascot's columns as block glyphs alone leaves this
/// card bare, as the Together run of 2026-10-16 saw it (defect D1: Claude's
/// tab showed only the vendor's banner for the whole run).
#[test]
fn the_workspace_card_wears_the_card_at_its_own_text_column() {
    let text =
        include_str!("../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-120col.txt");
    let screen: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
    let wand =
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}\u{2584}\u{2571}\u{2726}  ";
    assert_eq!(
        screen[0],
        format!("{wand}{}2.1.302", banner()),
        "the measured banner row"
    );
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 24, 120));
    publish(&app, session, "2.1.302");
    feed(&app, session, &screen);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let strip = strip_rows(&app, wid);
    let (top, text_col) = (0usize, 13usize);
    let (under, over) = card_marks(&app, wid);
    assert!(!under.is_empty(), "the border and the trail");
    assert!(over.iter().any(is_kitty), "the kitty: opaque art");
    let line_row = strip + top + 3;
    let want = expected_line(AutopilotView::Cruise, text_col, 120);
    let start = text_col + LINE_LEAD;
    assert_eq!(
        glass_text(&app, wid, line_row, start..start + want.chars().count()),
        want,
        "the line under the card at its text column"
    );
    assert_eq!(
        glass_text(&app, wid, strip + top, 0..screen[0].chars().count()),
        screen[0],
        "the vendor's banner row, the wand in it, untouched"
    );
    host.shutdown_and_join();
}

/// A frame of Claude Code 2.1.302's fullscreen entrance, the measured JUMP
/// (`aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-*`,
/// one launch, its screen read every ~50 ms, 144x40): the mascot standing,
/// a row low in its dust (twice), both arms up, and at rest with the wand.
fn jump(pose: &str) -> Vec<String> {
    let text = match pose {
        "1-stand" => include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-1-stand-144col.txt"
        ),
        "2-dust" => include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-2-dust-144col.txt"
        ),
        "3-dust" => include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-3-dust-144col.txt"
        ),
        "4-arms-up" => include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-4-arms-up-144col.txt"
        ),
        "5-rest" => include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-5-rest-144col.txt"
        ),
        other => panic!("no such frame: {other}"),
    };
    text.lines().skip(1).map(str::to_owned).collect()
}

/// The jump's frames in the order the screen drew them.
const JUMP: [&str; 10] = [
    "1-stand",
    "2-dust",
    "3-dust",
    "4-arms-up",
    "1-stand",
    "2-dust",
    "3-dust",
    "4-arms-up",
    "1-stand",
    "5-rest",
];

/// A headless window 144x40 showing the entrance's frame `pose`, the facts
/// of a 2.1.302 process published and its session supervised under Cruise.
fn entrance_glass(pose: &str) -> (App, WindowId, u64, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 40, 144));
    publish(&app, session, "2.1.302");
    feed(&app, session, &jump(pose));
    let host = supervise(&mut app, session);
    (app, wid, session, host)
}

/// Where the card decorated on `wid`'s last frame sits, `None` when no card
/// is on the glass.
fn decorated(app: &App, wid: WindowId) -> Option<CardPlace> {
    let on = app.windows[&wid]
        .claude_card
        .entries
        .iter()
        .any(|e| e.shown)
        && !marks(app, wid, FxSlot::Under).is_empty();
    on.then(|| laid_key(app, wid).place)
}

/// THE CARD STAYS ON THE GLASS THROUGH THE VENDOR'S ENTRANCE. Claude Code
/// 2.1.302's fullscreen renderer plays its mascot's entrance once per build
/// (the measured jump: stand, dust, dust, arms up, twice; stand; the wand at
/// rest), and the card found on the standing frame is decorated on every
/// frame after it — at its place through the dust and the raised arms, which
/// the reader places nothing on, held by its words — and at the rest's place
/// once the wand is up. A frame after each that changed nothing reads no
/// card: a held reading is kept like any other (`CardReading::holds`).
/// CONTROLS: the dust and arms-up frames alone, on a fresh window, draw no
/// card. Mutation-checked: a `read_card` that never holds leaves the six
/// frames of dust and arms bare, the blink main shows in the lab.
#[test]
fn the_card_stays_on_the_glass_through_the_entrance() {
    let (mut app, wid, session, host) = entrance_glass("1-stand");
    let mut shown = Vec::new();
    for pose in JUMP {
        feed(&app, session, &jump(pose));
        frame(&mut app, wid);
        let place = decorated(&app, wid);
        shown.push(place);
        let _ = crate::work_counts::take_card_reads();
        frame(&mut app, wid);
        assert_eq!(
            (crate::work_counts::take_card_reads(), decorated(&app, wid)),
            (0, place),
            "{pose}: the frame after, nothing changed"
        );
    }
    let stand = aterm_phase::launch_card_place(&jump("1-stand")).expect("standing");
    let rest = aterm_phase::launch_card_place(&jump("5-rest")).expect("at rest");
    assert_eq!((stand.top, stand.mascot_end, rest.mascot_end), (1, 9, 11));
    let mut want = vec![Some(stand); 9];
    want.push(Some(rest));
    assert_eq!(shown, want, "the card on every frame of the entrance");
    host.shutdown_and_join();
    for pose in ["2-dust", "3-dust", "4-arms-up"] {
        let (mut app, wid, _, host) = entrance_glass(pose);
        frame(&mut app, wid);
        assert_eq!(decorated(&app, wid), None, "CONTROL: {pose} alone");
        host.shutdown_and_join();
    }
}

/// A HELD CARD GOES ON THE FRAME ITS BANNER LEAVES, as the window holds it.
/// Held through the raised arms, it goes when the screen is then cleared:
/// the held reading is keyed to the card's own row, so a change there reads
/// anew. Only the LAST reading's card is held: with the card switched off
/// for two frames — the screen cleared, then the raised arms — the card that
/// went unseen is not held again when the card is back on. And a resize lets
/// it go: its place was read on the old grid. CONTROLS: the standing frame
/// decorates and the arms hold it in each run, and with the card switched
/// off for the arms alone the card is held when it is back on.
/// Mutation-checked: a held reading keyed to the reader's own banner (none,
/// for the raised arms) keeps the card on the cleared screen; an earlier
/// reading's card held as the last one's keeps it on the arms after the
/// unseen clear; a pane's text kept across a resize keeps it on the resized
/// arms.
#[test]
fn a_held_card_goes_on_the_frame_its_banner_leaves() {
    let blank = vec![String::new(); 40];
    let switch = |app: &mut App, on: bool| {
        app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
            card: Some(on),
            ..Default::default()
        });
    };
    let (mut app, wid, session, host) = entrance_glass("1-stand");
    frame(&mut app, wid);
    let stand = decorated(&app, wid).expect("CONTROL: the standing card");
    feed(&app, session, &jump("4-arms-up"));
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), Some(stand), "CONTROL: held");
    feed(&app, session, &blank);
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), None, "cleared after the hold");
    host.shutdown_and_join();

    for (screens, want) in [
        (vec![jump("4-arms-up")], Some(stand)),
        (vec![blank, jump("4-arms-up")], None),
    ] {
        let (mut app, wid, session, host) = entrance_glass("1-stand");
        frame(&mut app, wid);
        assert_eq!(
            decorated(&app, wid),
            Some(stand),
            "CONTROL: the standing card"
        );
        switch(&mut app, false);
        for rows in &screens {
            feed(&app, session, rows);
            frame(&mut app, wid);
            assert_eq!(decorated(&app, wid), None, "switched off");
        }
        switch(&mut app, true);
        frame(&mut app, wid);
        assert_eq!(
            decorated(&app, wid),
            want,
            "back on after {} unseen frames",
            screens.len()
        );
        host.shutdown_and_join();
    }

    let (mut app, wid, session, host) = entrance_glass("1-stand");
    frame(&mut app, wid);
    assert_eq!(
        decorated(&app, wid),
        Some(stand),
        "CONTROL: the standing card"
    );
    assert!(app.apply_term_resize(wid, 40, 120));
    feed(&app, session, &jump("4-arms-up"));
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), None, "the arms after a resize");
    host.shutdown_and_join();
}

/// LAND wears the sunset (red → gold → rose: no light bluer than it is red)
/// and says so on its line. CONTROL: Cruise's rainbow has blue in it.
#[test]
fn land_wears_the_sunset_and_says_land() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    let blue = |m: &Mark| {
        m.inks
            .iter()
            .any(|i| (i.color & 0xff) > ((i.color >> 16) & 0xff))
    };
    frame(&mut app, wid);
    assert!(
        marks(&app, wid, FxSlot::Under).iter().any(blue),
        "Cruise: the spectrum"
    );
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Land),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    let under = marks(&app, wid, FxSlot::Under);
    assert!(!under.is_empty());
    assert!(
        !under.iter().any(blue),
        "Land: a sunset, never bluer than red"
    );
    let strip = strip_rows(&app, wid);
    let want = expected_line(AutopilotView::Land, 11, 120);
    assert!(want.ends_with("\u{b7} autopilot \u{2913} land"), "{want:?}");
    assert_eq!(
        glass_text(&app, wid, strip + 4, 13..13 + want.chars().count()),
        want
    );
    host.shutdown_and_join();
}

/// A LANDING ON THE CARD'S LINE: under Land the task's face told `landed`
/// ends the line `· autopilot ✓ landed`; theirs, open and unknown keep the
/// mode's own (`⤓ land`). Cruise takes nothing as the end: every face keeps
/// `∞ cruise`. The words are made once per pair (a frame formats nothing),
/// the narrower rungs say no mode, and a face's move re-lays the line and
/// ARMS NO EPISODE. NEGATIVE CONTROL: Cruise, Off, and no harness, say no
/// end whatever is told.
#[test]
fn a_landing_is_said_on_the_cards_line_under_land_alone() {
    use aterm_agent::harness::acts::TaskFace;
    for view in AutopilotView::ALL {
        let faces = std::iter::once(None).chain(TaskFace::ALL.into_iter().map(Some));
        for task in faces {
            let want = match (view, task) {
                (AutopilotView::Land, Some(TaskFace::Landed)) => LineEnd::Landed,
                _ => LineEnd::Mode,
            };
            assert_eq!(LineEnd::of(view, task), want, "{view:?} {task:?}");
        }
    }
    for (end, tail) in [
        (LineEnd::Mode, "\u{2913} land"),
        (LineEnd::Landed, "\u{2713} landed"),
    ] {
        let texts = line_texts(AutopilotView::Land, end);
        assert!(
            texts[0].ends_with(&format!("\u{b7} autopilot {tail}")),
            "{texts:?}"
        );
        assert!(
            std::ptr::eq(texts, line_texts(AutopilotView::Land, end)),
            "made once"
        );
        assert_eq!(
            texts[1..],
            line_texts(AutopilotView::Land, LineEnd::Mode)[1..],
            "the narrower rungs"
        );
        let cruise = line_texts(AutopilotView::Cruise, end);
        assert!(
            cruise[0].ends_with("\u{b7} autopilot \u{221e} cruise"),
            "Cruise says its mode: {cruise:?}"
        );
        for view in [
            AutopilotView::Cruise,
            AutopilotView::Off,
            AutopilotView::MasterOff,
        ] {
            assert_eq!(
                line_texts(view, end),
                line_texts(view, LineEnd::Mode),
                "NEGATIVE CONTROL: {view:?} says no end"
            );
        }
    }
    let fits = |end: LineEnd| {
        line_texts(AutopilotView::Land, end)
            .iter()
            .find(|t| 11 + LINE_LEAD + text_cells(t) < 120)
            .expect("a line fits")
            .clone()
    };
    for word in [Autopilot::Land, Autopilot::Cruise] {
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
        let sid = app
            .pool
            .get(session)
            .unwrap()
            .ctx
            .self_id
            .as_str()
            .to_string();
        assert!(app.set_session_autopilot(session, Some(word), crate::autopilot::Surface::Chip));
        let mut t = Instant::now();
        set_now(Some(t));
        frame(&mut app, wid);
        t += Duration::from_millis(EPISODE_MS + 1000);
        set_now(Some(t));
        frame(&mut app, wid);
        assert_eq!(app.claude_card_deadline(t), None, "settled");
        let strip = strip_rows(&app, wid);
        for (face, end) in [
            (Some(TaskFace::Landed), LineEnd::Landed),
            (Some(TaskFace::Theirs), LineEnd::Mode),
            (Some(TaskFace::Open), LineEnd::Mode),
            (Some(TaskFace::Landed), LineEnd::Landed),
            (None, LineEnd::Mode),
        ] {
            app.apply_harness_fact(&sid, crate::HarnessFact::Task(face));
            frame(&mut app, wid);
            let want = match (word, end) {
                (Autopilot::Land, _) => fits(end),
                _ => expected_line(AutopilotView::Cruise, 11, 120),
            };
            assert_eq!(
                glass_text(&app, wid, strip + 4, 13..13 + want.chars().count()),
                want,
                "{word:?}: {face:?}"
            );
            assert_eq!(
                app.claude_card_deadline(t),
                None,
                "{word:?}: {face:?}: a face's move arms no episode"
            );
        }
        set_now(None);
        host.shutdown_and_join();
    }
}

/// A timer's face, `18:00` on the day it was set or (`other_day`) on a
/// Saturday.
fn six_pm(other_day: bool) -> crate::claude_lights::LandAtGlass {
    crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: other_day.then_some(crate::claude_lights::Weekday::Sat),
    }
}

/// THE LINE FOLLOWS AN ARMED TIMER (§10.6.1): under Cruise with a timer armed
/// the line's widest words end `· autopilot ∞ cruise ⤓18:00` — `⤓Sat 18:00`
/// for a timer on another day, as the title writes it — then narrow to
/// Cruise's own words, then as before. The words are made once per face of
/// the timer (a frame formats nothing). CONTROL: no timer, Land, and a
/// landing's end keep their own words (`LineEnd::timed`).
#[test]
fn the_cards_line_follows_an_armed_timer() {
    let cruise = line_texts(AutopilotView::Cruise, LineEnd::Mode);
    for (other_day, tail) in [(false, "\u{2913}18:00"), (true, "\u{2913}Sat 18:00")] {
        let at = six_pm(other_day);
        let end = LineEnd::Mode.timed(AutopilotView::Cruise, Some(at));
        assert_eq!(end, LineEnd::CruiseUntil(at));
        let texts = line_texts(AutopilotView::Cruise, end);
        assert_eq!(
            texts[0],
            format!("{} {tail}", cruise[0]),
            "the timer's time after Cruise's words"
        );
        assert!(
            texts[0].ends_with(&format!("\u{b7} autopilot \u{221e} cruise {tail}")),
            "{texts:?}"
        );
        assert_eq!(texts[1..], cruise[..], "then Cruise's own rungs");
        assert!(
            std::ptr::eq(texts, line_texts(AutopilotView::Cruise, end)),
            "made once"
        );
        // Against words made here, not the process's cache: a timed end
        // under Land must not leave its time in Land's words for every later
        // line (it did when this was the first Land line asked for).
        assert_eq!(
            line_texts(AutopilotView::Land, end),
            make_line_texts(AutopilotView::Land, LineEnd::Mode),
            "the timer is Cruise's"
        );
    }
    assert_ne!(
        line_texts(AutopilotView::Cruise, LineEnd::CruiseUntil(six_pm(false)))[0],
        line_texts(AutopilotView::Cruise, LineEnd::CruiseUntil(six_pm(true)))[0],
        "another face, other words"
    );
    let at = Some(six_pm(false));
    assert_eq!(
        LineEnd::Mode.timed(AutopilotView::Cruise, None),
        LineEnd::Mode,
        "CONTROL: no timer"
    );
    for (end, view) in [
        (LineEnd::Mode, AutopilotView::Land),
        (LineEnd::Landed, AutopilotView::Land),
        (LineEnd::Mode, AutopilotView::Off),
    ] {
        assert_eq!(end.timed(view, at), end, "CONTROL: {view:?} {end:?}");
    }

    // Agy starts untimed; Muse starts timed. Timed text must not occupy
    // the untimed mode slot, and changing a timer's face changes its words.
    for (rows_kind, agent, timed_first) in [(4, "agy", false), (1, "muse", true)] {
        let first = timed_first.then(|| {
            line_texts_for(
                rows_kind,
                AutopilotView::Cruise,
                LineEnd::CruiseUntil(six_pm(false)),
            )
        });
        let cruise = line_texts_for(rows_kind, AutopilotView::Cruise, LineEnd::Mode);
        assert!(cruise[0].contains(&format!(" \u{b7} {agent} \u{b7} autopilot")));
        assert!(
            !cruise[0].contains('\u{2913}'),
            "an untimed line stays untimed"
        );
        let mut timed = Vec::new();
        for (other_day, tail) in [(false, "\u{2913}18:00"), (true, "\u{2913}Sat 18:00")] {
            let end = LineEnd::CruiseUntil(six_pm(other_day));
            let texts = line_texts_for(rows_kind, AutopilotView::Cruise, end);
            assert_eq!(texts[0], format!("{} {tail}", cruise[0]));
            assert_eq!(texts[1..], cruise[..], "then the untimed rungs");
            assert!(std::ptr::eq(
                texts,
                line_texts_for(rows_kind, AutopilotView::Cruise, end),
            ));
            assert_eq!(
                line_texts_for(rows_kind, AutopilotView::Land, end),
                make_line_texts_for(Some(agent), AutopilotView::Land, LineEnd::Mode),
                "the timer is Cruise's",
            );
            timed.push(texts);
        }
        assert_ne!(timed[0][0], timed[1][0], "each face keeps its own words");
        if let Some(first) = first {
            assert!(
                std::ptr::eq(first, timed[0]),
                "the timed-first face is reused"
            );
        }
        assert!(std::ptr::eq(
            cruise,
            line_texts_for(rows_kind, AutopilotView::Cruise, LineEnd::Mode),
        ));
        assert!(
            !cruise[0].contains('\u{2913}'),
            "cleared timer, untimed words"
        );
    }
}

/// THE TIMER ON THE GLASS: a settled Cruise card whose session arms a timer
/// for 18:00 says `⤓18:00` at the end of its line on the next frame — laid
/// out once, no episode armed — and every frame after it replays the
/// settled frame and its line as they are, laying out nothing and inking
/// nothing; the timer gone, the line says Cruise's own words again.
/// CONTROL: the same card with no timer never says a time.
#[test]
fn a_cruise_cards_line_says_its_armed_timer() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let mut t = Instant::now();
    set_now(Some(t));
    frame(&mut app, wid);
    t += Duration::from_millis(EPISODE_MS + 1000);
    set_now(Some(t));
    frame(&mut app, wid);
    assert_eq!(app.claude_card_deadline(t), None, "settled");
    let line_row = strip_rows(&app, wid) + 4;
    let start = 11 + LINE_LEAD;
    let read = |app: &App, words: &str| {
        glass_text(app, wid, line_row, start..start + words.chars().count())
    };
    let plain = expected_line(AutopilotView::Cruise, 11, 120);
    assert_eq!(read(&app, &plain), plain, "CONTROL: no timer, no time");
    assert!(!glass_text(&app, wid, line_row, 0..120).contains('\u{2913}'));
    // Armed for 18:00 UTC, set at noon.
    let day_ms = 20_000 * 86_400_000_i64;
    let wall = day_ms + 12 * 3_600_000;
    let utc = |_: i64| Some(0);
    assert!(
        app.autopilot_timers
            .arm(session, day_ms + 18 * 3_600_000, t, wall, &utc, None)
    );
    let timed = format!("{plain} \u{2913}18:00");
    assert!(
        11 + LINE_LEAD + text_cells(&timed) < 120,
        "the timed line fits at 120 columns: {timed:?}"
    );
    let _ = crate::work_counts::take_card_layouts();
    frame(&mut app, wid);
    assert_eq!(read(&app, &timed), timed, "the timer's time on the line");
    assert_eq!(crate::work_counts::take_card_layouts(), 1, "laid out once");
    assert_eq!(
        app.claude_card_deadline(t),
        None,
        "a timer's move arms no episode"
    );
    let layers = card_layers(&app, wid);
    let row: Vec<RenderCell> = app.windows[&wid].input_scratch.cells[line_row].clone();
    let _ = crate::work_counts::take_card_line_inks();
    let _ = crate::work_counts::take_card_ticks();
    for i in 0..20u64 {
        set_now(Some(t + Duration::from_millis(1 + i * 7)));
        frame(&mut app, wid);
        assert_eq!(
            card_layers(&app, wid),
            layers,
            "the same contents, replayed: frame {i}"
        );
        assert_eq!(
            app.windows[&wid].input_scratch.cells[line_row], row,
            "frame {i}"
        );
    }
    assert_eq!(
        crate::work_counts::take_card_layouts(),
        0,
        "nothing laid out"
    );
    assert_eq!(
        crate::work_counts::take_card_line_inks(),
        0,
        "nothing inked"
    );
    assert_eq!(
        crate::work_counts::take_card_ticks(),
        0,
        "the settled frame, replayed"
    );
    // Cancelled: Cruise's own words again.
    assert!(app.autopilot_timers.forget(session));
    frame(&mut app, wid);
    assert_eq!(read(&app, &plain), plain, "the timer gone");
    assert!(!glass_text(&app, wid, line_row, 0..120).contains('\u{2913}'));
    set_now(None);
    host.shutdown_and_join();
}

/// One negative control: its name, and what it does to the wrapped card's
/// app and session.
type NegativeCase = (&'static str, fn(&mut App, u64));

/// NOTHING is drawn — no light, no line — for Off, a session this host does
/// not hold, `[claude_footer] card = false`, a card of another build than the
/// process in front, a scrolled-back frame, no host and the master switch
/// off. CONTROL: the wrapped card on the same screen draws.
#[test]
fn off_unsupervised_switched_off_another_build_or_scrolled_back_draw_nothing() {
    let bare = |app: &App, wid: WindowId| {
        let strip = strip_rows(app, wid);
        painted(app, wid).is_empty()
            && !glass_text(app, wid, strip + 4, 0..120).contains("aterm harness")
            && !app.windows[&wid]
                .claude_card
                .entries
                .iter()
                .any(|e| e.shown)
    };
    let cases: [NegativeCase; 7] = [
        ("off", |app, session| {
            assert!(app.set_session_autopilot(
                session,
                Some(Autopilot::Off),
                crate::autopilot::Surface::Chip
            ));
        }),
        ("unsupervised", |app, session| {
            let ctx = app.pool.get(session).unwrap().ctx.clone();
            crate::session_timeline::release_supervisor(
                &ctx,
                None,
                crate::metrics::now_us(),
                aterm_slog::Actor::Ctl,
            );
        }),
        ("switched off", |app, _| {
            app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
                card: Some(false),
                ..Default::default()
            });
        }),
        ("another build", |app, session| {
            publish(app, session, "2.1.282");
        }),
        ("scrolled back", |app, session| {
            let term = app.pool.get(session).unwrap().term.clone();
            let mut term = crate::term_lock(&term);
            term.process(b"\x1b[24;1H");
            for _ in 0..3 {
                term.process(b"\r\n");
            }
            term.scroll_display(2);
            assert!(term.grid().display_offset() > 0);
        }),
        ("no host", |app, _| app.harness = None),
        ("master switch off", |app, _| {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
            );
        }),
    ];
    for (what, case) in cases {
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        assert!(!bare(&app, wid), "{what}: CONTROL, the wrapped card draws");
        case(&mut app, session);
        frame(&mut app, wid);
        assert!(bare(&app, wid), "{what}: the vendor's card bare");
        host.shutdown_and_join();
    }
}

/// The terminal's own text, as `text`/`screen` read it: a fresh extraction
/// of the engine's grid, one string per row.
fn engine_text(app: &App, session: u64) -> Vec<String> {
    let mut fresh = RenderInput::empty();
    let term = app.pool.get(session).unwrap().term.clone();
    crate::term_lock(&term).cell_frame_into(&mut fresh, 24, 120);
    fresh
        .cells
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect::<String>())
        .collect()
}

/// GLASS ONLY: what the terminal holds — the text `text`/`screen` read —
/// never changes, and the frame's undo and strip leave the scratch exactly
/// as the vendor's frame: every cell, and the effect streams without the
/// card's light.
#[test]
fn the_grid_and_the_vendors_frame_are_untouched() {
    let (mut app, wid, session) = glass(120);
    let before = engine_text(&app, session);
    frame(&mut app, wid);
    let vendor = {
        let ws = app.windows.get_mut(&wid).unwrap();
        assert!(crate::claude_footer::undo(
            &mut ws.claude_footer_undo,
            &mut ws.input_scratch
        ));
        ws.input_scratch.cells.clone()
    };
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    assert!(!painted(&app, wid).is_empty());
    assert_ne!(app.windows[&wid].input_scratch.cells, vendor, "the line");
    assert_eq!(engine_text(&app, session), before, "the terminal's text");
    let ws = app.windows.get_mut(&wid).unwrap();
    let ours = ws.claude_card.painted.layers.clone();
    assert_eq!(ours.len(), 2, "the card's two layers");
    assert!(ours.iter().all(|l| ws.input_scratch.fx_layers.contains(l)));
    assert!(crate::claude_footer::undo(
        &mut ws.claude_footer_undo,
        &mut ws.input_scratch
    ));
    strip(&mut ws.claude_card, &mut ws.input_scratch);
    assert_eq!(ws.input_scratch.cells, vendor, "every cell the vendor's");
    assert!(
        ws.input_scratch
            .fx_layers
            .iter()
            .all(|l| ours.iter().all(|o| o.id != l.id)),
        "no layer of the card's left"
    );
    assert!(
        ws.input_scratch
            .word_decorations
            .iter()
            .all(|d| d.glyph != DecoGlyph::Star4 || usize::from(d.col) != 11)
    );
    host.shutdown_and_join();
}

/// C6: A CAPTURE'S RE-SPLICE never doubles the light. A capture re-splices a
/// scratch whose effect streams still hold the last frame's light
/// (`cell_frame_into` never writes them): the splice takes its own back
/// first, and under a tab strip the capture's refill already has — the
/// strip's prepend would move the card's grid-space layer a strip lower, out
/// from under the record. Splicing the same scratch again, and running the
/// whole capture again, bare and under a tab strip, leave every stream as
/// long as once.
#[test]
fn a_capture_resplice_never_doubles_the_light() {
    for tab_strip in [0, 1] {
        capture_resplice(tab_strip);
    }
}

/// [`a_capture_resplice_never_doubles_the_light`] under `tab_strip` strip
/// rows.
fn capture_resplice(tab_strip: u16) {
    let (mut app, wid, session) = glass(120);
    app.tab_strip_rows = tab_strip;
    let host = supervise(&mut app, session);
    let t0 = Instant::now();
    TEST_NOW.with(|now| now.set(Some(t0)));
    let lens = |app: &App| {
        let s = &app.windows[&wid].input_scratch;
        (s.fx_layers.len(), s.ink.len(), s.word_decorations.len())
    };
    frame(&mut app, wid);
    let once = lens(&app);
    assert!(!painted(&app, wid).is_empty());
    let (plan, route) = app.active_visible_frame_layout(wid).unwrap();
    for _ in 0..3 {
        app.splice_claude_footer(wid, &plan, route);
        assert_eq!(
            lens(&app),
            once,
            "strip {tab_strip}: the same scratch, spliced again"
        );
    }
    for _ in 0..3 {
        frame(&mut app, wid);
        assert_eq!(
            lens(&app),
            once,
            "strip {tab_strip}: the whole capture, run again"
        );
    }
    let p = painted(&app, wid);
    let scratch = &app.windows[&wid].input_scratch;
    assert_eq!(p.layers.len(), 2, "the card's two layers");
    for l in &p.layers {
        assert_eq!(
            scratch.fx_layers.iter().filter(|o| o.id == l.id).count(),
            1,
            "strip {tab_strip}: each layer once"
        );
    }
    let strip = strip_rows(&app, wid);
    assert_eq!(strip, usize::from(tab_strip), "CONTROL: the strip's rows");
    assert_eq!(
        glass_text(&app, wid, strip + 4, 0..120)
            .matches("aterm harness")
            .count(),
        1
    );
    TEST_NOW.with(|now| now.set(None));
    host.shutdown_and_join();
}

/// EPISODES, THEN STILLNESS (§3): a card first decorated plays its episode — a
/// frame ahead each step, the repaint key moving every frame — then asks once
/// for its settled frame, and from there its key is constant and no wake is
/// armed, over a thousand idle frames. Focus regained plays it again, the
/// kitty in the seat the glass showed and the trail whole (`harness_card`'s
/// `Ride`, review of slice 5b): no snap back to the trail's start.
/// NEGATIVE CONTROL: the first episode's first frame rides (the kitty left of
/// its seat, no trail yet).
#[test]
fn the_card_settles_to_a_constant_key_and_arms_no_wake() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let set_now = |t: Instant| TEST_NOW.with(|now| now.set(Some(t)));
    set_now(t0);
    assert_eq!(app.claude_card_deadline(t0), None, "nothing on show yet");
    frame(&mut app, wid);
    let riding = card_marks(&app, wid);
    let mut last_fp = app.claude_footer_fp(wid, &plan);
    let mut t = 0;
    while t + FRAME_MS < EPISODE_MS {
        let deadline = app.claude_card_deadline(at(t));
        assert_eq!(deadline, Some(at(t + FRAME_MS)), "a frame ahead at {t} ms");
        t += FRAME_MS;
        set_now(at(t));
        assert_eq!(app.claude_cards_due(at(t)), vec![wid], "{t} ms");
        let fp = app.claude_footer_fp(wid, &plan);
        assert_ne!(
            fp, last_fp,
            "the key moves while the episode plays ({t} ms)"
        );
        frame(&mut app, wid);
        last_fp = app.claude_footer_fp(wid, &plan);
    }
    // The episode runs out: one ask for the settled frame, once.
    let end = at(EPISODE_MS + 1);
    set_now(end);
    assert_eq!(app.claude_card_deadline(end), Some(end));
    assert_ne!(
        app.claude_footer_fp(wid, &plan),
        last_fp,
        "the settled frame is owed"
    );
    assert_eq!(app.claude_cards_due(end), vec![wid]);
    assert_eq!(
        app.claude_card_deadline(end),
        None,
        "asked once: no wake spins on a frame that never comes"
    );
    frame(&mut app, wid);
    let settled = app.claude_footer_fp(wid, &plan);
    let layers = card_layers(&app, wid);
    let settled_pic = card_pic(&app, wid);
    let at_rest = card_marks(&app, wid);
    for i in 0..1000u64 {
        let t = end + Duration::from_millis(1 + i * 7);
        set_now(t);
        assert_eq!(app.claude_footer_fp(wid, &plan), settled, "idle frame {i}");
        assert_eq!(app.claude_card_deadline(t), None, "idle frame {i}");
        assert!(app.claude_cards_due(t).is_empty(), "idle frame {i}");
    }
    for _ in 0..20 {
        frame(&mut app, wid);
        assert_eq!(
            card_layers(&app, wid),
            layers,
            "the same contents, replayed"
        );
        assert_eq!(app.claude_footer_fp(wid, &plan), settled);
    }
    // Focus regained: the episode again, its first frame wanted at once
    // (the last frame drawn is long past).
    let back = end + Duration::from_secs(60);
    set_now(back);
    app.rearm_claude_cards(wid, || back);
    assert!(app.claude_card_deadline(back).is_some_and(|d| d <= back));
    assert_ne!(app.claude_footer_fp(wid, &plan), settled);
    assert_eq!(app.claude_cards_due(back), vec![wid]);
    frame(&mut app, wid);
    assert_eq!(
        app.claude_card_deadline(back),
        Some(back + Duration::from_millis(FRAME_MS)),
        "drawn: the next frame a frame on"
    );
    let replayed = card_marks(&app, wid);
    assert_eq!(
        kitty_x(&replayed),
        kitty_x(&at_rest),
        "the kitty stays seated"
    );
    assert_eq!(
        trail_span(&replayed),
        trail_span(&at_rest),
        "the trail whole"
    );
    assert!(kitty_x(&at_rest).is_some() && trail_span(&at_rest).is_some());
    // CONTROL: the first episode rode in.
    assert!(
        kitty_x(&riding) < kitty_x(&at_rest),
        "the first episode rides"
    );
    assert_ne!(trail_span(&riding), trail_span(&at_rest));
    // NEGATIVE CONTROL: an unfocused window draws the settled frame and arms
    // nothing.
    app.windows.get_mut(&wid).unwrap().focused = false;
    frame(&mut app, wid);
    assert_eq!(app.claude_card_deadline(back), None);
    assert_eq!(card_pic(&app, wid), settled_pic, "the settled frame");
    TEST_NOW.with(|now| now.set(None));
    host.shutdown_and_join();
}

/// LIGHT THEMES (§3: "additive light is invisible on white"). A LIGHT
/// THEME'S CARD IS INK, ITS LINE TEXT ON ITS OWN GROUND: on white and on a
/// cream ground (Gruvbox Light's), every quad of the card is source-over ink,
/// the line's `✦` is opaque ink (on light it was a see-through tint that
/// fell under AA), every letter reads at 4.5:1 or more against the pane's
/// ground, and no letter is solved on its own — the scene's ground and the
/// line's are both the pane's, so the line's inks are the ones the card's
/// tick kept (`harness_card::take_line_ink_solves`). On cream, letters
/// solved for white fall under AA.
#[test]
fn a_light_theme_card_is_ink() {
    use aterm_effects::color_math::relative_luminance;
    let contrast = |a: u32, b: u32| {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    };
    for (osc, bg) in [("ff/ff/ff", 0x00FF_FFFF), ("fb/f1/c7", 0x00FB_F1C7)] {
        let (mut app, wid, session) = glass(120);
        {
            let term = app.pool.get(session).unwrap().term.clone();
            crate::term_lock(&term)
                .process(format!("\x1b]11;rgb:{osc}\x1b\\\x1b]10;rgb:20/20/20\x1b\\").as_bytes());
        }
        let host = supervise(&mut app, session);
        let _ = aterm_effects::harness_card::take_line_ink_solves();
        frame(&mut app, wid);
        let (under, over) = card_marks(&app, wid);
        assert!(!under.is_empty() && !over.is_empty(), "{osc}");
        assert!(
            under
                .iter()
                .chain(&over)
                .all(|m| m.inks.iter().all(|i| i.alpha > 0)),
            "{osc}: ink, never light"
        );
        let p = painted(&app, wid);
        assert!(
            !p.decos.is_empty()
                && p.decos
                    .iter()
                    .all(|d| d.blend == DecoBlend::Over && d.alpha == u8::MAX),
            "{osc}: {:?}",
            p.decos
        );
        assert!(p.ink.len() > 20, "{osc}: the line, {} letters", p.ink.len());
        for c in &p.ink {
            let rgb = pack(c.color);
            assert!(
                contrast(rgb, bg) >= 4.5,
                "{osc}: {rgb:06x} at {:.2}:1",
                contrast(rgb, bg)
            );
        }
        assert_eq!(
            aterm_effects::harness_card::take_line_ink_solves(),
            0,
            "{osc}: letters solved on their own"
        );
        host.shutdown_and_join();
    }
}

/// A NARROW PANE gives up the kitty and its trail first (they need 8 trail
/// cells and four of stars and border inside the pane), keeping the border
/// and the line, which steps down to fit. CONTROL: one column wider, the
/// kitty rides.
#[test]
fn a_narrow_pane_drops_the_kitty_and_keeps_the_border_and_line() {
    let model = "Opus 5.5 with xhigh effort \u{b7} Claude Max";
    let probe = App::headless_for_test();
    let (cw, ch) = probe.win_cell_size(WindowId(0));
    let kc = usize::from(kitty_cols(u16_of(cw), u16_of(ch)));
    assert!(kc > 0, "the headless cell holds the art ({cw}x{ch})");
    // The trail starts two cells past the model row's end.
    let start = 11 + model.chars().count() + 2;
    let narrow = start + TRAIL_MIN + kc + KITTY_MARGIN;
    for (cols, kitty) in [(narrow, false), (narrow + 1, true)] {
        let (mut app, wid, session) = glass(u16_of(cols));
        feed(&app, session, &card_screen(24, cols, model, VERSION));
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        let (under, over) = card_marks(&app, wid);
        assert!(!under.is_empty(), "{cols}: the border");
        assert_eq!(!over.is_empty(), kitty, "{cols}: the kitty and its stars");
        let strip = strip_rows(&app, wid);
        let want = expected_line(AutopilotView::Cruise, 11, cols);
        assert_eq!(
            glass_text(&app, wid, strip + 4, 13..13 + want.chars().count()),
            want,
            "{cols}: the line"
        );
        host.shutdown_and_join();
    }
}

/// A SPLIT: the card goes on its own pane only — every quad inside that
/// pane's pixels, the line inside its columns — and the sibling's cells are
/// untouched.
#[test]
fn a_split_pane_decorates_its_own_card_only() {
    let (mut app, wid, session) = glass(160);
    let _other = app.split_active_stub_tab(wid);
    let (pane_rows, pane_cols) = {
        let term = crate::term_lock(&app.pool.get(session).unwrap().term);
        (usize::from(term.rows()), usize::from(term.cols()))
    };
    assert!(pane_cols < 100, "a half-width pane: {pane_cols}");
    feed(
        &app,
        session,
        &card_screen(pane_rows, pane_cols, MODEL, VERSION),
    );
    frame(&mut app, wid);
    let vendor = app.windows[&wid].input_scratch.cells.clone();
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let (under, over) = card_marks(&app, wid);
    assert!(!under.is_empty() && !over.is_empty());
    let (cw, _) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let right = pixel(pad + pane_cols * cw);
    assert!(under.iter().all(|m| m.at.x1 <= right), "the border");
    assert!(over.iter().all(|m| m.at.x1 <= right), "the kitty and stars");
    let glass = &app.windows[&wid].input_scratch.cells;
    for (r, (now, was)) in glass.iter().zip(&vendor).enumerate() {
        assert_eq!(
            now.get(pane_cols..),
            was.get(pane_cols..),
            "row {r}: the sibling's cells"
        );
    }
    assert!(
        glass.iter().any(|row| row
            .iter()
            .take(pane_cols)
            .map(|c| c.ch)
            .collect::<String>()
            .contains("aterm harness")),
        "the line, inside the pane"
    );
    host.shutdown_and_join();
}

/// A row of cells for the layout tests: `text` one char a cell, in `blank`'s
/// style, `cols` wide.
fn cells_of(text: &str, blank: RenderCell, cols: usize) -> Vec<RenderCell> {
    let mut row: Vec<RenderCell> = text.chars().map(|ch| RenderCell { ch, ..blank }).collect();
    row.resize(cols, blank);
    row
}

/// A scratch of `rows` (no strip), each `cols` wide.
fn scratch_of(rows: &[String], cols: usize) -> RenderInput {
    let mut s = RenderInput::empty();
    s.rows = rows.len();
    s.cols = cols;
    s.cells = rows
        .iter()
        .map(|r| cells_of(r, RenderCell::default(), cols))
        .collect();
    s
}

/// [`scratch_of`]'s rows as one pane, its whole width, in the default blank.
fn cells_on(scratch: &RenderInput) -> PaneCells<'_> {
    PaneCells {
        scratch,
        pane: PaneGeom {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: scratch.rows,
            cols: scratch.cols,
        },
        blank: RenderCell::default(),
    }
}

/// The rows of [`card_screen`] above the composer, the card's banner at row 1.
fn card_rows(cols: usize, model: &str) -> Vec<String> {
    card_screen(12, cols, model, VERSION)[..8].to_vec()
}

/// THE LAYOUT'S RULES, on hand-made cells: the line under the card at its
/// text column (four boxed rows); a busy row under the card puts it on the
/// banner row two cells past the banner (three boxed rows) and the kitty
/// after it; a narrow pane narrows the words, then drops them; a wide last
/// glyph ends a row one cell later (`CardPlace::ends`). The kitty stands
/// three rows tall where the working directory's row is blank from the
/// trail's start to the kitty's margin — a glyph anywhere in that span, at
/// either end of it, keeps it two rows tall, one past it does not (the row
/// is read no further) — and the read is one of the layout's noted tests
/// ([`Seen`]): a glyph put in the span after it fails them. The box has its
/// AIR (§10.6.8) over and under it where the pane rows there are blank from
/// column 0 to the border's column: a glyph over the box at either end of
/// that span takes the air over it, one past it does not (the row is read no
/// further, and a glyph there passes the noted tests); none over the pane's
/// first row, where no row over it is read; none under it over a notice or
/// at the pane's last row; and none under a three-row box, whose row under
/// it the line's test read and is not read again.
#[test]
fn the_layout_follows_the_designs_rules() {
    let texts = vec!["w".repeat(40), "v".repeat(20)];
    // A scan with a visible agy banner but no complete card still owns
    // five rows. Completing only its path or feet must invalidate absence.
    let agy = vec![
        "      ▄▀▀▄        Antigravity CLI 1.4.0".to_string(),
        "     ▀▀▀▀▀▀       user@example.test (Google AI Ultra)".to_string(),
        "    ▀▀▀▀▀▀▀▀      Gemini 4 Argon (Medium)".to_string(),
        "   ▄▀▀    ▀▀▄     ~/aterm".to_string(),
        "  ▄▀▀      ▀▀▄".to_string(),
        String::new(),
    ];
    for missing in [3, 4] {
        let mut incomplete = agy.clone();
        incomplete[missing].clear();
        let reading = read_card(7, &incomplete, None);
        assert!(reading.read.is_none());
        assert!(
            !reading.holds(7, &agy, &[missing]),
            "row {missing} completes the card"
        );
        let complete = read_card(7, &agy, None);
        let card = complete.read.as_ref().expect("the completed agy card");
        assert_eq!((card.place.rows, &*card.version), (4, "1.4.0"));
        assert!(complete.holds(7, &agy, &[]));
        let mut under = agy.clone();
        under[5] = "ordinary output under the card".to_string();
        assert!(
            complete.holds(7, &under, &[5]),
            "unrelated output keeps the reading"
        );
    }
    // Claude's scanner has priority even when its newly drawn banner is
    // above an existing agy card, outside that card's own changed rows.
    let mut lower = vec![String::new(); 8];
    lower.extend(agy.clone());
    let reading = read_card(7, &lower, None);
    assert_eq!(reading.read.as_ref().expect("agy").place.rows, 4);
    let claude = card_rows(120, MODEL);
    let mut both = lower;
    both[..claude.len()].clone_from_slice(&claude);
    assert!(!reading.holds(7, &both, &[1, 2, 3]));
    let preferred = read_card(7, &both, None);
    let card = preferred.read.as_ref().expect("Claude remains preferred");
    assert_eq!((card.place.rows, &*card.version), (3, VERSION));
    let lay = |rows: &[String], cols: usize, kc: usize| {
        let s = scratch_of(rows, cols);
        let place = aterm_phase::launch_card_place(rows).expect("a card");
        let pane = PaneGeom {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: rows.len(),
            cols,
        };
        let cells = PaneCells {
            scratch: &s,
            pane,
            blank: RenderCell::default(),
        };
        layout(&cells, &place, kc, &texts, &mut Seen::default())
    };
    let model_end = 11 + MODEL.chars().count();
    // Room for everything: the line under the card, the kitty on the trail.
    let rows = card_rows(120, MODEL);
    let l = lay(&rows, 120, 6).expect("laid out");
    assert_eq!(l.rows, 4);
    assert_eq!(l.air, (true, true), "blank rows over and under the box");
    assert_eq!(
        l.line,
        Some(LinePlace {
            row: 4,
            col: 11,
            text: &texts[0],
            cells: 40
        })
    );
    let start = model_end + 2;
    assert_eq!(l.trail, Some((start, start + TRAIL_MAX)));
    assert_eq!(l.kitty_col, Some(start + TRAIL_MAX));
    assert_eq!(
        l.cols,
        (start + TRAIL_MAX + 6 + KITTY_MARGIN).max(11 + 2 + 40)
    );
    assert!(l.cols <= 119);
    // The tall kitty: `~/aterm` ends long before the trail starts.
    assert_eq!(l.kitty_rows, 3, "a short path: three rows tall");
    let margin_end = start + TRAIL_MAX + 6 + KITTY_MARGIN;
    let path_row = |at: usize| {
        let mut r = rows.clone();
        let head: String = r[3].chars().collect();
        let pad = at - head.chars().count();
        r[3] = format!("{head}{}x", " ".repeat(pad));
        r
    };
    for (at, tall) in [
        (start - 1, 3),
        (start, 2),
        (start + TRAIL_MAX / 2, 2),
        (start + TRAIL_MAX + 3, 2),
        (margin_end - 1, 2),
        (margin_end, 3),
    ] {
        let l = lay(&path_row(at), 120, 6).expect("laid out");
        assert!(l.kitty_col.is_some(), "a glyph at {at}: the kitty stays");
        assert_eq!(l.kitty_rows, tall, "a glyph on the path row at {at}");
    }
    {
        let s = scratch_of(&rows, 120);
        let place = aterm_phase::launch_card_place(&rows).expect("a card");
        let pane = PaneGeom {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: rows.len(),
            cols: 120,
        };
        let cells = PaneCells {
            scratch: &s,
            pane,
            blank: RenderCell::default(),
        };
        let mut seen = Seen::default();
        let l = layout(&cells, &place, 6, &texts, &mut seen).expect("laid out");
        assert_eq!(l.kitty_rows, 3);
        assert!(
            seen.0.contains(&Probe::Blanks {
                row: 3,
                from: start,
                to: margin_end,
                len: margin_end - start
            }),
            "the path row's read, noted: {:?}",
            seen.0
        );
        assert!(seen.hold(&cells), "the same cells hold");
        let moved = scratch_of(&path_row(margin_end - 1), 120);
        let cells = PaneCells {
            scratch: &moved,
            ..cells
        };
        assert!(
            !seen.hold(&cells),
            "CONTROL: a glyph in the span fails them"
        );
        let past = scratch_of(&path_row(margin_end), 120);
        let cells = PaneCells {
            scratch: &past,
            ..cells
        };
        assert!(seen.hold(&cells), "one past the span: never read");
    }
    // The air: the rows over and under the box, each read once from column
    // 0 to the border's column, noted.
    let span = l.cols + 1;
    let over = |at: usize| {
        let mut r = rows.clone();
        r[0] = format!("{}x", " ".repeat(at));
        r
    };
    for (at, air) in [(0, false), (l.cols, false), (span, true)] {
        let l = lay(&over(at), 120, 6).expect("laid out");
        assert_eq!(l.air, (air, true), "a glyph over the box at {at}");
    }
    {
        let s = scratch_of(&rows, 120);
        let place = aterm_phase::launch_card_place(&rows).expect("a card");
        let cells = cells_on(&s);
        let mut seen = Seen::default();
        layout(&cells, &place, 6, &texts, &mut seen).expect("laid out");
        for row in [0, 5] {
            let read = Probe::Blanks {
                row,
                from: 0,
                to: span,
                len: span,
            };
            assert!(seen.0.contains(&read), "row {row} read: {:?}", seen.0);
        }
        for (at, holds) in [(l.cols, false), (span, true)] {
            let moved = scratch_of(&over(at), 120);
            assert_eq!(
                seen.hold(&cells_on(&moved)),
                holds,
                "a glyph over the box at {at}"
            );
        }
        // The card on the pane's first row: no row over it, none read.
        let first = rows[1..].to_vec();
        let s = scratch_of(&first, 120);
        let place = aterm_phase::launch_card_place(&first).expect("a card");
        let mut seen = Seen::default();
        let l = layout(&cells_on(&s), &place, 6, &texts, &mut seen).expect("laid out");
        assert_eq!((place.top, l.air), (0, (false, true)));
        let from_0: Vec<usize> = seen
            .0
            .iter()
            .filter_map(|p| match *p {
                Probe::Blanks { row, from: 0, .. } => Some(row),
                _ => None,
            })
            .collect();
        assert_eq!(from_0, [4], "only the row under the box: {:?}", seen.0);
    }
    // A notice under the box, and the line's row the pane's last.
    let mut notice = rows.clone();
    notice[5] = "  Get to finished work sooner with Opus 5.5".to_string();
    assert_eq!(lay(&notice, 120, 6).expect("laid out").air, (true, false));
    let l = lay(&rows[..5], 120, 6).expect("laid out");
    assert_eq!((l.rows, l.air), (4, (true, false)), "nothing under the box");
    // Text under the card: the line goes on the banner row, the box keeps
    // three rows and the kitty rides after the line; the row under the box
    // is the one the line's test read, and is not read again for the air.
    let mut busy = rows.clone();
    busy[4] = "           a message under the card".to_string();
    let l = lay(&busy, 120, 6).expect("laid out");
    assert_eq!(l.rows, 3);
    assert_eq!(l.air, (true, false), "a message under the box");
    {
        let s = scratch_of(&busy, 120);
        let place = aterm_phase::launch_card_place(&busy).expect("a card");
        let mut seen = Seen::default();
        layout(&cells_on(&s), &place, 6, &texts, &mut seen).expect("laid out");
        let under = seen
            .0
            .iter()
            .filter(|p| matches!(p, Probe::Blanks { row: 4, .. }))
            .count();
        assert_eq!(under, 1, "the row under the box read once: {:?}", seen.0);
    }
    let banner_end = 11 + banner().chars().count() + VERSION.len();
    let line = l.line.clone().expect("on the banner row");
    assert_eq!((line.row, line.col), (1, banner_end + 2));
    let after = banner_end + 2 + LINE_LEAD + 40 + 2;
    assert_eq!(l.trail.map(|t| t.0), Some(after));
    // Narrow: the shorter words, then none; no room for the kitty either.
    let l = lay(&card_rows(40, MODEL), 40, 6).expect("laid out");
    assert_eq!(l.line.as_ref().map(|l| l.cells), Some(20));
    assert_eq!((l.kitty_col, l.trail), (None, None));
    assert!(l.cols <= 39);
    let l = lay(&card_rows(30, MODEL), 30, 6).expect("laid out");
    assert_eq!((l.line, l.rows), (None, 3));
    // No kitty art at this cell size: the trail goes with it.
    let l = lay(&rows, 120, 0).expect("laid out");
    assert_eq!((l.kitty_col, l.trail, l.kitty_rows), (None, None, 2));
    // A wide last glyph on the working directory row ends it a cell later.
    let mut wide = rows.clone();
    wide[3] =
        " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/src/github.com/someone/\u{6771}".to_string();
    let mut s = scratch_of(&wide, 120);
    // The engine's cells: the wide glyph at 36, then its continuation.
    s.cells[3].insert(
        37,
        RenderCell {
            wide: true,
            ..RenderCell::default()
        },
    );
    s.cells[3].truncate(120);
    let place = aterm_phase::launch_card_place(&wide).expect("a card");
    assert_eq!(place.ends[2], 37);
    let pane = PaneGeom {
        strip: 0,
        row_off: 0,
        col_off: 0,
        rows: wide.len(),
        cols: 120,
    };
    let cells = PaneCells {
        scratch: &s,
        pane,
        blank: RenderCell::default(),
    };
    let short = vec!["x".to_string()];
    let l = layout(&cells, &place, 0, &short, &mut Seen::default()).expect("laid out");
    assert_eq!(l.cols, 38, "the working directory's wide glyph, whole");
    assert!(38 > banner_end.max(model_end).max(11 + LINE_LEAD + 1));

    // agy's path is its fourth text row; CardPlace keeps only three ends.
    // Drive the actual scanner and layout with a wide final path glyph.
    let agy = vec![
        "      ▄▀▀▄        Antigravity CLI 1.4.0".to_string(),
        "     ▀▀▀▀▀▀       user@example.test (Google AI Ultra)".to_string(),
        "    ▀▀▀▀▀▀▀▀      Gemini 4 Argon (Medium)".to_string(),
        format!("   ▄▀▀    ▀▀▄     ~/{}東", "path/".repeat(12)),
        "  ▄▀▀      ▀▀▄".to_string(),
        String::new(),
    ];
    let place = aterm_phase::agy::launch_card_scan(&agy)
        .1
        .expect("a whole agy card");
    assert_eq!(place.rows, 4);
    let end = agy[3].chars().count();
    assert_eq!(place.ends[2], end);
    let mut s = scratch_of(&agy, 120);
    s.cells[3].insert(
        end,
        RenderCell {
            wide: true,
            ..RenderCell::default()
        },
    );
    s.cells[3].truncate(120);
    let mut seen = Seen::default();
    let l = layout(&cells_on(&s), &place, 0, &short, &mut seen).expect("wide path fits");
    assert_eq!(l.cols, end + 1, "border column clears the continuation");
    assert!(seen.hold(&cells_on(&s)));
    let mut narrow = agy.clone();
    narrow[3].pop();
    narrow[3].push('x');
    let narrow_place = aterm_phase::agy::launch_card_scan(&narrow)
        .1
        .expect("an ASCII path card");
    let ascii = scratch_of(&narrow, 120);
    let ordinary = layout(
        &cells_on(&ascii),
        &narrow_place,
        0,
        &short,
        &mut Seen::default(),
    )
    .expect("ASCII path fits");
    assert_eq!(
        ordinary.cols, end,
        "ASCII control needs no continuation column"
    );
    assert!(
        !seen.hold(&cells_on(&ascii)),
        "a changed fourth-row wide flag invalidates the cache"
    );
    // The continuation occupies the last pane cell: no clear border column
    // remains. The same-width ASCII card still fits without that extra cell.
    s.cols = end + 1;
    for row in &mut s.cells {
        row.truncate(s.cols);
    }
    assert!(layout(&cells_on(&s), &place, 0, &short, &mut Seen::default()).is_none());
    let ascii = scratch_of(&narrow, end + 1);
    assert_eq!(
        layout(
            &cells_on(&ascii),
            &narrow_place,
            0,
            &short,
            &mut Seen::default()
        )
        .expect("ASCII boundary control")
        .cols,
        end
    );
}

/// The pane width a corpus capture was taken at: its name's `-<n>col`, else
/// its provenance line's `(<cols>x<rows>`, else its widest row.
fn capture_cols(name: &str, text: &str, rows: &[String]) -> usize {
    let digits_before = |s: &str, at: usize| {
        let head = &s[..at];
        let from = head
            .rfind(|c: char| !c.is_ascii_digit())
            .map_or(0, |i| i + 1);
        head[from..].parse::<usize>().ok()
    };
    let by_name = name.rfind("col.txt").and_then(|at| digits_before(name, at));
    let by_header = text.lines().next().and_then(|line| {
        let open = line.find(" (")? + 2;
        let tail = &line[open..];
        let x = tail.find('x')?;
        tail[..x].parse::<usize>().ok()
    });
    by_name
        .or(by_header)
        .unwrap_or_else(|| rows.iter().map(|r| text_cells(r)).max().unwrap_or(80))
}

/// THE TALL KITTY OVER THE CORPUS (§10.6.3): every launch card in
/// `aterm-phase`'s fixtures that the reader places (about 150, Claude Code
/// 2.1.302's workspace card among them), laid out at its capture's width,
/// stands the kitty three rows tall exactly where the card's working
/// directory row is blank from the trail's start to the kitty's margin —
/// judged here from the row's text — and two rows tall elsewhere. CONTROL:
/// each card again with its path run on past the trail's start stands the
/// kitty two rows tall, its place and its trail unchanged. THE AIR OVER THE
/// CORPUS (§10.6.8): the box stands its border in the row over it, and in
/// the row under it, exactly where that row is blank across the box's
/// columns, judged from the rows' text — over the fullscreen renderer's
/// cards (row 1, under a blank row 0), never over a card at the pane's
/// first row (every inline capture, after the lab's clear) or under its
/// launch line; under the box unless one of Claude's notices is there.
#[test]
fn every_recorded_card_stands_the_kitty_tall_only_beside_a_blank_path_row() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .map(|e| e.expect("a fixture").path())
        .collect();
    paths.sort();
    let kc = usize::from(kitty_cols(9, 18));
    let texts = line_texts(AutopilotView::Cruise, LineEnd::Mode);
    let lay = |rows: &[String], cols: usize| {
        let s = scratch_of(rows, cols);
        let place = aterm_phase::launch_card_place(rows)?;
        let pane = PaneGeom {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: rows.len(),
            cols,
        };
        let cells = PaneCells {
            scratch: &s,
            pane,
            blank: RenderCell::default(),
        };
        layout(&cells, &place, kc, texts, &mut Seen::default()).map(|l| {
            (
                place,
                l.trail,
                l.kitty_col,
                l.kitty_rows,
                (l.rows, l.cols, l.air),
            )
        })
    };
    let (mut tall, mut short, mut bare, mut workspace) = (0, 0, 0, 0);
    // The air's tallies: over the box (the card at the pane's first row, a
    // blank row over it, text over it), and under it (blank, not).
    let (mut first, mut aired, mut lined, mut under, mut noticed) = (0, 0, 0, 0, 0);
    for path in paths {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let rows = aterm_phase::prompt::fixtures::screen(&text);
        let cols = capture_cols(&name, &text, &rows);
        let Some((place, trail, kitty_col, rows_tall, (boxed, box_cols, air))) = lay(&rows, cols)
        else {
            continue;
        };
        // THE AIR (§10.6.8): over the box where the row over it is blank
        // across the box's columns, under it where the row under it is —
        // judged here from the rows' text.
        let blank = |row: usize| {
            rows[row]
                .chars()
                .chain(std::iter::repeat(' '))
                .take(box_cols + 1)
                .all(|c| c == ' ')
        };
        let below = place.top + boxed;
        let want = (
            place.top > 0 && blank(place.top - 1),
            below < rows.len() && blank(below),
        );
        assert_eq!(air, want, "{name}: the air over and under the box");
        match (place.top, air.0) {
            (0, _) => first += 1,
            (_, true) => aired += 1,
            (_, false) => lined += 1,
        }
        if air.1 {
            under += 1;
        } else {
            noticed += 1;
        }
        let (Some((start, _)), Some(kitty_col)) = (trail, kitty_col) else {
            assert_eq!(rows_tall, 2, "{name}: no kitty");
            bare += 1;
            continue;
        };
        let path_row = place.top + 2;
        let end = kitty_col + kc + KITTY_MARGIN;
        let blank = rows[path_row]
            .chars()
            .chain(std::iter::repeat(' '))
            .skip(start)
            .take(end - start)
            .all(|c| c == ' ');
        assert_eq!(
            rows_tall,
            if blank { 3 } else { 2 },
            "{name}: {:?}",
            rows[path_row]
        );
        if blank {
            tall += 1;
        } else {
            short += 1;
        }
        if name.contains("-workspace-") {
            assert_eq!(rows_tall, 3, "{name}: the workspace card's short path");
            workspace += 1;
        }
        // CONTROL: the path runs on to the kitty's column.
        let mut long = rows.clone();
        let head: String = long[path_row].trim_end().to_string();
        let run = kitty_col.saturating_sub(text_cells(&head)).max(1);
        long[path_row] = format!(
            "{head}{}",
            "/deep".repeat(run).chars().take(run).collect::<String>()
        );
        let (_, trail_long, kitty_long, rows_long, _) =
            lay(&long, cols).unwrap_or_else(|| panic!("{name}: the long path's card"));
        assert_eq!((trail_long, kitty_long), (trail, Some(kitty_col)), "{name}");
        assert_eq!(
            rows_long, 2,
            "{name}: CONTROL: a long path keeps it two rows tall"
        );
    }
    assert!(
        tall + short + bare >= 140 && tall >= 100 && workspace >= 1,
        "the corpus must exercise the layout: {tall} tall, {short} short, {bare} bare, \
         {workspace} workspace"
    );
    assert!(
        first >= 140 && aired >= 70 && lined >= 10 && under >= 190 && noticed >= 40,
        "the corpus must exercise the air: {first} at the first row, {aired} aired over, \
         {lined} under a launch line, {under} aired under, {noticed} over a notice"
    );
}

/// THE TALL KITTY ON THE GLASS: beside the card's short path the scene says
/// three rows and the kitty's opaque art is the tall art — 21 of its square
/// pixels tall, its head where the two-row kitty's is, so at this cell size
/// it reaches past the two-row art's feet; a path run on past the trail's
/// start keeps the two-row art, 14 pixels tall. CONTROL: the two cards
/// differ in the path row alone.
#[test]
fn the_kitty_stands_three_rows_tall_beside_a_short_path() {
    for (long_path, rows_tall, art_h) in [(false, 3u8, 21), (true, 2, 14)] {
        let (mut app, wid, session) = glass(120);
        if long_path {
            let mut screen = card_screen(24, 120, MODEL, VERSION);
            screen[3] = format!(
                "{}/a/working/directory/that/runs/on/past/the/trail",
                screen[3]
            );
            feed(&app, session, &screen);
        }
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        let strip = strip_rows(&app, wid);
        let (_, ch) = app.win_cell_size(wid);
        let p = (ch - (ch / 14).max(1)) / 7;
        assert!(p >= 1, "a kitty at {ch} px");
        let card = card_marks(&app, wid);
        let art = kitty_mark(&card).expect("the kitty");
        // Grid-interior pixels, as the over layer is laid.
        let grid_top = grid_origin(&app, wid).1;
        let (y0, y1) = (
            usize::try_from(art.at.y0 - grid_top).unwrap(),
            usize::try_from(art.at.y1 - grid_top).unwrap(),
        );
        // The card's banner is pane row 1: the head sits half the two-row
        // art above the line between the card's first two rows.
        assert_eq!(
            y0,
            (strip + 2) * ch - 7 * p,
            "long path {long_path}: the head"
        );
        assert_eq!(
            y1 - y0,
            art_h * p,
            "long path {long_path}: the art's height"
        );
        let scene = app.windows[&wid].claude_card.entries[0]
            .laid
            .as_ref()
            .and_then(|(_, laid)| laid.as_ref())
            .map(|(_, scene)| scene.kitty_rows);
        assert_eq!(scene, Some(rows_tall), "long path {long_path}");
        host.shutdown_and_join();
    }
}

/// The switch reads on by default, and its Manual row and default are
/// there (`[claude_footer] card`).
#[test]
fn the_card_switch_is_on_by_default() {
    let config = crate::app_config::Config::default();
    assert!(config.claude_footer_card());
    let off = crate::app_config::Config {
        claude_footer: Some(crate::app_config::ClaudeFooterConfig {
            card: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(!off.claude_footer_card());
    assert!(off.claude_footer_usage(), "the other switch is untouched");
}

/// The strip takes back exactly what was appended, once each, wherever
/// later splices left it — and nothing a refill put there. A stream that
/// holds none of ours — refilled empty (a split's compose clears it), or
/// with another producer's entries off ours' rows (a keystroke's light on
/// its own row) — costs no sort of our keys; the tail as appended costs none
/// either. CONTROL: ours among another's, and a stream with one of
/// ours scrubbed, are sorted for (the counter sees the sort).
#[test]
fn the_strip_removes_only_its_own() {
    let key = |x: &u64| [*x, 0, 0];
    // Each entry on the frame row its value names.
    let row = |x: &u64| u16::try_from(*x).unwrap_or(u16::MAX);
    let mut keys = Vec::new();
    let _ = crate::work_counts::take_card_strip_sorts();
    let sorts = crate::work_counts::take_card_strip_sorts;
    let mut untouched = vec![1, 2, 3, 4, 5];
    remove_each_once(&mut untouched, &[3, 4, 5], &mut keys, key, row);
    assert_eq!(untouched, [1, 2], "the tail, as appended");
    assert_eq!(sorts(), 0, "the tail: no sort");
    let mut stream = vec![1, 2, 3, 9, 4, 5];
    remove_each_once(&mut stream, &[3, 4, 5], &mut keys, key, row);
    assert_eq!(stream, [1, 2, 9], "another producer's after ours");
    assert_eq!(sorts(), 1, "CONTROL: ours among another's, sorted for");
    let mut refilled = vec![7, 8];
    remove_each_once(&mut refilled, &[3, 4], &mut keys, key, row);
    assert_eq!(refilled, [7, 8], "a refill holds none of ours");
    assert_eq!(sorts(), 0, "a refill off ours' rows: no sort");
    let mut below = vec![1, 2, 9];
    remove_each_once(&mut below, &[3, 4, 5], &mut keys, key, row);
    assert_eq!(below, [1, 2, 9], "rows either side of ours");
    assert_eq!(sorts(), 0, "either side of ours' rows: no sort");
    let mut empty: Vec<u64> = Vec::new();
    remove_each_once(&mut empty, &[3, 4], &mut keys, key, row);
    assert!(empty.is_empty());
    assert_eq!(sorts(), 0, "an emptied stream: no sort");
    let mut scrubbed = vec![1, 4, 2];
    remove_each_once(&mut scrubbed, &[3, 4], &mut keys, key, row);
    assert_eq!(scrubbed, [1, 2], "a scrubbed entry does not keep the rest");
    assert_eq!(sorts(), 1, "CONTROL: one of ours left, sorted for");
    let mut twice = vec![4, 4, 4];
    remove_each_once(&mut twice, &[4, 4], &mut keys, key, row);
    assert_eq!(twice, [4], "once each");
}

/// THE TWO PIXEL SPACES the card's layers are laid in: the under layer in
/// window pixels (the border's left side reaching half a cell into the
/// window's padding), the over layer in grid-interior pixels the renderers
/// offset by the grid's origin themselves. A strip row moves the card one
/// row down — its origin a row lower in both layers, every mark exactly
/// one row down; window padding moves the
/// window layer's marks with it and leaves the grid layer's where they
/// were in its own space.
#[test]
fn the_layers_meet_the_renderers_pixel_spaces() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    TEST_NOW.with(|now| now.set(Some(Instant::now())));
    frame(&mut app, wid);
    let (under, over) = card_marks(&app, wid);
    assert!(!under.is_empty() && !over.is_empty());
    let layers = card_layers(&app, wid);
    let (cw, ch) = app.win_cell_size(wid);
    let ch = pixel(ch);
    // A strip row above the grid.
    app.tab_strip_rows = 1;
    frame(&mut app, wid);
    assert_eq!(strip_rows(&app, wid), 1);
    let down = |ms: &[Mark]| ms.iter().map(|m| m.moved(0, ch)).collect::<Vec<_>>();
    let (u1, o1) = card_marks(&app, wid);
    assert_eq!(u1, down(&under), "the border and the trail, a row down");
    assert_eq!(o1, down(&over), "the kitty and the stars, a row down");
    for (now, was) in card_layers(&app, wid).iter().zip(&layers) {
        assert_eq!(
            now.offset,
            (was.offset.0, was.offset.1 + ch),
            "{:?}: the origin a row down",
            now.slot
        );
    }
    // Window padding, left and top.
    app.tab_strip_rows = 0;
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.metrics.pad = 12;
        ws.metrics.pad_top = 8;
    }
    assert_eq!(
        (app.win_pad(wid), app.win_pad_top(wid) + app.win_head(wid)),
        (12, 8)
    );
    frame(&mut app, wid);
    let (u2, o2) = card_marks(&app, wid);
    assert_eq!(
        o2,
        over.iter().map(|m| m.moved(12, 8)).collect::<Vec<_>>(),
        "grid-interior: the renderer adds the padding"
    );
    let (was, now) = (hull(&under), hull(&u2));
    assert_eq!(now.x1, was.x1 + 12);
    assert_eq!(now.y0, was.y0 + 8);
    assert_eq!(now.y1, was.y1 + 8);
    assert_eq!(was.x0, 0, "no padding: the border at the grid's edge");
    assert_eq!(now.x0, 12 - pixel(cw / 2), "half a cell into the padding");
    TEST_NOW.with(|now| now.set(None));
    host.shutdown_and_join();
}

/// SERIOUS MODE (§3): a static border only — no kitty, trail or stars, the
/// box hugging the card's text and its line (its right side in the column
/// after the line) — and no wake. CONTROL: the same card out of serious mode
/// rides its kitty.
#[test]
fn serious_mode_keeps_a_still_border_only() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    frame(&mut app, wid);
    let right = |app: &App| hull(&marks(app, wid, FxSlot::Under)).x1;
    assert!(!marks(&app, wid, FxSlot::Over).is_empty());
    app.serious_mode = true;
    frame(&mut app, wid);
    let (under, over) = card_marks(&app, wid);
    assert!(!under.is_empty(), "the border");
    assert!(over.is_empty(), "no kitty, no stars");
    let (cw, _) = app.win_cell_size(wid);
    let line = expected_line(AutopilotView::Cruise, 11, 120);
    let line_end = 11 + LINE_LEAD + text_cells(&line);
    assert_eq!(
        right(&app),
        pixel((line_end + 1) * cw),
        "the box hugs the text"
    );
    assert_eq!(app.claude_card_deadline(Instant::now()), None, "still");
    let strip = strip_rows(&app, wid);
    assert!(glass_text(&app, wid, strip + 4, 0..120).contains("aterm harness"));
    host.shutdown_and_join();
}

/// Set the card's clock ([`card_now`]) to `t`, or back to the wall clock.
fn set_now(t: Option<Instant>) {
    TEST_NOW.with(|now| now.set(t));
}

/// The clock [`set_now`] holds, else the wall clock: what a measured pass's
/// capture drives its cursor effects with, so a harness window and its bare
/// twin are extracted at one instant, the card's, however long the machine
/// took between them.
fn held_now() -> Instant {
    TEST_NOW
        .with(std::cell::Cell::get)
        .unwrap_or_else(Instant::now)
}

/// THE TAB MENU over the card (`App::splice_tab_menu`, topmost chrome,
/// painted after the card, hanging from the first grid row where a fresh
/// card sits): none of the card's light, ink or star shows on the menu's
/// rectangle — its layers are cut there by the occluder the menu's scrub
/// pushes, painted through it by the CPU renderer's own slicer, and its ink
/// and star are cut from their streams — what lies clear of it stays, pixel
/// for pixel, and the record still matches the glass: framed again with the
/// menu open, each of the card's layers is on the frame exactly once.
/// CONTROL: painted without the occluder, the border, the kitty and its
/// stars light pixels on the rectangle, and before the menu's splice the
/// line's ink and its star (the menu at the card's text) lay under it.
#[test]
fn the_tab_menu_hides_the_card_under_it() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.tab_strip_rows = 1;
    set_now(Some(Instant::now()));
    frame(&mut app, wid);
    let (cw, _) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let kitty = over_col(&app, wid, cw, pad);
    for (anchor, over_kitty) in [(3, false), (u16_of(kitty), true)] {
        assert!(app.open_tab_context_menu(wid, 0, anchor, false));
        frame(&mut app, wid);
        let (layers, ink, decos) = {
            let p = painted(&app, wid);
            (p.layers.clone(), p.ink.clone(), p.decos.clone())
        };
        let (under, over) = card_marks(&app, wid);
        app.splice_tab_menu(wid);
        let rect = app.windows[&wid].tab_menu_rect.expect("the menu is up");
        let menu = menu_px(&app, wid, rect);
        let cell_on = |row: u16, col: u16| rect.contains(usize::from(row), usize::from(col));
        assert!(
            under.iter().any(|m| m.at.overlaps(menu)),
            "{anchor}: CONTROL, the border"
        );
        if over_kitty {
            assert!(
                over.iter().any(|m| m.at.overlaps(menu)),
                "{anchor}: CONTROL, the kitty"
            );
        } else {
            assert!(
                ink.iter().any(|c| cell_on(c.row, c.col)),
                "CONTROL, the ink"
            );
            assert!(
                decos.iter().any(|d| cell_on(d.row, d.col)),
                "CONTROL, the star"
            );
        }
        let p = painted(&app, wid);
        let s = &app.windows[&wid].input_scratch;
        assert!(
            s.fx_occluders.contains(&menu),
            "{anchor}: the menu's rectangle occludes the layers"
        );
        assert_eq!(p.layers, layers, "{anchor}: the layers are not cut here");
        assert!(
            !p.ink.iter().any(|c| cell_on(c.row, c.col)),
            "{anchor}: no ink"
        );
        assert!(
            !p.decos.iter().any(|d| cell_on(d.row, d.col)),
            "{anchor}: no star"
        );
        // On the glass: no pixel of the card's on the menu, the rest as it
        // was. CONTROL: without the occluder, the card lights the menu.
        let (shown, w) = card_pixels(&app, wid, true);
        let (whole, _) = card_pixels(&app, wid, false);
        let on_menu = |i: usize| {
            let (x, y) = (pixel(i % w), pixel(i / w));
            menu.x0 <= x && x < menu.x1 && menu.y0 <= y && y < menu.y1
        };
        assert!(
            (0..whole.len()).any(|i| on_menu(i) && whole[i] != 0),
            "{anchor}: CONTROL, the card lights the menu's rectangle"
        );
        for i in 0..shown.len() {
            let want = if on_menu(i) { 0 } else { whole[i] };
            assert_eq!(shown[i], want, "{anchor}: pixel ({}, {})", i % w, i / w);
        }
        // Framed again with the menu open: each of the card's layers once.
        frame(&mut app, wid);
        app.splice_tab_menu(wid);
        let p = painted(&app, wid);
        let s = &app.windows[&wid].input_scratch;
        assert_eq!(p.layers.len(), 2, "{anchor}");
        for l in &p.layers {
            assert_eq!(
                s.fx_layers.iter().filter(|o| *o == l).count(),
                1,
                "{anchor}: the record is the glass"
            );
        }
        assert!(app.close_tab_menu(wid));
    }
    set_now(None);
    host.shutdown_and_join();
}

/// The leftmost column the card's over layer (the kitty and its stars)
/// reaches on `wid`'s last frame.
fn over_col(app: &App, wid: WindowId, cw: usize, pad: usize) -> usize {
    let x0 = marks(app, wid, FxSlot::Over)
        .iter()
        .map(|m| m.at.x0)
        .min()
        .expect("the kitty and its stars");
    usize::try_from(x0).unwrap().saturating_sub(pad) / cw
}

/// THE ANCHORED FRAME: while an episode plays, the next frame is due a
/// frame after the last one DRAWN, whatever instant a wake asks at — a wake
/// every 10 ms (another window's build output, each a `WaitCancelled` that
/// re-arms the loop's timer) leaves the deadline where it is and finds the
/// frame due on time; once asked, no wake spins on it; drawn, the next is a
/// frame on.
#[test]
fn other_wakes_never_push_an_episode_frame_on() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    set_now(Some(t0));
    frame(&mut app, wid);
    for ms in [0, 10, 20, 30] {
        assert_eq!(
            app.claude_card_deadline(at(ms)),
            Some(at(FRAME_MS)),
            "a wake at {ms} ms: anchored at the frame drawn at 0"
        );
        assert!(app.claude_cards_due(at(ms)).is_empty(), "{ms} ms: not yet");
    }
    assert_eq!(app.claude_cards_due(at(40)), vec![wid], "due on time");
    assert_eq!(app.claude_card_deadline(at(40)), None, "asked once");
    assert!(app.claude_cards_due(at(41)).is_empty(), "asked once");
    set_now(Some(at(40)));
    frame(&mut app, wid);
    assert_eq!(app.claude_card_deadline(at(45)), Some(at(40 + FRAME_MS)));
    set_now(None);
    host.shutdown_and_join();
}

/// AN EPISODE PACES ITSELF, whatever else wakes the loop: under a stream of
/// wakes faster than its frame (a 120 Hz display's, the pointer's, another
/// pane's output), the window draws at most one card frame a [`FRAME_MS`] —
/// never one a wake — and the frame that settles the card is the last it
/// draws. The repaint gate is the loop's: the key is read BEFORE the splice
/// ticks the card, and a wake that finds it unmoved draws nothing.
/// CONTROL: the episode still plays (a frame every couple of frame times),
/// and a key that folded the frame just drawn — the card's fingerprint, as
/// it once did — draws on every wake (mutation-checked).
#[test]
fn an_episode_draws_a_frame_a_frame_time_whatever_else_wakes() {
    for wake_ms in [4u64, 8, 20] {
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let t0 = Instant::now();
        set_now(Some(t0));
        let mut drawn = app.claude_footer_fp(wid, &plan);
        frame(&mut app, wid);
        let (mut frames, mut late) = (0u64, 0u64);
        let mut t = 0;
        while t < EPISODE_MS + 1000 {
            t += wake_ms;
            let now = t0 + Duration::from_millis(t);
            set_now(Some(now));
            let _ = app.claude_cards_due(now);
            let key = app.claude_footer_fp(wid, &plan);
            if key != drawn {
                drawn = key;
                frame(&mut app, wid);
                frames += 1;
                if t > EPISODE_MS + 100 {
                    late += 1;
                }
            }
        }
        let most = EPISODE_MS / FRAME_MS + 2;
        assert!(
            frames <= most,
            "wakes every {wake_ms} ms: {frames} frames, at most {most}"
        );
        assert!(
            frames >= EPISODE_MS / (2 * FRAME_MS),
            "wakes every {wake_ms} ms: the episode played ({frames} frames)"
        );
        assert_eq!(late, 0, "wakes every {wake_ms} ms: settled, nothing more");
        assert_eq!(
            app.claude_card_deadline(t0 + Duration::from_millis(t)),
            None
        );
        set_now(None);
        host.shutdown_and_join();
    }
}

/// A CARD MOVES WITH ITS PANE: zooming the pane moves it to another leaf of
/// the window's plan (as a sibling split or closed to its left does); the
/// card is its session's, so its episode goes on where it stood — not
/// replayed from the start — and no second card lingers. Unzoomed, the same.
#[test]
fn a_pane_that_moves_in_the_plan_keeps_its_card() {
    let (mut app, wid, _) = glass(160);
    // Claude Code runs in the pane split off to the right: leaf 1.
    let session = app.split_active_stub_tab(wid);
    let paint = |app: &App| {
        let (rows, cols) = {
            let term = crate::term_lock(&app.pool.get(session).unwrap().term);
            (usize::from(term.rows()), usize::from(term.cols()))
        };
        feed(app, session, &card_screen(rows, cols, MODEL, VERSION));
    };
    publish(&app, session, VERSION);
    paint(&app);
    let host = supervise(&mut app, session);
    let t0 = Instant::now();
    set_now(Some(t0));
    frame(&mut app, wid);
    let card = |app: &App| {
        let entries = &app.windows[&wid].claude_card.entries;
        assert_eq!(entries.len(), 1, "one card for the session");
        (entries[0].leaf_index, entries[0].armed_at, entries[0].shown)
    };
    assert_eq!(card(&app), (1, t0, true));
    // Two seconds in, zoomed: the pane is the plan's only leaf.
    set_now(Some(t0 + Duration::from_secs(2)));
    app.toggle_pane_zoom();
    assert_eq!(app.active_visible_leaf_plan(wid).unwrap().leaves.len(), 1);
    paint(&app);
    frame(&mut app, wid);
    assert_eq!(
        card(&app),
        (0, t0, true),
        "zoomed: the same card, not replayed"
    );
    // Unzoomed: back at leaf 1, still the same card.
    app.toggle_pane_zoom();
    paint(&app);
    frame(&mut app, wid);
    assert_eq!(card(&app), (1, t0, true), "unzoomed");
    set_now(None);
    host.shutdown_and_join();
}

/// A NATIVE TAB IN FRONT (Settings) runs no footer splice, so the card keeps
/// the last terminal frame's `shown`: its episode arms no wake and asks for
/// no frame while it is there. Back on the terminal tab, the episode asks
/// again. CONTROL: on the terminal tab the playing episode wants frames.
#[test]
fn a_native_tab_in_front_arms_no_card_wake() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    set_now(Some(t0));
    frame(&mut app, wid);
    assert_eq!(
        app.claude_card_deadline(at(1)),
        Some(at(FRAME_MS)),
        "CONTROL"
    );
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    assert!(matches!(
        app.active_visible_content_route(wid),
        Some(crate::VisibleContentRoute::Native { .. })
    ));
    assert_eq!(app.claude_card_deadline(at(1)), None, "no wake behind it");
    assert!(app.claude_cards_due(at(40)).is_empty(), "no frame asked");
    app.switch_tab_in(wid, 0);
    assert_eq!(app.claude_card_deadline(at(40)), Some(at(FRAME_MS)), "back");
    set_now(None);
    host.shutdown_and_join();
}

/// SERIOUS MODE is in the repaint key: toggled on a settled card — whose
/// last frame, place and Autopilot all stay as they were — the key moves, so
/// the frame that strips the card to its border is drawn.
#[test]
fn serious_mode_moves_a_settled_cards_key() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let t0 = Instant::now();
    set_now(Some(t0));
    frame(&mut app, wid);
    set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 1000)));
    frame(&mut app, wid);
    let key = app.claude_footer_fp(wid, &plan);
    assert_eq!(app.claude_footer_fp(wid, &plan), key, "settled: constant");
    app.serious_mode = true;
    assert_ne!(
        app.claude_footer_fp(wid, &plan),
        key,
        "serious mode moves it"
    );
    app.serious_mode = false;
    assert_eq!(app.claude_footer_fp(wid, &plan), key, "and back");
    set_now(None);
    host.shutdown_and_join();
}

/// AN AUTOPILOT CHANGE ARMS AN EPISODE (§3): a settled card plays again when
/// its Autopilot moves from Cruise to Land, and when it comes back from Off
/// (the bare card) to Cruise; a frame with no change arms nothing.
#[test]
fn an_autopilot_change_arms_an_episode() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let mut t = Instant::now();
    set_now(Some(t));
    frame(&mut app, wid);
    let mut settle = |app: &mut App| {
        t += Duration::from_millis(EPISODE_MS + 1000);
        set_now(Some(t));
        frame(app, wid);
        assert_eq!(app.claude_card_deadline(t), None, "settled");
        frame(app, wid);
        assert_eq!(app.claude_card_deadline(t), None, "no change, no episode");
        t
    };
    let playing = |app: &App, now: Instant| {
        app.claude_card_deadline(now) == Some(now + Duration::from_millis(FRAME_MS))
    };
    let now = settle(&mut app);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Land),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    assert!(playing(&app, now), "Cruise to Land plays");
    let now = settle(&mut app);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    assert!(painted(&app, wid).is_empty(), "Off: bare");
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    assert!(playing(&app, now), "Off to Cruise plays");
    set_now(None);
    host.shutdown_and_join();
}

/// OFF TO CRUISE PLAYS THROUGH THE CLAIM'S GAP, as the real host runs it: Off
/// stops the session's worker and gives back its claim, and the word goes
/// back to Cruise tens of milliseconds before the new worker claims again —
/// a frame drawn in that gap shows the card bare under Cruise. The decorated
/// frame after the claim still plays the episode. CONTROL: Cruise to Land
/// under a held claim plays (the arm itself works), and a frame drawn bare
/// under Cruise with no Off before it (a gap with nothing changed) arms
/// nothing once the claim is back.
#[test]
fn off_to_cruise_plays_though_the_claim_lands_a_frame_later() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let reclaim = || {
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("the claim");
    };
    let release = || {
        assert!(crate::session_timeline::release_supervisor(
            &ctx,
            None,
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl
        ));
    };
    let mut t = Instant::now();
    set_now(Some(t));
    frame(&mut app, wid);
    let mut settle = |app: &mut App| {
        t += Duration::from_millis(EPISODE_MS + 1000);
        set_now(Some(t));
        frame(app, wid);
        frame(app, wid);
        assert_eq!(app.claude_card_deadline(t), None, "settled");
        t
    };
    let playing = |app: &App, now: Instant| {
        app.claude_card_deadline(now) == Some(now + Duration::from_millis(FRAME_MS))
    };
    // CONTROL: the arm works under a held claim.
    let now = settle(&mut app);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Land),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    assert!(playing(&app, now), "Cruise to Land plays");
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    let now = settle(&mut app);
    // Off: the worker stops and gives back its claim.
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    release();
    frame(&mut app, wid);
    assert!(painted(&app, wid).is_empty(), "Off: bare");
    // Back to Cruise: a frame in the gap before the new worker's claim.
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    assert!(
        painted(&app, wid).is_empty(),
        "the gap: Cruise, unclaimed, bare"
    );
    reclaim();
    frame(&mut app, wid);
    assert!(!painted(&app, wid).is_empty(), "claimed: decorated");
    assert!(
        playing(&app, now),
        "Off to Cruise plays though the claim came late"
    );
    // CONTROL: a claim gap under Cruise with no Off before it arms nothing.
    let now = settle(&mut app);
    release();
    frame(&mut app, wid);
    assert!(painted(&app, wid).is_empty(), "unclaimed: bare");
    reclaim();
    frame(&mut app, wid);
    assert!(!painted(&app, wid).is_empty(), "claimed again: decorated");
    assert_eq!(
        app.claude_card_deadline(now),
        None,
        "the same mode back: no episode"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// `tab_status` OFF (the footer's own gate: no status sweep, stale facts):
/// the splice draws no card — no light, no line — and no card is on show to
/// key the repaint or arm a wake. CONTROL: on, the card plays.
#[test]
fn tab_status_off_hides_every_card() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    set_now(Some(t0));
    frame(&mut app, wid);
    assert!(app.claude_card_deadline(t0).is_some(), "CONTROL");
    app.config.tab_status = Some(false);
    frame(&mut app, wid);
    assert!(painted(&app, wid).is_empty());
    assert!(!app.windows[&wid].claude_card.any_shown());
    assert_eq!(app.claude_card_deadline(t0), None);
    let strip = strip_rows(&app, wid);
    assert!(!glass_text(&app, wid, strip + 4, 0..120).contains("aterm harness"));
    set_now(None);
    host.shutdown_and_join();
}

/// A buffer's address and capacity: the same pair before and after a pass
/// is a pass that reallocated nothing there.
fn buffer<T>(v: &Vec<T>) -> (usize, usize) {
    (v.as_ptr() as usize, v.capacity())
}

/// A FRAME UNDER THE TAB MENU REALLOCATES NOTHING (`cut_out`, at the frame
/// rate while an episode plays under the open menu): each cut is built in
/// the window's spare and copied back, so the frame's layers and its ink
/// and decoration streams, the card's record of them and the spares keep
/// their buffers, frame after frame, once the first frame under the menu
/// has grown the spares; the card's layers are not cut here at all (the
/// menu's occluder cuts them in the renderer). CONTROL: each frame's cut
/// takes something out — the line's ink and its star (the menu at the
/// card's text) — and the menu's occluder lies on the border, and on the
/// kitty with the menu at the kitty.
#[test]
fn a_frame_under_the_tab_menu_keeps_its_buffers() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.tab_strip_rows = 1;
    set_now(Some(Instant::now()));
    frame(&mut app, wid);
    let (cw, _) = app.win_cell_size(wid);
    let kitty = over_col(&app, wid, cw, app.win_pad(wid));
    let buffers = |app: &App| {
        let (s, c) = (
            &app.windows[&wid].input_scratch,
            &app.windows[&wid].claude_card,
        );
        [
            buffer(&s.fx_layers),
            buffer(&s.ink),
            buffer(&s.word_decorations),
            buffer(&c.painted.layers),
            buffer(&c.painted.ink),
            buffer(&c.painted.decos),
            buffer(&c.spare_ink),
            buffer(&c.spare_decos),
        ]
    };
    let record = |app: &App| {
        let p = painted(app, wid);
        (p.layers.clone(), p.ink.clone(), p.decos.clone())
    };
    for (anchor, over_kitty) in [(3, false), (u16_of(kitty), true)] {
        assert!(app.open_tab_context_menu(wid, 0, anchor, false));
        frame(&mut app, wid);
        app.splice_tab_menu(wid);
        for i in 0..3 {
            frame(&mut app, wid);
            let (layers, ink, decos) = record(&app);
            let (under, over) = card_marks(&app, wid);
            let before = buffers(&app);
            app.splice_tab_menu(wid);
            let cut = record(&app);
            let rect = app.windows[&wid].tab_menu_rect.expect("the menu is up");
            let menu = menu_px(&app, wid, rect);
            assert!(
                under.iter().any(|m| m.at.overlaps(menu)),
                "{anchor}/{i}: CONTROL, the occluder lies on the border"
            );
            assert_eq!(cut.0, layers, "{anchor}/{i}: the layers are not cut here");
            if over_kitty {
                assert!(
                    over.iter().any(|m| m.at.overlaps(menu)),
                    "{anchor}/{i}: CONTROL, the occluder lies on the kitty's light"
                );
            } else {
                assert_ne!(cut.1, ink, "{anchor}/{i}: CONTROL, the ink is cut");
                assert_ne!(cut.2, decos, "{anchor}/{i}: CONTROL, the star is cut");
            }
            assert_eq!(buffers(&app), before, "{anchor}/{i}: nothing reallocated");
        }
        assert!(app.close_tab_menu(wid));
    }
    set_now(None);
    host.shutdown_and_join();
}

/// THE READING IS SHARED, NOT COPIED (`PaneTextCache::launch_card`): each
/// frame that reuses a pane's text hands its card the cache's own reading —
/// the same build, at the same address — and copies no build to compare it
/// with the process's (taken from the splice's own timeline lock, passed
/// in); and a REBUILD of the text (a repaint) under an unmoved card shares
/// the last rebuild's reading too, under a new rebuild number (the layout's
/// cells are compared once per rebuild). CONTROL: new text whose card names
/// another build is a new reading; a process of another build, under a card
/// its own group painted, leaves the card bare by the build rung.
#[test]
fn a_reused_pane_text_shares_its_card_reading() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let pane = PaneGeom {
        strip: strip_rows(&app, wid),
        row_off: 0,
        col_off: 0,
        rows: 24,
        cols: 120,
    };
    // The gate as the splice reads it, under its own timeline lock, and the
    // last frame's fence.
    let judged = |app: &App| {
        let gate = app
            .pool
            .get(session)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .card_gate();
        let ws = &app.windows[&wid];
        let mut check = GateCheck::new(gate, FenceFrame::of(&ws.input_scratch));
        card_leaf(
            &ws.claude_footer_texts.borrow(),
            app.config.claude_footer_card(),
            0,
            session,
            pane,
            RenderCell::default(),
            AutopilotView::Cruise,
            true,
            &mut check,
        )
    };
    let leaf = |app: &App| judged(app).card;
    let (first, again) = (leaf(&app), leaf(&app));
    let (Some((a, token, gen_a)), Some((b, _, gen_b))) = (&first, &again) else {
        panic!("a decorated card: {first:?} {again:?}");
    };
    assert_eq!(
        (Some(*token), &*a.version),
        (claude_image(PGID, VERSION, 1).token, VERSION)
    );
    assert!(Arc::ptr_eq(a, b), "one reading, shared");
    assert_eq!(gen_a, gen_b, "the same text");
    feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
    frame(&mut app, wid);
    let rebuilt = leaf(&app);
    let Some((c, _, gen_c)) = &rebuilt else {
        panic!("a decorated card: {rebuilt:?}");
    };
    assert_ne!(gen_c, gen_a, "the text was rebuilt");
    assert!(Arc::ptr_eq(c, a), "the same card, the same reading");
    publish(&app, session, "2.1.284");
    feed(&app, session, &card_screen(24, 120, MODEL, "2.1.284"));
    frame(&mut app, wid);
    let fresh = leaf(&app);
    let Some((d, _, _)) = &fresh else {
        panic!("a decorated card: {fresh:?}");
    };
    assert!(!Arc::ptr_eq(d, a), "CONTROL: another card, a new reading");
    assert_eq!(&*d.version, "2.1.284");
    // The process becomes another build while its card still names 2.1.284,
    // painted after the cut: the token and the fence hold, the build alone
    // refuses it.
    publish(&app, session, VERSION);
    feed(&app, session, &card_screen(24, 120, MODEL, "2.1.284"));
    frame(&mut app, wid);
    let other = judged(&app);
    assert_eq!(
        (other.card.is_none(), other.bare),
        (true, Some(Bare::Build)),
        "CONTROL: another build stays bare, by its build"
    );
    host.shutdown_and_join();
}

/// A FENCE OF ANOTHER HOLDER READS NO CARD (card engine §3.8, cheapest
/// first): once the front passes from the drawer to a shell (the drawer
/// stopped, its image still current), a frame whose pane text was rebuilt
/// judges the card by the token alone: bare, the card under it never read
/// or laid out. CONTROL: under the drawer's own fence the same rebuilt text
/// reads its card once and lays it at its new place. Mutation-checked: the
/// card read before the token costs the shell's frame one read.
#[test]
fn a_fence_of_another_holder_reads_no_card() {
    let base = card_screen(24, 120, MODEL, VERSION);
    let mut lowered = vec![String::new()];
    lowered.extend_from_slice(&base[..base.len() - 5]);
    lowered.extend_from_slice(&base[base.len() - 4..]);
    for drawer_front in [true, false] {
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        if !drawer_front {
            cut(&app, session, None, CutKind::Group, true);
        }
        feed(&app, session, &lowered);
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
        let cost = pass_cost(&mut app, wid);
        let n = usize::from(drawer_front);
        assert_eq!(
            (cost.text_rebuilds, cost.reads, cost.layouts),
            (1, n, n),
            "drawer in front {drawer_front}: {cost:?}"
        );
        host.shutdown_and_join();
    }
}

/// THE CARDS BEFORE THE WINDOW SERVER (`crate::work_counts`): whether a
/// window is on screen is a round trip off macOS, and the card's two judges
/// run on every wake, so they ask it only of a window with a card frame
/// wanted — not with no card, not for a still card, not once its frame is
/// asked for. CONTROL: a card playing its episode is asked about, once per
/// judge that finds a frame wanted, and its frame comes.
#[test]
fn only_a_wanted_card_frame_asks_whether_its_window_is_on_screen() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    set_now(Some(t0));
    let judge = |app: &mut App, now: Instant| {
        let _ = crate::work_counts::take_on_screen_asks();
        let wake = (app.claude_card_deadline(now), app.claude_cards_due(now));
        (wake, crate::work_counts::take_on_screen_asks())
    };
    assert_eq!(judge(&mut app, t0), ((None, vec![]), 0), "no card");
    // Unfocused, the card is drawn still and wants no frame.
    app.windows.get_mut(&wid).unwrap().focused = false;
    frame(&mut app, wid);
    assert!(app.windows[&wid].claude_card.any_shown());
    assert_eq!(judge(&mut app, t0), ((None, vec![]), 0), "a still card");
    // CONTROL: focus regained, its episode plays.
    app.windows.get_mut(&wid).unwrap().focused = true;
    app.rearm_claude_cards(wid, || t0);
    assert_eq!(
        judge(&mut app, t0),
        ((Some(at(FRAME_MS)), vec![]), 1),
        "CONTROL: a frame wanted, not yet due"
    );
    assert_eq!(
        judge(&mut app, at(FRAME_MS)),
        ((Some(at(FRAME_MS)), vec![wid]), 2),
        "CONTROL: due"
    );
    assert_eq!(
        judge(&mut app, at(FRAME_MS)),
        ((None, vec![]), 0),
        "asked for: nothing wanted until it is drawn"
    );
    set_now(None);
    host.shutdown_and_join();
}

// ---------------------------------------------------------------------------
// The steady state's cost (owner, 2026-10-15: "the user experince must be
// extremely smooth and zero latency!")
// ---------------------------------------------------------------------------

/// What one pass of the per-frame and per-wake paths cost: the footer's
/// repaint key (`App::claude_footer_fp`, with the chips' and the card's
/// terms), its splice (the card's pass and the chips' block inside it) and
/// the loop's two card judges (`App::claude_card_redraws`,
/// `App::claude_card_wake`) — counted by `crate::work_counts`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PassCost {
    /// Heap allocations and reallocations.
    allocations: usize,
    /// The harness's clock reads (the card's, a claim's lapse, a chip
    /// click's read-back).
    clock_reads: usize,
    /// Card frames the emitter drew (not replayed).
    ticks: usize,
    /// Card layouts laid over the pane's cells.
    layouts: usize,
    /// Launch cards parsed off the pane's text.
    reads: usize,
    /// Sessions' Autopilot reads (one metadata lock each).
    autopilot_reads: usize,
    /// Card lines whose ink was laid (not kept).
    line_inks: usize,
    /// Session timeline locks the splice took.
    timeline_locks: usize,
    /// Session timeline locks the dev card probe took (its rows: one a pane
    /// judged, none with the probe off).
    probe_locks: usize,
    /// Rebuilds of a Claude Code pane's text (the footer's, card or none).
    text_rebuilds: usize,
    /// The footer's own clock reads (its lights').
    footer_clock_reads: usize,
    /// Sorts of the card's recorded keys to take its light back out of a
    /// stream (`remove_each_once`): none where the stream holds none of it.
    strip_sorts: usize,
    /// Fx contents sealed on this thread (`FxContent::new`/`edit`): none
    /// for a card replayed at rest, whose kept contents go on as they are.
    fx_builds: u64,
    /// Fx contents copied on this thread (a `Clone` of the payload): none,
    /// ever — a layer hands its content on by `Arc`.
    fx_copies: u64,
    /// Fx arts built (`aterm_render::fx::registry`): none per frame — a
    /// card's arts are built once per cell height, off the frame path.
    art_builds: usize,
    /// Window cell boxes the renderer derived off its face (the card's look
    /// asks the window's, `App::win_cell_size`): none while the window's
    /// px and font are what they were.
    cell_geometries: usize,
}

/// Run one pass on `wid` (the footer's key, its splice and the card's
/// judges) and say what it cost. The counters are taken before it, so a
/// cost is this pass's alone.
fn pass_cost(app: &mut App, wid: WindowId) -> PassCost {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = (
        crate::work_counts::take_harness_clock_reads(),
        crate::work_counts::take_card_ticks(),
        crate::work_counts::take_card_layouts(),
        crate::work_counts::take_card_reads(),
        crate::work_counts::take_autopilot_reads(),
        crate::work_counts::take_card_line_inks(),
        crate::work_counts::take_footer_timeline_locks(),
        crate::work_counts::take_card_probe_locks(),
        crate::work_counts::take_pane_text_rebuilds(),
        crate::work_counts::take_footer_clock_reads(),
        crate::work_counts::take_card_strip_sorts(),
        crate::work_counts::take_cell_geometry_derivations(),
        aterm_render::test_counters::take_art_builds(),
    );
    let (builds0, copies0) = aterm_core::render::fx_counts_here();
    let ((), allocations) = crate::work_counts::allocations_in(|| {
        let _ = app.claude_footer_fp(wid, &plan);
        app.splice_claude_footer(wid, &plan, route);
        let _ = app.claude_card_redraws();
        let _ = app.claude_card_wake();
    });
    PassCost {
        allocations,
        clock_reads: crate::work_counts::take_harness_clock_reads(),
        ticks: crate::work_counts::take_card_ticks(),
        layouts: crate::work_counts::take_card_layouts(),
        reads: crate::work_counts::take_card_reads(),
        autopilot_reads: crate::work_counts::take_autopilot_reads(),
        line_inks: crate::work_counts::take_card_line_inks(),
        timeline_locks: crate::work_counts::take_footer_timeline_locks(),
        probe_locks: crate::work_counts::take_card_probe_locks(),
        text_rebuilds: crate::work_counts::take_pane_text_rebuilds(),
        footer_clock_reads: crate::work_counts::take_footer_clock_reads(),
        strip_sorts: crate::work_counts::take_card_strip_sorts(),
        fx_builds: aterm_core::render::fx_counts_here().0 - builds0,
        fx_copies: aterm_core::render::fx_counts_here().1 - copies0,
        art_builds: aterm_render::test_counters::take_art_builds(),
        cell_geometries: crate::work_counts::take_cell_geometry_derivations(),
    }
}

/// A keystroke into Claude's composer, framed: the engine takes the key's
/// echo (the grid is damaged), and the frame is extracted and stripped as
/// the capture route does, ready for [`pass_cost`].
fn keystroke(app: &mut App, wid: WindowId, session: u64, key: u8) {
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).process(&[key]);
    app.capture_words(wid, held_now());
    app.splice_tab_strip(wid);
}

/// The card fixture played out to its SETTLED frame on the test clock:
/// decorated at `t0`, its episode drawn, then the frame it owes.
fn settled_glass(cols: u16, t0: Instant) -> (App, WindowId, u64, crate::harness_host::HostHandle) {
    let (mut app, wid, session) = glass(cols);
    let host = supervise(&mut app, session);
    set_now(Some(t0));
    frame(&mut app, wid);
    set_now(Some(t0 + Duration::from_millis(FRAME_MS)));
    frame(&mut app, wid);
    set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 1)));
    frame(&mut app, wid);
    let ws = &app.windows[&wid];
    assert!(ws.claude_card.any_shown(), "the card is on show");
    assert!(
        ws.claude_card.entries.iter().all(|e| e.card.at_rest()),
        "the card is settled"
    );
    (app, wid, session, host)
}

/// The same screen with no harness host: no chip drawn, no card wrapped —
/// the footer's own cost, which the harness's is measured against.
fn bare_glass(cols: u16) -> (App, WindowId, u64) {
    let (mut app, wid, session) = glass(cols);
    frame(&mut app, wid);
    frame(&mut app, wid);
    assert!(
        !app.windows[&wid].claude_card.any_shown(),
        "no card wrapped"
    );
    (app, wid, session)
}

/// STEADY STATE 1 — A FRAME AT REST WITH A SETTLED, DECORATED CARD costs the
/// harness NOTHING per frame, frame after frame: the pass costs exactly what
/// the same screen with no harness host costs, counter for counter — the
/// same allocations (the chips' block and the card's line laid into kept
/// buffers, the footer's write record reused), no clock read (the card at
/// rest, the claim with no lapse), no frame drawn (the settled frame
/// replayed), no layout and no card parsed (the pane's text unchanged), the
/// line's ink kept, the session's Autopilot read once by the key and once by
/// the splice (the chips and the card share each reading) and ONE timeline
/// lock (the splice's own: the card takes the process from it). The glass
/// is the settled frame, its two layers the same content `Arc`s at the same
/// place every time, put on from where they are kept: no fx content sealed
/// or copied and no art built (`fx_builds`, `fx_copies`, `art_builds`), and
/// the card's look reads the window's cell box from what the renderer
/// kept, deriving none off the face (`cell_geometries`).
///
/// NEGATIVE CONTROLS: a window regaining focus re-arms the card, and its
/// next pass DRAWS a frame on the clock, seals its two contents and inks
/// its line (`ticks`, `clock_reads`, `fx_builds`, `line_inks` move — the
/// counters see the work the rest skips; a line cache that never keeps its
/// ink fails here, and so does a host that draws its card at rest,
/// mutation-checked); and a card decorated for the first time allocates its
/// buffers (the allocation counter sees the card's).
#[test]
fn a_settled_card_at_rest_costs_the_frame_nothing() {
    let t0 = Instant::now();
    let (mut app, wid, _session, host) = settled_glass(120, t0);
    let (mut bare, bare_wid, _) = bare_glass(120);
    let layers = card_layers(&app, wid);
    let (under, over) = card_marks(&app, wid);
    assert!(!under.is_empty(), "the border is on the glass");
    assert!(!over.is_empty(), "the kitty and its stars are on the glass");
    let _ = pass_cost(&mut app, wid);
    let _ = pass_cost(&mut bare, bare_wid);
    let floor = pass_cost(&mut bare, bare_wid);
    assert_eq!(
        (
            floor.timeline_locks,
            floor.autopilot_reads,
            floor.text_rebuilds
        ),
        (1, 2, 0),
        "the footer's own pass at rest: {floor:?}"
    );
    for i in 0..50u64 {
        set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 100 + 7 * i)));
        assert_eq!(pass_cost(&mut app, wid), floor, "frame {i}");
        assert_eq!(card_layers(&app, wid), layers, "frame {i}: the same glass");
    }
    assert_eq!(pass_cost(&mut bare, bare_wid), floor, "the floor holds");
    // NEGATIVE CONTROL: focus regained, the episode is drawn on the clock.
    let back = t0 + Duration::from_secs(60);
    set_now(Some(back));
    app.rearm_claude_cards(wid, || back);
    let replayed = pass_cost(&mut app, wid);
    assert_eq!(replayed.ticks, 1, "CONTROL: an episode frame is drawn");
    assert!(replayed.clock_reads > 0, "CONTROL: on the clock");
    assert_eq!(replayed.line_inks, 1, "CONTROL: its line inked anew");
    assert_eq!(
        (replayed.fx_builds, replayed.fx_copies, replayed.art_builds),
        (2, 0, 0),
        "CONTROL: a drawn frame seals its two contents, with the arts it had"
    );
    assert_ne!(card_layers(&app, wid), layers, "CONTROL: new contents");
    // NEGATIVE CONTROL: a card decorated for the first time grows its
    // buffers — a first frame with the harness allocates more than the same
    // first frame without it.
    let first_frame = |harness: bool| {
        let (mut fresh, fresh_wid, fresh_session) = glass(120);
        let fresh_host = harness.then(|| supervise(&mut fresh, fresh_session));
        fresh.capture_words(fresh_wid, Instant::now());
        fresh.splice_tab_strip(fresh_wid);
        let cost = pass_cost(&mut fresh, fresh_wid);
        if let Some(fresh_host) = fresh_host {
            fresh_host.shutdown_and_join();
        }
        cost
    };
    let (wrapped, unwrapped) = (first_frame(true), first_frame(false));
    assert!(
        wrapped.allocations > unwrapped.allocations && wrapped.ticks == 1,
        "CONTROL: the first decorated frame allocates ({wrapped:?} vs {unwrapped:?})"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// AN INLINE SCROLL OF A SETTLED CARD DRAWS NOTHING (card engine §4.8):
/// while Claude's inline output scrolls a card at rest up the screen a row
/// a frame, the card is read and laid out anew at its new place — and
/// REPLAYED there: the same two contents (`Arc::ptr_eq`) at an origin a row
/// up, every mark exactly a row up, and the pass draws no frame, seals or
/// copies no fx content, builds no art and reads no clock for the card.
/// NEGATIVE CONTROL: a window regaining focus re-arms the card, and its
/// next pass draws a frame and seals its two contents (mutation-checked: a
/// host that draws instead of replaying fails the rows above).
#[test]
fn an_inline_scroll_replays_the_settled_card_at_its_new_place() {
    let t0 = Instant::now();
    let base = card_screen(24, 120, MODEL, VERSION);
    // The card `down` rows lower, the composer kept at the bottom.
    let lowered = |down: usize| {
        let mut rows = vec![String::new(); down];
        rows.extend_from_slice(&base[..base.len() - 4 - down]);
        rows.extend_from_slice(&base[base.len() - 4..]);
        rows
    };
    let (mut app, wid, session) = glass(120);
    feed(&app, session, &lowered(3));
    let host = supervise(&mut app, session);
    for t in [0, FRAME_MS, EPISODE_MS + 1] {
        set_now(Some(t0 + Duration::from_millis(t)));
        frame(&mut app, wid);
    }
    assert!(
        app.windows[&wid].claude_card.entries[0].card.at_rest(),
        "settled"
    );
    let ch = pixel(app.win_cell_size(wid).1);
    for down in (0..3).rev() {
        let layers = card_layers(&app, wid);
        let (under, over) = card_marks(&app, wid);
        assert!(!under.is_empty() && !over.is_empty(), "down {down}: drawn");
        feed(&app, session, &lowered(down));
        set_now(Some(t0 + Duration::from_secs(10 + down as u64)));
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
        let cost = pass_cost(&mut app, wid);
        assert_eq!(
            (cost.reads, cost.layouts),
            (1, 1),
            "down {down}: CONTROL, read and laid anew at its new place: {cost:?}"
        );
        assert_eq!(
            (
                cost.ticks,
                cost.clock_reads,
                cost.fx_builds,
                cost.fx_copies,
                cost.art_builds
            ),
            (0, 0, 0, 0, 0),
            "down {down}: replayed, nothing drawn: {cost:?}"
        );
        let now = card_layers(&app, wid);
        assert_eq!(now.len(), 2);
        for (l, was) in now.iter().zip(&layers) {
            assert!(
                Arc::ptr_eq(&l.content, &was.content),
                "down {down}: {:?}, the same contents",
                l.slot
            );
            assert_eq!(l.offset, (was.offset.0, was.offset.1 - ch), "down {down}");
        }
        let up = |ms: &[Mark]| ms.iter().map(|m| m.moved(0, -ch)).collect::<Vec<_>>();
        assert_eq!(
            card_marks(&app, wid),
            (up(&under), up(&over)),
            "down {down}"
        );
    }
    // NEGATIVE CONTROL: re-armed, the next pass draws.
    let back = t0 + Duration::from_secs(60);
    set_now(Some(back));
    app.rearm_claude_cards(wid, || back);
    let drawn = pass_cost(&mut app, wid);
    assert_eq!(
        (drawn.ticks, drawn.fx_builds, drawn.fx_copies),
        (1, 2, 0),
        "CONTROL: a drawn frame seals its two contents: {drawn:?}"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// THE CARD'S ARTS, AHEAD (card engine §4.3): a card drawer's image
/// published for the session a window shows queues the card's arts at the
/// window's cell height for the renderer's residency — every art the card
/// can draw with (`CardArts::arts`) — when the status sweep visits the
/// session off the edged inbox; the card's first frame then draws with
/// those very arts. A cell metric change while the drawer is in front
/// queues them again (`App::sync_cell_pixel_size`). CONTROLS: a shell's
/// image queues nothing on its edge or on a metric change, and a drawer
/// with the card switched off queues nothing.
#[test]
fn a_published_drawer_queues_the_cards_arts() {
    use atpkg::image::{ImageBuild, ImageSource, ProgramTag};
    let shell = || {
        Arc::new(crate::holder_image::HolderImage::of(
            PGID,
            Some(1),
            Some(std::path::PathBuf::from("/bin/zsh")),
            ImageBuild::unknown(ProgramTag::new("zsh")),
            1,
        ))
    };
    let drawer = || {
        Arc::new(crate::holder_image::HolderImage::of(
            PGID,
            Some(2),
            Some(std::path::PathBuf::from(
                "/p/pkg/store/claude/1000002000001000304/bin/claude",
            )),
            ImageBuild {
                program: ProgramTag::new("claude"),
                build: 1_000_002_000_001_000_304,
                source: ImageSource::Store,
            },
            1,
        ))
    };
    let offer = |app: &App, session: u64, image| {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.offer_holder_image(image, crate::holder_image::EdgeKind::Group);
    };
    let edge = |app: &mut App, session: u64| {
        app.session_factory.edge_inbox.push(session);
        let _ = app.observe_session_statuses(Instant::now());
    };
    let pending = |app: &App, wid: WindowId| app.windows[&wid].fx_prefetch_pending.clone();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    // CONTROL: a shell's edge.
    offer(&app, session, shell());
    edge(&mut app, session);
    assert!(
        pending(&app, wid).is_empty(),
        "CONTROL: a shell queues nothing"
    );
    app.windows.get_mut(&wid).unwrap().cell_px_reported = None;
    app.sync_cell_pixel_size(wid);
    assert!(
        pending(&app, wid).is_empty(),
        "CONTROL: nor does a metric change under it"
    );
    // A drawer's edge.
    offer(&app, session, drawer());
    assert!(app.card_drawer_published(session));
    edge(&mut app, session);
    let want = CardArts::for_cell(u16_of(app.win_cell_size(wid).1)).expect("ordinary cell height");
    let want: Vec<_> = want.arts().cloned().collect();
    assert!(want.len() > 6, "the kitty's, the rims' and the stars' arts");
    let queued = |app: &App| {
        // As sets: the registry interns equal arts as one `Arc`, and the
        // queue holds each once.
        let got = pending(app, wid);
        want.iter().all(|a| got.iter().any(|g| Arc::ptr_eq(a, g)))
            && got.iter().all(|g| want.iter().any(|a| Arc::ptr_eq(a, g)))
    };
    assert!(queued(&app), "the card's arts, queued");
    // The card that comes draws with those very arts.
    publish(&app, session, VERSION);
    feed(&app, session, &card_screen(24, 80, MODEL, VERSION));
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let over = card_layer(&app, wid, FxSlot::Over).expect("the over layer");
    let got = pending(&app, wid);
    let sprites: Vec<_> = over
        .content
        .items()
        .iter()
        .filter_map(|i| match i {
            FxItem::Sprite(s) => Some(Arc::clone(&s.art)),
            _ => None,
        })
        .collect();
    assert!(!sprites.is_empty(), "the kitty drawn");
    assert!(
        sprites
            .iter()
            .all(|a| got.iter().any(|g| Arc::ptr_eq(a, g))),
        "every art the frame draws was queued"
    );
    // The renderer takes the queue; a metric change queues them again.
    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx_prefetch_pending
        .clear();
    app.windows.get_mut(&wid).unwrap().cell_px_reported = None;
    app.sync_cell_pixel_size(wid);
    assert!(queued(&app), "a metric change with the drawer in front");
    host.shutdown_and_join();
    // CONTROL: the card switched off.
    let mut off = App::headless_for_test();
    let off_session = off.focused_session_id(wid).expect("the front session");
    off.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
        card: Some(false),
        ..Default::default()
    });
    offer(&off, off_session, drawer());
    edge(&mut off, off_session);
    assert!(
        pending(&off, wid).is_empty(),
        "CONTROL: the card switched off queues nothing"
    );
}

/// THE PINNED CONSTELLATION (`card seed=`, a debug build's
/// `crate::card_verb`): two cards of different processes — each card's
/// stars seeded by its session and its process — draw the very same settled
/// frame once pinned to one seed, and a pin re-arms the card on show, whose
/// episode settles on the same frame again. CONTROLS: unpinned, the two
/// cards' stars differ; pinned to two seeds, one card's differ.
#[test]
fn a_pinned_seed_gives_every_card_the_same_stars() {
    use crate::card_verb::{Asked, line};
    let settled = |app: &mut App, wid: WindowId, t0: Instant| {
        for t in [0, FRAME_MS, EPISODE_MS + 1] {
            set_now(Some(t0 + Duration::from_millis(t)));
            frame(app, wid);
        }
        assert!(
            app.windows[&wid]
                .claude_card
                .entries
                .iter()
                .all(|e| e.card.at_rest()),
            "settled"
        );
        card_pic(app, wid)
    };
    let card = |pgid: i32, pin: Option<u64>| {
        let (mut app, wid, session) = glass(120);
        publish_as(&app, session, VERSION, pgid);
        // Its own card, painted after it took the front.
        feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
        let host = supervise(&mut app, session);
        assert_eq!(
            app.control_card_seed(Asked::Pin(pin)),
            Ok(line(pin, 0)),
            "no card on show yet"
        );
        assert_eq!(app.control_card_seed(Asked::Read), Ok(line(pin, 0)));
        let t0 = Instant::now();
        let first = settled(&mut app, wid, t0);
        assert!(!first.is_empty(), "the card is on the glass");
        let back = t0 + Duration::from_secs(60);
        set_now(Some(back));
        assert_eq!(
            app.control_card_seed(Asked::Pin(pin)),
            Ok(line(pin, 1)),
            "the card on show, re-armed"
        );
        assert!(
            !app.windows[&wid].claude_card.entries[0].card.at_rest(),
            "an episode armed"
        );
        assert_eq!(settled(&mut app, wid, back), first, "the same stars again");
        set_now(None);
        host.shutdown_and_join();
        first
    };
    assert_eq!(
        card(PGID, Some(42)),
        card(PGID + 1, Some(42)),
        "pinned: one constellation"
    );
    assert_ne!(
        card(PGID, None),
        card(PGID + 1, None),
        "CONTROL: unpinned, each card's own"
    );
    assert_ne!(
        card(PGID, Some(42)),
        card(PGID, Some(43)),
        "CONTROL: the seed is the stars'"
    );
    crate::claude_card::pin_card_seed(None);
}

/// Feed both `app`'s and `bare`'s session the same bytes and frame them as
/// the capture route does, ready for [`pass_cost`].
fn feed_both(app: &mut (App, WindowId, u64), bare: &mut (App, WindowId, u64), bytes: &[u8]) {
    for (app, wid, session) in [app, bare] {
        let term = app.pool.get(*session).expect("session").term.clone();
        crate::term_lock(&term).process(bytes);
        app.capture_words(*wid, held_now());
        app.splice_tab_strip(*wid);
    }
}

/// THE CHIPS COST THE RULE NOTHING (`claude_footer::rule_edits`): with the
/// harness's chips drawn at the composer rule's left end and the facts at
/// its right, a pass writes the bare footer's cells, the card's line and the
/// chips' few cells — never the rule's own glyphs between them, none of
/// which is written over with itself — keystroke after keystroke.
///
/// NEGATIVE CONTROLS: the rule between them is most of the pane's width
/// (what a whole write of the row copied, glyph over itself, before), and
/// the card's line is counted (the counter sees what the harness writes).
#[test]
fn the_chips_write_no_rule_glyph() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let (mut bare, bare_wid, bare_session) = bare_glass(120);
    let line = painted(&app, wid).ink.len();
    assert!(
        line > 40,
        "CONTROL: the card's line is on the glass ({line})"
    );
    for (i, key) in b"typing".iter().enumerate() {
        keystroke(&mut app, wid, session, *key);
        keystroke(&mut bare, bare_wid, bare_session, *key);
        let _ = crate::work_counts::take_footer_cells_written();
        let _ = pass_cost(&mut bare, bare_wid);
        let floor = crate::work_counts::take_footer_cells_written();
        let _ = pass_cost(&mut app, wid);
        let cells = crate::work_counts::take_footer_cells_written();
        let chips = cells.checked_sub(floor + line);
        assert!(
            chips.is_some_and(|c| (1..=8).contains(&c)),
            "pass {i}: {cells} cells written, the bare footer's {floor} and the line's {line}"
        );
        assert_eq!(
            app.windows[&wid]
                .claude_footer_undo
                .rewritten_as_they_were(),
            0,
            "pass {i}: no cell written over with itself"
        );
    }
    // CONTROL: the rule's own glyphs on the glass, which a whole write of the
    // row would have copied over themselves.
    let rule_row = strip_rows(&app, wid) + 22;
    let glyphs = glass_text(&app, wid, rule_row, 0..120)
        .chars()
        .filter(|c| *c == '\u{2500}')
        .count();
    assert!(glyphs > 80, "CONTROL: the rule between them: {glyphs}");
    set_now(None);
    host.shutdown_and_join();
}

/// The steady-state tests' common shape: the settled card (`app`) and its
/// bare twin (`bare`, the same screen with no harness host) take the same
/// change each pass (`step`, given the pass's index); every pass rebuilds
/// the pane's text (the control that the change reached the footer) and
/// costs the harness exactly the twin's pass, counter for counter — no
/// reading, no layout, no frame, no ink, no lock, no clock read, no sort of
/// its light's keys, no allocation of its own — and the card's glass stays
/// the settled frame.
fn costs_the_floor(
    what: &str,
    passes: usize,
    mut step: impl FnMut(usize, &mut (App, WindowId, u64), &mut (App, WindowId, u64)),
) {
    let t0 = Instant::now();
    let (app, wid, session, host) = settled_glass(120, t0);
    let mut app = (app, wid, session);
    let mut bare = bare_glass(120);
    // A few passes to grow what the change grows (the rows' buffers, a
    // streamed row's text, the streams a chrome row's own animation
    // republishes while it comes in), in both.
    // Every pass, these and the measured ones below, on the test's clock:
    // the card's and the captures' (`held_now`).
    const WARM: usize = 6;
    for i in 0..WARM {
        set_now(Some(
            t0 + Duration::from_millis(EPISODE_MS + 200 + 9 * i as u64),
        ));
        step(i, &mut app, &mut bare);
        let _ = pass_cost(&mut app.0, wid);
        let _ = pass_cost(&mut bare.0, bare.1);
    }
    let layers = card_layers(&app.0, wid);
    // What the glass's layer stream holds once the card is on it: the card's
    // light is there once a frame, never a copy left behind by the last
    // (a capture's tab strip shifts what the splice left, so a record that
    // missed it would leave one stale grid layer a pass).
    let stream = app.0.windows[&wid].input_scratch.fx_layers.len();
    assert!(
        !marks(&app.0, wid, FxSlot::Under).is_empty(),
        "{what}: the border"
    );
    let line = glass_text(&app.0, wid, strip_rows(&app.0, wid) + 4, 0..120);
    assert!(line.contains("aterm harness"), "{what}: the line: {line:?}");
    for i in WARM..WARM + passes {
        set_now(Some(
            t0 + Duration::from_millis(EPISODE_MS + 200 + 9 * i as u64),
        ));
        step(i, &mut app, &mut bare);
        let floor = pass_cost(&mut bare.0, bare.1);
        assert_eq!(
            floor.text_rebuilds, 1,
            "{what} {i}: CONTROL, rebuilt: {floor:?}"
        );
        let cost = pass_cost(&mut app.0, wid);
        // The stream first: a copy left behind is what a leak IS, while the
        // pass's allocation count sees it only when the stream outgrows its
        // capacity.
        assert_eq!(
            app.0.windows[&wid].input_scratch.fx_layers.len(),
            stream,
            "{what} {i}: the card's light once, no copy of the last frame's"
        );
        assert_eq!(cost, floor, "{what} {i}");
        assert_eq!(
            card_layers(&app.0, wid),
            layers,
            "{what} {i}: the same glass"
        );
        assert_eq!(
            glass_text(&app.0, wid, strip_rows(&app.0, wid) + 4, 0..120),
            line,
            "{what} {i}: the same line"
        );
    }
    set_now(None);
    host.shutdown_and_join();
}

/// STEADY STATE 2 — A KEYSTROKE INTO CLAUDE'S COMPOSER damages the grid and
/// re-splices the footer: the pane's text is rebuilt — only its composer row
/// changed, far below the card — and the harness costs nothing over the
/// bare footer's own pass: the card's reading is kept without a scan (no row
/// it reads changed: `CardReading::holds`), its layout kept by testing
/// again only the cells it read (`Seen::hold`), its light taken back out of
/// the re-extracted streams with no sort (`strip_sorts`: they hold none of
/// it), its frame replayed and its line kept, keystroke after keystroke.
///
/// NEGATIVE CONTROLS: the rebuild is real (`text_rebuilds`, in
/// `costs_the_floor`); keying the reading or the layout by the rebuild
/// again costs a reading and a layout per keystroke (mutation-checked); and
/// a keystroke while the card's episode plays draws the frame on the clock.
#[test]
fn a_keystroke_under_a_settled_card_costs_the_frame_nothing() {
    costs_the_floor("keystroke", 12, |i, app, bare| {
        feed_both(app, bare, &[b"keep typing!"[i % 12]]);
    });
    // NEGATIVE CONTROL: the card's episode plays; a keystroke draws a frame.
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let back = t0 + Duration::from_secs(60);
    set_now(Some(back));
    app.rearm_claude_cards(wid, || back);
    keystroke(&mut app, wid, session, b'!');
    let playing = pass_cost(&mut app, wid);
    assert_eq!(playing.ticks, 1, "CONTROL: an episode frame is drawn");
    assert!(playing.clock_reads > 0, "CONTROL: on the clock");
    set_now(None);
    host.shutdown_and_join();
}

/// STEADY STATE 3 — CLAUDE AT WORK UNDER THE CARD: a spinner ticking on its
/// row and streamed words landing between the card and the composer (the
/// normal state under Cruise) rebuild the pane's text every frame, and none
/// of it is a banner or a cell the card reads: the harness costs nothing
/// over the bare footer's pass, frame after frame.
#[test]
fn claude_streaming_under_the_card_costs_the_frame_nothing() {
    costs_the_floor("streamed", 12, |i, app, bare| {
        let spinner = ['\u{2722}', '\u{2733}', '\u{2736}', '\u{273B}'][i % 4];
        let bytes = format!(
            "\x1b7\x1b[9;1H\u{23FA} streamed line {i}: Claude Code v2.1.283 said so\x1b[K\
             \x1b[12;1H{spinner} Thinking\u{2026} ({i}s \u{b7} esc to interrupt)\x1b[K\x1b8"
        );
        feed_both(app, bare, bytes.as_bytes());
    });
}

/// STEADY STATE 4 — A REVISION BUMP THAT CHANGED NO CELL: a re-extract with
/// nothing new on the screen — bare, and under a tab strip's chrome row,
/// which forces a refill every frame — moves the scratch's revision and so
/// rebuilds the pane's text, every row the same: the harness costs nothing
/// over the bare footer's pass (the reading and the layout kept, the rows'
/// snapshot itself shared). The split compositor's every-frame compose is
/// `a_split_composed_every_frame_costs_the_card_nothing`.
#[test]
fn a_revision_bump_with_no_cell_changed_costs_the_frame_nothing() {
    for strip in [0, 1] {
        costs_the_floor(&format!("re-extract, strip {strip}"), 12, |i, app, bare| {
            for (app, wid, _) in [app, bare] {
                if i == 0 {
                    app.tab_strip_rows = strip;
                }
                app.capture_words(*wid, held_now());
                app.splice_tab_strip(*wid);
            }
        });
    }
}

/// A SPLIT, COMPOSED EVERY FRAME (`app_render` bumps the composite's
/// revision on each compose): the Claude pane's text is rebuilt every frame
/// with nothing in it changed, and its card costs no reading, no layout, no
/// frame and no allocation over the same split with no harness host.
/// CONTROL: each frame rebuilds the text.
#[test]
fn a_split_composed_every_frame_costs_the_card_nothing() {
    let split = |harness: bool| {
        let (mut app, wid, _) = glass(160);
        let session = app.split_active_stub_tab(wid);
        let (rows, cols) = {
            let term = crate::term_lock(&app.pool.get(session).unwrap().term);
            (usize::from(term.rows()), usize::from(term.cols()))
        };
        publish(&app, session, VERSION);
        feed(&app, session, &card_screen(rows, cols, MODEL, VERSION));
        let host = harness.then(|| supervise(&mut app, session));
        (app, wid, host)
    };
    let t0 = Instant::now();
    let (mut app, wid, host) = split(true);
    let (mut bare, bare_wid, _) = split(false);
    for t in [0, FRAME_MS, EPISODE_MS + 1] {
        set_now(Some(t0 + Duration::from_millis(t)));
        frame(&mut app, wid);
        frame(&mut bare, bare_wid);
    }
    assert!(
        !marks(&app, wid, FxSlot::Under).is_empty(),
        "the card is on the glass"
    );
    let layers = card_layers(&app, wid);
    for i in 0..10u64 {
        set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 100 + 9 * i)));
        for (app, wid) in [(&mut app, wid), (&mut bare, bare_wid)] {
            app.capture_words(wid, held_now());
            app.splice_tab_strip(wid);
        }
        let floor = pass_cost(&mut bare, bare_wid);
        assert!(floor.text_rebuilds >= 1, "{i}: CONTROL, rebuilt: {floor:?}");
        assert_eq!(pass_cost(&mut app, wid), floor, "frame {i}");
        assert_eq!(card_layers(&app, wid), layers, "frame {i}: the same glass");
    }
    set_now(None);
    if let Some(host) = host {
        host.shutdown_and_join();
    }
}

/// STEADY STATE 5 — A WINDOW WITH NO CLAUDE CODE SESSION costs the harness
/// nothing at all: no allocation anywhere in the pass, no clock read (not
/// even the footer lights' own: none is timed), no Autopilot read, no card
/// work, no text read — while its host is up, and after a Claude Code whose
/// card it decorated has left the screen. The footer's one timeline lock per
/// pane (whether its facts are Claude's) is all the pass takes.
///
/// NEGATIVE CONTROL: the same window with Claude Code's facts published
/// reads the session's Autopilot, and its first frame reads the pane's text
/// and composes its footer (the counters are wired to the pass).
#[test]
fn a_window_with_no_claude_session_costs_the_harness_nothing() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    // Claude Code leaves: a shell in front, its screen, no footer facts.
    leave(&app, session, false);
    feed(&app, session, &["$ ls".to_string()]);
    frame(&mut app, wid);
    assert!(
        !app.windows[&wid].claude_card.any_shown(),
        "no card on show"
    );
    for i in 0..20u64 {
        set_now(Some(
            t0 + Duration::from_secs(20) + Duration::from_millis(i),
        ));
        if i % 2 == 0 {
            keystroke(&mut app, wid, session, b'x');
        }
        assert_eq!(
            pass_cost(&mut app, wid),
            PassCost {
                timeline_locks: 1,
                ..PassCost::default()
            },
            "pass {i}: nothing at all"
        );
    }
    // NEGATIVE CONTROL: Claude Code's facts again — the pass reads them.
    publish(&app, session, VERSION);
    feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
    let claude = pass_cost(&mut app, wid);
    assert!(
        claude.autopilot_reads > 0 && claude.text_rebuilds == 1 && claude.allocations > 0,
        "CONTROL: a Claude Code pane is read ({claude:?})"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// A LAPSING CLAIM reads the clock ONCE a pass, however many readers ask:
/// the host's own lease (`ttl=`) is live only until its instant, so the
/// footer's key and its splice each read the claim clock once — shared by
/// the chips and the card — and a claim with no lapse reads none.
/// CONTROL: the claim lapsed, the card goes bare.
#[test]
fn a_lapsing_claim_reads_the_clock_once_a_pass() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let now_us = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        Some(now_us + 600_000_000),
        now_us,
    )
    .expect("the lease");
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = crate::work_counts::take_harness_clock_reads();
    let _ = app.claude_footer_fp(wid, &plan);
    assert_eq!(crate::work_counts::take_harness_clock_reads(), 1, "the key");
    app.splice_claude_footer(wid, &plan, route);
    assert_eq!(
        crate::work_counts::take_harness_clock_reads(),
        1,
        "the splice"
    );
    assert!(app.windows[&wid].claude_card.any_shown(), "still wrapped");
    // CONTROL: the lease lapsed.
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        Some(now_us.saturating_sub(1)),
        now_us,
    )
    .expect("a lapsed lease");
    app.splice_claude_footer(wid, &plan, route);
    assert!(
        !app.windows[&wid].claude_card.any_shown(),
        "CONTROL: a lapsed claim leaves the card bare"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// THE KEPT LAYOUT FOLLOWS THE CELLS: a pass over unchanged text lays
/// nothing out; text that moves beside the card (a word lands where the
/// kitty rode) is laid out anew on its frame — the kitty moves, the new
/// scene drawn once and then replayed — and the card's own text back again
/// is the first scene again, bit for bit. While a find bar is up (its tint
/// writes cells no revision moves) every pass lays the card out anew, and
/// none of those layouts is kept past it. CONTROL: the moved kitty differs
/// from the first.
#[test]
fn the_kept_layout_follows_the_cells() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    set_now(Some(t0 + Duration::from_secs(10)));
    let first = card_pic(&app, wid);
    assert!(
        !marks(&app, wid, FxSlot::Over).is_empty(),
        "the kitty and its stars"
    );
    assert_eq!(pass_cost(&mut app, wid).layouts, 0, "unchanged text: kept");
    // A word lands right of the card's banner: the kitty moves on past it.
    let mut moved = card_screen(24, 120, MODEL, VERSION);
    moved[1] = format!("{:<44}WORD", moved[1]);
    feed(&app, session, &moved);
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
    let cost = pass_cost(&mut app, wid);
    assert_eq!(
        (cost.layouts, cost.ticks),
        (1, 1),
        "laid out and drawn anew"
    );
    let shifted = card_layers(&app, wid);
    assert_ne!(card_pic(&app, wid), first, "CONTROL: the kitty moved");
    let again = pass_cost(&mut app, wid);
    assert_eq!(
        (again.layouts, again.ticks),
        (0, 0),
        "then kept and replayed"
    );
    assert_eq!(
        card_layers(&app, wid),
        shifted,
        "the same contents, replayed"
    );
    // The card's own text again: the first scene, bit for bit.
    feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
    assert_eq!(pass_cost(&mut app, wid).layouts, 1);
    assert_eq!(card_pic(&app, wid), first, "the first scene again");
    // A find bar up: laid out every pass, nothing kept past it.
    app.windows.get_mut(&wid).unwrap().search = Some(Default::default());
    for i in 0..3 {
        assert_eq!(pass_cost(&mut app, wid).layouts, 1, "find bar up, pass {i}");
    }
    app.windows.get_mut(&wid).unwrap().search = None;
    assert_eq!(
        pass_cost(&mut app, wid).layouts,
        1,
        "the bar gone: laid anew"
    );
    assert_eq!(pass_cost(&mut app, wid).layouts, 0, "then kept");
    assert_eq!(card_pic(&app, wid), first);
    set_now(None);
    host.shutdown_and_join();
}

/// The cells a lone [`layout`] of `key`'s card reads over `wid`'s frame as it
/// stands — the count a frame that lays that card anew may read, and a frame
/// that keeps it may read again, and no more.
fn layout_cells_read(app: &App, wid: WindowId, key: &LaidKey) -> usize {
    let scratch = &app.windows[&wid].input_scratch;
    let _ = crate::work_counts::take_card_cells_read();
    let laid = layout(
        &pane_cells(scratch, key),
        &key.place,
        kitty_width(key),
        line_texts(key.view, key.end),
        &mut Seen::default(),
    );
    assert!(laid.is_some(), "the card is laid out");
    let _ = crate::work_counts::take_card_layouts();
    crate::work_counts::take_card_cells_read()
}

/// The key the card on `wid` was last laid for.
fn laid_key(app: &App, wid: WindowId) -> LaidKey {
    app.windows[&wid].claude_card.entries[0]
        .laid
        .as_ref()
        .map(|(key, _)| *key)
        .expect("laid")
}

/// A CARD LAID ANEW READS EACH CELL ONCE: while Claude's output scrolls the
/// card down the screen a row a frame (still on show), every frame reads it
/// and lays it out anew — and tests exactly the cells that layout reads,
/// noted as it reads them (`Seen`): none is read again to keep the layout
/// for the next frame. The pass after, the same rebuild, reads none.
/// CONTROLS: the layout reads cells at all; the frame laid the card for the
/// key the count was taken for; the card is a fresh window's over each
/// screen. Mutation-checked: a scan of the cells after a fresh layout (the
/// fingerprint 675d61e96 took) fails here.
#[test]
fn a_card_laid_anew_reads_each_cell_once() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let base = card_screen(24, 120, MODEL, VERSION);
    // The card `down` rows lower, the composer kept at the bottom.
    let scrolled = |down: usize| {
        let mut rows = vec![String::new(); down];
        rows.extend_from_slice(&base[..base.len() - 4 - down]);
        rows.extend_from_slice(&base[base.len() - 4..]);
        rows
    };
    for down in 1..=4 {
        let rows = scrolled(down);
        feed(&app, session, &rows);
        set_now(Some(t0 + Duration::from_secs(10 + down as u64)));
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
        let place = aterm_phase::launch_card_place(&rows).expect("a card");
        let key = LaidKey {
            place,
            ..laid_key(&app, wid)
        };
        let own = layout_cells_read(&app, wid, &key);
        assert!(own > 0, "down {down}: CONTROL, the layout reads cells");
        let cost = pass_cost(&mut app, wid);
        let read = crate::work_counts::take_card_cells_read();
        assert_eq!(
            (cost.reads, cost.layouts),
            (1, 1),
            "down {down}: read and laid anew: {cost:?}"
        );
        assert_eq!(laid_key(&app, wid), key, "down {down}: CONTROL, the key");
        assert_eq!(read, own, "down {down}: each cell its layout reads, once");
        let again = pass_cost(&mut app, wid);
        assert_eq!(
            (again.layouts, crate::work_counts::take_card_cells_read()),
            (0, 0),
            "down {down}: the same rebuild reads nothing"
        );
        let kept = card_glass(&app, wid);
        assert_eq!(
            kept,
            fresh_card(120, &|app, session| feed(app, session, &rows)),
            "down {down}: a fresh window's card"
        );
    }
    set_now(None);
    host.shutdown_and_join();
}

/// A KEPT LAYOUT IS CHECKED BY THE CELLS ITS LAYOUT READ, AND NO OTHERS: a
/// keystroke under the settled card rebuilds the pane's text, and the card
/// keeps its layout by testing again exactly the cells its layout read — the
/// count a fresh layout of the same screen reads, no run to the border, no
/// cell twice — and in a pane twice as wide, the same count: the layout reads
/// the kitty's rows only as far as decides where it sits. CONTROLS: the check
/// runs (it reads cells; nothing is read or laid anew). Mutation-checked: the
/// kitty's run read to the border, or a kept layout checked by every cell to
/// the border (675d61e96's fingerprint), fails here.
#[test]
fn a_kept_layout_reads_only_the_cells_its_layout_read() {
    let checked = |cols: u16| {
        let t0 = Instant::now();
        let (mut app, wid, session, host) = settled_glass(cols, t0);
        set_now(Some(t0 + Duration::from_secs(10)));
        let mut each = Vec::new();
        for key in *b"typed" {
            keystroke(&mut app, wid, session, key);
            let own = layout_cells_read(&app, wid, &laid_key(&app, wid));
            let cost = pass_cost(&mut app, wid);
            let read = crate::work_counts::take_card_cells_read();
            assert_eq!(
                (cost.reads, cost.layouts, cost.text_rebuilds),
                (0, 0, 1),
                "{cols} columns: kept over a rebuilt text: {cost:?}"
            );
            assert!(read > 0, "{cols} columns: CONTROL, the cells are tested");
            assert_eq!(read, own, "{cols} columns: the layout's own cells");
            each.push(read);
        }
        set_now(None);
        host.shutdown_and_join();
        assert!(each.windows(2).all(|w| w[0] == w[1]), "{each:?}");
        each[0]
    };
    let (narrow, wide) = (checked(120), checked(240));
    assert_eq!(wide, narrow, "twice the width, the same cells");
}

/// What a card puts on the glass: its layers by value, its ink and star, and each line
/// it writes (frame row, column, cells).
#[derive(Debug, PartialEq)]
struct CardGlass {
    layers: Vec<LayerPic>,
    ink: Vec<InkCell>,
    decos: Vec<WordDecoration>,
    lines: Vec<(usize, usize, Vec<RenderCell>)>,
}

fn card_glass(app: &App, wid: WindowId) -> CardGlass {
    let p = painted(app, wid);
    CardGlass {
        layers: card_pic(app, wid),
        ink: p.ink.clone(),
        decos: p.decos.clone(),
        lines: app.windows[&wid]
            .claude_card
            .lines()
            .map(|l| (l.frame_row, l.col, l.cells.clone()))
            .collect(),
    }
}

/// The card a FRESH window `cols` wide draws, settled, over the screen
/// `paint` draws: what a card kept across frames must be, bit for bit.
fn fresh_card(cols: u16, paint: &dyn Fn(&App, u64)) -> CardGlass {
    let (mut app, wid, session) = glass(cols);
    paint(&app, session);
    let host = supervise(&mut app, session);
    let t0 = Instant::now();
    for t in [0, FRAME_MS, EPISODE_MS + 1] {
        set_now(Some(t0 + Duration::from_millis(t)));
        frame(&mut app, wid);
    }
    let glass = card_glass(&app, wid);
    host.shutdown_and_join();
    glass
}

/// EVERY CHANGE THE CARD READS IS READ, AND THE CARD IS A FRESH WINDOW'S:
/// one settled card is taken through a run of screens, each framed and then
/// held for a second pass, and after each its glass — light, ink, star and
/// line cells — is the glass a fresh window draws over the same screen, bit
/// for bit. A screen that moved nothing the card reads (the same screen
/// again, a keystroke in the composer, a row of output under it) costs no
/// reading and no layout; a cell in each region the layout reads — the row
/// under the card where the line goes, the card's rows right of their text
/// where the kitty runs, the rows over and under the box (its air) — is
/// laid anew, and so is a COLOUR-ONLY change of one (a coloured space is no
/// blank; its row's text is the same), a combining mark on a space there (a
/// side channel, the text the same) and a wide glyph; a card row whose last
/// glyph turns wide or narrow again with as many chars (the same place: only
/// the glyph's spacer cell says the row is a cell longer) is laid anew; a
/// newer card printed below, the card scrolled, its own words edited and a
/// resize read it anew; and the pass after each keeps what it made. An
/// anchor right of where the line goes, or right of the box over it, is past
/// what the layout reads, and costs nothing.
/// CONTROL: the comparisons are not vacuous — the line leaves the row under
/// the card, the kitty moves, the border closes on the card over words or a
/// notice, the card follows a scroll (each step's glass differs from the
/// settled one's). Mutation-checked: a kept layout blind to colour, to the
/// side channels, to the row under the card, to the kitty's rows, to the
/// air's rows or to a card row's wide last glyph (`Seen`'s wide tests), and a
/// reading kept across a changed card row or a newer banner, each fail here.
#[test]
fn a_change_the_card_reads_lays_it_as_a_fresh_window_would() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let base = card_screen(24, 120, MODEL, VERSION);
    let settled = card_glass(&app, wid);
    let edit = |k: usize, row: String| {
        let mut rows = base.clone();
        rows[k] = row;
        rows
    };
    let pad = |k: usize, at: usize, tail: &str| format!("{:<at$}{tail}", base[k]);
    // A space in background colour `bg` (SGR) at pane cell (row, col), the
    // cursor kept: no blank, though its row's text reads the same.
    let space = |bg: u8, row: usize, col: usize| {
        format!("\x1b7\x1b[{};{}H\x1b[{bg}m \x1b[0m\x1b8", row + 1, col + 1)
    };
    // `text` at pane cell (row, col), the cursor kept.
    let at = |row: usize, col: usize, text: &str| {
        format!("\x1b7\x1b[{};{}H{text}\x1b8", row + 1, col + 1)
    };
    let mut newer = base.clone();
    newer[10..13].clone_from_slice(&base[1..4]);
    let mut scrolled = base[1..base.len() - 4].to_vec();
    scrolled.push(String::new());
    scrolled.extend_from_slice(&base[base.len() - 4..]);
    let mut composer = base.clone();
    composer[base.len() - 3] = "\u{276F} keep going".into();
    let mut output = base.clone();
    output[9] = "\u{23FA} Here is what changed in the footer.".into();
    // Each step: what it is, the screen's rows, bytes painted over them, the
    // readings and layouts its frame costs, and whether the card moved from
    // the settled one.
    type Step<'a> = (&'a str, Vec<String>, String, (usize, usize), bool);
    let steps: Vec<Step> = vec![
        (
            "the same screen",
            base.clone(),
            String::new(),
            (0, 0),
            false,
        ),
        ("a keystroke", composer, String::new(), (0, 0), false),
        (
            "output under the card",
            output,
            String::new(),
            (0, 0),
            false,
        ),
        (
            "words where the line goes",
            edit(4, pad(4, 40, "word")),
            String::new(),
            (0, 1),
            true,
        ),
        ("back", base.clone(), String::new(), (0, 1), false),
        (
            "words over the card",
            edit(0, "x".into()),
            String::new(),
            (0, 1),
            true,
        ),
        ("back", base.clone(), String::new(), (0, 1), false),
        (
            "an anchor right of the box over the card",
            edit(0, pad(0, 100, "zz")),
            String::new(),
            (0, 0),
            false,
        ),
        (
            "a notice under the box",
            edit(5, "  Get to finished work sooner".into()),
            String::new(),
            (0, 1),
            true,
        ),
        ("back", base.clone(), String::new(), (0, 1), false),
        (
            "an anchor right of the line",
            edit(4, pad(4, 100, "zz")),
            String::new(),
            (0, 0),
            false,
        ),
        (
            "a coloured space where the line goes",
            edit(4, pad(4, 100, "zz")),
            space(44, 4, 40),
            (0, 1),
            true,
        ),
        (
            "words where the kitty runs",
            edit(2, pad(2, 60, "WORD")),
            String::new(),
            (1, 1),
            true,
        ),
        (
            "a coloured anchor right of the kitty",
            base.clone(),
            space(41, 2, 112),
            (1, 1),
            false,
        ),
        (
            "a coloured space where the kitty runs",
            base.clone(),
            space(41, 2, 112) + &space(44, 2, 50),
            (0, 1),
            true,
        ),
        (
            "a coloured anchor right of the banner",
            base.clone(),
            space(41, 1, 112),
            (1, 1),
            false,
        ),
        (
            "a coloured space beside the banner",
            base.clone(),
            space(41, 1, 112) + &space(44, 1, 50),
            (0, 1),
            true,
        ),
        (
            "a plain space where the kitty runs",
            base.clone(),
            at(2, 50, " "),
            (1, 1),
            false,
        ),
        (
            "a combining mark on it",
            base.clone(),
            at(2, 50, " \u{301}"),
            (0, 1),
            true,
        ),
        (
            "a wide glyph where the line goes",
            base.clone(),
            at(4, 40, "\u{4E2D}"),
            (1, 1),
            true,
        ),
        (
            "the model row's last glyph wide",
            card_screen(24, 120, MODEL_WIDE_END, VERSION),
            String::new(),
            (1, 1),
            true,
        ),
        (
            "and narrow again",
            base.clone(),
            String::new(),
            (1, 1),
            false,
        ),
        ("a newer card below", newer, String::new(), (1, 1), true),
        ("the card scrolled", scrolled, String::new(), (1, 1), true),
        (
            "its words edited",
            card_screen(24, 120, "Sonnet 5.5", VERSION),
            String::new(),
            (1, 1),
            true,
        ),
        (
            "the settled card again",
            base.clone(),
            String::new(),
            (1, 1),
            false,
        ),
    ];
    for (i, (what, rows, extra, (reads, layouts), moved)) in steps.into_iter().enumerate() {
        let paint = |app: &App, session: u64| {
            feed(app, session, &rows);
            let term = app.pool.get(session).expect("session").term.clone();
            crate::term_lock(&term).process(extra.as_bytes());
        };
        paint(&app, session);
        set_now(Some(t0 + Duration::from_secs(10 + i as u64)));
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
        let cost = pass_cost(&mut app, wid);
        assert_eq!(
            (cost.reads, cost.layouts),
            (reads, layouts),
            "{what}: {cost:?}"
        );
        let kept = card_glass(&app, wid);
        assert_eq!(
            kept,
            fresh_card(120, &paint),
            "{what}: a fresh window's card"
        );
        assert_eq!(kept != settled, moved, "{what}: CONTROL, the card moved");
        set_now(Some(
            t0 + Duration::from_secs(10 + i as u64) + Duration::from_millis(500),
        ));
        let again = pass_cost(&mut app, wid);
        assert_eq!(
            (again.reads, again.layouts, again.ticks),
            (0, 0, 0),
            "{what}: then kept"
        );
        assert_eq!(card_glass(&app, wid), kept, "{what}: then kept");
    }
    // A resize: the pane's rows and columns move, and the card is read and
    // laid anew for them.
    assert!(app.apply_term_resize(wid, 24, 100));
    let narrower = card_screen(24, 100, MODEL, VERSION);
    feed(&app, session, &narrower);
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
    let cost = pass_cost(&mut app, wid);
    assert_eq!((cost.reads, cost.layouts), (1, 1), "a resize: {cost:?}");
    assert_eq!(
        card_glass(&app, wid),
        fresh_card(100, &|app, session| feed(app, session, &narrower)),
        "a resize: a fresh window's card"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// A split window 160 wide: its left pane a shell's screen, its right pane
/// (its cells at a column offset) Claude Code's with the card, the facts of
/// its process published; then `left` and `right` painted over the two.
/// Returns the window, the two sessions and the right pane's width.
fn split_card(left: &str, right: &str) -> (App, WindowId, u64, u64, usize) {
    let (mut app, wid, shell) = glass(160);
    let session = app.split_active_stub_tab(wid);
    let (rows, cols) = {
        let term = crate::term_lock(&app.pool.get(session).unwrap().term);
        (usize::from(term.rows()), usize::from(term.cols()))
    };
    let shell_cols = {
        let term = crate::term_lock(&app.pool.get(shell).unwrap().term);
        usize::from(term.cols())
    };
    let lines: Vec<String> = (0..rows).map(|r| format!("$ ls {r}")).collect();
    feed(&app, shell, &lines);
    publish(&app, session, VERSION);
    feed(&app, session, &card_screen(rows, cols, MODEL, VERSION));
    for (s, bytes) in [(shell, left), (session, right)] {
        let term = app.pool.get(s).expect("session").term.clone();
        crate::term_lock(&term).process(bytes.as_bytes());
    }
    assert!(
        shell_cols > 20 && cols > 60,
        "two panes: {shell_cols} | {cols}"
    );
    (app, wid, shell, session, cols)
}

/// THE SAME IN A SPLIT, the card's pane on the right (its cells at a column
/// offset, its neighbour's text up against its left edge): a settled card
/// kept across frames that the split composes anew each time draws what a
/// fresh split window draws over the same two screens, bit for bit. Words up
/// to the boundary in the neighbour's pane on the card's rows, and a wide
/// glyph in its last columns, cost the card no reading and no layout; in the
/// card's own pane a plain space where the kitty runs costs nothing (the
/// composed rows' text and cells read the same), a combining
/// mark put on that space — the text the same, the cell no blank — lays it
/// anew, a coloured space in its place keeps that (the cells the layout read
/// say what they said: the kitty still stops there), a wide glyph where the
/// line goes lays it anew, and the screens back again. CONTROL: the
/// comparisons are not vacuous (each change that moves the card moves its
/// glass). Mutation-checked: a kept layout's check blind to the side
/// channels, or to the pane's column offset, fails here.
#[test]
fn a_change_in_a_split_lays_the_card_as_a_fresh_split_would() {
    let fresh = |left: &str, right: &str| {
        let (mut app, wid, _, session, _) = split_card(left, right);
        let host = supervise(&mut app, session);
        let t0 = Instant::now();
        for t in [0, FRAME_MS, EPISODE_MS + 1] {
            set_now(Some(t0 + Duration::from_millis(t)));
            frame(&mut app, wid);
        }
        let glass = card_glass(&app, wid);
        host.shutdown_and_join();
        glass
    };
    let t0 = Instant::now();
    let (mut app, wid, shell, session, cols) = split_card("", "");
    let host = supervise(&mut app, session);
    for t in [0, FRAME_MS, EPISODE_MS + 1] {
        set_now(Some(t0 + Duration::from_millis(t)));
        frame(&mut app, wid);
    }
    let settled = card_glass(&app, wid);
    assert!(
        settled.layers.len() == 2 && settled.layers.iter().all(|l| !l.items.is_empty()),
        "the card and its kitty"
    );
    let shell_cols = usize::from(crate::term_lock(&app.pool.get(shell).unwrap().term).cols());
    let at = |row: usize, col: usize, text: &str| {
        format!("\x1b7\x1b[{};{}H{text}\x1b8", row + 1, col + 1)
    };
    let up_to_the_edge: String = (1..=4)
        .map(|r| at(r, shell_cols - 12, &"x".repeat(12)))
        .collect();
    let wide_at_the_edge = at(2, shell_cols - 2, "\u{4E2D}");
    let plain = at(2, 50, " ");
    let combining = at(2, 50, " \u{301}");
    let coloured = at(2, 50, "\x1b[44m \x1b[0m");
    let wide_line = at(4, 40, "\u{4E2D}");
    // Each step: the neighbour's bytes, the card pane's, the readings and
    // layouts the frame costs, and whether the card moved.
    type Step<'a> = (&'a str, String, String, (usize, usize), bool);
    let steps: Vec<Step> = vec![
        (
            "the same screens",
            String::new(),
            String::new(),
            (0, 0),
            false,
        ),
        (
            "words up to the edge",
            up_to_the_edge.clone(),
            String::new(),
            (0, 0),
            false,
        ),
        (
            "a wide glyph at the edge",
            up_to_the_edge.clone() + &wide_at_the_edge,
            String::new(),
            (0, 0),
            false,
        ),
        (
            "a plain space where the kitty runs",
            up_to_the_edge.clone(),
            plain,
            (0, 0),
            false,
        ),
        (
            "a combining mark on it",
            up_to_the_edge.clone(),
            combining,
            (0, 1),
            true,
        ),
        (
            "a coloured space instead",
            up_to_the_edge.clone(),
            coloured,
            (0, 0),
            true,
        ),
        (
            "a wide glyph where the line goes",
            up_to_the_edge.clone(),
            wide_line,
            (0, 1),
            true,
        ),
        ("back", String::new(), String::new(), (0, 1), false),
    ];
    let shell_lines: Vec<String> = (0..24).map(|r| format!("$ ls {r}")).collect();
    for (i, (what, left, right, (reads, layouts), moved)) in steps.into_iter().enumerate() {
        let rows = usize::from(crate::term_lock(&app.pool.get(session).unwrap().term).rows());
        feed(&app, shell, &shell_lines[..rows]);
        feed(&app, session, &card_screen(rows, cols, MODEL, VERSION));
        for (s, bytes) in [(shell, &left), (session, &right)] {
            let term = app.pool.get(s).expect("session").term.clone();
            crate::term_lock(&term).process(bytes.as_bytes());
        }
        set_now(Some(t0 + Duration::from_secs(10 + i as u64)));
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
        let cost = pass_cost(&mut app, wid);
        assert_eq!(
            (cost.reads, cost.layouts),
            (reads, layouts),
            "{what}: {cost:?}"
        );
        let kept = card_glass(&app, wid);
        assert_eq!(kept, fresh(&left, &right), "{what}: a fresh split's card");
        assert_eq!(kept != settled, moved, "{what}: CONTROL, the card moved");
    }
    set_now(None);
    host.shutdown_and_join();
}

/// A WINDOW HIDDEN MID-EPISODE (minimized, occluded: its splice never runs
/// again) keeps its card on show, playing, and the loop's two judges read
/// the clock on every wake while the episode could still want a frame —
/// the window may come back. Past the episode and its settle grace the card
/// wants no frame at any later instant until a splice ticks it, and the
/// first wake that finds so lets it go: every wake after costs no clock read
/// and asks the window server nothing. Shown again, its frame settles the
/// card; focus regained plays it again.
/// CONTROLS: mid-episode the hidden window's frame is wanted and the judges
/// ask (two clock reads a wake); one shown again mid-episode is due at once;
/// and without the let-go every later wake reads the clock twice, for good
/// (mutation-checked).
#[test]
fn a_window_hidden_mid_episode_costs_the_wakes_nothing() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    set_now(Some(t0));
    frame(&mut app, wid);
    let hide = |app: &mut App, hidden: bool| app.windows.get_mut(&wid).unwrap().occluded = hidden;
    // One loop wake's judges at `now`: what they ask for, and what they cost.
    let wake = |app: &mut App, now: Instant| {
        set_now(Some(now));
        let _ = (
            crate::work_counts::take_harness_clock_reads(),
            crate::work_counts::take_on_screen_asks(),
        );
        let judged = (app.claude_card_redraws(), app.claude_card_wake());
        (
            judged,
            crate::work_counts::take_harness_clock_reads(),
            crate::work_counts::take_on_screen_asks(),
        )
    };
    hide(&mut app, true);
    assert_eq!(
        wake(&mut app, at(100)),
        ((vec![], None), 2, 2),
        "CONTROL: mid-episode, the hidden window's frame is wanted and asked about"
    );
    // CONTROL: shown again mid-episode, its frame is due at once.
    hide(&mut app, false);
    assert_eq!(
        wake(&mut app, at(110)).0.0,
        vec![wid],
        "CONTROL: due when shown"
    );
    frame(&mut app, wid);
    hide(&mut app, true);
    let past = EPISODE_MS + aterm_effects::harness_card::SETTLE_GRACE_MS + 10;
    assert_eq!(
        wake(&mut app, at(past)),
        ((vec![], None), 1, 0),
        "the wake that finds no frame wanted any more"
    );
    for i in 1..=100 {
        assert_eq!(
            wake(&mut app, at(past + 7 * i)),
            ((vec![], None), 0, 0),
            "wake {i} after: nothing"
        );
    }
    assert!(app.windows[&wid].claude_card.any_shown(), "still on show");
    // Shown again: its frame draws the card settled, and it stays at rest.
    hide(&mut app, false);
    set_now(Some(at(past + 1000)));
    frame(&mut app, wid);
    assert!(
        app.windows[&wid]
            .claude_card
            .entries
            .iter()
            .all(|e| e.card.at_rest()),
        "settled"
    );
    assert_eq!(wake(&mut app, at(past + 1100)), ((vec![], None), 0, 0));
    // Focus regained: the episode again.
    let back = at(past + 2000);
    app.rearm_claude_cards(wid, || back);
    assert_eq!(wake(&mut app, back).0, (vec![wid], None), "playing again");
    set_now(None);
    host.shutdown_and_join();
}

/// AN EPISODE CUT OFF BY LOST FOCUS RESTS AT NO COST: focus lost 66 ms into
/// the episode, the next frame draws the card still — its settled frame,
/// once — and the episode keeps its clock, held back. A minute on, frame
/// after frame costs the harness nothing over the bare footer's pass: the
/// held card is replayed (no tick), no clock read, its line kept.
/// NEGATIVE CONTROL: the same window still focused 66 ms in draws the
/// episode's frame on the clock; and a replay that refused a held-back
/// playing episode (before 82f408659) ticks every frame a minute on
/// (mutation-checked).
#[test]
fn an_episode_cut_off_by_lost_focus_rests_at_no_cost() {
    for lose_focus in [false, true] {
        let t0 = Instant::now();
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        let (mut bare, bare_wid, _) = bare_glass(120);
        bare.windows.get_mut(&bare_wid).unwrap().focused = false;
        for t in [0, FRAME_MS] {
            set_now(Some(t0 + Duration::from_millis(t)));
            frame(&mut app, wid);
        }
        app.windows.get_mut(&wid).unwrap().focused = !lose_focus;
        set_now(Some(t0 + Duration::from_millis(2 * FRAME_MS)));
        let cut = pass_cost(&mut app, wid);
        assert_eq!(
            cut.ticks, 1,
            "lose focus {lose_focus}: 66 ms in, a frame is drawn"
        );
        if !lose_focus {
            // NEGATIVE CONTROL: focused, that frame is the episode's.
            assert!(cut.clock_reads > 0, "CONTROL: on the clock");
            assert!(!app.windows[&wid].claude_card.entries[0].card.is_still());
            host.shutdown_and_join();
            continue;
        }
        let held = &app.windows[&wid].claude_card.entries[0].card;
        assert!(
            held.at_rest() && !held.is_still(),
            "held back, its clock kept"
        );
        let layers = card_layers(&app, wid);
        let _ = pass_cost(&mut bare, bare_wid);
        for i in 0..20u64 {
            set_now(Some(
                t0 + Duration::from_secs(60) + Duration::from_millis(7 * i),
            ));
            let floor = pass_cost(&mut bare, bare_wid);
            assert_eq!(pass_cost(&mut app, wid), floor, "a minute on, frame {i}");
            assert_eq!(card_layers(&app, wid), layers, "frame {i}: the held frame");
        }
        set_now(None);
        host.shutdown_and_join();
    }
}

/// A zone re-derivation can keep HH:MM and another-day presence while
/// changing the weekday. The same timer that changes the card's words
/// must move the footer key that admits their next composition.
#[test]
fn a_timer_weekday_change_moves_the_footer_key_with_its_card_words() {
    use crate::autopilot_timers::AutopilotTimers;
    use crate::claude_lights::{ChipGlass, WindowLights};

    let now = Instant::now();
    let wall = 20_000 * 86_400_000_i64 + 12 * 3_600_000;
    let at = wall + 86_400_000;
    let session = 17;
    let mut timers = AutopilotTimers::default();
    let lights = WindowLights::default();
    let west = |_: i64| Some(-10 * 3600);
    let east = |_: i64| Some(14 * 3600);
    let term = |glass| {
        lights.autopilot_term(
            session,
            Some(Autopilot::Cruise),
            AutopilotView::Cruise,
            ChipGlass {
                land_at: glass,
                ..ChipGlass::default()
            },
        )
    };
    let words = |glass| {
        line_texts(
            AutopilotView::Cruise,
            LineEnd::Mode.timed(AutopilotView::Cruise, glass),
        )
    };

    assert!(timers.arm(session, at, now, wall, &west, None));
    let before = timers.glass(session).expect("armed face");
    let before_key = term(Some(before));
    let before_words = words(Some(before));
    assert_ne!(before_key, term(None), "CONTROL: arming moves the key");
    assert_ne!(before_words, words(None), "CONTROL: armed timer words");
    assert!(timers.rederive(now, wall, &west).is_empty());
    assert_eq!(term(timers.glass(session)), before_key);
    assert_eq!(words(timers.glass(session)), before_words);

    assert_eq!(timers.rederive(now, wall, &east), vec![session]);
    let after = timers.glass(session).expect("re-derived face");
    assert_eq!(before.clock, after.clock, "the same HH:MM");
    assert!(before.other_day.is_some() && after.other_day.is_some());
    assert_ne!(before.other_day, after.other_day);
    assert_ne!(words(Some(after)), before_words, "the card's words moved");
    assert_ne!(
        term(Some(after)),
        before_key,
        "TIMER_WEEKDAY_DISPLAY_MUST_MOVE_FOOTER_KEY"
    );
}

/// DRIFT IS A FACT FOR LATER, NEVER A GATE (card engine §3.10, §3.8). The
/// Store drawer image names one build — its banner prints it — and the
/// registry row reports another: one exception entry, however many footer
/// wakes follow, and the card is decorated exactly where, and as, it is
/// where the two agree: the banner is held to its image's build, never to
/// the row's. CONTROL: the image naming the row's own build raises nothing.
#[test]
fn a_build_drift_is_one_entry_and_leaves_the_decoration_alone() {
    use aterm_agent::harness::footer::FactsOwner;

    let run = |image: &str| {
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        // The group's image `exec`s into the build `image`, which paints its
        // own card.
        launch(&app, session, image, PGID, 5);
        feed(&app, session, &card_screen(24, 120, MODEL, image));
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            // The row the image wrote, read after its publication.
            timeline.set_claude_footer(
                PGID,
                None,
                Some(FooterFacts {
                    model: Some("Opus 5.5".into()),
                    version: Some(VERSION.into()),
                    owner: Some(FactsOwner {
                        floor: None,
                        session_id: "00000000-0000-4000-8000-000000000a52".into(),
                    }),
                    ..FooterFacts::default()
                }),
            );
        }
        app.on_claude_footer_changed(session);
        app.on_claude_footer_changed(session);
        frame(&mut app, wid);
        let strip = strip_rows(&app, wid);
        let line = glass_text(&app, wid, strip + 4, 0..120).contains("aterm harness");
        let entries = app
            .messages
            .digest()
            .entries
            .iter()
            .filter(|entry| entry.key.ends_with(".build-drift"))
            .count();
        let place = decorated(&app, wid);
        host.shutdown_and_join();
        (place, line, entries)
    };
    let (agreed, agreed_line, none) = run(VERSION);
    assert!(
        agreed.is_some() && agreed_line,
        "CONTROL: the card decorates"
    );
    assert_eq!(none, 0, "CONTROL: agreeing builds raise nothing");
    let (drifted, drifted_line, one) = run("2.1.299");
    assert_eq!(one, 1, "one entry, however many wakes");
    assert_eq!(
        (drifted, drifted_line),
        (agreed, agreed_line),
        "the decoration is the same"
    );
}

/// The real append/strip seams preserve another producer, even with the same
/// id or an equal duplicate; menu occlusion uses the shared renderer path.
#[test]
fn a_card_layer_survives_a_menu_cut_and_is_withdrawn_once() {
    use aterm_core::render::{FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace};
    use aterm_effects::harness_card::CardFrame;
    let content = |color, x, y, w, h| {
        Arc::new(FxContent::new(
            vec![FxItem::Rect(FxRect { x, y, w, h, ink: 0 })],
            vec![FxInk::over(color, 255); 2],
        ))
    };
    let frame = CardFrame {
        under: content(0xe02014, 0, 0, 8, 6),
        over: content(0xc0ae10, 2, 1, 4, 4),
        origin: (0, 0),
    };
    let mut cards = WindowCards::default();
    let mut input = RenderInput {
        cells: vec![vec![RenderCell::default(); 8]; 3],
        cols: 8,
        ..RenderInput::default()
    };
    append_card_frame(
        &mut cards.painted,
        &mut input,
        7,
        frame,
        PxRect::new(0, 0, 8, 6),
        (0, 0),
    );
    assert_eq!(cards.painted.layers.len(), 2);
    let equal_peer = cards.painted.layers[0].clone();
    let same_id_peer = FxLayer::new(
        equal_peer.id,
        content(0x345678, 7, 5, 1, 1),
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Exact,
    );
    input.fx_layers.insert(0, equal_peer.clone());
    input.fx_layers.push(same_id_peer.clone());
    let f = aterm_render::FxFrame {
        w: 8,
        h: 6,
        pad: 0,
        grid_top: 0,
        cell_h: 2,
        rows: 3,
    };
    let mut before = vec![0x010203; 48];
    aterm_render::fx::paint_layers(&mut before, &f, &input.fx_layers, &[]);
    let menu = crate::tab_menu::MenuRect {
        row: 1,
        col: 3,
        w: 2,
        h: 1,
    };
    cut_out(&mut cards, &mut input, menu);
    crate::app_render::scrub_overlay_card(&mut input, menu, (1, 2), (0, 0));
    let mut after = vec![0x010203; 48];
    aterm_render::fx::paint_layers(&mut after, &f, &input.fx_layers, &input.fx_occluders);
    for y in 0..6 {
        for x in 0..8 {
            assert_eq!(
                after[y * 8 + x],
                if (2..4).contains(&y) && (3..5).contains(&x) {
                    0x010203
                } else {
                    before[y * 8 + x]
                }
            );
        }
    }
    assert_ne!(
        before[2 * 8 + 3],
        0x010203,
        "the occluder actually cuts painted pixels"
    );
    strip(&mut cards, &mut input);
    assert!(cards.painted.is_empty());
    assert_eq!(input.fx_layers, [equal_peer, same_id_peer]);
    assert_eq!(
        input.fx_occluders.len(),
        1,
        "chrome owns its occluder, not the card record"
    );
    let peers = input.fx_layers.clone();
    strip(&mut cards, &mut input);
    assert_eq!(
        input.fx_layers, peers,
        "a consumed record removes nothing again"
    );
}

/// The gate switch's laws (card engine §7 "A, the gate"), on this module's
/// fixtures.
#[path = "claude_card_gate_tests.rs"]
mod gate;

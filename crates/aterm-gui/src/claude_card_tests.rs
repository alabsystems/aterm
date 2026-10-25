// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness card on the glass: a real engine, a real frame through the capture route's splices, the real splice — and the layout's rules on hand-made cells.

use super::*;

use std::sync::Arc;
use std::time::Duration;

use aterm_agent::harness::footer::FooterFacts;
use aterm_agent::supervise::Autopilot;
use aterm_core::render::{FxInk, FxItem, FxLayer, FxRamps, FxRect, FxSlot, FxSpace, PxRect};
use aterm_core::terminal::{CutKind, HolderCut};
use aterm_effects::harness_card::{EPISODE_MS, FLOW_SLACK_MS, KITTY_W};
use aterm_effects::ripple::{FLOW_STEP_MS, GLINT_PERIOD_MS};

const VERSION: &str = "2.1.283";
const PGID: i32 = 4242;

const MODEL: &str = "Opus 5.5 \u{b7} Claude Max";

const MODEL_WIDE_END: &str = "Opus 5.5 \u{b7} Claude Ma\u{4E2D}";

const GRAY: [u8; 3] = [130, 130, 130];

fn banner() -> &'static str {
    aterm_phase::anchor!("launch.banner")
}

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

fn cut(app: &App, session: u64, to: Option<u64>, kind: CutKind, from_alive: bool) {
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).note_holder_cut(HolderCut {
        to,
        kind,
        from_alive,
    });
}

fn publish(app: &App, session: u64, version: &str) {
    publish_as(app, session, version, PGID);
}

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

fn frame(app: &mut App, wid: WindowId) {
    app.capture_words(wid, Instant::now());
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    app.splice_tab_strip(wid);
    app.splice_claude_footer(wid, &plan, route);
}

const SHELL_PGID: i32 = 4000;

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

fn strip_rows(app: &App, wid: WindowId) -> usize {
    app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows)
}

fn glass_text(app: &App, wid: WindowId, row: usize, cols: std::ops::Range<usize>) -> String {
    app.windows[&wid].input_scratch.cells[row]
        .iter()
        .skip(cols.start)
        .take(cols.len())
        .map(|c| c.ch)
        .collect()
}

fn painted(app: &App, wid: WindowId) -> &CardPainted {
    &app.windows[&wid].claude_card.painted
}

fn card_drew(app: &App, wid: WindowId) -> bool {
    let p = painted(app, wid);
    card_only(app, wid).next().is_some()
        || !p.under.is_empty()
        || !p.halo.is_empty()
        || !p.ink.is_empty()
        || !p.decos.is_empty()
}

fn card_only(app: &App, wid: WindowId) -> impl Iterator<Item = &FxLayer> {
    let mark = app
        .focused_session_id(wid)
        .map(crate::claude_footer::top_mark_id);
    painted(app, wid)
        .layers
        .iter()
        .filter(move |l| Some(l.id) != mark)
}

fn card_layer(app: &App, wid: WindowId, slot: FxSlot) -> Option<FxLayer> {
    card_only(app, wid).find(|l| l.slot == slot).cloned()
}

fn card_layers(app: &App, wid: WindowId) -> Vec<FxLayer> {
    card_only(app, wid).cloned().collect()
}

fn grid_origin(app: &App, wid: WindowId) -> (i32, i32) {
    (
        pixel(app.win_pad(wid)),
        pixel(app.win_pad_top(wid) + app.win_head(wid)),
    )
}

#[derive(Clone, Debug, PartialEq)]
struct Mark {
    at: PxRect,
    inks: Vec<FxInk>,
    art: Option<(u128, u16)>,
}

impl Mark {
    fn moved(&self, dx: i32, dy: i32) -> Self {
        Self {
            at: self.at.translate(dx, dy),
            ..self.clone()
        }
    }
}

fn marks(app: &App, wid: WindowId, slot: FxSlot) -> Vec<Mark> {
    let grid = grid_origin(app, wid);
    let mut out = Vec::new();
    for layer in card_only(app, wid).filter(|l| l.slot == slot) {
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

fn card_marks(app: &App, wid: WindowId) -> (Vec<Mark>, Vec<Mark>) {
    (
        marks(app, wid, FxSlot::Under),
        marks(app, wid, FxSlot::Over),
    )
}

fn is_kitty(m: &Mark) -> bool {
    m.art.is_some_and(|(_, w)| usize::from(w) == KITTY_W) && m.inks.iter().all(|i| i.alpha == 255)
}

fn kitty_mark((_, over): &(Vec<Mark>, Vec<Mark>)) -> Option<&Mark> {
    over.iter().find(|m| is_kitty(m))
}

fn kitty_x(card: &(Vec<Mark>, Vec<Mark>)) -> Option<i32> {
    kitty_mark(card).map(|m| m.at.x0)
}

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

fn hull(marks: &[Mark]) -> PxRect {
    marks.iter().fold(PxRect::EMPTY, |h, m| h.hull(m.at))
}

#[derive(Clone, Debug, PartialEq)]
struct LayerPic {
    slot: FxSlot,
    space: FxSpace,
    offset: (i32, i32),
    clip: PxRect,
    items: Vec<ItemPic>,
    palette: Vec<FxInk>,
}

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

fn card_pic(app: &App, wid: WindowId) -> Vec<LayerPic> {
    card_only(app, wid).map(layer_pic).collect()
}

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
    let layers: Vec<FxLayer> = card_only(app, wid).cloned().collect();
    aterm_render::fx::paint_layers(&mut px, &fx, &layers, occluders);
    (px, fx.w)
}

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

fn expected_line(view: AutopilotView, at: usize, cols: usize) -> String {
    line_texts(view, LineEnd::Mode)
        .iter()
        .find(|t| at + LINE_LEAD + text_cells(t) < cols)
        .expect("a line fits")
        .clone()
}

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
    let (u, o) = (
        card_layer(&app, wid, FxSlot::Under).expect("the under layer"),
        card_layer(&app, wid, FxSlot::Over).expect("the over layer"),
    );
    assert_eq!((u.space, o.space), (FxSpace::Window, FxSpace::Grid));
    assert_eq!((o.offset.0 + grid.0, o.offset.1 + grid.1), u.offset);
    assert_eq!(o.clip.translate(grid.0, grid.1), u.clip);
    let border = hull(&under);
    assert_eq!(
        (border.y0, border.y1),
        (
            y_px(strip + top) - pixel(ch / 2),
            y_px(strip + top + 4) + pixel(ch / 2)
        ),
        "the border stands in the box's air"
    );
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
    let hero = start + 12..start + 17;
    let word: String = scratch.cells[line_row][hero.clone()]
        .iter()
        .map(|c| c.ch)
        .collect();
    assert_eq!(word, HERO_WORD, "the hero word's cells");
    for col in start..start + want.chars().count() {
        assert_eq!(
            scratch.cells[line_row][col].bold,
            hero.contains(&col),
            "col {col}: bold on the hero word alone"
        );
    }
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

#[test]
fn the_cards_glass_is_the_pets_occupied_ground() {
    let covered = |app: &App, wid: WindowId, row: usize, col: usize| {
        let (r, c) = (row as f32 + 0.5, col as f32 + 0.5);
        app.windows[&wid].pet_chrome.held().iter().any(|rect| {
            rect.row <= r && r < rect.row + rect.rows && rect.col <= c && c < rect.col + rect.cols
        })
    };
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

fn decorated(app: &App, wid: WindowId) -> Option<CardPlace> {
    let on = app.windows[&wid]
        .claude_card
        .entries
        .iter()
        .any(|e| e.shown)
        && !marks(app, wid, FxSlot::Under).is_empty();
    on.then(|| laid_key(app, wid).place)
}

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

/// The glass's marks (`BandIcon::glass_mark`) the frame draws on frame row
/// `row`.
fn drawn_marks(app: &App, wid: WindowId, row: usize) -> Vec<aterm_core::render::ChromeIcon> {
    app.windows[&wid]
        .input_scratch
        .chrome_rasters
        .iter()
        .filter(|raster| usize::from(raster.row) == row)
        .flat_map(|raster| raster.icons.iter().copied())
        .filter(|i| aterm_core::render::BandIcon::glass_mark(i.icon.ch()).is_some())
        .collect()
}

/// THE CARD'S LINE DRAWS ITS MODE'S MARK AS THE GLASS'S OWN (the one map,
/// `BandIcon::glass_mark`): Land's `· autopilot ⤓ land` (STIX Two Math's `⤓`
/// stood 7 × 13 at 1×, over the capitals and below the baseline) and Off's
/// `○ off` are drawn once at the line's cell that keeps the character, frame
/// after frame. NEGATIVE CONTROL: Cruise's `∞`, a glyph of the face's own in
/// the set's size, draws no icon on the line's row.
#[test]
fn the_cards_line_draws_its_modes_mark_as_the_glasss() {
    use aterm_core::render::{BandIcon, ChromeIcon};
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let row = strip_rows(&app, wid) + 4;
    let line = glass_text(&app, wid, row, 0..120);
    assert!(line.contains("autopilot \u{221e} cruise"), "{line:?}");
    assert_eq!(drawn_marks(&app, wid, row), [], "control: Cruise's ∞");
    for (mode, icon) in [
        (Autopilot::Land, BandIcon::ToBar),
        (Autopilot::Off, BandIcon::Ring),
    ] {
        assert!(app.set_session_autopilot(session, Some(mode), crate::autopilot::Surface::Chip));
        for k in 0..3 {
            frame(&mut app, wid);
            let cells = &app.windows[&wid].input_scratch.cells[row];
            let col = cells
                .iter()
                .position(|c| c.ch == icon.ch())
                .unwrap_or_else(|| panic!("{mode:?}: the line keeps {:?}", icon.ch()));
            let col = u16::try_from(col).expect("a column");
            assert_eq!(
                drawn_marks(&app, wid, row),
                [ChromeIcon { col, icon }],
                "{mode:?}, frame {k}: the line's mark, once"
            );
        }
    }
    host.shutdown_and_join();
}

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
        let versioned = |texts: &[String]| -> Vec<String> {
            texts[1..]
                .iter()
                .filter(|t| t.starts_with("enhanced by "))
                .cloned()
                .collect()
        };
        assert_eq!(
            versioned(texts),
            versioned(line_texts(AutopilotView::Land, LineEnd::Mode)),
            "the narrower versioned rungs"
        );
        let glyph = tail.split(' ').next().expect("a glyph");
        let short: Vec<&String> = texts
            .iter()
            .filter(|t| !t.starts_with("enhanced by "))
            .collect();
        assert_eq!(
            short,
            [
                &format!("{HERO_WORD} harness \u{b7} {tail}"),
                &format!("{HERO_WORD} \u{b7} {tail}"),
                &format!("{HERO_WORD} {glyph}"),
            ],
            "the short rungs say the end too"
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
        assert!(intro_over(&app, wid), "the intro over");
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
            assert!(
                intro_over(&app, wid),
                "{word:?}: {face:?}: a face's move arms no episode"
            );
            assert_ne!(
                app.claude_card_deadline(t),
                Some(t + Duration::from_millis(FRAME_MS)),
                "{word:?}: {face:?}: no intro frame wanted"
            );
        }
        set_now(None);
        host.shutdown_and_join();
    }
}

fn six_pm(other_day: bool) -> crate::claude_lights::LandAtGlass {
    crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: other_day.then_some(crate::claude_lights::Weekday::Sat),
    }
}

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

    for (rows_kind, agent, timed_first) in [
        (4, aterm_phase::CliHarness::Agy, false),
        (1, aterm_phase::CliHarness::Muse, true),
    ] {
        let first = timed_first.then(|| {
            line_texts_for(
                rows_kind,
                AutopilotView::Cruise,
                LineEnd::CruiseUntil(six_pm(false)),
            )
        });
        let cruise = line_texts_for(rows_kind, AutopilotView::Cruise, LineEnd::Mode);
        let head = format!(
            "enhanced by {HERO_WORD} harness v{} \u{b7} {}",
            agent.version_str(),
            agent.id()
        );
        if agent == aterm_phase::CliHarness::Agy {
            assert_eq!(cruise[0], head, "{cruise:?}");
        } else {
            assert!(
                cruise[0].starts_with(&format!("{head} \u{b7} autopilot")),
                "{cruise:?}"
            );
        }
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
                make_line_texts_for(agent, AutopilotView::Land, LineEnd::Mode),
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
    let build = crate::menu::version_signature(
        crate::menu::VersionFacts::this_build(),
        crate::menu::VersionRung::Full,
    )
    .expect("aterm's build signature");
    for harness in aterm_phase::CliHarness::ALL {
        let id = harness.id();
        let ver = harness.version_str();
        let texts = make_line_texts_for(harness, AutopilotView::Cruise, LineEnd::Mode);
        let head = format!("enhanced by {HERO_WORD} harness v{ver}");
        let named = match harness {
            aterm_phase::CliHarness::ClaudeCode => head.clone(),
            _ => format!("{head} \u{b7} {id}"),
        };
        assert!(
            texts[0].starts_with(&named),
            "{harness:?}: its own version beside the word harness: {texts:?}"
        );
        for text in texts {
            let versions = text
                .match_indices(" v")
                .filter(|&(at, _)| text[at + 2..].starts_with(|c: char| c.is_ascii_digit()))
                .count();
            assert!(versions <= 1, "{harness:?}: one version: {text:?}");
            assert!(
                !text.contains(&build),
                "{harness:?}: never aterm's build ({build}): {text:?}"
            );
        }
    }
    assert_eq!(
        make_line_texts(AutopilotView::Cruise, LineEnd::Mode)[0],
        format!(
            "enhanced by {HERO_WORD} harness v{} \u{b7} autopilot \u{221e} cruise",
            aterm_phase::CliHarness::ClaudeCode.version_str()
        ),
        "Claude Code's line: its harness's version, then the autopilot"
    );
}

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
    hold(&mut app, wid);
    assert_eq!(app.claude_card_deadline(t), None, "settled");
    let line_row = strip_rows(&app, wid) + 4;
    let start = 11 + LINE_LEAD;
    let read = |app: &App, words: &str| {
        glass_text(app, wid, line_row, start..start + words.chars().count())
    };
    let plain = expected_line(AutopilotView::Cruise, 11, 120);
    assert_eq!(read(&app, &plain), plain, "CONTROL: no timer, no time");
    assert!(!glass_text(&app, wid, line_row, 0..120).contains('\u{2913}'));
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
    assert!(app.autopilot_timers.forget(session));
    frame(&mut app, wid);
    assert_eq!(read(&app, &plain), plain, "the timer gone");
    assert!(!glass_text(&app, wid, line_row, 0..120).contains('\u{2913}'));
    set_now(None);
    host.shutdown_and_join();
}

type NegativeCase = (&'static str, fn(&mut App, u64));

#[test]
fn unsupervised_switched_off_another_build_scrolled_back_no_host_or_master_off_draw_nothing() {
    let bare = |app: &App, wid: WindowId| {
        let strip = strip_rows(app, wid);
        !card_drew(app, wid)
            && !glass_text(app, wid, strip + 4, 0..120).contains("aterm harness")
            && !app.windows[&wid]
                .claude_card
                .entries
                .iter()
                .any(|e| e.shown)
    };
    let cases: [NegativeCase; 7] = [
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
        ("agent switch off, word Off", agent_switch_off),
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

fn agent_switch_off(app: &mut App, session: u64) {
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.set_program(PGID, Some("claude".into()));
    }
    app.harness_policy =
        crate::harness_policy_cell::HarnessPolicyCell::new(crate::app_config::HarnessPolicy::read(
            "[harness]\nheadless = true\n[harness.claude]\nenabled = false\n",
        ));
    app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip,
    );
    let (word, view, supervised) = app.session_autopilot(session);
    assert_eq!(word, Some(Autopilot::Off));
    assert!(supervised, "the claim is still held");
    assert_eq!(
        view,
        AutopilotView::AgentOff(aterm_phase::Program::Claude),
        "the agent's own switch"
    );
}

#[test]
fn off_wears_the_white_card_without_a_claim() {
    let lit = |app: &mut App, wid: WindowId, session: u64| {
        let ctx = app.pool.get(session).unwrap().ctx.clone();
        app.set_session_autopilot(
            session,
            Some(Autopilot::Off),
            crate::autopilot::Surface::Chip,
        );
        assert_eq!(app.session_autopilot(session).0, Some(Autopilot::Off));
        crate::session_timeline::release_supervisor(
            &ctx,
            None,
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        );
        assert!(!app.session_autopilot(session).2, "the claim is given back");
        frame(app, wid);
    };
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    lit(&mut app, wid, session);
    let (under, over) = card_marks(&app, wid);
    assert!(!under.is_empty(), "the moonbow border and trail");
    assert!(!over.is_empty(), "the kitty and the stars");
    let strip = strip_rows(&app, wid);
    let line = glass_text(&app, wid, strip + 4, 0..120);
    assert!(
        line.contains("enhanced by aterm harness") && line.contains("autopilot \u{25cb} off"),
        "the line says Off: {line:?}"
    );
    let entry = app.windows[&wid]
        .claude_card
        .entries
        .iter()
        .find(|e| e.shown)
        .expect("the card on show");
    assert_eq!(entry.view, AutopilotView::Off);
    let (_, (_, scene)) = entry
        .laid
        .as_ref()
        .and_then(|(key, laid)| Some((key, laid.as_ref()?)))
        .expect("laid");
    assert_eq!(scene.palette, aterm_effects::harness_card::CardPalette::Off);
    host.shutdown_and_join();
    let cases: [NegativeCase; 2] = [
        ("master switch off", |app, _| {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
            );
        }),
        ("agent switch off", agent_switch_off),
    ];
    for (what, case) in cases {
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        case(&mut app, session);
        lit(&mut app, wid, session);
        let strip = strip_rows(&app, wid);
        assert!(!card_drew(&app, wid), "{what}: no light");
        assert!(
            !glass_text(&app, wid, strip + 4, 0..120).contains("aterm harness"),
            "{what}: no line"
        );
        host.shutdown_and_join();
    }
}

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
    assert!(card_drew(&app, wid));
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

#[test]
fn a_capture_resplice_never_doubles_the_light() {
    for tab_strip in [0, 1] {
        capture_resplice(tab_strip);
    }
}

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
    assert!(card_drew(&app, wid));
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

#[test]
fn the_card_flows_a_frame_a_step_and_held_keys_constant() {
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
        assert_ne!(fp, last_fp, "the key moves while the intro plays ({t} ms)");
        frame(&mut app, wid);
        last_fp = app.claude_footer_fp(wid, &plan);
    }
    let end = at(EPISODE_MS + 1);
    set_now(end);
    assert_eq!(app.claude_card_deadline(end), Some(end));
    assert_ne!(
        app.claude_footer_fp(wid, &plan),
        last_fp,
        "the flow's first frame is owed"
    );
    assert_eq!(app.claude_cards_due(end), vec![wid]);
    assert_eq!(
        app.claude_card_deadline(end),
        None,
        "asked once: no wake spins on a frame that never comes"
    );
    frame(&mut app, wid);
    assert!(intro_over(&app, wid), "the intro let go");
    let step = |k: u64| at(k * FLOW_STEP_MS);
    let slack = Duration::from_millis(FLOW_SLACK_MS);
    let mut k = (EPISODE_MS + 1) / FLOW_STEP_MS;
    let mut key = app.claude_footer_fp(wid, &plan);
    let mut drawn = card_marks(&app, wid);
    for _ in 0..2 * GLINT_PERIOD_MS / FLOW_STEP_MS {
        let due = step(k + 1) + slack;
        let inside = step(k + 1) - Duration::from_millis(1);
        set_now(inside);
        assert_eq!(
            app.claude_card_deadline(inside),
            Some(due),
            "step {k}: the next step's instant"
        );
        assert!(
            app.claude_cards_due(inside).is_empty(),
            "step {k}: nothing due inside it"
        );
        assert_eq!(
            app.claude_footer_fp(wid, &plan),
            key,
            "step {k}: the key holds inside it"
        );
        k += 1;
        set_now(due);
        assert_eq!(app.claude_cards_due(due), vec![wid], "step {k}: due");
        assert_ne!(
            app.claude_footer_fp(wid, &plan),
            key,
            "step {k}: the key moves"
        );
        frame(&mut app, wid);
        let glass = card_marks(&app, wid);
        assert_ne!(glass, drawn, "step {k}: a new frame");
        (key, drawn) = (app.claude_footer_fp(wid, &plan), glass);
    }
    let now = step(k) + slack;
    app.windows.get_mut(&wid).unwrap().focused = false;
    assert_ne!(
        app.claude_footer_fp(wid, &plan),
        key,
        "a gate closing moves the key"
    );
    frame(&mut app, wid);
    let settled = app.claude_footer_fp(wid, &plan);
    let layers = card_layers(&app, wid);
    let settled_pic = card_pic(&app, wid);
    let at_rest = card_marks(&app, wid);
    for i in 0..1000u64 {
        let t = now + Duration::from_millis(1 + i * 7);
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
    let back = now + Duration::from_secs(60);
    set_now(back);
    app.windows.get_mut(&wid).unwrap().focused = true;
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
    assert!(
        kitty_x(&riding) < kitty_x(&at_rest),
        "the first intro rides"
    );
    assert_ne!(trail_span(&riding), trail_span(&at_rest));
    app.windows.get_mut(&wid).unwrap().focused = false;
    frame(&mut app, wid);
    assert_eq!(app.claude_card_deadline(back), None);
    assert_eq!(card_pic(&app, wid), settled_pic, "the settled frame");
    TEST_NOW.with(|now| now.set(None));
    host.shutdown_and_join();
}

fn own_wakes(app: &mut App, wid: WindowId, from: Instant, until: Instant) -> usize {
    let g0 = app.windows[&wid]
        .claude_card
        .entries
        .iter()
        .find_map(|e| e.card.ripple_origin())
        .expect("the card's ripple");
    let step = Duration::from_millis(FLOW_STEP_MS);
    let into = Duration::from_millis(20);
    let mut repaint = g0 + into;
    while repaint <= from {
        repaint += step;
    }
    let (mut t, mut own) = (from, 0);
    while repaint <= until {
        while let Some(due) = app.claude_card_deadline(t).filter(|&due| due < repaint) {
            let at = due.max(t);
            set_now(Some(at));
            assert_eq!(app.claude_cards_due(at), vec![wid], "the wake's frame");
            frame(app, wid);
            own += 1;
            t = at;
            assert!(own < 10_000, "the card's wakes spin");
        }
        set_now(Some(repaint));
        frame(app, wid);
        t = repaint;
        repaint += step;
    }
    set_now(Some(until));
    own
}

#[test]
fn a_focus_blip_replays_no_intro_and_the_card_rides_on() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    set_now(Some(t0));
    frame(&mut app, wid);
    let _ = own_wakes(&mut app, wid, t0, at(EPISODE_MS + 1000));
    assert!(intro_over(&app, wid), "CONTROL: the intro let go");
    let flowing = |app: &App| {
        app.windows[&wid]
            .claude_card
            .entries
            .iter()
            .any(|e| e.card.flowing(card_now()))
    };
    assert!(flowing(&app), "CONTROL: the card flows");
    let steady = own_wakes(&mut app, wid, at(EPISODE_MS + 1000), at(EPISODE_MS + 4000));
    assert_eq!(steady, 0, "CONTROL: the flow rides Claude's repaints");
    let g0 = app.windows[&wid]
        .claude_card
        .entries
        .iter()
        .find_map(|e| e.card.ripple_origin())
        .expect("the card's ripple");
    let mut lost = g0 + Duration::from_millis(50);
    while lost <= at(EPISODE_MS + 4000) {
        lost += Duration::from_millis(FLOW_STEP_MS);
    }
    set_now(Some(lost));
    frame(&mut app, wid);
    let step_pic = card_pic(&app, wid);
    app.window_focus_event(wid, false);
    assert!(!app.windows[&wid].focused, "CONTROL: the window lost focus");
    frame(&mut app, wid);
    assert_eq!(
        card_pic(&app, wid),
        step_pic,
        "a loss within the bound draws the step's flow frame, not the settled one"
    );
    let back = lost + Duration::from_millis(20);
    set_now(Some(back));
    app.window_focus_event(wid, true);
    assert!(app.windows[&wid].focused, "CONTROL: the window has it back");
    assert!(intro_over(&app, wid), "a blip arms no intro");
    frame(&mut app, wid);
    assert_eq!(
        card_pic(&app, wid),
        step_pic,
        "the flow ran on through the blip: the same step, the same colours and glints"
    );
    let after = own_wakes(&mut app, wid, back, back + Duration::from_secs(3));
    assert!(flowing(&app), "the flow resumed");
    assert_eq!(
        after, 0,
        "after a blip Claude's repaints still carry every step"
    );
    let away = back + Duration::from_secs(3) + Duration::from_millis(50);
    set_now(Some(away));
    app.window_focus_event(wid, false);
    frame(&mut app, wid);
    let settle = away + Duration::from_millis(RETURN_MS + 2 * FLOW_STEP_MS);
    let mut held = own_wakes(&mut app, wid, away, settle);
    assert!(
        !flowing(&app),
        "away past the bound, the card settles on its next step"
    );
    assert_eq!(app.claude_card_deadline(settle), None, "and arms nothing");
    held += own_wakes(&mut app, wid, settle, away + Duration::from_secs(2));
    assert_eq!(held, 0, "CONTROL: a held card wakes for nothing");
    let returned = away + Duration::from_secs(2);
    set_now(Some(returned));
    app.window_focus_event(wid, true);
    assert!(
        !intro_over(&app, wid),
        "NEGATIVE CONTROL: a return plays the intro"
    );
    let intro = own_wakes(&mut app, wid, returned, returned + Duration::from_secs(3));
    assert!(
        intro > 30,
        "NEGATIVE CONTROL: the intro's frames come ahead of Claude's repaints ({intro})"
    );
    set_now(None);
    host.shutdown_and_join();
}

#[test]
fn a_light_theme_card_is_ink() {
    use aterm_effects::color_math::relative_luminance;
    let contrast = |a: u32, b: u32| {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    };
    let modes = [
        (Autopilot::Cruise, "\u{221e} cruise"),
        (Autopilot::Land, "\u{2913} land"),
        (Autopilot::Off, "\u{25cb} off"),
    ];
    let grounds = [("ff/ff/ff", 0x00FF_FFFF), ("fb/f1/c7", 0x00FB_F1C7)];
    for ((mode, word), (rgb, bg)) in modes.into_iter().flat_map(|m| grounds.map(|g| (m, g))) {
        let osc = &format!("{mode:?} {rgb}");
        let (mut app, wid, session) = glass(120);
        {
            let term = app.pool.get(session).unwrap().term.clone();
            crate::term_lock(&term)
                .process(format!("\x1b]11;rgb:{rgb}\x1b\\\x1b]10;rgb:20/20/20\x1b\\").as_bytes());
        }
        let host = supervise(&mut app, session);
        if mode != Autopilot::Cruise {
            assert!(app.set_session_autopilot(
                session,
                Some(mode),
                crate::autopilot::Surface::Chip
            ));
        }
        let _ = aterm_effects::harness_card::take_line_ink_solves();
        let t0 = Instant::now();
        for t in [0, EPISODE_MS + 1, EPISODE_MS + 1 + 5 * FLOW_STEP_MS] {
            set_now(Some(t0 + Duration::from_millis(t)));
            frame(&mut app, wid);
            let (under, over) = card_marks(&app, wid);
            assert!(!under.is_empty() && !over.is_empty(), "{osc} {t} ms");
            let p = painted(&app, wid);
            assert!(
                under
                    .iter()
                    .chain(&over)
                    .all(|m| m.inks.iter().all(|i| i.alpha > 0)),
                "{osc} {t} ms: ink, never light"
            );
            assert!(
                !p.decos.is_empty()
                    && p.decos
                        .iter()
                        .all(|d| d.blend == DecoBlend::Over && d.alpha == u8::MAX),
                "{osc} {t} ms: {:?}",
                p.decos
            );
            assert!(
                p.ink.len() > 20,
                "{osc} {t} ms: the line, {} letters",
                p.ink.len()
            );
            for c in &p.ink {
                let rgb = pack(c.color);
                assert!(
                    contrast(rgb, bg) >= 4.5,
                    "{osc} {t} ms: {rgb:06x} at {:.2}:1",
                    contrast(rgb, bg)
                );
            }
        }
        assert_eq!(
            aterm_effects::harness_card::take_line_ink_solves(),
            0,
            "{osc}: letters solved on their own"
        );
        assert!(
            intro_over(&app, wid),
            "{osc}: CONTROL: the last frame is the flow's"
        );
        let line_row = strip_rows(&app, wid) + 4;
        assert!(
            glass_text(&app, wid, line_row, 0..120)
                .trim_end()
                .ends_with(&format!("autopilot {word}")),
            "{osc}: CONTROL: the card wears {mode:?}"
        );
        let start = 11 + LINE_LEAD;
        let scratch = &app.windows[&wid].input_scratch;
        let hero = start + 12..start + 17;
        assert!(
            scratch.cells[line_row][hero.clone()].iter().all(|c| c.bold),
            "{osc}: bold on light"
        );
        assert!(
            !scratch.cells[line_row][start].bold,
            "{osc}: the line's own letters not"
        );
        assert!(
            hero.clone().all(|col| scratch
                .ink
                .iter()
                .any(|i| usize::from(i.row) == line_row && usize::from(i.col) == col)),
            "{osc}: the hero word inked"
        );
        set_now(None);
        host.shutdown_and_join();
    }
}

/// THE LINE'S `✦` READS ON A MID GROUND ON THE GLASS (`harness_card::line_star`): on Silver
/// Aerogel's `#929292` and Unikitty's `#ff8cd9`, grounds the harness calls dark on which the
/// line's letters are floored dark, the `✦` leading the words is laid over the ground in the
/// first letter's ink through the episode and the flow, reading at least `MARK_CONTRAST` (its
/// light added, as it was, read 1.59-1.65:1 on Silver Aerogel in the real-Claude lab). NEGATIVE
/// CONTROL: that ink added to the ground misses the floor.
#[test]
fn the_lines_star_reads_on_a_mid_ground() {
    use aterm_effects::color_math::contrast_ratio;
    use aterm_effects::harness_card::MARK_CONTRAST;
    for (name, osc, bg) in [
        ("Silver Aerogel", "92/92/92", 0x0092_9292u32),
        ("Unikitty", "ff/8c/d9", 0x00FF_8CD9),
    ] {
        let (mut app, wid, session) = glass(120);
        {
            let term = app.pool.get(session).unwrap().term.clone();
            crate::term_lock(&term)
                .process(format!("\x1b]11;rgb:{osc}\x1b\\\x1b]10;rgb:00/00/00\x1b\\").as_bytes());
        }
        let host = supervise(&mut app, session);
        let t0 = Instant::now();
        let mut added = 0.0f32;
        for t in [
            0,
            1500,
            3000,
            EPISODE_MS + 1,
            EPISODE_MS + 1 + 5 * FLOW_STEP_MS,
        ] {
            set_now(Some(t0 + Duration::from_millis(t)));
            frame(&mut app, wid);
            let line_row = strip_rows(&app, wid) + 4;
            let scratch = &app.windows[&wid].input_scratch;
            let star = scratch
                .word_decorations
                .iter()
                .find(|d| usize::from(d.row) == line_row)
                .unwrap_or_else(|| panic!("{name} {t} ms: the star"));
            let first = scratch
                .ink
                .iter()
                .find(|c| (usize::from(c.row), usize::from(c.col)) == (line_row, 11 + LINE_LEAD))
                .unwrap_or_else(|| panic!("{name} {t} ms: the first letter's ink"));
            assert_eq!(
                (star.glyph, star.blend, star.color, star.alpha),
                (
                    DecoGlyph::Star4,
                    DecoBlend::Over,
                    pack(first.color),
                    u8::MAX
                ),
                "{name} {t} ms: the first letter's ink, laid over"
            );
            let k = contrast_ratio(star.color, bg);
            assert!(
                k >= MARK_CONTRAST,
                "{name} {t} ms: {:06x} at {k:.2}:1",
                star.color
            );
            added = added.max(contrast_ratio(aterm_render::add_sat(bg, star.color), bg));
        }
        assert!(
            added < MARK_CONTRAST,
            "NEGATIVE CONTROL: {name}: the ink added, at best {added:.2}:1"
        );
        set_now(None);
        host.shutdown_and_join();
    }
}

#[test]
fn a_narrow_pane_drops_the_kitty_and_keeps_the_border_and_line() {
    let model = "Opus 5.5 with xhigh effort \u{b7} Claude Max";
    let probe = App::headless_for_test();
    let (cw, ch) = probe.win_cell_size(WindowId(0));
    let kc = usize::from(kitty_cols(u16_of(cw), u16_of(ch)));
    assert!(kc > 0, "the headless cell holds the art ({cw}x{ch})");
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

fn cells_of(text: &str, blank: RenderCell, cols: usize) -> Vec<RenderCell> {
    let mut row: Vec<RenderCell> = text.chars().map(|ch| RenderCell { ch, ..blank }).collect();
    row.resize(cols, blank);
    row
}

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

fn card_rows(cols: usize, model: &str) -> Vec<String> {
    card_screen(12, cols, model, VERSION)[..8].to_vec()
}

#[test]
fn the_layout_follows_the_designs_rules() {
    let texts = vec!["w".repeat(40), "v".repeat(20)];
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
    let mut notice = rows.clone();
    notice[5] = "  Get to finished work sooner with Opus 5.5".to_string();
    assert_eq!(lay(&notice, 120, 6).expect("laid out").air, (true, false));
    let l = lay(&rows[..5], 120, 6).expect("laid out");
    assert_eq!((l.rows, l.air), (4, (true, false)), "nothing under the box");
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
    let l = lay(&card_rows(40, MODEL), 40, 6).expect("laid out");
    assert_eq!(l.line.as_ref().map(|l| l.cells), Some(20));
    assert_eq!((l.kitty_col, l.trail), (None, None));
    assert!(l.cols <= 39);
    let l = lay(&card_rows(30, MODEL), 30, 6).expect("laid out");
    assert_eq!((l.line, l.rows), (None, 3));
    let l = lay(&rows, 120, 0).expect("laid out");
    assert_eq!((l.kitty_col, l.trail, l.kitty_rows), (None, None, 2));
    let mut wide = rows.clone();
    wide[3] =
        " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/src/github.com/someone/\u{6771}".to_string();
    let mut s = scratch_of(&wide, 120);
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
        let grid_top = grid_origin(&app, wid).1;
        let (y0, y1) = (
            usize::try_from(art.at.y0 - grid_top).unwrap(),
            usize::try_from(art.at.y1 - grid_top).unwrap(),
        );
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

#[test]
fn the_strip_removes_only_its_own() {
    let key = |x: &u64| [*x, 0, 0];
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

fn set_now(t: Option<Instant>) {
    TEST_NOW.with(|now| now.set(t));
}

fn hold(app: &mut App, wid: WindowId) {
    app.windows.get_mut(&wid).expect("the window").focused = false;
    frame(app, wid);
}

fn intro_over(app: &App, wid: WindowId) -> bool {
    app.windows[&wid]
        .claude_card
        .entries
        .iter()
        .all(|e| !e.card.intro_live())
}

fn held_now() -> Instant {
    TEST_NOW
        .with(std::cell::Cell::get)
        .unwrap_or_else(Instant::now)
}

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

fn over_col(app: &App, wid: WindowId, cw: usize, pad: usize) -> usize {
    let x0 = marks(app, wid, FxSlot::Over)
        .iter()
        .map(|m| m.at.x0)
        .min()
        .expect("the kitty and its stars");
    usize::try_from(x0).unwrap().saturating_sub(pad) / cw
}

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
                if t > EPISODE_MS {
                    late += 1;
                } else {
                    frames += 1;
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
        let steps = 1000 / FLOW_STEP_MS + 2;
        assert!(
            late <= steps,
            "wakes every {wake_ms} ms: {late} flow frames, at most {steps}"
        );
        assert!(
            late >= 1000 / (2 * FLOW_STEP_MS),
            "wakes every {wake_ms} ms: CONTROL, the card flows ({late} frames)"
        );
        let end = t0 + Duration::from_millis(t);
        assert!(
            intro_over(&app, wid),
            "wakes every {wake_ms} ms: the intro let go"
        );
        assert_ne!(
            app.claude_card_deadline(end),
            Some(end + Duration::from_millis(FRAME_MS)),
            "wakes every {wake_ms} ms: no intro frame wanted"
        );
        set_now(None);
        host.shutdown_and_join();
    }
}

#[test]
fn a_pane_that_moves_in_the_plan_keeps_its_card() {
    let (mut app, wid, _) = glass(160);
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
    app.toggle_pane_zoom();
    paint(&app);
    frame(&mut app, wid);
    assert_eq!(card(&app), (1, t0, true), "unzoomed");
    set_now(None);
    host.shutdown_and_join();
}

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
    hold(&mut app, wid);
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
        assert!(intro_over(app, wid), "the intro over");
        frame(app, wid);
        assert!(intro_over(app, wid), "no change, no episode");
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
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    let now = settle(&mut app);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    assert!(card_drew(&app, wid), "Off: the white card");
    assert!(playing(&app, now), "Cruise to Off plays");
    let now = settle(&mut app);
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
        assert!(intro_over(app, wid), "the intro over");
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
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    let now = settle(&mut app);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    release();
    frame(&mut app, wid);
    assert!(card_drew(&app, wid), "Off, claim released: the white card");
    assert!(
        playing(&app, now),
        "Cruise to Off plays, the claim released"
    );
    let now = settle(&mut app);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    assert!(!card_drew(&app, wid), "the gap: Cruise, unclaimed, bare");
    reclaim();
    frame(&mut app, wid);
    assert!(card_drew(&app, wid), "claimed: decorated");
    assert!(
        playing(&app, now),
        "Off to Cruise plays though the claim came late"
    );
    let now = settle(&mut app);
    release();
    frame(&mut app, wid);
    assert!(!card_drew(&app, wid), "unclaimed: bare");
    reclaim();
    frame(&mut app, wid);
    assert!(card_drew(&app, wid), "claimed again: decorated");
    assert!(intro_over(&app, wid), "the same mode back: no episode");
    assert!(!playing(&app, now), "the same mode back: no intro frame");
    set_now(None);
    host.shutdown_and_join();
}

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
    assert!(!card_drew(&app, wid));
    assert!(!app.windows[&wid].claude_card.any_shown());
    assert_eq!(app.claude_card_deadline(t0), None);
    let strip = strip_rows(&app, wid);
    assert!(!glass_text(&app, wid, strip + 4, 0..120).contains("aterm harness"));
    set_now(None);
    host.shutdown_and_join();
}

fn buffer<T>(v: &Vec<T>) -> (usize, usize) {
    (v.as_ptr() as usize, v.capacity())
}

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
    app.windows.get_mut(&wid).unwrap().focused = false;
    frame(&mut app, wid);
    assert!(app.windows[&wid].claude_card.any_shown());
    assert_eq!(judge(&mut app, t0), ((None, vec![]), 0), "a still card");
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PassCost {
    allocations: usize,
    clock_reads: usize,
    ticks: usize,
    layouts: usize,
    reads: usize,
    autopilot_reads: usize,
    line_inks: usize,
    notice_reads: usize,
    timeline_locks: usize,
    probe_locks: usize,
    text_rebuilds: usize,
    footer_clock_reads: usize,
    strip_sorts: usize,
    fx_builds: u64,
    fx_copies: u64,
    art_builds: usize,
    cell_geometries: usize,
}

fn pass_cost(app: &mut App, wid: WindowId) -> PassCost {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = (
        crate::work_counts::take_harness_clock_reads(),
        crate::work_counts::take_card_ticks(),
        crate::work_counts::take_card_layouts(),
        crate::work_counts::take_card_reads(),
        crate::work_counts::take_autopilot_reads(),
        crate::work_counts::take_card_line_inks(),
        crate::work_counts::take_card_notice_reads(),
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
        notice_reads: crate::work_counts::take_card_notice_reads(),
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

fn keystroke(app: &mut App, wid: WindowId, session: u64, key: u8) {
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).process(&[key]);
    app.capture_words(wid, held_now());
    app.splice_tab_strip(wid);
}

fn settled_glass(cols: u16, t0: Instant) -> (App, WindowId, u64, crate::harness_host::HostHandle) {
    let (mut app, wid, session) = glass(cols);
    let host = supervise(&mut app, session);
    set_now(Some(t0));
    frame(&mut app, wid);
    set_now(Some(t0 + Duration::from_millis(FRAME_MS)));
    frame(&mut app, wid);
    set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 1)));
    frame(&mut app, wid);
    hold(&mut app, wid);
    let ws = &app.windows[&wid];
    assert!(ws.claude_card.any_shown(), "the card is on show");
    assert!(
        ws.claude_card.entries.iter().all(|e| e.card.at_rest()),
        "the card is settled"
    );
    (app, wid, session, host)
}

fn bare_glass(cols: u16) -> (App, WindowId, u64) {
    let (mut app, wid, session) = glass(cols);
    frame(&mut app, wid);
    hold(&mut app, wid);
    assert!(
        !app.windows[&wid].claude_card.any_shown(),
        "no card wrapped"
    );
    (app, wid, session)
}

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
    let back = t0 + Duration::from_secs(60);
    set_now(Some(back));
    app.windows.get_mut(&wid).expect("the window").focused = true;
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

#[test]
fn an_inline_scroll_replays_the_settled_card_at_its_new_place() {
    let t0 = Instant::now();
    let base = card_screen(24, 120, MODEL, VERSION);
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
    hold(&mut app, wid);
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
    offer(&app, session, drawer());
    assert!(app.card_drawer_published(session));
    edge(&mut app, session);
    let want = CardArts::for_cell(u16_of(app.win_cell_size(wid).1)).expect("ordinary cell height");
    let want: Vec<_> = want.arts().cloned().collect();
    assert!(want.len() > 6, "the kitty's, the rims' and the stars' arts");
    let queued = |app: &App| {
        let got = pending(app, wid);
        want.iter().all(|a| got.iter().any(|g| Arc::ptr_eq(a, g)))
            && got.iter().all(|g| want.iter().any(|a| Arc::ptr_eq(a, g)))
    };
    assert!(queued(&app), "the card's arts, queued");
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
    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx_prefetch_pending
        .clear();
    app.windows.get_mut(&wid).unwrap().cell_px_reported = None;
    app.sync_cell_pixel_size(wid);
    assert!(queued(&app), "a metric change with the drawer in front");
    host.shutdown_and_join();
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

#[test]
fn a_pinned_seed_gives_every_card_the_same_stars() {
    use crate::card_verb::{Asked, line};
    let settled = |app: &mut App, wid: WindowId, t0: Instant| {
        app.windows.get_mut(&wid).expect("the window").focused = true;
        for t in [0, FRAME_MS, EPISODE_MS + 1] {
            set_now(Some(t0 + Duration::from_millis(t)));
            frame(app, wid);
        }
        hold(app, wid);
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

fn feed_both(app: &mut (App, WindowId, u64), bare: &mut (App, WindowId, u64), bytes: &[u8]) {
    for (app, wid, session) in [app, bare] {
        let term = app.pool.get(*session).expect("session").term.clone();
        crate::term_lock(&term).process(bytes);
        app.capture_words(*wid, held_now());
        app.splice_tab_strip(*wid);
    }
}

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
    let rule_row = strip_rows(&app, wid) + 22;
    let glyphs = glass_text(&app, wid, rule_row, 0..120)
        .chars()
        .filter(|c| *c == '\u{2500}')
        .count();
    assert!(glyphs > 80, "CONTROL: the rule between them: {glyphs}");
    set_now(None);
    host.shutdown_and_join();
}

fn costs_the_floor(
    what: &str,
    passes: usize,
    mut step: impl FnMut(usize, &mut (App, WindowId, u64), &mut (App, WindowId, u64)),
) {
    let t0 = Instant::now();
    let (app, wid, session, host) = settled_glass(120, t0);
    let mut app = (app, wid, session);
    let mut bare = bare_glass(120);
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
        assert_eq!(
            app.0.windows[&wid].input_scratch.fx_layers, layers,
            "{what} {i}: the card's two layers on the frame, none left behind"
        );
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

#[test]
fn a_keystroke_under_a_settled_card_costs_the_frame_nothing() {
    costs_the_floor("keystroke", 12, |i, app, bare| {
        feed_both(app, bare, &[b"keep typing!"[i % 12]]);
    });
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let back = t0 + Duration::from_secs(60);
    set_now(Some(back));
    app.windows.get_mut(&wid).expect("the window").focused = true;
    app.rearm_claude_cards(wid, || back);
    keystroke(&mut app, wid, session, b'!');
    let playing = pass_cost(&mut app, wid);
    assert_eq!(playing.ticks, 1, "CONTROL: an episode frame is drawn");
    assert!(playing.clock_reads > 0, "CONTROL: on the clock");
    set_now(None);
    host.shutdown_and_join();
}

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
    hold(&mut app, wid);
    hold(&mut bare, bare_wid);
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

#[test]
fn a_window_with_no_claude_session_costs_the_harness_nothing() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
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
    feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
    assert_eq!(pass_cost(&mut app, wid).layouts, 1);
    assert_eq!(card_pic(&app, wid), first, "the first scene again");
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

fn laid_key(app: &App, wid: WindowId) -> LaidKey {
    app.windows[&wid].claude_card.entries[0]
        .laid
        .as_ref()
        .map(|(key, _)| *key)
        .expect("laid")
}

#[test]
fn a_card_laid_anew_reads_each_cell_once() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let base = card_screen(24, 120, MODEL, VERSION);
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

#[derive(Debug, PartialEq)]
struct CardGlass {
    layers: Vec<LayerPic>,
    ink: Vec<InkCell>,
    decos: Vec<WordDecoration>,
    lines: Vec<(usize, usize, Vec<RenderCell>)>,
    notices: Vec<(usize, usize, Vec<RenderCell>)>,
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
        notices: app.windows[&wid]
            .claude_card
            .notice_edits()
            .map(|(row, col, cells, _)| (row, col, cells.to_vec()))
            .collect(),
    }
}

fn fresh_card(cols: u16, paint: &dyn Fn(&App, u64)) -> CardGlass {
    let (mut app, wid, session) = glass(cols);
    paint(&app, session);
    let host = supervise(&mut app, session);
    let t0 = Instant::now();
    for t in [0, FRAME_MS, EPISODE_MS + 1] {
        set_now(Some(t0 + Duration::from_millis(t)));
        frame(&mut app, wid);
    }
    hold(&mut app, wid);
    let glass = card_glass(&app, wid);
    host.shutdown_and_join();
    glass
}

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
    let space = |bg: u8, row: usize, col: usize| {
        format!("\x1b7\x1b[{};{}H\x1b[{bg}m \x1b[0m\x1b8", row + 1, col + 1)
    };
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
    let update_notice = notice_screen(24, 120, &notice_rows()[1]);
    let mut recounted = update_notice.clone();
    recounted[NOTICE_TOP] = recounted[NOTICE_TOP].replace("Got 66", "Got 67");
    assert_ne!(recounted, update_notice, "recounted");
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
            "Claude's update notice under the box",
            update_notice.clone(),
            String::new(),
            (0, 1),
            true,
        ),
        (
            "its summary reworded past the box's air",
            recounted,
            String::new(),
            (0, 0),
            true,
        ),
        (
            "the update notice again",
            update_notice,
            String::new(),
            (0, 0),
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
        hold(&mut app, wid);
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
    hold(&mut app, wid);
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
    hide(&mut app, false);
    set_now(Some(at(past + 1000)));
    frame(&mut app, wid);
    assert!(intro_over(&app, wid), "the intro let go");
    assert!(
        app.windows[&wid]
            .claude_card
            .entries
            .iter()
            .all(|e| e.card.flowing(at(past + 1000))),
        "flowing"
    );
    assert_ne!(
        app.claude_card_deadline(at(past + 1000)),
        Some(at(past + 1000 + FRAME_MS)),
        "no intro frame wanted"
    );
    let back = at(past + 2000);
    app.rearm_claude_cards(wid, || back);
    assert_eq!(wake(&mut app, back).0, (vec![wid], None), "playing again");
    set_now(None);
    host.shutdown_and_join();
}

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

fn flowing_glass(t0: Instant, at_ms: u64) -> (App, WindowId, u64, crate::harness_host::HostHandle) {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    for t in [0, at_ms] {
        set_now(Some(t0 + Duration::from_millis(t)));
        frame(&mut app, wid);
    }
    assert!(
        app.windows[&wid]
            .claude_card
            .entries
            .iter()
            .all(|e| e.card.flowing(t0 + Duration::from_millis(at_ms))),
        "flowing"
    );
    (app, wid, session, host)
}

#[test]
fn a_flowing_card_draws_once_a_step_whatever_else_wakes() {
    for wake_ms in [4u64, 8, 20] {
        let t0 = Instant::now();
        let from = EPISODE_MS + 1;
        let (mut app, wid, _, host) = flowing_glass(t0, from);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let mut drawn = app.claude_footer_fp(wid, &plan);
        let _ = crate::work_counts::take_card_ticks();
        let span = 2 * GLINT_PERIOD_MS;
        let (mut frames, mut t) = (0u64, from);
        while t < from + span {
            t += wake_ms;
            let now = t0 + Duration::from_millis(t);
            set_now(Some(now));
            let _ = app.claude_cards_due(now);
            let key = app.claude_footer_fp(wid, &plan);
            if key != drawn {
                drawn = key;
                frame(&mut app, wid);
                frames += 1;
            }
        }
        let ticks = crate::work_counts::take_card_ticks() as u64;
        let steps = span / FLOW_STEP_MS + 1;
        assert!(
            frames <= steps && ticks <= steps,
            "wakes every {wake_ms} ms: {frames} frames, {ticks} drawn, at most {steps}"
        );
        assert!(
            ticks >= steps - 2,
            "wakes every {wake_ms} ms: CONTROL, it flows ({ticks} drawn)"
        );
        set_now(None);
        host.shutdown_and_join();
    }
}

#[test]
fn a_flowing_card_costs_one_clock_read_and_a_copy_between_steps() {
    let t0 = Instant::now();
    let k = EPISODE_MS / FLOW_STEP_MS + 1;
    let (mut app, wid, _, host) = flowing_glass(t0, k * FLOW_STEP_MS + 1);
    let (mut bare, bare_wid, _) = bare_glass(120);
    bare.windows.get_mut(&bare_wid).unwrap().focused = true;
    let layers = card_layers(&app, wid);
    let pic = card_pic(&app, wid);
    let line = glass_text(&app, wid, strip_rows(&app, wid) + 4, 0..120);
    for warm in [3, 5] {
        set_now(Some(t0 + Duration::from_millis(k * FLOW_STEP_MS + warm)));
        let _ = pass_cost(&mut app, wid);
        let _ = pass_cost(&mut bare, bare_wid);
    }
    for i in 0..10u64 {
        let now = t0 + Duration::from_millis(k * FLOW_STEP_MS + 10 + 11 * i);
        set_now(Some(now));
        let floor = pass_cost(&mut bare, bare_wid);
        let cost = pass_cost(&mut app, wid);
        assert_eq!(
            (
                cost.ticks,
                cost.line_inks,
                cost.layouts,
                cost.reads,
                cost.fx_builds
            ),
            (0, 0, 0, 0, 0),
            "pass {i}: replayed ({cost:?})"
        );
        assert_eq!(
            cost.allocations, floor.allocations,
            "pass {i}: no allocation"
        );
        assert_eq!(
            cost.clock_reads, 4,
            "pass {i}: the key, the splice and the two judges, one read each"
        );
        assert_eq!(
            card_layers(&app, wid),
            layers,
            "pass {i}: the step's kept contents"
        );
        assert_eq!(
            glass_text(&app, wid, strip_rows(&app, wid) + 4, 0..120),
            line,
            "pass {i}: its line"
        );
    }
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = crate::work_counts::take_harness_clock_reads();
    app.splice_claude_footer(wid, &plan, route);
    assert_eq!(
        crate::work_counts::take_harness_clock_reads(),
        1,
        "the splice reads the clock once"
    );
    set_now(Some(t0 + Duration::from_millis((k + 1) * FLOW_STEP_MS + 1)));
    let next = pass_cost(&mut app, wid);
    assert_eq!(
        (next.ticks, next.line_inks),
        (1, 1),
        "CONTROL: the next step drawn, its line inked ({next:?})"
    );
    assert_eq!(next.fx_builds, 2, "CONTROL: its two contents sealed");
    assert_eq!(
        next.art_builds, 0,
        "a step is a new palette on the arts its height registered: no art built"
    );
    assert_ne!(card_pic(&app, wid), pic, "CONTROL: a new step's glass");
    // The kept step's contents go back to the card's ring, as a renderer
    // lets go of a frame it has drawn past.
    drop(layers);
    for step in 2..=6 {
        set_now(Some(
            t0 + Duration::from_millis((k + step) * FLOW_STEP_MS + 1),
        ));
        let floor = pass_cost(&mut bare, bare_wid);
        let drawn = pass_cost(&mut app, wid);
        assert_eq!(drawn.ticks, 1, "step {step}: drawn");
        if step >= 3 {
            assert_eq!(
                drawn.allocations, floor.allocations,
                "step {step}: each of the ring's three pairs has held a step, so a drawn step \
                 refills one in place and allocates nothing"
            );
        }
    }
    set_now(None);
    host.shutdown_and_join();
}

#[test]
fn a_gate_reopening_moves_the_key_and_the_card_flows() {
    for gate in ["reduce motion", "load shed"] {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let (mut app, wid, _, host) = flowing_glass(t0, EPISODE_MS + 1);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let shut = |app: &mut App, shut: bool| match gate {
            "reduce motion" => app.system_reduce_motion = shut,
            _ => app.perf_reduced = shut,
        };
        shut(&mut app, true);
        set_now(Some(at(EPISODE_MS + 100)));
        frame(&mut app, wid);
        assert!(
            app.windows[&wid]
                .claude_card
                .entries
                .iter()
                .all(|e| e.card.at_rest()),
            "{gate}: held"
        );
        let held = app.claude_footer_fp(wid, &plan);
        for i in 0..100u64 {
            let t = at(EPISODE_MS + 100 + 10 * i);
            set_now(Some(t));
            assert_eq!(
                app.claude_footer_fp(wid, &plan),
                held,
                "{gate}: CONTROL {i}"
            );
            assert_eq!(app.claude_card_deadline(t), None, "{gate}: no wake {i}");
        }
        shut(&mut app, false);
        let back_ms = EPISODE_MS + 1500;
        set_now(Some(at(back_ms)));
        assert_ne!(
            app.claude_footer_fp(wid, &plan),
            held,
            "{gate}: reopened, the key moves"
        );
        frame(&mut app, wid);
        let k = back_ms / FLOW_STEP_MS;
        assert!(
            app.windows[&wid]
                .claude_card
                .entries
                .iter()
                .all(|e| e.card.flowing(at(back_ms))),
            "{gate}: it flows again"
        );
        assert_eq!(
            app.claude_card_deadline(at(back_ms)),
            Some(at((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS)),
            "{gate}: the next step wanted"
        );
        set_now(None);
        host.shutdown_and_join();
    }
}

fn workspace_glass() -> (App, WindowId, u64, crate::harness_host::HostHandle) {
    let text =
        include_str!("../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-120col.txt");
    let screen: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 24, 120));
    publish(&app, session, "2.1.302");
    feed(&app, session, &screen);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    (app, wid, session, host)
}

fn paint_cell(app: &App, session: u64, (col, row): (usize, usize), ch: char, rgb: [u8; 3]) {
    let bytes = format!(
        "\x1b7\x1b[{};{}H\x1b[38;2;{};{};{}m{ch}\x1b[0m\x1b8",
        row + 1,
        col + 1,
        rgb[0],
        rgb[1],
        rgb[2]
    );
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).process(bytes.as_bytes());
}

#[test]
fn the_wand_flare_locks_the_ring_train() {
    use aterm_effects::harness_card::FLARE_STEP_LEAD_MS;
    let wand = (10, 0);
    let rest = [255, 204, 104];
    let flicker = [240, 128, 72];
    let next =
        |grid: u64, t: u64| grid + ((t - grid) / FLOW_STEP_MS + 1) * FLOW_STEP_MS + FLOW_SLACK_MS;
    let lag = FLARE_STEP_LEAD_MS;
    let beat = |grid: u64, t: u64| {
        let from = grid + lag;
        from + (t.saturating_sub(from)).div_ceil(FLOW_STEP_MS) * FLOW_STEP_MS
    };
    for light in [false, true] {
        for flare in [aterm_phase::WAND_FLARE_DARK, aterm_phase::WAND_FLARE_LIGHT] {
            let case = format!(
                "{flare:?} on a {} ground",
                if light { "light" } else { "dark" }
            );
            let (mut app, wid, session, host) = workspace_glass();
            if light {
                let term = app.pool.get(session).unwrap().term.clone();
                crate::term_lock(&term)
                    .process(b"\x1b]11;rgb:f8/fa/fd\x1b\\\x1b]10;rgb:20/20/20\x1b\\");
            }
            let t0 = Instant::now();
            let at = |ms: u64| t0 + Duration::from_millis(ms);
            let step = |app: &mut App, t: u64, ink: Option<[u8; 3]>| {
                if let Some(ink) = ink {
                    paint_cell(app, session, wand, '\u{2726}', ink);
                }
                set_now(Some(at(t)));
                frame(app, wid);
            };
            step(&mut app, 0, Some(rest));
            step(&mut app, 1000, Some(flare));
            let grid = 1000 - lag;
            step(&mut app, 1050, None);
            step(&mut app, beat(grid, 1100), Some(rest));
            step(&mut app, EPISODE_MS + 50, None);
            assert_eq!(
                app.claude_card_deadline(at(EPISODE_MS + 50)),
                Some(at(next(grid, EPISODE_MS + 50))),
                "{case}: adopted in the intro, the flow on the flare's grid"
            );
            let seen = grid + 2 * GLINT_PERIOD_MS + 20 + lag;
            step(&mut app, seen, Some(flare));
            let grid = grid + 5;
            assert_eq!(
                app.claude_card_deadline(at(seen)),
                Some(at(next(grid, seen))),
                "{case}: slewed by 5 ms"
            );
            step(&mut app, seen + 50, None);
            assert_eq!(
                app.claude_card_deadline(at(seen + 50)),
                Some(at(next(grid, seen + 50))),
                "{case}: held lit, the same grid"
            );
            step(&mut app, beat(grid, seen + 300), Some(rest));
            let seen = grid + 3 * GLINT_PERIOD_MS + 1000 + lag;
            step(&mut app, seen, Some(flare));
            let grid = grid + FLOW_STEP_MS;
            assert_eq!(
                app.claude_card_deadline(at(seen)),
                Some(at(next(grid, seen))),
                "{case}: a step toward it"
            );
            step(&mut app, beat(grid, seen + 300), Some(rest));
            let later = beat(grid, seen + 5 * GLINT_PERIOD_MS + 77);
            step(&mut app, later, Some(flicker));
            assert_eq!(
                app.claude_card_deadline(at(later)),
                Some(at(next(grid, later))),
                "{case}: free-running where the lock left it"
            );
            set_now(None);
            host.shutdown_and_join();
        }
    }
    for ink in [rest, flicker] {
        let (mut app, wid, session, host) = workspace_glass();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        for t in [0, 1000, EPISODE_MS + 50] {
            paint_cell(&app, session, wand, '\u{2726}', ink);
            set_now(Some(at(t)));
            frame(&mut app, wid);
        }
        paint_cell(&app, session, wand, '\u{2726}', ink);
        set_now(Some(at(7100)));
        frame(&mut app, wid);
        assert_eq!(
            app.claude_card_deadline(at(7100)),
            Some(at(next(0, 7100))),
            "CONTROL: {ink:?}, free-running on the first arm's grid"
        );
        set_now(None);
        host.shutdown_and_join();
    }
    let t0 = Instant::now();
    let (mut app, wid, session, host) = flowing_glass(t0, EPISODE_MS + 50);
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let place = decorated(&app, wid).expect("the plain card");
    let cell = place.wand().expect("its wand cell");
    paint_cell(&app, session, cell, ' ', aterm_phase::WAND_FLARE_DARK);
    paint_cell(
        &app,
        session,
        (cell.0 - 1, cell.1),
        '\u{2588}',
        aterm_phase::WAND_FLARE_DARK,
    );
    let t = beat(0, 7100);
    set_now(Some(at(t)));
    frame(&mut app, wid);
    assert!(decorated(&app, wid).is_some(), "CONTROL: still decorated");
    assert_eq!(
        app.claude_card_deadline(at(t)),
        Some(at(next(0, t))),
        "CONTROL: no wand, free-running"
    );
    set_now(None);
    host.shutdown_and_join();
}

#[test]
fn the_shimmer_locks_the_flows_steps() {
    use aterm_effects::harness_card::FLARE_STEP_LEAD_MS;
    let next =
        |grid: u64, t: u64| grid + ((t - grid) / FLOW_STEP_MS + 1) * FLOW_STEP_MS + FLOW_SLACK_MS;
    let (mut app, wid, session, host) = workspace_glass();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let inks = [[240, 128, 72], [255, 204, 104], [250, 160, 90]];
    let body = (3, 1);
    let beat = |t: u64| {
        let from = FLARE_STEP_LEAD_MS;
        from + (t.saturating_sub(from)).div_ceil(FLOW_STEP_MS) * FLOW_STEP_MS
    };
    for (i, t) in [0, beat(1000), beat(EPISODE_MS + 50)]
        .into_iter()
        .enumerate()
    {
        paint_cell(&app, session, body, '\u{2588}', inks[i % 2]);
        set_now(Some(at(t)));
        frame(&mut app, wid);
    }
    let t = beat(EPISODE_MS + 50);
    assert_eq!(
        app.claude_card_deadline(at(t)),
        Some(at(next(0, t))),
        "CONTROL: the repaints came on the beat"
    );
    let seen = 60 * FLOW_STEP_MS + FLARE_STEP_LEAD_MS + 40;
    paint_cell(&app, session, body, '\u{2588}', inks[2]);
    set_now(Some(at(seen)));
    frame(&mut app, wid);
    assert_eq!(
        app.claude_card_deadline(at(seen)),
        Some(at(next(10, seen))),
        "a quarter of 40: +10"
    );
    set_now(Some(at(seen + 20)));
    frame(&mut app, wid);
    assert_eq!(
        app.claude_card_deadline(at(seen + 20)),
        Some(at(next(10, seen + 20))),
        "a frame that repainted nothing locks nothing"
    );
    let seen = seen + FLOW_STEP_MS;
    paint_cell(&app, session, body, '\u{2588}', inks[0]);
    set_now(Some(at(seen)));
    frame(&mut app, wid);
    assert_eq!(
        app.claude_card_deadline(at(seen)),
        Some(at(next(17, seen))),
        "the 30 ms left, a quarter: +7"
    );
    paint_cell(&app, session, (20, 4), 'x', inks[1]);
    let seen = seen + FLOW_STEP_MS;
    set_now(Some(at(seen)));
    frame(&mut app, wid);
    assert_eq!(
        app.claude_card_deadline(at(seen)),
        Some(at(next(17, seen))),
        "NEGATIVE CONTROL: no mascot cell changed"
    );
    set_now(None);
    host.shutdown_and_join();
}

#[test]
fn every_rung_of_the_line_names_aterm() {
    let ends = [
        LineEnd::Mode,
        LineEnd::Landed,
        LineEnd::CruiseUntil(six_pm(false)),
        LineEnd::CruiseUntil(six_pm(true)),
    ];
    let mut rungs = 0;
    for view in AutopilotView::ALL {
        for end in ends {
            for rows_kind in [3, 4, 1] {
                let texts = line_texts_for(rows_kind, view, end);
                for (i, text) in texts.iter().enumerate() {
                    let short = i + 3 >= texts.len();
                    assert_eq!(!text.starts_with("enhanced by "), short, "{text:?}");
                    let at = if short { 0 } else { 12 };
                    assert_eq!(hero_cells(text), Some(at..at + 5), "{text:?}");
                    let word: String = text.chars().skip(usize::from(at)).take(5).collect();
                    assert_eq!(word, HERO_WORD, "{text:?}");
                    rungs += 1;
                }
                let widths: Vec<usize> = texts.iter().map(|t| text_cells(t)).collect();
                assert!(
                    widths.windows(2).all(|w| w[0] > w[1]),
                    "widest first: {texts:?}"
                );
            }
        }
    }
    assert!(rungs > 100, "CONTROL: the rungs were walked ({rungs})");
    assert_eq!(
        hero_cells("enhanced by materm harness"),
        None,
        "NEGATIVE CONTROL"
    );
    assert_eq!(
        hero_cells("enhanced by aterms harness"),
        None,
        "NEGATIVE CONTROL"
    );
    assert_eq!(hero_cells("no word"), None, "NEGATIVE CONTROL");
    assert_eq!(
        hero_cells("\u{4E2D} aterm"),
        Some(3..8),
        "a wide glyph, two cells"
    );
}

#[derive(Clone, Copy, Debug)]
struct NoticeCapture {
    name: &'static str,
    text: &'static str,
    rows: usize,
    cols: usize,
    card: usize,
    card_rows: usize,
    top: usize,
    summary: usize,
    details: usize,
    end: usize,
}

impl NoticeCapture {
    fn screen(&self) -> Vec<String> {
        self.text.lines().skip(1).map(str::to_owned).collect()
    }

    fn notice(&self) -> std::ops::Range<usize> {
        self.top..self.top + self.summary + self.details
    }
}

const CAPTURED: &str = "2.1.304";

const NOTICE_CAPTURES: [NoticeCapture; 5] = [
    NoticeCapture {
        name: "fullscreen-120col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-fullscreen-120col.txt"
        ),
        rows: 40,
        cols: 120,
        card: 1,
        card_rows: 3,
        top: 5,
        summary: 1,
        details: 1,
        end: 74,
    },
    NoticeCapture {
        name: "fullscreen-50col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-fullscreen-50col.txt"
        ),
        rows: 40,
        cols: 50,
        card: 1,
        card_rows: 3,
        top: 5,
        summary: 2,
        details: 1,
        end: 47,
    },
    NoticeCapture {
        name: "fullscreen-44col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-fullscreen-44col.txt"
        ),
        rows: 40,
        cols: 44,
        card: 1,
        card_rows: 3,
        top: 5,
        summary: 2,
        details: 2,
        end: 41,
    },
    NoticeCapture {
        name: "inline-workspace-120col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-inline-workspace-120col.txt"
        ),
        rows: 40,
        cols: 120,
        card: 0,
        card_rows: 3,
        top: 4,
        summary: 1,
        details: 1,
        end: 74,
    },
    NoticeCapture {
        name: "inline-50col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-inline-50col.txt"
        ),
        rows: 40,
        cols: 50,
        card: 0,
        card_rows: 3,
        top: 4,
        summary: 2,
        details: 1,
        end: 47,
    },
];

const WIDE: NoticeCapture = NOTICE_CAPTURES[0];

const NARROW_CAPTURES: [NoticeCapture; 7] = [
    NoticeCapture {
        name: "narrow-workspace-fullscreen-50col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-narrow-workspace-fullscreen-50col.txt"
        ),
        rows: 40,
        cols: 50,
        card: 1,
        card_rows: 4,
        top: 6,
        summary: 2,
        details: 1,
        end: 47,
    },
    NoticeCapture {
        name: "narrow-workspace-fullscreen-34col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-narrow-workspace-fullscreen-34col.txt"
        ),
        rows: 40,
        cols: 34,
        card: 1,
        card_rows: 4,
        top: 6,
        summary: 3,
        details: 2,
        end: 34,
    },
    NoticeCapture {
        name: "narrow-workspace-fullscreen-33col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-narrow-workspace-fullscreen-33col.txt"
        ),
        rows: 40,
        cols: 33,
        card: 1,
        card_rows: 4,
        top: 6,
        summary: 3,
        details: 2,
        end: 33,
    },
    NoticeCapture {
        name: "narrow-workspace-inline-50col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-narrow-workspace-inline-50col.txt"
        ),
        rows: 40,
        cols: 50,
        card: 0,
        card_rows: 4,
        top: 5,
        summary: 2,
        details: 1,
        end: 47,
    },
    NoticeCapture {
        name: "narrow-workspace-inline-33col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-narrow-workspace-inline-33col.txt"
        ),
        rows: 40,
        cols: 33,
        card: 0,
        card_rows: 4,
        top: 5,
        summary: 3,
        details: 2,
        end: 33,
    },
    NoticeCapture {
        name: "narrow-xhigh-fullscreen-50col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-narrow-xhigh-fullscreen-50col.txt"
        ),
        rows: 40,
        cols: 50,
        card: 1,
        card_rows: 4,
        top: 6,
        summary: 2,
        details: 1,
        end: 47,
    },
    NoticeCapture {
        name: "narrow-xhigh-fullscreen-31col",
        text: include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-narrow-xhigh-fullscreen-31col.txt"
        ),
        rows: 40,
        cols: 31,
        card: 1,
        card_rows: 4,
        top: 6,
        summary: 3,
        details: 2,
        end: 31,
    },
];

fn notice_rows() -> [String; 2] {
    let screen = WIDE.screen();
    let rows = [screen[WIDE.top].clone(), screen[WIDE.top + 1].clone()];
    assert!(rows[0].starts_with(&format!("  {}", aterm_phase::anchor!("notice.updated"))));
    assert_eq!(
        rows[1],
        format!("  {}", aterm_phase::anchor!("notice.updated_details"))
    );
    rows
}

const NOTICE_TOP: usize = 5;

fn notice_screen(rows: usize, cols: usize, details: &str) -> Vec<String> {
    let mut screen = card_screen(rows, cols, MODEL, VERSION);
    let [summary, _] = notice_rows();
    screen[NOTICE_TOP] = summary;
    screen[NOTICE_TOP + 1] = details.to_string();
    screen
}

#[cfg(unix)]
fn capture_at(rows: usize, cols: usize, details: &str) -> Vec<String> {
    assert!(cols >= WIDE.end && rows >= 12, "the notice fits the pane");
    let screen = WIDE.screen();
    let fit = |row: &String| {
        if !row.is_empty() && row.chars().all(|c| c == '\u{2500}') {
            "\u{2500}".repeat(cols)
        } else {
            row.clone()
        }
    };
    let mut out: Vec<String> = screen[..WIDE.notice().end].iter().map(fit).collect();
    out[WIDE.top + WIDE.summary] = details.to_string();
    let foot = &screen[screen.len() - 4..];
    out.resize(rows - foot.len(), String::new());
    out.extend(foot.iter().map(fit));
    out
}

fn feed_capture(app: &App, session: u64, c: &NoticeCapture, rows: &[String]) {
    let mut out = String::from("\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        let sgr = if (c.card + 2..c.card + c.card_rows).contains(&i) {
            format!("\x1b[38;2;{};{};{}m", GRAY[0], GRAY[1], GRAY[2])
        } else if (c.top..c.top + c.summary).contains(&i) {
            "\x1b[1m".to_string()
        } else if c.notice().contains(&i) {
            "\x1b[38;2;153;153;153m".to_string()
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

fn capture_glass(c: &NoticeCapture) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, u16_of(c.rows), u16_of(c.cols)));
    publish(&app, session, CAPTURED);
    feed_capture(&app, session, c, &c.screen());
    (app, wid, session)
}

fn engine_rows(app: &App, session: u64) -> Vec<String> {
    let mut fresh = RenderInput::empty();
    let term = app.pool.get(session).unwrap().term.clone();
    let mut term = crate::term_lock(&term);
    let (rows, cols) = (usize::from(term.rows()), usize::from(term.cols()));
    term.cell_frame_into(&mut fresh, rows, cols);
    fresh
        .cells
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect::<String>())
        .collect()
}

fn capture_bare(app: &App, wid: WindowId, c: &NoticeCapture) -> bool {
    let strip = strip_rows(app, wid);
    let screen = c.screen();
    c.notice()
        .all(|r| glass_text(app, wid, strip + r, 0..text_cells(&screen[r])) == screen[r])
}

fn capture_banner(app: &App, wid: WindowId, c: &NoticeCapture) -> bool {
    let strip = strip_rows(app, wid);
    let words = text_cells(NOTICE.text);
    let at = 2 + LINE_LEAD;
    let blank = |r: usize, from: usize| {
        glass_text(app, wid, strip + r, from..c.end) == " ".repeat(c.end - from)
    };
    glass_text(app, wid, strip + c.top, 0..at) == " ".repeat(at)
        && glass_text(app, wid, strip + c.top, at..at + words) == NOTICE.text
        && blank(c.top, at + words)
        && c.notice().skip(1).all(|r| blank(r, 0))
}

fn notice_bare(app: &App, wid: WindowId) -> bool {
    capture_bare(app, wid, &WIDE)
}

fn banner_shown(app: &App, wid: WindowId) -> bool {
    capture_banner(app, wid, &WIDE)
}

#[test]
fn the_update_notice_wears_the_harness_notice_on_the_glass_only() {
    let mut rungs = [0; 2];
    for c in NOTICE_CAPTURES.iter().chain(&NARROW_CAPTURES) {
        let name = c.name;
        let (mut app, wid, session) = capture_glass(c);
        let before = engine_rows(&app, session);
        let screen = c.screen();
        for r in c.notice() {
            assert!(
                before[r].starts_with(&screen[r]),
                "{name}: CONTROL: the vendor's notice is in the terminal"
            );
        }
        frame(&mut app, wid);
        assert!(
            capture_bare(&app, wid, c),
            "{name}: CONTROL: no host, the vendor's notice"
        );
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
        assert!(
            capture_banner(&app, wid, c),
            "{name}: the banner over the notice"
        );
        assert_eq!(
            decorated(&app, wid).map(|p| (p.top, p.rows)),
            Some((c.card, c.card_rows)),
            "{name}: the card on the glass"
        );
        let chrome = strip_rows(&app, wid);
        let line = glass_text(&app, wid, chrome + c.card + c.card_rows, 0..c.cols);
        let words = line.trim_start().trim_start_matches('\u{2726}').trim();
        let short = !words.starts_with("enhanced by ");
        assert!(
            words.starts_with(HERO_WORD) == short && line.contains(HERO_WORD),
            "{name}: the card's line: {line:?}"
        );
        let lead = line.chars().take_while(|&ch| ch == ' ').count();
        let rung = line_texts(AutopilotView::Cruise, LineEnd::Mode)
            .iter()
            .find(|t| lead + text_cells(t) < c.cols)
            .expect("a rung fits");
        assert_eq!(words, rung, "{name}: the widest rung that fits: {line:?}");
        rungs[usize::from(short)] += 1;
        let hero = line[..line.find(HERO_WORD).expect("the hero")]
            .chars()
            .count();
        let line_row = &app.windows[&wid].input_scratch.cells[chrome + c.card + c.card_rows];
        assert!(
            line_row
                .iter()
                .take(c.cols)
                .enumerate()
                .all(|(col, cell)| cell.bold == (hero..hero + HERO_WORD.len()).contains(&col)),
            "{name}: bold on the hero word alone, on every rung"
        );
        let (row, at, words) = (chrome + c.top, 2 + LINE_LEAD, text_cells(NOTICE.text));
        let scratch = &app.windows[&wid].input_scratch;
        let blank = scratch.implicit_blank;
        assert_eq!(
            scratch.cells[row][at].fg,
            crate::claude_footer::value_fg(GRAY, blank.fg),
            "{name}: the line's value colour under the ink"
        );
        let underlined: Vec<usize> = (0..c.end)
            .filter(|&col| scratch.cells[row][col].underline != UnderlineStyle::None)
            .collect();
        assert_eq!(
            underlined,
            (at..at + words).collect::<Vec<_>>(),
            "{name}: the link's letters underlined, not the star's cells or the rest of the row"
        );
        assert!(
            scratch.cells[row][at..at + words]
                .iter()
                .all(|cell| cell.underline == UnderlineStyle::Single
                    && cell.underline_color.is_none()),
            "{name}: a single underline in each letter's own ink"
        );
        let inked: Vec<u16> = scratch
            .ink
            .iter()
            .filter(|c| usize::from(c.row) == row)
            .map(|c| c.col)
            .collect();
        assert_eq!(
            inked,
            (at..at + words).map(|c| c as u16).collect::<Vec<_>>(),
            "{name}: every letter inked, and nothing else on the row"
        );
        let hidden: Vec<usize> = c.notice().skip(1).map(|r| chrome + r).collect();
        assert!(
            scratch
                .ink
                .iter()
                .all(|i| !hidden.contains(&usize::from(i.row))),
            "{name}: nothing inked on the blanked rows"
        );
        let star = scratch
            .word_decorations
            .iter()
            .find(|d| usize::from(d.row) == row)
            .expect("the banner's star");
        let first = scratch
            .ink
            .iter()
            .find(|c| (usize::from(c.row), usize::from(c.col)) == (row, at))
            .expect("the first letter's ink");
        assert_eq!(
            (usize::from(star.col), star.glyph, star.blend, star.color),
            (2, DecoGlyph::Star4, DecoBlend::Add, pack(first.color)),
            "{name}: the star leads the words in the first letter's colour"
        );
        let hits = &app.windows[&wid].claude_card.notice_hits;
        let mut want = vec![
            (session, row, 2..at + words, Some(NOTICE.url)),
            (session, row, 2..c.end, None),
        ];
        want.extend(hidden.iter().map(|&r| (session, r, 2..c.end, None)));
        assert_eq!(
            hits.iter()
                .map(|h| (h.session, h.frame_row, h.cols.clone(), h.url))
                .collect::<Vec<_>>(),
            want,
            "{name}: the link on the star and its words, nothing on the rest"
        );
        assert_eq!(
            engine_rows(&app, session),
            before,
            "{name}: the terminal's text"
        );
        let ws = app.windows.get_mut(&wid).unwrap();
        assert!(crate::claude_footer::undo(
            &mut ws.claude_footer_undo,
            &mut ws.input_scratch
        ));
        strip(&mut ws.claude_card, &mut ws.input_scratch);
        assert_eq!(
            ws.input_scratch.cells, vendor,
            "{name}: every cell the vendor's"
        );
        assert!(
            ws.input_scratch
                .word_decorations
                .iter()
                .all(|d| usize::from(d.row) != row),
            "{name}: the banner's star taken back"
        );
        host.shutdown_and_join();
    }
    assert!(
        rungs[0] > 0 && rungs[1] > 0,
        "CONTROL: the captures hold a versioned rung and a short one ({rungs:?})"
    );
    assert_eq!(NOTICE.url, "https://alab.systems");
    assert_eq!(
        NOTICE.text, NOTICE.url,
        "the banner reads as the link it opens"
    );
    assert_eq!(NOTICE.short, "alab.systems");
    let url = text_cells(NOTICE.text);
    assert_eq!(
        NOTICE.words_within(url),
        Some(NOTICE.text),
        "the link whole"
    );
    assert_eq!(
        NOTICE.words_within(url - 1),
        Some(NOTICE.short),
        "where the link cannot fit, the site's name"
    );
    assert_eq!(NOTICE.words_within(text_cells(NOTICE.short) - 1), None);
}

#[test]
fn the_box_strokes_no_glyph_in_the_panes_last_column() {
    let mut open = 0;
    let mut closed = 0;
    for c in &NARROW_CAPTURES {
        let name = c.name;
        let (mut app, wid, session) = capture_glass(c);
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        assert_eq!(
            decorated(&app, wid).map(|p| (p.top, p.rows)),
            Some((c.card, c.card_rows)),
            "{name}: the card is boxed"
        );
        let (cw, ch) = app.win_cell_size(wid);
        let (pad, grid_top) = (app.win_pad(wid), app.win_pad_top(wid) + app.win_head(wid));
        let chrome = strip_rows(&app, wid);
        let screen = c.screen();
        let last = c.cols - 1;
        let column = pad + last * cw..pad + (last + 1) * cw;
        let stroked = |row: usize| {
            let y = grid_top + row * ch + ch / 2;
            card_marks(&app, wid).0.iter().any(|m| {
                let y = pixel(y);
                (m.at.y0..m.at.y1).contains(&y)
                    && m.at.x0 < pixel(column.end)
                    && m.at.x1 > pixel(column.start)
            })
        };
        let full = |r: usize| screen[r].chars().nth(last).is_some_and(|ch| ch != ' ');
        let boxed = c.card..c.card + c.card_rows;
        if boxed.clone().any(full) {
            for r in boxed.filter(|&r| full(r)) {
                assert!(
                    !stroked(chrome + r),
                    "{name}: row {r}'s last glyph under the border"
                );
            }
            open += 1;
        } else if screen[c.card].chars().count() == last {
            for r in boxed {
                assert!(stroked(chrome + r), "{name}: CONTROL: the side at row {r}");
            }
            closed += 1;
        }
        host.shutdown_and_join();
    }
    assert_eq!(
        (open, closed),
        (3, 1),
        "CONTROL: the captures hold both edges"
    );
}

type NoticeCase = (&'static str, bool, fn(&mut App, u64));

#[test]
fn the_vendors_notice_stays_bare_where_the_banner_may_not_go() {
    let cases: [NoticeCase; 5] = [
        ("unsupervised", false, |app, session| {
            let ctx = app.pool.get(session).unwrap().ctx.clone();
            crate::session_timeline::release_supervisor(
                &ctx,
                None,
                crate::metrics::now_us(),
                aterm_slog::Actor::Ctl,
            );
        }),
        ("switched off", false, |app, _| {
            app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
                card: Some(false),
                ..Default::default()
            });
        }),
        ("master switch off", false, |app, _| {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
            );
        }),
        ("reworded", true, |app, session| {
            let mut rows = WIDE.screen();
            rows[WIDE.top + 1] =
                format!("  see {}", aterm_phase::anchor!("notice.updated_details"));
            feed_capture(app, session, &WIDE, &rows);
        }),
        ("find bar up", true, |app, _| app.search_enter()),
    ];
    for (what, line, case) in cases {
        let (mut app, wid, session) = capture_glass(&WIDE);
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        assert!(banner_shown(&app, wid), "{what}: CONTROL, the banner");
        case(&mut app, session);
        frame(&mut app, wid);
        let strip = strip_rows(&app, wid);
        assert!(
            what == "reworded" || notice_bare(&app, wid),
            "{what}: the vendor's notice bare"
        );
        assert!(!banner_shown(&app, wid), "{what}: no banner");
        assert!(
            app.windows[&wid].claude_card.notice_hits.is_empty(),
            "{what}: nothing to press"
        );
        assert_eq!(
            glass_text(&app, wid, strip + WIDE.card + 3, 0..120).contains("aterm harness"),
            line,
            "{what}: the card's line"
        );
        host.shutdown_and_join();
    }
    let (mut app, wid, session) = capture_glass(&WIDE);
    let host = supervise(&mut app, session);
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip,
    );
    crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    );
    assert!(!app.session_autopilot(session).2, "the claim is given back");
    frame(&mut app, wid);
    assert!(banner_shown(&app, wid), "Off's white card wears the banner");
    host.shutdown_and_join();
}

#[cfg(unix)]
fn cell_px(app: &App, wid: WindowId, (row, col): (usize, usize)) -> (f64, f64) {
    let (cw, ch) = app.win_cell_size(wid);
    let (ox, oy) = app.frame_origin(wid);
    let x = ox as f64 + (app.win_pad(wid) + col * cw + cw / 2) as f64;
    let y = oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + row * ch + ch / 2) as f64;
    assert_eq!(
        app.frame_cell_at(wid, x, y),
        Some((row, col)),
        "the pixel names the cell"
    );
    (x, y)
}

#[cfg(unix)]
fn link_modifier() -> aterm_window::keyboard::ModifiersState {
    #[cfg(target_os = "macos")]
    {
        aterm_window::keyboard::ModifiersState::SUPER
    }
    #[cfg(not(target_os = "macos"))]
    {
        aterm_window::keyboard::ModifiersState::CONTROL
    }
}

#[cfg(unix)]
fn click(
    app: &mut App,
    wid: WindowId,
    at: (usize, usize),
    mods: aterm_window::keyboard::ModifiersState,
) {
    click_when(app, wid, at, mods, Instant::now());
}

/// A click on `at` with `mods` held whose button went down and up at `when`.
#[cfg(unix)]
fn click_when(
    app: &mut App,
    wid: WindowId,
    at: (usize, usize),
    mods: aterm_window::keyboard::ModifiersState,
    when: Instant,
) {
    use aterm_window::event::{ElementState, MouseButton};
    let (x, y) = cell_px(app, wid, at);
    app.windows.get_mut(&wid).expect("window").mods = mods;
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input_at(wid, ElementState::Pressed, MouseButton::Left, when);
    app.on_mouse_input_at(wid, ElementState::Released, MouseButton::Left, when);
    app.windows.get_mut(&wid).expect("window").mods =
        aterm_window::keyboard::ModifiersState::empty();
}

/// A double click's interval, whose last instant still continues a run of presses.
#[cfg(unix)]
fn interval() -> Duration {
    Duration::from_millis(
        u64::try_from(crate::multi_click_ms()).expect("a double click's interval"),
    )
}

/// Where a test's stated presses start: a minute ago, far enough from the clock that a press or
/// a stamp that read the clock instead of its stated time lands on the wrong side of a double
/// click's interval.
#[cfg(unix)]
fn a_minute_ago() -> Instant {
    Instant::now()
        .checked_sub(Duration::from_secs(60))
        .expect("a minute of uptime")
}

/// How long after a press a person's next, separate click comes: a millisecond past a double
/// click's interval.
#[cfg(unix)]
fn apart() -> Duration {
    interval() + Duration::from_millis(1)
}

#[cfg(unix)]
fn pty(reader: &mut std::os::unix::net::UnixStream, want: usize) -> Vec<u8> {
    use std::io::{ErrorKind, Read};
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if got.len() >= want || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("reading the PTY observer: {e}"),
        }
    }
    got
}

#[cfg(unix)]
#[test]
fn a_press_on_the_notice_never_reaches_the_vendors_link() {
    use aterm_session::sink::SinkWriter;
    let changelog = aterm_phase::anchor!("notice.changelog");
    let vendor_url = format!("https://{changelog}");
    let tail = aterm_phase::anchor!("notice.updated_details")
        .strip_prefix(changelog)
        .expect("the details row opens with the changelog link");
    let details = format!("  \x1b]8;;{vendor_url}\x1b\\{changelog}\x1b]8;;\x1b\\{tail}");
    let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("PTY observer");
    reader.set_nonblocking(true).expect("nonblocking observer");
    let mut reader = reader;
    let mut app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new_owned(writer.into())));
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, u16_of(WIDE.rows), u16_of(WIDE.cols)));
    publish(&app, session, CAPTURED);
    feed_capture(&app, session, &WIDE, &WIDE.screen());
    frame(&mut app, wid);
    let strip = strip_rows(&app, wid);
    let (words, hidden) = ((strip + NOTICE_TOP, 6), (strip + NOTICE_TOP + 1, 10));
    let (x, y) = cell_px(&app, wid, hidden);
    app.on_cursor_moved(wid, x, y);
    assert_eq!(
        app.link_under_pointer(wid),
        None,
        "the measured row: no link"
    );
    let mut rows = WIDE.screen();
    rows[NOTICE_TOP + 1] = details;
    feed_capture(&app, session, &WIDE, &rows);
    frame(&mut app, wid);
    assert!(notice_bare(&app, wid), "CONTROL: the same text");
    app.on_cursor_moved(wid, x, y);
    assert_eq!(
        app.link_under_pointer(wid).as_deref(),
        Some(vendor_url.as_str()),
        "CONTROL: the vendor's link on its row"
    );
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    assert!(banner_shown(&app, wid), "the banner over the notice");
    app.on_cursor_moved(wid, x, y);
    assert_eq!(app.link_under_pointer(wid), None, "the hidden row's link");
    assert!(app.path_link_under_pointer(wid).is_none());
    let (wx, wy) = cell_px(&app, wid, words);
    app.on_cursor_moved(wid, wx, wy);
    assert_eq!(
        app.link_under_pointer(wid).as_deref(),
        Some(NOTICE.url),
        "the banner's link"
    );
    assert!(app.path_link_under_pointer(wid).is_none());
    let probe = |app: &mut App, mods| {
        app.windows.get_mut(&wid).unwrap().mods = mods;
        app.update_hover_cursor(wid);
        let ws = &app.windows[&wid];
        (
            ws.hover_pointer,
            ws.native_text_cursor,
            ws.link_hover.is_some(),
        )
    };
    assert_eq!(
        probe(&mut app, link_modifier()),
        (true, false, false),
        "the hand, and no caption of the hidden link"
    );
    assert_eq!(
        probe(&mut app, aterm_window::keyboard::ModifiersState::empty()),
        (true, false, false),
        "the hand without the modifier too: a plain press opens the link"
    );
    for other in [
        aterm_window::keyboard::ModifiersState::SHIFT,
        aterm_window::keyboard::ModifiersState::ALT,
    ] {
        assert_eq!(
            probe(&mut app, other),
            (false, true, false),
            "{other:?}: the I-beam, as a press with it selects"
        );
    }
    assert_eq!(
        probe(&mut app, aterm_window::keyboard::ModifiersState::empty()),
        (true, false, false),
        "CONTROL: the hand again"
    );
    app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::SHIFT);
    assert!(
        !app.windows[&wid].hover_pointer,
        "Shift going down over the still link takes the hand away"
    );
    app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::empty());
    assert!(
        app.windows[&wid].hover_pointer,
        "and its release gives it back"
    );
    app.on_cursor_moved(wid, x, y);
    assert_eq!(
        probe(&mut app, link_modifier()),
        (false, true, false),
        "no hand on the hidden row, and no caption of its link"
    );
    assert_eq!(
        probe(&mut app, aterm_window::keyboard::ModifiersState::empty()),
        (false, true, false),
        "CONTROL: the I-beam on the hidden row without the modifier"
    );
    app.windows.get_mut(&wid).unwrap().mods = aterm_window::keyboard::ModifiersState::empty();
    let _ = crate::platform::take_opened_urls();
    // Each press states when it happened, so how long the work between presses takes never
    // moves one across a double click's interval.
    let mut when = a_minute_ago();
    click_when(&mut app, wid, words, link_modifier(), when);
    assert_eq!(
        crate::platform::take_opened_urls(),
        vec![NOTICE.url.to_string()],
        "a modifier press on the words opens the banner's link"
    );
    when += apart();
    click_when(&mut app, wid, hidden, link_modifier(), when);
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "a modifier press on the hidden row opens nothing"
    );
    when += apart();
    for (k, opens) in [1, 0, 0].into_iter().enumerate() {
        if k > 0 {
            when += interval();
        }
        click_when(
            &mut app,
            wid,
            words,
            aterm_window::keyboard::ModifiersState::empty(),
            when,
        );
        assert_eq!(
            crate::platform::take_opened_urls().len(),
            opens,
            "press {k} of a triple click, each at the interval's last instant after the one \
             before: a plain press on the words opens the banner's link once"
        );
    }
    when += apart();
    click_when(
        &mut app,
        wid,
        words,
        aterm_window::keyboard::ModifiersState::empty(),
        when,
    );
    assert_eq!(
        crate::platform::take_opened_urls(),
        vec![NOTICE.url.to_string()],
        "CONTROL: a click a millisecond after the interval opens it again"
    );
    for other in [
        aterm_window::keyboard::ModifiersState::SHIFT,
        aterm_window::keyboard::ModifiersState::ALT,
    ] {
        when += apart();
        app.windows.get_mut(&wid).unwrap().last_press = None;
        click_when(&mut app, wid, words, other, when);
        assert!(
            crate::platform::take_opened_urls().is_empty(),
            "{other:?}: a press with another modifier opens nothing"
        );
        assert!(
            app.windows[&wid].last_press.is_some(),
            "{other:?}: the press is the grid's, as on the hidden row"
        );
    }
    let _ = pty(&mut reader, 0);
    when += apart();
    click_when(
        &mut app,
        wid,
        hidden,
        aterm_window::keyboard::ModifiersState::empty(),
        when,
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing tracks the mouse");
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "a plain press on the hidden row opens nothing"
    );
    {
        let term = app.pool.get(session).unwrap().term.clone();
        crate::term_lock(&term).process(b"\x1b[?1000h\x1b[?1006h");
    }
    assert!(
        app.pool
            .get(session)
            .unwrap()
            .ctx
            .modes
            .mouse_tracking_enabled()
    );
    frame(&mut app, wid);
    assert!(banner_shown(&app, wid), "the banner stays");
    let _ = pty(&mut reader, 0);
    let _ = crate::platform::take_opened_urls();
    for at in [words, hidden] {
        let opens = if at == words {
            vec![NOTICE.url.to_string()]
        } else {
            Vec::new()
        };
        when += apart();
        click_when(
            &mut app,
            wid,
            at,
            aterm_window::keyboard::ModifiersState::empty(),
            when,
        );
        assert!(
            pty(&mut reader, 0).is_empty(),
            "{at:?}: a plain press on the banner reaches no PTY"
        );
        assert_eq!(
            crate::platform::take_opened_urls(),
            opens,
            "{at:?}: under tracking a plain press opens the link alone"
        );
        when += apart();
        click_when(&mut app, wid, at, link_modifier(), when);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "{at:?}: nor does a modifier press"
        );
        assert_eq!(crate::platform::take_opened_urls(), opens, "{at:?}");
    }
    when += apart();
    app.windows.get_mut(&wid).unwrap().last_press = None;
    click_when(
        &mut app,
        wid,
        words,
        aterm_window::keyboard::ModifiersState::ALT,
        when,
    );
    assert!(
        pty(&mut reader, 0).is_empty() && crate::platform::take_opened_urls().is_empty(),
        "under tracking an Option press on the link reports nothing and opens nothing"
    );
    assert!(
        app.windows[&wid].last_press.is_some(),
        "it takes a local selection, as on the hidden row"
    );
    when += apart();
    click_when(
        &mut app,
        wid,
        (strip + 10, 40),
        aterm_window::keyboard::ModifiersState::empty(),
        when,
    );
    let report = pty(&mut reader, 6);
    assert!(
        report.starts_with(b"\x1b[<0;"),
        "CONTROL: the press reported: {report:?}"
    );
    host.shutdown_and_join();
}

#[cfg(unix)]
#[test]
fn a_press_on_the_notice_in_a_mixed_tab_is_the_banners() {
    use aterm_session::sink::SinkWriter;
    use aterm_window::keyboard::ModifiersState;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(app.apply_term_resize(wid, 30, 240));
    let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("PTY observer");
    reader.set_nonblocking(true).expect("nonblocking observer");
    let mut reader = reader;
    let (session, _) = app.split_active_with_stub_terminal_with_sink(
        wid,
        crate::tab_model::SplitAxis::Vertical,
        Arc::new(SinkWriter::new_owned(writer.into())),
    );
    let (rows, cols) = {
        let term = crate::term_lock(&app.pool.get(session).unwrap().term);
        (usize::from(term.rows()), usize::from(term.cols()))
    };
    assert!(cols >= 100, "the pane holds the notice whole ({cols})");
    let changelog = aterm_phase::anchor!("notice.changelog");
    let tail = aterm_phase::anchor!("notice.updated_details")
        .strip_prefix(changelog)
        .expect("the details row opens with the changelog link");
    let details = format!("  \x1b]8;;https://{changelog}\x1b\\{changelog}\x1b]8;;\x1b\\{tail}");
    publish(&app, session, CAPTURED);
    feed(&app, session, &capture_at(rows, cols, &details));
    let host = supervise(&mut app, session);
    let mixed_frame = |app: &mut App| {
        let Some((plan, crate::VisibleContentRoute::Heterogeneous)) =
            app.active_visible_frame_layout(wid)
        else {
            panic!("a mixed tab");
        };
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        app.splice_claude_footer(wid, &plan, crate::VisibleContentRoute::Heterogeneous);
    };
    mixed_frame(&mut app);
    let hits = app.windows[&wid].claude_card.notice_hits.clone();
    let words = hits
        .iter()
        .find(|h| h.url.is_some())
        .map(|h| (h.frame_row, h.cols.end - 1))
        .expect("the banner on the mixed tab's glass");
    let hidden = hits
        .iter()
        .find(|h| h.frame_row == words.0 + 1)
        .map(|h| (h.frame_row, h.cols.start + 8))
        .expect("the hidden details row");
    assert_eq!(
        app.focused_session_id(wid),
        Some(session),
        "CONTROL: the terminal pane has the focus"
    );
    let hand = |app: &mut App, at, mods| {
        let (x, y) = cell_px(app, wid, at);
        app.windows.get_mut(&wid).unwrap().mods = mods;
        app.on_cursor_moved(wid, x, y);
        app.update_hover_cursor(wid);
        let hand = app.windows[&wid].hover_pointer;
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        hand
    };
    assert!(
        hand(&mut app, words, ModifiersState::empty()),
        "the hand on the mixed tab's link without the modifier"
    );
    assert!(hand(&mut app, words, link_modifier()), "and with it");
    assert!(
        !hand(&mut app, hidden, ModifiersState::empty()),
        "CONTROL: no hand on the hidden row"
    );
    let _ = crate::platform::take_opened_urls();
    let mut when = a_minute_ago();
    click_when(&mut app, wid, words, link_modifier(), when);
    assert_eq!(
        crate::platform::take_opened_urls(),
        vec![NOTICE.url.to_string()],
        "the banner's link"
    );
    mixed_frame(&mut app);
    when += apart();
    click_when(&mut app, wid, hidden, link_modifier(), when);
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "nothing from the hidden row"
    );
    {
        let term = app.pool.get(session).unwrap().term.clone();
        crate::term_lock(&term).process(b"\x1b[?1000h\x1b[?1006h");
    }
    mixed_frame(&mut app);
    let _ = pty(&mut reader, 0);
    for at in [words, hidden] {
        when += apart();
        click_when(&mut app, wid, at, ModifiersState::empty(), when);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "{at:?}: a plain press on the banner reaches no PTY"
        );
        let opens = if at == words {
            vec![NOTICE.url.to_string()]
        } else {
            Vec::new()
        };
        assert_eq!(
            crate::platform::take_opened_urls(),
            opens,
            "{at:?}: a plain press opens the banner's link, and nothing from the hidden row"
        );
    }
    when += apart();
    click_when(
        &mut app,
        wid,
        (words.0 + 6, words.1),
        ModifiersState::empty(),
        when,
    );
    let report = pty(&mut reader, 6);
    assert!(
        report.starts_with(b"\x1b[<0;"),
        "CONTROL: the press reported: {report:?}"
    );
    host.shutdown_and_join();
}

#[cfg(unix)]
#[test]
fn a_press_on_an_unfocused_panes_banner_focuses_the_pane() {
    use aterm_session::sink::SinkWriter;
    use aterm_window::keyboard::ModifiersState;
    let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("PTY observer");
    reader.set_nonblocking(true).expect("nonblocking observer");
    let mut reader = reader;
    let mut app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new_owned(writer.into())));
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 24, 160));
    let other = app.split_active_stub_tab(wid);
    assert_eq!(
        app.focused_session_id(wid),
        Some(other),
        "CONTROL: the new pane has the focus"
    );
    let (rows, cols) = {
        let term = crate::term_lock(&app.pool.get(session).unwrap().term);
        (usize::from(term.rows()), usize::from(term.cols()))
    };
    let [_, details] = notice_rows();
    publish(&app, session, CAPTURED);
    feed(&app, session, &capture_at(rows, cols, &details));
    let host = supervise(&mut app, session);
    {
        let term = app.pool.get(session).unwrap().term.clone();
        crate::term_lock(&term).process(b"\x1b[?1000h\x1b[?1006h");
    }
    frame(&mut app, wid);
    let words = app.windows[&wid]
        .claude_card
        .notice_hits
        .iter()
        .find(|h| h.url.is_some())
        .map(|h| (h.frame_row, h.cols.end - 1))
        .expect("the banner on the unfocused pane");
    let hand = |app: &mut App| {
        let (x, y) = cell_px(app, wid, words);
        app.on_cursor_moved(wid, x, y);
        app.update_hover_cursor(wid);
        app.windows[&wid].hover_pointer
    };
    assert!(
        !hand(&mut app),
        "no hand on an unfocused pane's link: a press there only focuses"
    );
    let _ = pty(&mut reader, 0);
    let _ = crate::platform::take_opened_urls();
    click(&mut app, wid, words, ModifiersState::empty());
    assert_eq!(
        app.focused_session_id(wid),
        Some(session),
        "the press on the banner focuses its pane"
    );
    assert!(
        pty(&mut reader, 0).is_empty(),
        "the focus click reports nothing"
    );
    assert!(
        crate::platform::take_opened_urls().is_empty(),
        "the focus click opens nothing"
    );
    frame(&mut app, wid);
    assert!(hand(&mut app), "focused, the hand on the link");
    click(&mut app, wid, words, ModifiersState::empty());
    assert!(
        pty(&mut reader, 0).is_empty(),
        "focused, a plain press on the banner reaches no PTY"
    );
    assert_eq!(
        crate::platform::take_opened_urls(),
        vec![NOTICE.url.to_string()],
        "focused, a plain press on the banner opens its link"
    );
    assert_eq!(app.focused_session_id(wid), Some(session));
    click(
        &mut app,
        wid,
        (words.0 + 6, words.1),
        ModifiersState::empty(),
    );
    let report = pty(&mut reader, 6);
    assert!(
        report.starts_with(b"\x1b[<0;"),
        "CONTROL: the press reported: {report:?}"
    );
    host.shutdown_and_join();
}

#[cfg(unix)]
#[test]
fn a_press_on_the_banners_link_settles_a_tab_name_being_edited() {
    use crate::app_rename::RenameSurface;
    use aterm_window::keyboard::ModifiersState;
    for mods in [ModifiersState::empty(), link_modifier()] {
        let (mut app, wid, session) = capture_glass(&WIDE);
        app.tab_strip_rows = 1;
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        assert!(
            banner_shown(&app, wid),
            "{mods:?}: the banner over the notice"
        );
        let words = app.windows[&wid]
            .claude_card
            .notice_hits
            .iter()
            .find(|h| h.url.is_some())
            .map(|h| (h.frame_row, h.cols.end - 1))
            .expect("the banner's link");
        let tab = app.windows[&wid].tab_set.active_id().expect("Claude's tab");
        assert!(app.begin_session_rename(wid, tab));
        assert_eq!(
            app.inline_rename_edit(wid).map(|edit| edit.surface),
            Some(RenameSurface::InGrid),
            "{mods:?}: CONTROL: a tab name being edited"
        );
        let _ = crate::platform::take_opened_urls();
        click(&mut app, wid, words, mods);
        assert!(
            app.inline_rename_edit(wid).is_none(),
            "{mods:?}: the press on the link settled the edit"
        );
        assert_eq!(
            crate::platform::take_opened_urls(),
            vec![NOTICE.url.to_string()],
            "{mods:?}: and opened the link"
        );
        host.shutdown_and_join();
    }
}

#[test]
fn a_notice_too_narrow_for_the_url_draws_the_sites_name_inside_its_end() {
    let (mut app, wid, session) = capture_glass(&WIDE);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    assert!(
        banner_shown(&app, wid),
        "CONTROL: the banner over the notice"
    );
    let pane = PaneGeom {
        strip: strip_rows(&app, wid),
        row_off: 0,
        col_off: 0,
        rows: WIDE.rows,
        cols: WIDE.cols,
    };
    let ws = &app.windows[&wid];
    let gate = app
        .pool
        .get(session)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .card_gate();
    let mut check = GateCheck::new(gate, FenceFrame::of(&ws.input_scratch));
    let leaf = card_leaf(
        &ws.claude_footer_texts.borrow(),
        app.config.claude_footer_card(),
        0,
        session,
        pane,
        RenderCell::default(),
        AutopilotView::Cruise,
        true,
        &mut check,
    );
    let (read, _, _) = leaf.card.as_ref().expect("the decorated card's reading");
    let place = leaf.notice.expect("the notice beside the card");
    let card = &ws.claude_card.entries[0].card;
    let drawn = |room: usize| {
        let place = NoticePlace {
            end: place.text_col + LINE_LEAD + room,
            ..place
        };
        let mut notice = NoticeGlass::default();
        make_notice(
            &mut notice,
            card,
            &place,
            read,
            pane,
            RenderCell::default(),
            &ws.input_scratch,
            CardPalette::default(),
            None,
            false,
        );
        (notice, place)
    };
    let (url, short) = (text_cells(NOTICE.text), text_cells(NOTICE.short));
    for room in short..=url + 1 {
        let want = if room >= url {
            NOTICE.text
        } else {
            NOTICE.short
        };
        let words = text_cells(want);
        let (notice, place) = drawn(room);
        assert!(notice.on, "{room}: a banner");
        let row = &notice.rows()[0];
        let end = pane.frame_col(place.end);
        assert_eq!(
            row.col + row.cells.len(),
            end,
            "{room}: the row ends at the notice's end"
        );
        let letters: String = row.cells[LINE_LEAD..LINE_LEAD + words]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(letters, want, "{room}: the widest words that fit");
        let underlined: Vec<usize> = (0..row.cells.len())
            .filter(|&k| row.cells[k].underline == UnderlineStyle::Single)
            .collect();
        assert_eq!(
            underlined,
            (LINE_LEAD..LINE_LEAD + words).collect::<Vec<_>>(),
            "{room}: those letters underlined, nothing else"
        );
        assert_eq!(
            notice.link,
            row.col..row.col + LINE_LEAD + words,
            "{room}: the link spans the star and the words drawn"
        );
        assert!(notice.link.end <= end, "{room}: the link inside the end");
        assert!(
            notice.ink.len() == words && notice.ink.iter().all(|c| usize::from(c.col) < end),
            "{room}: the words' ink, inside the end"
        );
    }
    assert!(
        !drawn(short - 1).0.on,
        "neither of the banner's words fits: no banner"
    );
    host.shutdown_and_join();
}

#[test]
fn a_settled_notice_at_rest_costs_the_frame_nothing() {
    let [summary, details] = notice_rows();
    let notice = format!(
        "\x1b7\x1b[{};1H{summary}\x1b[{};1H{details}\x1b8",
        NOTICE_TOP + 1,
        NOTICE_TOP + 2
    );
    costs_the_floor("notice", 12, |i, app, bare| {
        if i == 0 {
            feed_both(app, bare, notice.as_bytes());
            return;
        }
        assert!(banner_shown(&app.0, app.1), "pass {i}: the banner");
        let spinner = ['\u{2722}', '\u{2733}', '\u{2736}', '\u{273B}'][i % 4];
        let bytes = format!(
            "\x1b7\x1b[9;1H\u{23FA} streamed line {i}\x1b[K\
             \x1b[12;1H{spinner} Thinking\u{2026} ({i}s)\x1b[K\x1b8"
        );
        feed_both(app, bare, bytes.as_bytes());
    });
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let term = app.pool.get(session).unwrap().term.clone();
    let step = |app: &mut App, bytes: &[u8]| {
        crate::term_lock(&term).process(bytes);
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
        pass_cost(app, wid)
    };
    assert_eq!(
        step(&mut app, notice.as_bytes()).notice_reads,
        1,
        "it comes"
    );
    assert!(banner_shown(&app, wid));
    assert_eq!(step(&mut app, b"x").notice_reads, 0, "kept");
    let reworded = format!(
        "\x1b7\x1b[{};1H  see {}\x1b8",
        NOTICE_TOP + 2,
        aterm_phase::anchor!("notice.updated_details")
    );
    assert_eq!(
        step(&mut app, reworded.as_bytes()).notice_reads,
        1,
        "CONTROL: a change to its rows is scanned again"
    );
    assert!(!banner_shown(&app, wid), "CONTROL: and the banner goes");
    set_now(None);
    host.shutdown_and_join();
}

const SCROLLED_29: &str = include_str!(
    "../../aterm-phase/src/fixtures/claude-2.1.304-update-notice-scrolled-29rows-fullscreen-120col.txt"
);

#[test]
fn the_banner_stays_while_the_card_scrolls_off() {
    let cut: Vec<String> = SCROLLED_29.lines().skip(1).map(str::to_owned).collect();
    let mut whole = vec![format!(
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {}{CAPTURED}",
        banner()
    )];
    whole.extend(cut.iter().cloned());
    assert_eq!(whole.len(), 30, "CONTROL: the derived whole screen");
    let scrolled = |k: usize| -> Vec<String> {
        let mut rows = whole[k..].to_vec();
        rows.resize(whole.len(), String::new());
        rows
    };
    let paint = |app: &App, session: u64, rows: &[String]| {
        let mut out = String::from("\x1b[2J");
        for (i, row) in rows.iter().enumerate() {
            let sgr = if row.ends_with("/proj") {
                format!("\x1b[38;2;{};{};{}m", GRAY[0], GRAY[1], GRAY[2])
            } else if row.starts_with(&format!("  {}", aterm_phase::anchor!("notice.updated"))) {
                "\x1b[1m".to_string()
            } else if row.contains(aterm_phase::anchor!("notice.updated_details")) {
                "\x1b[38;2;153;153;153m".to_string()
            } else {
                String::new()
            };
            out.push_str(&format!("\x1b[{};1H{sgr}{row}\x1b[0m", i + 1));
        }
        let term = app.pool.get(session).expect("session").term.clone();
        crate::term_lock(&term).process(out.as_bytes());
    };
    let words = text_cells(NOTICE.text);
    let at = 2 + LINE_LEAD;
    let banner_on = |app: &App, wid: WindowId, top: usize| {
        let strip = strip_rows(app, wid);
        glass_text(app, wid, strip + top, at..at + words) == NOTICE.text
            && glass_text(app, wid, strip + top + 1, 0..74)
                .trim()
                .is_empty()
    };
    let bare_on = |app: &App, wid: WindowId, rows: &[String], top: usize| {
        let strip = strip_rows(app, wid);
        (top..top + 2).all(|r| glass_text(app, wid, strip + r, 0..text_cells(&rows[r])) == rows[r])
    };
    let window = || {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        assert!(app.apply_term_resize(wid, 30, 120));
        publish(&app, session, CAPTURED);
        let host = supervise(&mut app, session);
        (app, wid, session, host)
    };
    let (mut app, wid, session, host) = window();
    for k in 0..=3 {
        let rows = scrolled(k);
        paint(&app, session, &rows);
        frame(&mut app, wid);
        let top = 4 - k;
        assert!(
            rows[top].starts_with(&format!("  {}", aterm_phase::anchor!("notice.updated"))),
            "CONTROL: the notice at row {top}: {rows:#?}"
        );
        assert_eq!(
            banner_on(&app, wid, top),
            k < 3,
            "scrolled {k}: the banner while a row of the card stands"
        );
        assert_eq!(bare_on(&app, wid, &rows, top), k == 3, "scrolled {k}");
        assert_eq!(
            decorated(&app, wid).map(|p| p.top),
            (k == 0).then_some(0),
            "scrolled {k}: the box only round the whole card"
        );
        assert!(
            engine_rows(&app, session)
                .iter()
                .zip(&rows)
                .all(|(drawn, fed)| drawn.starts_with(fed.as_str())),
            "scrolled {k}: GLASS ONLY"
        );
        frame(&mut app, wid);
        assert_eq!(banner_on(&app, wid, top), k < 3, "scrolled {k}: kept");
    }
    host.shutdown_and_join();
    let (mut app, wid, session, host) = window();
    for k in [0, 2, 1, 0] {
        paint(&app, session, &scrolled(k));
        frame(&mut app, wid);
        assert!(
            banner_on(&app, wid, 4 - k),
            "scrolled back to {k}: the banner"
        );
    }
    assert_eq!(
        decorated(&app, wid).map(|p| p.top),
        Some(0),
        "whole again: the box"
    );
    paint(&app, session, &scrolled(2));
    frame(&mut app, wid);
    let mut other = scrolled(1);
    other[0] = other[0].replace("API Usage Billing", "Claude Max");
    paint(&app, session, &other);
    frame(&mut app, wid);
    assert!(
        bare_on(&app, wid, &other, 3) && !banner_on(&app, wid, 3),
        "NEGATIVE CONTROL: another card's row scrolled back"
    );
    host.shutdown_and_join();
    let (mut app, wid, session, host) = window();
    let rows = scrolled(1);
    paint(&app, session, &rows);
    frame(&mut app, wid);
    assert!(
        bare_on(&app, wid, &rows, 3) && !banner_on(&app, wid, 3),
        "NEGATIVE CONTROL: no card was whole on this window"
    );
    host.shutdown_and_join();
    let (mut app, wid, session, host) = window();
    paint(&app, session, &whole);
    frame(&mut app, wid);
    assert!(banner_on(&app, wid, 4), "CONTROL: the whole card's banner");
    let mut quoted = scrolled(1);
    quoted[0] = "\u{23FA} The notice read:".into();
    quoted[1] = String::new();
    paint(&app, session, &quoted);
    frame(&mut app, wid);
    assert!(
        bare_on(&app, wid, &quoted, 3) && !banner_on(&app, wid, 3),
        "NEGATIVE CONTROL: a reply quoting the notice is never hidden"
    );
    host.shutdown_and_join();
    let (mut app, wid, session, host) = window();
    paint(&app, session, &whole);
    frame(&mut app, wid);
    assert!(banner_on(&app, wid, 4), "CONTROL: the whole card's banner");
    let rows = scrolled(1);
    paint(&app, session, &rows);
    frame(&mut app, wid);
    assert!(banner_on(&app, wid, 3), "CONTROL: scrolled, the banner");
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Land),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    assert!(
        banner_on(&app, wid, 3),
        "the mode moved while the card is scrolled: the banner stays"
    );
    host.shutdown_and_join();
    let (mut app, wid, session, host) = window();
    paint(&app, session, &whole);
    frame(&mut app, wid);
    app.windows.get_mut(&wid).unwrap().search = Some(Default::default());
    frame(&mut app, wid);
    app.windows.get_mut(&wid).unwrap().search = None;
    paint(&app, session, &rows);
    frame(&mut app, wid);
    assert!(
        banner_on(&app, wid, 3),
        "a find bar opened and closed over the whole card: the banner"
    );
    host.shutdown_and_join();
}

#[test]
fn a_build_drift_is_one_entry_and_leaves_the_decoration_alone() {
    use aterm_agent::harness::footer::FactsOwner;

    let run = |image: &str| {
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        launch(&app, session, image, PGID, 5);
        feed(&app, session, &card_screen(24, 120, MODEL, image));
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap();
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
    cut_out(&mut cards, &mut input, menu, 1, 0);
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

#[cfg(a11y_tree)]
#[path = "claude_card_a11y_tests.rs"]
mod a11y_tests;

#[path = "claude_card_gate_tests.rs"]
mod gate;

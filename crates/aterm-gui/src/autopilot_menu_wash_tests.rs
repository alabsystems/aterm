// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pixels a footer card adds under its rows' glyphs (§1.4, §1.5): the one wash of the row
//! or the slider's stop the pointer or the keys rest on, its fade once they leave, a slider's
//! track and a warn note's wash, on rendered CPU and GPU frames and on an open card's layers.

use aterm_core::render::{FxLayer, PxRect, RenderInput};
use aterm_effects::harness_card::CardPalette;
use std::time::Duration;

use super::*;
use crate::footer_card_plate::tests::bundled_cpu;
use crate::footer_card_plate::{CardPlate, CardWash};

/// A card of a head, two choices, a slider with its thumb on its second stop, a warn note and
/// a plain note.
fn washed_card() -> Vec<MenuRow> {
    vec![
        MenuRow::Header("Effort".to_string()),
        choice("", "First", FactAct::NewTabHere, true, false),
        choice("", "Second", FactAct::CopyPath, true, false),
        MenuRow::stops(
            "Effort",
            [stop("low", true), stop("high", true), stop("max", true)],
            Some(1),
        ),
        MenuRow::Note {
            text: "out until Mon".to_string(),
            spoken: "out until Mon".to_string(),
            tone: NoteTone::Warn,
        },
        MenuRow::note("a note"),
    ]
}

const ITEM: usize = 1;
const SLIDER: usize = 3;
const WARN: usize = 4;
const NOTE: usize = 5;

/// The card on a blank page, as the window lays it, with what was washed.
struct Scene {
    /// The page with the card's cells, its occluder and its frame's layer.
    framed: RenderInput,
    /// The wash's layer; `None` where nothing is washed.
    wash: Option<FxLayer>,
    card: PxRect,
    inks: CardInks,
    cell: (i32, i32),
    rect: MenuRect,
    rows: Vec<MenuRow>,
}

impl Scene {
    fn washed(&self) -> RenderInput {
        let mut input = self.framed.clone();
        input.fx_layers.extend(self.wash.clone());
        input
    }

    /// The pixel index, in a frame `width` wide, of the point `(half, part)` of the card's row
    /// `i`: `half` half cells from the card's left edge, `part` 64ths of a cell down the row.
    fn at(&self, width: usize, i: usize, half: usize, part: i32) -> usize {
        let x = self.card.x0 + i32::try_from(half).unwrap() * self.cell.0 / 2;
        let y =
            self.card.y0 + i32::try_from(1 + i).unwrap() * self.cell.1 + self.cell.1 * part / 64;
        usize::try_from(y).unwrap() * width + usize::try_from(x).unwrap()
    }
}

fn scene(
    cpu: &aterm_render::Renderer,
    palette: Option<CardPalette>,
    washed: Option<Washed>,
) -> Scene {
    let mut rows = washed_card();
    if let (Some((i, stop)), MenuRow::Stops { focus, .. }) = (washed, &mut rows[SLIDER]) {
        *focus = stop.filter(|_| i == SLIDER);
    }
    scene_of(cpu, rows, palette, washed)
}

/// A card of `rows` on a blank page, laid as the window lays it.
fn scene_of(
    cpu: &aterm_render::Renderer,
    rows: Vec<MenuRow>,
    palette: Option<CardPalette>,
    washed: Option<Washed>,
) -> Scene {
    let theme = aterm_render::Theme::default();
    let cell = cpu.cell_size();
    let mut term = aterm_core::terminal::Terminal::new(14, 60);
    let page: String = (1..=14)
        .map(|r| format!("\x1b[{r};1H{}", ".".repeat(60)))
        .collect();
    term.process(format!("\x1b[?25l{page}").as_bytes());
    let mut bare = term.cell_frame(14, 60);
    let rect = MenuRect {
        row: 2,
        col: 6,
        w: 40,
        h: rows.len() + 2,
    };
    let viewport = MenuViewport::default();
    let painted = paint(rect, &rows, viewport, washed.map(|w| w.0), theme, palette);
    for (r, line) in painted.iter().enumerate() {
        bare.cells[rect.row + r][rect.col..rect.col + rect.w].copy_from_slice(line);
    }
    let card =
        crate::app_render::scrub_overlay_card(&mut bare, rect, cell, (cpu.pad(), cpu.grid_top()))
            .expect("a card");
    let inks = CardInks::of(theme, palette);
    let key = plate_key(rect, &inks, cell, 1.0, false);
    // The layers the window pushes ([`card_layers`]).
    let [frame, wash] = card_layers(
        (rect, &rows, viewport),
        (&inks, key),
        Some(card),
        (9, 10),
        (&mut CardPlate::default(), &mut CardWash::default()),
        washed.map(|washed| (washed, inks.wash.1)),
    );
    bare.fx_layers.push(frame.expect("a frame"));
    let px = |side: usize| i32::try_from(side).expect("px");
    Scene {
        framed: bare,
        wash,
        card,
        inks,
        cell: (px(cell.0), px(cell.1)),
        rect,
        rows,
    }
}

const fn rgb(p: u32) -> [u8; 3] {
    [(p >> 16) as u8, (p >> 8) as u8, p as u8]
}

/// Whether `got` is `ink` at `alpha` over `ground`, to one step a channel.
fn is_over(got: u32, ink: [u8; 3], alpha: u8, ground: [u8; 3]) -> bool {
    let (got, a) = (rgb(got), u32::from(alpha));
    (0..3).all(|c| {
        let want = (u32::from(ink[c]) * a + u32::from(ground[c]) * (255 - a) + 127) / 255;
        u32::from(got[c]).abs_diff(want) <= 1
    })
}

/// THE WASH IS ONE ROUNDED BAR UNDER THE ROW'S GLYPHS, on the CPU's pixels: the highlighted
/// choice stands on the pane's ink at [`WASH_ALPHA`] over the card's ground from half a cell
/// inside the card's left edge to half a cell inside its right, a point clear of the rows
/// above and below, its corners rounded away; the row's glyphs keep every pixel of their own
/// ink; no other row moves a pixel. A slider's rested stop wears the same bar, the same tone,
/// over its span and half a cell either side, and nowhere else on its row. Negative controls:
/// with no wash layer the row is the ground; a note and a warn note are washed by nothing.
#[test]
fn the_wash_is_one_rounded_bar_under_the_rows_glyphs() {
    let mut cpu = bundled_cpu();
    for palette in [Some(CardPalette::Cruise), None] {
        let s = scene(&cpu, palette, Some((ITEM, None)));
        let before = cpu.render_input(&s.framed);
        let (width, before) = (before.width, before.pixels);
        let after = cpu.render_input(&s.washed()).pixels;
        let (ground, (ink, alpha)) = (s.inks.ground, s.inks.wash);
        assert_eq!(alpha, WASH_ALPHA);
        let last = 2 * s.rect.w;
        // The row's blank end, at its middle height: washed inside the bar, ground outside.
        for half in [2, last / 2, last - 3] {
            let i = s.at(width, ITEM, half, 32);
            assert!(is_over(after[i], ink, alpha, ground), "{palette:?}: {half}");
            assert_eq!(rgb(before[i]), ground, "CONTROL: no layer, no wash");
        }
        let tone = rgb(after[s.at(width, ITEM, 2, 32)]);
        assert!(
            (0..3).all(|c| tone[c].abs_diff(s.inks.sel_bg()[c]) <= 1),
            "{tone:?} is the tone the row's words are floored on, {:?}",
            s.inks.sel_bg()
        );
        // Half a cell in from each edge, less a pixel: the card's ground.
        assert_eq!(
            after[s.at(width, ITEM, 1, 32) - 1],
            before[s.at(width, ITEM, 1, 32) - 1]
        );
        assert_eq!(
            after[s.at(width, ITEM, last - 1, 32)],
            before[s.at(width, ITEM, last - 1, 32)]
        );
        // A point clear of the row above, and the bar's corner rounded away.
        assert_eq!(
            rgb(after[s.at(width, ITEM, last / 2, 0)]),
            ground,
            "the top point"
        );
        assert!(
            is_over(
                after[s.at(width, ITEM, last / 2, 0) + width],
                ink,
                alpha,
                ground
            ),
            "the bar's first row of pixels"
        );
        assert_eq!(
            rgb(after[s.at(width, ITEM, 1, 0) + width]),
            ground,
            "its corner"
        );
        // Glyphs keep their ink: a pixel wholly of the row's words is the same with the wash.
        let row = s.at(width, ITEM, 0, 0)..s.at(width, ITEM + 1, 0, 0);
        let inked = row
            .clone()
            .filter(|&i| rgb(before[i]) == s.inks.sel_fg)
            .inspect(|&i| assert_eq!(after[i], before[i], "{palette:?}: a glyph's pixel"))
            .count();
        assert!(inked > 0, "the row's words draw in the hover ink");
        // No other row moves.
        for i in (0..before.len()).filter(|i| !row.contains(i)) {
            assert_eq!(
                after[i], before[i],
                "{palette:?}: pixel {i} outside the row"
            );
        }

        // A slider's rested stop: the same bar over its span.
        let s = scene(&cpu, palette, Some((SLIDER, Some(0))));
        let before = cpu.render_input(&s.framed).pixels;
        let after = cpu.render_input(&s.washed()).pixels;
        let (text_start, text_end) = text_span(s.rect.w, &s.rows);
        let MenuRow::Stops { stops, .. } = &s.rows[SLIDER] else {
            panic!("the slider");
        };
        let spans = stop_spans(stops, text_end - text_start);
        let (from, to) = (
            2 * (text_start + spans[0].start),
            2 * (text_start + spans[0].end),
        );
        // A stop's mark and its blank: the blank's cell is washed.
        assert!(is_over(
            after[s.at(width, SLIDER, from + 3, 32)],
            ink,
            alpha,
            ground
        ));
        assert!(
            is_over(after[s.at(width, SLIDER, to, 32)], ink, alpha, ground),
            "half a cell past"
        );
        let beyond = s.at(width, SLIDER, to + 1, 32) + 1;
        assert_eq!(after[beyond], before[beyond], "and no further");
        let other = s.at(width, SLIDER, 2 * (text_start + spans[2].start) + 3, 32);
        assert_eq!(after[other], before[other], "another stop is not washed");

        // Rows that are no choice take no wash.
        for i in [NOTE, WARN, 0] {
            let s = scene(&cpu, palette, Some((i, None)));
            assert!(
                s.wash.is_none(),
                "{palette:?}: row {i} is washed by nothing"
            );
        }
    }
}

/// A SLIDER'S TRACK AND A WARN NOTE'S WASH ARE THE FRAME'S PIXELS, on the CPU's frame: between
/// two stops a capsule two points tall at the row's middle, the glass's lit ink before the
/// thumb and its track ink after it, the row's ground above and below it; under a warn note's
/// words, where the warn ink has a hue (Cruise), a pill of it at [`WARN_WASH`] from half a
/// cell before the words to half a cell past them, and with no palette, where it has none, no
/// pill. Negative control: a plain note's row is the ground from edge to edge.
#[test]
fn a_sliders_track_and_a_warn_notes_wash_are_the_frames_pixels() {
    let mut cpu = bundled_cpu();
    let s = scene(&cpu, Some(CardPalette::Cruise), None);
    let frame = cpu.render_input(&s.framed);
    let (width, px) = (frame.width, frame.pixels);
    let (text_start, text_end) = text_span(s.rect.w, &s.rows);
    let MenuRow::Stops { stops, .. } = &s.rows[SLIDER] else {
        panic!("the slider");
    };
    let spans = stop_spans(stops, text_end - text_start);
    let gap = |j: usize| text_start + (spans[j].end + spans[j + 1].start) / 2;
    let (lit, track) = (unpack(s.inks.glass.lit()), unpack(s.inks.glass.track()));
    let mid = |j: usize| s.at(width, SLIDER, 2 * gap(j) + 1, 32);
    assert_eq!(rgb(px[mid(0)]), lit, "before the thumb: lit");
    assert_eq!(rgb(px[mid(1)]), track, "after it: the track");
    assert_ne!(lit, track);
    for j in [0, 1] {
        let tall = (-3..3)
            .filter(|dy| rgb(px[(mid(j) as i64 + dy * width as i64) as usize]) != s.inks.ground)
            .count();
        assert_eq!(tall, 2, "gap {j}: two points tall at 1x");
    }
    // A blank parts the track from each stop.
    let blank = s.at(width, SLIDER, 2 * (text_start + spans[0].end) + 1, 32);
    assert_eq!(rgb(px[blank]), s.inks.ground, "the blank after a stop");

    let cells = "out until Mon".len();
    let warn = s.inks.warn();
    let alpha = (WARN_WASH * 255.0).round() as u8;
    assert_ne!(
        s.inks.glass.warn(),
        s.inks.glass.lit(),
        "Cruise's warn has a hue"
    );
    let before = s.at(width, WARN, 2 * text_start - 1, 32) + 1;
    let past = s.at(width, WARN, 2 * (text_start + cells), 32);
    for i in [before, past] {
        assert!(
            is_over(px[i], warn, alpha, s.inks.ground),
            "the warn pill at {i}"
        );
    }
    let beyond = s.at(width, WARN, 2 * (text_start + cells) + 2, 32);
    assert_eq!(
        rgb(px[beyond]),
        s.inks.ground,
        "half a cell past the words, no more"
    );
    let row = s.at(width, NOTE, 0, 0) + 2..s.at(width, NOTE, 2 * s.rect.w, 0) - 2;
    assert!(!row.is_empty());
    assert!(
        row.clone().all(|i| rgb(px[i]) == s.inks.ground),
        "CONTROL: a plain note's first pixel row is the ground"
    );

    let plain = scene(&cpu, None, None);
    assert_eq!(
        plain.inks.glass.warn(),
        plain.inks.glass.lit(),
        "no palette: no hue"
    );
    let px = cpu.render_input(&plain.framed).pixels;
    assert_eq!(rgb(px[before]), plain.inks.ground, "and no pill");
}

/// THE CARD IS OPAQUE ON A WALLPAPER AND ONE GROUND IN A SEE-THROUGH WINDOW, on the CPU's
/// pixels, under a palette and with none: over a wallpaper, which a renderer shows through
/// every cell of the pane's own ground, a blank of each of the card's rows is the card's
/// ground, as a blank of its first and of its last row is; in a window whose ground lets the
/// desktop through, the same blanks are the first row's pixel, the card's ground at one
/// opacity from its first row to its last (the window's own: a layer does not change how much
/// of the desktop a pixel lets through). A drawn row's busy cell and its warn cell keep the
/// ground of their own that their cells carry: the card's ground is drawn round them.
/// Negative control: with no frame layer the wallpaper shows in the card's body.
#[test]
fn the_card_is_opaque_on_a_wallpaper_and_one_ground_in_a_see_through_window() {
    let plot = |ink: PlotInk, busy: bool| PlotCell { ch: ' ', ink, busy };
    let rows = || {
        vec![
            MenuRow::Header("Account".to_string()),
            MenuRow::plot(
                vec![
                    plot(PlotInk::Value, false),
                    plot(PlotInk::Warn, false),
                    plot(PlotInk::Warn, false),
                    plot(PlotInk::Value, false),
                    plot(PlotInk::Value, true),
                ],
                "a drawn row",
            ),
            MenuRow::Rule,
            MenuRow::note("a note"),
        ]
    };
    for palette in [Some(CardPalette::Cruise), None] {
        let mut cpu = bundled_cpu();
        let s = scene_of(&cpu, rows(), palette, None);
        let (text_start, _) = text_span(s.rect.w, &s.rows);
        let ground = s.inks.ground;
        let plain = cpu.render_input(&s.framed);
        let (width, height, plain) = (plain.width, plain.height, plain.pixels);
        // The middle of a cell of the card: row `r` of the card, column `col`.
        let at = |r: usize, col: usize| {
            let x = s.card.x0 + i32::try_from(col).unwrap() * s.cell.0 + s.cell.0 / 2;
            let y = s.card.y0 + i32::try_from(r).unwrap() * s.cell.1 + s.cell.1 / 4;
            usize::try_from(y).unwrap() * width + usize::try_from(x).unwrap()
        };
        let far = s.rect.w - 3;
        let blanks = [
            at(0, far),
            at(1, far),
            at(2, far),
            at(3, 1),
            at(4, far),
            at(5, far),
        ];
        let (warn, busy) = (at(2, text_start + 1), at(2, text_start + 4));
        let wash = warn_wash(&s.inks).map(|(wash, _)| wash);
        assert_eq!(wash.is_some(), palette.is_some(), "Cruise's warn has a hue");
        let own = |px: &[u32], what: &str| {
            assert_eq!(
                rgb(px[warn]),
                wash.unwrap_or(ground),
                "{palette:?} {what}: a warn cell's ground"
            );
            assert_ne!(rgb(px[busy]), ground, "{palette:?} {what}: a busy cell's");
            assert_eq!(px[busy], plain[busy], "{palette:?} {what}: a busy cell's");
        };
        own(&plain, "a plain window");

        let mut bare = s.framed.clone();
        bare.fx_layers.clear();
        let paper = std::sync::Arc::new(aterm_core::render::SceneAtlas {
            width: u32::try_from(width).unwrap(),
            height: u32::try_from(height).unwrap(),
            rgba: [0xC0, 0x20, 0x90, 0xFF].repeat(width * height),
            version: 1,
        });
        let mut papered = s.framed.clone();
        papered.wallpaper = Some(std::sync::Arc::clone(&paper));
        bare.wallpaper = Some(paper);
        let over = cpu.render_input(&papered).pixels;
        for (k, i) in blanks.into_iter().enumerate() {
            assert_eq!(
                rgb(over[i]),
                ground,
                "{palette:?}: blank {k} on a wallpaper"
            );
            assert_eq!(over[i], over[blanks[0]], "{palette:?}: as the first row's");
        }
        own(&over, "a wallpaper");
        let shown = cpu.render_input(&bare).pixels;
        assert_ne!(
            rgb(shown[blanks[1]]),
            ground,
            "CONTROL {palette:?}: with no frame the body holds the wallpaper out"
        );

        cpu.set_background_opacity(0.5);
        let through = cpu.render_input(&s.framed).pixels;
        for (k, i) in blanks.into_iter().enumerate() {
            assert_eq!(
                through[i], through[blanks[0]],
                "{palette:?}: blank {k} in a see-through window"
            );
            assert_eq!(rgb(through[i]), ground);
        }
    }
}

/// THE GPU DRAWS THE WASH THE CPU DRAWS: on every backend the wash moves the card's pixels,
/// and on Metal, held to the CPU's bytes, every pixel inside the card that holds no glyph's ink
/// is the CPU's byte for byte, under a washed choice and under a washed stop: the wash, the
/// track and the warn pill. A glyph's soft edge over the translucent wash is held to one step
/// a channel, as it is over the frame's shadow: the two round it apart.
#[test]
fn the_gpu_draws_the_wash_the_cpu_draws() {
    let theme = aterm_render::Theme::default();
    let gpu = aterm_gpu::GpuContext::new()
        .and_then(|ctx| aterm_gpu::GpuRenderer::from_parts(ctx, bundled_cpu(), None, theme));
    let mut gpu = match gpu {
        Ok(gpu) => gpu,
        Err(error) => {
            aterm_spec::host::could_not_run(format_args!("no GPU renderer on this host: {error}"))
        }
    };
    let mut cpu = bundled_cpu();
    assert_eq!(cpu.cell_size(), gpu.cell_size(), "both lay the same cells");
    let mut win = aterm_gpu::WindowGpu::new();
    for washed in [(ITEM, None), (SLIDER, Some(2))] {
        let s = scene(&cpu, Some(CardPalette::Land), Some(washed));
        let got_framed = gpu.render_input(&mut win, &s.framed, None).pixels;
        let got = gpu.render_input(&mut win, &s.washed(), None).pixels;
        assert_ne!(got, got_framed, "{washed:?}: the GPU draws the wash");
        if !cfg!(target_os = "macos") {
            continue;
        }
        // The card with no glyph on it: where a pixel of it is no glyph's.
        let mut blank = s.washed();
        for line in &mut blank.cells[s.rect.row..s.rect.row + s.rect.h] {
            for cell in &mut line[s.rect.col..s.rect.col + s.rect.w] {
                cell.ch = ' ';
            }
        }
        blank.chrome_rasters.clear();
        let blank = cpu.render_input(&blank).pixels.clone();
        let want = cpu.render_input(&s.washed());
        let (width, want) = (want.width, want.pixels);
        let step = |a: u32, b: u32| {
            (0..3).all(|c| (a >> (8 * c) & 0xFF).abs_diff(b >> (8 * c) & 0xFF) <= 1)
        };
        let (mut exact, mut edges) = (0, 0);
        for y in s.card.y0..s.card.y1 {
            for x in s.card.x0..s.card.x1 {
                let i = usize::try_from(y).unwrap() * width + usize::try_from(x).unwrap();
                if want[i] == blank[i] {
                    assert_eq!(got[i], want[i], "{washed:?}: ({x}, {y})");
                    exact += 1;
                } else {
                    assert!(step(got[i], want[i]), "{washed:?}: a glyph at ({x}, {y})");
                    edges += 1;
                }
            }
        }
        assert!(
            exact > 20 * edges && edges > 0,
            "{exact} exact, {edges} glyph pixels"
        );
    }
}

/// A RELEASED WASH FADES IN STEPS AND THEN ARMS NOTHING: a wash that moves off a row lets that
/// row's go at four fifths of its tone, a step fainter every quarter of [`WASH_FADE`] and never
/// none while it stands, each step due once at exactly the instant the wake named; at
/// [`WASH_FADE`] it is gone and no wake stands. A wash that first lights, one that returns to
/// the row it left, and rows that moved under a fade leave nothing fading. Negative control:
/// where motion is reduced the old wash goes in the frame that lights the next row.
#[test]
fn a_released_wash_fades_in_steps_then_arms_nothing() {
    let t0 = std::time::Instant::now();
    let mut wash = WashState::default();
    wash.settle(Some((1, None)), 7, t0, true);
    assert_eq!(
        (wash.release, wash.wake()),
        (None, None),
        "a first wash fades nothing"
    );
    wash.settle(Some((2, None)), 7, t0, true);
    let release = wash.release.expect("the row left fades");
    assert_eq!(release.washed, (1, None));
    let step = WASH_FADE / WASH_STEPS;
    let mut alphas = vec![release.alpha(WASH_ALPHA)];
    assert!(alphas[0] < WASH_ALPHA, "fainter than the new row's at once");
    assert!(!wash.due(t0), "the frame that let it go drew step 0");
    for k in 1..WASH_STEPS {
        let at = t0 + step * k;
        assert_eq!(
            wash.wake(),
            Some(at),
            "the wake is the instant the step changes"
        );
        assert!(!wash.due(at - Duration::from_nanos(1)), "not before it");
        assert!(wash.due(at), "step {k} is owed a frame");
        assert!(!wash.due(at), "once");
        // Settling the same wash again, as each frame does, keeps the fade.
        wash.settle(Some((2, None)), 7, at, true);
        alphas.push(wash.release.expect("fading").alpha(WASH_ALPHA));
    }
    assert!(alphas.windows(2).all(|w| w[1] < w[0]), "{alphas:?}");
    assert!(
        alphas.iter().all(|a| *a > 0),
        "{alphas:?}: never none while it stands"
    );
    assert_eq!(wash.wake(), Some(t0 + WASH_FADE));
    assert!(wash.due(t0 + WASH_FADE), "the frame that clears it");
    assert_eq!(
        (wash.release, wash.wake()),
        (None, None),
        "then nothing is armed"
    );
    assert!(!wash.due(t0 + WASH_FADE * 2));

    // Back onto the row that is fading: it is lit, not fading.
    wash.settle(Some((1, None)), 7, t0, true);
    assert_eq!(wash.release.map(|r| r.washed), Some((2, None)));
    wash.settle(Some((2, None)), 7, t0, true);
    assert_eq!(wash.release.map(|r| r.washed), Some((1, None)));
    wash.settle(Some((1, None)), 7, t0, true);
    assert_eq!(wash.release.map(|r| r.washed), Some((2, None)));
    // Rows that moved end a fade: its row index names another row now.
    wash.settle(Some((1, None)), 8, t0, true);
    assert_eq!(wash.release, None);

    // CONTROL: reduced motion swaps in one frame.
    let mut still = WashState::default();
    still.settle(Some((1, None)), 7, t0, false);
    still.settle(Some((2, None)), 7, t0, false);
    assert_eq!((still.release, still.wake()), (None, None));
}

fn layers_of(app: &App, wid: WindowId, id: u64) -> Vec<FxLayer> {
    app.windows[&wid]
        .input_scratch
        .fx_layers
        .iter()
        .filter(|l| l.id == id)
        .cloned()
        .collect()
}

/// THE KEYS AND THE POINTER PAINT ONE WASH, AND A HOVER BUILDS NO ART: on an open card the row
/// the keys rest on and the row the pointer rests on wear the same layer, a pill of the
/// card's wash ink at its one opacity standing on the card's own occluder, and the row's
/// cells keep the card's ground; a frame at rest hands on the same content, a wash that moves
/// to another row builds no art, and a headless window, which fades nothing, lays one pill and
/// arms no wake. A card with nothing lit and a closed card lay no wash. Negative control: the
/// first wash of a width builds its one art.
#[test]
fn the_keys_and_the_pointer_paint_one_wash_and_a_hover_builds_no_art() {
    use aterm_core::render::{FxBlend, FxItem, FxSlot, FxSpace};
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    let id = crate::claude_footer::quota::card_wash_id(session);
    open_card_by_pointer(&mut app, wid, session);
    frame(&mut app, wid);
    assert!(layers_of(&app, wid, id).is_empty(), "nothing lit: no wash");
    let _ = aterm_render::test_counters::take_art_builds();
    assert!(app.autopilot_menu_nav(wid, MenuKey::Down));
    frame(&mut app, wid);
    let by_key = layers_of(&app, wid, id);
    assert_eq!(by_key.len(), 1, "one wash layer");
    assert_eq!(
        aterm_render::test_counters::take_art_builds(),
        1,
        "CONTROL: the first wash of a width builds its cap art"
    );
    let layer = &by_key[0];
    assert_eq!(
        (layer.slot, layer.space, layer.blend),
        (FxSlot::Under, FxSpace::Grid, FxBlend::Exact)
    );
    let menu = app.autopilot_menu.as_ref().expect("open");
    let (rect, lit) = (menu.rect.expect("painted"), menu.highlight.expect("lit"));
    let scratch = &app.windows[&wid].input_scratch;
    assert!(
        scratch
            .fx_occluders
            .contains(&layer.chrome.expect("on the card")),
        "the wash stands on the card's own occluder"
    );
    let plate = layers_of(
        &app,
        wid,
        crate::claude_footer::quota::card_plate_id(session),
    );
    let order = |id: u64| scratch.fx_layers.iter().position(|l| l.id == id);
    assert!(order(plate[0].id) < order(id), "over the frame's layer");
    let inks = CardInks::of(app.overlay_card_theme(wid), app.card_palette(session));
    let visible = menu.viewport.range(menu.rows.len(), rect.h - 2);
    let line = &scratch.cells[rect.row + 1 + lit - visible.start][rect.col..rect.col + rect.w];
    assert!(
        line.iter().all(|cell| cell.bg == inks.ground),
        "cells keep the ground"
    );
    assert!(
        line.iter()
            .any(|cell| cell.ch != ' ' && cell.fg == inks.sel_fg)
    );
    assert!(line.iter().all(|cell| !cell.bold), "and their weight");
    assert!(
        layer
            .content
            .items()
            .iter()
            .any(|item| matches!(item, FxItem::Sprite(_))),
        "a rounded bar"
    );
    let next = step(&menu.rows, Some(lit), true).expect("another choice");
    assert_eq!(
        app.card_wash_wake(),
        None,
        "a headless window fades nothing"
    );

    frame(&mut app, wid);
    let again = layers_of(&app, wid, id);
    assert!(
        again.len() == 1 && std::sync::Arc::ptr_eq(&again[0].content, &layer.content),
        "a frame at rest seals no new content"
    );

    // The pointer on the next choice: the same pill, a row down.
    assert_ne!(next, lit);
    let ((x, y), _) = row_px(&app, wid, next);
    app.on_cursor_moved(wid, x, y);
    assert_eq!(app.autopilot_menu.as_ref().unwrap().highlight, Some(next));
    frame(&mut app, wid);
    let by_pointer = layers_of(&app, wid, id);
    assert_eq!(
        by_pointer.len(),
        1,
        "one pill: the row left is not fading here"
    );
    assert_eq!(
        by_pointer[0].content.items().len(),
        layer.content.items().len(),
        "the same bar"
    );
    assert_eq!(
        by_pointer[0].content.palette(),
        layer.content.palette(),
        "the same tone"
    );
    let top = |layer: &FxLayer| {
        layer
            .content
            .items()
            .iter()
            .map(|item| match item {
                FxItem::Sprite(sprite) => sprite.y,
                FxItem::Rect(rect) => rect.y,
                _ => i32::MAX,
            })
            .min()
    };
    let ch = i32::try_from(app.win_cell_size(wid).1).unwrap();
    assert_eq!(
        top(&by_pointer[0]),
        top(layer).map(|y| y + ch * i32::try_from(next - lit).unwrap()),
        "on the row the pointer is on"
    );
    assert_eq!(
        aterm_render::test_counters::take_art_builds(),
        0,
        "a hover builds no art"
    );
    assert!(app.close_autopilot_menu());
    frame(&mut app, wid);
    assert!(
        layers_of(&app, wid, id).is_empty(),
        "no wash once the card is gone"
    );
}

/// THE LOOP WAKES FOR EACH STEP OF A FADE AND ASKS ONE FRAME A STEP: with a wash let go on the
/// open card, the loop's wake is the fade's next step, the card's window is owed a frame
/// exactly when a step is reached, the repaint key moves with each step, and each frame lays
/// two pills, the row left's the fainter; once the fade has run the window is owed its last
/// frame, one pill stands and no wake is armed.
#[test]
fn the_loop_wakes_for_each_step_of_a_fade_and_asks_one_frame_a_step() {
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    let id = crate::claude_footer::quota::card_wash_id(session);
    open_card(&mut app);
    frame(&mut app, wid);
    let lit = app.autopilot_menu.as_ref().unwrap().highlight.expect("lit");
    assert!(app.autopilot_menu_nav(wid, MenuKey::Down));
    let menu = app.autopilot_menu.as_mut().unwrap();
    assert_ne!(menu.highlight, Some(lit));
    // What a focused window on glass does at the next frame: the row left is let go.
    let t0 = std::time::Instant::now();
    let (washed, generation) = (menu.washed(), menu.generation);
    menu.wash.settle(washed, generation, t0, true);
    assert_eq!(menu.wash.release.map(|r| r.washed), Some((lit, None)));
    let fp = |app: &App| fingerprint(app.autopilot_menu.as_ref(), wid);
    let mut seen = vec![fp(&app)];
    let pills = |app: &App| {
        let layers = layers_of(app, wid, id);
        assert_eq!(layers.len(), 1);
        layers[0].content.palette().len() / aterm_render::rounded_bar::BAR_INKS
    };
    frame(&mut app, wid);
    assert_eq!(pills(&app), 2, "the row left and the row lit");
    let quarter = WASH_FADE / WASH_STEPS;
    for k in 1..WASH_STEPS {
        assert_eq!(app.card_wash_wake(), Some(t0 + quarter * k));
        assert_eq!(app.card_wash_due(t0 + quarter * k), Some(wid), "step {k}");
        assert_eq!(
            app.card_wash_due(t0 + quarter * k),
            None,
            "one frame a step"
        );
        seen.push(fp(&app));
        frame(&mut app, wid);
        assert_eq!(pills(&app), 2);
    }
    assert_eq!(
        app.card_wash_due(t0 + WASH_FADE),
        Some(wid),
        "the frame that clears it"
    );
    seen.push(fp(&app));
    assert!(
        seen.iter().collect::<std::collections::BTreeSet<_>>().len() == seen.len(),
        "the repaint key moves with every step: {seen:?}"
    );
    frame(&mut app, wid);
    assert_eq!(pills(&app), 1, "one pill stands");
    assert_eq!(app.card_wash_wake(), None, "and nothing is armed");
    assert_eq!(app.card_wash_due(t0 + WASH_FADE * 3), None);
}

/// A FADE WHOSE STEP FRAMES NEVER CAME STILL GETS ITS LAST FRAME: the frame that let a row's
/// wash go took its repaint key before its compose made the release, so the key it stamped
/// already reads no fade. Where no step frame follows before the fade has run (a busy main
/// thread serves the wake late), the frame the fade's end asks for must still differ from that
/// stamped key, or it is proved unchanged and the row left keeps its wash (the defect: it stood
/// at 80% until something else repainted).
#[test]
fn a_fade_that_ends_unseen_still_repaints() {
    let (mut app, wid, _session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    open_card(&mut app);
    frame(&mut app, wid);
    assert!(app.autopilot_menu_nav(wid, MenuKey::Down));
    let fp = |app: &App| fingerprint(app.autopilot_menu.as_ref(), wid);
    // The frame that lets the row go: its key first, then the compose's settle.
    let stamped = fp(&app);
    let t0 = std::time::Instant::now();
    let menu = app.autopilot_menu.as_mut().unwrap();
    let (washed, generation) = (menu.washed(), menu.generation);
    menu.wash.settle(washed, generation, t0, true);
    assert!(
        menu.wash.release.is_some(),
        "precondition: the row left fades"
    );
    assert_eq!(
        app.card_wash_due(t0 + WASH_FADE),
        Some(wid),
        "the fade's end asks its frame"
    );
    assert!(app.autopilot_menu.as_ref().unwrap().wash.release.is_none());
    assert_ne!(
        fp(&app),
        stamped,
        "the frame the fade's end asks for is not proved unchanged"
    );
}

/// `pointer move item=<n>` AIMS AT THE MIDDLE OF A FOOTER CARD'S ROW: over an open card the
/// verb's pointer lands in the frame row of the row `controls fact-menu` counts `index=<n>`,
/// at the middle of the card's width, and lights that row as a person's pointer does. A row
/// that is no choice, and one past the card's rows, is no aim and moves nothing. Negative
/// control: with no card open the verb names what it found.
#[test]
fn pointer_move_item_aims_at_the_middle_of_a_footer_cards_row() {
    use crate::app_mouse::PointerAction;
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    assert_eq!(
        app.pointer_cmd(PointerAction::MoveItem(1)),
        Err("no palette, Overview or footer card is open".to_string()),
        "CONTROL: no card"
    );
    open_card_by_pointer(&mut app, wid, session);
    frame(&mut app, wid);
    let menu = app.autopilot_menu.as_ref().expect("open");
    let rect = menu.rect.expect("painted");
    let choices: Vec<usize> = (0..menu.rows.len())
        .filter(|i| menu.rows[*i].selectable())
        .collect();
    let dead = (0..menu.rows.len())
        .find(|i| !menu.rows[*i].selectable())
        .expect("the card's head");
    let total = menu.rows.len();
    assert!(choices.len() >= 2, "{choices:?}");
    for &item in choices.iter().rev() {
        app.pointer_cmd(PointerAction::MoveItem(item))
            .unwrap_or_else(|why| panic!("row {item}: {why}"));
        let (x, y) = app.windows[&wid].last_cursor_px;
        assert_eq!(
            app.frame_cell_at(wid, x, y),
            Some((rect.row + 1 + item, rect.col + rect.w / 2)),
            "the middle of row {item}"
        );
        let menu = app.autopilot_menu.as_ref().expect("still open");
        assert_eq!(menu.highlight, Some(item), "the pointer lights it");
        let line = menu
            .controls_lines("s")
            .into_iter()
            .find(|line| line.contains(&format!(" index={item} ")))
            .expect("the row's line");
        assert!(line.contains(" selected=true "), "{line}");
    }
    let lit = app.autopilot_menu.as_ref().unwrap().highlight;
    for off in [dead, total] {
        assert_eq!(
            app.pointer_cmd(PointerAction::MoveItem(off)),
            Err(format!("item {off} is not a row the card shows"))
        );
    }
    assert_eq!(app.autopilot_menu.as_ref().unwrap().highlight, lit);
}

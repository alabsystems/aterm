// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Every letter a cursor trail lies on reads at the bar: the trail gallery's scenes replayed through
// the shipping effects pipeline (`EffectsPipeline::apply`, which runs a window's cursor family) and
// the real renderer, every 16 ms frame, the frame's glyph ink read off its input as the GUI reads
// it, each lettered cell judged as `aterm-lab trail-contrast` judges a gallery frame
// (`local_contrast`): what a reader sees round each stroke. Each solid ink pixel of the clean
// glyph (at least `SOLID_SHARE` of its most contrasting pixel's ratio against the cell's ground)
// is read in the frame against its ring, each channel's median of the frame's pixels on the
// nearest Chebyshev ring (1 px out, else 2) that are the cell's ground in the clean frame; a pixel
// row reads the lower median of its pixels' ratios, and the letter its worst row. It is the one
// model the letter guard keeps (`letter_guard::ring` in aterm-effects, read from the glyph's own
// raster): a light the cell's median ground would count, laid away from the strokes, or a
// companion's coat over part of the cell, is not what a reader reads the letter against.

use crate::wcag::{contrast, luma};
use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Rgb, Terminal};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::kitty_pet::{PetInputKind, art_cols, head_law};
use aterm_effects::pipeline::EffectsPipeline;
use aterm_render::{Renderer, Theme};

pub const ROWS: u16 = 14;
pub const COLS: u16 = 72;
const FRAME_MS: u64 = 16;
pub const BAR: f64 = 4.5;
/// The share of its own reading a letter keeps where that reading is under [`BAR`] on the bare
/// page (the letter guard's `OWN_SHARE`): a dim theme's letters, and in [`Scene::Context`] the
/// theme's own powerline segments and selected row.
pub const OWN_SHARE: f64 = 0.97;
const SOLID_SHARE: f64 = 0.85;
const BLANK_BELOW: f64 = 1.5;
const RING_REACH: i64 = 2;

/// A theme as the gallery photographs it: text, page and caret.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub name: &'static str,
    pub fg: u32,
    pub bg: u32,
    pub cursor: u32,
}

pub const DARK: Look = Look {
    name: "dark",
    fg: 0x00D0_D0D0,
    bg: 0x0011_1318,
    cursor: 0x0050_FA7B,
};
pub const GITHUB_LIGHT: Look = Look {
    name: "light",
    fg: 0x001F_2328,
    bg: 0x00FF_FFFF,
    cursor: 0x0009_69DA,
};
pub const DEFAULT_LIGHT: Look = Look {
    name: "default-light",
    fg: 0x002B_2D33,
    bg: 0x00F6_F7F9,
    cursor: 0x001A_7F37,
};
/// Text that clears the bar with little room, whose solid ink reads under
/// it on the bare page: a 5.0:1 grey on the dark page, Solarized Dark's
/// text (4.75:1) and a 4.95:1 grey on white.
pub const DIM_GREY: Look = Look {
    name: "dim-grey",
    fg: 0x0085_8585,
    bg: 0x0011_1318,
    cursor: 0x0050_FA7B,
};
pub const SOLARIZED_DARK: Look = Look {
    name: "solarized-dark",
    fg: 0x0083_9496,
    bg: 0x0000_2B36,
    cursor: 0x0093_A1A1,
};
pub const PAPER_GREY: Look = Look {
    name: "paper-grey",
    fg: 0x0070_7070,
    bg: 0x00FF_FFFF,
    cursor: 0x0009_69DA,
};

/// The most pixels a ring reads: the Chebyshev ring [`RING_REACH`] px out.
const RING_MOST: usize = 8 * RING_REACH as usize;

/// Each channel's median (the upper one of an even count) of a ring's pixels: `at` gives the
/// pixels, at most [`RING_MOST`] of them. A ring of one colour is that colour.
fn median(n: usize, at: impl Fn(usize) -> u32) -> u32 {
    let first = at(0) & 0x00FF_FFFF;
    let mut chan = [[0u8; RING_MOST]; 3];
    let mut uniform = true;
    let [red, green, blue] = &mut chan;
    for (i, ((r, g), b)) in red
        .iter_mut()
        .zip(green.iter_mut())
        .zip(blue.iter_mut())
        .take(n)
        .enumerate()
    {
        let p = at(i);
        uniform &= p & 0x00FF_FFFF == first;
        *r = (p >> 16) as u8;
        *g = (p >> 8) as u8;
        *b = p as u8;
    }
    if uniform {
        return first;
    }
    let mut out = 0;
    for (c, sh) in chan.iter_mut().zip([16u32, 8, 0]) {
        out |= u32::from(*c[..n].select_nth_unstable(n / 2).1) << sh;
    }
    out
}

/// The commonest colour, the lowest of a tie; a cell holds a few dozen colours, so each is
/// counted in a short list rather than sorting every pixel.
fn mode(px: &[u32]) -> u32 {
    let mut seen: Vec<(u32, usize)> = Vec::new();
    for p in px {
        let p = p & 0x00FF_FFFF;
        match seen.iter_mut().find(|(c, _)| *c == p) {
            Some((_, n)) => *n += 1,
            None => seen.push((p, 1)),
        }
    }
    let (mut best, mut best_n) = (0, 0);
    for (c, n) in seen {
        if n > best_n || (n == best_n && c < best) {
            (best, best_n) = (c, n);
        }
    }
    best
}

/// One judged letter.
#[derive(Clone, Copy, Debug)]
pub struct Letter {
    pub row: usize,
    pub col: usize,
    pub ch: char,
    /// The letter's reading: its worst pixel row's lower median of each solid ink pixel against
    /// its ring of ground (`local_contrast`).
    pub local: f64,
    /// Its clean twin judged the same way, its own reading, when the replay reads it
    /// ([`Replay::own`]); infinite otherwise.
    pub own: f64,
}

impl Letter {
    /// What the letter is judged at: its ring's reading ([`Letter::local`]).
    pub fn judged(&self) -> f64 {
        self.local
    }
}

/// A frame and its clean twin, `w` px wide, cells `cw`x`ch` from the frame's origin.
pub struct Pair<'a> {
    pub frame: &'a [u32],
    pub clean: &'a [u32],
    pub w: usize,
    pub cw: usize,
    pub ch: usize,
}

impl Pair<'_> {
    fn at(px: &[u32], w: usize, x: usize, y: usize) -> u32 {
        px[y * w + x] & 0x00FF_FFFF
    }

    /// The cell's box, px: `(x0, y0, x1, y1)`.
    fn cell_box(&self, row: usize, col: usize, span: usize) -> (usize, usize, usize, usize) {
        let h = self.clean.len() / self.w;
        let (x0, y0) = (col * self.cw, row * self.ch);
        (
            x0,
            y0,
            (x0 + span * self.cw).min(self.w),
            (y0 + self.ch).min(h),
        )
    }

    /// Whether the effects left the cell `(row, col)` as its twin.
    fn untouched(&self, row: usize, col: usize, span: usize) -> bool {
        let (x0, y0, x1, y1) = self.cell_box(row, col, span);
        (y0..y1).all(|y| {
            let (from, to) = (y * self.w + x0, y * self.w + x1);
            self.frame[from..to]
                .iter()
                .zip(&self.clean[from..to])
                .all(|(f, c)| (f ^ c) & 0x00FF_FFFF == 0)
        })
    }

    /// The cell `(row, col)` as `aterm-lab trail-contrast` measures it, read by its plan
    /// ([`Self::plan`]); `None` for a blank cell or one the effects left as its twin.
    pub fn letter(&self, row: usize, col: usize, span: usize, ch: char) -> Option<Letter> {
        if self.untouched(row, col, span) {
            return None;
        }
        let plan = self.plan(row, col, span)?;
        Some(Letter {
            row,
            col,
            ch,
            local: plan.reading(self.frame, &mut Vec::new()),
            own: f64::INFINITY,
        })
    }

    /// The cell `(row, col)`'s own reading: its plan read on the clean twin; `None` for a blank
    /// cell.
    pub fn own(&self, row: usize, col: usize, span: usize) -> Option<f64> {
        let plan = self.plan(row, col, span)?;
        Some(plan.own(self.clean, &mut Vec::new()))
    }

    /// The cell `(row, col)` as `aterm-lab trail-contrast` measures it, all it reads of the clean
    /// frame; `None` for a blank cell.
    fn plan(&self, row: usize, col: usize, span: usize) -> Option<LetterPlan> {
        let h = self.clean.len() / self.w;
        let (x0, y0, x1, y1) = self.cell_box(row, col, span);
        let mut clean_px = Vec::with_capacity((x1 - x0) * (y1 - y0));
        for y in y0..y1 {
            clean_px.extend(
                self.clean[y * self.w + x0..y * self.w + x1]
                    .iter()
                    .map(|c| c & 0x00FF_FFFF),
            );
        }
        if clean_px.is_empty() {
            return None;
        }
        let ground = mode(&clean_px);
        let ratios: Vec<f64> = clean_px.iter().map(|c| contrast(*c, ground)).collect();
        let extreme = ratios.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if extreme < BLANK_BELOW {
            return None;
        }
        let solid_at = SOLID_SHARE * extreme;
        let mut plan = LetterPlan {
            solid: Vec::new(),
            rings: Vec::new(),
            own: std::cell::Cell::new(None),
        };
        for y in y0..y1 {
            for x in x0..x1 {
                if ratios[(y - y0) * (x1 - x0) + x - x0] < solid_at {
                    continue;
                }
                let from = plan.rings.len();
                for d in 1..=RING_REACH {
                    for dy in -d..=d {
                        for dx in -d..=d {
                            if dx.abs().max(dy.abs()) != d {
                                continue;
                            }
                            let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                            if xx < 0 || yy < 0 || xx >= self.w as i64 || yy >= h as i64 {
                                continue;
                            }
                            let (xx, yy) = (xx as usize, yy as usize);
                            if Self::at(self.clean, self.w, xx, yy) == ground {
                                plan.rings.push((yy * self.w + xx) as u32);
                            }
                        }
                    }
                    if plan.rings.len() > from {
                        plan.solid.push((
                            (y * self.w + x) as u32,
                            y as u32,
                            plan.rings.len() as u32,
                        ));
                        break;
                    }
                }
            }
        }
        Some(plan)
    }
}

/// What the judge reads a lettered cell by: each solid ink pixel of the clean frame (at least
/// [`SOLID_SHARE`] of its most contrasting pixel's ratio against the cell's ground) that has ground
/// on its nearest ring, and those ring pixels. All of it is the clean frame's, so one plan reads
/// every frame drawn over that clean frame ([`CleanFrame::plans`]).
struct LetterPlan {
    /// The pixels, row-major: each one's frame index, its pixel row, and where its ring ends in
    /// [`Self::rings`] (it starts where the one before ends).
    solid: Vec<(u32, u32, u32)>,
    rings: Vec<u32>,
    /// [`Letter::own`], its reading on the clean frame, once a replay has read it.
    own: std::cell::Cell<Option<f64>>,
}

impl LetterPlan {
    /// The letter's reading on frame `px`: its worst pixel row's lower median of each solid ink
    /// pixel against its ring of ground (infinite with no such pixel).
    fn reading(&self, px: &[u32], ratios: &mut Vec<f64>) -> f64 {
        let mut worst = f64::INFINITY;
        // A stroke's pixels are mostly one colour over one ring: the last pair's ratio stands.
        let mut last = (u32::MAX, u32::MAX, 0.0);
        let (mut i, mut from) = (0, 0);
        while i < self.solid.len() {
            let row = self.solid[i].1;
            ratios.clear();
            while let Some(&(at, _, to)) = self.solid.get(i).filter(|s| s.1 == row) {
                let ring = &self.rings[from..to as usize];
                from = to as usize;
                let g = median(ring.len(), |k| px[ring[k] as usize]);
                let p = px[at as usize] & 0x00FF_FFFF;
                if (p, g) != (last.0, last.1) {
                    last = (p, g, contrast(p, g));
                }
                ratios.push(last.2);
                i += 1;
            }
            let mid = (ratios.len() - 1) / 2;
            worst = worst.min(*ratios.select_nth_unstable_by(mid, f64::total_cmp).1);
        }
        worst
    }

    /// The letter's own reading: this plan on its `clean` frame.
    fn own(&self, clean: &[u32], ratios: &mut Vec<f64>) -> f64 {
        if let Some(own) = self.own.get() {
            return own;
        }
        let own = self.reading(clean, ratios);
        self.own.set(Some(own));
        own
    }
}

/// A frame with no effect drawn and what it was drawn from: the terminal's
/// `screen_mutations` and caret, its pixels, and the plan of each cell its letters have been
/// read in, by `(row, col, span)` ([`LetterPlan`], `None` for a blank cell). The plans read
/// these pixels alone, so they last exactly as long as they do. NEGATIVE CONTROL (2026-11-05):
/// plans carried to the next clean frame fail `every_letter_a_trail_lies_on_reads_at_the_bar` and
/// `a_dim_letter_keeps_its_own_reading`.
struct CleanFrame {
    at: (u64, (usize, usize)),
    px: Vec<u32>,
    plans: std::collections::HashMap<(usize, usize, usize), Option<LetterPlan>>,
}

/// The lettered cells a frame's `blur` can lay light on or round, where `m` keeps it off every one:
/// `None` where one stands outside the mask. What it reads is what the blur's composite reads
/// (`bloom::apply_kept_off`): the blur paints no pixel past the source's bounds grown by its reach
/// ([`bloom::reach_px`]), and none within a kept cell's box grown by [`bloom::LetterMask::grow`],
/// which holds the [`RING_REACH`] the judge reads a letter's ground in. So where this is `Some`, the
/// blur changes no pixel of a judged letter ([`Pair::plan`]). A cell the blur can reach and the
/// mask's window leaves out reads as not kept: a window shrunk under the reach is `None` here.
fn spared_letters_in_reach(
    m: &aterm_render::bloom::LetterMask,
    blur: &[aterm_render::GlowQuad],
    input: &RenderInput,
) -> Option<usize> {
    use aterm_render::bloom;
    let ring = RING_REACH as f32;
    if m.origin != [0.0, 0.0] || m.grow[0] < ring || m.grow[1] < ring {
        return None;
    }
    let (Some([x0, y0, x1, y1]), _) = bloom::source_quads_into(blur, input.rows, &mut Vec::new())
    else {
        return Some(0);
    };
    let d = i64::from(bloom::reach_px(bloom::BLOOM_RADIUS));
    let (cw, ch) = (m.cell[0] as i64, m.cell[1] as i64);
    let reached = |col: i64, row: i64| {
        col * cw - RING_REACH < i64::from(x1) + d
            && (col + 1) * cw + RING_REACH > i64::from(x0) - d
            && row * ch - RING_REACH < i64::from(y1) + d
            && (row + 1) * ch + RING_REACH > i64::from(y0) - d
    };
    let mut in_reach = 0;
    for (row, line) in input.cells.iter().enumerate() {
        for (col, c) in line.iter().enumerate().take(input.cols) {
            if !c.ch.is_alphanumeric() {
                continue;
            }
            for col in col..col + if c.wide { 2 } else { 1 } {
                if reached(col as i64, row as i64) {
                    if !m.kept(col as i32, row as i32) {
                        return None;
                    }
                    in_reach += 1;
                }
            }
        }
    }
    Some(in_reach)
}

/// What a replay does at an instant.
#[derive(Clone, Debug)]
pub enum Ev {
    /// A key: the host notes it, and its echo lands 2 ms later.
    Key(char, Vec<u8>),
    /// Output lands.
    Out(Vec<u8>),
}

/// One judged frame.
#[derive(Clone, Debug)]
pub struct Judged {
    pub ms: u64,
    letters: Option<Vec<Letter>>,
    /// The page's pixels the frame draws under [`SHADE_SHARE`] of the page's
    /// luminance, when the replay reads them ([`Replay::shade`]).
    pub shade: usize,
    /// The blank cells' pixels of page the frame lights past [`AIR_LIFT`] of the page's
    /// luminance, when the replay reads them ([`Replay::air`]).
    pub air: usize,
    /// The light below the caret's row, column by column from the window's left edge to
    /// the caret's, or to where a companion's drawn box meets the band (the underline cat
    /// stands on its rail's head, its body over it): each column's greatest channel lift
    /// over the clean frame in the [`RAIL_BAND`] px under the row's bottom, when the replay
    /// reads it ([`Replay::rail`]).
    pub rail: Vec<u8>,
    /// The letter under the caret, judged against its clean twin (the theme's own caret
    /// over it), its [`Letter::own`] that twin's reading, when the replay reads it
    /// ([`Replay::caret`]) and the caret's cell holds one the effects changed.
    pub caret: Option<Letter>,
    /// Where the companion's body lay this frame, when the replay reads it ([`Replay::coat`]).
    pub coat: Coat,
    /// The cells the cursor trail fills this frame (`RenderInput::cursor_trail`).
    pub trail: usize,
}

/// Where a frame's companion lay ([`Replay::coat`]): its drawn body (the knock layer's rects
/// that meet the pet's box), against the letters' ink and the word icons.
#[derive(Clone, Debug, Default)]
pub struct Coat {
    /// The pet's drawn box, frame px `(x0, x1, y0, y1)`.
    pub pet: Option<(i32, i32, i32, i32)>,
    /// The alpha the frame put the pet on glass at (`0`: none drawn).
    pub alpha: u8,
    /// How far (rows) the head law draws the body down its row and the share of its height it
    /// fits it to (`EffectsPipeline::companion_head_sink_fit`), when it is drawn.
    pub sink_fit: Option<(f32, f32)>,
    /// The letters' ink pixels (a clean frame's pixels off the page in a cell that holds a
    /// glyph) the body covers, and the cells they lie in `(row, col, px)`.
    pub ink_px: usize,
    pub cells: Vec<(usize, usize, usize)>,
    /// The cells holding a glyph that the pet's drawn box meets by a pixel or more, `(row,
    /// col)`: the box read as `aterm-lab trail-companion` reads a gallery frame's (its
    /// `on_letters`), whatever of the box the body's art fills.
    pub box_cells: Vec<(usize, usize)>,
    /// How far across into those cells the box reaches, px: of each, its overlap across the
    /// cell's width, and of them the most (`0`: it meets none). Read as the effects replay reads
    /// it (`steam_tests::reach_of`): a box a pixel down into a lettered row reaches across every
    /// cell of it under its width.
    pub box_reach: usize,
    /// The caret cell's pixels the body covers: its runs on the knock layer, which a body drawn
    /// under the letters has and one drawn over blank ground does not (`box_on_caret` reads that
    /// one).
    pub caret_px: usize,
    /// Whether the pet's drawn box lies on the caret's cell as the companion's brain judges a
    /// box ([`aterm_effects::kitty_pet::lies_on_the_caret`]: past the box's first cell, short of
    /// its transparent end), over the letters or under them alike.
    pub box_on_caret: bool,
    /// The word icons on glass, frame px, and the pixels of the pet's box that meet them.
    pub icons: Vec<(i32, i32, i32, i32)>,
    pub icon_px: usize,
    /// The page the frame drew, for a reader that seats a body on it (`seat_replay`): each
    /// row's cells, lettered (a glyph that is no white space, and a wide glyph's second cell)
    /// or not, across every column of the pane.
    pub lettered: Vec<Vec<bool>>,
    /// The caret's cell on that page, `(row, col)`.
    pub caret_cell: (usize, usize),
    /// The pet's drawn box and the word icons on the pane's grid, in cells from the grid's
    /// first: `(col0, col1, row0, row1)`.
    pub pet_cells: Option<(f32, f32, f32, f32)>,
    pub icon_cells: Vec<(f32, f32, f32, f32)>,
}

/// The px band under a row's bottom a rail laid clear of the letter guard's ring lies in
/// ([`Judged::rail`]): from the ring's edge two pixels down.
pub const RAIL_BAND: std::ops::Range<usize> = 2..6;

/// A blank cell's pixel of page drawn past this multiple of the page's luminance shows light.
pub const AIR_LIFT: f64 = 1.5;

/// A pixel of page drawn under this share of the page's luminance is a shade.
pub const SHADE_SHARE: f64 = 0.6;

impl Judged {
    /// The frame's judged letters. A replay that did not judge them ([`Replay::letters`]) panics
    /// here rather than read as a frame with nothing under its bar.
    pub fn letters(&self) -> &[Letter] {
        self.letters
            .as_deref()
            .expect("fixture: this replay does not judge its letters (`Replay::letters`)")
    }

    pub fn worst(&self) -> Option<Letter> {
        self.letters()
            .iter()
            .copied()
            .min_by(|a, b| a.judged().total_cmp(&b.judged()))
    }
}

pub struct Replay {
    pub term: Terminal,
    pub fx: EffectsPipeline,
    pub rend: Renderer,
    pub cell: (usize, usize),
    pub look: Look,
    pub ms: u64,
    clean: Option<CleanFrame>,
    /// Frames to keep as PNGs: `(ms, path)`.
    pub dump: Vec<(u64, String)>,
    /// The laws the companion's negative control reverts, set before each frame: the alternate
    /// screen's companion is its own, born as the program enters it.
    pub old_paths: Vec<aterm_effects::kitty_pet::OldPath>,
    /// Whether each frame judges its letters ([`Judged::letters`]), half of what a frame costs
    /// a replay that reads something else of it.
    pub letters: bool,
    /// Whether each judged letter reads its own reading too ([`Letter::own`]).
    pub own: bool,
    /// The host's bloom: `Some(unheld)` blooms each frame as the GPU does
    /// (`aterm_render::bloom`), over text that holds it too when `unheld`
    /// (a negative control); `None` draws none, as a CPU frame.
    pub bloom: Option<bool>,
    /// Whether each frame counts the page's pixels it shades ([`Judged::shade`]).
    pub shade: bool,
    /// Whether each frame counts the blank cells' pixels it lights ([`Judged::air`]).
    pub air: bool,
    /// Whether each frame reads the light below the caret's row ([`Judged::rail`]).
    pub rail: bool,
    /// Whether each frame reads the letter under the caret ([`Judged::caret`]).
    pub caret: bool,
    /// Whether each frame takes the host's post passes as the GUI runs them over the
    /// over-text stream (`post_process`): the bloom unless the frame holds it, at the page's
    /// share of its strength, then the SDR crown at the default boost.
    pub post: bool,
    /// Whether each frame reads where the companion's body lies ([`Judged::coat`]).
    pub coat: bool,
    /// Whether a frame whose bloom keeps off its letters by a mask leaves the blur out: the
    /// mask's zero covers every lettered cell the blur can reach and the ring the judge reads
    /// round it ([`spared_letters_in_reach`], asserted on each such frame), so no judged letter
    /// reads otherwise. Only for a replay that reads nothing but its letters
    /// ([`Replay::present`] holds it to that). The real blur stays with the frames that have no
    /// mask or an open gate, and with the replays that keep it: that the composite honours the
    /// mask is held by [`the_post_passes_spare_the_letters`] and `aterm_render::bloom`'s
    /// `a_spared_bloom_*`.
    pub skip_spared_blur: bool,
    /// The lettered cells within reach of the blurs [`Replay::skip_spared_blur`] left out.
    pub spared: usize,
    /// Whether the keys reach the terminal without passing the console, as the trail
    /// gallery's `aterm ctl key` types them: their echo lands and the host notes nothing.
    pub keys_unnoted: bool,
    /// The frames' times, ms, in place of one every [`FRAME_MS`]: a headless instance
    /// advances its effects only when a frame renders, and the trail gallery renders one
    /// after each key and on its own steps ([`Scene::gallery_frames`]).
    pub frames_at: Option<Vec<u64>>,
    /// The pane's rows: [`ROWS`], or a scene's own ([`Scene::rows`]).
    pub rows: u16,
    /// The pane's columns: [`COLS`], or a scene's own ([`Scene::cols`]).
    pub cols: u16,
    /// Frames to keep in [`Replay::kept`] as the glass shows them: their times, ms.
    pub keep: Vec<u64>,
    /// The frames [`Replay::keep`] names, in time order.
    pub kept: Vec<Kept>,
    /// Where the frames [`Replay::keep`] names go instead of [`Replay::kept`], each as it is
    /// drawn: a replay that reads every frame of a scene against another's holds two at a time.
    pub keep_to: Option<std::sync::mpsc::SyncSender<Kept>>,
}

/// A frame a replay kept ([`Replay::keep`]): its time, its pixels (`0x00RRGGBB`) as the glass
/// shows them, its width, the grid's top on it, its clean twin's pixels (the same page with no
/// effect drawn), the grid's left, the caret's cell and the cell's size.
pub struct Kept {
    pub ms: u64,
    pub px: Vec<u32>,
    pub w: usize,
    pub top: usize,
    pub clean: Vec<u32>,
    pub left: usize,
    pub caret: (usize, usize),
    pub cell: (usize, usize),
    /// The ground the effects read a bar or underline caret against this frame
    /// (`EffectsPipeline::thin_caret_ground`).
    pub caret_ground: Option<u32>,
    /// What the frame's sidecars would say of it ([`Glass`]).
    pub glass: Glass,
}

/// What a kept frame's sidecars say of it, as the trail gallery keeps them beside a real
/// frame: the screen's rows (`aterm ctl text`), the caret's visibility and style (`cursor`),
/// and of `trail status` the fields `aterm-lab`'s frame readers use. Boxes are frame px
/// `(x0, x1, y0, y1)`, end exclusive.
#[derive(Clone, Debug)]
pub struct Glass {
    pub text: Vec<String>,
    pub caret_visible: bool,
    pub caret_style: aterm_core::terminal::CursorStyle,
    /// The companion's drawn body ([`Coat::pet`]) and the alpha the frame put it on glass at.
    pub pet_body: Option<(i32, i32, i32, i32)>,
    pub pet_alpha: u8,
    /// The pose the frame drew and the brain's action (`pet_pose=`, `pet_action=`).
    pub pet_pose: &'static str,
    pub pet_action: String,
    /// The one seat rule's answer beside the frame (`pet_seat=`); nothing is drawn from it.
    pub pet_seat: String,
    /// The flying head's drawn body (`head_body=`).
    pub head_body: Option<(i32, i32, i32, i32)>,
    /// The word icons on glass (`word_icons=`).
    pub word_icons: Vec<(i32, i32, i32, i32)>,
    /// Rainbow kitty v2's quads, halos, stars and meteors, where it owns the frame.
    pub v2: Option<[u32; 4]>,
}

/// `cursor_glow_sdr_boost`'s default, the SDR crown's ([`Replay::post`]).
const SDR_BOOST: f32 = 0.25;

/// The trail gallery's font size, px: the window's face a replay draws ([`Replay::new`]).
pub const GALLERY_PX: f32 = 22.0;

/// The trail gallery's row, px: macOS's SF Mono at [`GALLERY_PX`] lays a 14 x 26 px cell.
const GALLERY_ROW: usize = 26;

/// The whole px size at which the window's face lays the trail gallery's row ([`GALLERY_ROW`]),
/// the nearest to it and then to [`GALLERY_PX`]: [`GALLERY_PX`] on macOS, 20 px under Linux's
/// bundled JetBrains Mono (12 x 26 px cells; 13 x 29 at [`GALLERY_PX`]). A negative control
/// measured on the gallery's cell, whose light lays more of itself round a taller row's letters
/// or under it, replays there too, its law with it.
pub fn gallery_row_px() -> f32 {
    static PX: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *PX.get_or_init(|| {
        let gallery = GALLERY_PX as u8;
        let px = (16..=28_u8)
            .min_by_key(|&px| {
                let rend = Renderer::from_system(f32::from(px), Theme::default())
                    .expect("no font: build with aterm-render's default embedded-font feature");
                (
                    rend.cell_size().1.abs_diff(GALLERY_ROW),
                    px.abs_diff(gallery),
                )
            })
            .expect("a size");
        f32::from(px)
    })
}

/// Whether the window's face lays the trail gallery's row at [`GALLERY_PX`] ([`gallery_row_px`]
/// is [`GALLERY_PX`]): macOS's SF Mono, on whose 14 x 26 px cell the negative controls were
/// first measured. A control keyed on it replays that measured scene where it holds and the
/// scene measured on another face's cell where it does not, each required where it runs.
pub fn lays_the_gallery_row() -> bool {
    gallery_row_px() == GALLERY_PX
}

/// The cell the window's face lays at [`GALLERY_PX`], px: a [`Replay::new`]'s.
pub fn gallery_cell() -> (usize, usize) {
    static CELL: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
    *CELL.get_or_init(|| {
        Renderer::from_system(GALLERY_PX, Theme::default())
            .expect("no font: build with aterm-render's default embedded-font feature")
            .cell_size()
    })
}

/// [`Scene::LastRow`]'s columns on a `cell` (px): the burst's line (the prompt and
/// [`BURST_TEXT`]) and, past it to the grid's end, the blank cells its companion stands in there,
/// fitted to the row (a letter in every cell over it and no row under it,
/// [`aterm_effects::kitty_pet::head_law`]): the fitted body's width rounded up to a cell. On the
/// gallery's 14 x 26 px cell that body is 2.82 cells across, and the grid is the gallery window's
/// [`COLS`], three cells past the line. Under Linux's bundled JetBrains Mono's 13 x 29 it is 3.38
/// across (44 px, three cells 39): on the gallery's columns its box, at the grid's end, met the
/// line's last letter by 5 to 7 px (6 in 252 of the 378 frames held at the margin), before the
/// wall that holds no seat hid such a body (`OldPath::NoSeatHidesTheBody`; Linux arm64,
/// 2026-11-07), so the grid takes a fourth, and the scene is one whose wall seats the body.
pub fn last_row_cols((cw, ch): (usize, usize)) -> u16 {
    let (_, fit) = head_law(true, true);
    let room = (art_cols(cw as u16, ch as u16) * fit).ceil() as u16;
    last_row_line() + room
}

/// The burst's line on [`Scene::LastRow`]'s last row, cells: the prompt and [`BURST_TEXT`].
pub fn last_row_line() -> u16 {
    ("$ ".len() + BURST_TEXT.len()) as u16
}

/// The cell Linux's bundled JetBrains Mono lays at [`GALLERY_PX`], px ([`gallery_row_px`]).
const BUNDLED_CELL: (usize, usize) = (13, 29);

/// Whether the window's face lays [`BUNDLED_CELL`] at [`GALLERY_PX`] ([`gallery_cell`]): Linux's
/// bundled face, on which a control measured there is required as measured.
pub fn lays_the_bundled_cell() -> bool {
    gallery_cell() == BUNDLED_CELL
}

/// What a replay's `style` word asks for: a `cursor_trail_style`, the pet beside it, and the
/// rainbow's ribbon body.
///
/// The word is `[underline |flat ]<cursor_trail_style>[ + cat| + dog]`. `rainbow` is the
/// shipped theme, the tall body (its band behind the words, its rail under them), and
/// `rainbow + cat` the shipped default beside its pet. `underline` and `flat` name the two
/// ribbon bodies no built-in theme picks ([`EffectsPipeline::set_ribbon_shape`]): the replays
/// keep judging them while the engine keeps them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec<'a> {
    pub trail: &'a str,
    pub pet: Option<aterm_effects::kitty_pet::PetSpecies>,
    pub tall: bool,
    pub flat: bool,
}

impl<'a> Spec<'a> {
    pub fn of(word: &'a str) -> Self {
        let (body, pet) = match word.split_once(" + ") {
            Some((body, pet)) => (
                body,
                Some(
                    aterm_effects::cursor_pet::species(pet)
                        .unwrap_or_else(|| panic!("{word:?}: {pet:?} is no pet")),
                ),
            ),
            None => (word, None),
        };
        let (trail, tall, flat) = if let Some(trail) = body.strip_prefix("underline ") {
            (trail, false, false)
        } else if let Some(trail) = body.strip_prefix("flat ") {
            (trail, true, true)
        } else {
            (body, true, false)
        };
        assert!(
            aterm_effects::cursor_glow::spellings::row(trail).is_some(),
            "{word:?}: {trail:?} is no cursor_trail_style"
        );
        Self {
            trail,
            pet,
            tall,
            flat,
        }
    }
}

/// `fx`'s glow as `word` ([`Spec`]) asks, at the gallery's knobs.
pub fn set_glow(
    fx: &mut EffectsPipeline,
    word: &str,
    trail_color: TrailColor,
    intensity: f32,
    cursor: u32,
) {
    let spec = Spec::of(word);
    fx.set_cursor_glow(
        true,
        spec.trail,
        trail_color,
        None,
        260,
        24,
        intensity,
        0.6,
        true,
        cursor,
    );
    fx.set_ribbon_shape(spec.tall, spec.flat);
}

impl Replay {
    pub fn new(style: &str, look: Look, trail_color: TrailColor) -> Self {
        Self::sized(style, look, trail_color, ROWS)
    }

    /// [`Replay::new`] on a pane of `rows` rows.
    pub fn sized(style: &str, look: Look, trail_color: TrailColor, rows: u16) -> Self {
        Self::gridded(style, look, trail_color, rows, COLS)
    }

    /// [`Replay::new`] on a pane of `rows` rows and `cols` columns.
    pub fn gridded(style: &str, look: Look, trail_color: TrailColor, rows: u16, cols: u16) -> Self {
        Self::gridded_in(GALLERY_PX, style, look, trail_color, (rows, cols))
    }

    /// [`Replay::sized`] with the window's face at `px`.
    pub fn sized_at(style: &str, look: Look, trail_color: TrailColor, rows: u16, px: f32) -> Self {
        Self::gridded_in(px, style, look, trail_color, (rows, COLS))
    }

    /// [`Replay::gridded`] in a face of `px`.
    pub fn gridded_in(
        px: f32,
        style: &str,
        look: Look,
        trail_color: TrailColor,
        grid: (u16, u16),
    ) -> Self {
        let rend = Renderer::from_system(px, Theme::default())
            .expect("no font: build with aterm-render's default embedded-font feature");
        Self::gridded_on(rend, style, look, trail_color, grid)
    }

    /// [`Replay::gridded`] drawn by `rend`: in its face, on the cell it lays.
    pub fn gridded_on(
        rend: Renderer,
        style: &str,
        look: Look,
        trail_color: TrailColor,
        (rows, cols): (u16, u16),
    ) -> Self {
        let (cw, ch) = rend.cell_size();
        let mut term = Terminal::new(rows, cols);
        let rgb = |c: u32| Rgb {
            r: (c >> 16) as u8,
            g: (c >> 8) as u8,
            b: c as u8,
        };
        term.set_default_foreground(rgb(look.fg));
        term.set_default_background(rgb(look.bg));
        term.set_default_cursor_color(Some(rgb(look.cursor)));
        let mut fx = EffectsPipeline::new();
        fx.set_focused(true);
        let classic = style.trim().eq_ignore_ascii_case("classic");
        set_glow(
            &mut fx,
            style,
            trail_color,
            if classic { 0.7 } else { 1.0 },
            look.cursor,
        );
        let pet = Spec::of(style).pet;
        if let Some(species) = pet {
            fx.set_cursor_pet_species(species);
        }
        fx.set_cursor_pet(pet.is_some(), 0x5EED);
        Self {
            term,
            fx,
            rend,
            cell: (cw, ch),
            look,
            ms: 0,
            clean: None,
            dump: Vec::new(),
            old_paths: Vec::new(),
            letters: false,
            own: false,
            bloom: None,
            shade: false,
            air: false,
            rail: false,
            caret: false,
            post: false,
            coat: false,
            skip_spared_blur: false,
            spared: 0,
            keys_unnoted: false,
            frames_at: None,
            rows,
            cols,
            keep: Vec::new(),
            kept: Vec::new(),
            keep_to: None,
        }
    }

    fn to(&mut self, ms: u64) {
        if ms > self.ms {
            self.fx.advance((ms - self.ms) as f64);
            self.ms = ms;
        }
    }

    fn out(&mut self, bytes: &[u8]) {
        let at = ClockReading {
            monotonic: self.fx.now(),
            wall_ms: None,
        };
        self.term.process_at(bytes, at);
    }

    /// The frame just applied as its sidecars would name it ([`Glass`]), the grid's origin on
    /// the frame at `(px, py)`: the effects name their boxes in grid px here, as
    /// [`Coat::pet`] reads the companion's.
    fn glass(&self, (px, py): (i32, i32)) -> Glass {
        let placed = |(x0, x1, y0, y1): (i32, i32, i32, i32)| (x0 + px, x1 + px, y0 + py, y1 + py);
        let pet_body = self.fx.companion_hit_rect().map(placed);
        let brain = self.fx.companion().brain();
        Glass {
            text: (0..usize::from(self.rows))
                .map(|row| self.term.row_text(row).unwrap_or_default())
                .collect(),
            caret_visible: self.term.cursor_visible(),
            caret_style: self.term.cursor_style(),
            pet_body,
            pet_alpha: self.fx.cursor_pet_alpha(),
            pet_pose: brain.pose_name(),
            pet_seat: brain.seat_shadow().to_string(),
            pet_action: match pet_body {
                Some(_) => format!("{:?}", brain.action()).to_ascii_lowercase(),
                None => "none".to_string(),
            },
            head_body: self.fx.cursor().glow.head_body_window_box().map(placed),
            word_icons: self
                .fx
                .word_icon_boxes()
                .iter()
                .copied()
                .map(placed)
                .collect(),
            v2: self
                .fx
                .cursor()
                .glow
                .v2_status()
                .map(|s| [s.quads, s.halos, s.stars, s.meteors]),
        }
    }

    /// Present one frame and judge its letters.
    pub fn present(&mut self) -> Judged {
        assert!(
            !self.skip_spared_blur
                || (self.letters
                    && self.dump.is_empty()
                    && self.keep.is_empty()
                    && !(self.shade || self.air || self.rail || self.caret)),
            "fixture: a replay that leaves a blur out reads nothing but its letters"
        );
        let (cw, ch) = self.cell;
        let (rows, cols) = (usize::from(self.rows), usize::from(self.cols));
        let mut input: RenderInput = self.term.cell_frame(rows, cols);
        let caret = (input.cursor_row, input.cursor_col);
        let key = (self.term.screen_mutations(), caret);
        if self.clean.as_ref().is_none_or(|c| c.at != key) {
            self.clean = Some(CleanFrame {
                at: key,
                px: self.rend.render_input(&input).pixels,
                plans: Default::default(),
            });
        }
        // What the frame's glyphs lay down, read off this input before the effects run, as the
        // GUI reads it every frame (`app_render`'s `glyph_ink_for` and `observe_glyph_ink`): the
        // letter guard reads each letter by its face's raster.
        let ink = self.rend.glyph_ink_for(&input);
        self.fx.cursor_mut().glow.observe_glyph_ink(Some(ink));
        for &law in &self.old_paths {
            self.fx.set_pet_old_path(law, true);
        }
        self.fx.apply(&mut self.term, &mut input, cw, ch);
        let mut frame = self.rend.render_input(&input);
        if let Some(unheld) = self.bloom
            && (unheld || !input.bloom_gate.held())
        {
            aterm_render::bloom::apply(
                &mut frame,
                &input.cursor_glow_add,
                input.rows,
                aterm_render::bloom::BloomParams::default(),
            )
            .expect("the frame blooms");
        }
        let place = self.rend.fx_frame(frame.width, frame.height, rows);
        if self.post {
            use aterm_render::{bloom, hdr};
            let bg = self.look.bg;
            // The GPU's bloom source (`fx_gpu::bloom_source`): the over-text stream with each
            // ramp row the quad it stands for, then the `bloom` layers' quads. The crown reads
            // the same lights.
            let mut source = Vec::new();
            input
                .cursor_glow_add_ramps
                .expand_into(&input.cursor_glow_add, &mut source);
            aterm_render::fx_bloom_quads(
                &place,
                &input.fx_layers,
                &input.fx_occluders,
                &mut aterm_render::FxSlicer::default(),
                &mut aterm_render::SpriteCols::default(),
                &mut source,
            );
            // A frame whose bloom spares its letters crowns and blooms from the
            // lights kept off them (`bloom::spared_sources`), as the GPU does.
            let (mut crown, mut blur) = (Vec::new(), Vec::new());
            let mut mask = None;
            let (crown, blur) = if input.bloom_gate.spares_letters() {
                let grid = bloom::LetterGrid {
                    pad: place.pad,
                    grid_top: place.grid_top,
                    cell_w: cw,
                    cell_h: ch,
                };
                mask = bloom::spared_sources(
                    &source,
                    &input,
                    grid,
                    bloom::BLOOM_RADIUS,
                    &mut crown,
                    &mut blur,
                );
                (&crown, &blur)
            } else {
                (&source, &source)
            };
            if !input.bloom_gate.held() {
                let reached = mask
                    .as_ref()
                    .filter(|_| self.skip_spared_blur)
                    .map(|m| spared_letters_in_reach(m, blur, &input));
                if let Some(reached) = reached {
                    let reached = reached.unwrap_or_else(|| {
                        panic!(
                            "{}: a letter the blur could reach stands outside its mask at {} ms",
                            self.look.name, self.ms
                        )
                    });
                    self.spared += reached;
                } else {
                    let params = bloom::BloomParams {
                        strength: bloom::BLOOM_STRENGTH * hdr::light_page_bloom_share(bg),
                        radius: bloom::BLOOM_RADIUS,
                    };
                    bloom::apply_kept_off(&mut frame, blur, input.rows, params, mask.as_ref())
                        .expect("the frame blooms");
                }
            }
            let budget = hdr::sdr_glow_budget(hdr::packed_luma(bg), SDR_BOOST);
            bloom::sdr_crown(&mut frame, crown, input.rows, budget).expect("the frame is crowned");
        }
        let mut coat = Coat::default();
        if self.coat {
            let mut bodies = Vec::new();
            aterm_render::knock_rects(
                &place,
                &input.fx_layers,
                &input.fx_occluders,
                &mut aterm_render::FxSlicer::default(),
                &mut aterm_render::SpriteCols::default(),
                &mut bodies,
            );
            let (px, py) = (place.pad as i32, place.grid_top as i32);
            let pet = self
                .fx
                .companion_hit_rect()
                .map(|(x0, x1, y0, y1)| (x0 + px, x1 + px, y0 + py, y1 + py));
            coat.pet = pet;
            coat.alpha = self.fx.cursor_pet_alpha();
            coat.sink_fit = pet.map(|_| self.fx.companion_head_sink_fit());
            coat.icons = self
                .fx
                .word_icon_boxes()
                .iter()
                .map(|&(x0, x1, y0, y1)| (x0 + px, x1 + px, y0 + py, y1 + py))
                .collect();
            coat.caret_cell = caret;
            coat.lettered = input
                .cells
                .iter()
                .map(|line| {
                    let mut row = vec![false; cols];
                    for (col, c) in line.iter().enumerate().take(cols) {
                        if !(c.ch == '\0' || c.ch.is_control() || c.ch.is_whitespace()) {
                            row[col] = true;
                            if c.wide && col + 1 < cols {
                                row[col + 1] = true;
                            }
                        }
                    }
                    row
                })
                .collect();
            let in_cells = |(x0, x1, y0, y1): (i32, i32, i32, i32)| {
                (
                    (x0 - px) as f32 / cw as f32,
                    (x1 - px) as f32 / cw as f32,
                    (y0 - py) as f32 / ch as f32,
                    (y1 - py) as f32 / ch as f32,
                )
            };
            coat.pet_cells = pet.map(in_cells);
            coat.icon_cells = coat.icons.iter().copied().map(in_cells).collect();
            if let Some((qx0, qx1, qy0, qy1)) = pet {
                let (crow, ccol) = caret;
                let (cy0, cy1) = (place.grid_top + crow * ch, place.grid_top + (crow + 1) * ch);
                let cell = |x: i32| (x - place.pad as i32) as f32 / cw as f32;
                coat.box_on_caret = qy0 < cy1 as i32
                    && qy1 > cy0 as i32
                    && u16::try_from(ccol).is_ok_and(|cc| {
                        aterm_effects::kitty_pet::lies_on_the_caret((cell(qx0), cell(qx1)), cc)
                    });
                let meets = |b: &aterm_render::KnockRect| {
                    b.x0 < qx1 && b.x1 > qx0 && b.y0 < qy1 && b.y1 > qy0
                };
                let body: Vec<_> = bodies.iter().filter(|b| meets(b)).copied().collect();
                let on_body = |x: i32, y: i32| {
                    body.iter().any(|b| {
                        x >= b.x0.max(qx0)
                            && x < b.x1.min(qx1)
                            && y >= b.y0.max(qy0)
                            && y < b.y1.min(qy1)
                    })
                };
                let page = self.look.bg & 0x00FF_FFFF;
                let w = frame.width;
                let h = frame.pixels.len() / w;
                let clean = &self.clean.as_ref().expect("rendered above").px;
                let off_page = |c: u32| {
                    [16u32, 8, 0].iter().any(|&sh| {
                        (((c >> sh) & 0xff) as i32 - ((page >> sh) & 0xff) as i32).abs() > 24
                    })
                };
                for (row, line) in input.cells.iter().enumerate() {
                    // Every column of the row, its cells or not: a row's cells end at its last
                    // written one, and the caret past them (a line typed to its end, the margin's
                    // wall) is judged too.
                    for col in 0..cols {
                        let letter = line.get(col).map_or(' ', |c| c.ch);
                        let is_caret = (row, col) == caret;
                        let glyph = !(letter == ' ' || letter == '\0' || letter.is_control());
                        if !glyph && !is_caret {
                            continue;
                        }
                        let (x0, y0) = (place.pad + col * cw, place.grid_top + row * ch);
                        if glyph
                            && !letter.is_whitespace()
                            && (x0 as i32) < qx1
                            && (x0 + cw) as i32 > qx0
                            && (y0 as i32) < qy1
                            && (y0 + ch) as i32 > qy0
                        {
                            coat.box_cells.push((row, col));
                            let across = ((x0 + cw) as i32).min(qx1) - (x0 as i32).max(qx0);
                            coat.box_reach = coat.box_reach.max(across as usize);
                        }
                        // `on_body` holds nowhere outside the box.
                        let mut n = 0usize;
                        for y in
                            y0.max(qy0.max(0) as usize)..(y0 + ch).min(h).min(qy1.max(0) as usize)
                        {
                            for x in x0.max(qx0.max(0) as usize)
                                ..(x0 + cw).min(w).min(qx1.max(0) as usize)
                            {
                                if !on_body(x as i32, y as i32) {
                                    continue;
                                }
                                if is_caret {
                                    coat.caret_px += 1;
                                }
                                if glyph && off_page(clean[y * w + x] & 0x00FF_FFFF) {
                                    n += 1;
                                }
                            }
                        }
                        if n > 0 {
                            coat.ink_px += n;
                            coat.cells.push((row, col, n));
                        }
                    }
                }
                for &(ix0, ix1, iy0, iy1) in &coat.icons {
                    let dx = (ix1.min(qx1) - ix0.max(qx0)).max(0);
                    let dy = (iy1.min(qy1) - iy0.max(qy0)).max(0);
                    coat.icon_px += (dx * dy) as usize;
                }
            }
        }
        if let Some(i) = self.dump.iter().position(|(at, _)| *at == self.ms) {
            let (_, path) = self.dump.remove(i);
            std::fs::write(&path, frame.to_png()).expect("dump");
        }
        if self.keep.contains(&self.ms) {
            let kept = Kept {
                ms: self.ms,
                px: frame.pixels.clone(),
                w: frame.width,
                top: place.grid_top,
                clean: self.clean.as_ref().expect("rendered above").px.clone(),
                left: place.pad,
                caret,
                cell: self.cell,
                caret_ground: self.fx.thin_caret_ground(),
                glass: self.glass((place.pad as i32, place.grid_top as i32)),
            };
            match &self.keep_to {
                Some(to) => to.send(kept).expect("the kept frames' reader"),
                None => self.kept.push(kept),
            }
        }
        let w = frame.width;
        let CleanFrame {
            px: clean, plans, ..
        } = self.clean.as_mut().expect("rendered above");
        let clean = &*clean;
        let pair = Pair {
            frame: &frame.pixels,
            clean,
            w,
            cw,
            ch,
        };
        let mut ratios = Vec::new();
        // The cell `(row, col)` as `aterm-lab trail-contrast` measures it, its own reading too
        // where `own` says; `None` for a blank cell or one the effects left as its twin.
        let mut judge = |row: usize, col: usize, span: usize, ch: char, own: bool| {
            if pair.untouched(row, col, span) {
                return None;
            }
            let plan = plans
                .entry((row, col, span))
                .or_insert_with(|| pair.plan(row, col, span))
                .as_ref()?;
            Some(Letter {
                row,
                col,
                ch,
                local: plan.reading(pair.frame, &mut ratios),
                own: if own {
                    plan.own(clean, &mut ratios)
                } else {
                    f64::INFINITY
                },
            })
        };
        let letters = self.letters.then(|| {
            let mut letters = Vec::new();
            for (row, line) in input.cells.iter().enumerate() {
                for (col, c) in line.iter().enumerate().take(cols) {
                    if (row, col) == caret || c.ch == ' ' || c.ch == '\0' || c.ch.is_control() {
                        continue;
                    }
                    if !c.ch.is_alphanumeric() {
                        continue;
                    }
                    let span = if c.wide { 2 } else { 1 };
                    if let Some(l) = judge(row, col, span, c.ch, self.own) {
                        letters.push(l);
                    }
                }
            }
            letters
        });
        let caret_letter = if self.caret {
            let (row, col) = caret;
            input
                .cells
                .get(row)
                .and_then(|line| line.get(col))
                .filter(|c| !c.wide && c.ch != ' ' && c.ch != '\0' && !c.ch.is_control())
                .and_then(|c| judge(row, col, 1, c.ch, true))
        } else {
            None
        };
        let mut shade = 0;
        if self.shade {
            let page = self.look.bg & 0x00FF_FFFF;
            let bar = SHADE_SHARE * luma(page);
            shade = frame
                .pixels
                .iter()
                .zip(clean.iter())
                .filter(|&(&f, &c)| c & 0x00FF_FFFF == page && luma(f & 0x00FF_FFFF) < bar)
                .count();
        }
        let mut air = 0;
        if self.air {
            let page = self.look.bg & 0x00FF_FFFF;
            let bar = AIR_LIFT * luma(page);
            for row in 0..rows {
                for col in 0..cols {
                    let blank = input
                        .cells
                        .get(row)
                        .and_then(|line| line.get(col))
                        .is_none_or(|c| c.ch == ' ' || c.ch == '\0');
                    if (row, col) == caret || !blank {
                        continue;
                    }
                    let (x0, y0) = (col * cw, row * ch);
                    for y in y0..(y0 + ch).min(frame.pixels.len() / w) {
                        for x in x0..(x0 + cw).min(w) {
                            let (f, c) = (frame.pixels[y * w + x], clean[y * w + x]);
                            if c & 0x00FF_FFFF == page && luma(f & 0x00FF_FFFF) > bar {
                                air += 1;
                            }
                        }
                    }
                }
            }
        }
        let mut rail = Vec::new();
        if self.rail {
            let (row, col) = caret;
            let h = frame.pixels.len() / w;
            let band = (
                (row + 1) * ch + RAIL_BAND.start,
                (row + 1) * ch + RAIL_BAND.end,
            );
            let stands = self.fx.companion_hit_rect().and_then(|(x0, _, y0, y1)| {
                let (px, py) = (place.pad as i32, place.grid_top as i32);
                (y0 + py < band.1 as i32 && y1 + py > band.0 as i32).then_some(x0 + px)
            });
            let lift = |f: u32, c: u32| {
                [16u32, 8, 0]
                    .iter()
                    .map(|&sh| {
                        (((f >> sh) & 0xff) as i32 - ((c >> sh) & 0xff) as i32).unsigned_abs()
                    })
                    .max()
                    .unwrap_or(0) as u8
            };
            for x in 0..(col * cw).min(w) {
                if stands.is_some_and(|x0| x as i32 >= x0) {
                    break;
                }
                let top = (row + 1) * ch;
                rail.push(
                    RAIL_BAND
                        .clone()
                        .map(|dy| top + dy)
                        .filter(|&y| y < h)
                        .map(|y| lift(frame.pixels[y * w + x], clean[y * w + x]))
                        .max()
                        .unwrap_or(0),
                );
            }
        }
        Judged {
            ms: self.ms,
            letters,
            shade,
            air,
            rail,
            caret: caret_letter,
            coat,
            trail: input.cursor_trail.len(),
        }
    }

    /// Run `events` (`(ms, event)`, sorted) and `tail` ms after the last, presenting a frame every
    /// [`FRAME_MS`]; every frame judged.
    pub fn run(&mut self, events: &[(u64, Ev)], tail: u64) -> Vec<Judged> {
        let mut timeline: Vec<(u64, u8, usize)> = Vec::new();
        for (i, (at, ev)) in events.iter().enumerate() {
            timeline.push((*at, 0, i));
            if matches!(ev, Ev::Key(..)) {
                timeline.push((*at + 2, 1, i));
            }
        }
        let end = events.last().map_or(0, |e| e.0) + tail;
        if let Some(frames) = &self.frames_at {
            timeline.extend(frames.iter().map(|&f| (f, 2, 0)));
        } else {
            let mut f = FRAME_MS;
            while f <= end {
                timeline.push((f, 2, 0));
                f += FRAME_MS;
            }
        }
        timeline.sort_unstable();
        let mut judged = Vec::new();
        for (at, kind, i) in timeline {
            self.to(at);
            match (kind, &events[i].1) {
                (0, Ev::Key(..)) if self.keys_unnoted => {}
                (0, Ev::Key('\r', _)) => {
                    // A window's Enter: the console's submit (the trail's
                    // return flare and the companion's) and the keystroke.
                    self.fx.note_console_input(PetInputKind::Submit);
                    self.fx.note_keystroke();
                }
                (0, Ev::Key('\u{7f}', _)) => {
                    // A window's Backspace: the console's delete and the keystroke.
                    self.fx.note_console_input(PetInputKind::Delete);
                    self.fx.note_keystroke();
                }
                (0, Ev::Key(LEFT | HOME | END, _)) => {
                    // A window's Left arrow, Home or End: the console's move along the line and
                    // the keystroke.
                    self.fx.note_console_input(PetInputKind::Navigate);
                    self.fx.note_keystroke();
                }
                (0, Ev::Key(c, _)) => {
                    let input = self
                        .term
                        .cell_frame(usize::from(self.rows), usize::from(self.cols));
                    let _ = self.fx.note_committed_char(&mut self.term, &input, *c);
                }
                (0, Ev::Out(bytes)) | (1, Ev::Key(_, bytes)) => {
                    let bytes = bytes.clone();
                    self.out(&bytes);
                }
                (2, _) => judged.push(self.present()),
                _ => {}
            }
        }
        judged
    }
}

/// `text` typed from `start` ms, a key every `every` ms.
pub fn typed(text: &str, start: u64, every: u64) -> Vec<(u64, Ev)> {
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let mut b = [0u8; 4];
            (
                start + i as u64 * every,
                Ev::Key(c, c.encode_utf8(&mut b).as_bytes().to_vec()),
            )
        })
        .collect()
}

pub const TYPING_TEXT: &str = "echo the quick brown fox";
/// `fold-rest`'s line, the gallery's fox cycled to three cells short of a prompt's row end.
pub const FOLD_REST_TEXT: &str =
    "echo the quick brown fox walks along every line kitty types echo the ";
/// `focus-wrap`'s columns.
pub const FOCUS_WRAP_COLS: u16 = 32;
/// The row `last-row`'s page lays over the prompt, a letter in every cell.
pub const LAST_ROW_FULL: &str = "Typography_jumps;quick_brown_gyps(qwerty)_pqgyj_{braces}[ok]|";
/// The rows `last-row`'s page lays between its full ones.
pub const LAST_ROW_RAGGED: [&str; 4] = [
    "fn main() {",
    "    let fox = \"the quick brown fox\";",
    "    // every glyph here must stay legible: pqgyj {braces} [ok]",
    "}",
];
pub const BURST_TEXT: &str = "echo the quick brown fox jumps over the lazy dog as the kitty flies";
/// [`LastRowThen::Gapped`]'s burst: [`BURST_TEXT`]'s length, its last words a run of fourteen
/// spaces and two letters, so the line's last letters lie by the margin and a stand by the caret
/// off them lies in the spaces before them.
pub const GAPPED_TEXT: &str = "echo the quick brown fox jumps over the lazy dog as              ab";
const _: () = assert!(GAPPED_TEXT.len() == BURST_TEXT.len());
/// A Left arrow's key in a replay's events: the console reads it as a move along the line.
pub const LEFT: char = '\u{2190}';
/// A Home key's and an End key's in a replay's events: each a move along the line, as [`LEFT`].
pub const HOME: char = '\u{21e4}';
pub const END: char = '\u{21e5}';

/// What [`Scene::LastRowRoom`]'s line does once its burst is typed, before its Enter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LastRowThen {
    /// It rests to the Enter.
    Rests,
    /// `keys` Backspaces 50 ms apart from `after` ms past the burst's last key, each echoed as a
    /// line editor echoes one at its line's end (back a cell, erase to the end).
    Erased { keys: u16, after: u64 },
    /// Ten Left arrows 50 ms apart from 1.5 s past the burst's last key, each echoed back a cell.
    Back,
    /// The burst typed to [`PAUSED_SHORT`] keys short of its end, a 3 s pause, then the rest.
    Paused,
    /// 1.5 s past the burst's last key the program clears the row over the line
    /// ([`OPEN_ROW_OVER`]), the caret kept where it stands: a seat opens beside the line.
    Opened,
    /// [`GAPPED_TEXT`] typed for the burst, and it rests to the Enter.
    Gapped,
}

/// The bytes [`LastRowThen::Opened`]'s program writes: the caret saved, a row up, that row
/// cleared, the caret restored.
pub const OPEN_ROW_OVER: &[u8] = b"\x1b7\x1b[A\x1b[2K\x1b8";

/// How many keys short of its end [`LastRowThen::Paused`]'s burst pauses.
pub const PAUSED_SHORT: usize = 6;

/// [`Scene::LastRow`]'s timeline, its line doing what `then` says: the burst's last key, the
/// first key past it (a Backspace's, a Left arrow's), for [`LastRowThen::Paused`] the first past
/// the pause, and for [`LastRowThen::Opened`] the row's clearing, and the Enter, 6.05 s past the
/// burst's last key (ms).
pub fn last_row_times(then: LastRowThen) -> (u64, Option<u64>, u64) {
    let burst = 1_000 + 50 * (BURST_TEXT.len() as u64 - 1);
    let (last_key, then_from) = match then {
        LastRowThen::Rests | LastRowThen::Gapped => (burst, None),
        LastRowThen::Erased { after, .. } => (burst, Some(burst + after)),
        LastRowThen::Back | LastRowThen::Opened => (burst, Some(burst + 1_500)),
        LastRowThen::Paused => {
            let resume = 1_000 + 50 * (BURST_TEXT.len() - PAUSED_SHORT) as u64 + 3_000;
            (resume + 50 * (PAUSED_SHORT as u64 - 1), Some(resume))
        }
    };
    (last_key, then_from, last_key + 6_050)
}
/// [`Scene::Wrapped`]'s line: past the margin of the gallery's 72 columns from the prompt, onto a
/// second row, where it stops at column 23.
pub const WRAPPED_TEXT: &str =
    "echo the quick brown fox jumps over the lazy dog as the kitty flies past the margin too";

/// The gallery's scenes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    Typing,
    EnterBottom,
    Burst,
    Cjk,
    /// The gallery's `move-up`: lettered rows above a prompt, `./k` typed and entered, and a
    /// second later the program moves the caret up onto `println!`'s `!` and holds it there,
    /// as vim's `k` does.
    MoveUp,
    /// [`Scene::MoveUp`] with its Enter 10 ms after the last key, before the cat has walked
    /// clear of the line's last letters: where the gallery's frames left its cat when it
    /// pressed the Enter (over `k` in every `move-up` instance of looks8 round 2's first
    /// capture), a pace the 16 ms replay of the gallery's 84 ms does not reach.
    MoveUpHasty,
    /// The gallery's `split` burst: [`Scene::Burst`] in the caret's pane of a window split
    /// top over bottom, 6 rows, the line on its last row but one ([`Scene::rows`]).
    SplitBurst,
    /// The gallery's `context`: the typed line between a filled powerline row above and a
    /// selected row below, behind a prompt segment, lettered rows past both (a shell prompt
    /// under a powerline, Claude Code's input box between its rules).
    Context,
    /// A line typed on past the margin ([`WRAPPED_TEXT`]): wrapped onto a second row and stopped
    /// there, the cat resting beside the caret under the line's full first row (r24 K7-4).
    Wrapped,
    /// The gallery's `fold-rest`: a burst to three cells short of the row's end, the last three
    /// keys at once across the wrap ([`FOLD_REST_TEXT`]), and the rest after them.
    FoldRest,
    /// The gallery's `focus-wrap`: on [`FOCUS_WRAP_COLS`] columns in the alternate screen, a
    /// composer's prompt written two cells short of its second row's end, a focus round trip
    /// reported on a status row under it, and six more keys across the wrap.
    FocusWrap,
    /// [`Scene::FocusWrap`]'s page on the main screen, a status row under the composer: the stand
    /// by the wrap's caret would be fitted over the status row's end, so the crossing leaves the
    /// row under the line for a whole seat a row lower (the head law's seat is the full screen's
    /// composer row on [`Scene::FocusWrap`]; a gallery capture, whose frames are no presents, reads
    /// no full screen and flies this one).
    StatusWrap,
    /// The gallery's `last-row`: the caret on the grid's last row under a page whose row over it
    /// holds a letter in every cell ([`last_row_page`]), a burst along it, an Enter whose answer
    /// scrolls the page, and the typing line again. The burst's line ends as near the grid's end
    /// as leaves its fitted companion the room it takes ([`last_row_cols`]).
    LastRow,
    /// [`Scene::LastRow`] on a grid as wide as leaves this many blank cells past the burst's line
    /// (`-1`: one cell too narrow for it, so the line wraps at its last key and the page scrolls),
    /// the line doing what the [`LastRowThen`] says before its Enter.
    LastRowRoom(i16, LastRowThen),
    /// The gallery's `steam`: the burst between two lettered rows, then twelve Backspaces 50 ms
    /// apart and the fade.
    Steam,
    /// [`Scene::Typing`] underlined (SGR 4): the stroke lies in the row's foot band, under its
    /// letters' baseline, where a trail lays its bed and a companion its rail.
    Underlined,
    /// [`Scene::Context`] with the typed line underlined (SGR 4): over the selected row, a
    /// trail lays its rail in the typed row's own foot band, beside the stroke.
    UnderlinedContext,
}

impl Scene {
    /// Whether the scene lays the gallery's `context` page ([`Scene::Context`]), whose powerline
    /// segments and selected row read under the bar on their own.
    pub fn on_context(self) -> bool {
        matches!(self, Scene::Context | Scene::UnderlinedContext)
    }

    /// The scene's pane rows: the `split` caret pane's 6, else the gallery window's [`ROWS`].
    pub fn rows(self) -> u16 {
        match self {
            Scene::SplitBurst => 6,
            _ => ROWS,
        }
    }

    /// The scene's columns: `focus-wrap`'s narrow composer's, `last-row`'s line and the room its
    /// fitted companion takes past it on the window's face ([`last_row_cols`]), else the gallery
    /// window's [`COLS`].
    pub fn cols(self) -> u16 {
        match self {
            Scene::FocusWrap | Scene::StatusWrap => FOCUS_WRAP_COLS,
            Scene::LastRow => last_row_cols(gallery_cell()),
            Scene::LastRowRoom(room, _) => {
                u16::try_from(i32::from(last_row_line()) + i32::from(room))
                    .expect("a grid as wide as the line")
            }
            _ => COLS,
        }
    }

    /// The frames the trail gallery renders through the scene, ms, where it has its own
    /// steps (`aterm-lab`'s `trail_gallery`): `move-up`'s a frame 30 ms after each key and
    /// its `typed` frame, every 60 ms for 900 ms after the Enter, none while it awaits the
    /// program's mark, then `up` as the mark lands, every 40 ms to `u120` and every 60 ms
    /// to `u470`.
    pub fn gallery_frames(self) -> Option<Vec<u64>> {
        match self {
            Scene::MoveUp | Scene::MoveUpHasty => {
                let (ev, _) = self.events();
                let keys: Vec<u64> = ev
                    .iter()
                    .filter(|(_, e)| matches!(e, Ev::Key(c, _) if *c != '\r'))
                    .map(|(at, _)| *at)
                    .collect();
                let enter = ev
                    .iter()
                    .find(|(_, e)| matches!(e, Ev::Key('\r', _)))
                    .map(|(at, _)| *at)?;
                let mark = ev.last().map(|(at, _)| *at)?;
                let mut frames: Vec<u64> = keys.iter().map(|k| k + 30).collect();
                frames.push(enter - 20);
                frames.extend((1..=15).map(|i| enter + 60 * i));
                let up = mark + 9;
                frames.push(up);
                frames.extend((1..=3).map(|i| up + 40 * i));
                let mut f = up + 120;
                while f < up + 470 {
                    f = (f + 60).min(up + 470);
                    frames.push(f);
                }
                Some(frames)
            }
            _ => None,
        }
    }

    /// [`Scene::events`] on `look`: the context's selected row in the gallery's selection
    /// colour for the theme (`aterm-lab`'s `trail_gallery`).
    pub fn events_on(self, look: Look) -> (Vec<(u64, Ev)>, u64) {
        let (mut ev, tail) = self.events();
        if self.on_context() && luma(look.bg) > 0.5 {
            for (_, e) in &mut ev {
                if let Ev::Out(bytes) = e {
                    let text = String::from_utf8_lossy(bytes).replace("51;65;94", "182;227;255");
                    *bytes = text.into_bytes();
                }
            }
        }
        (ev, tail)
    }

    pub fn events(self) -> (Vec<(u64, Ev)>, u64) {
        let prompt = |at: u64| (at, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()));
        match self {
            Scene::Context => {
                let sel = format!(
                    "\x1b[48;2;51;65;94m{:<72}\x1b[0m\r\n",
                    "src/trail.rs:412  a selected line under the prompt"
                );
                let page = format!(
                    "\x1b[2J\x1b[3H\
                     Above: Typography jumps; quick brown gyps (qwerty) _gy|\r\n\
                     \x1b[30;42m ok \x1b[97;44m ~/aterm \x1b[30;43m main +2 \x1b[0;48;5;236m\x1b[K\x1b[0m\r\n\
                     \r\n{sel}\
                     Below: every glyph here must stay legible! pqgyj {{braces}} [ok]\r\n\
                     \x1b[31merror\x1b[0m: \x1b[1mcolour contrast\x1b[0m below the caret path\r\n\
                     \x1b[5H\x1b[30;46m aterm \x1b[0m "
                );
                let mut ev = vec![(200, Ev::Out(page.into_bytes()))];
                ev.extend(typed(TYPING_TEXT, 1_000, 60));
                (ev, 600)
            }
            Scene::Typing => {
                let mut ev = vec![prompt(200)];
                ev.extend(typed(TYPING_TEXT, 1_000, 60));
                (ev, 600)
            }
            Scene::Underlined | Scene::UnderlinedContext => {
                let (mut ev, tail) = if self == Scene::Underlined {
                    Scene::Typing.events()
                } else {
                    Scene::Context.events()
                };
                // The page leaves the caret behind its prompt; the keys' echoes print underlined.
                if let Some((_, Ev::Out(page))) = ev.first_mut() {
                    page.extend_from_slice(b"\x1b[4m");
                }
                (ev, tail)
            }
            Scene::Wrapped => {
                let mut ev = vec![prompt(200)];
                ev.extend(typed(WRAPPED_TEXT, 1_000, 60));
                (ev, 3_000)
            }
            Scene::Cjk => {
                let mut ev = vec![prompt(200)];
                ev.extend(typed("echo 你好世界", 1_000, 60));
                (ev, 600)
            }
            Scene::Burst | Scene::SplitBurst => {
                let mut ev = vec![prompt(200)];
                ev.extend(typed(BURST_TEXT, 1_000, 50));
                (ev, 4_600)
            }
            Scene::EnterBottom => {
                let mut seq = String::from("\x1b[2J\x1b[H$ seq 1 40\r\n");
                for n in 1..=40 {
                    seq.push_str(&format!("{n}\r\n"));
                }
                seq.push_str("$ ");
                let mut ev = vec![(200, Ev::Out(seq.into_bytes()))];
                ev.extend(typed(TYPING_TEXT, 1_000, 60));
                let enter = 1_000 + 60 * TYPING_TEXT.len() as u64 + 200;
                ev.push((enter, Ev::Key('\r', b"\r\n".to_vec())));
                ev.push((enter + 6, Ev::Out(b"the quick brown fox\r\n$ ".to_vec())));
                (ev, 600)
            }
            Scene::FoldRest => {
                let mut ev = vec![prompt(200)];
                ev.extend(typed(FOLD_REST_TEXT, 1_000, 50));
                let fold = 1_000 + 50 * FOLD_REST_TEXT.len() as u64;
                ev.extend(typed("xyz", fold, 1));
                (ev, 2_200)
            }
            Scene::FocusWrap | Scene::StatusWrap => {
                let screen: &[u8] = if self == Scene::FocusWrap {
                    b"\x1b[?1049h\x1b[?1004h"
                } else {
                    b""
                };
                let mut page = screen.to_vec();
                page.extend_from_slice(
                    b"\x1b[2J\x1b[5;1H\x1b[J> \x1b7\x1b[8;1H\x1b[2Kfocus ready\x1b8",
                );
                let mut ev = vec![(200, Ev::Out(page))];
                let warm = format!("{}shared", "a".repeat(54));
                ev.extend(typed(&warm, 1_000, 50));
                let done = 1_000 + 50 * warm.len() as u64;
                let report = b"\x1b7\x1b[8;1H\x1b[2Kfocus in\x1b8".to_vec();
                ev.push((done + 300, Ev::Out(report)));
                ev.extend(typed(" state", done + 500, 60));
                (ev, 1_500)
            }
            Scene::LastRow | Scene::LastRowRoom(..) => {
                let mut page = String::from("\x1b[H\x1b[2J");
                for over in (1..usize::from(ROWS)).rev() {
                    let text: String = if over % 2 == 1 {
                        LAST_ROW_FULL
                            .chars()
                            .cycle()
                            .take(usize::from(self.cols()))
                            .collect()
                    } else {
                        LAST_ROW_RAGGED[over / 2 % LAST_ROW_RAGGED.len()].to_string()
                    };
                    page.push_str(&text);
                    page.push_str("\r\n");
                }
                page.push_str("$ ");
                let mut ev = vec![(200, Ev::Out(page.into_bytes()))];
                let then = match self {
                    Scene::LastRowRoom(_, then) => then,
                    _ => LastRowThen::Rests,
                };
                let (_, then_from, enter) = last_row_times(then);
                match (then, then_from) {
                    (LastRowThen::Paused, Some(resume)) => {
                        let short = BURST_TEXT.len() - PAUSED_SHORT;
                        ev.extend(typed(&BURST_TEXT[..short], 1_000, 50));
                        ev.extend(typed(&BURST_TEXT[short..], resume, 50));
                    }
                    (LastRowThen::Erased { keys, .. }, Some(from)) => {
                        ev.extend(typed(BURST_TEXT, 1_000, 50));
                        ev.extend(
                            (0..u64::from(keys)).map(|k| {
                                (from + 50 * k, Ev::Key('\u{7f}', b"\x08\x1b[K".to_vec()))
                            }),
                        );
                    }
                    (LastRowThen::Back, Some(from)) => {
                        ev.extend(typed(BURST_TEXT, 1_000, 50));
                        ev.extend(
                            (0..10u64).map(|k| (from + 50 * k, Ev::Key(LEFT, b"\x08".to_vec()))),
                        );
                    }
                    (LastRowThen::Opened, Some(from)) => {
                        ev.extend(typed(BURST_TEXT, 1_000, 50));
                        ev.push((from, Ev::Out(OPEN_ROW_OVER.to_vec())));
                    }
                    (LastRowThen::Gapped, _) => ev.extend(typed(GAPPED_TEXT, 1_000, 50)),
                    _ => ev.extend(typed(BURST_TEXT, 1_000, 50)),
                }
                ev.push((enter, Ev::Key('\r', b"\r\n".to_vec())));
                ev.push((enter + 6, Ev::Out(b"Below: pqgyj [ok]\r\n$ ".to_vec())));
                ev.extend(typed(TYPING_TEXT, enter + 470, 60));
                (ev, 6_000)
            }
            Scene::Steam => {
                let page = "\x1b[2J\x1b[3H\
                    Above: Typography jumps; quick brown gyps (qwerty) _gy|\r\n\
                    Above2: The lazy dog jogged by, every glyph legible.\r\n\r\n\
                    Below: pqgyj Wide test {braces} [ok]\r\n\
                    \x1b[5H$ ";
                let mut ev = vec![(200, Ev::Out(page.as_bytes().to_vec()))];
                ev.extend(typed(TYPING_TEXT, 1_000, 50));
                let start = 1_000 + 50 * TYPING_TEXT.len() as u64 + 300;
                for k in 0..12u64 {
                    ev.push((start + 50 * k, Ev::Key('\u{8}', b"\x08 \x08".to_vec())));
                }
                (ev, 1_400)
            }
            Scene::MoveUp | Scene::MoveUpHasty => {
                let page = "\x1b[2J\x1b[H\
                    Above: Typography jumps; quick brown gyps (qwerty) _gy|\r\n\
                    fn main() {\r\n\
                    \x20   let fox = \"the quick brown fox\";\r\n\
                    \x20   println!(\"{fox} jumps over the lazy dog\");\r\n\
                    \x20   // every glyph here must stay legible: pqgyj {braces} [ok]\r\n\
                    }\r\n\
                    \x1b[8H$ ";
                let mut ev = vec![(200, Ev::Out(page.as_bytes().to_vec()))];
                ev.extend(typed("./k", 1_000, 60));
                // The Enter 84 ms after the last key, while the hand's rhythm runs, as the
                // gallery presses it (its `typed` frame between); the hasty one's 10 ms.
                let after = if self == Scene::MoveUpHasty { 10 } else { 84 };
                let enter = 1_000 + 60 * 2 + after;
                ev.push((enter, Ev::Key('\r', b"\r\n".to_vec())));
                ev.push((
                    enter + 1_000,
                    Ev::Out(b"[vim] NORMAL\x1b[5A\x1b[12G".to_vec()),
                ));
                (ev, 2_400)
            }
        }
    }
}

/// The worst letter of a replay and its frame.
pub fn worst(judged: &[Judged]) -> Option<(u64, Letter)> {
    judged
        .iter()
        .filter_map(|j| j.worst().map(|l| (j.ms, l)))
        .min_by(|a, b| a.1.judged().total_cmp(&b.1.judged()))
}

pub const STYLES: [&str; 17] = [
    "rainbow + cat",
    "underline rainbow + cat",
    "underline rainbow + dog",
    "underline rainbow",
    "rainbow + dog",
    "flat rainbow + cat",
    "rainbow",
    "fire + dog",
    "phaser",
    "fire",
    "water",
    "laser",
    "comet",
    "sparkle",
    "lumen",
    "beam",
    "classic",
];

/// One replay's verdict: how many judged letters fell under the bar, and the worst.
#[derive(Clone, Debug)]
pub struct Verdict {
    pub style: &'static str,
    pub look: &'static str,
    pub scene: Scene,
    pub under: usize,
    pub worst: Option<(u64, Letter)>,
    /// [`Replay::spared`].
    pub spared: usize,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:24} {:13} {:12?} under={:5} worst=",
            self.style, self.look, self.scene, self.under
        )?;
        match self.worst {
            None => write!(f, "-"),
            Some((ms, l)) => write!(
                f,
                "{:.2} at {ms} ms r{} c{} '{}'",
                l.judged(),
                l.row,
                l.col,
                l.ch
            ),
        }
    }
}

/// The bar a letter on `look` is judged at: [`BAR`], or four fifths of the theme's own
/// contrast where that leaves its clean letters under the bar (the judge reads a clean
/// letter's solid ink at [`SOLID_SHARE`] of it).
pub fn look_bar(look: Look) -> f64 {
    BAR.min(0.8 * contrast(look.fg, look.bg))
}

/// Replay `style` on `look` through `scene` in the trail colour `color`, the letter guard on
/// or off, every frame judged as the glass shows it, through the host's post passes (the bloom
/// and the SDR crown, [`Replay::post`]), at [`look_bar`]; `dump` names frames to keep as PNGs. A
/// pinned colour's Classic runs at the full default intensity, as
/// [`classic_in_a_chosen_colour_keeps_every_letter`] runs it. On the `context` page
/// ([`Scene::on_context`]) a letter whose own reading is under the bar on the bare page (the
/// theme's own powerline segments and selected row) is judged at [`OWN_SHARE`] of it. A guarded
/// replay that keeps no picture leaves out the blur its letters' judge cannot see
/// ([`Replay::skip_spared_blur`]).
pub fn replay_in(
    style: &'static str,
    look: Look,
    scene: Scene,
    guard: bool,
    dump: Vec<(u64, String)>,
    color: TrailColor,
) -> Verdict {
    let skip_spared_blur = guard && dump.is_empty();
    replay_with(
        style,
        look,
        scene,
        (guard, true, skip_spared_blur),
        dump,
        color,
    )
}

/// [`replay_in`] with the host's post passes sparing the letters (`BloomGate::SparesLetters`)
/// or, as a negative control, taking light from beside them too (`spares` false), its frames'
/// blur run whatever the mask keeps off.
pub fn replay_spared(
    style: &'static str,
    look: Look,
    scene: Scene,
    guard: bool,
    spares: bool,
    dump: Vec<(u64, String)>,
    color: TrailColor,
) -> Verdict {
    replay_with(style, look, scene, (guard, spares, false), dump, color)
}

fn replay_with(
    style: &'static str,
    look: Look,
    scene: Scene,
    (guard, spares, skip_spared_blur): (bool, bool, bool),
    dump: Vec<(u64, String)>,
    color: TrailColor,
) -> Verdict {
    let mut r = Replay::sized(style, look, color, scene.rows());
    r.letters = true;
    if matches!(color, TrailColor::Pinned(_)) {
        let spec = Spec::of(style);
        r.fx.set_cursor_glow(
            true,
            spec.trail,
            color,
            None,
            260,
            24,
            1.0,
            0.6,
            true,
            look.cursor,
        );
        r.fx.set_ribbon_shape(spec.tall, spec.flat);
    }
    r.fx.set_letter_guard(guard);
    r.fx.set_bloom_spares_letters(spares);
    r.post = true;
    r.own = scene.on_context();
    r.dump = dump;
    r.skip_spared_blur = skip_spared_blur;
    let (ev, tail) = scene.events_on(look);
    let judged = r.run(&ev, tail);
    let bar = look_bar(look);
    Verdict {
        style,
        look: r.look.name,
        scene,
        under: judged
            .iter()
            .map(|j| {
                j.letters()
                    .iter()
                    .filter(|l| l.judged() < bar.min(OWN_SHARE * l.own))
                    .count()
            })
            .sum(),
        worst: worst(&judged),
        spared: r.spared,
    }
}

/// [`replay_in`] every job in the automatic colour, spread over the machine's cores.
pub fn replay_all(
    jobs: &[(&'static str, Look, Scene)],
    guard: bool,
    dump: impl Fn(&str, Look, Scene) -> Vec<(u64, String)> + Sync,
) -> Vec<Verdict> {
    replay_all_in(jobs, guard, dump, TrailColor::Automatic)
}

/// [`replay_in`] every job in the trail colour `color`, spread over the machine's cores.
pub fn replay_all_in(
    jobs: &[(&'static str, Look, Scene)],
    guard: bool,
    dump: impl Fn(&str, Look, Scene) -> Vec<(u64, String)> + Sync,
    color: TrailColor,
) -> Vec<Verdict> {
    let lanes = aterm_cores::available_parallelism().map_or(4, usize::from);
    let dump = &dump;
    std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(lanes).max(1))
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|&(style, look, scene)| {
                            replay_in(style, look, scene, guard, dump(style, look, scene), color)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a replay lane"))
            .collect()
    })
}

const SCENES: [Scene; 4] = [Scene::Typing, Scene::EnterBottom, Scene::Burst, Scene::Cjk];
/// The scenes every letter law judges ([`every_letter_a_trail_lies_on_reads_at_the_bar`],
/// [`a_dim_letter_keeps_its_own_reading`]): [`SCENES`], the gallery's `move-up`, its split pane's
/// burst, its `context`, and a line typed on past the margin ([`Scene::Wrapped`]), whose second
/// row is typed under the line's full first row and crossed to by the companion's bound (r24
/// K7-6).
const BAR_SCENES: [Scene; 8] = [
    Scene::Typing,
    Scene::EnterBottom,
    Scene::Burst,
    Scene::Cjk,
    Scene::MoveUp,
    Scene::SplitBurst,
    Scene::Context,
    Scene::Wrapped,
];
/// The bundled JetBrains Mono, Linux's face: [`a_dim_letter_keeps_its_own_reading`]'s underlined
/// controls replay in it on every OS, in the 13 x 29 px cells they were measured in at
/// [`GALLERY_PX`]. The face and its cells carry over; macOS's raster does not, since it
/// rasterizes the face through CoreText, and Windows runs Linux's hinted coverage fill but has
/// not been measured, so the controls' margins on both are unmeasured.
const JETBRAINS_MONO: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");
/// The size [`a_dim_letter_keeps_its_own_reading`]'s ring control replays at where the window's
/// face does not lay the gallery's row at [`GALLERY_PX`] ([`lays_the_gallery_row`]): at 19 px
/// Linux's bundled JetBrains Mono lays 11 x 25 px cells, on which Fire's wrapped line on the dim
/// grey, read by its grounds' luminance, leaves a dim letter under 97% of its own reading (Linux
/// arm64, 2026-11-07); at the size that lays the gallery's row no line of [`BAR_SCENES`] does.
const RING_CONTROL_PX: f32 = 19.0;
/// The underlined lines [`a_dim_letter_keeps_its_own_reading`] judges besides [`BAR_SCENES`]: the
/// stroke in the typed row's foot band at the bare prompt and over the `context` page's
/// selected row.
const UNDERLINED_SCENES: [Scene; 2] = [Scene::Underlined, Scene::UnderlinedContext];
const LOOKS: [Look; 3] = [DARK, GITHUB_LIGHT, DEFAULT_LIGHT];
const DIM_LOOKS: [Look; 3] = [DIM_GREY, SOLARIZED_DARK, PAPER_GREY];

/// Every style but classic (whose spectrum face keeps its v0.28 bytes), on dark, GitHub Light
/// and Default Light, through typing, a burst, an Enter at the bottom row (the echo and the
/// printed line every frame after it), wide letters, a move up, a split pane's burst, the
/// gallery's context and a line wrapped onto a second row ([`BAR_SCENES`]), keeps every letter
/// it lies on at 4.5:1 or better, frame by frame, as the glass shows it: through the host's
/// bloom and SDR crown ([`replay_in`]). Judged before the
/// post passes, as it was until looks10, Lumen's typing kept `x` at 6.25:1 where the glass
/// showed 4.37 ([`the_post_passes_spare_the_letters`]). A frame whose bloom keeps off its letters by
/// a mask leaves the blur out, asserting that it could change no judged letter
/// ([`Replay::skip_spared_blur`]); with the mask emptied of a letter that assert fires here, and with
/// the composite made to ignore its mask [`the_post_passes_spare_the_letters`] does.
#[test]
fn every_letter_a_trail_lies_on_reads_at_the_bar() {
    let mut jobs = Vec::new();
    for style in STYLES.into_iter().filter(|s| *s != "classic") {
        for look in LOOKS {
            for scene in BAR_SCENES {
                jobs.push((style, look, scene));
            }
        }
    }
    let verdicts = replay_all(&jobs, true, |_, _, _| Vec::new());
    assert_eq!(verdicts.len(), jobs.len());
    let failed: Vec<String> = verdicts
        .iter()
        .filter(|v| v.under > 0)
        .map(ToString::to_string)
        .collect();
    assert!(
        failed.is_empty(),
        "letters under {BAR}:1:\n{}",
        failed.join("\n")
    );
    // Not vacuous: the scenes put the trails on letters and the judge read them, and the blurs left
    // out could reach letters, each held to its mask.
    assert!(verdicts.iter().all(|v| v.worst.is_some()));
    assert!(
        verdicts.iter().any(|v| v.spared > 0),
        "fixture: no blur left out could reach a letter, so no mask was read"
    );
}

/// looks10 L1, THE POST PASSES SPARE THE LETTERS: the host blooms and crowns the over-text
/// lights after the letter guard has weighed them (`aterm_render::bloom`), and the bloom's
/// 12 px reach carried a cut light's neighbours back over the letters it spared: on the dark
/// page Lumen's typing left `x` at 4.37:1, Phaser's Enter `q` at 4.37 and Sparkle's `x` at
/// 4.38. A frame whose guard spares the letters (`BloomGate::SparesLetters`) blooms and crowns
/// only the light laid clear of them (`bloom::spared_sources`). Negative control: these
/// replays, the post passes taking light from beside the letters too, bury a letter.
#[test]
fn the_post_passes_spare_the_letters() {
    let jobs = [
        ("lumen", DARK, Scene::Typing),
        ("phaser", DARK, Scene::EnterBottom),
        ("sparkle", DARK, Scene::EnterBottom),
    ];
    for (style, look, scene) in jobs {
        let spared = replay_spared(
            style,
            look,
            scene,
            true,
            true,
            Vec::new(),
            TrailColor::Automatic,
        );
        assert_eq!(
            spared.under, 0,
            "spared, a letter falls under the bar: {spared}"
        );
        let bloomed = replay_spared(
            style,
            look,
            scene,
            true,
            false,
            Vec::new(),
            TrailColor::Automatic,
        );
        assert!(
            bloomed.under > 0,
            "the post passes over the letters keep every letter (the control no longer \
             controls): {bloomed}"
        );
    }
}

/// Negative control: with the letter guard off, each of these replays buries a letter (Laser's
/// Enter flare across the printed line, the caret blooms on the newest letters, Water's wave, the
/// ribbon head's bloom in the burst, Fire's spark on a wide letter). The flat ribbon's Enter on
/// Default Light stood here until its paper Return came out from under the printed line instead
/// of flaring across it (looks9 Q5, `rainbow_kitty::meteor`): unguarded it now keeps every letter
/// at 7.28:1 or better, so it controls nothing.
///
/// Fire's is a defect of the host's Han face: unguarded, its spark buries macOS's `你` (2.78:1
/// and 2.58:1) and no Latin letter in any scene or look, while under Windows' Han fallback
/// (Microsoft YaHei, `msyh.ttc`) every letter of the scene reads at 6.6:1 or better (Windows 11
/// arm64, 2026-10-24), and under Linux's every letter reads at 6.06:1 or better (its worst a
/// Latin `o`; Linux arm64, 2026-11-06). So off macOS the guard bears no load for Fire, and its
/// rows would control nothing there.
///
/// Water's wave on Default Light buries a letter in its burst on the gallery's cell, macOS's SF
/// Mono ([`lays_the_gallery_row`]), and that is the scene it replays there. Where the face lays
/// another cell it replays its wrapped line: under Linux's bundled JetBrains Mono (13 x 29 px
/// cells) the burst's worst unguarded letter reads 4.51:1, at the bar, and the wrapped line's
/// `y` 3.99:1 (Linux arm64, 2026-11-06). Windows' bundled Iosevka Term SemiBold replays the
/// scene its cell picks, unmeasured on Windows since this row was keyed on the face.
#[test]
fn without_the_letter_guard_the_trails_bury_letters() {
    let water = if lays_the_gallery_row() {
        Scene::Burst
    } else {
        Scene::Wrapped
    };
    let mut jobs = vec![
        ("laser", DARK, Scene::EnterBottom),
        ("beam", DARK, Scene::Typing),
        ("phaser", DARK, Scene::Burst),
        ("comet", DARK, Scene::Typing),
        ("lumen", DARK, Scene::Typing),
        ("sparkle", DARK, Scene::Typing),
        ("water", DARK, Scene::Typing),
        ("water", DEFAULT_LIGHT, water),
        ("rainbow", DARK, Scene::Burst),
    ];
    if cfg!(target_os = "macos") {
        jobs.extend([
            ("fire", GITHUB_LIGHT, Scene::Cjk),
            ("fire", DEFAULT_LIGHT, Scene::Cjk),
        ]);
    }
    let off = replay_all(&jobs, false, |_, _, _| Vec::new());
    let held: Vec<String> = off
        .iter()
        .filter(|v| v.under == 0)
        .map(ToString::to_string)
        .collect();
    assert!(
        held.is_empty(),
        "the guard is off, yet these read at the bar (the control no longer controls):\n{}",
        held.join("\n")
    );
}

/// Classic's one-colour face in a colour the user chose (`cursor_trail_color = "#d03a8c"`) keeps
/// every letter at the bar through typing and a burst: the guard holds that face in intensity
/// only (the spectrum face keeps its v0.28 bytes). Negative control: the guard off buries a letter.
#[test]
fn classic_in_a_chosen_colour_keeps_every_letter() {
    let run = |scene: Scene, guard: bool| {
        let pink = TrailColor::Pinned(0x00D0_3A8C);
        let mut r = Replay::new("classic", DARK, pink);
        r.letters = true;
        // A chosen colour's Classic runs at the full default intensity (the
        // spectrum face's own default is the 0.7 `Replay::new` gives it).
        r.fx.set_cursor_glow(
            true,
            "classic",
            pink,
            None,
            260,
            24,
            1.0,
            0.6,
            true,
            DARK.cursor,
        );
        r.fx.set_letter_guard(guard);
        let (ev, tail) = scene.events();
        let judged = r.run(&ev, tail);
        let bar = look_bar(DARK);
        Verdict {
            style: "classic #d03a8c",
            look: DARK.name,
            scene,
            under: judged
                .iter()
                .map(|j| j.letters().iter().filter(|l| l.judged() < bar).count())
                .sum(),
            worst: worst(&judged),
            spared: 0,
        }
    };
    for scene in [Scene::Typing, Scene::Burst] {
        let v = run(scene, true);
        assert_eq!(
            v.under, 0,
            "a letter under the chosen colour reads under the bar: {v}"
        );
    }
    let off = run(Scene::Typing, false);
    assert!(
        off.under > 0,
        "the guard is off, yet the chosen colour keeps every letter (the control no longer controls): {off}"
    );
}

/// CLASSIC ON PAPER KEEPS EVERY LETTER (looks8 round 2): the ruling that spares Classic's
/// spectrum face the letter bar names dark, not paper, and its paper landing ring lays its square
/// in ink over the letters beside the caret. Through typing, an Enter at the bottom row, a burst,
/// wide letters and the trail gallery's `move-up` (the caret moved up into a program's lettered
/// lines, where the ring lands among their letters), Classic on GitHub Light and on Default Light
/// keeps every letter at the bar, frame by frame. Negative control: the letter guard passing
/// Classic's paper face by, as it did, with the paper ring laid over the letters as it was until
/// looks10 L12 (now it is whole and off the letters, or not drawn), leaves a letter beside the
/// `move-up` landing under the bar on each. Since L12 either law alone keeps those letters, so
/// each is held alone as well: the guard with the ring laid over the letters, and the ring with
/// the guard passing the face by, each keep every letter beside the `move-up` landing.
#[test]
fn classic_on_paper_keeps_every_letter() {
    let run = |look: Look, scene: Scene, guarded: bool, ring_cut: bool| {
        let mut r = Replay::new("classic", look, TrailColor::Automatic);
        r.letters = true;
        r.fx.set_letter_guard_classic_paper(guarded);
        r.fx.set_classic_paper_ring_cut(ring_cut);
        let (ev, tail) = scene.events();
        let judged = r.run(&ev, tail);
        let bar = look_bar(look);
        Verdict {
            style: "classic",
            look: look.name,
            scene,
            under: judged
                .iter()
                .map(|j| j.letters().iter().filter(|l| l.judged() < bar).count())
                .sum(),
            worst: worst(&judged),
            spared: 0,
        }
    };
    for look in [GITHUB_LIGHT, DEFAULT_LIGHT] {
        for scene in SCENES.into_iter().chain([Scene::MoveUp]) {
            let v = run(look, scene, true, false);
            assert!(
                v.worst.is_some(),
                "fixture: the judge read the letters: {v}"
            );
            assert_eq!(
                v.under, 0,
                "a letter under Classic on paper reads under the bar: {v}"
            );
        }
        let guard = run(look, Scene::MoveUp, true, true);
        assert_eq!(
            guard.under, 0,
            "the letter guard alone, the ring laid over the letters, takes a letter under the \
             bar: {guard}"
        );
        let ring = run(look, Scene::MoveUp, false, false);
        assert_eq!(
            ring.under, 0,
            "the ring alone, the guard passing the face by, takes a letter under the bar: {ring}"
        );
        let old = run(look, Scene::MoveUp, false, true);
        assert!(
            old.under > 0,
            "negative control: the guard passing Classic's paper face by, with the ring laid \
             over the letters as it was, keeps every letter (the control no longer controls): {old}"
        );
    }
}

/// THE DARK RING PASSES UNDER THE LETTERS (looks8 round 2): "Ring on jumps" lands a rim about
/// the caret, and on the dark page the rim is drawn under the text at a fifth of its light on
/// every lettered cell. Replayed with the host's post passes (the bloom and the SDR crown, which
/// spread the over-text light), the Enter at the bottom row keeps every letter at the bar from
/// the Enter on, under each style that reads the Ring knob, with the rim laid there (where
/// too little of it keeps its whole light the ring is not drawn at all, looks8 round 3, so
/// the replay lays it whatever its share to judge the letters it passes under). Negative controls: laid over the
/// text as it was, the rim crosses `echo` after the Enter (the trail gallery read its `e` at
/// 4.26:1 under Lumen) and, bloomed over the letters as the host bloomed it until looks10
/// (`BloomGate::SparesLetters` off), takes letters under the bar under Lumen, Sparkle and
/// Phaser; laid under the text at its whole light, it spends the room the guard leaves a letter
/// and the bloom of the trail's own light over the letters takes one under the bar under Lumen
/// (and under Laser until its crackle kinked, looks10). With the post passes sparing the letters each control keeps them (Lumen's `e`
/// at 4.81:1 with the rim over it, its `c` at 4.73 with the rim whole): two laws now hold
/// the bloom off these letters.
#[test]
fn the_dark_ring_passes_under_the_letters() {
    let (ev, tail) = Scene::EnterBottom.events();
    let enter = ev
        .iter()
        .find(|(_, e)| matches!(e, Ev::Key('\r', _)))
        .map_or(0, |(at, _)| *at);
    assert!(enter > 0, "fixture: the scene has its Enter");
    let run = |style: &'static str, over: bool, share: Option<f32>| {
        let mut r = Replay::new(style, DARK, TrailColor::Automatic);
        r.letters = true;
        r.post = true;
        // Each control blooms over the letters, as the host did until looks10.
        r.fx.set_bloom_spares_letters(!over && share.is_none());
        // The rim laid whatever share of it keeps its whole light: there, after an Enter
        // at the bottom row, the ring is otherwise not drawn at all (`RIM_SHOWN_SHARE`).
        r.fx.set_dark_rim_fragments(true);
        r.fx.set_dark_rim_over(over);
        if let Some(share) = share {
            r.fx.set_dark_rim_under_letters(share);
        }
        let judged: Vec<Judged> = r
            .run(&ev, tail)
            .into_iter()
            .filter(|j| j.ms >= enter)
            .collect();
        let bar = look_bar(DARK);
        Verdict {
            style,
            look: DARK.name,
            scene: Scene::EnterBottom,
            under: judged
                .iter()
                .map(|j| j.letters().iter().filter(|l| l.judged() < bar).count())
                .sum(),
            worst: worst(&judged),
            spared: 0,
        }
    };
    for style in ["lumen", "comet", "sparkle", "phaser", "laser"] {
        let v = run(style, false, None);
        assert!(
            v.worst.is_some(),
            "fixture: the judge read the letters: {v}"
        );
        assert_eq!(
            v.under, 0,
            "a letter after the Enter reads under the bar: {v}"
        );
    }
    for style in ["lumen", "sparkle", "phaser"] {
        let old = run(style, true, None);
        assert!(
            old.under > 0,
            "negative control: the rim laid over the text keeps every letter (the control no \
             longer controls): {old}"
        );
    }
    // Lumen's rim controls. Laser's did until looks10, whose crackle kinks
    // (`spawn_bolt`) and draws its sparks from another stretch of the trail's
    // random stream: its worst letter there reads 4.80 at the whole light.
    let whole = run("lumen", false, Some(1.0));
    assert!(
        whole.under > 0,
        "negative control: the rim under the text at its whole light keeps every letter (the \
         control no longer controls): {whole}"
    );
}

/// looks7 WIDEN, THE BLOOM ON DIM TEXT: the GPU blurs the over-text light as far as 12 px past
/// every cut the guard made on a letter (`aterm_render::bloom`), and a dim theme's letters,
/// keeping only their own reading, have no room for it: in the gallery Solarized Dark's newest
/// letter read 3.47:1 under Comet's bloom (4.10 without it) and 3.79 under the plain caret's
/// typing glow (4.01). Replayed with the host's bloom, eight styles typing and bursting on
/// Solarized Dark and the dim grey keep every letter at [`look_bar`] (3.90:1 at worst on
/// Solarized, Water's burst), each frame's text holding the bloom
/// (`RenderInput::bloom_gate`); the dark page's text keeps it, its frames blooming as before.
/// Negative control: the bloom drawn over the held text buries Comet's letters in its burst
/// (3.03:1 on Solarized, 3.25 on the grey).
#[test]
fn a_dim_theme_s_letters_take_no_bloom() {
    let worst = |style: &'static str, look: Look, scene: Scene, bloom: Option<bool>| {
        let (ev, tail) = scene.events();
        let mut r = Replay::new(style, look, TrailColor::Automatic);
        r.letters = true;
        r.bloom = bloom;
        r.run(&ev, tail)
            .iter()
            .filter_map(Judged::worst)
            .map(|l| l.judged())
            .fold(f64::INFINITY, f64::min)
    };
    let mut jobs = Vec::new();
    for look in [SOLARIZED_DARK, DIM_GREY] {
        for style in [
            "comet", "beam", "laser", "sparkle", "lumen", "phaser", "water", "fire",
        ] {
            for scene in [Scene::Typing, Scene::Burst] {
                jobs.push((style, look, scene));
            }
        }
    }
    let lanes = aterm_cores::available_parallelism().map_or(4, usize::from);
    let worst = &worst;
    let verdicts: Vec<(&str, Look, Scene, f64)> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(lanes).max(1))
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|&(style, look, scene)| {
                            (style, look, scene, worst(style, look, scene, Some(false)))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a replay lane"))
            .collect()
    });
    let under: Vec<String> = verdicts
        .iter()
        .filter(|v| v.3 < look_bar(v.1))
        .map(|v| format!("{} on {} {:?}: {:.2}:1", v.0, v.1.name, v.2, v.3))
        .collect();
    assert!(
        under.is_empty(),
        "letters under the bar with the host's bloom:\n{}",
        under.join("\n")
    );
    for look in [SOLARIZED_DARK, DIM_GREY] {
        let unheld = worst("comet", look, Scene::Burst, Some(true));
        assert!(
            unheld < look_bar(look),
            "control: blooming over its held text, Comet keeps {} at {unheld:.2}:1",
            look.name
        );
    }
    // The dark page's text keeps its bloom: held or not, its frames bloom alike.
    let dark = [None, Some(false), Some(true)].map(|b| worst("comet", DARK, Scene::Typing, b));
    assert!(
        dark[1] == dark[2] && dark[1] < dark[0],
        "the dark page's bloom (none, honouring the hold, over it): {dark:?}"
    );
}

/// A replay's dim letter keeping the least share of its own reading.
#[derive(Clone, Copy, Debug)]
struct OwnShare {
    share: f64,
    ms: u64,
    ch: char,
    read: f64,
    own: f64,
}

/// looks7 WIDEN, A DIM LETTER'S OWN READING: on a dim theme a letter whose own reading is
/// under the guard's floor keeps 97% of it under the lights (`OWN_SHARE`), judged as the
/// gallery judges it, its clean twin by the same judge. Every style but Classic (whose spectrum
/// face is ruled), through every scene ([`BAR_SCENES`], the line wrapped past the margin too), on
/// Solarized Dark and the dim grey, through the host's post passes (every frame here holds the
/// bloom, so the crown alone lays light), keeps every such letter at 97% of its own reading: the
/// guard reads each letter as the judge does, by the ring round its strokes
/// from the glyph's own raster (`letter_guard::ring`, round 27), and reads it again on the lights
/// its cuts leave (a neighbour's feather, the night grounds laid after it).
///
/// looks7 round 2: a bed laid at the page's own luminance in its own hue reads no darker
/// than the page by luminance, yet where an edge of it crosses a letter the judge's ring takes
/// each channel's median of page and bed, brighter than either. And a cut light lays no more
/// than the share the guard weighed: rounded to the nearest byte, six of an Enter tube's bed
/// slabs cut to an eighth each laid a byte of light and lit the page under `fox` to `#00313c`.
/// The ledger truncates its cut pieces (`guard_mask_quads` and its tests); the guard's second
/// reading reads the cut streams as they are laid, a byte a rounded piece would lay included,
/// so no scene of this law tells rounded cuts from truncated ones any more (round 27's review:
/// none of them leaves a dim letter under 97% with its cuts rounded).
///
/// The same frames hold a dim theme's bar. These looks' solid ink reads under the guard's floor on
/// the bare page (a guard that gave up on it would leave every light whole), yet every style keeps
/// each letter it lies on at four fifths of the theme's own contrast ([`look_bar`]; in
/// [`Scene::Context`] at 97% of its own reading where that is lower), and every style lays light on
/// some letter it is judged by. Neither law implies the other: 97% of a letter reading 4.5:1 of its
/// own lies over the grey's bar of 4.0, and 97% of one reading under that bar lies under it.
///
/// The sweep judges an underlined line too ([`UNDERLINED_SCENES`], the underlined letters' review,
/// 2026-11-06, measured on Linux arm64 in the bundled JetBrains Mono's 13 x 29 px cells). On
/// Solarized Dark the underline cat laid a wash at the page's lightness in its own hue under
/// the stroke, and the judge's channel medians read `h` at 4.14:1 of its own 4.58 (90.5%) in
/// both lines where the guard, reading the stroke's grounds by luminance, saw no lift. Over the
/// selected row every style but Fire and Laser laid light in the typed row's foot band against
/// the stroke, its rails parted round the row's descenders alone, on both dim looks: the
/// rainbow kitty pet's `h` at 1.08:1.
///
/// Negative controls: read by its solid ink against the least and greatest luminance of its grounds
/// (`set_letter_guard_ring(false)` and `set_letter_guard_channels(false)`, the guard before round
/// 27), Fire's wrapped line on the dim grey keeps a dim letter under 97% of its own reading, where
/// the ring's channel medians read the light round a stroke apart from its grounds' luminance: at
/// the gallery's size where the window's face lays the gallery's row there
/// ([`lays_the_gallery_row`]), Fire's `c` at 94.9% and Lumen's `o` on its Enter at the bottom at
/// 96.2% under macOS's SF Mono (round 27 named the nyan rainbow's `p` on Solarized Dark's wrapped
/// line at 96.6%, which, with a letter at AA kept at AA, no longer falls under 97% with the ring
/// off: after-land round 3); and at [`RING_CONTROL_PX`] where it does not. Under Linux's bundled
/// JetBrains Mono at the size that lays the gallery's row (20 px, 12 x 26 px cells) Laser's `w` on
/// Solarized Dark's Enter read 96.4% before after-land round 3 joined main; now no line of
/// [`BAR_SCENES`] on either dim look reads a dim letter under 97% there by its grounds' luminance
/// (the least, Lumen's `o` on the dim grey's Enter, 98.25%). At 19 px (11 x 25 px cells) Fire's
/// wrapped line on the dim grey reads `o` at 96.4% of its own 4.577, the ring off alone 96.4% and
/// the channels' luminance alone 98.5% (Linux arm64, 2026-11-07). Read by their rings, Fire's and
/// the nyan rainbow's replays keep the law at that size too. At 22 px that face's 13 x 29 px cells
/// leave the ring no load: read by its grounds' luminance, every dim letter of every line of
/// [`BAR_SCENES`] keeps 98.2% of its own or more (the least, Laser's `w` on Solarized Dark's Enter,
/// 98.28%, Linux arm64, 2026-11-07). Windows' bundled Iosevka Term SemiBold replays the scene at
/// [`RING_CONTROL_PX`], unmeasured on Windows. The underlined
/// line's, on Solarized Dark in [`JETBRAINS_MONO`] at the gallery's size on every OS: read by its
/// grounds' luminance (`set_letter_guard_channels(false)`), the underline cat's line keeps `h` at
/// 90.5% of its own reading; the rails parted round no underline's stroke
/// (`set_feet_deep_unread(true)`), the rainbow kitty pet's line over the selected row leaves `h`
/// at 1.08:1 of its own 4.58 (both Linux arm64, 2026-11-06, as in the window's own face there).
/// Another cell lays the stroke elsewhere: Iosevka Term SemiBold at 22 px, replayed here, bites
/// neither, so both stay in the face they were measured in, unmeasured on macOS and Windows. A
/// rail's lane past a foot read by its glyph's raster alone (`set_lanes_deep_unread(true)`, the
/// cursor-trail tree as it first met the deep feet) runs Phaser's rail at its whole light against
/// the stroke over the selected row, in that face on Solarized Dark: 281 letters under the bar,
/// `c` at 1.13:1 (macOS, 2026-11-06; in the window's SF Mono 1.11:1, and Comet's, Sparkle's,
/// Lumen's and Beam's lines under the bar with it on both dim looks), unmeasured on Linux and
/// Windows. And the guard off buries a letter under the bar in each of four replays.
/// A letter whose glyph the face's table holds no raster for is held by
/// [`a_dim_letter_with_no_raster_keeps_its_own_reading`].
#[test]
fn a_dim_letter_keeps_its_own_reading() {
    #[derive(Clone, Copy)]
    struct Knobs {
        ring: bool,
        channels: bool,
        feet_deep: bool,
        lanes_deep: bool,
        px: f32,
        /// The face's bytes, the window's own face where `None`.
        face: Option<&'static [u8]>,
    }
    const SHIPPED: Knobs = Knobs {
        ring: true,
        channels: true,
        feet_deep: true,
        lanes_deep: true,
        px: GALLERY_PX,
        face: None,
    };
    let least = |style: &'static str, look: Look, scene: Scene, k: Knobs| {
        let (ev, tail) = scene.events_on(look);
        let mut r = Replay::sized_at(style, look, TrailColor::Automatic, scene.rows(), k.px);
        if let Some(bytes) = k.face {
            r.rend = Renderer::from_bytes(bytes, k.px, Theme::default()).expect("the face");
            r.cell = r.rend.cell_size();
        }
        r.letters = true;
        r.fx.set_letter_guard_ring(k.ring);
        r.fx.cursor_mut().glow.set_letter_guard_channels(k.channels);
        r.fx.cursor_mut().glow.set_feet_deep_unread(!k.feet_deep);
        r.fx.cursor_mut().glow.set_lanes_deep_unread(!k.lanes_deep);
        r.own = true;
        r.post = true;
        let judged = r.run(&ev, tail);
        let bar = look_bar(look);
        let verdict = Verdict {
            style,
            look: look.name,
            scene,
            under: judged
                .iter()
                .map(|j| {
                    j.letters()
                        .iter()
                        .filter(|l| {
                            l.judged()
                                < if scene.on_context() {
                                    bar.min(OWN_SHARE * l.own)
                                } else {
                                    bar
                                }
                        })
                        .count()
                })
                .sum(),
            worst: worst(&judged),
            spared: 0,
        };
        let mut least = OwnShare {
            share: f64::INFINITY,
            ms: 0,
            ch: ' ',
            read: 0.0,
            own: 0.0,
        };
        for j in &judged {
            for l in j.letters() {
                if l.own >= 4.6 || l.judged() >= BAR {
                    continue;
                }
                let share = (l.judged() + 0.005) / l.own;
                if share < least.share {
                    least = OwnShare {
                        share,
                        ms: j.ms,
                        ch: l.ch,
                        read: l.judged(),
                        own: l.own,
                    };
                }
            }
        }
        (verdict, least)
    };
    let lanes = aterm_cores::available_parallelism().map_or(4, usize::from);
    let least = &least;
    let sweep = |jobs: &[(&'static str, Look, Scene)], k: Knobs| -> Vec<(Verdict, OwnShare)> {
        std::thread::scope(|s| {
            let handles: Vec<_> = jobs
                .chunks(jobs.len().div_ceil(lanes).max(1))
                .map(|chunk| {
                    s.spawn(move || {
                        chunk
                            .iter()
                            .map(|&(style, look, scene)| least(style, look, scene, k))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().expect("a replay lane"))
                .collect()
        })
    };
    let under = |swept: &[(Verdict, OwnShare)]| -> Vec<String> {
        swept
            .iter()
            .filter(|(_, l)| l.share < 0.97)
            .map(|(v, l)| {
                format!(
                    "{} on {} {:?} at {} ms: `{}` {:.2}:1 of its own {:.2} ({:.1}%)",
                    v.style,
                    v.look,
                    v.scene,
                    l.ms,
                    l.ch,
                    l.read,
                    l.own,
                    100.0 * l.share
                )
            })
            .collect()
    };
    let mut jobs = Vec::new();
    for look in [SOLARIZED_DARK, DIM_GREY] {
        for style in STYLES.iter().copied().filter(|s| *s != "classic") {
            for scene in BAR_SCENES.into_iter().chain(UNDERLINED_SCENES) {
                jobs.push((style, look, scene));
            }
        }
    }
    let swept = sweep(&jobs, SHIPPED);
    assert_eq!(swept.len(), jobs.len());
    let failed: Vec<String> = swept
        .iter()
        .filter(|(v, _)| v.under > 0)
        .map(|(v, _)| v.to_string())
        .collect();
    assert!(
        failed.is_empty(),
        "letters under four fifths of their theme's contrast:\n{}",
        failed.join("\n")
    );
    // Every style lays light on some letter it is judged by (a scene may lay
    // none: the guard cuts Sparkle's stars off a dim theme's letters whole).
    let unlit: Vec<&str> = STYLES
        .into_iter()
        .filter(|s| *s != "classic")
        .filter(|s| {
            !swept
                .iter()
                .any(|(v, _)| v.style == *s && v.worst.is_some())
        })
        .collect();
    assert!(
        unlit.is_empty(),
        "styles whose lights reach no letter on a dim theme (the law judges nothing): {unlit:?}"
    );
    let fell = under(&swept);
    assert!(
        fell.is_empty(),
        "dim letters under 97% of their own reading:\n{}",
        fell.join("\n")
    );
    // The ring's control: Fire's wrapped line on the dim grey, at the gallery's size where the
    // face lays the gallery's row there (macOS's measured cell), and at RING_CONTROL_PX where it
    // does not (Linux's measured cell). The nyan rainbow's wrapped line, round 27's control, is
    // read by its ring at that size too.
    let fire = ("fire", DIM_GREY, Scene::Wrapped);
    let nyan = ("rainbow", SOLARIZED_DARK, Scene::Wrapped);
    let px = if lays_the_gallery_row() {
        GALLERY_PX
    } else {
        RING_CONTROL_PX
    };
    if px != GALLERY_PX {
        let ringed = sweep(&[fire, nyan], Knobs { px, ..SHIPPED });
        let failed: Vec<String> = ringed
            .iter()
            .filter(|(v, _)| v.under > 0)
            .map(|(v, _)| v.to_string())
            .chain(under(&ringed))
            .collect();
        assert!(
            failed.is_empty(),
            "the controls' replays at {px} px, read by their rings:\n{}",
            failed.join("\n")
        );
    }
    // Windows' own face (Iosevka Term NL) lays thinner letters that keep their reading under
    // this control at either size, so it replays in the face the control was measured in.
    let luminance = Knobs {
        ring: false,
        channels: false,
        px,
        face: cfg!(windows).then_some(JETBRAINS_MONO),
        ..SHIPPED
    };
    assert!(
        !under(&sweep(&[fire], luminance)).is_empty(),
        "control: read by its grounds' luminance at {px} px, {fire:?} still keeps 97% of every \
         dim letter's own reading"
    );
    // The underlined lines' controls, in the face and cell they were measured in: a letter read
    // by its solid ink against its grounds' least and greatest luminance, and the rails parting
    // round no underline's stroke.
    let underlined = ("underline rainbow + cat", SOLARIZED_DARK, Scene::Underlined);
    let luma = Knobs {
        channels: false,
        face: Some(JETBRAINS_MONO),
        ..SHIPPED
    };
    assert!(
        !under(&sweep(&[underlined], luma)).is_empty(),
        "control: read by its grounds' luminance, {underlined:?} still keeps 97% of every dim \
         letter's own reading"
    );
    let over_selection = ("rainbow + cat", SOLARIZED_DARK, Scene::UnderlinedContext);
    let shallow = Knobs {
        feet_deep: false,
        face: Some(JETBRAINS_MONO),
        ..SHIPPED
    };
    assert!(
        sweep(&[over_selection], shallow)
            .iter()
            .any(|(v, _)| v.under > 0),
        "control: the rails parting round no underline, {over_selection:?} still keeps every \
         letter"
    );
    let phaser_over_selection = ("phaser", SOLARIZED_DARK, Scene::UnderlinedContext);
    let glyph_lanes = Knobs {
        lanes_deep: false,
        face: Some(JETBRAINS_MONO),
        ..SHIPPED
    };
    assert!(
        sweep(&[phaser_over_selection], glyph_lanes)
            .iter()
            .any(|(v, _)| v.under > 0),
        "control: the rails' lanes past a foot read by its glyph alone, \
         {phaser_over_selection:?} still keeps every letter"
    );
    let controls = [
        ("sparkle", DIM_GREY, Scene::Typing),
        ("rainbow + cat", DIM_GREY, Scene::Burst),
        ("laser", SOLARIZED_DARK, Scene::EnterBottom),
        ("water", SOLARIZED_DARK, Scene::Typing),
    ];
    let off = replay_all(&controls, false, |_, _, _| Vec::new());
    let held: Vec<String> = off
        .iter()
        .filter(|v| v.under == 0)
        .map(ToString::to_string)
        .collect();
    assert!(
        held.is_empty(),
        "the guard is off, yet these keep their reading (the control no longer controls):\n{}",
        held.join("\n")
    );
}

/// r27 LG1 review, A LETTER WITH NO RASTER KEEPS ITS OWN READING: the face's glyph table holds
/// the printable ASCII glyphs' rasters, so the guard reads an accented, Cyrillic or Greek letter
/// by its solid ink against its grounds' least and greatest (by luminance then, by each channel
/// since 2026-11-06), not by the ring round its strokes the judge reads, and keeps the floor's
/// room over its share there (`OWN_ROOM`). Every style but classic, typing and bursting such
/// letters on Solarized Dark and the dim grey, the host's bloom honoured, keeps every letter
/// whose own reading is under the guard's floor at 97% of it, and so does the control's replay
/// where the window's face lays the gallery's row ([`gallery_row_px`]). Negative control: with
/// no room over a letter, Fire typing them on Solarized Dark at that size leaves a letter under
/// 97% of its own reading. Where the face lays the gallery's row, a letter read too by its solid
/// ink against its grounds' luminance (`set_letter_guard_room(false)` and
/// `set_letter_guard_channels(false)`, the round's first fix): `м` at 95.7% under macOS's SF
/// Mono. Where it lays another, the room alone off (`set_letter_guard_room(false)`), each
/// channel's grounds read as shipped: `р` at 96.1% under Linux's bundled JetBrains Mono at 20 px
/// (97.3% with the room; 96.1% too read by luminance, Linux arm64, 2026-11-06). At 22 px that
/// face's 13 x 29 px cells left the room no load, its grounds read by luminance: no replay's
/// least share fell by more than a hundredth of a percent with it off.
#[test]
fn a_dim_letter_with_no_raster_keeps_its_own_reading() {
    const TEXT: &str = "echo привет мир λόγος ζωή über straße";
    // `room` and `channels` false replay the round's first fix: no room, the grounds read by
    // luminance.
    let least =
        |style: &'static str, look: Look, every: u64, px: f32, room: bool, channels: bool| {
            let mut r = Replay::sized_at(style, look, TrailColor::Automatic, ROWS, px);
            r.letters = true;
            r.own = true;
            r.bloom = Some(false);
            r.fx.set_letter_guard_room(room);
            r.fx.cursor_mut().glow.set_letter_guard_channels(channels);
            let mut ev = vec![(200, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()))];
            ev.extend(typed(TEXT, 1_000, every));
            let mut least = OwnShare {
                share: f64::INFINITY,
                ms: 0,
                ch: ' ',
                read: 0.0,
                own: 0.0,
            };
            for j in r.run(&ev, 600) {
                for l in j.letters() {
                    if l.own >= 4.6 || l.judged() >= BAR {
                        continue;
                    }
                    let share = (l.judged() + 0.005) / l.own;
                    if share < least.share {
                        least = OwnShare {
                            share,
                            ms: j.ms,
                            ch: l.ch,
                            read: l.judged(),
                            own: l.own,
                        };
                    }
                }
            }
            (style, look, every, px, least)
        };
    let mut jobs = Vec::new();
    for look in [SOLARIZED_DARK, DIM_GREY] {
        for style in STYLES.into_iter().filter(|s| *s != "classic") {
            for every in [60, 30] {
                jobs.push((style, look, every, GALLERY_PX));
            }
        }
    }
    // The control's replay with the room, where the window's face lays the gallery's row.
    let control = ("fire", SOLARIZED_DARK, 60, gallery_row_px());
    if control.3 != GALLERY_PX {
        jobs.push(control);
    }
    let lanes = aterm_cores::available_parallelism().map_or(4, usize::from);
    let least = &least;
    let verdicts: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(lanes).max(1))
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|&(style, look, every, px)| least(style, look, every, px, true, true))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a replay lane"))
            .collect()
    });
    let fell: Vec<String> = verdicts
        .iter()
        .filter(|(_, _, _, _, l)| l.share < 0.97)
        .map(|(style, look, every, px, l)| {
            format!(
                "{style} on {} at {px} px a key every {every} ms, at {} ms: `{}` {:.2}:1 of its \
                 own {:.2} ({:.1}%)",
                look.name,
                l.ms,
                l.ch,
                l.read,
                l.own,
                100.0 * l.share
            )
        })
        .collect();
    assert!(
        fell.is_empty(),
        "dim letters with no raster under 97% of their own reading:\n{}",
        fell.join("\n")
    );
    assert!(
        verdicts.iter().all(|(_, _, _, _, l)| l.share.is_finite()),
        "fixture: every replay's lights lie on a dim letter"
    );
    // The control's grounds: by luminance on the gallery's cell (macOS's measured one), by each
    // channel as shipped where the face lays another, so the room is its only change there.
    let (style, look, every, px) = control;
    let channels = !lays_the_gallery_row();
    let (_, _, _, _, bare) = least(style, look, every, px, false, channels);
    assert!(
        bare.share < 0.97,
        "control: with no room over a letter (its grounds read by each channel: {channels}), \
         Fire typing on Solarized Dark at {px} px still keeps `{}` at {:.1}% of its own reading",
        bare.ch,
        100.0 * bare.share
    );
}

/// A page some of whose text reads between AA and the guard's floor: the flood's magenta a step
/// darker than its `poppy`'s (`#c04082`, whose `pp` read 4.56 of their own in the gallery and 3.9
/// to 4.4 at this size) on Default Light's page, where its `h` reads 4.58.
const PAPER_AT_AA: Look = Look {
    name: "paper-at-aa",
    fg: 0x00B9_417A,
    bg: 0x00F6_F7F9,
    cursor: 0x001A_7F37,
};

/// After-land round 3 (W8), A LETTER AT AA KEEPS AA: the legibility law's bar is 4.5, so a letter
/// that reads 4.5 or more on its bare page reads 4.5 or more under every light, and only a letter
/// under it keeps its share of its own (`OWN_SHARE`). The guard held a letter under its floor at
/// 97% of its reading, so the flood's `pp` on Default Light, 4.56 of its own, read 4.49 under
/// Comet's tail. Every style (Classic's spectrum face is ruled on dark pages only), typing, a burst
/// and an Enter at the bottom row on a paper page whose text reads between AA and the floor, and
/// every style but Classic on Solarized Dark, through the host's post passes: every letter whose
/// own reading lies between AA and the floor reads AA or more on every frame. Negative control
/// (`with_share_over_aa`): such a letter keeping 97% of its reading, some style buries one under
/// AA on each page.
///
/// Beam's body on Solarized Dark is a page shade (`InkLedger::note_page_shade_laid`), which the
/// guard holds to the letters it reaches by itself (`CursorGlow::hold_page_shades`). Kept whole, a
/// ring lying on it and on the page read, by each channel's median (the upper one of an even
/// count), a ground brighter than either: on Linux's face Beam's `w`, 4.509 on its page, read 4.488
/// typing and at an Enter on the bottom row (Linux arm64, 2026-11-07), and on the Mac's its burst's
/// `d` (row 4, column 47), 4.5086 on its page, read 4.4900 and 4.4963 at 3584 and 3600 ms, the two
/// letters this law left Beam's burst there before the hold was read on a Mac (macOS, 2026-11-11).
/// Held, the `d` reads its own 4.5086. Negative control (`set_letter_guard_shades`): each page shade
/// kept whole, Beam on Solarized Dark reads more letters at AA under AA over its three scenes than
/// held where the face is one the hold was measured on: 4 against none on Linux's
/// ([`lays_the_bundled_cell`]), 2 against none on the Mac's ([`lays_the_gallery_row`]), and on
/// another face at least as many.
#[test]
fn a_letter_at_aa_keeps_aa() {
    const SCENES: [Scene; 3] = [Scene::Typing, Scene::Burst, Scene::EnterBottom];
    // (letters read at AA or over and under the floor, of those the ones under AA, the least),
    // each page shade held by the guard where `shades`.
    let read = |style: &'static str, look: Look, scene: Scene, share: bool, shades: bool| {
        let run = || {
            let (ev, tail) = scene.events_on(look);
            let mut r = Replay::sized(style, look, TrailColor::Automatic, scene.rows());
            r.fx.set_letter_guard_shades(shades);
            r.letters = true;
            r.own = true;
            r.post = true;
            r.run(&ev, tail)
        };
        let judged = if share {
            aterm_effects::cursor_glow::with_share_over_aa(run)
        } else {
            run()
        };
        let (mut at, mut under, mut least) = (0usize, 0usize, (f64::INFINITY, 0u64, ' '));
        for j in &judged {
            for l in j.letters() {
                if (BAR..4.6).contains(&l.own) {
                    at += 1;
                    under += usize::from(l.judged() < BAR);
                    if l.judged() < least.0 {
                        least = (l.judged(), j.ms, l.ch);
                    }
                }
            }
        }
        (style, look.name, scene, at, under, least)
    };
    let sweep = |jobs: &[(&'static str, Look, Scene)], share: bool| {
        let lanes = aterm_cores::available_parallelism().map_or(4, usize::from);
        let read = &read;
        std::thread::scope(|s| {
            let handles: Vec<_> = jobs
                .chunks(jobs.len().div_ceil(lanes).max(1))
                .map(|chunk| {
                    s.spawn(move || {
                        chunk
                            .iter()
                            .map(|&(style, look, scene)| read(style, look, scene, share, true))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().expect("a replay lane"))
                .collect::<Vec<_>>()
        })
    };
    let mut jobs = Vec::new();
    for look in [PAPER_AT_AA, SOLARIZED_DARK] {
        for style in STYLES
            .into_iter()
            .filter(|&s| look.bg == PAPER_AT_AA.bg || s != "classic")
        {
            for scene in SCENES {
                jobs.push((style, look, scene));
            }
        }
    }
    let swept = sweep(&jobs, false);
    assert_eq!(swept.len(), jobs.len());
    for look in [PAPER_AT_AA, SOLARIZED_DARK] {
        let at: usize = swept.iter().filter(|v| v.1 == look.name).map(|v| v.3).sum();
        assert!(
            at > 0,
            "premise: no letter on {} reads between AA and the floor",
            look.name
        );
    }
    let fell: Vec<String> = swept
        .iter()
        .filter(|v| v.4 > 0)
        .map(|(style, look, scene, at, under, (read, ms, ch))| {
            format!(
                "{style} on {look} {scene:?}: {under} of {at} under AA, `{ch}` {read:.3} at {ms} ms"
            )
        })
        .collect();
    assert!(
        fell.is_empty(),
        "letters at AA read under it:\n{}",
        fell.join("\n")
    );
    let held: usize = swept
        .iter()
        .filter(|v| (v.0, v.1) == ("beam", SOLARIZED_DARK.name))
        .map(|v| v.4)
        .sum();
    let whole: usize = SCENES
        .into_iter()
        .map(|scene| read("beam", SOLARIZED_DARK, scene, false, false).4)
        .sum();
    if lays_the_bundled_cell() || lays_the_gallery_row() {
        assert!(
            whole > held,
            "control: each page shade kept whole, Beam on Solarized Dark reads {whole} letters at \
             AA under AA over its scenes, held {held}"
        );
    } else {
        assert!(
            whole >= held,
            "control: each page shade kept whole, Beam on Solarized Dark reads {whole} letters at \
             AA under AA over its scenes, fewer than held ({held})"
        );
    }
    let control = sweep(&jobs, true);
    for look in [PAPER_AT_AA, SOLARIZED_DARK] {
        assert!(
            control.iter().any(|v| v.1 == look.name && v.4 > 0),
            "control: at 97% of its own reading every letter at AA on {} still reads AA",
            look.name
        );
    }
}

/// How long past a burst's last key [`the_cursor_trail_s_fill_is_a_letter_s_ground`] judges its fill:
/// its last cell is drawn 788 ms past that key, and the last reading it changes lies 772 ms past.
const FILL_GONE_MS: u64 = 1_000;

/// r27 LG1, THE CURSOR TRAIL'S FILL IS A LETTER'S GROUND: the classic opaque comet trail
/// (`cursor_trail`) fills each of its cells with its colour at its coverage, under every light,
/// and the clean frame draws none of it, so a letter's ring reads the frame's lights over that
/// fill. Read on its page's ground, the guard passed lights the fill had already spent the room
/// for: Water on dark, its trail on, left `o` at 3.98:1 typing (main, round 27). Every style but
/// classic with the trail on, typing and bursting on dark, GitHub Light and Solarized Dark, keeps
/// every letter at its bar ([`look_bar`], and 97% of its own reading where that is lower) through
/// the host's post passes. A burst is judged until [`FILL_GONE_MS`] past its last key: the fill is
/// gone by then (each replay asserts it), and the frames past it are the frames with no trail at all,
/// which [`every_letter_a_trail_lies_on_reads_at_the_bar`] judges on dark and GitHub Light and
/// [`a_dim_letter_keeps_its_own_reading`] on Solarized Dark, at their whole tails. Negative control:
/// the guard blind to the fill (`set_letter_guard_trail(false)`) leaves Water's typing letters under
/// it on dark.
#[test]
fn the_cursor_trail_s_fill_is_a_letter_s_ground() {
    let run = |style: &'static str, look: Look, scene: Scene, seen: bool| {
        let mut r = Replay::sized(style, look, TrailColor::Automatic, scene.rows());
        r.letters = true;
        r.post = true;
        r.own = true;
        r.skip_spared_blur = true;
        r.fx.set_cursor_trail(true, 260, 24, None, look.cursor);
        r.fx.set_letter_guard_trail(seen);
        let (ev, tail) = scene.events_on(look);
        let judged = r.run(&ev, tail.min(FILL_GONE_MS));
        assert!(
            tail <= FILL_GONE_MS || judged.last().is_some_and(|j| j.trail == 0),
            "{style} on {} {scene:?}: the trail's fill outlives the {FILL_GONE_MS} ms this law \
             judges of it",
            look.name
        );
        let bar = look_bar(look);
        Verdict {
            style,
            look: look.name,
            scene,
            under: judged
                .iter()
                .map(|j| {
                    j.letters()
                        .iter()
                        .filter(|l| l.judged() < bar.min(OWN_SHARE * l.own))
                        .count()
                })
                .sum(),
            worst: worst(&judged),
            spared: r.spared,
        }
    };
    let mut jobs = Vec::new();
    for style in STYLES.into_iter().filter(|s| *s != "classic") {
        for look in [DARK, GITHUB_LIGHT, SOLARIZED_DARK] {
            for scene in [Scene::Typing, Scene::Burst] {
                jobs.push((style, look, scene));
            }
        }
    }
    let lanes = aterm_cores::available_parallelism().map_or(4, usize::from);
    let run = &run;
    let verdicts: Vec<Verdict> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(lanes).max(1))
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|&(style, look, scene)| run(style, look, scene, true))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a replay lane"))
            .collect()
    });
    let failed: Vec<String> = verdicts
        .iter()
        .filter(|v| v.under > 0)
        .map(ToString::to_string)
        .collect();
    assert!(
        failed.is_empty(),
        "letters under their bar with the cursor trail on:\n{}",
        failed.join("\n")
    );
    assert!(
        verdicts.iter().all(|v| v.worst.is_some()),
        "fixture: every replay's lights lie on letters"
    );
    assert!(
        verdicts.iter().any(|v| v.spared > 0),
        "fixture: no blur left out could reach a letter, so no mask was read"
    );
    let blind = run("water", DARK, Scene::Typing, false);
    assert!(
        blind.under > 0,
        "control: blind to the trail's fill, the guard still keeps every letter: {blind}"
    );
}

/// r25 K8-2, FIRE'S FLAME FOOT CLEARS THE JUDGE'S RING: under a letter with no descender a kept
/// flame was held to the letter's bar down to the baseline and eased off from there, so its foot
/// and the night ground it lays lit the ring the judge reads a pixel or two under the letter's
/// ink. On the line typed past the margin ([`Scene::Wrapped`]) the `m` after the wrap read 92.0%
/// of its own reading on Solarized Dark. Held past the ring before it eases off
/// (`CursorGlow::fire_low_top`), every dim letter Fire lies on there keeps 97% of its own
/// reading, the host's bloom honoured, as [`a_dim_letter_keeps_its_own_reading`] judges them.
#[test]
fn fire_s_flame_foot_keeps_a_dim_letter_s_reading_past_the_wrap() {
    let (ev, tail) = Scene::Wrapped.events();
    let mut r = Replay::new("fire", SOLARIZED_DARK, TrailColor::Automatic);
    r.letters = true;
    r.own = true;
    r.bloom = Some(false);
    let mut least: Option<(f64, u64, char)> = None;
    for j in r.run(&ev, tail) {
        for l in j.letters() {
            if l.own >= 4.6 || l.judged() >= BAR {
                continue;
            }
            let share = (l.judged() + 0.005) / l.own;
            if least.is_none_or(|(s, ..)| share < s) {
                least = Some((share, j.ms, l.ch));
            }
        }
    }
    let (share, ms, ch) = least.expect("fixture: Fire lies on no dim letter");
    assert!(
        share >= 0.97,
        "at {ms} ms Fire leaves `{ch}` at {:.1}% of its own reading",
        100.0 * share
    );
}

/// looks7 round 2, A DIM PAGE'S TRAILS LAY NO SHADE: on Solarized Dark (its text 4.75:1 on
/// `#002b36`, every letter keeping 97% of its own reading) the streak styles typing and
/// entering at the bottom draw no pixel of page under three fifths of the page's luminance:
/// under the letters each bed lays its hue at the page's own luminance, and in the air its
/// light. Negative control: held to the letters' budget alone, the beds solve to black and
/// lay the gallery's black smoke (Beam's tube, Lumen's and Laser's tracers, Phaser's bar).
#[test]
fn a_dim_page_s_trails_lay_no_shade() {
    let shaded = |style: &'static str, scene: Scene, lit: bool| -> usize {
        let (ev, tail) = scene.events();
        let mut r = Replay::new(style, SOLARIZED_DARK, TrailColor::Automatic);
        r.fx.set_bed_page_lit(lit);
        r.shade = true;
        r.run(&ev, tail).iter().map(|j| j.shade).sum()
    };
    let styles = ["beam", "lumen", "laser", "comet", "phaser"];
    let mut jobs = Vec::new();
    for style in styles {
        for scene in [Scene::Typing, Scene::EnterBottom] {
            jobs.push((style, scene));
        }
    }
    let lit: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|&(style, scene)| s.spawn(move || (style, scene, shaded(style, scene, true))))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a replay"))
            .filter(|&(_, _, n)| n > 0)
            .map(|(style, scene, n)| format!("{style} {scene:?}: {n} px of shade"))
            .collect()
    });
    assert!(
        lit.is_empty(),
        "shade on Solarized Dark:\n{}",
        lit.join("\n")
    );
    for style in styles {
        assert!(
            shaded(style, Scene::EnterBottom, false) > 0,
            "control: {style}'s bed held to its budget lays no shade on Solarized Dark"
        );
    }
}

/// The after-land lows' W6 under the guard's ring model, BEAM'S BODY ON A DIM PAGE REACHES THE
/// CARET: on Solarized Dark Beam's bed turns its body off the page's hue at the page's luminance
/// (`bed::page_lit_body_ink`), and the letter guard holds that shade by itself, apart from the
/// caret's halo, cutting it only where it lowers a letter under the lower of its raster bar and its
/// own reading ([`beam_s_held_body_reaches_the_caret_on_every_frame`]), so the tube stands behind
/// the letters right up to the caret and no letter falls under the dim bar. Replayed with the
/// frame's glyph ink, on the frame just after the last key, each of the three cells behind the
/// caret lies at least 0.05 off the page in OKLab over its bare page. Negative control
/// (`EffectsPipeline::set_lit_body_a_shade(false)`): weighed with the lights in its cells, the
/// guard cuts the body under the last word, the navy box stopping short of the caret again.
#[test]
fn beam_s_body_on_a_dim_page_reaches_the_caret() {
    let rgb = |c: u32| aterm_types::Rgb {
        r: (c >> 16) as u8,
        g: (c >> 8) as u8,
        b: c as u8,
    };
    let look = SOLARIZED_DARK;
    // The least mean distance off the page of the bare pixels in the letters' rows, over the
    // three cells behind the caret, and the letter readings under the bar.
    let read = |scene: Scene, shade: bool| -> (f64, usize) {
        let mut r = Replay::sized("beam", look, TrailColor::Automatic, scene.rows());
        r.fx.set_lit_body_a_shade(shade);
        r.post = true;
        r.letters = true;
        r.own = true;
        let (ev, tail) = scene.events_on(look);
        let last = ev
            .iter()
            .rev()
            .find(|(_, e)| matches!(e, Ev::Key(..)))
            .map(|(at, _)| *at)
            .expect("the scene types");
        r.keep = vec![(last + 4).div_ceil(FRAME_MS) * FRAME_MS];
        let judged = r.run(&ev, tail);
        let bar = look_bar(look);
        let under = judged
            .iter()
            .flat_map(|j| j.letters())
            .filter(|l| l.judged() < bar.min(OWN_SHARE * l.own))
            .count();
        let k = &r.kept[0];
        let (cw, ch) = k.cell;
        let y0 = k.top + k.caret.0 * ch;
        let least = (1..=3)
            .map(|back| {
                let x0 = k.left + (k.caret.1 - back) * cw;
                let (mut sum, mut n) = (0.0, 0usize);
                for y in y0 + ch / 8..y0 + ch * 3 / 4 {
                    for x in x0..x0 + cw {
                        let i = y * k.w + x;
                        if k.clean[i] & 0x00FF_FFFF == look.bg {
                            sum += aterm_types::scheme_signature::oklab_distance(
                                rgb(k.px[i]),
                                rgb(look.bg),
                            );
                            n += 1;
                        }
                    }
                }
                assert!(
                    n > 0,
                    "premise: the cell {back} behind the caret shows its page"
                );
                sum / n as f64
            })
            .fold(f64::INFINITY, f64::min);
        (least, under)
    };
    for scene in [Scene::Typing, Scene::Cjk] {
        let (least, under) = read(scene, true);
        assert!(
            least >= 0.05,
            "{scene:?}: Beam's body falls to {least:.3} off the page behind the caret"
        );
        assert_eq!(under, 0, "{scene:?}: letter readings under the dim bar");
        let (cut, _) = read(scene, false);
        assert!(
            cut < 0.03,
            "control: {scene:?}: weighed with the lights in its cells, Beam's body keeps {cut:.3} \
             off the page behind the caret"
        );
    }
}

/// W6 on every frame, BEAM'S HELD BODY REACHES THE CARET ON EVERY FRAME: the letter guard holds
/// Beam's body on Solarized Dark to the letters it reaches (`CursorGlow::hold_page_shades`),
/// cutting it only where it lowers a letter under the lower of its raster bar and its own reading,
/// and W6's reading behind the caret falls nowhere. Every frame of Beam typing, bursting, typing
/// CJK, at an Enter on the bottom row and typing past the margin there, through the host's post
/// passes, is read against the same frame with the body kept whole
/// (`set_letter_guard_shades(false)`): each of the three cells behind the caret that lies at least
/// 0.05 off the page in OKLab over its bare page (W6's reading and bar) with the body whole does so
/// held. Negative controls, each failing the law in some scene:
/// - `EffectsPipeline::set_lit_body_a_shade(false)`: weighed with the lights in its cells, the body
///   is cut with the caret's halo under the last word.
/// - `EffectsPipeline::set_letter_guard_shades_to_own(false)`: each letter held to its raster bar,
///   as the hold's second pass held it, the body is cut from under a letter of its own 4.5086,
///   under AA's level in hand (4.563), where the whole body lifts it short of that bar. Typing,
///   18 of the 227 cells fall, bursting 48 of 610, at the Enter 16 of 260 and past the margin 77
///   of 879 (Linux arm64, 2026-11-07).
///
/// Both the law and the second control rest on the face: they need a letter of its own reading
/// under AA's level in hand behind the caret, which the whole body lifts short of that bar. They
/// were measured on Linux's bundled face ([`lays_the_bundled_cell`]) and on the Mac's
/// ([`lays_the_gallery_row`]), where none falls held and the second control is required. On the
/// Mac's, of the 257 cells typing, 623 bursting, 98 typing CJK, 293 at the Enter and 950 past the
/// margin that W6 reads at its bar with the body whole, none falls held; held to the raster bar, 16
/// fall bursting and 22 past the margin, the first the `d` of column 47 at 0.035 against 0.052
/// whole (macOS, 2026-11-11). On any other face each scene lets no more fall held than held to the
/// raster bar.
#[test]
fn beam_s_held_body_reaches_the_caret_on_every_frame() {
    use std::sync::mpsc::sync_channel;
    const SCENES: [Scene; 5] = [
        Scene::Typing,
        Scene::Burst,
        Scene::Cjk,
        Scene::EnterBottom,
        Scene::Wrapped,
    ];
    let look = SOLARIZED_DARK;
    let rgb = |c: u32| aterm_types::Rgb {
        r: (c >> 16) as u8,
        g: (c >> 8) as u8,
        b: c as u8,
    };
    // W6's reading of the cell `(row, col)` on frame `px` over its clean twin: the mean OKLab
    // distance off the page of its page pixels in the letters' rows; `None` with none.
    let w6 = |k: &Kept, px: &[u32], (row, col): (usize, usize)| {
        let (cw, ch) = k.cell;
        let (x0, y0) = (k.left + col * cw, k.top + row * ch);
        let (mut sum, mut n) = (0.0, 0usize);
        for y in y0 + ch / 8..y0 + ch * 3 / 4 {
            for x in x0..x0 + cw {
                let i = y * k.w + x;
                if k.clean[i] & 0x00FF_FFFF == look.bg {
                    sum += aterm_types::scheme_signature::oklab_distance(rgb(px[i]), rgb(look.bg));
                    n += 1;
                }
            }
        }
        (n > 0).then_some(sum / n as f64)
    };
    // Every frame of `scene` with the body as `set` lays it, against the body kept whole: the
    // frames read, the cells behind the caret W6 reads at its bar whole, those of them that fall
    // under it with the body as `set` lays it, and the first such `(ms, column, set, whole)`.
    let read = |scene: Scene, set: fn(&mut EffectsPipeline)| {
        let (ev, tail) = scene.events_on(look);
        let end = ev.last().map_or(0, |e| e.0) + tail;
        let frames: Vec<u64> = (1..=end / FRAME_MS).map(|k| k * FRAME_MS).collect();
        let replay = |set: fn(&mut EffectsPipeline), to| {
            let mut r = Replay::sized("beam", look, TrailColor::Automatic, scene.rows());
            set(&mut r.fx);
            r.post = true;
            r.keep.clone_from(&frames);
            r.keep_to = Some(to);
            let _ = r.run(&ev, tail);
        };
        let (to_set, set_frames) = sync_channel::<Kept>(1);
        let (to_whole, whole_frames) = sync_channel::<Kept>(1);
        std::thread::scope(|s| {
            // Owned here, the readers go with a failed read, and so the replays with them.
            let (set_frames, whole_frames) = (set_frames, whole_frames);
            s.spawn(|| replay(set, to_set));
            s.spawn(|| replay(|fx| fx.set_letter_guard_shades(false), to_whole));
            let (mut read, mut at_bar, mut fell, mut first) = (0usize, 0usize, 0usize, None);
            for (a, b) in set_frames.iter().zip(whole_frames.iter()) {
                assert_eq!(a.ms, b.ms, "fixture: the replays keep the same frames");
                read += 1;
                let (row, caret) = b.caret;
                for back in 1..=3.min(caret) {
                    let cell = (row, caret - back);
                    let (Some(whole), Some(kept)) = (w6(&b, &b.px, cell), w6(&b, &a.px, cell))
                    else {
                        continue;
                    };
                    if whole >= 0.05 {
                        at_bar += 1;
                        if kept < 0.05 {
                            fell += 1;
                            first.get_or_insert((a.ms, cell.1, kept, whole));
                        }
                    }
                }
            }
            (scene, read, at_bar, fell, first)
        })
    };
    let measured = lays_the_bundled_cell() || lays_the_gallery_row();
    let held = SCENES.map(|scene| read(scene, |_| {}));
    let to_bar = SCENES.map(|scene| read(scene, |fx| fx.set_letter_guard_shades_to_own(false)));
    for ((scene, frames, at_bar, fell, first), bar) in held.into_iter().zip(&to_bar) {
        assert!(
            frames > 0 && at_bar > 0,
            "premise: {scene:?}: {frames} frames, {at_bar} cells behind the caret at W6's bar"
        );
        if measured {
            assert_eq!(
                fell, 0,
                "{scene:?}: of {at_bar} cells behind the caret at W6's bar with Beam's body \
                 whole, {fell} fall under it held, the first (ms, column, held, whole) {first:?}"
            );
        } else {
            assert!(
                fell <= bar.3,
                "{scene:?}: of {at_bar} cells behind the caret at W6's bar with Beam's body \
                 whole, {fell} fall under it held, more than the {} held to the raster bar, the \
                 first (ms, column, held, whole) {first:?}",
                bar.3
            );
        }
    }
    let weighed = SCENES.map(|scene| read(scene, |fx| fx.set_lit_body_a_shade(false)));
    assert!(
        weighed.iter().any(|v| v.3 > 0),
        "control: weighed with the lights in its cells, Beam's body keeps W6's bar behind the \
         caret: {weighed:?}"
    );
    if measured {
        assert!(
            to_bar.iter().any(|v| v.3 > 0),
            "control: each letter held to its raster bar, Beam's body keeps W6's bar behind the \
             caret: {to_bar:?}"
        );
    }
}

#[test]
#[ignore = "survey: prints every style's worst letter (TRAIL_SURVEY_STYLES, TRAIL_LOOKS=dark|light|dim, \
            TRAIL_SCENES=<Scene,...>, TRAIL_GUARD=0, TRAIL_COLOR=#rrggbb, TRAIL_DUMP=<dir>, \
            TRAIL_DUMP_MS=<ms,...>)"]
fn survey_every_style_s_worst_letter() {
    let want: Option<Vec<String>> = std::env::var("TRAIL_SURVEY_STYLES")
        .ok()
        .map(|s| s.split(',').map(str::to_string).collect());
    let looks: Vec<Look> = match std::env::var("TRAIL_LOOKS").ok().as_deref() {
        Some("dark") => vec![DARK],
        Some("light") => vec![GITHUB_LIGHT, DEFAULT_LIGHT],
        Some("dim") => DIM_LOOKS.to_vec(),
        _ => LOOKS.to_vec(),
    };
    let scenes: Vec<Scene> = std::env::var("TRAIL_SCENES").map_or(SCENES.to_vec(), |s| {
        BAR_SCENES
            .into_iter()
            .filter(|scene| s.split(',').any(|w| w == format!("{scene:?}")))
            .collect()
    });
    let mut jobs = Vec::new();
    for style in STYLES {
        if want.as_ref().is_some_and(|w| !w.iter().any(|x| x == style)) {
            continue;
        }
        for &look in &looks {
            for &scene in &scenes {
                jobs.push((style, look, scene));
            }
        }
    }
    let dir = std::env::var("TRAIL_DUMP").ok();
    let dump_ms: Option<Vec<u64>> = std::env::var("TRAIL_DUMP_MS")
        .ok()
        .map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect());
    let guard = std::env::var("TRAIL_GUARD").map_or(true, |v| v != "0");
    let color = std::env::var("TRAIL_COLOR")
        .ok()
        .and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok())
        .map_or(TrailColor::Automatic, TrailColor::Pinned);
    let verdicts = replay_all_in(
        &jobs,
        guard,
        |style, look, scene| {
            let Some(dir) = &dir else {
                return Vec::new();
            };
            let all: Vec<u64> = (0..10_000).step_by(16).collect();
            dump_ms
                .as_ref()
                .unwrap_or(&all)
                .iter()
                .map(|&f| {
                    (
                        f,
                        format!(
                            "{dir}/{}-{}-{scene:?}-{f:05}.png",
                            style.replace(' ', "-"),
                            look.name
                        ),
                    )
                })
                .collect()
        },
        color,
    );
    for v in verdicts {
        println!("{v}");
    }
}

/// looks7 round 2, A DIM PAGE'S RETURN SHOWS IN THE AIR: on Solarized Dark a letter keeps 97%
/// of its own reading and no bed may light it, yet where no letter lies (the blank prompt row
/// a Return's tube crosses, the leading past a line's end) a bed wears the light it wears on
/// the reference dark theme (`AirLight`). From the Return on, Beam's frames light twice as many
/// of the blank cells' pixels past half again the page's luminance as its glow and caret do
/// alone (24 270 to 10 753). Negative control: laid at the page's luminance in the air too, the
/// tube does not show, and the frames light only the glow's and the caret's.
#[test]
fn a_dim_page_s_return_shows_in_the_air() {
    let lit = |on: bool| -> usize {
        let (ev, tail) = Scene::EnterBottom.events();
        let enter = ev
            .iter()
            .find(|(_, e)| matches!(e, Ev::Key('\r', _)))
            .map_or(0, |(at, _)| *at);
        let mut r = Replay::new("beam", SOLARIZED_DARK, TrailColor::Automatic);
        r.fx.set_bed_air_light(on);
        r.air = true;
        r.run(&ev, tail)
            .iter()
            .filter(|j| j.ms >= enter)
            .map(|j| j.air)
            .sum()
    };
    let (on, off) = (lit(true), lit(false));
    assert!(
        on >= 2 * off,
        "Beam's Return lights {on} blank pixels, {off} without the air's light"
    );
}

/// A LETTER'S PLAN READS WHAT THE JUDGE WRITTEN OUT READS. On seeded frames (a page and a second
/// ground, inks strong and faint, lights in a few colours over the frame's alpha byte), each cell
/// and two-cell span read through [`Pair::plan`] (blank or not, [`LetterPlan::reading`], its own
/// reading on the clean frame, [`Pair::untouched`]) reads to the bit what `aterm-lab
/// trail-contrast`'s judge reads, written out here pixel by pixel: the mode ground, the solid share
/// of the extreme, each solid pixel's nearest ring of ground in the clean frame, its channel medians
/// on the frame, each row's lower median, the worst row. NEGATIVE CONTROLS (2026-11-05), each fails
/// here in the first case: a row's upper median (`ratios.len() / 2`), a ratio memo keyed on the
/// ring's colour alone, and a plan that tries the far ring first.
#[test]
fn a_letter_s_plan_reads_the_judge_written_out() {
    fn written_out(frame: &[u32], clean: &[u32], w: usize, cell: [usize; 4]) -> Option<f64> {
        const RGB: u32 = 0x00FF_FFFF;
        let [x0, y0, x1, y1] = cell;
        let h = clean.len() / w;
        let at = |px: &[u32], x: usize, y: usize| px[y * w + x] & RGB;
        let mut seen: Vec<u32> = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .map(|(x, y)| at(clean, x, y))
            .collect();
        seen.sort_unstable();
        let runs = seen.chunk_by(|a, b| a == b);
        let longest = runs.clone().map(<[u32]>::len).max()?;
        let ground = runs.into_iter().find(|r| r.len() == longest)?[0];
        let extreme = seen
            .iter()
            .map(|c| contrast(*c, ground))
            .fold(f64::NEG_INFINITY, f64::max);
        if extreme < BLANK_BELOW {
            return None;
        }
        let mut worst = f64::INFINITY;
        for y in y0..y1 {
            let mut ratios = Vec::new();
            for x in x0..x1 {
                if contrast(at(clean, x, y), ground) < SOLID_SHARE * extreme {
                    continue;
                }
                let g = (1..=RING_REACH).find_map(|d| {
                    let ring: Vec<u32> = (-d..=d)
                        .flat_map(|dy| (-d..=d).map(move |dx| (dx, dy)))
                        .filter(|&(dx, dy)| dx.abs().max(dy.abs()) == d)
                        .map(|(dx, dy)| (x as i64 + dx, y as i64 + dy))
                        .filter(|&(xx, yy)| xx >= 0 && yy >= 0 && xx < w as i64 && yy < h as i64)
                        .map(|(xx, yy)| (xx as usize, yy as usize))
                        .filter(|&(xx, yy)| at(clean, xx, yy) == ground)
                        .map(|(xx, yy)| at(frame, xx, yy))
                        .collect();
                    (!ring.is_empty()).then(|| {
                        [16u32, 8, 0].into_iter().fold(0, |out, sh| {
                            let mut c: Vec<u32> = ring.iter().map(|p| (p >> sh) & 0xff).collect();
                            c.sort_unstable();
                            out | c[c.len() / 2] << sh
                        })
                    })
                });
                if let Some(g) = g {
                    ratios.push(contrast(at(frame, x, y), g));
                }
            }
            if !ratios.is_empty() {
                ratios.sort_by(f64::total_cmp);
                worst = worst.min(ratios[(ratios.len() - 1) / 2]);
            }
        }
        Some(worst)
    }
    let mut x: u32 = 0x9E37_79B9;
    let mut next = move |n: u32| {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x % n
    };
    let (cw, ch, cols, rows) = (6usize, 10usize, 5usize, 3usize);
    let w = cw * cols;
    let mut ratios = Vec::new();
    let (mut blank, mut read, mut untouched) = (0, 0, 0);
    for case in 0..64 {
        // Each cell a ground and, most of them, ink in two strengths.
        let mut clean = vec![0xFF00_2B36u32; w * rows * ch];
        let (ink, faint) = (0xFF83_9496, 0xFF40_5A62);
        for (i, px) in clean.iter_mut().enumerate() {
            let (col, row) = ((i % w) / cw, (i / w) / ch);
            if (col + row + case) % 4 == 3 {
                *px = 0xFF11_2233;
            }
            if (col + 2 * row + case) % 5 != 4 {
                match next(9) {
                    0 | 1 => *px = ink,
                    2 => *px = faint,
                    _ => {}
                }
            }
        }
        // Most cells lit, in a few colours, one or two of them left as their twin.
        let mut frame = clean.clone();
        let lights = [0xFF50_2B36, 0xFF00_4A36, 0xFF20_2020, 0xFFE0_E0D0];
        for (i, px) in frame.iter_mut().enumerate() {
            let (col, row) = ((i % w) / cw, (i / w) / ch);
            if (col + row) % 7 != 6 && next(3) == 0 {
                *px = lights[next(4) as usize];
            }
        }
        let pair = Pair {
            frame: &frame,
            clean: &clean,
            w,
            cw,
            ch,
        };
        for row in 0..rows {
            for col in 0..cols {
                for span in 1..=2 {
                    let (x0, y0) = (col * cw, row * ch);
                    let (x1, y1) = ((x0 + span * cw).min(w), y0 + ch);
                    let same = (y0..y1).all(|y| {
                        (x0..x1).all(|x| (frame[y * w + x] ^ clean[y * w + x]) & 0x00FF_FFFF == 0)
                    });
                    assert_eq!(pair.untouched(row, col, span), same);
                    untouched += usize::from(same);
                    let plan = pair.plan(row, col, span);
                    let cell = [x0, y0, x1, y1];
                    let want = written_out(&frame, &clean, w, cell);
                    assert_eq!(
                        plan.is_none(),
                        want.is_none(),
                        "case {case} cell {row},{col}+{span}: blank"
                    );
                    let (Some(plan), Some(want)) = (plan, want) else {
                        blank += 1;
                        continue;
                    };
                    read += 1;
                    let got = plan.reading(&frame, &mut ratios);
                    assert_eq!(
                        got.to_bits(),
                        want.to_bits(),
                        "case {case} cell {row},{col}+{span}: the plan reads {got}, the judge {want}"
                    );
                    let own = written_out(&clean, &clean, w, cell).expect("not blank");
                    let got = plan.own(&clean, &mut ratios);
                    assert_eq!(
                        got.to_bits(),
                        own.to_bits(),
                        "case {case} cell {row},{col}+{span}: its own reading {got}, the judge {own}"
                    );
                }
            }
        }
    }
    assert!(
        blank > 0 && read > 0 && untouched > 0,
        "{blank} {read} {untouched}"
    );
}

/// [`median`] is each channel's sorted middle (`sorted[len / 2]`) over a ring's every size, its
/// one-colour path the same, and [`mode`] the first longest run of the sorted colours, for odd and
/// even counts, repeated values, ties and channels that differ in order. NEGATIVE CONTROLS
/// (2026-11-05): `select_nth_unstable((n - 1) / 2)` takes the value below the middle of an even
/// count, a uniform path that drops the mask keeps the top byte of `0xAA11_2233`, and `c > best`
/// takes a tie's highest colour; each fails here.
#[test]
fn median_and_mode_are_the_sorted_readings() {
    let sorted_middle = |px: &[u32]| {
        [16, 8, 0].into_iter().fold(0, |out, sh| {
            let mut ch: Vec<u32> = px.iter().map(|p| (p >> sh) & 0xff).collect();
            ch.sort_unstable();
            out | ch[ch.len() / 2] << sh
        })
    };
    let sorted_mode = |px: &[u32]| {
        let mut v: Vec<u32> = px.iter().map(|p| p & 0x00FF_FFFF).collect();
        v.sort_unstable();
        let runs = v.chunk_by(|a, b| a == b);
        let longest = runs.clone().map(<[u32]>::len).max().unwrap_or(0);
        runs.into_iter()
            .find(|r| r.len() == longest)
            .map_or(0, |r| r[0])
    };
    let mut px = Vec::new();
    let mut x: u32 = 0x9E37_79B9;
    for len in 1..=RING_MOST {
        px.clear();
        for _ in 0..len {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            px.push(match x % 6 {
                0 | 1 => 0x00FF_00FF,
                2 | 3 => 0xAA11_2233,
                _ => x & 0x00FF_FFFF,
            });
        }
        assert_eq!(median(len, |i| px[i]), sorted_middle(&px), "{px:08x?}");
        assert_eq!(mode(&px), sorted_mode(&px), "{px:08x?}");
        let one = vec![0xAA11_2233; len];
        assert_eq!(median(len, |i| one[i]), sorted_middle(&one), "{len} alike");
    }
}

/// The least share of its peak a frame's rail keeps between the first column it lights at half
/// its peak and the caret ([`Judged::rail`]), with the frame's time and that peak; `None` for a
/// replay whose rails never light `floor`.
fn rail_dip(judged: &[Judged], floor: u8) -> Option<(f64, u64, u8)> {
    let mut worst: Option<(f64, u64, u8)> = None;
    for j in judged {
        let peak = j.rail.iter().copied().max().unwrap_or(0);
        if peak < floor {
            continue;
        }
        let half = peak / 2;
        let Some(first) = j.rail.iter().position(|&v| v >= half) else {
            continue;
        };
        let least = j.rail[first..].iter().copied().min().unwrap_or(peak);
        let share = f64::from(least) / f64::from(peak);
        if worst.is_none_or(|w| share < w.0) {
            worst = Some((share, j.ms, peak));
        }
    }
    worst
}

/// looks7 round 3, A DIM PAGE'S RAIL IS ONE STROKE: on Solarized Dark a letter keeps 97% of
/// its own reading, and the letter guard cuts the lights on its row deep; a rail laid a
/// hairline under the row, clear of the guard's ring round a descender's foot, lights no
/// letter, and the guard cuts it only where it meets a dimmed cell's box. Typing, Classic in a
/// chosen colour (`#d03a8c`), Comet and the underline cat keep their rail one stroke from its
/// tail to the caret while the keys come: no column under the row falls under half the
/// stroke's peak once the stroke has risen to it. Negative control: the guard feathering its cuts into the rail as
/// into any light, Classic's rail broke into a dash under the word gap, a gap under `fox`'s
/// cut letters and the caret's foot (the gallery's `classic-pink/solarized-dark`, looks7 round
/// 2), and the underline cat's warm stops sank to olive and brown under its cut letters.
#[test]
fn a_dim_page_s_rail_is_one_stroke() {
    let read = |style: &'static str, color: TrailColor, scene: Scene, clear: bool| {
        let mut r = Replay::new(style, SOLARIZED_DARK, color);
        if matches!(color, TrailColor::Pinned(_)) {
            let spec = Spec::of(style);
            r.fx.set_cursor_glow(
                true,
                spec.trail,
                color,
                None,
                260,
                24,
                1.0,
                0.6,
                true,
                SOLARIZED_DARK.cursor,
            );
            r.fx.set_ribbon_shape(spec.tall, spec.flat);
        }
        r.fx.set_letter_guard_rails_clear(clear);
        r.rail = true;
        let (ev, tail) = scene.events();
        // While the keys come: once they stop, the run's cohorts fade apart.
        let last_key = ev
            .iter()
            .filter(|(_, e)| matches!(e, Ev::Key(..)))
            .map(|(at, _)| *at)
            .max()
            .unwrap_or(0);
        let judged: Vec<Judged> = r
            .run(&ev, tail)
            .into_iter()
            .filter(|j| j.ms <= last_key + FRAME_MS)
            .collect();
        rail_dip(&judged, RAIL_LIT)
    };
    let pink = TrailColor::Pinned(0x00D0_3A8C);
    let jobs = [
        ("classic", pink, Scene::Typing, true),
        ("comet", TrailColor::Automatic, Scene::Typing, false),
        (
            "underline rainbow + cat",
            TrailColor::Automatic,
            Scene::Typing,
            true,
        ),
        (
            "underline rainbow + cat",
            TrailColor::Automatic,
            Scene::Burst,
            true,
        ),
    ];
    for (style, color, scene, controlled) in jobs {
        let Some((share, ms, peak)) = read(style, color, scene, true) else {
            panic!("{style} {scene:?}: fixture: no rail lit past {RAIL_LIT} under the row");
        };
        assert!(
            share >= RAIL_KEPT,
            "{style} {scene:?}: at {ms} ms the rail falls to {share:.2} of its peak {peak} on its way to the caret"
        );
        if controlled {
            let off = read(style, color, scene, false);
            assert!(
                off.is_some_and(|(share, ..)| share < RAIL_KEPT),
                "{style} {scene:?}: control: the guard feathering its cuts into the rail, it keeps {off:?}"
            );
        }
    }
}

/// A rail lights a column past this lift to be read ([`rail_dip`]).
const RAIL_LIT: u8 = 96;
/// The least share of its peak a rail keeps from where it first reaches half of it to the
/// caret: a ramp's first column holds half, a dither's byte under it.
const RAIL_KEPT: f64 = 0.45;

/// looks7 round 3, A DIM PAGE'S TRAIL SHOWS BELOW ITS LETTERS: on Solarized Dark a bed under
/// the letters lays only its hue at the page's luminance, and Beam, Lumen and Comet drew
/// nothing behind the caret while typing, each a bare glowing caret; the flat rainbow showed
/// no rainbow past `jumps`. Each lays its light in the air a hairline below the row
/// (`BedShape::on_lit_page`, the flat ribbon's soft rail): in the frame after the last key,
/// more than half a cell's columns behind the caret (farther than a cell and a half from it,
/// past the caret's own glow) are lit under the row (Comet's wedge is the shortest), at the
/// gallery's size and where the window's face lays the gallery's row ([`gallery_row_px`]).
/// Negative control, on the gallery's row: with no light in the air
/// (`set_bed_air_light(false)`), Beam's, Lumen's and Comet's light no more than a quarter of a
/// cell's. A taller row lays more of Comet's wedge in the band under it: under Linux's bundled
/// JetBrains Mono at 22 px (13 x 29 px cells) Comet lights 5 of those columns with no light in
/// the air (17 with it), past a quarter's 3, and at 20 px (12 x 26) 2 (15 with it).
#[test]
fn a_dim_page_s_trail_shows_below_its_letters() {
    // The columns lit below the row, and the cell's width.
    let lit = |style: &'static str, px: f32, air: bool| -> (usize, usize) {
        let mut r = Replay::sized_at(style, SOLARIZED_DARK, TrailColor::Automatic, ROWS, px);
        r.fx.set_bed_air_light(air);
        r.rail = true;
        let (ev, tail) = Scene::Typing.events();
        let last_key = ev
            .iter()
            .filter(|(_, e)| matches!(e, Ev::Key(..)))
            .map(|(at, _)| *at)
            .max()
            .unwrap_or(0);
        let judged = r.run(&ev, tail);
        let after = judged
            .iter()
            .find(|j| j.ms > last_key)
            .expect("a frame after the last key");
        let behind = after.rail.len().saturating_sub(3 * r.cell.0 / 2);
        let lit = after.rail[..behind]
            .iter()
            .filter(|&&v| v >= BELOW_LIT)
            .count();
        (lit, r.cell.0)
    };
    let row_px = gallery_row_px();
    let mut sizes = vec![GALLERY_PX];
    if row_px != GALLERY_PX {
        sizes.push(row_px);
    }
    for px in sizes {
        for style in ["beam", "lumen", "comet", "flat rainbow + cat"] {
            let (on, cw) = lit(style, px, true);
            assert!(
                on > cw / 2,
                "{style} at {px} px: only {on} columns behind the caret lit below the row"
            );
            if style != "flat rainbow + cat" && px == row_px {
                let (off, _) = lit(style, px, false);
                assert!(
                    off <= cw / 4,
                    "{style} at {px} px: control: with no light in the air, {off} columns lit \
                     below the row"
                );
            }
        }
    }
}

/// A column under the row this far over the clean frame shows light ([`Judged::rail`]).
const BELOW_LIT: u8 = 40;

/// looks10 round 3, L6: A ONE-COLOUR MOVE UP A DIM PAGE IS ONE STROKE TO THE CARET. Classic in
/// a chosen colour (`#d03a8c`) on Solarized Dark, the trail gallery's `move-up`: the caret
/// leaves the `[vim]` row for `println!`'s `!` five rows up, over `// every glyph` and the
/// `}` row. On a dim page the tracer keeps no light over the letters, and under them it lays a
/// shade at the page's own luminance in its hue; laid at the tracer's share of its light and
/// cut with the lights in its cells by the letter guard, that shade kept a trace no eye found
/// at 1x (1.02:1), and the judges saw a lone lavender slash in the blank rows. Laid whole and
/// kept from the guard (it can only darken the ground of a letter lighter than the page), it
/// carries the stroke's hue over `// every glyph` to the caret's row: in each of the up move's
/// frames, the rows from the caret's to the comment's hold a run of pixels whose red stands
/// clear of their green ([`HUE_LIFT`]), and every letter keeps the page's bar. Negative control:
/// the shade laid faint and cut, as round 2 laid it (`set_classic_faint_shade`), falls short of
/// the caret's row in some frame of the move.
#[test]
fn a_one_colour_move_up_a_dim_page_is_one_stroke() {
    let pink = TrailColor::Pinned(0x00D0_3A8C);
    let (ev, tail) = Scene::MoveUp.events();
    let up = ev.last().map_or(0, |e| e.0);
    let frames: Vec<u64> = (0..4)
        .map(|k| up.div_ceil(FRAME_MS) * FRAME_MS + k * FRAME_MS)
        .collect();
    let read = |faint: bool| {
        let mut r = Replay::new("classic", SOLARIZED_DARK, pink);
        r.letters = true;
        r.fx.set_cursor_glow(
            true,
            "classic",
            pink,
            None,
            260,
            24,
            1.0,
            0.6,
            true,
            SOLARIZED_DARK.cursor,
        );
        r.fx.set_classic_faint_shade(faint);
        r.post = true;
        r.keep.clone_from(&frames);
        let judged = r.run(&ev, tail);
        let bar = look_bar(SOLARIZED_DARK);
        let under: usize = judged
            .iter()
            .map(|j| j.letters().iter().filter(|l| l.judged() < bar).count())
            .sum();
        let (_, ch) = r.cell;
        // Each kept frame's warm pixels in the caret's row (`println!`, row 3) and in the
        // comment's under it (row 4): the least of the two.
        let hued: Vec<(u64, usize)> = r
            .kept
            .iter()
            .map(|k| {
                let in_row = |row: usize| {
                    let (y0, y1) = (k.top + row * ch, k.top + (row + 1) * ch);
                    k.px[y0 * k.w..y1 * k.w]
                        .iter()
                        .filter(|&&p| {
                            let (red, green) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF);
                            red >= green + HUE_LIFT
                        })
                        .count()
                };
                (k.ms, in_row(3).min(in_row(4)))
            })
            .collect();
        assert_eq!(
            hued.len(),
            frames.len(),
            "fixture: the replay kept the up move's frames"
        );
        (under, hued, worst(&judged), ch)
    };
    let (under, hued, least, ch) = read(false);
    assert_eq!(
        under, 0,
        "a letter under the move up reads under the bar (least {least:?})"
    );
    for &(ms, n) in &hued {
        assert!(
            n >= ch,
            "at {ms} ms the stroke shows {n} warm pixels in the caret's row or the comment's, \
             under a cell's height ({ch}): it does not reach the caret"
        );
    }
    let (_, faint, _, _) = read(true);
    assert!(
        faint.iter().any(|&(_, n)| n < ch),
        "control: the shade laid faint and cut, the rows to the caret show the stroke in every \
         frame: {faint:?}"
    );
}

/// A pixel whose red stands this far over its green shows a warm hue on a teal page
/// ([`a_one_colour_move_up_a_dim_page_is_one_stroke`]).
const HUE_LIFT: u32 = 16;

/// The floor a block fill over a letter is solved for (`caret_letter_min_contrast` in
/// aterm-effects), the letter guard's own (`LETTER_FLOOR`).
const CARET_LETTER_FLOOR: f64 = 4.6;

/// What the letter under the caret keeps: [`CARET_LETTER_FLOOR`], or 97% of what the theme's own
/// caret leaves it where that is under the floor (the lab's dim-page rule).
fn caret_bar(own: f64) -> f64 {
    CARET_LETTER_FLOOR.min(0.97 * own)
}

/// The least share of its bar the letter under the caret keeps through
/// [`Scene::MoveUp`] (`style` on `look`, the host's post passes drawn, sparing the letters when
/// `spared`), with the caret-letter law `off` turned off, and the frame and letter that kept it.
fn caret_letter_least(
    style: &'static str,
    look: Look,
    off: Option<aterm_effects::cursor_glow::CaretLetterLaw>,
    spared: bool,
) -> Option<(u64, Letter)> {
    let (ev, tail) = Scene::MoveUp.events();
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.post = true;
    r.caret = true;
    if let Some(law) = off {
        r.fx.set_caret_letter_law(law, false);
    }
    r.fx.set_bloom_spares_letters(spared);
    r.run(&ev, tail)
        .iter()
        .filter_map(|j| j.caret.map(|l| (j.ms, l)))
        .min_by(|a, b| {
            (a.1.judged() / caret_bar(a.1.own)).total_cmp(&(b.1.judged() / caret_bar(b.1.own)))
        })
}

/// A style on a look, and the least reading of the letter under its caret with its frame's
/// time ([`caret_letter_least`]).
type CaretVerdict = (&'static str, Look, Option<(u64, Letter)>);

/// looks8 WIDEN, THE LETTER UNDER THE CARET READS: the gallery's `move-up` (a program moves
/// the caret up onto `println!`'s `!`, as vim's `k` does) read that `!` at 1.00:1 under
/// Laser's bolt, which hid it, 2.45:1 under Water and 4.05:1 under Fire on the dark page:
/// the block caret draws the letter cut out of its fill in the page's colour, so the letter
/// reads the fill's contrast, which the page floor held only to 4.5:1, and the post passes
/// drawn over the finished frame (the SDR crown, the bloom) put the caret's own light back on
/// it. Replayed with the host's post passes, every style but Classic keeps the letter under
/// the caret at the letter's floor ([`caret_bar`]) on the dark, GitHub Light, Default Light and
/// Solarized Dark pages, every frame the caret holds it. Negative controls, on the dark page with
/// the post passes over the letters in both arms, as the host drew them until looks10
/// (`BloomGate::SparesLetters` off), so that the law alone moves the reading: with it each style
/// keeps the letter at its bar, and without it Laser's bolt kept over the letter hides it, Fire's
/// ember held to its page floor leaves it under the letter's floor, and Water's light left on the
/// caret's cell lifts it under that floor (spared, Water's least reading of `!` is the same with
/// its light left on, 5.62:1, which is why these arms are not spared). The floor is the letter
/// guard's, what the fill's law is solved for, not the 4.5 bar:
/// read by the ring round its strokes (r27 LG1), the `!` reads its fill's contrast nearly whole,
/// so Fire's ember held to its page floor with after-land L3's margin
/// (`cursor_fx::EFFECT_CARET_PAGE_GUARD`) reads 4.55:1 off the law against 5.60:1 on it, and a
/// bar of 4.5 could not tell them apart.
#[test]
fn the_letter_under_the_caret_reads() {
    use aterm_effects::cursor_glow::CaretLetterLaw;
    let mut jobs = Vec::new();
    for look in [DARK, GITHUB_LIGHT, DEFAULT_LIGHT, SOLARIZED_DARK] {
        for style in STYLES.iter().copied().filter(|s| *s != "classic") {
            jobs.push((style, look));
        }
    }
    let lanes = aterm_cores::available_parallelism().map_or(4, usize::from);
    let verdicts: Vec<CaretVerdict> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(lanes).max(1))
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|&(style, look)| {
                            (style, look, caret_letter_least(style, look, None, true))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a replay lane"))
            .collect()
    });
    let under: Vec<String> = verdicts
        .iter()
        .filter_map(|(style, look, least)| {
            let (ms, l) = (*least)?;
            (l.judged() < caret_bar(l.own)).then(|| {
                format!(
                    "{style} on {} at {ms} ms: `{}` {:.2}:1, its own caret's {:.2}",
                    look.name,
                    l.ch,
                    l.judged(),
                    l.own
                )
            })
        })
        .collect();
    assert!(
        under.is_empty(),
        "the letter under the caret under its bar:\n{}",
        under.join("\n")
    );
    for (style, law) in [
        ("laser", CaretLetterLaw::BoltStandsDown),
        ("fire", CaretLetterLaw::LetterFloor),
        ("water", CaretLetterLaw::LightOff),
    ] {
        let read =
            |off| caret_letter_least(style, DARK, off, false).expect("a caret on its letter");
        let (ms, l) = read(None);
        assert!(
            l.judged() >= caret_bar(l.own),
            "{law:?} on, the post passes over the letters: {style} leaves `{}` at {:.2}:1 ({ms} ms)",
            l.ch,
            l.judged()
        );
        let (ms, l) = read(Some(law));
        assert!(
            l.judged() < caret_bar(l.own),
            "control: {law:?} off, {style} still keeps `{}` at {:.2}:1 ({ms} ms)",
            l.ch,
            l.judged()
        );
    }
}

/// The worst reading over the replay of a letter on row 5 of the gallery's `context`-like page
/// on dark: the prompt on row 4, row 5 on a filled ground (`#073642`) holding `accented` nine
/// times from column 7, then [`TYPING_TEXT`] typed under the rainbow kitty pet; the frame's
/// combining marks unread when `unread` (`EffectsPipeline::set_frame_marks_unread`). Returns
/// the worst judged reading of a row-5 letter and how many letter-frames fall under [`BAR`].
fn accented_row_under_the_foot(accented: &str, unread: bool) -> (f64, usize) {
    let mut r = Replay::new("rainbow + cat", DARK, TrailColor::Automatic);
    r.letters = true;
    r.fx.set_frame_marks_unread(unread);
    let mut row = " ".repeat(7);
    for _ in 0..9 {
        row.push_str(accented);
    }
    row.push_str(&" ".repeat(usize::from(COLS) - 16));
    let page = format!("\x1b[2J\x1b[6;1H\x1b[48;2;7;54;66m{row}\x1b[0m\x1b[5;1H$ ");
    let mut ev = vec![(200, Ev::Out(page.into_bytes()))];
    ev.extend(typed(TYPING_TEXT, 1_000, 60));
    let judged = r.run(&ev, 600);
    let mut worst = f64::INFINITY;
    let mut under = 0;
    for l in judged
        .iter()
        .flat_map(|j| j.letters().iter())
        .filter(|l| l.row == 5)
    {
        worst = worst.min(l.judged());
        under += usize::from(l.judged() < BAR);
    }
    (worst, under)
}

/// looks9's review: A DECOMPOSED ACCENT UNDER THE FOOT READS. The effects read a cell's short
/// glyph off its base letter alone, so a decomposed `e` + U+0301 (a macOS filename's NFD)
/// counted as a short `e`; the rainbow kitty's foot rail dipped into the cell's top, onto the
/// accent, and the letter read 2.56:1 (90 letter-frames under the bar). On the gallery's
/// `context`-like page, a row of decomposed `é` under the typed line keeps every letter at the
/// bar, as the precomposed `é` does. Negative control (`set_frame_marks_unread`, the marks
/// unread): the decomposed row falls under it, on the gallery's cell, macOS's SF Mono
/// ([`lays_the_gallery_row`]). Where the face lays another cell the acute may lie clear of the
/// dip: under Linux's bundled JetBrains Mono (13 x 29 px cells) the `é` row reads 5.56:1 unread.
/// A row of decomposed `å` (`a` + U+030A), its ring a pixel higher, keeps every letter on every
/// cell too, as the precomposed `å` does, and is the control where the face lays another cell:
/// unread, it reads 2.49:1 (90 letter-frames under the bar), 5.42:1 read (Linux arm64,
/// 2026-11-06). Windows' bundled Iosevka Term SemiBold replays the control its cell picks,
/// unmeasured on Windows.
#[test]
fn a_decomposed_accent_under_the_foot_reads() {
    const ACUTE: (&str, &str) = ("e\u{301}", "\u{e9}");
    const RING: (&str, &str) = ("a\u{30a}", "\u{e5}");
    // The control's row: `é`'s on the gallery's cell (macOS's measured one), `å`'s where the
    // face lays another.
    let control = if lays_the_gallery_row() { ACUTE } else { RING };
    for (decomposed, precomposed) in [ACUTE, RING] {
        let (worst, under) = accented_row_under_the_foot(decomposed, false);
        assert!(worst.is_finite(), "fixture: the row's letters are judged");
        assert_eq!(
            under, 0,
            "{under} letter-frames of the decomposed {precomposed} row under {BAR}:1 (worst \
             {worst:.2}:1)"
        );
        let (composed, composed_under) = accented_row_under_the_foot(precomposed, false);
        assert_eq!(
            composed_under, 0,
            "fixture: the precomposed {precomposed} row reads at the bar (worst {composed:.2}:1)"
        );
    }
    let (decomposed, precomposed) = control;
    let (old, old_under) = accented_row_under_the_foot(decomposed, true);
    assert!(
        old_under > 0,
        "control: the marks unread, the decomposed {precomposed} row still reads {old:.2}:1"
    );
}

/// A RAIL ON A GRID'S LAST ROW RUNS ON BEHIND EVERY DESCENDER, AT EVERY SIZE, IN ITS WHOLE BAND
/// (after-land A5, its review; the forward round's W9). With no row below to dip into, the rail
/// keeps its foot band past each descender, in the light that letter leaves near its ink
/// (`bed::RowFeet::lane`). The real face leaves `q` a pixel at 22 px and `g` and `y` none, and
/// nothing on any descender at 14 px; kept to a pixel the glyph left free, the rail broke at each
/// of those in two pieces with a gap round the cell, and laid as a thread on the row's last pixel
/// round the foot's whole cell it read as that gap at 1x in the window (W9: `beam-dark-rail-g`'s
/// `last-g`, the thread a pixel of `#073d57` beside the band's `#0774a6`, a 26 px gap under `g`).
/// So through the typed line of [`Scene::LastRow`], at each size, every ground pixel of each of
/// the band's pixel rows between the rail's tail and the caret is lit, and every letter holds.
/// Controls, by source edit: the lane kept to a free pixel and the thread's light kept off a
/// one-pixel band (14 px and under) each fail this. In the test (`set_rail_threaded`), the thread
/// on the row's last pixel past each descender breaks the band's upper rows at 22 px (W9).
#[test]
fn a_last_row_rail_runs_on_behind_every_descender() {
    /// A lift (channels summed) past the tail's own first pixels, from where the rail is read.
    const TAIL: i32 = 16;
    /// A pixel lit under this, in the run, is a break in the rail.
    const LIT: i32 = 2;
    let scene = Scene::LastRow;
    let end = 1_000 + 50 * BURST_TEXT.len() as u64;
    // The letters under their bar, and the breaks in the rail's band (`runs_on`).
    let read = |px: f32, style: &str, look: Look, runs_on: bool, threaded: bool| {
        let mut r = Replay::gridded_in(
            px,
            style,
            look,
            TrailColor::Automatic,
            (scene.rows(), scene.cols()),
        );
        r.fx.cursor_mut().glow.set_rail_threaded(threaded);
        r.letters = true;
        r.own = true;
        r.post = true;
        let (ev, _) = scene.events_on(look);
        let ev: Vec<_> = ev.into_iter().filter(|e| e.0 <= end).collect();
        r.keep = (1_504..end).step_by(48).collect();
        let judged = r.run(&ev, 16);
        let bar = look_bar(look);
        let mut under = Vec::new();
        for j in &judged {
            for l in j.letters() {
                if l.judged() < bar.min(OWN_SHARE * l.own) {
                    under.push(format!(
                        "{style} on {} at {px} px, {} ms: `{}` r{} c{} reads {:.2}, own {:.2}",
                        look.name,
                        j.ms,
                        l.ch,
                        l.row,
                        l.col,
                        l.judged(),
                        l.own
                    ));
                }
            }
        }
        let mut broken = Vec::new();
        if runs_on {
            assert!(r.kept.len() > 30, "fixture: {} frames kept", r.kept.len());
        }
        for k in r.kept.iter().filter(|_| runs_on) {
            let (cw, ch) = k.cell;
            let band = ((aterm_effects::rainbow_kitty::ribbon::ROW_RAIL_CH * ch as f32).floor()
                as usize)
                .max(1);
            let bottom = k.top + (k.caret.0 + 1) * ch;
            for row in bottom - band..bottom {
                let at = |x: usize| (k.px[row * k.w + x], k.clean[row * k.w + x]);
                let ground = at(k.left).1;
                let lift = |(a, b): (u32, u32)| {
                    [16, 8, 0]
                        .iter()
                        .map(|sh| ((a >> sh & 255) as i32 - (b >> sh & 255) as i32).abs())
                        .sum::<i32>()
                };
                let caret_x = k.left + k.caret.1 * cw;
                let tail = (k.left..caret_x)
                    .find(|&x| lift(at(x)) >= TAIL)
                    .unwrap_or_else(|| {
                        panic!(
                            "fixture: {style} at {px} px, {} ms lays no rail on y{row}",
                            k.ms
                        )
                    });
                let breaks: Vec<usize> = (tail..caret_x)
                    .filter(|&x| at(x).1 == ground && lift(at(x)) < LIT)
                    .collect();
                if !breaks.is_empty() {
                    broken.push(format!(
                        "{style} at {px} px, {} ms: the rail breaks on y{row} between x{tail} and \
                         the caret at x{caret_x}, at {breaks:?}",
                        k.ms
                    ));
                }
            }
        }
        (under, broken)
    };
    for px in [13.0, 14.0, 20.0, 22.0, 24.0] {
        for (style, look, runs_on) in [
            ("beam", DARK, true),
            ("phaser", DARK, true),
            // Solarized Dark's letters take almost no light: the band is the page's own level.
            ("beam", SOLARIZED_DARK, false),
        ] {
            let (under, broken) = read(px, style, look, runs_on, false);
            assert!(
                under.is_empty(),
                "letters under their bar:\n{}",
                under.join("\n")
            );
            assert!(broken.is_empty(), "{}", broken.join("\n"));
        }
    }
    let (_, control) = read(22.0, "beam", DARK, true, true);
    assert!(
        !control.is_empty(),
        "control: laid as a thread on the row's last pixel past each descender, the rail's band \
         runs on unbroken"
    );
}

/// THE FORWARD ROUND'S M-M4, A DEEP CELL'S FOOT LEAVES A RAIL NO LIGHT. A cell the frame lays
/// deep ink in (an underline's stroke, a combining mark, a subscript) holds ink its glyph's raster
/// does not, so its end is taken as the row's bottom; the light its letter leaves a rail with no
/// lane was read off that raster all the same, and an underline's own stroke, in a colour of its
/// own (SGR 58), took the light its glyph's ink could bear. Through [`Scene::LastRow`]'s typed
/// line underlined straight, doubled and curled in `#c1c1c1` (solid beside the dark look's
/// `#d0d0d0` ink, the judges' 85%, and under it), at each size, under each style whose rail
/// takes a lane there, every letter holds its bar.
/// Control (`set_deep_light_rastered`): with the light read off the raster, Phaser's curl at 24 px
/// leaves a `p` at 4.10 of its own 10.32.
#[test]
fn a_deep_cell_s_foot_leaves_a_rail_no_light() {
    let scene = Scene::LastRow;
    let end = 1_000 + 50 * BURST_TEXT.len() as u64;
    let look = DARK;
    let bar = look_bar(look);
    // The letter-frames judged, and those under their bar.
    let read = |sgr: &[u8], line: &str, px: f32, style: &str, rastered: bool| {
        let mut r = Replay::gridded_in(
            px,
            style,
            look,
            TrailColor::Automatic,
            (scene.rows(), scene.cols()),
        );
        r.fx.cursor_mut().glow.set_deep_light_rastered(rastered);
        r.letters = true;
        r.own = true;
        r.post = true;
        let (mut ev, _) = scene.events_on(look);
        if let Some((_, Ev::Out(page))) = ev.first_mut() {
            page.extend_from_slice(sgr);
        }
        let ev: Vec<_> = ev.into_iter().filter(|e| e.0 <= end).collect();
        let judged = r.run(&ev, 16);
        let mut under = Vec::new();
        let mut n = 0usize;
        for j in &judged {
            for l in j.letters() {
                n += 1;
                if l.judged() < bar.min(OWN_SHARE * l.own) {
                    under.push(format!(
                        "{style} at {px} px, {line}, {} ms: `{}` r{} c{} reads {:.2}, own {:.2}",
                        j.ms,
                        l.ch,
                        l.row,
                        l.col,
                        l.judged(),
                        l.own
                    ));
                }
            }
        }
        (n, under)
    };
    const CURLED: &[u8] = b"\x1b[4:3m\x1b[58;2;193;193;193m";
    let mut under = Vec::new();
    let mut judged_n = 0usize;
    for (sgr, line) in [
        (&b"\x1b[4m\x1b[58;2;193;193;193m"[..], "straight"),
        (&b"\x1b[4:2m\x1b[58;2;193;193;193m"[..], "doubled"),
        (CURLED, "curled"),
    ] {
        for px in [13.0, 16.0, 20.0, 24.0] {
            for style in ["beam", "phaser", "comet"] {
                let (n, u) = read(sgr, line, px, style, false);
                judged_n += n;
                under.extend(u);
            }
        }
    }
    assert!(judged_n > 1_000, "fixture: {judged_n} letter-frames judged");
    assert!(
        under.is_empty(),
        "{} letter-frames under their bar:\n{}",
        under.len(),
        under.join("\n")
    );
    let (_, control) = read(CURLED, "curled", 24.0, "phaser", true);
    assert!(
        !control.is_empty(),
        "control: with the light read off its glyph's raster, every letter over Phaser's curl at \
         24 px holds its bar"
    );
}

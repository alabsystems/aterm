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
use aterm_effects::kitty_pet::PetInputKind;
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
    for i in 0..n {
        let p = at(i);
        uniform &= p & 0x00FF_FFFF == first;
        chan[0][i] = (p >> 16) as u8;
        chan[1][i] = (p >> 8) as u8;
        chan[2][i] = p as u8;
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
    /// The letters' ink pixels (a clean frame's pixels off the page in a cell that holds a
    /// glyph) the body covers, and the cells they lie in `(row, col, px)`.
    pub ink_px: usize,
    pub cells: Vec<(usize, usize, usize)>,
    /// The cells holding a glyph that the pet's drawn box meets by a pixel or more, `(row,
    /// col)`: the box read as `aterm-lab trail-companion` reads a gallery frame's (its
    /// `on_letters`), whatever of the box the body's art fills.
    pub box_cells: Vec<(usize, usize)>,
    /// The caret cell's pixels the body covers.
    pub caret_px: usize,
    /// The word icons on glass, frame px, and the pixels of the pet's box that meet them.
    pub icons: Vec<(i32, i32, i32, i32)>,
    pub icon_px: usize,
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
    /// Frames to keep in [`Replay::kept`] as the glass shows them: their times, ms.
    pub keep: Vec<u64>,
    /// The frames [`Replay::keep`] names, in time order.
    pub kept: Vec<Kept>,
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
}

/// `cursor_glow_sdr_boost`'s default, the SDR crown's ([`Replay::post`]).
const SDR_BOOST: f32 = 0.25;

impl Replay {
    pub fn new(style: &str, look: Look, trail_color: TrailColor) -> Self {
        Self::sized(style, look, trail_color, ROWS)
    }

    /// [`Replay::new`] on a pane of `rows` rows.
    pub fn sized(style: &str, look: Look, trail_color: TrailColor, rows: u16) -> Self {
        let rend = Renderer::from_system(22.0, Theme::default())
            .expect("no font: build with aterm-render's default embedded-font feature");
        let (cw, ch) = rend.cell_size();
        let mut term = Terminal::new(rows, COLS);
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
        fx.set_cursor_glow(
            true,
            style,
            trail_color,
            None,
            260,
            24,
            if classic { 0.7 } else { 1.0 },
            0.6,
            true,
            look.cursor,
        );
        let pet = aterm_effects::cursor_glow::GlowStyle::style_names_any_pet(style);
        fx.set_cursor_pet(pet, 0x5EED);
        Self {
            term,
            fx,
            rend,
            cell: (cw, ch),
            look,
            ms: 0,
            clean: None,
            dump: Vec::new(),
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
            keep: Vec::new(),
            kept: Vec::new(),
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
        let (rows, cols) = (usize::from(self.rows), usize::from(COLS));
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
            coat.icons = self
                .fx
                .word_icon_boxes()
                .iter()
                .map(|&(x0, x1, y0, y1)| (x0 + px, x1 + px, y0 + py, y1 + py))
                .collect();
            if let Some((qx0, qx1, qy0, qy1)) = pet {
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
                    for (col, c) in line.iter().enumerate().take(cols) {
                        let is_caret = (row, col) == caret;
                        let glyph = !(c.ch == ' ' || c.ch == '\0' || c.ch.is_control());
                        if !glyph && !is_caret {
                            continue;
                        }
                        let (x0, y0) = (place.pad + col * cw, place.grid_top + row * ch);
                        if glyph
                            && !c.ch.is_whitespace()
                            && (x0 as i32) < qx1
                            && (x0 + cw) as i32 > qx0
                            && (y0 as i32) < qy1
                            && (y0 + ch) as i32 > qy0
                        {
                            coat.box_cells.push((row, col));
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
            self.kept.push(Kept {
                ms: self.ms,
                px: frame.pixels.clone(),
                w: frame.width,
                top: place.grid_top,
                clean: self.clean.as_ref().expect("rendered above").px.clone(),
                left: place.pad,
                caret,
                cell: self.cell,
                caret_ground: self.fx.thin_caret_ground(),
            });
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
                (0, Ev::Key(c, _)) => {
                    let input = self
                        .term
                        .cell_frame(usize::from(self.rows), usize::from(COLS));
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
pub const BURST_TEXT: &str = "echo the quick brown fox jumps over the lazy dog as the kitty flies";
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
}

impl Scene {
    /// The scene's pane rows: the `split` caret pane's 6, else the gallery window's [`ROWS`].
    pub fn rows(self) -> u16 {
        match self {
            Scene::SplitBurst => 6,
            _ => ROWS,
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
        if self == Scene::Context && luma(look.bg) > 0.5 {
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

pub const STYLES: [&str; 14] = [
    "rainbow kitty pet",
    "rainbow kitty underline",
    "rainbow dog",
    "flat rainbow",
    "nyan rainbow",
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
/// [`classic_in_a_chosen_colour_keeps_every_letter`] runs it. In [`Scene::Context`] a letter
/// whose own reading is under the bar on the bare page (the theme's own powerline segments and
/// selected row) is judged at [`OWN_SHARE`] of it. A guarded replay that keeps no picture leaves
/// out the blur its letters' judge cannot see ([`Replay::skip_spared_blur`]).
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
        r.fx.set_cursor_glow(
            true,
            style,
            color,
            None,
            260,
            24,
            1.0,
            0.6,
            true,
            look.cursor,
        );
    }
    r.fx.set_letter_guard(guard);
    r.fx.set_bloom_spares_letters(spares);
    r.post = true;
    r.own = scene == Scene::Context;
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
    let lanes = std::thread::available_parallelism().map_or(4, usize::from);
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
/// arm64, 2026-10-24). So on Windows the guard bears no load for Fire, and its rows would
/// control nothing there.
#[test]
fn without_the_letter_guard_the_trails_bury_letters() {
    let mut jobs = vec![
        ("laser", DARK, Scene::EnterBottom),
        ("beam", DARK, Scene::Typing),
        ("phaser", DARK, Scene::Burst),
        ("comet", DARK, Scene::Typing),
        ("lumen", DARK, Scene::Typing),
        ("sparkle", DARK, Scene::Typing),
        ("water", DARK, Scene::Typing),
        ("water", DEFAULT_LIGHT, Scene::Burst),
        ("nyan rainbow", DARK, Scene::Burst),
    ];
    if !cfg!(windows) {
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
    let lanes = std::thread::available_parallelism().map_or(4, usize::from);
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
/// Negative controls: read by its solid ink against the least and greatest luminance of its
/// grounds (`set_letter_guard_ring(false)`, the guard before round 27), the nyan rainbow's `p` on
/// Solarized Dark's wrapped line keeps 96.6% of its own reading, where a glow at the page's
/// lightness in another hue meets the page round a stroke and the ring's channel medians read
/// brighter than either; and the guard off buries a letter under the bar in each of four replays.
/// A letter whose glyph the face's table holds no raster for is held by
/// [`a_dim_letter_with_no_raster_keeps_its_own_reading`].
#[test]
fn a_dim_letter_keeps_its_own_reading() {
    #[derive(Clone, Copy)]
    struct Knobs {
        ring: bool,
    }
    const SHIPPED: Knobs = Knobs { ring: true };
    let least = |style: &'static str, look: Look, scene: Scene, k: Knobs| {
        let (ev, tail) = scene.events_on(look);
        let mut r = Replay::sized(style, look, TrailColor::Automatic, scene.rows());
        r.letters = true;
        r.fx.set_letter_guard_ring(k.ring);
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
                                < if scene == Scene::Context {
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
    let lanes = std::thread::available_parallelism().map_or(4, usize::from);
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
            for scene in BAR_SCENES {
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
    let glow_edge = [("nyan rainbow", SOLARIZED_DARK, Scene::Wrapped)];
    assert!(
        !under(&sweep(&glow_edge, Knobs { ring: false })).is_empty(),
        "control: read by its grounds' luminance, {glow_edge:?} still keeps 97% of every dim \
         letter's own reading"
    );
    let controls = [
        ("sparkle", DIM_GREY, Scene::Typing),
        ("rainbow kitty pet", DIM_GREY, Scene::Burst),
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
/// by its solid ink against its grounds' least and greatest luminance, not by the ring round its
/// strokes the judge reads, and keeps the floor's room over its share there (`OWN_ROOM`). Every
/// style but classic, typing and bursting such letters on Solarized Dark and the dim grey, the
/// host's bloom honoured, keeps every letter whose own reading is under the guard's floor at 97%
/// of it. Negative control: with no room over a letter read by its solid ink
/// (`set_letter_guard_room(false)`, the round's first fix), Fire typing them on Solarized Dark
/// leaves `м` at 95.7% of its own reading.
#[test]
fn a_dim_letter_with_no_raster_keeps_its_own_reading() {
    const TEXT: &str = "echo привет мир λόγος ζωή über straße";
    let least = |style: &'static str, look: Look, every: u64, room: bool| {
        let mut r = Replay::new(style, look, TrailColor::Automatic);
        r.letters = true;
        r.own = true;
        r.bloom = Some(false);
        r.fx.set_letter_guard_room(room);
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
        (style, look, every, least)
    };
    let mut jobs = Vec::new();
    for look in [SOLARIZED_DARK, DIM_GREY] {
        for style in STYLES.into_iter().filter(|s| *s != "classic") {
            for every in [60, 30] {
                jobs.push((style, look, every));
            }
        }
    }
    let lanes = std::thread::available_parallelism().map_or(4, usize::from);
    let least = &least;
    let verdicts: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(lanes).max(1))
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|&(style, look, every)| least(style, look, every, true))
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
        .filter(|(_, _, _, l)| l.share < 0.97)
        .map(|(style, look, every, l)| {
            format!(
                "{style} on {} a key every {every} ms, at {} ms: `{}` {:.2}:1 of its own {:.2} \
                 ({:.1}%)",
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
        verdicts.iter().all(|(_, _, _, l)| l.share.is_finite()),
        "fixture: every replay's lights lie on a dim letter"
    );
    let (_, _, _, bare) = least("fire", SOLARIZED_DARK, 60, false);
    assert!(
        bare.share < 0.97,
        "control: with no room over a letter read by its solid ink, Fire typing on Solarized Dark \
         still keeps `{}` at {:.1}% of its own reading",
        bare.ch,
        100.0 * bare.share
    );
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
    let lanes = std::thread::available_parallelism().map_or(4, usize::from);
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
            r.fx.set_cursor_glow(
                true,
                style,
                color,
                None,
                260,
                24,
                1.0,
                0.6,
                true,
                SOLARIZED_DARK.cursor,
            );
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
            "rainbow kitty underline",
            TrailColor::Automatic,
            Scene::Typing,
            true,
        ),
        (
            "rainbow kitty underline",
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
/// past the caret's own glow) are lit under the row (Comet's wedge is the shortest). Negative
/// control: with no light in the air (`set_bed_air_light(false)`), Beam's, Lumen's and
/// Comet's light no more than a quarter of a cell's.
#[test]
fn a_dim_page_s_trail_shows_below_its_letters() {
    let lit = |style: &'static str, air: bool| -> usize {
        let mut r = Replay::new(style, SOLARIZED_DARK, TrailColor::Automatic);
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
        after.rail[..behind]
            .iter()
            .filter(|&&v| v >= BELOW_LIT)
            .count()
    };
    let cw = Replay::new("off", SOLARIZED_DARK, TrailColor::Automatic)
        .cell
        .0;
    for style in ["beam", "lumen", "comet", "flat rainbow"] {
        let on = lit(style, true);
        assert!(
            on > cw / 2,
            "{style}: only {on} columns behind the caret lit below the row"
        );
        if style != "flat rainbow" {
            let off = lit(style, false);
            assert!(
                off <= cw / 4,
                "{style}: control: with no light in the air, {off} columns lit below the row"
            );
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

/// What the letter under the caret keeps: [`BAR`], or 97% of what the theme's own caret
/// leaves it where that is under the bar (the lab's dim-page rule).
fn caret_bar(own: f64) -> f64 {
    BAR.min(0.97 * own)
}

/// The least share of its bar the letter under the caret keeps through
/// [`Scene::MoveUp`] (`style` on `look`, the host's post passes drawn), with the caret-letter
/// law `off` turned off, and the frame and letter that kept it.
fn caret_letter_least(
    style: &'static str,
    look: Look,
    off: Option<aterm_effects::cursor_glow::CaretLetterLaw>,
) -> Option<(u64, Letter)> {
    let (ev, tail) = Scene::MoveUp.events();
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.post = true;
    r.caret = true;
    if let Some(law) = off {
        r.fx.set_caret_letter_law(law, false);
        r.fx.set_bloom_spares_letters(false);
    }
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
/// the caret at the bar ([`caret_bar`]) on the dark, GitHub Light, Default Light and Solarized
/// Dark pages, every frame the caret holds it. Negative controls, each law off on the dark
/// page and the post passes over the letters as the host drew them until looks10
/// (`BloomGate::SparesLetters` off): Laser's bolt kept over the letter hides it, Fire's ember
/// held to the page floor leaves it under the bar, and Water's light left on the caret's cell
/// lifts it under the bar (spared, Water keeps `!` at 4.79:1 with its light left on).
#[test]
fn the_letter_under_the_caret_reads() {
    use aterm_effects::cursor_glow::CaretLetterLaw;
    let mut jobs = Vec::new();
    for look in [DARK, GITHUB_LIGHT, DEFAULT_LIGHT, SOLARIZED_DARK] {
        for style in STYLES.iter().copied().filter(|s| *s != "classic") {
            jobs.push((style, look));
        }
    }
    let lanes = std::thread::available_parallelism().map_or(4, usize::from);
    let verdicts: Vec<CaretVerdict> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(lanes).max(1))
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|&(style, look)| (style, look, caret_letter_least(style, look, None)))
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
        let (ms, l) = caret_letter_least(style, DARK, Some(law)).expect("a caret on its letter");
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
    let mut r = Replay::new("rainbow kitty pet", DARK, TrailColor::Automatic);
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
/// unread): the decomposed row falls under it.
#[test]
fn a_decomposed_accent_under_the_foot_reads() {
    let (worst, under) = accented_row_under_the_foot("e\u{301}", false);
    assert!(worst.is_finite(), "fixture: the row's letters are judged");
    assert_eq!(
        under, 0,
        "{under} letter-frames of the decomposed row under {BAR}:1 (worst {worst:.2}:1)"
    );
    let (composed, composed_under) = accented_row_under_the_foot("\u{e9}", false);
    assert_eq!(
        composed_under, 0,
        "fixture: the precomposed row reads at the bar (worst {composed:.2}:1)"
    );
    let (old, old_under) = accented_row_under_the_foot("e\u{301}", true);
    assert!(
        old_under > 0,
        "control: the marks unread, the decomposed row still reads {old:.2}:1"
    );
}

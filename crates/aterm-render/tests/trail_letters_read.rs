// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Every letter a cursor trail lies on reads at the bar: the trail gallery's scenes replayed through
// the shipping effects pipeline (`EffectsPipeline::apply`, which runs a window's cursor family) and
// the real renderer, every 16 ms frame, each lettered cell judged as `aterm-lab trail-contrast`
// judges a gallery frame: the lower of the whole cell's median ink against its median ground and
// the worst pixel row's lower median of each solid ink pixel against its nearest ring of ground
// (`local_contrast`). A letter that lies on a companion's body (a knock layer: the pet, the dog,
// the kitty cursor, a word-cat) is judged as the lab judges a companion, by that ring alone: the
// body stands on the trail's light, opaque, and the knock-out cuts it away round the letter's ink
// so the letter reads against what lies under it (`companion_knock_out.rs`), so the cell's median
// ground is the coat, not a ground the trail lays.

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
const SOLID_SHARE: f64 = 0.85;
const CORE_SHARE: f64 = 0.70;
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

/// Each channel's median (the upper one of an even count): a channel is a byte, so it is the
/// first value whose running count passes half the pixels, with no sort of every ring.
fn median(px: &[u32]) -> u32 {
    let mid = px.len() / 2;
    let mut out = 0;
    for sh in [16, 8, 0] {
        let mut count = [0u32; 256];
        for p in px {
            count[((p >> sh) & 0xff) as usize] += 1;
        }
        let mut seen = 0;
        for (v, &n) in (0u32..).zip(&count) {
            seen += n as usize;
            if seen > mid {
                out |= v << sh;
                break;
            }
        }
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
    pub cell: f64,
    pub local: f64,
    /// Most of the cell lies on a companion's body.
    pub on_body: bool,
    /// Its clean twin judged the same way, its own reading, when the replay reads it
    /// ([`Replay::own`]); infinite otherwise.
    pub own: f64,
}

impl Letter {
    pub fn judged(&self) -> f64 {
        if self.on_body {
            self.local
        } else {
            self.cell.min(self.local)
        }
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

    /// The cell `(row, col)` as `aterm-lab trail-contrast` measures it; `None` for a blank cell or
    /// one the effects left as its twin.
    pub fn letter(&self, row: usize, col: usize, span: usize, ch: char) -> Option<Letter> {
        self.judge(row, col, span, ch, false)
    }

    /// The cell `(row, col)`'s own reading: its clean twin judged the same way.
    pub fn own(&self, row: usize, col: usize, span: usize, ch: char) -> Option<f64> {
        let twin = Pair {
            frame: self.clean,
            ..*self
        };
        twin.judge(row, col, span, ch, true).map(|l| l.judged())
    }

    fn judge(&self, row: usize, col: usize, span: usize, ch: char, twin: bool) -> Option<Letter> {
        let h = self.clean.len() / self.w;
        let (x0, y0) = (col * self.cw, row * self.ch);
        let (x1, y1) = ((x0 + span * self.cw).min(self.w), (y0 + self.ch).min(h));
        let mut same = true;
        let mut clean_px = Vec::with_capacity((x1 - x0) * (y1 - y0));
        let mut frame_px = Vec::with_capacity(clean_px.capacity());
        for y in y0..y1 {
            for x in x0..x1 {
                let (c, f) = (
                    Self::at(self.clean, self.w, x, y),
                    Self::at(self.frame, self.w, x, y),
                );
                same &= c == f;
                clean_px.push(c);
                frame_px.push(f);
            }
        }
        if (same && !twin) || clean_px.is_empty() {
            return None;
        }
        let ground = mode(&clean_px);
        let extreme = clean_px
            .iter()
            .copied()
            .max_by(|a, b| contrast(*a, ground).total_cmp(&contrast(*b, ground)))?;
        if contrast(extreme, ground) < BLANK_BELOW {
            return None;
        }
        let gl = luma(ground);
        let reach = CORE_SHARE * (luma(extreme) - gl).abs();
        let (mut ink, mut bed) = (Vec::new(), Vec::new());
        for (&c, &f) in clean_px.iter().zip(&frame_px) {
            if c == ground {
                bed.push(f);
            } else if (luma(c) - gl).abs() >= reach {
                ink.push(f);
            }
        }
        let cell = if ink.is_empty() || bed.is_empty() {
            f64::INFINITY
        } else {
            contrast(median(&ink), median(&bed))
        };
        let solid = SOLID_SHARE * contrast(extreme, ground);
        let mut worst_row = f64::INFINITY;
        let mut ratios = Vec::new();
        let mut ring = Vec::new();
        for y in y0..y1 {
            ratios.clear();
            for x in x0..x1 {
                if contrast(Self::at(self.clean, self.w, x, y), ground) < solid {
                    continue;
                }
                let mut g = None;
                for d in 1..=RING_REACH {
                    ring.clear();
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
                                ring.push(Self::at(self.frame, self.w, xx, yy));
                            }
                        }
                    }
                    if !ring.is_empty() {
                        g = Some(median(&ring));
                        break;
                    }
                }
                if let Some(g) = g {
                    ratios.push(contrast(Self::at(self.frame, self.w, x, y), g));
                }
            }
            if !ratios.is_empty() {
                ratios.sort_by(f64::total_cmp);
                worst_row = worst_row.min(ratios[(ratios.len() - 1) / 2]);
            }
        }
        Some(Letter {
            row,
            col,
            ch,
            cell,
            local: worst_row,
            on_body: false,
            own: f64::INFINITY,
        })
    }
}

impl Pair<'_> {
    /// The cell `(row, col)`'s bed: the mean over the clean twin's ground pixels of each one's
    /// largest channel rise in the frame, the light the effects laid on the page there.
    pub fn bed(&self, row: usize, col: usize) -> f64 {
        let h = self.clean.len() / self.w;
        let (x0, y0) = (col * self.cw, row * self.ch);
        let (x1, y1) = ((x0 + self.cw).min(self.w), (y0 + self.ch).min(h));
        let mut clean_px = Vec::with_capacity((x1 - x0) * (y1 - y0));
        for y in y0..y1 {
            for x in x0..x1 {
                clean_px.push(Self::at(self.clean, self.w, x, y));
            }
        }
        if clean_px.is_empty() {
            return 0.0;
        }
        let ground = mode(&clean_px);
        let (mut sum, mut n) = (0.0, 0usize);
        for y in y0..y1 {
            for x in x0..x1 {
                let c = Self::at(self.clean, self.w, x, y);
                if c != ground {
                    continue;
                }
                let f = Self::at(self.frame, self.w, x, y);
                let rise = [16, 8, 0]
                    .iter()
                    .map(|&sh| ((f >> sh) & 0xff) as i32 - ((c >> sh) & 0xff) as i32)
                    .max()
                    .unwrap_or(0)
                    .max(0);
                sum += f64::from(rise);
                n += 1;
            }
        }
        if n == 0 { 0.0 } else { sum / n as f64 }
    }
}

/// One cell's bed ([`Pair::bed`]), the cell's character with it.
#[derive(Clone, Copy, Debug)]
pub struct Bed {
    pub row: usize,
    pub col: usize,
    pub ch: char,
    pub lift: f64,
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
    pub letters: Vec<Letter>,
    /// Every cell's bed, row by row, when the replay reads them ([`Replay::beds`]).
    pub beds: Vec<Bed>,
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
    pub fn worst(&self) -> Option<Letter> {
        self.letters
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
    clean: Option<(u64, (usize, usize), Vec<u32>)>,
    /// Frames to keep as PNGs: `(ms, path)`.
    pub dump: Vec<(u64, String)>,
    /// Whether each frame reads every cell's bed ([`Judged::beds`]).
    pub beds: bool,
    /// Whether each frame reads each judged letter's own reading ([`Letter::own`]).
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
    /// Whether the keys reach the terminal without passing the console, as the trail
    /// gallery's `aterm ctl key` types them: their echo lands and the host notes nothing.
    pub keys_unnoted: bool,
    /// The frames' times, ms, in place of one every [`FRAME_MS`]: a headless instance
    /// advances its effects only when a frame renders, and the trail gallery renders one
    /// after each key and on its own steps ([`Scene::gallery_frames`]).
    pub frames_at: Option<Vec<u64>>,
    /// The pane's rows: [`ROWS`], or a scene's own ([`Scene::rows`]).
    pub rows: u16,
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
            beds: false,
            own: false,
            bloom: None,
            shade: false,
            air: false,
            rail: false,
            caret: false,
            post: false,
            coat: false,
            keys_unnoted: false,
            frames_at: None,
            rows,
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
        let (cw, ch) = self.cell;
        let (rows, cols) = (usize::from(self.rows), usize::from(COLS));
        let mut input: RenderInput = self.term.cell_frame(rows, cols);
        let caret = (input.cursor_row, input.cursor_col);
        let key = (self.term.screen_mutations(), caret);
        if self
            .clean
            .as_ref()
            .is_none_or(|(at, c, _)| (*at, *c) != key)
        {
            let clean = self.rend.render_input(&input).pixels;
            self.clean = Some((key.0, key.1, clean));
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
        if self.post {
            use aterm_render::{bloom, hdr};
            let bg = self.look.bg;
            if !input.bloom_gate.held() {
                let params = bloom::BloomParams {
                    strength: bloom::BLOOM_STRENGTH * hdr::light_page_bloom_share(bg),
                    radius: bloom::BLOOM_RADIUS,
                };
                bloom::apply(&mut frame, &input.cursor_glow_add, input.rows, params)
                    .expect("the frame blooms");
            }
            let budget = hdr::sdr_glow_budget(hdr::packed_luma(bg), SDR_BOOST);
            bloom::sdr_crown(&mut frame, &input.cursor_glow_add, input.rows, budget)
                .expect("the frame is crowned");
        }
        let place = self.rend.fx_frame(frame.width, frame.height, rows);
        let mut bodies = Vec::new();
        aterm_render::knock_rects(
            &place,
            &input.fx_layers,
            &input.fx_occluders,
            &mut aterm_render::FxSlicer::default(),
            &mut aterm_render::SpriteCols::default(),
            &mut bodies,
        );
        let mut coat = Coat::default();
        if self.coat {
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
                let clean = &self.clean.as_ref().expect("rendered above").2;
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
                        let mut n = 0usize;
                        for y in y0..(y0 + ch).min(h) {
                            for x in x0..(x0 + cw).min(w) {
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
        let w = frame.width;
        let clean = &self.clean.as_ref().expect("rendered above").2;
        let pair = Pair {
            frame: &frame.pixels,
            clean,
            w,
            cw,
            ch,
        };
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
                if let Some(mut l) = pair.letter(row, col, span, c.ch) {
                    let (x0, y0) = (
                        (place.pad + col * cw) as i32,
                        (place.grid_top + row * ch) as i32,
                    );
                    let (x1, y1) = (x0 + (span * cw) as i32, y0 + ch as i32);
                    let covered: i32 = bodies
                        .iter()
                        .map(|b| {
                            (b.x1.min(x1) - b.x0.max(x0)).max(0)
                                * (b.y1.min(y1) - b.y0.max(y0)).max(0)
                        })
                        .sum();
                    l.on_body = 2 * covered >= (x1 - x0) * (y1 - y0);
                    if self.own {
                        l.own = pair.own(row, col, span, c.ch).unwrap_or(f64::INFINITY);
                    }
                    letters.push(l);
                }
            }
        }
        let caret_letter = if self.caret {
            let (row, col) = caret;
            input
                .cells
                .get(row)
                .and_then(|line| line.get(col))
                .filter(|c| !c.wide && c.ch != ' ' && c.ch != '\0' && !c.ch.is_control())
                .and_then(|c| {
                    let mut l = pair.letter(row, col, 1, c.ch)?;
                    l.own = pair.own(row, col, 1, c.ch).unwrap_or(f64::INFINITY);
                    Some(l)
                })
        } else {
            None
        };
        let mut beds = Vec::new();
        if self.beds {
            for (row, line) in input.cells.iter().enumerate() {
                for (col, c) in line.iter().enumerate().take(cols) {
                    beds.push(Bed {
                        row,
                        col,
                        ch: c.ch,
                        lift: pair.bed(row, col),
                    });
                }
            }
        }
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
            beds,
            shade,
            air,
            rail,
            caret: caret_letter,
            coat,
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

    pub fn events(self) -> (Vec<(u64, Ev)>, u64) {
        let prompt = |at: u64| (at, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()));
        match self {
            Scene::Typing => {
                let mut ev = vec![prompt(200)];
                ev.extend(typed(TYPING_TEXT, 1_000, 60));
                (ev, 600)
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
                "{:.2} (cell {:.2} local {:.2}) at {ms} ms r{} c{} '{}'",
                l.judged(),
                l.cell,
                l.local,
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
/// or off, every frame judged at [`look_bar`]; `dump` names frames to keep as PNGs. A pinned
/// colour's Classic runs at the full default intensity, as
/// [`classic_in_a_chosen_colour_keeps_every_letter`] runs it.
pub fn replay_in(
    style: &'static str,
    look: Look,
    scene: Scene,
    guard: bool,
    dump: Vec<(u64, String)>,
    color: TrailColor,
) -> Verdict {
    let mut r = Replay::new(style, look, color);
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
    r.dump = dump;
    let (ev, tail) = scene.events();
    let judged = r.run(&ev, tail);
    let bar = look_bar(look);
    Verdict {
        style,
        look: r.look.name,
        scene,
        under: judged
            .iter()
            .map(|j| j.letters.iter().filter(|l| l.judged() < bar).count())
            .sum(),
        worst: worst(&judged),
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
const LOOKS: [Look; 3] = [DARK, GITHUB_LIGHT, DEFAULT_LIGHT];
const DIM_LOOKS: [Look; 3] = [DIM_GREY, SOLARIZED_DARK, PAPER_GREY];

/// Every style but classic (whose spectrum face keeps its v0.28 bytes), on dark, GitHub Light
/// and Default Light, through typing, a burst, an Enter at the bottom row (the echo and the
/// printed line every frame after it) and wide letters, keeps every letter it lies on at 4.5:1
/// or better, frame by frame.
#[test]
fn every_letter_a_trail_lies_on_reads_at_the_bar() {
    let mut jobs = Vec::new();
    for style in STYLES.into_iter().filter(|s| *s != "classic") {
        for look in LOOKS {
            for scene in SCENES {
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
    // Not vacuous: the scenes put the trails on letters and the judge read them.
    assert!(verdicts.iter().all(|v| v.worst.is_some()));
}

/// Negative control: with the letter guard off, each of these replays buries a letter (the
/// ribbon's and Laser's Enter flares across the printed line, the caret blooms on the newest
/// letters, Water's wave, the ribbon head's bloom in the burst, Fire's spark on a wide letter).
#[test]
fn without_the_letter_guard_the_trails_bury_letters() {
    let jobs = [
        ("flat rainbow", DEFAULT_LIGHT, Scene::EnterBottom),
        ("laser", DARK, Scene::EnterBottom),
        ("beam", DARK, Scene::Typing),
        ("phaser", DARK, Scene::Burst),
        ("comet", DARK, Scene::Typing),
        ("lumen", DARK, Scene::Typing),
        ("sparkle", DARK, Scene::Typing),
        ("water", DARK, Scene::Typing),
        ("water", DEFAULT_LIGHT, Scene::Burst),
        ("nyan rainbow", DARK, Scene::Burst),
        ("fire", GITHUB_LIGHT, Scene::Cjk),
        ("fire", DEFAULT_LIGHT, Scene::Cjk),
    ];
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
                .map(|j| j.letters.iter().filter(|l| l.judged() < bar).count())
                .sum(),
            worst: worst(&judged),
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
/// Classic's paper face by, as it did, leaves a letter beside the `move-up` landing under the bar
/// on each.
#[test]
fn classic_on_paper_keeps_every_letter() {
    let run = |look: Look, scene: Scene, guarded: bool| {
        let mut r = Replay::new("classic", look, TrailColor::Automatic);
        r.fx.set_letter_guard_classic_paper(guarded);
        let (ev, tail) = scene.events();
        let judged = r.run(&ev, tail);
        let bar = look_bar(look);
        Verdict {
            style: "classic",
            look: look.name,
            scene,
            under: judged
                .iter()
                .map(|j| j.letters.iter().filter(|l| l.judged() < bar).count())
                .sum(),
            worst: worst(&judged),
        }
    };
    for look in [GITHUB_LIGHT, DEFAULT_LIGHT] {
        for scene in SCENES.into_iter().chain([Scene::MoveUp]) {
            let v = run(look, scene, true);
            assert!(
                v.worst.is_some(),
                "fixture: the judge read the letters: {v}"
            );
            assert_eq!(
                v.under, 0,
                "a letter under Classic on paper reads under the bar: {v}"
            );
        }
        let old = run(look, Scene::MoveUp, false);
        assert!(
            old.under > 0,
            "negative control: the guard passing Classic's paper face by keeps every letter \
             (the control no longer controls): {old}"
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
/// 4.26:1 under Lumen) and, bloomed, takes letters under the bar under Lumen, Sparkle and
/// Phaser; laid under the text at its whole light, it spends the room the guard leaves a letter
/// and the bloom of the trail's own light takes one under the bar under Lumen and Laser.
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
        r.post = true;
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
                .map(|j| j.letters.iter().filter(|l| l.judged() < bar).count())
                .sum(),
            worst: worst(&judged),
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
    for style in ["lumen", "laser"] {
        let whole = run(style, false, Some(1.0));
        assert!(
            whole.under > 0,
            "negative control: the rim under the text at its whole light keeps every letter (the \
             control no longer controls): {whole}"
        );
    }
}

/// On [`DIM_GREY`] and [`SOLARIZED_DARK`], whose solid ink reads under the guard's floor on the
/// bare page (a guard that gave up on it would leave every light whole), every style but classic,
/// through every scene, keeps each letter it lies on at four fifths of the theme's own contrast
/// ([`look_bar`]). Negative control: the guard off buries a letter under that bar in each control
/// replay.
#[test]
fn every_letter_on_a_dim_theme_keeps_its_own_reading() {
    let mut jobs = Vec::new();
    for look in [DIM_GREY, SOLARIZED_DARK] {
        for style in STYLES.into_iter().filter(|s| *s != "classic") {
            for scene in SCENES {
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
        "letters under four fifths of their theme's contrast:\n{}",
        failed.join("\n")
    );
    // Every style lays light on some letter it is judged by (a scene may lay
    // none: the guard cuts Sparkle's stars off a dim theme's letters whole).
    let unlit: Vec<&str> = STYLES
        .into_iter()
        .filter(|s| *s != "classic")
        .filter(|s| !verdicts.iter().any(|v| v.style == *s && v.worst.is_some()))
        .collect();
    assert!(
        unlit.is_empty(),
        "styles whose lights reach no letter on a dim theme (the law judges nothing): {unlit:?}"
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
/// gallery judges it, its clean twin by the same judge. At the bare share the guard reads
/// solid ink and the judge each glyph's ring, so a letter the guard passed read under its
/// share (the gallery's Solarized Dark Water typing: `u` at 4.39:1 of its own 4.57, 96%).
/// With the floor's room over the share (`OWN_ROOM`), every style but Classic (whose spectrum
/// face is ruled) typing, bursting, entering at the bottom and typing wide letters on
/// Solarized Dark, the host's bloom honoured, keeps every such letter at 97% of its own
/// reading.
///
/// looks7 round 2: a bed laid at the page's own luminance in its own hue reads no darker
/// than the page by luminance, yet where an edge of it crosses a letter the judge's ring takes
/// each channel's median of page and bed, brighter than either. The pet's veil, laid over the
/// lights, cut the bed back to the page along the silhouette's rows across the letters it
/// veiled (`#342b36` from `#002b36` and the flat rainbow's red `#341f20`); it lies beneath
/// them, on the pet's coat. And a cut light lays no more than the share the guard weighed:
/// rounded to the nearest byte, six of an Enter tube's bed slabs cut to an eighth each laid a
/// byte of light and lit the page under `fox` to `#00313c`.
///
/// Negative controls: at the bare share the flat rainbow's burst leaves `f` at 3.77:1 of its
/// own 4.06 (92.9%); with its cuts rounded, Comet's Enter leaves `o` at 3.98:1 of its own
/// 4.28 (93.1%); the veil over the lights leaves the rainbow kitty pet's `o` typing wide
/// letters at 98.1% of its own 4.28, where beneath them its least letter keeps 99.98% (the
/// pet on its old paths: since looks8 round 3 the first keys spring it off the letters it
/// lay on, and no letter of the scene lies under its veil). Over
/// the lights the veil once took the flat rainbow's burst `o` to 90.7%; since a letter on a
/// companion's body is judged by its ring (`companion_knock_out.rs`) and the pet keeps off
/// the lettered row in a burst (looks8 K1), no scene takes one under 97% with it there.
#[test]
fn a_dim_letter_keeps_its_own_reading() {
    #[derive(Clone, Copy)]
    struct Knobs {
        room: bool,
        veil_beneath: bool,
        cuts_rounded: bool,
        old_paths: bool,
    }
    const SHIPPED: Knobs = Knobs {
        room: true,
        veil_beneath: true,
        cuts_rounded: false,
        old_paths: false,
    };
    let least = |style: &'static str, scene: Scene, k: Knobs| {
        let (ev, tail) = scene.events();
        let mut r = Replay::new(style, SOLARIZED_DARK, TrailColor::Automatic);
        r.fx.set_letter_guard_room(k.room);
        r.fx.set_pet_veil_beneath(k.veil_beneath);
        r.fx.set_letter_guard_cuts_rounded(k.cuts_rounded);
        r.fx.set_pet_old_paths(k.old_paths);
        r.own = true;
        r.bloom = Some(false);
        let mut least = OwnShare {
            share: f64::INFINITY,
            ms: 0,
            ch: ' ',
            read: 0.0,
            own: 0.0,
        };
        for j in r.run(&ev, tail) {
            for l in &j.letters {
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
        (style, scene, least)
    };
    let lanes = std::thread::available_parallelism().map_or(4, usize::from);
    let least = &least;
    let under = |jobs: &[(&'static str, Scene)], k: Knobs| -> Vec<String> {
        let verdicts: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = jobs
                .chunks(jobs.len().div_ceil(lanes).max(1))
                .map(|chunk| {
                    s.spawn(move || {
                        chunk
                            .iter()
                            .map(|&(style, scene)| least(style, scene, k))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().expect("a replay lane"))
                .collect()
        });
        verdicts
            .iter()
            .filter(|(_, _, l)| l.share < 0.97)
            .map(|(style, scene, l)| {
                format!(
                    "{style} {scene:?} at {} ms: `{}` {:.2}:1 of its own {:.2} ({:.1}%)",
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
    for style in STYLES.iter().copied().filter(|s| *s != "classic") {
        for scene in SCENES {
            jobs.push((style, scene));
        }
    }
    let fell = under(&jobs, SHIPPED);
    assert!(
        fell.is_empty(),
        "dim letters under 97% of their own reading:\n{}",
        fell.join("\n")
    );
    let bare = under(
        &jobs,
        Knobs {
            room: false,
            ..SHIPPED
        },
    );
    assert!(
        !bare.is_empty(),
        "control: at the bare share every dim letter still keeps 97% of its own reading"
    );
    let rounded = [("comet", Scene::EnterBottom)];
    assert!(
        !under(
            &rounded,
            Knobs {
                cuts_rounded: true,
                ..SHIPPED
            }
        )
        .is_empty(),
        "control: with its cuts rounded, {rounded:?} still keeps 97% of every dim letter's own \
         reading"
    );
    let share = |k: Knobs| least("rainbow kitty pet", Scene::Cjk, k).2.share;
    let old = Knobs {
        old_paths: true,
        ..SHIPPED
    };
    let (beneath, over) = (
        share(old),
        share(Knobs {
            veil_beneath: false,
            ..old
        }),
    );
    assert!(
        over + 0.01 < beneath,
        "control: the pet's veil over the lights keeps {over:.4} of a dim letter's own \
         reading, beneath them {beneath:.4}"
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

/// looks7 M4, the letter guard's part: cutting letter by letter, the guard let a word gap
/// between cut letters climb the ramp's slope from each cut, and past the run's last cut keep
/// its whole light beside a cut letter. A cell's kept light is its bed with the guard over its
/// bed without it, the same replay frame by frame (a glyph covers part of a lettered cell's
/// ground either way). Through the typing scene, every frame, a one-column gap between two
/// lettered cells the bar lies on keeps no more than the line between its letters' kept light
/// (their mean), but for the cuts' feather (2.3% of its light over it at worst); climbing the
/// slope and whole past the run (the negative control), it keeps 5.2% over. The gallery's
/// stripe itself lay in the bed, the guard on or off alike: its gap knot the bar's most vivid
/// blue (`cursor_glow::tests::streak_bed::phaser_s_dark_bed_lays_no_vivid_knot_in_a_word_gap`).
#[test]
fn phaser_s_dark_bar_keeps_no_stripe_in_a_word_gap() {
    let (ev, tail) = Scene::Typing.events();
    let replay = |guard: bool, line: bool| {
        let mut r = Replay::new("phaser", DARK, TrailColor::Automatic);
        r.fx.set_letter_guard(guard);
        r.fx.set_letter_guard_word_gaps(line);
        r.beds = true;
        r.run(&ev, tail)
    };
    let bare = replay(false, true);
    let stripe = |line: bool| -> (f64, usize) {
        let held = replay(true, line);
        assert_eq!(held.len(), bare.len(), "the replays' frames pair up");
        let (mut worst, mut gaps) = (0.0f64, 0usize);
        for (on, off) in held.iter().zip(&bare) {
            assert_eq!((on.ms, on.beds.len()), (off.ms, off.beds.len()));
            for (w, b) in on.beds.windows(3).zip(off.beds.windows(3)) {
                let (a, g, c) = (w[0], w[1], w[2]);
                let side_by_side = a.row == c.row && a.col + 1 == g.col && g.col + 1 == c.col;
                let lettered = |ch: char| ch.is_alphanumeric();
                if !side_by_side || g.ch != ' ' || !lettered(a.ch) || !lettered(c.ch) {
                    continue;
                }
                if b.iter().any(|x| x.lift < 16.0) {
                    continue;
                }
                let kept = |i: usize| w[i].lift / b[i].lift;
                gaps += 1;
                worst = worst.max(kept(1) - 0.5 * (kept(0) + kept(2)));
            }
        }
        (worst, gaps)
    };
    let (worst, gaps) = stripe(true);
    let climbed = stripe(false);
    assert!(
        gaps >= 10,
        "fixture: the bar lay across only {gaps} word gaps"
    );
    assert!(
        worst <= 0.035,
        "a word gap keeps {worst:.3} more of its light than the line between its letters"
    );
    assert!(
        climbed.0 >= 0.045,
        "control: climbing the slope a word gap keeps only {:.3} over the line between its letters",
        climbed.0
    );
}

#[test]
#[ignore = "survey: prints every style's worst letter (TRAIL_SURVEY_STYLES, TRAIL_LOOKS=dark|light|dim, \
            TRAIL_GUARD=0, TRAIL_COLOR=#rrggbb, TRAIL_DUMP=<dir>, TRAIL_DUMP_MS=<ms,...>)"]
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
    let mut jobs = Vec::new();
    for style in STYLES {
        if want.as_ref().is_some_and(|w| !w.iter().any(|x| x == style)) {
            continue;
        }
        for &look in &looks {
            for scene in SCENES {
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

/// [`median`] is each channel's sorted middle (`sorted[len / 2]`) and [`mode`] the first longest
/// run of the sorted colours, for odd and even counts, repeated values, ties and channels that
/// differ in order. NEGATIVE CONTROLS: `seen >= mid` takes the value below the middle of an even
/// count, and `c > best` takes a tie's highest colour; each fails.
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
    for len in 1..=40 {
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
        assert_eq!(median(&px), sorted_middle(&px), "{px:08x?}");
        assert_eq!(mode(&px), sorted_mode(&px), "{px:08x?}");
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
                off.is_some_and(|(share, ..)| share < 0.3),
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
/// page: Laser's bolt kept over the letter hides it, Fire's ember held to the page floor
/// leaves it under the bar, and Water's light left on the caret's cell lifts it under the bar.
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

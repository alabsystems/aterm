// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless frame capture of the RAINBOW KITTY v2 engine
//! ([`aterm_effects::rainbow_kitty`]) — the offline "after" twin of the
//! `aterm ctl video` captures in `docs/design/RAINBOW-KITTY-V2.md` §21.2, so
//! the engine's output can be audited frame by frame without a window.
//!
//! Drives the engine exactly as the seam does — `on_event` at the key's edge,
//! the glyph probe written every tick from the typed text, one `tick` per
//! 120 Hz frame on a deterministic clock — through four scenarios on ONE
//! continuous engine, and composites every captured frame to a PNG at the
//! owner's retina cell (`cw 15`, `ch 28` device px) over a dark ground:
//!
//! * **A — prose.** `echo the quick brown fox jumps over the lazy dog` at
//!   70 ms/key, then 1.5 s idle; a frame every 4th tick (33 ms).
//! * **B — meteor.** A same-row Ctrl-A (the line's end → column 0), then a
//!   Ctrl-E back 400 ms later; a frame EVERY tick from the first spawn to the
//!   second's `T + 350 ms`. With `RK_FRAMES_CTRL_A_ONLY=1` in the environment
//!   the Ctrl-E is dropped and B runs the lone Ctrl-A out to +720 ms, so one
//!   impact can be read to its end with nothing retiring it (the owner's
//!   2026-09-08 "bigger, more special rainbow impact" audit: frames at +0,
//!   +8, +50, +100, +200, +350 and +500 ms are copied out as
//!   `ctrl_a_T+<ms>ms.png` beside their 4× zooms).
//! * **C — erase.** Six Backspaces at 80 ms/key; a frame every tick.
//! * **D — Codex streaming** (§27). The engine is re-seated with the composer
//!   on row 1; fourteen keys at 70 ms/key, then a ROW-BAND MOVE every 30 ms
//!   for 600 ms with three more keys interleaved: the viewport — the row
//!   above the composer down to the bottom row — slides down one row per
//!   streamed line and the composer, its text and its light ride along
//!   (phase A); once the composer sits on the bottom row each further line
//!   archives the transcript above it up one row under the pinned composer
//!   (phase B, Codex's own A→B transition). A frame every tick; the contact
//!   sheet shows the band sliding with the caret and never vanishing — before
//!   the band path every line reset it. `RK_FRAMES_CODEX=0` skips it.
//!
//! Beside the frames: `index.json` (one row per frame), `stats.csv` (one row
//! per TICK — stream counts, live pools, fingerprint, luminance), a contact
//! sheet per scenario (every 3rd frame, four columns) and 4× nearest-neighbour
//! zooms of the five frames the audit reads first.
//!
//! The rasterizer is the CPU reference's law, not a look-alike: `under` quads
//! composite BENEATH the glyph ink and `out` quads above it, both through
//! `aterm_render`'s own `add_sat` / `over_premul`; halos through its
//! `halo_row_ny` / `halo_weight` falloff with every `Add` before every `Over`.
//! The caret block is the host's (v2 supplies only the caret seam, §7.1): a stand-in
//! that reads the seam the way the host's block would — white on the flare
//! frame, relaxing onto the field stop on `spring-snap`, lit by `paint`.
//!
//!   targo --unverified run -p aterm-effects --example rainbow_kitty_v2_frames --release -- <out_dir>

use std::fmt::Write as _;
use std::time::Duration;

use aterm_time::Instant;

use aterm_effects::cursor_glow::{Geom, SoundCue, band_pos};
use aterm_effects::rainbow_kitty::timing::{flight, spring_snap};
use aterm_effects::rainbow_kitty::{
    CaretSeam, Config, Dir, Engine, Event, Frame, Licence, TypedClass, meteor,
};
use aterm_effects::spectrum::spectrum;
use aterm_effects::trail_sound::SoundKind;
use aterm_render::{
    BeamVertex, GlowQuad, HaloMode, RainHalo, add_sat, halo_over_cap, halo_row_ny, halo_weight,
    over_premul, over_rgb, premul_rgb,
};

/// Cell width in device px — the owner's retina cell (§21.1).
const CW: usize = 15;
/// Cell height in device px.
const CH: usize = 28;
/// Grid columns: the 48-cell line plus room for a landing fan's reach.
const COLS: usize = 64;
/// Grid rows: five rows of sky above the line, two below.
const ROWS: usize = 8;
/// The row the prose is typed on.
const TEXT_ROW: u16 = 5;
/// The dark ground, `(17, 19, 24)`.
const GROUND: u32 = 0x0011_1318;
/// Glyph ink — the engine tests' theme foreground.
const INK: u32 = 0x00E8_E8F0;
/// The idle caret block: a neutral the theme's cursor colour stands in for.
const CARET_IDLE: u32 = 0x0058_5A66;
/// One 120 Hz tick.
const TICK_US: u64 = 8_333;
/// Scenario A's key cadence.
const KEY_MS: u64 = 70;
/// Scenario C's key cadence.
const ERASE_MS: u64 = 80;
/// What scenario A types.
const TEXT: &str = "echo the quick brown fox jumps over the lazy dog";
/// The nearest-neighbour zoom factor of the audit crops.
const ZOOM: usize = 4;
/// Contact-sheet columns.
const SHEET_COLS: usize = 4;
/// Contact-sheet gutter, px.
const GUTTER: usize = 6;
/// Height of a tile's label strip, px (one 8×8 glyph row plus padding).
const LABEL_H: usize = 12;
/// A `out` quad or halo no wider than this on both axes is a POINT MARK (a
/// star body, an arm stub, a pin nucleus) for the over-ink census; the hot
/// edge and the caret's own light are cell-wide and excluded by it.
const POINT_MARK_PX: u16 = 12;

// ===========================================================================
// Pixels
// ===========================================================================

/// An RGB frame as `0x00RRGGBB` words, so `aterm_render`'s integer blend
/// helpers apply verbatim.
struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u32>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![GROUND; w * h],
        }
    }

    fn clear(&mut self) {
        self.px.fill(GROUND);
    }

    /// One [`GlowQuad`], clipped: additive at `alpha == 0`, premultiplied
    /// source-over otherwise — the one blend both modes are.
    fn quad(&mut self, q: &GlowQuad) {
        let x_end = (usize::from(q.x) + usize::from(q.w)).min(self.w);
        let y_end = (usize::from(q.y) + usize::from(q.h)).min(self.h);
        for y in usize::from(q.y)..y_end {
            for x in usize::from(q.x)..x_end {
                let p = &mut self.px[y * self.w + x];
                *p = if q.alpha == 0 {
                    add_sat(*p, q.color)
                } else {
                    over_premul(*p, q.color, q.alpha)
                };
            }
        }
    }

    /// One [`RainHalo`] under the CPU reference's radial law — the same
    /// `halo_row_ny` / `halo_weight` the renderer and the GPU shader share,
    /// without the reference's span-skip (which only skips pixels whose weight
    /// is already zero, so this is byte-identical).
    fn halo(&mut self, a: &RainHalo) {
        let rx2 = i32::from(a.rx) * i32::from(a.rx);
        let ry2 = i32::from(a.ry) * i32::from(a.ry);
        if rx2 == 0 || ry2 == 0 {
            return;
        }
        let (cx, cy) = (i32::from(a.cx), i32::from(a.cy));
        let cap = halo_over_cap(a.color);
        let x_end = (usize::from(a.x) + usize::from(a.w)).min(self.w);
        let y_end = (usize::from(a.y) + usize::from(a.h)).min(self.h);
        for y in usize::from(a.y)..y_end {
            let ny = halo_row_ny(y as i32 - cy, ry2);
            if ny >= 256 {
                continue;
            }
            for x in usize::from(a.x)..x_end {
                let wt = halo_weight(x as i32 - cx, ny, rx2);
                if wt == 0 {
                    continue;
                }
                let wt = wt.min(255) as u8;
                let p = &mut self.px[y * self.w + x];
                *p = match a.mode {
                    HaloMode::Add => add_sat(*p, premul_rgb(a.color, wt)),
                    HaloMode::Over => over_rgb(*p, a.color, wt.min(cap)),
                };
            }
        }
    }

    fn rect(&mut self, x: usize, y: usize, rw: usize, rh: usize, color: u32) {
        let x_end = (x + rw).min(self.w);
        let y_end = (y + rh).min(self.h);
        for py in y..y_end {
            for px in x..x_end {
                self.px[py * self.w + px] = color;
            }
        }
    }

    /// An 8×8 font bitmap scaled (nearest) into `gw × gh` at `(gx, gy)`.
    fn bitmap(&mut self, c: char, at: (usize, usize), size: (usize, usize), color: u32) {
        let i = c as usize;
        let bm: [u8; 8] = if i < 128 {
            font8x8::legacy::BASIC_LEGACY[i]
        } else {
            [0; 8]
        };
        let (gx, gy) = at;
        let (gw, gh) = size;
        for py in 0..gh {
            let sy = py * 8 / gh;
            for px in 0..gw {
                let sx = px * 8 / gw;
                if bm[sy] & (1 << sx) != 0 {
                    let (x, y) = (gx + px, gy + py);
                    if x < self.w && y < self.h {
                        self.px[y * self.w + x] = color;
                    }
                }
            }
        }
    }

    /// A glyph inset in its cell, as the sibling demos draw one.
    fn glyph(&mut self, col: usize, row: usize, c: char) {
        self.bitmap(c, (col * CW + 2, row * CH + 4), (CW - 4, CH - 8), INK);
    }

    /// A 1:1 label in the 8×8 font.
    fn label(&mut self, x: usize, y: usize, text: &str) {
        for (i, c) in text.chars().enumerate() {
            self.bitmap(c, (x + i * 9, y), (8, 8), INK);
        }
    }

    /// Blit `src` at `(x, y)`, clipped.
    fn blit(&mut self, src: &Canvas, x: usize, y: usize) {
        for sy in 0..src.h.min(self.h.saturating_sub(y)) {
            let dst = (y + sy) * self.w + x;
            let n = src.w.min(self.w.saturating_sub(x));
            self.px[dst..dst + n].copy_from_slice(&src.px[sy * src.w..sy * src.w + n]);
        }
    }

    /// Nearest-neighbour upscale by `k`.
    fn zoom(&self, k: usize) -> Canvas {
        let mut z = Canvas::new(self.w * k, self.h * k);
        for y in 0..z.h {
            let src = &self.px[(y / k) * self.w..(y / k + 1) * self.w];
            for (x, p) in z.px[y * z.w..(y + 1) * z.w].iter_mut().enumerate() {
                *p = src[x / k];
            }
        }
        z
    }

    /// `(Σ max-channel above the ground, brightest max-channel)`.
    fn luminance(&self) -> (u64, u8) {
        let floor = max_channel(GROUND);
        let mut sum = 0u64;
        let mut max = 0u8;
        for &p in &self.px {
            let m = max_channel(p);
            sum += u64::from(m.saturating_sub(floor));
            max = max.max(m);
        }
        (sum, max)
    }

    fn write_png(&self, path: &str) {
        let mut rgb = Vec::with_capacity(self.px.len() * 3);
        for &p in &self.px {
            rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
        let file = std::fs::File::create(path).unwrap_or_else(|e| panic!("create {path}: {e}"));
        let mut enc =
            aterm_png::Encoder::new(std::io::BufWriter::new(file), self.w as u32, self.h as u32);
        enc.set_color(aterm_png::ColorType::Rgb);
        enc.set_depth(aterm_png::BitDepth::Eight);
        enc.write_header()
            .and_then(|mut w| w.write_image_data(&rgb))
            .unwrap_or_else(|e| panic!("write {path}: {e}"));
    }
}

fn max_channel(p: u32) -> u8 {
    ((p >> 16) as u8).max((p >> 8) as u8).max(p as u8)
}

/// Per-channel lerp, round-half.
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |sh: u32| -> u32 {
        let (x, y) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
        (x + (y - x) * t).round() as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// The 30°-bucket hue of a lit colour, or `None` for a dark one — the arc
/// census (§21.3 "Arc span") reads distinct buckets across the train.
fn hue_bucket(color: u32) -> Option<u8> {
    let (r, g, b) = (
        ((color >> 16) & 0xff) as f32,
        ((color >> 8) & 0xff) as f32,
        (color & 0xff) as f32,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max < 24.0 || max - min < 8.0 {
        return None;
    }
    let d = max - min;
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    Some(((h / 30.0) as u8) % 12)
}

// ===========================================================================
// The simulated host
// ===========================================================================

/// The host's frame scratch, reused across ticks exactly as `CursorGlow`
/// reuses its own.
#[derive(Default)]
struct Scratch {
    under: Vec<GlowQuad>,
    out: Vec<GlowQuad>,
    halos: Vec<RainHalo>,
    beams: Vec<BeamVertex>,
    cues: Vec<SoundCue>,
}

/// What one tick produced, for `stats.csv` and the summary.
#[derive(Clone, Default)]
struct TickStats {
    under: usize,
    out: usize,
    halos: usize,
    stars: u32,
    meteors: u32,
    fp: u64,
    cues: String,
    paint: f32,
    flare: bool,
    /// The seam's `field_t` — the caret's own stop this frame (C2).
    field_t: f32,
    /// `out` quads 2-12 px on both axes whose centre sits on an inked cell of
    /// the text row other than the caret's — a star BODY over ink (L4).
    point_over_ink: usize,
    /// The same census for 1-px-thin quads up to 12 px: star arms and
    /// hairlines — and, while a meteor flies, the train's per-column slabs.
    thin_over_ink: usize,
    /// Halos whose centre sits on such a cell.
    halos_over_ink: usize,
    /// A few of those marks spelled out, when no meteor is live (a flight's
    /// slabs over the line are the design, not a finding).
    offenders: String,
    /// Distinct hue buckets across the `under` stream — the train's arc.
    under_hues: usize,
    /// The widest achromatic halo — the meteor nucleus while one flies:
    /// `(cx, cy, rx)`.
    nucleus: Option<(u16, u16, u16)>,
    /// The flow state: heat, combo, high-water mark.
    flow: (f32, u32, u32),
    /// A meteor spawned this tick: its `T`, its landing column, its `cells`.
    spawn: Option<(Duration, u16, u16)>,
    /// On a spawn tick, seven `under` colours sampled left → right across
    /// the frame (the arc, §6.4).
    arc: String,
}

/// One driven engine on a deterministic 120 Hz clock.
struct Sim {
    eng: Engine,
    cfg: Config,
    geom: Geom,
    base: Instant,
    tick: u64,
    typed: Vec<char>,
    caret: (u16, u16),
    /// The row the typed text — the composer — is on: [`TEXT_ROW`] until a
    /// band move (scenario D) slides it.
    text_row: u16,
    sc: Scratch,
    seam: CaretSeam,
    probe: Vec<bool>,
}

impl Sim {
    fn new() -> Self {
        let mut eng = Engine::new();
        eng.set_engaged(true);
        Self {
            eng,
            cfg: Config {
                dark_theme: true,
                intensity: 1.0,
                duration: Duration::from_millis(900),
                ribbon_tall: true,
                theme_fg: INK,
                theme_bg: GROUND,
                reduced_motion: false,
            },
            geom: Geom {
                cw: CW,
                ch: CH,
                rows: ROWS,
                cols: COLS,
                origin_x: 0,
                origin_y: 0,
                win_w: (COLS * CW) as u16,
                win_h: (ROWS * CH) as u16,
                head: 0,
            },
            base: Instant::now(),
            tick: 0,
            typed: Vec::new(),
            caret: (TEXT_ROW, 0),
            text_row: TEXT_ROW,
            sc: Scratch::default(),
            seam: CaretSeam::default(),
            probe: vec![false; COLS],
        }
    }

    fn at(&self, tick: u64) -> Instant {
        self.base + Duration::from_micros(tick * TICK_US)
    }

    fn now(&self) -> Instant {
        self.at(self.tick)
    }

    /// Milliseconds since the run began, at `tick`.
    fn ms(tick: u64) -> f64 {
        (tick * TICK_US) as f64 / 1000.0
    }

    /// The first tick whose `now` is at or after `ms`.
    fn tick_of(ms: u64) -> u64 {
        (ms * 1000).div_ceil(TICK_US)
    }

    fn mv(&mut self, to: (u16, u16), licence: Licence) {
        let from = self.caret;
        self.eng.on_event(
            Event::Move {
                from,
                to,
                licence,
                dir: Dir::of(
                    i32::from(to.1) - i32::from(from.1),
                    i32::from(to.0) - i32::from(from.0),
                ),
            },
            self.now(),
        );
        self.caret = to;
    }

    /// A typed key: the echo's move under the typed licence, then the glyph.
    fn key(&mut self, c: char) {
        let to = (self.text_row, self.caret.1 + 1);
        self.mv(to, Licence::Typed);
        let class = if c == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        self.eng.on_event(
            Event::Typed {
                cells: 1,
                shifted: false,
                class,
            },
            self.now(),
        );
        self.typed.push(c);
    }

    /// A Backspace: `Erase` at the key, the retreat under the typed licence
    /// (the host's `mv.deletion` arm), the glyph gone from the probe.
    fn backspace(&mut self) {
        self.eng.on_event(Event::Erase, self.now());
        let to = (self.text_row, self.caret.1.saturating_sub(1));
        self.mv(to, Licence::Typed);
        self.typed.pop();
    }

    /// A nav-licensed same-row jump (Ctrl-A / Ctrl-E).
    fn nav(&mut self, col: u16) {
        self.mv((self.text_row, col), Licence::Nav);
    }

    /// A ROW-BAND MOVE (seam point 12, the band path) — the fixture's Codex.
    /// While the composer is above the bottom row the inline viewport — the
    /// row above the composer down to the bottom row — slides `d` rows, and
    /// the composer, its text and the caret ride along (phase A: `ESC[{vt};
    /// 57r … RI` in the capture, one row per streamed line). Once the
    /// composer sits on the bottom row a further line cannot slide it, and
    /// Codex archives instead: the transcript above the composer moves UP one
    /// row under the pinned composer (phase B: `ESC[1;52r … LF`). The probe
    /// is re-primed at once, as the host re-probes before the next deal (the
    /// engine's band path drops the sky's probe).
    fn band_move(&mut self, d: i16) {
        let last = ROWS as u16 - 1;
        let row = self.text_row;
        let slid = i32::from(row) + i32::from(d);
        if d > 0 && slid > i32::from(last) {
            if row >= 1 {
                self.eng.translate_band(0, row - 1, -1, CH as u16, 0);
            }
        } else {
            let top = row.saturating_sub(1);
            self.eng.translate_band(top, last, d, CH as u16, 0);
            self.text_row = band_pos(row, top, last, d);
            self.caret.0 = self.text_row;
        }
        self.probe_rows();
    }

    /// Re-seat the fixture for scenario D: a fresh session with the composer
    /// on `row`, nothing typed, nothing lit — the reset the band path exists
    /// to replace is used here on purpose, once, between scenarios, after the
    /// gap has let everything go dark.
    fn reseat(&mut self, row: u16) {
        self.eng.reset();
        self.typed.clear();
        self.text_row = row;
        self.caret = (row, 0);
        self.probe.fill(false);
    }

    /// Write the probe for the caret's row and its neighbours from the typed
    /// text (a space is not ink), as the host does before seam point 3.
    fn probe_rows(&mut self) {
        for (col, slot) in self.probe.iter_mut().enumerate() {
            *slot = self.typed.get(col).is_some_and(|&c| c != ' ');
        }
        let blank = vec![false; COLS];
        let row = i32::from(self.text_row);
        for r in [row - 1, row + 1] {
            self.eng.probe_mut().probe_row(r, &blank);
        }
        self.eng.probe_mut().probe_row(row, &self.probe);
    }

    /// Seam point 3 at this tick, then the clock advances one tick.
    fn step(&mut self) -> TickStats {
        self.probe_rows();
        self.sc.under.clear();
        self.sc.out.clear();
        self.sc.halos.clear();
        self.sc.cues.clear();
        let now = self.now();
        let mut fr = Frame {
            under: &mut self.sc.under,
            out: &mut self.sc.out,
            halos: &mut self.sc.halos,
            beams: &mut self.sc.beams,
            cues: &mut self.sc.cues,
            caret: CaretSeam::default(),
            companion: None,
            fp: 0,
        };
        self.eng.tick(now, self.geom, &self.cfg, &mut fr);
        self.seam = fr.caret;
        let _ = self.eng.take_companion_impulse();
        let st = self.eng.status();
        let mut cues = String::new();
        let mut spawn = None;
        for c in &self.sc.cues {
            if !cues.is_empty() {
                cues.push(' ');
            }
            // `Debug` spells a payload with commas; the CSV column may not.
            let _ = write!(cues, "{}", format!("{:?}", c.kind).replace(", ", "; "));
            if let SoundKind::Meteor { cells, .. } = c.kind {
                spawn = Some((flight(f32::from(cells)), c.col, cells));
            }
        }
        let (point_over_ink, thin_over_ink, halos_over_ink, offenders) =
            self.census(st.meteors == 0);
        let stats = TickStats {
            under: self.sc.under.len(),
            out: self.sc.out.len(),
            halos: self.sc.halos.len(),
            stars: st.stars,
            meteors: st.meteors,
            fp: st.fp,
            cues,
            paint: self.seam.paint,
            flare: self.seam.flare_at.is_some(),
            field_t: self.seam.field_t,
            point_over_ink,
            thin_over_ink,
            halos_over_ink,
            offenders,
            under_hues: {
                let mut seen = [false; 12];
                for q in &self.sc.under {
                    if let Some(h) = hue_bucket(q.color) {
                        seen[usize::from(h)] = true;
                    }
                }
                seen.iter().filter(|&&s| s).count()
            },
            nucleus: self.nucleus(),
            flow: (st.flow.heat, st.flow.combo, st.flow.best),
            spawn,
            arc: if spawn.is_some() {
                self.arc()
            } else {
                String::new()
            },
        };
        self.tick += 1;
        stats
    }

    /// True where a window pixel sits on an inked cell of the text row that
    /// is not the caret's.
    fn on_ink(&self, x: usize, y: usize) -> bool {
        let (row, col) = (y / CH, x / CW);
        row == usize::from(self.text_row)
            && col != usize::from(self.caret.1)
            && self.probe.get(col).copied().unwrap_or(false)
    }

    /// `(bodies, thin, halos, offenders)` over inked non-caret cells of the
    /// text row; `describe` spells the first few out.
    fn census(&self, describe: bool) -> (usize, usize, usize, String) {
        let mut bodies = 0;
        let mut thin = 0;
        let mut halos = 0;
        let mut text = String::new();
        let mut note = |s: String| {
            if describe && text.len() < 160 {
                if !text.is_empty() {
                    text.push_str(" | ");
                }
                text.push_str(&s);
            }
        };
        for q in &self.sc.out {
            if q.w > POINT_MARK_PX || q.h > POINT_MARK_PX {
                continue;
            }
            let cx = usize::from(q.x) + usize::from(q.w) / 2;
            let cy = usize::from(q.y) + usize::from(q.h) / 2;
            if !self.on_ink(cx, cy) {
                continue;
            }
            if q.w.min(q.h) == 1 {
                thin += 1;
            } else {
                bodies += 1;
            }
            note(format!(
                "q({},{} {}x{} #{:06x} a{})",
                q.x,
                q.y,
                q.w,
                q.h,
                q.color & 0x00FF_FFFF,
                q.alpha
            ));
        }
        for a in &self.sc.halos {
            if !self.on_ink(usize::from(a.cx), usize::from(a.cy)) {
                continue;
            }
            halos += 1;
            note(format!(
                "h({},{} r{}x{} #{:06x})",
                a.cx,
                a.cy,
                a.rx,
                a.ry,
                a.color & 0x00FF_FFFF
            ));
        }
        (bodies, thin, halos, text)
    }

    /// The meteor nucleus: the widest achromatic halo on the frame (the
    /// nucleus is `#FFFFFF` premultiplied; the coma and every star halo carry
    /// a tint).
    fn nucleus(&self) -> Option<(u16, u16, u16)> {
        self.sc
            .halos
            .iter()
            .filter(|a| {
                let (r, g, b) = (
                    (a.color >> 16) & 0xff,
                    (a.color >> 8) & 0xff,
                    a.color & 0xff,
                );
                r == g && g == b
            })
            .max_by_key(|a| a.rx)
            .map(|a| (a.cx, a.cy, a.rx))
    }

    /// Seven `under` colours sampled left → right across the frame, as
    /// `x:#rrggbb` — the train's arc on a spawn tick (and whatever ribbon is
    /// still dying beside it).
    fn arc(&self) -> String {
        let mut xs: Vec<(u16, u32)> = self.sc.under.iter().map(|q| (q.x, q.color)).collect();
        if xs.is_empty() {
            return String::new();
        }
        xs.sort_unstable_by_key(|&(x, _)| x);
        (0..7)
            .map(|i| {
                let (x, c) = xs[(i * (xs.len() - 1)) / 6];
                format!("{x}:#{:06x}", c & 0x00FF_FFFF)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The host's caret block's fill, read off the v2 seam (§7.1).
    fn caret_fill(&self) -> u32 {
        // `field_t` is the RAW classic walk — unbounded past 1.0 on a long
        // line — so it is folded through the family's one reflection before
        // `spectrum`, exactly as the ribbon head and the meteor's arc do (D4:
        // reflected, never clamped). Reading it raw painted the block violet
        // beside an orange ribbon head on every line longer than 16 cells.
        let lit = mix(
            CARET_IDLE,
            spectrum(meteor::tri(self.seam.field_t)),
            self.seam.paint,
        );
        match self.seam.flare_at {
            Some(at) => {
                // The seam was sampled at the tick BEFORE the clock advanced.
                let age = self.at(self.tick - 1).saturating_duration_since(at);
                if age < Duration::from_micros(TICK_US) {
                    0x00FF_FFFF
                } else {
                    mix(0x00FF_FFFF, lit, spring_snap(age.as_secs_f32()))
                }
            }
            None => lit,
        }
    }

    /// Composite the last tick: `under` → ink → caret block → `out` → halos
    /// (`Add` before `Over`).
    fn compose(&self, canvas: &mut Canvas) {
        canvas.clear();
        for q in &self.sc.under {
            canvas.quad(q);
        }
        for (col, &c) in self.typed.iter().enumerate() {
            canvas.glyph(col, usize::from(self.text_row), c);
        }
        canvas.rect(
            usize::from(self.caret.1) * CW,
            usize::from(self.caret.0) * CH,
            CW,
            CH,
            self.caret_fill(),
        );
        for q in &self.sc.out {
            canvas.quad(q);
        }
        for mode in [HaloMode::Add, HaloMode::Over] {
            for a in self.sc.halos.iter().filter(|a| a.mode == mode) {
                canvas.halo(a);
            }
        }
    }
}

// ===========================================================================
// Scenarios
// ===========================================================================

/// What the driver does on a tick.
#[derive(Clone, Copy)]
enum Action {
    Key(char),
    CtrlA,
    CtrlE,
    Backspace,
    /// A streamed line: the viewport's row band moves `d` rows
    /// ([`Sim::band_move`]).
    BandMove(i16),
}

impl Action {
    fn label(self) -> String {
        match self {
            Self::Key(' ') => "key space".to_string(),
            Self::Key(c) => format!("key {c}"),
            Self::CtrlA => "ctrl-a".to_string(),
            Self::CtrlE => "ctrl-e".to_string(),
            Self::Backspace => "backspace".to_string(),
            Self::BandMove(d) => format!("band {d:+}"),
        }
    }
}

/// One scenario: a schedule of actions on absolute ticks, a capture cadence
/// (`every == 0` captures nothing — a silent gap that only ticks), and the
/// tick it ends on (inclusive).
struct Scenario {
    name: &'static str,
    sched: Vec<(u64, Action)>,
    every: u64,
    end: u64,
}

/// One captured frame.
struct FrameRec {
    frame: usize,
    tick: u64,
    event: String,
    lum: (u64, u8),
}

/// The best-so-far frame of a selection, its pixels kept.
struct Best {
    key: u64,
    frame: usize,
    px: Option<Canvas>,
}

impl Best {
    fn new() -> Self {
        Self {
            key: 0,
            frame: usize::MAX,
            px: None,
        }
    }

    fn offer(&mut self, key: u64, frame: usize, canvas: &Canvas) {
        if self.px.is_none() || key > self.key {
            self.key = key;
            self.frame = frame;
            self.px = Some(Canvas {
                w: canvas.w,
                h: canvas.h,
                px: canvas.px.clone(),
            });
        }
    }
}

/// Everything a scenario run leaves behind for the summary.
struct RunOut {
    frames: Vec<FrameRec>,
    ticks: Vec<(u64, TickStats)>,
    /// Frame index → pixels, for the frames named in `keep`.
    kept: Vec<(usize, Canvas)>,
    peak: Best,
    densest: Best,
}

/// The output sinks shared by every scenario.
struct Sinks {
    dir: String,
    index: String,
    stats: String,
}

impl Sinks {
    fn new(dir: &str) -> Self {
        Self {
            dir: dir.to_string(),
            index: String::from("[\n"),
            stats: String::from(
                "scenario,tick,frame,t_ms,abs_ms,event,under,out,halos,stars,meteors,fp,cues,\
                 paint,flare,point_over_ink,under_hues,lum_sum,lum_max,field_t,thin_over_ink,\
                 halos_over_ink,nucleus_cx,nucleus_cy,nucleus_rx,flow,combo,best\n",
            ),
        }
    }
}

/// Run one scenario on the shared engine; `keep` names the frame indices whose
/// pixels the caller wants back (the zoom targets known up front).
fn run(sim: &mut Sim, sc: &Scenario, keep: &[usize], sinks: &mut Sinks) -> RunOut {
    let sub = format!("{}/{}", sinks.dir, sc.name);
    let start = sim.tick;
    // `every == 0` is a silent gap: no frames and no directory.
    let n_frames = (sc.end - start)
        .checked_div(sc.every)
        .map_or(0, |n| n as usize + 1);
    if n_frames > 0 {
        std::fs::create_dir_all(&sub).unwrap_or_else(|e| panic!("mkdir {sub}: {e}"));
    }
    let (w, h) = (COLS * CW, ROWS * CH);
    let mut canvas = Canvas::new(w, h);
    let mut sheet = Sheet::new(sc.name, n_frames.div_ceil(3), w, h);
    let mut out = RunOut {
        frames: Vec::new(),
        ticks: Vec::new(),
        kept: Vec::new(),
        peak: Best::new(),
        densest: Best::new(),
    };
    while sim.tick <= sc.end {
        let tick = sim.tick;
        let mut event = String::new();
        for &(_, action) in sc.sched.iter().filter(|&&(at, _)| at == tick) {
            match action {
                Action::Key(c) => sim.key(c),
                Action::CtrlA => sim.nav(0),
                Action::CtrlE => sim.nav(TEXT.len() as u16),
                Action::Backspace => sim.backspace(),
                Action::BandMove(d) => sim.band_move(d),
            }
            if !event.is_empty() {
                event.push(' ');
            }
            event.push_str(&action.label());
        }
        let st = sim.step();
        let captured = sc.every > 0 && (tick - start).is_multiple_of(sc.every);
        let mut frame_col = String::from("-");
        let mut lum = (0u64, 0u8);
        if captured {
            let frame = out.frames.len();
            frame_col = frame.to_string();
            sim.compose(&mut canvas);
            lum = canvas.luminance();
            canvas.write_png(&format!("{sub}/frame_{frame:04}.png"));
            if frame.is_multiple_of(3) {
                let t_ms = Sim::ms(tick) - Sim::ms(start);
                sheet.add(
                    &canvas,
                    &format!("{} f{frame:04} t={t_ms:.1}ms {event}", sc.name),
                );
            }
            if keep.contains(&frame) {
                out.kept.push((
                    frame,
                    Canvas {
                        w,
                        h,
                        px: canvas.px.clone(),
                    },
                ));
            }
            out.peak.offer(lum.0, frame, &canvas);
            out.densest.offer(u64::from(st.stars), frame, &canvas);
            let _ = writeln!(
                sinks.index,
                "  {{\"scenario\": \"{}\", \"frame\": {frame}, \"t_ms\": {:.3}, \"abs_ms\": {:.3}, \
                 \"tick\": {tick}, \"event\": \"{event}\"}},",
                sc.name,
                Sim::ms(tick) - Sim::ms(start),
                Sim::ms(tick),
            );
            out.frames.push(FrameRec {
                frame,
                tick,
                event: event.clone(),
                lum,
            });
            println!(
                "{} f{frame:04} tick={tick} t={:.1}ms under={} out={} halos={} stars={} meteors={} \
                 fp={:016x}{}{}",
                sc.name,
                Sim::ms(tick) - Sim::ms(start),
                st.under,
                st.out,
                st.halos,
                st.stars,
                st.meteors,
                st.fp,
                if event.is_empty() {
                    String::new()
                } else {
                    format!(" [{event}]")
                },
                if st.cues.is_empty() {
                    String::new()
                } else {
                    format!(" cues={{{}}}", st.cues)
                },
            );
        }
        let (ncx, ncy, nrx) = st.nucleus.map_or((-1, -1, -1), |(x, y, r)| {
            (i32::from(x), i32::from(y), i32::from(r))
        });
        let _ = writeln!(
            sinks.stats,
            "{},{tick},{frame_col},{:.3},{:.3},{event},{},{},{},{},{},{:016x},{},{:.3},{},{},{},{},{},{:.4},{},{},{ncx},{ncy},{nrx},{:.3},{},{}",
            sc.name,
            Sim::ms(tick) - Sim::ms(start),
            Sim::ms(tick),
            st.under,
            st.out,
            st.halos,
            st.stars,
            st.meteors,
            st.fp,
            st.cues,
            st.paint,
            u8::from(st.flare),
            st.point_over_ink,
            st.under_hues,
            lum.0,
            lum.1,
            st.field_t,
            st.thin_over_ink,
            st.halos_over_ink,
            st.flow.0,
            st.flow.1,
            st.flow.2,
        );
        out.ticks.push((tick, st));
    }
    sheet.write(&format!("{}/contact_{}.png", sinks.dir, sc.name));
    out
}

/// A contact sheet under construction: `SHEET_COLS` tiles across, each a
/// full frame under a one-line label.
struct Sheet {
    canvas: Canvas,
    tile: (usize, usize),
    n: usize,
    name: &'static str,
}

impl Sheet {
    fn new(name: &'static str, tiles: usize, w: usize, h: usize) -> Self {
        let rows = tiles.div_ceil(SHEET_COLS).max(1);
        let tile = (w + GUTTER, h + LABEL_H + GUTTER);
        Self {
            canvas: Canvas::new(SHEET_COLS * tile.0 + GUTTER, rows * tile.1 + GUTTER),
            tile,
            n: 0,
            name,
        }
    }

    fn add(&mut self, frame: &Canvas, label: &str) {
        let (col, row) = (self.n % SHEET_COLS, self.n / SHEET_COLS);
        let x = GUTTER + col * self.tile.0;
        let y = GUTTER + row * self.tile.1;
        self.canvas.label(x, y + 2, label);
        self.canvas.blit(frame, x, y + LABEL_H);
        self.n += 1;
    }

    fn write(&self, path: &str) {
        if self.n == 0 {
            return;
        }
        self.canvas.write_png(path);
        println!("wrote {path} ({} tiles of {})", self.n, self.name);
    }
}

/// The scenarios, on one clock: A from tick 0, B from A's end, a silent
/// 300 ms gap for B to go dark, C from its first Backspace, a 1.6 s gap (the
/// swoosh's whole life) for C to go dark, then D from its first key.
fn scenarios() -> [Scenario; 6] {
    let keys: Vec<(u64, Action)> = TEXT
        .chars()
        .enumerate()
        .map(|(i, c)| (Sim::tick_of((i as u64 + 1) * key_ms()), Action::Key(c)))
        .collect();
    let last_key = keys.last().map_or(0, |&(t, _)| t);
    let a_end = last_key + Sim::tick_of(1_500);

    let b0 = a_end + 1;
    let ctrl_e = b0 + Sim::tick_of(400);
    // `T + 350` past the second spawn, with `T = flight_ms(48)`.
    let t_flight = flight(TEXT.len() as f32).as_micros() as u64;
    let (b_sched, b_end) = if ctrl_a_only() {
        (vec![(b0, Action::CtrlA)], b0 + Sim::tick_of(720))
    } else {
        (
            vec![(b0, Action::CtrlA), (ctrl_e, Action::CtrlE)],
            ctrl_e + (t_flight + 350_000).div_ceil(TICK_US),
        )
    };

    let c0 = b_end + 1 + Sim::tick_of(300);
    let erases: Vec<(u64, Action)> = (0..6u64)
        .map(|k| (c0 + Sim::tick_of(k * ERASE_MS), Action::Backspace))
        .collect();
    let c_end = c0 + Sim::tick_of(5 * ERASE_MS + 500);

    // D: fourteen keys, then a streamed line every 30 ms for 600 ms with
    // three more keys interleaved, then 500 ms of run-out.
    let d0 = c_end + 1 + Sim::tick_of(1_600);
    let mut d_sched: Vec<(u64, Action)> = TEXT
        .chars()
        .take(14)
        .enumerate()
        .map(|(i, c)| (d0 + Sim::tick_of(i as u64 * key_ms()), Action::Key(c)))
        .collect();
    let stream0 = d0 + Sim::tick_of(14 * key_ms());
    d_sched.extend((0..20u64).map(|k| (stream0 + Sim::tick_of(k * 30), Action::BandMove(1))));
    d_sched.extend(TEXT.chars().skip(14).take(3).enumerate().map(|(i, c)| {
        (
            stream0 + Sim::tick_of(150 + i as u64 * 150) + 1,
            Action::Key(c),
        )
    }));
    d_sched.sort_by_key(|&(t, _)| t);
    let d_end = stream0 + Sim::tick_of(600 + 500);

    [
        Scenario {
            name: "A",
            sched: keys,
            every: 4,
            end: a_end,
        },
        Scenario {
            name: "B",
            sched: b_sched,
            every: 1,
            end: b_end,
        },
        Scenario {
            name: "gap",
            sched: Vec::new(),
            every: 0,
            end: c0 - 1,
        },
        Scenario {
            name: "C",
            sched: erases,
            every: 1,
            end: c_end,
        },
        Scenario {
            name: "gap2",
            sched: Vec::new(),
            every: 0,
            end: d0 - 1,
        },
        Scenario {
            name: "D",
            sched: d_sched,
            every: 1,
            end: d_end,
        },
    ]
}

/// `RK_FRAMES_CODEX=0`: skip scenario D (the A/B/C frames are unchanged by
/// it — D runs last, on a re-seated engine).
fn codex_frames() -> bool {
    !std::env::var("RK_FRAMES_CODEX").is_ok_and(|v| v == "0")
}

/// `RK_FRAMES_KEY_MS=<n>`: scenario A's key cadence, ms per key — [`KEY_MS`]
/// (70 ms, 14.3 cps) unless the environment names another. 83 ms is the
/// 12 cps hand the flow-state captures are read at.
fn key_ms() -> u64 {
    std::env::var("RK_FRAMES_KEY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(KEY_MS)
}

/// `RK_FRAMES_CTRL_A_ONLY=1`: scenario B is the lone Ctrl-A, run out to
/// +720 ms (see the module doc).
fn ctrl_a_only() -> bool {
    std::env::var("RK_FRAMES_CTRL_A_ONLY").is_ok_and(|v| v == "1")
}

/// The instants of the lone-Ctrl-A audit, ms after the spawn — the seven
/// frames the owner's impact review reads.
const CTRL_A_AUDIT_MS: [u64; 7] = [0, 8, 50, 100, 200, 350, 500];

/// For each meteor spawn in `ticks`: the spawn frame and its arc, the head's
/// first-frame travel (§21.3 "Head already moving"), the nucleus at arrival,
/// and when the colour train, the pool and the fingerprint went dark — each
/// read only up to the NEXT spawn, because a second flight inside the
/// first's life (the retire law, §6.9) shares the same counts.
fn meteor_report(ticks: &[(u64, TickStats)]) -> Vec<String> {
    let age = |t0: u64, t: u64, t_ms: f64| format!("T+{:.0}ms", Sim::ms(t - t0) - t_ms);
    let spawns: Vec<usize> = ticks
        .iter()
        .enumerate()
        .filter(|(_, (_, s))| s.spawn.is_some())
        .map(|(i, _)| i)
        .collect();
    let mut lines = Vec::new();
    for (k, &i) in spawns.iter().enumerate() {
        let (t0, st) = &ticks[i];
        let Some((t_flight, col, cells)) = st.spawn else {
            continue;
        };
        let t_ms = t_flight.as_secs_f64() * 1000.0;
        let next = spawns.get(k + 1).copied().unwrap_or(ticks.len());
        let window = &ticks[i..next];
        let head1 = ticks.get(i + 1).and_then(|(_, s)| s.nucleus);
        let arrive = i + (t_flight.as_micros() as u64).div_ceil(TICK_US) as usize;
        let head_t = ticks.get(arrive).and_then(|(_, s)| s.nucleus);
        let dx = match (st.nucleus, head1) {
            (Some(a), Some(b)) => i32::from(b.0) - i32::from(a.0),
            _ => 0,
        };
        let first = |pred: fn(&TickStats) -> bool| {
            window.iter().find(|(_, s)| pred(s)).map_or_else(
                || "not before the next spawn".to_string(),
                |(t, _)| age(*t0, *t, t_ms),
            )
        };
        let last_live = window
            .iter()
            .rev()
            .find(|(_, s)| s.meteors > 0)
            .map_or_else(|| "-".to_string(), |(t, _)| age(*t0, *t, t_ms));
        lines.push(format!(
            "meteor @tick {t0} → col {col} ({cells} cells): T={t_ms:.0}ms; spawn frame under={} out={} halos={} stars={} hues={} field_t={:.3}; arc {}",
            st.under, st.out, st.halos, st.stars, st.under_hues, st.field_t, st.arc
        ));
        lines.push(format!(
            "  nucleus f0={:?} f1={:?} (dx {dx} px vs 0.10·L = {} px) at T={:?} (landing x {}..{}); colour train gone {}; pool last live {last_live}; fp==0 {}",
            st.nucleus,
            head1,
            usize::from(cells) * CW / 10,
            head_t,
            usize::from(col) * CW,
            usize::from(col) * CW + CW,
            first(|s| s.under == 0),
            first(|s| s.fp == 0),
        ));
    }
    lines
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir {dir}: {e}"));
    let mut sinks = Sinks::new(&dir);
    let mut sim = Sim::new();
    let [a, b, gap, c, gap2, d] = scenarios();
    let t_flight = flight(TEXT.len() as f32);
    // B's pin frame: Ctrl-A's `T + 150 ms`, as a frame index from B's spawn.
    let pin_frame = ((t_flight.as_micros() as u64 + 150_000).div_ceil(TICK_US)) as usize;
    // With the lone Ctrl-A there is no second spawn: the Ctrl-E frame index
    // is parked past the run so no zoom claims it.
    let ctrl_e_frame = b
        .sched
        .get(1)
        .map_or(usize::MAX / 2, |&(t, _)| (t - b.sched[0].0) as usize);
    let pin_frame_e = ctrl_e_frame.saturating_add(pin_frame);
    let audit_frames: Vec<usize> = if ctrl_a_only() {
        CTRL_A_AUDIT_MS
            .iter()
            .map(|&ms| Sim::tick_of(ms) as usize)
            .collect()
    } else {
        Vec::new()
    };
    let mut keep_b = vec![0, pin_frame, pin_frame_e, ctrl_e_frame];
    keep_b.extend(audit_frames.iter().copied());

    let ra = run(&mut sim, &a, &[], &mut sinks);
    let rb = run(&mut sim, &b, &keep_b, &mut sinks);
    let _ = run(&mut sim, &gap, &[], &mut sinks);
    let rc = run(&mut sim, &c, &[2], &mut sinks);
    let rd = codex_frames().then(|| {
        let _ = run(&mut sim, &gap2, &[], &mut sinks);
        // D starts with the composer on row 1, so the band has six rows to
        // slide through before Codex's A→B transition pins it.
        sim.reseat(1);
        run(&mut sim, &d, &[], &mut sinks)
    });

    // ---- the audit zooms ----
    let mut zooms: Vec<(String, &Canvas, String)> = Vec::new();
    for (frame, px) in &rb.kept {
        let what = if *frame == 0 {
            "B frame 0 (Ctrl-A spawn)".to_string()
        } else if *frame == ctrl_e_frame {
            "B Ctrl-E spawn".to_string()
        } else if *frame == pin_frame {
            "B Ctrl-A T+150 (pin)".to_string()
        } else {
            "B Ctrl-E T+150 (pin)".to_string()
        };
        let name = match *frame {
            0 => "zoom_B_spawn.png".to_string(),
            f if f == ctrl_e_frame => "zoom_B_spawn_ctrl_e.png".to_string(),
            f if f == pin_frame => "zoom_B_pin_t150.png".to_string(),
            _ => "zoom_B_pin_t150_ctrl_e.png".to_string(),
        };
        zooms.push((name, px, format!("{what} = B/frame_{frame:04}")));
    }
    if let Some(px) = &rb.peak.px {
        zooms.push((
            "zoom_B_peak.png".to_string(),
            px,
            format!(
                "B peak luminance = B/frame_{:04} (sum {})",
                rb.peak.frame, rb.peak.key
            ),
        ));
    }
    if let Some(px) = &ra.densest.px {
        zooms.push((
            "zoom_A_densest.png".to_string(),
            px,
            format!(
                "A densest stardust = A/frame_{:04} ({} live stars)",
                ra.densest.frame, ra.densest.key
            ),
        ));
    }
    for (frame, px) in &rc.kept {
        zooms.push((
            "zoom_C_frame2.png".to_string(),
            px,
            format!("C frame 2 = C/frame_{frame:04}"),
        ));
    }
    let mut zoom_lines = Vec::new();
    for (name, px, what) in &zooms {
        let path = format!("{dir}/{name}");
        px.zoom(ZOOM).write_png(&path);
        zoom_lines.push(format!("{name}: {what}"));
    }
    // The lone-Ctrl-A audit: the seven instants, 1:1 and at 4×, named by
    // their age so a before/after pair can be read side by side.
    for (&ms, &frame) in CTRL_A_AUDIT_MS.iter().zip(&audit_frames) {
        if let Some((_, px)) = rb.kept.iter().find(|(f, _)| *f == frame) {
            let name = format!("ctrl_a_T+{ms:03}ms");
            px.write_png(&format!("{dir}/{name}.png"));
            px.zoom(ZOOM).write_png(&format!("{dir}/{name}_zoom.png"));
            zoom_lines.push(format!(
                "{name}.png: B/frame_{frame:04} ({:.1} ms after the Ctrl-A)",
                Sim::ms(frame as u64)
            ));
        }
    }

    // ---- sinks ----
    if sinks.index.ends_with(",\n") {
        sinks.index.truncate(sinks.index.len() - 2);
        sinks.index.push('\n');
    }
    sinks.index.push_str("]\n");
    std::fs::write(format!("{dir}/index.json"), &sinks.index).expect("write index.json");
    std::fs::write(format!("{dir}/stats.csv"), &sinks.stats).expect("write stats.csv");

    // ---- summary (the last lines of the run) ----
    let over_ink = |r: &RunOut| {
        let m = |f: fn(&TickStats) -> usize| r.ticks.iter().map(|(_, s)| f(s)).max().unwrap_or(0);
        format!(
            "over-ink max bodies {} / thin {} / halos {}",
            m(|s| s.point_over_ink),
            m(|s| s.thin_over_ink),
            m(|s| s.halos_over_ink)
        )
    };
    let offenders = |r: &RunOut| {
        r.ticks
            .iter()
            .filter(|(_, s)| !s.offenders.is_empty())
            .take(3)
            .map(|(t, s)| format!("tick {t}: {}", s.offenders))
            .collect::<Vec<_>>()
            .join(" || ")
    };
    let transitions = |r: &RunOut| {
        let mut prev = None;
        let mut v = Vec::new();
        for (t, s) in &r.ticks {
            if prev != Some(s.meteors) {
                v.push(format!("{}@{t}", s.meteors));
                prev = Some(s.meteors);
            }
        }
        v.join(" ")
    };
    let a_first_lit = ra.ticks.iter().find(|(_, s)| s.fp != 0).map(|(t, _)| *t);
    let a_last_lit = ra
        .ticks
        .iter()
        .rev()
        .find(|(_, s)| s.fp != 0)
        .map(|(t, _)| *t);
    let a_max_stars = ra.ticks.iter().map(|(_, s)| s.stars).max().unwrap_or(0);
    let c_max_stars = rc.ticks.iter().map(|(_, s)| s.stars).max().unwrap_or(0);
    let c_last_lit = rc
        .ticks
        .iter()
        .rev()
        .find(|(_, s)| s.fp != 0)
        .map(|(t, _)| *t);
    let b_max_halos = rb.ticks.iter().map(|(_, s)| s.halos).max().unwrap_or(0);
    let b_max_under = rb.ticks.iter().map(|(_, s)| s.under).max().unwrap_or(0);
    let frames = |r: &RunOut| {
        r.frames
            .iter()
            .filter(|f| !f.event.is_empty())
            .map(|f| format!("f{:04}={}", f.frame, f.event))
            .collect::<Vec<_>>()
            .join(" ")
    };
    println!("== summary ==");
    println!(
        "A: {} frames (every 4th tick) ticks {}..={}; caret ends at col {}; first lit tick {:?}, last lit tick {:?} (last key tick {}); max live stars {a_max_stars}; {}",
        ra.frames.len(),
        ra.ticks.first().map_or(0, |(t, _)| *t),
        ra.ticks.last().map_or(0, |(t, _)| *t),
        TEXT.len(),
        a_first_lit,
        a_last_lit,
        a.sched.last().map_or(0, |(t, _)| *t),
        over_ink(&ra),
    );
    println!(
        "A: densest stardust frame = f{:04} ({} live stars); offenders: {}",
        ra.densest.frame,
        ra.densest.key,
        offenders(&ra)
    );
    println!(
        "B: {} frames (every tick) ticks {}..={}; spawns at {}; pin frames f{pin_frame:04} (Ctrl-A) / f{pin_frame_e:04} (Ctrl-E); peak lum f{:04}; max under {b_max_under}, max halos {b_max_halos}; {}; live meteors {}",
        rb.frames.len(),
        rb.ticks.first().map_or(0, |(t, _)| *t),
        rb.ticks.last().map_or(0, |(t, _)| *t),
        frames(&rb),
        rb.peak.frame,
        over_ink(&rb),
        transitions(&rb),
    );
    for l in meteor_report(&rb.ticks) {
        println!("B: {l}");
    }
    println!(
        "C: {} frames (every tick) ticks {}..={}; backspaces at {}; max live stars {c_max_stars}; {}; last lit tick {:?}; offenders: {}",
        rc.frames.len(),
        rc.ticks.first().map_or(0, |(t, _)| *t),
        rc.ticks.last().map_or(0, |(t, _)| *t),
        frames(&rc),
        over_ink(&rc),
        c_last_lit,
        offenders(&rc),
    );
    if let Some(f) = rc.frames.get(2) {
        let st = &rc
            .ticks
            .iter()
            .find(|(t, _)| *t == f.tick)
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        println!(
            "C: frame 2 (tick {}): under={} out={} halos={} stars={} lum_max={}",
            f.tick, st.under, st.out, st.halos, st.stars, f.lum.1
        );
    }
    if let Some(rd) = &rd {
        // The law the sheet is read for: on every tick from the first band
        // move to the last the ribbon is lit (the fingerprint is non-zero) —
        // the band slides with the caret and never vanishes.
        let bands: Vec<u64> = d
            .sched
            .iter()
            .filter(|(_, a)| matches!(a, Action::BandMove(_)))
            .map(|&(t, _)| t)
            .collect();
        let (first_band, last_band) = (
            bands.iter().copied().min().unwrap_or(0),
            bands.iter().copied().max().unwrap_or(0),
        );
        let dark_under_stream = rd
            .ticks
            .iter()
            .filter(|(t, s)| *t >= first_band && *t <= last_band && s.fp == 0)
            .count();
        println!(
            "D: {} frames (every tick) ticks {}..={}; band moves {} (ticks {first_band}..={last_band}); composer ends on row {}; dark ticks while streaming {dark_under_stream}; last lit tick {:?}; {}",
            rd.frames.len(),
            rd.ticks.first().map_or(0, |(t, _)| *t),
            rd.ticks.last().map_or(0, |(t, _)| *t),
            bands.len(),
            sim.text_row,
            rd.ticks
                .iter()
                .rev()
                .find(|(_, s)| s.fp != 0)
                .map(|(t, _)| *t),
            over_ink(rd),
        );
    }
    for l in &zoom_lines {
        println!("zoom {l}");
    }
    let rd_frames = rd.as_ref().map_or(0, |r| r.frames.len());
    let rd_ticks = rd.as_ref().map_or(0, |r| r.ticks.len());
    println!(
        "index.json: {} frames; stats.csv: {} ticks",
        ra.frames.len() + rb.frames.len() + rc.frames.len() + rd_frames,
        ra.ticks.len() + rb.ticks.len() + rc.ticks.len() + rd_ticks
    );
}

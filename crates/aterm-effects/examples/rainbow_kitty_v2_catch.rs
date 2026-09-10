// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE CAT CATCHES A STAR** — headless capture of `RAINBOW-KITTY-V2.md`
//! panel #10(b), rendered as an A/B so the catch is readable as a difference
//! rather than as a claim.
//!
//! One hand types a line; the resident pet ([`aterm_effects::kitty_pet`])
//! chases the caret, earns its contentment and settles into a purring sit; the
//! sky deals a GOLD m1 within
//! [`aterm_effects::rainbow_kitty::companion::CATCH_REACH_CELLS`] of it. Two
//! identical engines are then run side by side from that instant:
//!
//! * **live** — nobody reaches; the star lives its own 460 ms m1 sky life.
//! * **caught** — the paw lands 200 ms after the offer and `Engine::catch_star`
//!   spends the rest of that life on the sky's 40 ms finish.
//!
//! Everything on the glass is the REAL producer: the quads and halos are
//! `Engine::tick`'s, composited through `aterm_render`'s own blend helpers;
//! the cat is the authored art baked through [`PetBakeKey::bake`] at the pose
//! the pet's own brain resolved; the offer is
//! `Engine::pet_offer(geom, PetOnGlass::of(&pet_frame, geom))` — the published
//! call, with the pet's own settled/purring verdict as its gate. The paw is
//! the ONE stand-in: the pet has no reach-out pose, so the arm is drawn as a
//! short arc from the shoulder to the star, and it is drawn on the caught arm
//! only.
//!
//!   targo --unverified run -p aterm-effects --example rainbow_kitty_v2_catch \
//!       --release -- <out_dir>

use std::time::Duration;

use aterm_time::Instant;

use aterm_effects::cat_baker::CatColorKey;
use aterm_effects::cursor_glow::{Geom, SoundCue};
use aterm_effects::kitty_pet::{PetBrain, PetFrame, PetSense};
use aterm_effects::pet_baker::{PetBakeKey, PetBaker};
use aterm_effects::rainbow_kitty::companion::{PetOnGlass, StarCatch};
use aterm_effects::rainbow_kitty::{
    CaretSeam, Config, Dir, Engine, Event, Frame, Licence, TypedClass,
};
use aterm_render::{
    BeamVertex, GlowQuad, HaloMode, RainHalo, add_sat, halo_over_cap, halo_row_ny, halo_weight,
    over_premul, over_rgb, premul_rgb,
};

/// Cell width in device px — the owner's retina cell (§21.1).
const CW: usize = 15;
/// Cell height in device px.
const CH: usize = 28;
const COLS: usize = 64;
const ROWS: usize = 8;
/// The row the prose is typed on.
const TEXT_ROW: u16 = 5;
/// The dark ground, `(17, 19, 24)`.
const GROUND: u32 = 0x0011_1318;
/// Glyph ink.
const INK: u32 = 0x00E8_E8F0;
/// The idle caret block.
const CARET_IDLE: u32 = 0x0058_5A66;
/// One 120 Hz tick.
const TICK_US: u64 = 8_333;
/// The typing cadence.
const KEY_MS: u64 = 70;
/// What the hand types.
const TEXT: &str = "echo the quick brown fox jumps over the lazy dog and sits";
/// How long the hand rests before the catch, ms — long enough for the pet to
/// stop, sit, and spend its earned contentment as a purr.
const REST_MS: u64 = 4_000;
/// How long after the offer the paw lands, ms.
const PAW_MS: u64 = 200;
/// How long past the offer the capture runs, ms — an m1's whole sky life plus
/// a breath, so the LIVE arm is seen dying of old age.
const RUN_MS: u64 = 620;
/// Nearest-neighbour zoom of the crops.
const ZOOM: usize = 5;

// ===========================================================================
// Pixels — the sibling capture's rasterizer, which is the CPU reference's law
// ===========================================================================

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

    /// One [`GlowQuad`]: additive at `alpha == 0`, premultiplied source-over
    /// otherwise — the one blend both modes are.
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

    /// One [`RainHalo`] under the CPU reference's radial law.
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

    /// One premultiplied-by-`a` source-over pixel — the paw's own blend.
    fn dot(&mut self, x: i32, y: i32, color: u32, a: u8) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let p = &mut self.px[y as usize * self.w + x as usize];
        *p = over_rgb(*p, color, a);
    }

    /// An RGBA tile (the baked cat), source-over.
    fn tile(&mut self, rgba: &[u8], tw: usize, th: usize, x0: i32, y0: i32, gain: u8) {
        for ty in 0..th {
            for tx in 0..tw {
                let s = (ty * tw + tx) * 4;
                let a = u32::from(rgba[s + 3]) * u32::from(gain) / 255;
                if a == 0 {
                    continue;
                }
                let rgb = (u32::from(rgba[s]) << 16)
                    | (u32::from(rgba[s + 1]) << 8)
                    | u32::from(rgba[s + 2]);
                self.dot(x0 + tx as i32, y0 + ty as i32, rgb, a as u8);
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

    fn glyph(&mut self, col: usize, row: usize, c: char) {
        self.bitmap(c, (col * CW + 2, row * CH + 4), (CW - 4, CH - 8), INK);
    }

    fn label(&mut self, x: usize, y: usize, text: &str, color: u32) {
        for (i, c) in text.chars().enumerate() {
            self.bitmap(c, (x + i * 18, y), (16, 16), color);
        }
    }

    fn blit(&mut self, src: &Canvas, x: usize, y: usize) {
        for sy in 0..src.h.min(self.h.saturating_sub(y)) {
            let dst = (y + sy) * self.w + x;
            let n = src.w.min(self.w.saturating_sub(x));
            self.px[dst..dst + n].copy_from_slice(&src.px[sy * src.w..sy * src.w + n]);
        }
    }

    fn crop(&self, x0: usize, y0: usize, w: usize, h: usize) -> Canvas {
        let mut c = Canvas::new(w, h);
        for y in 0..h.min(self.h.saturating_sub(y0)) {
            let src = (y0 + y) * self.w + x0;
            let n = w.min(self.w.saturating_sub(x0));
            c.px[y * w..y * w + n].copy_from_slice(&self.px[src..src + n]);
        }
        c
    }

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

// ===========================================================================
// The driven world: one v2 engine, one pet brain, one clock
// ===========================================================================

#[derive(Default)]
struct Scratch {
    under: Vec<GlowQuad>,
    out: Vec<GlowQuad>,
    halos: Vec<RainHalo>,
    beams: Vec<BeamVertex>,
    cues: Vec<SoundCue>,
}

struct Sim {
    eng: Engine,
    pet: PetBrain,
    frame: PetFrame,
    /// The pet frame the sky reads: the one PUBLISHED before this tick's
    /// engine ran. §7.2(b) is that the pet seats itself and v2 never drives
    /// it, so the offer is always made against the cat as it was last seen —
    /// and that one frame of staleness is exactly what lets the FIRST key
    /// after a rest be offered to a cat that is still purring, before its own
    /// brain has seen the caret move.
    prev: PetFrame,
    cfg: Config,
    geom: Geom,
    base: Instant,
    tick: u64,
    typed: Vec<char>,
    caret: (u16, u16),
    sc: Scratch,
    seam: CaretSeam,
    probe: Vec<bool>,
}

impl Sim {
    fn new() -> Self {
        let mut eng = Engine::new();
        eng.set_engaged(true);
        let mut pet = PetBrain::default();
        let base = Instant::now();
        let geom = Geom {
            cw: CW,
            ch: CH,
            rows: ROWS,
            cols: COLS,
            origin_x: 0,
            origin_y: 0,
            win_w: (COLS * CW) as u16,
            win_h: (ROWS * CH) as u16,
            head: 0,
        };
        let frame = pet.tick(PetSense {
            now: base,
            caret: Some((TEXT_ROW, 0)),
            wrapped: false,
            rows: ROWS as u16,
            cols: COLS as u16,
            cell_w: CW as u16,
            cell_h: CH as u16,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        });
        Self {
            eng,
            pet,
            frame,
            prev: frame,
            cfg: Config {
                dark_theme: true,
                intensity: 1.0,
                duration: Duration::from_millis(900),
                ribbon_tall: true,
                theme_fg: INK,
                theme_bg: GROUND,
                reduced_motion: false,
            },
            geom,
            base,
            tick: 0,
            typed: Vec::new(),
            caret: (TEXT_ROW, 0),
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

    fn tick_of(ms: u64) -> u64 {
        (ms * 1000).div_ceil(TICK_US)
    }

    fn key(&mut self, c: char) {
        let from = self.caret;
        let to = (TEXT_ROW, self.caret.1 + 1);
        self.eng.on_event(
            Event::Move {
                from,
                to,
                licence: Licence::Typed,
                dir: Dir::of(i32::from(to.1) - i32::from(from.1), 0),
            },
            self.now(),
        );
        self.caret = to;
        self.eng.on_event(
            Event::Typed {
                cells: 1,
                shifted: false,
                class: if c == ' ' {
                    TypedClass::Space
                } else {
                    TypedClass::Glyph
                },
            },
            self.now(),
        );
        self.typed.push(c);
    }

    /// A CAPITAL: the typed key, plus §5.8's earned m1 at its own cell —
    /// the host's own `Engine::earn_hero` seam, on the key's edge.
    fn capital(&mut self, c: char) {
        self.key(c);
        let cell = self.caret;
        self.eng.earn_hero(self.now(), cell.0, cell.1);
    }

    fn probe_rows(&mut self) {
        for (col, slot) in self.probe.iter_mut().enumerate() {
            *slot = self.typed.get(col).is_some_and(|&c| c != ' ');
        }
        let blank = vec![false; COLS];
        for row in [TEXT_ROW - 1, TEXT_ROW + 1] {
            self.eng.probe_mut().probe_row(i32::from(row), &blank);
        }
        self.eng
            .probe_mut()
            .probe_row(i32::from(TEXT_ROW), &self.probe);
    }

    /// One frame: the pet's brain, then seam point 3, then the clock.
    fn step(&mut self) {
        self.prev = self.frame;
        self.frame = self.pet.tick(PetSense {
            now: self.now(),
            caret: Some(self.caret),
            wrapped: false,
            rows: ROWS as u16,
            cols: COLS as u16,
            cell_w: CW as u16,
            cell_h: CH as u16,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        });
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
        self.tick += 1;
    }

    /// The cat as the sky sees it — the PUBLISHED conversion, off the pet's
    /// own last frame.
    fn cat(&self) -> Option<PetOnGlass> {
        PetOnGlass::of(&self.prev, self.geom)
    }

    fn caret_fill(&self) -> u32 {
        let field = aterm_effects::spectrum::spectrum(self.seam.field_t);
        let lit = mix(CARET_IDLE, field, self.seam.paint.clamp(0.0, 1.0));
        if self.seam.flare_at.is_some() {
            0x00FF_FFFF
        } else {
            lit
        }
    }

    /// The world, in §6.5's order, plus the cat and (on the caught arm) the paw.
    fn draw(&self, canvas: &mut Canvas, baker: &mut PetBaker, paw: Option<(f32, f32)>) {
        canvas.clear();
        for q in &self.sc.under {
            canvas.quad(q);
        }
        for (col, &c) in self.typed.iter().enumerate() {
            canvas.glyph(col, usize::from(TEXT_ROW), c);
        }
        canvas.rect(
            usize::from(self.caret.1) * CW,
            usize::from(self.caret.0) * CH,
            CW,
            CH,
            self.caret_fill(),
        );
        self.draw_cat(canvas, baker);
        if let Some((sx, sy)) = paw {
            self.draw_paw(canvas, sx, sy);
        }
        for q in &self.sc.out {
            canvas.quad(q);
        }
        for mode in [HaloMode::Add, HaloMode::Over] {
            for a in self.sc.halos.iter().filter(|a| a.mode == mode) {
                canvas.halo(a);
            }
        }
    }

    /// The authored pet, baked at the pose its own brain resolved this frame,
    /// in the dest rect its own emitter would use ([`PetFrame::body_px`]).
    fn draw_cat(&self, canvas: &mut Canvas, baker: &mut PetBaker) {
        if self.frame.alpha == 0 {
            return;
        }
        let Some((x0, x1, y0, y1)) =
            self.frame
                .body_px(CW as u16, CH as u16, COLS as u16, ROWS as u16)
        else {
            return;
        };
        let (w, h) = ((x1 - x0).max(1) as u16, (y1 - y0).max(1) as u16);
        let key = PetBakeKey {
            pose: self.frame.pose,
            coat: 8,
            iris: 4,
            colors: CatColorKey {
                accent: 12,
                background: 0,
            },
            w,
            h,
        };
        let _ = baker;
        let tile = key.bake();
        canvas.tile(
            tile.pixels(),
            tile.width() as usize,
            tile.height() as usize,
            x0,
            y0,
            self.frame.alpha,
        );
    }

    /// THE ONE STAND-IN: the pet owns no reach-out pose, so the paw is drawn
    /// as a short tapering arc from the cat's shoulder to the star. It is
    /// annotation, not a proposal — no art is added by this round.
    fn draw_paw(&self, canvas: &mut Canvas, sx: f32, sy: f32) {
        let Some((x0, x1, y0, _)) =
            self.frame
                .body_px(CW as u16, CH as u16, COLS as u16, ROWS as u16)
        else {
            return;
        };
        let toward_left = sx < ((x0 + x1) / 2) as f32;
        let shoulder = (
            if toward_left { x0 + 2 } else { x1 - 2 } as f32,
            (y0 + (CH / 3) as i32) as f32,
        );
        for i in 0..=48 {
            let u = i as f32 / 48.0;
            let x = shoulder.0 + (sx - shoulder.0) * u;
            // A little lift in the middle, so the arm reads as a reach.
            let y = shoulder.1 + (sy - shoulder.1) * u - 4.0 * (u * (1.0 - u)) * 4.0;
            let a = (200.0 * (1.0 - 0.5 * u)) as u8;
            for d in -1..=1 {
                canvas.dot(
                    x as i32,
                    y as i32 + d,
                    0x00D8_D2C8,
                    a / (1 + d.unsigned_abs() as u8),
                );
            }
        }
    }
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let f = |sh: u32| {
        let (x, y) = (((a >> sh) & 0xFF) as f32, ((b >> sh) & 0xFF) as f32);
        ((x + (y - x) * t.clamp(0.0, 1.0)) as u32) << sh
    };
    f(16) | f(8) | f(0)
}

// ===========================================================================
// The script
// ===========================================================================

/// Type the line and rest until the pet has settled into a purring sit.
/// Returns the tick the rest ended on.
fn write_and_settle(sim: &mut Sim) -> u64 {
    let mut next = 0u64;
    for (i, c) in TEXT.chars().enumerate() {
        next = Sim::tick_of(i as u64 * KEY_MS);
        while sim.tick < next {
            sim.step();
        }
        sim.key(c);
        sim.step();
    }
    rest_until_contented(sim, next + Sim::tick_of(REST_MS));
    sim.tick
}

/// Tick on until the cat says it is settled AND purring (its own two
/// verdicts), or until `floor` at the latest.
fn rest_until_contented(sim: &mut Sim, floor: u64) {
    while sim.tick < floor || !sim.cat().is_some_and(|c| c.contented()) {
        sim.step();
        if sim.tick > floor + Sim::tick_of(6_000) {
            return;
        }
    }
}

/// **THE MOMENT.** A contented cat is sitting beside the caret; the hand
/// types ONE CAPITAL. §5.8 says a capital earns an m1 outright, §5.3 deals it
/// gold 15 % of the time, and the offer is made against the cat as it was
/// last published — which is still the purring sit, because the pet's own
/// brain has not yet seen the caret move. Repeat the beat (rest, one capital)
/// until the earned m1 comes up gold.
///
/// Returns the tick the capital was struck on, and how many it took.
fn strike_until_gold(sim: &mut Sim, tries: usize) -> Option<(u64, usize)> {
    for n in 0..tries {
        rest_until_contented(sim, sim.tick + Sim::tick_of(1_200));
        if sim.caret.1 + 3 >= COLS as u16 {
            return None;
        }
        sim.capital('A');
        sim.step();
        if let Some(cat) = sim.cat()
            && sim.eng.pet_offer(sim.geom, Some(cat)).catch.is_some()
        {
            return Some((sim.tick, n + 1));
        }
    }
    None
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    std::fs::create_dir_all(&out).expect("out dir");

    // ---- find the moment: a settled, contented cat handed a gold m1.
    let mut probe = Sim::new();
    write_and_settle(&mut probe);
    let Some((offer_tick, tries)) = strike_until_gold(&mut probe, 60) else {
        eprintln!(
            "no gold m1 reached the cat (cat: {:?}) — nothing to render",
            probe.cat()
        );
        std::process::exit(2);
    };
    println!("the gold m1 came up on capital {tries}, at tick {offer_tick}");

    // ---- replay both arms to that tick on their own engines.
    let mut arms: Vec<(&str, Sim, Option<StarCatch>)> = Vec::new();
    for name in ["live", "caught"] {
        let mut sim = Sim::new();
        write_and_settle(&mut sim);
        let star = strike_until_gold(&mut sim, 60)
            .and_then(|_| sim.cat())
            .and_then(|cat| sim.eng.pet_offer(sim.geom, Some(cat)).catch);
        assert_eq!(sim.tick, offer_tick, "the replay must be deterministic");
        arms.push((name, sim, star));
    }

    let star = arms[0].2.expect("the replay must reach the same offer");
    let cat = arms[0].1.cat().expect("the cat is on glass at the offer");
    println!(
        "offer: star at ({:.0}, {:.0}) col {} born {:?} | cat cols {:.1}..{:.1} row {} \
         settled {} purr {:.3} | reach {:.1} cells",
        star.x,
        star.y,
        star.col,
        star.born,
        cat.span().0,
        cat.span().1,
        cat.row,
        cat.settled,
        cat.purr,
        cat.columns_to(star.col),
    );

    // ---- what the offer COSTS, measured on the live pool at the offer and
    //      again on a HOT sky (the hand still typing, the pool near its cap).
    {
        let mut hot = Sim::new();
        for (i, c) in TEXT.chars().enumerate() {
            while hot.tick < Sim::tick_of(i as u64 * KEY_MS) {
                hot.step();
            }
            hot.key(c);
            hot.step();
        }
        let hot_cat = hot.cat().unwrap_or(cat);
        let reps = 200_000u32;
        let t = std::time::Instant::now();
        let mut sink = 0u32;
        for _ in 0..reps {
            let o = hot.eng.pet_offer(hot.geom, Some(hot_cat));
            sink = sink.wrapping_add(u32::from(o.catch.is_some()) + o.mote_rgb.unwrap_or(0));
        }
        let ns = t.elapsed().as_nanos() as f64 / f64::from(reps);
        println!(
            "pet_offer: {ns:.3} ns/call on a HOT {}-star sky (sink {sink})",
            hot.eng.status().stars
        );
    }
    {
        let sim = &arms[0].1;
        let reps = 200_000u32;
        let t = std::time::Instant::now();
        let mut sink = 0u32;
        for _ in 0..reps {
            let o = sim.eng.pet_offer(sim.geom, Some(cat));
            sink = sink.wrapping_add(u32::from(o.catch.is_some()) + o.mote_rgb.unwrap_or(0));
        }
        let ns = t.elapsed().as_nanos() as f64 / f64::from(reps);
        println!(
            "pet_offer: {ns:.3} ns/call over {reps} calls on a {}-star pool (sink {sink})",
            sim.eng.status().stars
        );
    }

    // ---- run both arms forward, capturing every tick.
    let paw_tick = offer_tick + Sim::tick_of(PAW_MS);
    let end_tick = offer_tick + Sim::tick_of(RUN_MS);
    let mut baker = PetBaker::default();
    let mut canvas = Canvas::new(COLS * CW, ROWS * CH);
    let mut csv = String::from("arm,tick,t_ms,stars,out,halos,lum_sum,lum_max,caught\n");
    let mut strips: Vec<(String, Vec<Canvas>)> = Vec::new();
    // The crop the A/B strip reads: the cat, the star, and a cell of margin.
    let crop_x = ((star.x as usize).min(cat.span().0 as usize * CW)).saturating_sub(CW);
    let crop_w = (9 * CW).min(COLS * CW - crop_x);
    let crop_y = (usize::from(TEXT_ROW) - 2) * CH + CH / 2;
    let crop_h = 3 * CH;

    for (name, sim, offered) in &mut arms {
        let dir = format!("{out}/{name}");
        std::fs::create_dir_all(&dir).expect("arm dir");
        let mut crops = Vec::new();
        let mut caught = false;
        let mut n = 0usize;
        while sim.tick <= end_tick {
            if *name == "caught" && !caught && sim.tick >= paw_tick {
                let at = sim.now();
                caught = sim.eng.catch_star(offered.expect("the offer"), at);
            }
            let paw = (*name == "caught"
                && sim.tick >= paw_tick.saturating_sub(Sim::tick_of(60))
                && sim.tick <= paw_tick + Sim::tick_of(80))
            .then_some((star.x, star.y));
            sim.draw(&mut canvas, &mut baker, paw);
            let ms = (sim.tick.saturating_sub(offer_tick) * TICK_US) as f64 / 1000.0;
            let (sum, max) = {
                let floor = max_channel(GROUND);
                let mut s = 0u64;
                let mut m = 0u8;
                for &p in &canvas.px {
                    let c = max_channel(p);
                    s += u64::from(c.saturating_sub(floor));
                    m = m.max(c);
                }
                (s, m)
            };
            csv.push_str(&format!(
                "{name},{},{ms:.1},{},{},{},{sum},{max},{caught}\n",
                sim.tick,
                sim.eng.status().stars,
                sim.sc.out.len(),
                sim.sc.halos.len(),
            ));
            canvas.write_png(&format!("{dir}/{n:03}_t+{ms:.0}ms.png"));
            if n.is_multiple_of(12) {
                crops.push(canvas.crop(crop_x, crop_y, crop_w, crop_h).zoom(ZOOM));
            }
            n += 1;
            sim.step();
        }
        println!("{name}: {n} frames -> {dir}/");
        strips.push(((*name).to_string(), crops));
    }

    // ---- the A/B strip: live above, caught below, one column per 66 ms.
    let cols = strips[0].1.len().min(strips[1].1.len());
    let (tw, th) = (strips[0].1[0].w, strips[0].1[0].h);
    let gutter = 8;
    let label_h = 24;
    let mut sheet = Canvas::new(
        cols * (tw + gutter) + gutter,
        2 * (th + label_h + gutter) + gutter,
    );
    for (r, (name, crops)) in strips.iter().enumerate() {
        for (c, img) in crops.iter().take(cols).enumerate() {
            let x = gutter + c * (tw + gutter);
            let y = gutter + r * (th + label_h + gutter);
            sheet.blit(img, x, y);
            let ms = (c * 12) as f64 * TICK_US as f64 / 1000.0;
            sheet.label(x, y + th + 3, &format!("{name} t+{ms:.0}ms"), INK);
        }
    }
    sheet.write_png(&format!("{out}/catch_ab.png"));
    std::fs::write(format!("{out}/catch.csv"), csv).expect("csv");
    println!("A/B strip -> {out}/catch_ab.png, per-frame numbers -> {out}/catch.csv");
}

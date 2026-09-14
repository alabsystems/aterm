// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **RUN-SEAM INSTRUMENT** (2026-09-13, a temporary analysis example — not a
//! law, not a golden). Tests the hypothesis that the 1–3 px dark VERTICAL
//! SLITS the owner sees inside a lit line are RUN BOUNDARIES: a same-row
//! contiguous band that `Ribbon::build_runs` splits into two `ribbon_beam`
//! calls because the COHORT changes at a column, so `plan_run` feathers the
//! newer run's LEFT boundary at `RUN_TAIL_EASE` (0.10) and the first cell
//! prints as three slabs at 25 % / 55 % / 85 % of its neighbours.
//!
//! Two twins are driven with the SAME events in the SAME tick batches:
//!
//! * the real [`Engine`] (mod.rs's replay: every event of a tick is applied
//!   against the FINAL caret mirror, plus the echo-ledger bridge) — its
//!   `under` quads are composited to the PNGs that are scanned, and its run
//!   count is read off the quad stream (each run is emitted head-first, so
//!   an x that INCREASES between consecutive slabs is a run boundary);
//! * a bare [`Ribbon`] with a hand-built [`Ctx`] (same replay semantic, no
//!   bridge) — for the cohort census per column, the runs `build_runs`
//!   would make, and the planned `(x, cov)` at every vertex near a seam.
//!
//! Every scenario is one line typed at 70 ms/key with the echo 7 ms after
//! the key (120 Hz ticks, so the `Typed` lands on the tick BEFORE its echo on
//! some keys and on the echo's own tick on others — the real seam's phase
//! walk). Frames are captured at the second burst's start + 0 / 0.1 / 0.3 /
//! 1.0 / 1.5 s, and one mid-pause.
//!
//!   targo --unverified run -p aterm-effects --example rk_run_seam --release -- <out_dir>

use std::fmt::Write as _;
use std::time::Duration;

use aterm_time::Instant;

use aterm_effects::cursor_glow::{Geom, SoundCue};
use aterm_effects::rainbow_kitty::ribbon::{
    CHAIN_GAP_MAX, LIFT_GRACE_S, RETRACT_START_S, RUN_TAIL_EASE, Ribbon, SLABS_PER_CELL,
    SWOOSH_TOTAL_S,
};
use aterm_effects::rainbow_kitty::{
    CaretSeam, Config, Ctx, Dir, Engine, Event, Flow, Frame, Licence, TypedClass,
};
use aterm_render::{BeamVertex, GlowQuad, RainHalo, add_sat, over_premul};

/// The dark ground, `(17, 19, 24)` — the frames example's.
const GROUND: u32 = 0x0011_1318;
/// Glyph ink.
const INK: u32 = 0x00E8_E8F0;
/// One 120 Hz tick.
const TICK_US: u64 = 8_333;
/// Key cadence.
const KEY_MS: u64 = 70;
/// Echo latency after the key (the sibling census uses 6–9 ms).
const ECHO_MS: u64 = 7;
/// The typed row.
const TEXT_ROW: u16 = 3;
const ROWS: usize = 6;
const COLS: usize = 48;
/// The first typed glyph's column (a short prompt before it).
const COL0: u16 = 4;
/// Zoom of the audit crops.
const ZOOM: usize = 4;

// ===========================================================================
// Pixels
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

    fn rect(&mut self, x: usize, y: usize, rw: usize, rh: usize, color: u32) {
        for py in y..(y + rh).min(self.h) {
            for px in x..(x + rw).min(self.w) {
                self.px[py * self.w + px] = color;
            }
        }
    }

    fn glyph(&mut self, col: usize, row: usize, c: char, cw: usize, ch: usize) {
        let i = c as usize;
        let bm: [u8; 8] = if i < 128 {
            font8x8::legacy::BASIC_LEGACY[i]
        } else {
            [0; 8]
        };
        let (gx, gy) = (col * cw + 2, row * ch + 4);
        let (gw, gh) = (cw.saturating_sub(4).max(1), ch.saturating_sub(8).max(1));
        for py in 0..gh {
            let sy = py * 8 / gh;
            for px in 0..gw {
                let sx = px * 8 / gw;
                if bm[sy] & (1 << sx) != 0 {
                    let (x, y) = (gx + px, gy + py);
                    if x < self.w && y < self.h {
                        self.px[y * self.w + x] = INK;
                    }
                }
            }
        }
    }

    fn crop(&self, x0: usize, y0: usize, w: usize, h: usize) -> Canvas {
        let mut c = Canvas::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (x0 + x, y0 + y);
                if sx < self.w && sy < self.h {
                    c.px[y * w + x] = self.px[sy * self.w + sx];
                }
            }
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
// The rig: one Engine and one bare Ribbon, fed identically
// ===========================================================================

#[derive(Default)]
struct Scratch {
    under: Vec<GlowQuad>,
    out: Vec<GlowQuad>,
    halos: Vec<RainHalo>,
    beams: Vec<BeamVertex>,
    cues: Vec<SoundCue>,
}

/// Something the host will post at an instant.
#[derive(Clone, Copy)]
enum Post {
    Typed(TypedClass),
    /// A typing-licensed echo: `Sweep{from..to}` then `Move`, as the seam
    /// sends them for a same-row forward echo.
    Echo {
        from: u16,
        to: u16,
    },
    /// An arrow: a nav-licensed one-cell move, no sweep.
    Nav {
        from: u16,
        to: u16,
    },
}

struct Rig {
    eng: Engine,
    rib: Ribbon,
    cfg: Config,
    geom: Geom,
    base: Instant,
    tick: u64,
    /// Tick period, µs (120 Hz by default; 60 Hz for the rollover shape).
    tick_us: u64,
    /// A host STALL: no tick lands inside `[a, b)` — the next tick after
    /// `a` is the first one at or after `b`, and it replays everything that
    /// arrived meanwhile in one batch (a blocked frame).
    stall: Option<(Instant, Instant)>,
    /// The caret as the engine's mirror sees it (updated when a Move is
    /// POSTED — `Engine::on_event` moves its mirror at arrival).
    mirror: (u16, u16),
    /// Events posted since the last tick, for the bare ribbon's replay.
    pending: Vec<(Event, Instant)>,
    /// What the host will post, `(at, what)`; drained in time order.
    sched: Vec<(Instant, Post)>,
    /// One past the last glyph the shell holds.
    shell_end: u16,
    typed: Vec<(u16, char)>,
    eng_sc: Scratch,
    rib_sc: Scratch,
    probe: Vec<bool>,
    log: Vec<String>,
}

impl Rig {
    fn new(cw: usize, ch: usize) -> Self {
        let mut eng = Engine::new();
        eng.set_engaged(true);
        let cfg = Config {
            dark_theme: true,
            intensity: 1.0,
            duration: Duration::from_millis(900),
            ribbon_tall: true,
            ribbon_flat: false,
            theme_fg: INK,
            theme_bg: GROUND,
            reduced_motion: false,
        };
        let geom = Geom {
            cw,
            ch,
            rows: ROWS,
            cols: COLS,
            origin_x: 0,
            origin_y: 0,
            win_w: (COLS * cw) as u16,
            win_h: (ROWS * ch) as u16,
            head: 0,
        };
        let base = Instant::now();
        let mut rig = Self {
            eng,
            rib: Ribbon::new(),
            cfg,
            geom,
            base,
            tick: 0,
            tick_us: TICK_US,
            stall: None,
            mirror: (TEXT_ROW, COL0 - 1),
            pending: Vec::new(),
            sched: Vec::new(),
            shell_end: COL0,
            typed: Vec::new(),
            eng_sc: Scratch::default(),
            rib_sc: Scratch::default(),
            probe: vec![false; COLS],
            log: Vec::new(),
        };
        // Seat the mirror at the prompt's end with one licensed, swept echo
        // of a warm-up key that is then left to go out completely.
        let k0 = base + Duration::from_millis(100);
        rig.sched.push((k0, Post::Typed(TypedClass::Glyph)));
        rig.sched.push((
            k0 + Duration::from_millis(ECHO_MS),
            Post::Echo {
                from: COL0 - 1,
                to: COL0,
            },
        ));
        rig.run_to(base + Duration::from_secs(8), &mut Vec::new(), "", cw, ch);
        assert!(rig.rib.cells().is_empty(), "the warm-up mark is gone");
        rig.log.clear();
        rig
    }

    fn at(&self, tick: u64) -> Instant {
        self.base + Duration::from_micros(tick * self.tick_us)
    }

    fn now(&self) -> Instant {
        self.at(self.tick)
    }

    fn ms(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.base).as_secs_f64() * 1000.0
    }

    fn post(&mut self, ev: Event, at: Instant) {
        self.eng.on_event(ev, at);
        if let Event::Move { to, .. } = ev {
            self.mirror = to;
        }
        self.pending.push((ev, at));
    }

    fn post_echo(&mut self, from: u16, to: u16, at: Instant) {
        self.post(
            Event::Sweep {
                row: TEXT_ROW,
                col0: from,
                col1: to,
            },
            at,
        );
        self.post(
            Event::Move {
                from: (TEXT_ROW, from),
                to: (TEXT_ROW, to),
                licence: Licence::Typed,
                dir: Dir::of(i32::from(to) - i32::from(from), 0),
            },
            at,
        );
    }

    fn post_nav(&mut self, from: u16, to: u16, at: Instant) {
        self.post(
            Event::Move {
                from: (TEXT_ROW, from),
                to: (TEXT_ROW, to),
                licence: Licence::Nav,
                dir: Dir::of(i32::from(to) - i32::from(from), 0),
            },
            at,
        );
    }

    /// Schedule one key at `t`: the `Typed` at the key; the echo `ECHO_MS`
    /// later unless the caller coalesces echoes itself (`echo == false`).
    fn key(&mut self, t: Instant, ch: char, echo: bool) {
        self.key_echo(t, ch, echo.then_some(ECHO_MS));
    }

    /// A key whose echo lands `echo_ms` after it (`None`: the caller posts
    /// the echo itself).
    fn key_echo(&mut self, t: Instant, ch: char, echo_ms: Option<u64>) {
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        self.sched.push((t, Post::Typed(class)));
        let col = self.shell_end;
        self.typed.push((col, ch));
        self.shell_end += 1;
        if let Some(echo_ms) = echo_ms {
            self.sched.push((
                t + Duration::from_millis(echo_ms),
                Post::Echo {
                    from: col,
                    to: col + 1,
                },
            ));
        }
    }

    fn echo(&mut self, t: Instant, from: u16, to: u16) {
        self.sched.push((t, Post::Echo { from, to }));
    }

    fn nav(&mut self, t: Instant, from: u16, to: u16) {
        self.sched.push((t, Post::Nav { from, to }));
        self.shell_end = to;
    }

    fn probe_rows(&mut self) {
        self.probe.fill(false);
        for &(col, c) in &self.typed {
            if c != ' ' && usize::from(col) < COLS {
                self.probe[usize::from(col)] = true;
            }
        }
        let blank = vec![false; COLS];
        for r in [i32::from(TEXT_ROW) - 1, i32::from(TEXT_ROW) + 1] {
            self.eng.probe_mut().probe_row(r, &blank);
        }
        self.eng
            .probe_mut()
            .probe_row(i32::from(TEXT_ROW), &self.probe);
    }

    /// One tick: post what is due, tick the engine, replay the same batch
    /// into the bare ribbon against the same mirror.
    fn step(&mut self) {
        let now = self.now();
        self.sched.sort_by_key(|&(t, _)| t);
        while let Some(&(t, what)) = self.sched.first() {
            if t > now {
                break;
            }
            self.sched.remove(0);
            match what {
                Post::Typed(class) => {
                    self.post(
                        Event::Typed {
                            cells: 1,
                            shifted: false,
                            class,
                        },
                        t,
                    );
                    self.log.push(format!(
                        "    +{:8.1} ms  POST Typed (mirror {:?})",
                        self.ms(t),
                        self.mirror
                    ));
                }
                Post::Echo { from, to } => {
                    self.post_echo(from, to, t);
                    self.log.push(format!(
                        "    +{:8.1} ms  POST Sweep[{from},{to}) + Move {from}->{to} (typed)",
                        self.ms(t)
                    ));
                }
                Post::Nav { from, to } => {
                    self.post_nav(from, to, t);
                    self.log.push(format!(
                        "    +{:8.1} ms  POST Move {from}->{to} (nav)",
                        self.ms(t)
                    ));
                }
            }
        }
        self.probe_rows();
        // The engine.
        self.eng_sc.under.clear();
        self.eng_sc.out.clear();
        self.eng_sc.halos.clear();
        self.eng_sc.cues.clear();
        let mut fr = Frame {
            under: &mut self.eng_sc.under,
            out: &mut self.eng_sc.out,
            halos: &mut self.eng_sc.halos,
            beams: &mut self.eng_sc.beams,
            cues: &mut self.eng_sc.cues,
            caret: CaretSeam::default(),
            companion: None,
            fp: 0,
        };
        self.eng.tick(now, self.geom, &self.cfg, &mut fr);
        let _ = self.eng.take_companion_impulse();
        let disp = self.eng.status().disp;
        // The bare ribbon: the same batch, the same mirror, the engine's
        // spine as the birth price.
        let ctx = Ctx {
            now,
            geom: self.geom,
            cfg: &self.cfg,
            disp,
            birth_disp: disp,
            phase: 0.0,
            caret: self.mirror,
            caret_t: 0.0,
            mend: None,
            surge: 0.0,
            flow: Flow {
                heat: 0.0,
                combo: 0,
                best: 0,
            },
        };
        if !self.pending.is_empty() {
            let batch: Vec<String> = self
                .pending
                .iter()
                .map(|(ev, at)| format!("{}@{:.1}", short(ev), self.ms(*at)))
                .collect();
            self.log.push(format!(
                "  tick {:5} +{:8.1} ms  replay [{}] against mirror {:?}",
                self.tick,
                self.ms(now),
                batch.join(", "),
                self.mirror
            ));
        }
        for (ev, at) in std::mem::take(&mut self.pending) {
            self.rib.on_event(&ev, at, &ctx);
        }
        self.rib.plan(&ctx);
        self.rib_sc.under.clear();
        self.rib_sc.out.clear();
        self.rib_sc.halos.clear();
        self.rib_sc.cues.clear();
        let mut fr = Frame {
            under: &mut self.rib_sc.under,
            out: &mut self.rib_sc.out,
            halos: &mut self.rib_sc.halos,
            beams: &mut self.rib_sc.beams,
            cues: &mut self.rib_sc.cues,
            caret: CaretSeam::default(),
            companion: None,
            fp: 0,
        };
        self.rib.emit(&ctx, &mut fr);
        self.tick += 1;
    }

    /// Tick to `until`, capturing at every checkpoint on the first tick at
    /// or after it.
    fn run_to(
        &mut self,
        until: Instant,
        checkpoints: &mut Vec<(Instant, String)>,
        dir: &str,
        cw: usize,
        ch: usize,
    ) {
        while self.now() <= until {
            if let Some((a, b)) = self.stall
                && self.now() >= a
                && self.now() < b
            {
                // The blocked frame: skip to the first tick at or after `b`.
                while self.now() < b {
                    self.tick += 1;
                }
                self.log.push(format!(
                    "  STALL: no tick in [{:.1}, {:.1}) ms; resuming at tick {} (+{:.1} ms)",
                    self.ms(a),
                    self.ms(b),
                    self.tick,
                    self.ms(self.now())
                ));
                self.stall = None;
            }
            self.step();
            let now = self.at(self.tick - 1);
            checkpoints.sort_by_key(|&(t, _)| t);
            while let Some((t, _)) = checkpoints.first() {
                if *t > now {
                    break;
                }
                let (_, label) = checkpoints.remove(0);
                self.capture(&label, dir, cw, ch);
            }
        }
    }

    // -- census --------------------------------------------------------------

    /// The runs `build_runs` makes on the text row: newest owner per column,
    /// split on a column hole or a cohort change → `(col0, col1, cohort)`.
    fn cell_runs(&self) -> Vec<(u16, u16, u32)> {
        let mut owner: std::collections::BTreeMap<u16, u32> = Default::default();
        for c in self.rib.cells() {
            if c.row == TEXT_ROW {
                // Append-ordered pool: the last writer is the newest.
                owner.insert(c.col, c.cohort);
            }
        }
        let mut runs: Vec<(u16, u16, u32)> = Vec::new();
        for (&col, &coh) in &owner {
            match runs.last_mut() {
                Some(r) if r.1 + 1 == col && r.2 == coh => r.1 = col,
                _ => runs.push((col, col, coh)),
            }
        }
        runs
    }

    /// The planned runs: consecutive vertices ascend in x; a vertex at or
    /// left of its predecessor starts a new run → `(lo, hi)` index ranges.
    fn plan_runs(&self) -> Vec<(usize, usize)> {
        let segs = self.rib.plan_segments();
        let mut runs = Vec::new();
        let mut lo = 0;
        for i in 1..segs.len() {
            if segs[i].x <= segs[i - 1].x {
                runs.push((lo, i));
                lo = i;
            }
        }
        if !segs.is_empty() {
            runs.push((lo, segs.len()));
        }
        runs
    }

    /// Runs in the ENGINE's `under` stream on the text row band: each run
    /// is emitted head-first (x descending slab by slab), so an x that rises
    /// between consecutive slabs is a run boundary.
    fn engine_runs(&self, ch: usize) -> Vec<(u16, u16)> {
        let y_lo = (usize::from(TEXT_ROW) * ch) as u16;
        let y_hi = ((usize::from(TEXT_ROW) + 2) * ch) as u16;
        let mut runs: Vec<(u16, u16)> = Vec::new();
        let mut last_x: Option<u16> = None;
        for q in &self.eng_sc.under {
            if q.y < y_lo || q.y >= y_hi {
                continue;
            }
            match last_x {
                Some(lx) if q.x == lx => {}
                Some(lx) if q.x < lx => {
                    if let Some(r) = runs.last_mut() {
                        r.0 = r.0.min(q.x);
                    }
                }
                _ => runs.push((q.x, q.x + q.w)),
            }
            if let Some(r) = runs.last_mut() {
                r.1 = r.1.max(q.x + q.w);
            }
            last_x = Some(q.x);
        }
        runs
    }

    fn cohort_census(&self) -> String {
        let now = self.now();
        let mut s = String::new();
        for k in self.rib.cohorts() {
            if k.row != TEXT_ROW {
                continue;
            }
            let _ = write!(
                s,
                " #{}[{}..{}) {:?}{}{} idle={:.0}ms",
                k.id,
                k.col0,
                k.col1,
                k.phase,
                if k.abandoned { " ABANDONED" } else { "" },
                if k.wake { " wake" } else { "" },
                now.saturating_duration_since(k.alive_at).as_secs_f64() * 1000.0
            );
        }
        s
    }

    /// Composite the engine's `under` onto a fresh canvas (no glyphs), scan
    /// the text row band for per-column dips, write the PNGs, print the
    /// table.
    fn capture(&self, label: &str, dir: &str, cw: usize, ch: usize) {
        let (w, h) = (COLS * cw, ROWS * ch);
        let mut bed = Canvas::new(w, h);
        for q in &self.eng_sc.under {
            bed.quad(q);
        }
        // Band rows: the tall body reaches 1.10 ch above the row bottom and
        // ~0.3 ch below it.
        let y0 = (usize::from(TEXT_ROW) * ch).saturating_sub(3);
        let y1 = ((usize::from(TEXT_ROW) + 1) * ch + (0.31 * ch as f64).ceil() as usize).min(h);
        let ground = f32::from(max_channel(GROUND));
        let profile: Vec<f32> = (0..w)
            .map(|x| {
                (y0..y1)
                    .map(|y| f32::from(max_channel(bed.px[y * w + x])))
                    .sum::<f32>()
                    / (y1 - y0) as f32
            })
            .collect();
        let dips = dips(&profile, cw, ground);

        // The plan's runs and the cells' runs, zipped.
        let cell_runs = self.cell_runs();
        let plan_runs = self.plan_runs();
        let segs = self.rib.plan_segments();
        let eng_runs = self.engine_runs(ch);
        let now_ms = self.ms(self.now());

        println!(
            "\n  FRAME {label}  (t=+{now_ms:.1} ms, mirror {:?})  runs: plan={} cells={} (engine monotone-x groups={})   cohorts:{}",
            self.mirror,
            plan_runs.len(),
            cell_runs.len(),
            eng_runs.len(),
            self.cohort_census()
        );
        for (i, (lo, hi)) in plan_runs.iter().enumerate() {
            let cr = cell_runs.get(i);
            let (x0, x1) = (segs[*lo].x, segs[*hi - 1].x);
            cr.map_or_else(
                || println!("    run {i}: x {x0:.0}..{x1:.0}  (no matching cell run)"),
                |&(c0, c1, coh)| {
                    println!(
                        "    run {i}: cols {c0}..={c1} cohort #{coh}  x {x0:.0}..{x1:.0}  verts {}  cov first/last {}/{}  engine-x {:?}",
                        hi - lo,
                        segs[*lo].cov,
                        segs[*hi - 1].cov,
                        eng_runs.get(i)
                    )
                },
            );
        }
        // Vertices around every interior run boundary.
        for i in 1..plan_runs.len() {
            let (plo, phi) = plan_runs[i - 1];
            let (nlo, nhi) = plan_runs[i];
            let _ = phi;
            let tail: Vec<String> = segs[plo..nlo]
                .iter()
                .rev()
                .take(2)
                .map(|s| format!("({:.0},{})", s.x, s.cov))
                .collect();
            let head: Vec<String> = segs[nlo..nhi]
                .iter()
                .take(4)
                .map(|s| format!("({:.0},{})", s.x, s.cov))
                .collect();
            let (a, b) = (&segs[nlo - 1], &segs[nlo]);
            println!(
                "    seam {}|{}: older run's last verts (x,cov) {}   newer run's first verts {}   shared-x geometry older (spine {:.2} up {:.2} dn {:.2} t {:.3}) newer (spine {:.2} up {:.2} dn {:.2} t {:.3}) -> top-edge step {:.2} px, bottom-edge step {:.2} px",
                i - 1,
                i,
                tail.join(" "),
                head.join(" "),
                a.spine,
                a.up,
                a.dn,
                a.t,
                b.spine,
                b.up,
                b.dn,
                b.t,
                (a.spine - a.up) - (b.spine - b.up),
                (a.spine + a.dn) - (b.spine + b.dn)
            );
        }
        if dips.is_empty() {
            println!("    dips: none");
        }
        let cells_at = |col: i64| -> String {
            if col < 0 {
                return "-".into();
            }
            self.rib
                .cells()
                .iter()
                .rfind(|c| c.row == TEXT_ROW && i64::from(c.col) == col)
                .map_or("-".into(), |c| format!("#{}", c.cohort))
        };
        for d in &dips {
            let bcol = (d.x as f64 / cw as f64).round() as i64;
            let left = (d.x as i64 - 1).div_euclid(cw as i64);
            let right = ((d.x + d.w) as i64).div_euclid(cw as i64);
            let ratios: Vec<String> = d.ratios.iter().map(|r| format!("{r:.2}")).collect();
            // The engine's slabs on the spine row across the dip.
            let y_spine = ((usize::from(TEXT_ROW) + 1) * ch - 1) as u16;
            let mut slabs: Vec<(u16, u16, u8)> = self
                .eng_sc
                .under
                .iter()
                .filter(|q| {
                    q.y == y_spine
                        && usize::from(q.x) + usize::from(q.w) > d.x.saturating_sub(cw)
                        && usize::from(q.x) < d.x + d.w + cw
                })
                .map(|q| (q.x, q.w, q.alpha))
                .collect();
            slabs.sort_unstable();
            let slabs: Vec<String> = slabs
                .iter()
                .map(|(x, w, a)| format!("[{x},+{w})a{a}"))
                .collect();
            println!(
                "    DIP x={} width={} px  boundary col {bcol} (x/cw={:.2})  cells: col{} {} | col{} {}  min ratio {:.2} of ref {:.0}  per-px ratio {}  spine-row slabs {}",
                d.x,
                d.w,
                d.x as f64 / cw as f64,
                left,
                cells_at(left),
                right,
                cells_at(right),
                d.min_ratio,
                d.reference,
                ratios.join(" "),
                slabs.join(" ")
            );
        }
        // PNGs: bed only, bed + glyphs + caret, and a 4× crop of the band.
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir {dir}: {e}"));
        bed.write_png(&format!("{dir}/{label}_bed.png"));
        let mut full = Canvas::new(w, h);
        for q in &self.eng_sc.under {
            full.quad(q);
        }
        for &(col, c) in &self.typed {
            full.glyph(usize::from(col), usize::from(TEXT_ROW), c, cw, ch);
        }
        full.rect(
            usize::from(self.mirror.1) * cw,
            usize::from(self.mirror.0) * ch,
            cw,
            ch,
            0x0058_5A66,
        );
        for q in &self.eng_sc.out {
            full.quad(q);
        }
        full.write_png(&format!("{dir}/{label}.png"));
        let x0 = (usize::from(COL0) * cw).saturating_sub(cw);
        let crop_w = (self.typed.len() + 4) * cw;
        full.crop(x0, y0, crop_w.min(w - x0), y1 - y0)
            .zoom(ZOOM)
            .write_png(&format!("{dir}/{label}_zoom.png"));
    }
}

fn short(ev: &Event) -> String {
    match ev {
        Event::Typed { .. } => "Typed".into(),
        Event::Sweep { col0, col1, .. } => format!("Sweep[{col0},{col1})"),
        Event::Move {
            from, to, licence, ..
        } => format!("Move{}->{}{:?}", from.1, to.1, licence),
        other => format!("{other:?}"),
    }
}

struct Dip {
    x: usize,
    w: usize,
    min_ratio: f32,
    reference: f32,
    ratios: Vec<f32>,
}

/// Per-column dips: a column under 60 % of the lower of its two flanking
/// maxima (one cell each side), grouped; the flank must be lit.
fn dips(profile: &[f32], cw: usize, ground: f32) -> Vec<Dip> {
    let w = profile.len();
    let mut out = Vec::new();
    let mut x = 0;
    let lit = ground + 8.0;
    while x < w {
        let left = profile[x.saturating_sub(cw)..x]
            .iter()
            .cloned()
            .fold(0.0, f32::max);
        let right = profile[(x + 1).min(w)..(x + 1 + cw).min(w)]
            .iter()
            .cloned()
            .fold(0.0, f32::max);
        let reference = left.min(right);
        let is_dip = |v: f32| reference > lit && v < ground + 0.6 * (reference - ground);
        if is_dip(profile[x]) {
            let mut x2 = x;
            while x2 + 1 < w && is_dip(profile[x2 + 1]) {
                x2 += 1;
            }
            let lo = x.saturating_sub(2);
            let hi = (x2 + 4).min(w);
            let ratios: Vec<f32> = profile[lo..hi]
                .iter()
                .map(|v| (v - ground) / (reference - ground))
                .collect();
            let min = profile[x..=x2]
                .iter()
                .cloned()
                .fold(f32::INFINITY, f32::min);
            out.push(Dip {
                x,
                w: x2 - x + 1,
                min_ratio: (min - ground) / (reference - ground),
                reference,
                ratios,
            });
            x = x2 + 1;
        } else {
            x += 1;
        }
    }
    out
}

// ===========================================================================
// Scenarios
// ===========================================================================

/// What the second burst does after the first.
#[derive(Clone, Copy)]
enum Shape {
    /// A pause, then a clean per-key burst.
    Pause(u64),
    /// A pause, then a STALLED host frame: two keys 70 ms apart and their
    /// echoes all arrive while no tick lands; the frame that resumes sees
    /// the caret two ahead and sweeps once — `[Typed, Typed, Sweep[c,c+2),
    /// Move c->c+2]` replayed in one batch.
    CoalescedEcho(u64),
    /// The stall coalesce, then every later key replayed ON its echo's tick
    /// (echo 0 ms): the seam cell is never re-taken by the older cohort, so
    /// the seam LOCKS for the rest of the line and rides the swoosh.
    CoalescedLocked(u64),
    /// No stall: a 60 Hz host, two keys ROLLED OVER 4 ms apart with echoes
    /// 7 ms after each, so one 16.7 ms frame can hold both keys and both
    /// echoes (a fast typist's chord on a 60 Hz display).
    Rollover60(u64),
    /// A pause, then an arrow-right over a shell glyph, then the burst.
    ArrowHop(u64),
}

fn scenario(name: &str, shape: Shape, cw: usize, ch: usize, out: &str) {
    let dir = format!("{out}/{name}_{cw}x{ch}");
    println!(
        "\n=== SCENARIO {name} at cw {cw} / ch {ch}  ({shape_desc})",
        shape_desc = match shape {
            Shape::Pause(p) => format!(
                "\"asdf asdf\" · pause {p} ms · \"dsfa\", per-key echoes {ECHO_MS} ms after the key"
            ),
            Shape::CoalescedEcho(p) => format!(
                "\"asdf asdf\" · pause {p} ms · host STALL over \"ds\": both Typed and ONE coalesced Sweep[c,c+2)+Move in one tick · \"fa\""
            ),
            Shape::Rollover60(p) => format!(
                "\"asdf asdf\" · pause {p} ms · 60 Hz host, \"ds\" rolled over 4 ms apart, echoes 7 ms after each · \"fa\""
            ),
            Shape::CoalescedLocked(p) => format!(
                "\"asdf asdf\" · pause {p} ms · host STALL over \"ds\" (one coalesced echo) · \"fasd\" with each Typed replayed ON its echo's tick (echo 0 ms) — the seam locks"
            ),
            Shape::ArrowHop(p) =>
                format!("\"asdf asdf\" · pause {p} ms · Right-arrow over a shell glyph · \"dsfa\""),
        }
    );
    let mut rig = Rig::new(cw, ch);
    if matches!(shape, Shape::Rollover60(_)) {
        rig.tick_us = 16_667;
    }
    let ms = |n: u64| Duration::from_millis(n);
    let t0 = rig.now() + ms(200);
    let mut t = t0;
    for c in "asdf asdf".chars() {
        rig.key(t, c, true);
        t += ms(KEY_MS);
    }
    let burst1_end = t;
    let pause = match shape {
        Shape::Pause(p)
        | Shape::CoalescedEcho(p)
        | Shape::CoalescedLocked(p)
        | Shape::ArrowHop(p)
        | Shape::Rollover60(p) => p,
    };
    let mut checkpoints: Vec<(Instant, String)> = Vec::new();
    checkpoints.push((
        burst1_end + ms(pause / 2),
        format!("pause_mid+{}ms", pause / 2),
    ));
    if pause >= 1000 {
        checkpoints.push((burst1_end + ms(1000), "pause+1000ms".into()));
    }
    t = burst1_end + ms(pause);
    let burst2_start = t;
    match shape {
        Shape::Pause(_) => {
            for c in "dsfa".chars() {
                rig.key(t, c, true);
                t += ms(KEY_MS);
            }
        }
        Shape::CoalescedEcho(_) => {
            let c = rig.shell_end;
            // The host blocks from 2 ms before 'd' until 10 ms after 's'
            // echoed: both keys and one coalesced echo land in one batch.
            rig.stall = Some((t - ms(2), t + ms(KEY_MS) + ms(ECHO_MS) + ms(10)));
            rig.key(t, 'd', false);
            t += ms(KEY_MS);
            rig.key(t, 's', false);
            rig.echo(t + ms(ECHO_MS), c, c + 2);
            t += ms(KEY_MS);
            for ch2 in "fa".chars() {
                rig.key(t, ch2, true);
                t += ms(KEY_MS);
            }
        }
        Shape::CoalescedLocked(_) => {
            let c = rig.shell_end;
            rig.stall = Some((t - ms(2), t + ms(KEY_MS) + ms(ECHO_MS) + ms(10)));
            rig.key(t, 'd', false);
            t += ms(KEY_MS);
            rig.key(t, 's', false);
            rig.echo(t + ms(ECHO_MS), c, c + 2);
            t += ms(KEY_MS);
            for ch2 in "fasd".chars() {
                rig.key_echo(t, ch2, Some(0));
                t += ms(KEY_MS);
            }
        }
        Shape::Rollover60(_) => {
            let c = rig.shell_end;
            // 'd' and 's' 4 ms apart; the host coalesces what one frame saw.
            rig.key(t, 'd', false);
            rig.key(t + ms(4), 's', false);
            rig.echo(t + ms(4) + ms(ECHO_MS), c, c + 2);
            t += ms(KEY_MS);
            for ch2 in "fa".chars() {
                rig.key(t, ch2, true);
                t += ms(KEY_MS);
            }
        }
        Shape::ArrowHop(_) => {
            let c = rig.shell_end;
            // A shell glyph already at `c` (drawn, never typed here).
            rig.typed.push((c, 'x'));
            rig.nav(t, c, c + 1);
            t += ms(KEY_MS);
            for ch2 in "dsfa".chars() {
                rig.key(t, ch2, true);
                t += ms(KEY_MS);
            }
        }
    }
    let offs: &[u64] = if matches!(shape, Shape::CoalescedLocked(_)) {
        &[0, 100, 300, 500, 1000, 1200, 1300, 1400, 1500, 1700]
    } else {
        &[0, 100, 300, 1000, 1500]
    };
    for &off in offs {
        checkpoints.push((burst2_start + ms(off), format!("burst2+{off:04}ms")));
    }
    let end = burst2_start + ms(1800);
    rig.run_to(end, &mut checkpoints, &dir, cw, ch);
    println!("  event log (posts and tick batches):");
    for l in &rig.log {
        println!("{l}");
    }
}

fn analytic(cw: usize) {
    let slabs = SLABS_PER_CELL;
    let stride = cw.div_ceil(slabs);
    let xs: Vec<f32> = (0..=slabs)
        .map(|j| (cw as f32 * j as f32 / slabs as f32).round())
        .collect();
    let covs: Vec<f32> = (0..=slabs)
        .map(|j| RUN_TAIL_EASE + (1.0 - RUN_TAIL_EASE) * j as f32 / slabs as f32)
        .collect();
    let mut line = String::new();
    for k in 0..slabs {
        let (a, b) = (xs[k], xs[k + 1]);
        let mid = (covs[k] + covs[k + 1]) * 0.5;
        let _ = write!(
            line,
            " [{a:.0},{b:.0}) {}px @{:.0}%",
            (b - a) as i32,
            mid * 100.0
        );
    }
    println!(
        "  cw {cw}: stride {stride}, vertex xs {xs:?}, vertex cov share {covs:?} -> slab centres:{line}"
    );
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "rk_run_seam_out".into());
    println!(
        "RUN-SEAM instrument. RUN_TAIL_EASE={RUN_TAIL_EASE} SLABS_PER_CELL={SLABS_PER_CELL} LIFT_GRACE_S={LIFT_GRACE_S} RETRACT_START_S={RETRACT_START_S} SWOOSH_TOTAL_S={SWOOSH_TOTAL_S} CHAIN_GAP_MAX={CHAIN_GAP_MAX}"
    );
    println!(
        "ANALYTIC: the feathered first cell of a run whose left neighbour is another run (ease 0.10 -> 1.0 over one cell, sampled at slab centres):"
    );
    for cw in [15usize, 14, 7] {
        analytic(cw);
    }
    for (name, shape) in [
        ("pause0300", Shape::Pause(300)),
        ("pause1300", Shape::Pause(1300)),
        ("pause6000", Shape::Pause(6000)),
        ("coalesced_pause1300", Shape::CoalescedEcho(1300)),
        ("coalesced_pause0000", Shape::CoalescedEcho(0)),
        ("coalesced_locked_pause0000", Shape::CoalescedLocked(0)),
        ("rollover60_pause0000", Shape::Rollover60(0)),
        ("arrowhop_pause0300", Shape::ArrowHop(300)),
    ] {
        scenario(name, shape, 15, 28, &out);
    }
    for (name, shape) in [
        ("pause1300", Shape::Pause(1300)),
        ("coalesced_pause1300", Shape::CoalescedEcho(1300)),
        ("coalesced_locked_pause0000", Shape::CoalescedLocked(0)),
    ] {
        scenario(name, shape, 7, 14, &out);
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ESCORT STAYS OFF WHAT YOU JUST TYPED — measured on a real `Terminal`.
//!
//! Typing bursts with pauses — three runs of "hello world this is a test "
//! at a steady key cadence, one frame per 16 ms, the host's ink seam fed
//! every frame, the first burst waking a cat asleep at the prompt. The oracle
//! reads the DRAWN body (`PetFrame::body_px`) against an independent
//! `PetWorld`, so it never asks the brain what it decided: frames whose body
//! covers a glyph typed on the caret's row, how far the body trails the
//! caret on the ground, its largest step between two grounded frames, how
//! often it turns round, and any contact brace.
//!
//! Before the rework, at 62 / 100 / 150 ms a key: 30.9 / 19.8 / 9.8 % of
//! the frames covered typed glyphs, the body trailed by up to 4.7 cells,
//! turned round 1.00 / 0.72 / 0.56 times a second, and the first key braced
//! it for nine frames a run.

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetBrain, PetFrame, PetInputKind, PetSense, art_cols};
use aterm_effects::pet_glyphs_gen::PetGlyphId;
use aterm_effects::pet_world::{PetPane, PetWorld, PetWorldFacts};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;
const TICK_MS: u64 = 16;
const WORDS: [&[u8]; 6] = [b"hello ", b"world ", b"this ", b"is ", b"a ", b"test "];
/// Cells past the escort's own width inside which the station may already be
/// crossing to the wall side (the wall's hysteresis margin).
const WALL_BAND: f32 = 3.0;

struct Fx {
    brain: PetBrain,
    world: PetWorld,
    term: Terminal,
    now: Instant,
}

impl Fx {
    fn new() -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07");
        term.process(b"$ ");
        let mut fx = Self {
            brain: PetBrain::default(),
            world: PetWorld::default(),
            term,
            now: Instant::now(),
        };
        for _ in 0..40 {
            fx.tick(100);
        }
        fx
    }

    fn tick(&mut self, ms: u64) -> PetFrame {
        self.now += Duration::from_millis(ms);
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, 7);
        assert!(self.world.observe(&input, &facts, PetPane::full(&input)));
        // The host's ink seam (`pet_ink()` → `sense_ink`), every frame.
        let spans: Vec<(u16, u16)> = (0..usize::from(ROWS))
            .map(|row| {
                let inked = |col: &usize| self.world.ink_at(row, *col) == Some(true);
                let first = (0..usize::from(COLS)).find(inked);
                let last = (0..usize::from(COLS)).rfind(inked);
                first
                    .zip(last)
                    .map_or((0, 0), |(f, l)| (f as u16, l as u16 + 1))
            })
            .collect();
        let live = spans.iter().rposition(|&(f, e)| e > f).map(|r| r as u16);
        self.brain.sense_ink(0, &spans, live);
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(true);
        let c = self.term.cursor();
        self.brain.tick(PetSense {
            caret_drawn: true,
            now: self.now,
            caret: Some((c.row, c.col)),
            wrapped: false,
            rows: ROWS,
            cols: COLS,
            cell_w: CELL_W,
            cell_h: CELL_H,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }

    fn key(&mut self, ch: u8) {
        self.brain.note_console_input(self.now, PetInputKind::Text);
        self.term.process(&[ch]);
    }
}

/// One observed frame, reduced to what the oracle judges.
#[derive(Clone, Copy, Debug)]
struct Seen {
    caret: (u16, u16),
    /// Drawn body, grid pixels `(x0, x1, y0, y1)`.
    body: Option<(i32, i32, i32, i32)>,
    col: f32,
    flying: bool,
    facing_left: bool,
    brace: bool,
    /// Just-typed glyphs (caret row, left of the caret) under the body.
    covered: usize,
}

/// Glyphs on the caret's row, left of the caret, that a body rect covers.
fn covered(world: &PetWorld, caret: (u16, u16), body: Option<(i32, i32, i32, i32)>) -> usize {
    let Some((x0, x1, y0, y1)) = body else {
        return 0;
    };
    let (cw, ch) = (i32::from(CELL_W), i32::from(CELL_H));
    let row = usize::from(caret.0);
    let rows = (y0 / ch) as usize..((y1 + ch - 1) / ch) as usize;
    if !rows.contains(&row) {
        return 0;
    }
    let (c0, c1) = ((x0 / cw) as usize, ((x1 + cw - 1) / cw) as usize);
    (c0..c1.min(usize::from(caret.1)))
        .filter(|&col| world.ink_at(row, col) == Some(true))
        .count()
}

#[derive(Debug, Default)]
struct Feel {
    frames: usize,
    /// Frames whose caret leaves the escort room on its right.
    free: usize,
    covering: usize,
    /// …and frames covering at the WALL, where the stand crosses to the
    /// caret's left over the line by design — reported, not judged.
    wall_covering: usize,
    max_cover: usize,
    max_trail: f32,
    max_jump: f32,
    flips: usize,
    brace: usize,
    secs: f32,
}

impl Feel {
    fn covering_pct(&self) -> f32 {
        100.0 * self.covering as f32 / self.free as f32
    }

    fn flips_per_sec(&self) -> f32 {
        self.flips as f32 / self.secs
    }

    /// The oracle over a run of frames. Coverage and trail are judged only
    /// where the escort's station fits on the caret's RIGHT (at the wall it
    /// crosses to the left over the line by design), trail never in the air.
    fn judge(seen: &[Seen]) -> Self {
        let width = art_cols(CELL_W, CELL_H);
        let mut m = Self {
            frames: seen.len(),
            secs: seen.len() as f32 * TICK_MS as f32 / 1000.0,
            ..Self::default()
        };
        for (i, s) in seen.iter().enumerate() {
            let free_side = f32::from(s.caret.1) + 1.0 + width + WALL_BAND <= f32::from(COLS);
            m.free += usize::from(free_side);
            if s.covered > 0 {
                if free_side {
                    m.covering += 1;
                } else {
                    m.wall_covering += 1;
                }
            }
            if free_side {
                m.max_cover = m.max_cover.max(s.covered);
            }
            m.brace += usize::from(s.brace);
            if s.body.is_some() && !s.flying && free_side {
                m.max_trail = m.max_trail.max(f32::from(s.caret.1) - s.col);
            }
            if let Some(p) = i.checked_sub(1).map(|j| seen[j]) {
                if p.facing_left != s.facing_left {
                    m.flips += 1;
                }
                if !p.flying && !s.flying && p.body.is_some() && s.body.is_some() {
                    m.max_jump = m.max_jump.max((s.col - p.col).abs());
                }
            }
        }
        m
    }
}

/// Three bursts of `WORDS` at `ms_per_key`, each followed by `pause_ms`.
fn run(ms_per_key: u64, pause_ms: u64, trace: bool) -> (Vec<Seen>, Fx) {
    let mut f = Fx::new();
    let mut seen = Vec::new();
    let step = |f: &mut Fx, seen: &mut Vec<Seen>| {
        let fr = f.tick(TICK_MS);
        let c = f.term.cursor();
        let body = fr.body_px(CELL_W, CELL_H, COLS, ROWS);
        let s = Seen {
            caret: (c.row, c.col),
            body,
            col: fr.col,
            flying: fr.action.airborne() || fr.lift > 0.0,
            facing_left: fr.facing_left,
            brace: matches!(
                fr.pose,
                PetGlyphId::PetContactBrace | PetGlyphId::PetTailTuck
            ),
            covered: covered(&f.world, (c.row, c.col), body),
        };
        if trace {
            println!(
                "f{:4} cov={} caret={:2},{:2} col={:6.2} row={:5.2} lift={:.2} {:?} {:?} left={}",
                seen.len(),
                s.covered,
                c.row,
                c.col,
                fr.col,
                fr.row,
                fr.lift,
                fr.action,
                fr.pose,
                fr.facing_left
            );
        }
        seen.push(s);
    };
    for _ in 0..3 {
        // Keys on their own clock, frames on theirs: key `k` of the burst is
        // due at `k · ms_per_key`, and lands before the first frame past it.
        let mut clock = 0;
        for (k, &ch) in WORDS.concat().iter().enumerate() {
            f.key(ch);
            while clock + TICK_MS <= (k as u64 + 1) * ms_per_key {
                step(&mut f, &mut seen);
                clock += TICK_MS;
            }
        }
        for _ in 0..pause_ms / TICK_MS {
            step(&mut f, &mut seen);
        }
    }
    (seen, f)
}

/// `(ms per key, pause ms, covering % ceiling, flips/s ceiling)`. The flip
/// ceilings are this fixture's measurement before the escort was reworked
/// (1.00 / 0.72 / 0.56 per second, the settle-turn and the walk home
/// included): the rework may not turn the cat round more often than that.
const CADENCES: [(u64, u64, f32, f32); 3] = [
    (62, 1000, 10.0, 1.00),
    (100, 1000, 5.0, 0.72),
    (150, 700, 5.0, 0.56),
];

#[test]
fn the_escort_keeps_off_the_words_it_leads() {
    let runs: Vec<_> = CADENCES
        .iter()
        .map(|&(ms, pause, max_pct, max_flips)| {
            (
                ms,
                max_pct,
                max_flips,
                Feel::judge(&run(ms, pause, false).0),
            )
        })
        .collect();
    for (ms, _, _, m) in &runs {
        println!(
            "{ms}ms/key: frames={} covering={:.1}% (all frames {:.1}%, wall {}) max_cover={} max_trail={:.2} max_jump={:.2} flips={:.2}/s brace={}",
            m.frames,
            m.covering_pct(),
            100.0 * (m.covering + m.wall_covering) as f32 / m.frames as f32,
            m.wall_covering,
            m.max_cover,
            m.max_trail,
            m.max_jump,
            m.flips_per_sec(),
            m.brace
        );
    }
    for (ms, max_pct, max_flips, m) in &runs {
        assert!(
            m.covering_pct() < *max_pct,
            "{ms} ms/key: the body covers just-typed glyphs in {:.1}% of frames",
            m.covering_pct()
        );
        assert!(
            m.max_trail <= 4.0,
            "{ms} ms/key: trails {:.2} cells",
            m.max_trail
        );
        assert!(
            m.max_jump <= 1.2,
            "{ms} ms/key: a {:.2}-cell pop",
            m.max_jump
        );
        assert!(
            m.flips_per_sec() <= *max_flips,
            "{ms} ms/key: {:.2} flips/s",
            m.flips_per_sec()
        );
        assert_eq!(m.brace, 0, "{ms} ms/key: typing braced the escort");
    }
}

/// NEGATIVE CONTROL: the same oracle on the same run, with every drawn body
/// shifted two cells back — the follower lag this lane removed — must flag
/// the coverage and the trail. An oracle that cannot see a trailing cat
/// proves nothing.
#[test]
fn the_oracle_sees_a_trailing_escort() {
    let (seen, fx) = run(100, 1000, false);
    let cw = i32::from(CELL_W);
    let lagged: Vec<Seen> = seen
        .iter()
        .map(|s| {
            let body = s
                .body
                .map(|(x0, x1, y0, y1)| (x0 - 2 * cw, x1 - 2 * cw, y0, y1));
            Seen {
                body,
                col: s.col - 2.0,
                covered: covered(&fx.world, s.caret, body),
                ..*s
            }
        })
        .collect();
    let m = Feel::judge(&lagged);
    assert!(
        m.covering_pct() >= 5.0 && m.max_trail > 2.0,
        "the oracle missed a lagging body: {:.1}% covering, trail {:.2}",
        m.covering_pct(),
        m.max_trail
    );
}

#[test]
#[ignore = "diagnostic trace: MS=<ms per key>"]
fn escort_feel_trace() {
    let ms = std::env::var("MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let _ = run(ms, 1000, true);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE HAND TAKES THE WAKE IT BRIDGED — FROM EITHER SIDE** (2026-09-16).
//!
//! A word hop across the dark stretch a mid-line edit left lays a wake; the
//! key typed at its landing used to join whichever cohort `join_cohort`
//! found first. When the landing lay past the line's old end that was the
//! wake, and the first cut of `Ribbon::fold_wakes_into_band` folded it into
//! the band it touched. When the landing lay INSIDE the typed cohort's
//! bounds — which never shrink when the witness retires the cells under
//! them — the key joined the typed cohort instead, the wake stayed a wake
//! no key holds, and its cells expired on the hop's 0.55 s under a typing
//! hand: `###########..########` (the adversarial review of the first cut,
//! 540 ms after the hop). Every wake at the cell a typed key lays is folded
//! now, from either side; a band already on its way out is not taken back
//! by a key in a wake beside it; and no column ends up with two owners of
//! one cohort.
//!
//! Driven at the host seam through a real [`Terminal`], as
//! `tests/abandoned_ribbon.rs` drives it, with the plan census of
//! `tests/codex_particle_replay.rs`.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;
const CW: usize = 8;
const CH: usize = 16;
/// A cell counts as LIT at this planned coverage (`tests/codex_particle_replay.rs`).
const LIT_COV: u8 = 20;

fn geom() -> Geom {
    Geom {
        cw: CW,
        ch: CH,
        rows: ROWS,
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * CW) as u16,
        win_h: (ROWS * CH) as u16,
        head: 0,
    }
}

fn cfg() -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style: GlowStyle::RainbowKitty,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
    }
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
}

impl Host {
    fn at_row(row: u16) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(format!("\x1b[{};1H", row + 1).as_bytes());
        let mut h = Self {
            term,
            glow: CursorGlow::default(),
            cfg: cfg(),
            g: geom(),
            now: Instant::now(),
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
        };
        h.frame();
        h
    }

    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            self.glow.observe_ribbon_row(r, &self.row_buf);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
    }

    fn idle(&mut self, ms: u64) {
        let end = self.now + Duration::from_millis(ms);
        while self.now < end {
            self.now += Duration::from_millis(16);
            self.frame();
        }
    }

    fn key(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(90);
        self.glow.note_typed_cells(self.now, 1);
        self.term.process(bytes);
        self.frame();
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            let mut b = [0u8; 4];
            self.key(ch.encode_utf8(&mut b).as_bytes());
        }
    }

    /// A navigation key, as `app_input.rs` stamps an arrow.
    fn nav(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(90);
        self.glow.note_motion(self.now);
        self.term.process(bytes);
        self.frame();
    }

    /// The plan's coverage per column of `row`, as the glass shows it.
    fn plan_cov(&self, row: u16) -> [u8; COLS] {
        let mut cov = [0u8; COLS];
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        for sg in rib.plan_segments() {
            let r = ((sg.spine / CH as f32) - 0.5).floor();
            let c = (sg.x / CW as f32).floor();
            if r >= 0.0 && c >= 0.0 && r as usize == usize::from(row) && (c as usize) < COLS {
                let i = c as usize;
                cov[i] = cov[i].max(sg.cov);
            }
        }
        cov
    }

    /// The interior dark runs of the row's planned band.
    fn plan_holes(&self, row: u16) -> Vec<usize> {
        let cov = self.plan_cov(row);
        let lit: Vec<usize> = (0..COLS).filter(|&i| cov[i] >= LIT_COV).collect();
        let mut holes = Vec::new();
        if lit.len() < 2 {
            return holes;
        }
        let mut run = 0usize;
        for &v in &cov[lit[0]..=lit[lit.len() - 1]] {
            if v >= LIT_COV {
                if run > 0 {
                    holes.push(run);
                }
                run = 0;
            } else {
                run += 1;
            }
        }
        holes
    }

    fn cohort_count(&self, row: u16) -> usize {
        self.glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cohorts()
            .iter()
            .filter(|c| c.row == row)
            .count()
    }

    /// Program output with no key behind it, one frame on.
    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }
}

/// **THE LANDING INSIDE THE BAND'S STALE BOUNDS** (the review's finding 1).
/// A line, the caret scrubbed back into it, one glyph inserted (the witness
/// retires the shifted suffix; the cohort's bounds still span the old line),
/// a word hop across the dark stretch, a key at the landing, and typing on.
/// RED before this: the key joined the typed cohort by its bounds, the wake
/// at the landing stayed a wake, and its cells expired under the typing
/// hand — `###########..########` from 540 ms after the hop.
#[test]
fn a_word_hop_landing_inside_the_band_s_stale_bounds_folds_its_wake() {
    let mut h = Host::at_row(5);
    h.type_str("aaaa bbbb cd ef gh");
    h.idle(200);
    h.nav(b"\x1b[7D");
    h.nav(b"\x1b[6D");
    // An insert echo: the tail shifts right, `X` lands at the caret.
    h.key(b"\x1b[@X");
    h.idle(150);
    h.nav(b"\x1b[4C");
    h.nav(b"\x1b[3C");
    h.key(b"Y");
    assert_eq!(
        h.cohort_count(5),
        1,
        "the wake at the landing is band now: one cohort on the row"
    );
    let mut broken = Vec::new();
    for i in 0..8 {
        h.key(b"z");
        let holes = h.plan_holes(5);
        if !holes.is_empty() {
            broken.push((i, holes));
        }
        for _ in 0..5 {
            h.now += Duration::from_millis(16);
            h.frame();
            let holes = h.plan_holes(5);
            if !holes.is_empty() {
                broken.push((i, holes));
            }
        }
    }
    h.idle(200);
    assert!(
        broken.is_empty(),
        "the band under a typing hand carried interior dark runs: {broken:?}"
    );
}

/// **A BAND ON ITS WAY OUT IS NOT TAKEN BACK BY A KEY IN A WAKE** (the
/// review's finding 3). `hello`, rested into its retract, three Right
/// arrows off its end (a hop wake), a key inside the wake: the fold would
/// have put the key's hold on the fading band and brought its spent cells
/// back to full in one frame — `Ribbon::scrub`'s own measured defect.
#[test]
fn a_key_in_a_hop_wake_does_not_revive_a_retracting_band() {
    let mut h = Host::at_row(5);
    h.type_str("hello");
    h.idle(1300);
    let before = h.plan_cov(5);
    assert!(
        before[0] == 0 && before[1] < LIT_COV,
        "the band's far end has spent below the lit floor: {:?}",
        &before[..6]
    );
    h.nav(b"\x1b[C");
    h.nav(b"\x1b[C");
    h.nav(b"\x1b[C");
    h.key(b"x");
    let after = h.plan_cov(5);
    assert!(
        after[0] <= before[0] + 8 && after[1] <= before[1] + 8,
        "a key in the wake beside a retracting band lit its spent cells: {:?} -> {:?}",
        &before[..6],
        &after[..6]
    );
    assert_eq!(
        h.cohort_count(5),
        2,
        "the wake stays its own cohort beside the leaving band"
    );
}

/// **ONE OWNER PER COLUMN PER COHORT** (the review's finding 4). The hand's
/// own spent cells are still resident; a hop lays wake cells over them; the
/// key in the wake folds it into the band — and the band's cells under the
/// wake's go with it, so `place`'s law holds.
#[test]
fn the_wake_fold_keeps_one_owner_per_column_per_cohort() {
    let mut h = Host::at_row(5);
    h.program(b"\x1b[6;4H");
    h.type_str("hello");
    h.idle(500);
    h.nav(b"\x1b[5D");
    h.nav(b"\x1b[2D");
    h.key(b"x");
    let rib = h.glow.v2_ribbon().expect("rainbow kitty owns the frame");
    let mut owners: Vec<(u16, u32)> = rib
        .cells()
        .iter()
        .filter(|c| c.row == 5 && !c.leaving())
        .map(|c| (c.col, c.cohort))
        .collect();
    owners.sort_unstable();
    let before = owners.len();
    owners.dedup();
    assert_eq!(
        before,
        owners.len(),
        "two live owners of one column in one cohort: {owners:?}"
    );
}

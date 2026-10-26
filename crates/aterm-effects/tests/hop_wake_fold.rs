// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A typed key takes the hop wake it lands in, from either side.

use crate::common::{
    LIT_COV, caret_sample, coverage, holes_of, note_blink, observe_caret_row, observe_ribbon_rows,
    rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;
const CW: usize = 8;
const CH: usize = 16;
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
            glow: crate::common::quads_glow(),
            cfg: rainbow_cfg(),
            g: crate::common::geom(ROWS, COLS, CW, CH),
            now: Instant::now(),
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
        };
        h.frame();
        h
    }

    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink_seen, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
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

    fn nav(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(90);
        self.glow.note_motion(self.now);
        self.term.process(bytes);
        self.frame();
    }

    fn plan_cov(&self, row: u16) -> Vec<u8> {
        coverage(&self.glow, self.g, row)
    }

    fn plan_holes(&self, row: u16) -> Vec<usize> {
        holes_of(&self.plan_cov(row))
            .into_iter()
            .map(|(_, len)| len)
            .collect()
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

    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }
}

#[test]
fn a_word_hop_landing_inside_the_band_s_stale_bounds_folds_its_wake() {
    let mut h = Host::at_row(5);
    h.type_str("aaaa bbbb cd ef gh");
    h.idle(200);
    h.nav(b"\x1b[7D");
    h.nav(b"\x1b[6D");
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

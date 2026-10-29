// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-seam tests for abandoned cursor-ribbon bands across wraps, redraws, and relocations.

use crate::common::{
    caret_sample, note_blink, observe_caret_row, observe_ribbon_rows, rainbow_cfg, sync_scroll,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::ribbon::{
    FLOW_TOTAL_S, PHRASE_REST_MIN_S, RETRACT_DUR_S, RETRACT_FADE_S, WALK_LAY_RATE,
};
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
    /// The clock of the last typed key, for the grace-window guards.
    last_key: Instant,
    /// The content-scroll state the last frame saw (`app_render.rs`'s `sync_cursor_effect_scroll`).
    scroll: Option<ContentScrollState>,
}

impl Host {
    fn at_row(row: u16) -> Self {
        Self::sized(ROWS as u16, COLS as u16, row)
    }

    fn sized(rows: u16, cols: u16, row: u16) -> Self {
        let mut term = Terminal::new(rows, cols);
        term.process(format!("\x1b[{};1H", row + 1).as_bytes());
        let now = Instant::now();
        let mut h = Self {
            term,
            glow: crate::common::quads_glow(),
            cfg: rainbow_cfg(),
            g: crate::common::geom(usize::from(rows), usize::from(cols), CW, CH),
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            last_key: now,
            scroll: None,
        };
        h.frame();
        h
    }

    fn frame(&mut self) {
        sync_scroll(&self.term, &mut self.glow, &mut self.scroll, self.now);
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

    fn key(&mut self, bytes: &[u8], cells: u16) {
        self.now += Duration::from_millis(90);
        self.last_key = self.now;
        self.glow.note_typed_cells(self.now, cells);
        self.term.process(bytes);
        self.frame();
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            let cells = if ch.is_ascii() { 1 } else { 2 };
            let mut buf = [0u8; 4];
            self.key(ch.encode_utf8(&mut buf).as_bytes(), cells);
        }
    }

    fn nav(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(90);
        self.glow.note_navigation(self.now);
        self.term.process(bytes);
        self.frame();
    }

    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }

    fn cells(&self, row: u16) -> Vec<(u16, bool)> {
        let mut v: Vec<(u16, bool)> = self
            .glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cells()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| (c.col, c.leaving()))
            .collect();
        v.sort_unstable();
        v
    }

    fn cells_at(&self, row: u16, col: u16) -> Vec<(Instant, bool)> {
        let mut v: Vec<(Instant, bool)> = self
            .glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cells()
            .iter()
            .filter(|c| c.row == row && c.col == col)
            .map(|c| (c.born, c.leaving()))
            .collect();
        v.sort_unstable();
        v
    }

    fn live(&self, row: u16) -> Vec<u16> {
        self.cells(row)
            .into_iter()
            .filter(|&(_, leaving)| !leaving)
            .map(|(col, _)| col)
            .collect()
    }

    fn leaving(&self, row: u16) -> Vec<u16> {
        self.cells(row)
            .into_iter()
            .filter(|&(_, leaving)| leaving)
            .map(|(col, _)| col)
            .collect()
    }

    fn lit(&self, row: u16) -> bool {
        let centre = f32::from(self.g.origin_y) + (f32::from(row) + 0.5) * CH as f32;
        self.glow.under_quads().iter().any(|q| {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            q.w > 0 && q.alpha > 0 && y0 <= centre && centre < y1
        })
    }

    fn retired(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.retired)
    }

    fn followed(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.followed)
    }

    fn since_key(&self) -> f32 {
        self.now
            .saturating_duration_since(self.last_key)
            .as_secs_f32()
    }
}

const GRACE_S: f32 = PHRASE_REST_MIN_S;
const TEXT: &str = "hello world";

fn hello(row: u16) -> Host {
    let mut h = Host::at_row(row);
    h.type_str(TEXT);
    let want: Vec<u16> = (0..TEXT.len() as u16).collect();
    assert_eq!(h.live(row), want, "the typed band is laid under the text");
    assert!(h.lit(row));
    h
}

#[test]
fn a_wrapped_line_s_first_row_goes_out_on_its_own_clock_while_the_hand_types_on_the_second() {
    let mut h = Host::at_row(5);
    for i in 0..COLS {
        h.key(&[b'a' + (i % 26) as u8], 1);
    }
    let c = h.term.cursor();
    assert_eq!(c.row, 5, "eighty keys: the caret is still on row 5");
    assert!(h.lit(5), "row 5 is lit under the hand");
    let left_row_5 = h.now;
    let mut row_5_dark_at: Option<f32> = None;
    let mut row_5_lit_at_grace = false;
    for i in 0..26u16 {
        h.key(&[b'a' + (i % 26) as u8], 1);
        let c = h.term.cursor();
        assert_eq!(c.row, 6, "the wrap: the caret is on row 6");
        let since = h.now.saturating_duration_since(left_row_5).as_secs_f32();
        assert!(h.lit(6), "row 6 is lit under the hand at +{since:.2} s");
        if since < (RETRACT_DUR_S + RETRACT_FADE_S) * 0.9 {
            assert!(
                h.lit(5),
                "row 5 keeps its light through the flow's first half (+{since:.2} s)"
            );
            row_5_lit_at_grace = true;
        }
        if row_5_dark_at.is_none() && h.cells(5).is_empty() && !h.lit(5) {
            row_5_dark_at = Some(since);
        }
    }
    assert!(row_5_lit_at_grace, "the flow's first half was observed");
    let dark = row_5_dark_at.expect(
        "row 5 must go dark while the hand is still typing on row 6 — a global hold          keeps it at full light for as long as any key is live",
    );
    assert!(
        (FLOW_TOTAL_S..=FLOW_TOTAL_S + 0.09 + 0.1).contains(&dark),
        "row 5 went out at +{dark:.2} s after the caret left it; the bound is the \
         fold flow's own span, {FLOW_TOTAL_S} s from the wrap key, and never past \
         one key's cadence after it"
    );
    assert!(h.lit(6), "…and the live row's band is untouched");
    assert!(
        h.live(6).len() >= 20,
        "the live row holds the keys typed on it: {:?}",
        h.live(6)
    );
}

fn walk_on(h: &Host, row: u16) -> Vec<(u16, f32)> {
    let mut v: Vec<(u16, f32)> = h
        .glow
        .v2_ribbon()
        .expect("rainbow kitty owns the frame")
        .cells()
        .iter()
        .filter(|c| c.row == row && !c.leaving())
        .map(|c| (c.col, c.t))
        .collect();
    v.sort_unstable_by_key(|&(col, _)| col);
    v
}

fn typed_to_the_margin() -> (Host, f32) {
    let mut h = Host::at_row(5);
    for i in 0..COLS {
        h.key(&[b'a' + (i % 26) as u8], 1);
    }
    assert_eq!(h.term.cursor().row, 5, "eighty keys: the caret is on row 5");
    let above = walk_on(&h, 5);
    assert!(above.len() >= 60, "row 5 is laid under the text: {above:?}");
    let n = above.len();
    let (c_prev, t_prev) = above[n - 2];
    let (c_last, t_last) = above[n - 1];
    let pace = (t_last - t_prev) / f32::from(c_last - c_prev);
    assert!(
        (pace - WALK_LAY_RATE).abs() < 1e-4,
        "the premise: row 5 lays at WALK_LAY_RATE at its end — {t_prev} @{c_prev}, \
         {t_last} @{c_last}"
    );
    (h, pace)
}

#[test]
fn a_shell_fold_continues_the_walk_at_the_pace_the_row_above_had_reached() {
    let (mut h, pace) = typed_to_the_margin();
    for i in 0..30u16 {
        h.key(&[b'a' + (i % 26) as u8], 1);
    }
    assert_eq!(h.term.cursor().row, 6, "the fold: the caret is on row 6");
    let below = walk_on(&h, 6);
    assert!(
        below.len() >= 24 && below[0].0 == 0,
        "row 6 is laid from column 0 past the walk's fast phase: {below:?}"
    );
    for w in below.windows(2) {
        let (c0, t0) = w[0];
        let (c1, t1) = w[1];
        assert!(
            c1 == c0 + 1 && (t1 - t0 - pace).abs() < 1e-4,
            "row 6's step from column {c0} to {c1} is {} — row 5 laid at {pace} a cell \
             at its end; the walk restarted its pace at the fold: {below:?}",
            t1 - t0
        );
    }
}

#[test]
fn a_shell_fold_s_first_cell_is_the_row_above_s_last_glyph_continued_one_step() {
    let (mut h, pace) = typed_to_the_margin();
    h.key(b"a", 1);
    assert_eq!(h.term.cursor().row, 6, "the fold: the caret is on row 6");
    let above = walk_on(&h, 5);
    let below = walk_on(&h, 6);
    let (c_last, t_last) = *above.last().expect("row 5 is laid");
    let (c_first, t_first) = *below.first().expect("row 6's first cell is laid");
    assert_eq!(
        (c_last, c_first),
        (COLS as u16 - 1, 0),
        "the fold's two glyphs are laid: row 5 {above:?}, row 6 {below:?}"
    );
    let want = t_last + pace;
    assert!(
        (t_first - want).abs() < 1e-4,
        "the colour crosses the fold one step at a time: row 6's first cell is \
         {t_first}, row 5's last glyph ({t_last} @{c_last}) continued one step is {want}"
    );
}

fn flowing(h: &Host, row: u16) -> bool {
    h.glow
        .v2_ribbon()
        .expect("rainbow kitty owns the frame")
        .cohorts()
        .iter()
        .any(|k| k.row == row && k.flow.is_some())
}

fn dark_after(h: &mut Host, row: u16, since: Instant, flowed: &mut bool) -> Option<u64> {
    let end = h.now + Duration::from_millis(3000);
    while h.now < end {
        h.now += Duration::from_millis(16);
        h.frame();
        *flowed |= flowing(h, row);
        if !h.lit(row) {
            return Some(h.now.saturating_duration_since(since).as_millis() as u64);
        }
    }
    None
}

#[test]
fn a_backspace_back_up_through_a_shell_fold_holds_the_row_under_the_parked_hand() {
    for pause_ms in [0u64, 1000] {
        let control = {
            let mut h = Host::at_row(5);
            for i in 0..79u8 {
                h.key(&[b'a' + i % 26], 1);
            }
            h.now += Duration::from_millis(180 + pause_ms);
            let bs = h.now;
            h.glow.note_backspace(h.now);
            h.term.process(b"\x08\x1b[K");
            h.frame();
            let mut flowed = false;
            let dark =
                dark_after(&mut h, 5, bs, &mut flowed).expect("the control goes dark inside 3 s");
            assert!(!flowed, "the control never flows");
            dark
        };
        for late in [false, true] {
            let what = format!(
                "{} ms pause, echo {}",
                pause_ms,
                if late { "a frame late" } else { "with its key" }
            );
            let (mut h, _) = typed_to_the_margin();
            h.key(b"x", 1);
            assert_eq!(h.term.cursor().row, 6, "{what}: `x` folds onto row 6");
            let bs1 = h.now + Duration::from_millis(90 + pause_ms);
            while h.now + Duration::from_millis(16) < bs1 {
                h.now += Duration::from_millis(16);
                h.frame();
            }
            assert!(
                flowing(&h, 5) && h.lit(5),
                "{what}: the premise: row 5 is flowing, and lit, when the hand comes back"
            );
            let rest = h.glow.v2_ribbon().expect("the ribbon").rest_s();
            assert!(
                pause_ms == 0 || (90 + pause_ms) as f32 >= rest * 1000.0,
                "{what}: the premise: the run begins past the {rest} s rest"
            );
            h.now = bs1;
            h.glow.note_backspace(h.now);
            h.term.process(b"\x08\x1b[K");
            h.frame();
            assert_eq!(
                (h.term.cursor().row, h.term.cursor().col),
                (6, 0),
                "{what}: the first Backspace erased the `x`"
            );
            h.now += Duration::from_millis(90);
            let bs2 = h.now;
            h.glow.note_backspace(h.now);
            if late {
                h.frame();
                h.now += Duration::from_millis(16);
            }
            h.term.process(b"\x1b[A\x1b[80G\x1b[K");
            h.frame();
            assert_eq!(
                (h.term.cursor().row, h.term.cursor().col),
                (5, 79),
                "{what}: the second Backspace went up through the fold"
            );
            assert!(
                !flowing(&h, 5),
                "{what}: row 5 still flows on the frame the hand came back to it"
            );
            let mut flowed = false;
            let dark = dark_after(&mut h, 5, bs2, &mut flowed);
            assert!(!flowed, "{what}: row 5 flowed again under the parked hand");
            let dark = dark.expect("row 5 goes dark inside 3 s");
            assert!(
                dark >= control,
                "{what}: row 5 went dark +{dark} ms after the hand came back to it; the \
                 never-folded control holds to +{control} ms"
            );
        }
    }
}

fn typed_to_the_margin_of(rows: u16, cols: u16, row: u16) -> (Host, f32, f32) {
    let mut h = Host::sized(rows, cols, row);
    for i in 0..cols {
        h.key(&[b'a' + (i % 26) as u8], 1);
    }
    assert_eq!(h.term.cursor().row, row, "the caret is still on row {row}");
    let above = walk_on(&h, row);
    let n = above.len();
    assert!(
        n >= usize::from(cols) - 2 && above[n - 1].0 == cols - 2,
        "row {row} is laid up to the column before the margin: {above:?}"
    );
    let (c_prev, t_prev) = above[n - 2];
    let (c_last, t_last) = above[n - 1];
    (h, t_last, (t_last - t_prev) / f32::from(c_last - c_prev))
}

#[test]
fn a_fold_s_seam_holds_when_two_keys_echoes_land_on_one_frame() {
    for (rows, cols, row) in [(24u16, 80u16, 5u16), (24, 80, 23), (24, 12, 5)] {
        for lagged in [false, true] {
            let what = format!(
                "{cols} columns on row {row}, {}",
                if lagged {
                    "the fold key's echo lagging to the next key's"
                } else {
                    "two keys 6 ms apart on one frame"
                }
            );
            let (mut h, t_before, pace) = typed_to_the_margin_of(rows, cols, row);
            let t_last = t_before + pace;
            h.now += Duration::from_millis(90);
            h.last_key = h.now;
            h.glow.note_typed_cells(h.now, 1);
            if lagged {
                h.now += Duration::from_millis(16);
                h.frame();
                h.now += Duration::from_millis(16);
                h.frame();
                h.now += Duration::from_millis(8);
            } else {
                h.term.process(b"a");
                h.now += Duration::from_millis(6);
            }
            h.last_key = h.now;
            h.glow.note_typed_cells(h.now, 1);
            h.term.process(if lagged { b"ab" } else { b"b" });
            h.frame();
            let (above_row, below_row) = if row + 1 == rows {
                (row - 1, row)
            } else {
                (row, row + 1)
            };
            assert_eq!(h.term.cursor().row, below_row, "{what}: the fold");
            for _ in 0..4u16 {
                h.key(b"c", 1);
            }
            let above = walk_on(&h, above_row);
            let below = walk_on(&h, below_row);
            let &(c_above, t_above) = above.last().expect("the row above is laid");
            assert!(
                c_above == cols - 1 && (t_above - t_last).abs() < 1e-4,
                "{what}: the margin glyph continues its row one step, {t_last}: {above:?}"
            );
            assert!(
                below.len() >= 4 && below[0].0 == 0 && below[1].0 == 1,
                "{what}: the new row is laid from its first column: {below:?}"
            );
            for (k, &(col, t)) in below.iter().take(4).enumerate() {
                let want = t_last + (k as f32 + 1.0) * pace;
                assert!(
                    (t - want).abs() < 1e-4,
                    "{what}: the new row's column {col} is {t} — the margin glyph {t_last} \
                     continued {} steps of {pace} is {want}: above {above:?}, below {below:?}",
                    k + 1
                );
            }
        }
    }
}

#[test]
fn the_box_growth_wrap_carries_the_band_up_with_its_text_and_the_flush_pays_its_landing() {
    let mut h = hello(6);
    h.key(b"\x1b[6;1Hhello world\x1b[7;1H\x1b[2Kx", 1);
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (6, 1),
        "the caret stayed on its row and re-anchored left"
    );
    let want: Vec<u16> = (0..11).collect();
    assert!(
        h.cells(6).is_empty(),
        "the band left the caret row with its text, unstamped: {:?}",
        h.cells(6)
    );
    assert_eq!(
        h.live(5),
        want,
        "…and stands under its text one row up, every cell and the space between"
    );
    assert_eq!(h.retired(), 0, "nothing was retired by content");
    assert_eq!(h.followed(), 11, "every cell followed its text");
    assert!(h.lit(5), "the row the text moved to carries the band");
    assert_eq!(
        h.glow.in_flight_tally().park_flushed,
        0,
        "…held, not judged: nothing flushed yet"
    );
    let flowing = h
        .glow
        .v2_ribbon()
        .expect("rainbow kitty owns the frame")
        .cohorts()
        .iter()
        .filter(|k| k.row == 5)
        .all(|k| k.flow.is_some());
    assert!(
        flowing,
        "the run the text carried away from the caret's row flows"
    );
    h.type_str("yz");
    assert_eq!(
        h.glow.in_flight_tally().park_flushed,
        1,
        "the next key flushed the park"
    );
    assert_eq!(
        h.live(6),
        vec![0, 1, 2],
        "the wrap's own landing and the keys typed since, and only them"
    );
    h.idle(200);
    assert!(h.since_key() < GRACE_S);
    assert_eq!(
        h.cells(6),
        vec![(0, false), (1, false), (2, false)],
        "the typed cells stand on the caret row"
    );
    assert!(h.lit(5), "the followed band is still flowing 0.4 s on");
    assert!(h.lit(6));
    h.idle((FLOW_TOTAL_S * 1000.0) as u64);
    assert!(
        h.cells(5).is_empty() && !h.lit(5),
        "the followed band flowed out inside FLOW_TOTAL_S: {:?}",
        h.cells(5)
    );
    assert_eq!(h.retired(), 0, "…and still nothing was retired by content");
}

#[test]
fn a_prompt_redraw_that_puts_the_same_text_back_keeps_every_cell() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[2J\x1b[6;1Hhello world");
    assert!(
        h.since_key() < GRACE_S,
        "inside the grace: the swoosh is not the ending here"
    );
    let want: Vec<u16> = (0..11).collect();
    assert_eq!(
        h.live(5),
        want,
        "the band is untouched by a redraw of the same text"
    );
    assert!(h.leaving(5).is_empty(), "nothing is leaving");
    assert!(h.lit(5));
    assert_eq!(
        h.retired(),
        0,
        "the witness fired on a redraw of the same text"
    );
    h.idle(200);
    assert_eq!(h.live(5), want, "…and it stays");
    assert_eq!(h.retired(), 0);
}

#[test]
fn a_relocated_input_box_carries_the_band_with_its_text_and_stamps_nothing() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[2J\x1b[8;1Hhello world");
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (7, 11), "the caret went with the text");
    assert_eq!(
        h.glow.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "the relocation had no key behind it"
    );
    let want: Vec<u16> = (0..11).collect();
    assert!(
        h.cells(5).is_empty(),
        "the band left row 5 with its text: {:?}",
        h.cells(5)
    );
    assert_eq!(h.live(7), want, "…and stands under it on row 7, unstamped");
    assert_eq!(h.retired(), 0, "nothing was retired by content");
    assert_eq!(h.followed(), 11, "every cell followed its text");
    assert!(h.lit(7), "row 7 is lit under the text the hand typed");
    assert!(!h.lit(5), "…and row 5 is dark");
    h.idle(200);
    assert!(
        h.since_key() < GRACE_S,
        "inside the grace: the swoosh could not have taken it"
    );
    assert_eq!(h.live(7), want, "the band stays under its text");
    assert!(h.cells(5).is_empty());
    assert!(h.lit(7));
    assert!(!h.lit(5));
}

#[test]
fn a_killed_line_under_an_identical_line_is_released_not_followed_onto_it() {
    for above in ["cd ..", "ls -la"] {
        let mut h = Host::at_row(4);
        h.program(format!("$ {above}\r\n$ ").as_bytes());
        h.type_str("cd ..");
        h.idle(64);
        assert_eq!(h.live(5), vec![2, 3, 4, 5, 6], "the band is under `cd ..`");
        assert!(!h.lit(4), "row 4 is dark before the kill");
        h.program(b"\r$ \x1b[K");
        assert_eq!(h.followed(), 0, "above `{above}`: nothing followed");
        let mut lit4 = 0;
        for _ in 0..90 {
            h.idle(16);
            lit4 += usize::from(h.lit(4));
        }
        assert_eq!(
            lit4, 0,
            "above `{above}`: the previous command's row never lights"
        );
        assert!(h.cells(4).is_empty(), "above `{above}`: {:?}", h.cells(4));
    }
}

#[test]
fn a_ctrl_u_under_an_identical_line_drains_on_its_own_row() {
    let mut h = Host::at_row(4);
    h.program(b"$ cd ..\r\n$ ");
    h.type_str("cd ..");
    h.idle(300);
    h.now += Duration::from_millis(90);
    h.glow.note_kill(h.now, true);
    h.term.process(b"\x08\x08\x08\x08\x08\x1b[K");
    h.frame();
    assert_eq!(h.followed(), 0, "nothing followed onto the line above");
    let (mut lit4, mut lit5) = (0, 0);
    for _ in 0..90 {
        h.idle(16);
        lit4 += usize::from(h.lit(4));
        lit5 += usize::from(h.lit(5));
    }
    assert_eq!(lit4, 0, "the previous command's row never lights");
    assert!(lit5 > 0, "the kill's swoosh drains on the killed row");
}

#[test]
fn the_next_key_after_a_declined_relocation_lays_at_the_caret_s_true_cell() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[2J\x1b[8;1Hhello world");
    h.idle(200);
    assert!(h.cells(5).is_empty());
    h.key(b"x", 1);
    let want: Vec<u16> = (0..12).collect();
    assert_eq!(
        h.live(7),
        want,
        "the key laid its own cell on row 7 beside the band that followed"
    );
    assert!(
        h.cells(5).is_empty(),
        "…and nothing on the row the caret left"
    );
    assert!(h.lit(7));
}

#[test]
fn a_band_overwritten_with_different_text_is_retired() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[6;1HHELLO WORLD");
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (5, 11), "the caret did not move");
    let want: Vec<u16> = (0..11).collect();
    assert_eq!(
        h.leaving(5),
        want,
        "every overwritten glyph's cell is retired, and the blank with its run"
    );
    assert!(h.live(5).is_empty());
    assert_eq!(h.retired(), 11);
    h.idle(200);
    assert!(h.since_key() < GRACE_S);
    assert!(
        h.cells(5).is_empty(),
        "the retired band is out of the pool inside 200 ms"
    );
    assert!(!h.lit(5));
}

#[test]
fn a_composer_repaint_does_not_cut_a_live_ribbon_at_each_space() {
    const PHRASE: &str = "git commit and git pull and work on remote main";
    let mut h = Host::at_row(5);
    h.type_str(PHRASE);
    h.idle(64);
    let spaces: Vec<u16> = PHRASE
        .bytes()
        .enumerate()
        .filter_map(|(col, b)| (b == b' ').then_some(col as u16))
        .collect();
    assert_eq!(h.live(5).len(), PHRASE.len());
    assert_eq!(spaces.len(), 9, "exercise several word boundaries");

    let edited = PHRASE.replacen('g', "G", 1);
    h.program(format!("\x1b[6;1H\x1b[2K{edited}").as_bytes());
    assert_eq!(h.leaving(5), vec![0], "only the replaced glyph retires");
    for _ in 0..12 {
        let live = h.live(5);
        for &col in &spaces {
            assert!(live.contains(&col), "space {col} lost its ribbon");
            let x = (f32::from(col) + 0.5) * CW as f32;
            let y = 5.5 * CH as f32;
            assert!(
                h.glow.under_quads().iter().any(|q| {
                    q.alpha > 0
                        && f32::from(q.x) <= x
                        && x < f32::from(q.x) + f32::from(q.w)
                        && f32::from(q.y) <= y
                        && y < f32::from(q.y) + f32::from(q.h)
                }),
                "space {col} has a record but no painted ribbon"
            );
        }
        h.idle(16);
    }
    assert!(h.since_key() < GRACE_S);
    assert_eq!(h.retired(), 1);

    h.program(format!("\x1b[6;1H\x1b[2K{}", PHRASE.to_ascii_uppercase()).as_bytes());
    assert!(h.live(5).is_empty(), "a fully replaced run keeps no spaces");
    h.idle(200);
    assert!(h.cells(5).is_empty());
    assert!(!h.lit(5));
}

#[test]
fn a_wide_glyph_s_two_cells_are_retired_as_one_unit() {
    let mut h = Host::at_row(5);
    h.type_str("ab\u{4f60}");
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (5, 4), "a + b + wide = four cells");
    assert_eq!(
        h.live(5),
        vec![0, 1, 2, 3],
        "both cells of the wide glyph are laid"
    );
    h.idle(400);
    h.program(b"\x1b[6;3Hxy\x1b[6;5H");
    assert_eq!(
        h.leaving(5),
        vec![2, 3],
        "the wide glyph's two cells go together"
    );
    assert_eq!(h.live(5), vec![0, 1], "its neighbours are untouched");
    assert_eq!(h.retired(), 2);

    let mut h = Host::at_row(5);
    h.type_str("ab\u{4f60}");
    h.idle(400);
    h.program("\x1b[6;3H\u{597d}\x1b[6;5H".as_bytes());
    assert_eq!(h.leaving(5), vec![2, 3]);
    assert_eq!(h.live(5), vec![0, 1]);

    let mut h = Host::at_row(5);
    h.type_str("a\u{4f60}b");
    h.idle(400);
    h.program(b"\x1b[6;2Hxy\x1b[6;5H");
    assert_eq!(
        h.leaving(5),
        Vec::<u16>::new(),
        "an interior change stays lit"
    );
    assert_eq!(h.live(5), vec![0, 1, 2, 3]);
    assert_eq!(h.retired(), 0);
}

#[test]
fn the_box_growth_wrap_carries_the_band_up_and_a_silent_flush_lays_its_landing() {
    let mut h = hello(6);
    h.key(b"\x1b[6;1Hhello world\x1b[7;1H\x1b[2Kx", 1);
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (6, 1),
        "the caret stayed on its row and re-anchored left"
    );
    let want: Vec<u16> = (0..11).collect();
    assert!(
        h.cells(6).is_empty(),
        "the band left the caret row with its text: {:?}",
        h.cells(6)
    );
    assert_eq!(h.live(5), want, "…and stands under it one row up");
    assert_eq!(h.retired(), 0);
    assert_eq!(h.followed(), 11);
    assert!(h.lit(5), "the row the text moved to carries the band");
    h.idle(200);
    assert!(h.since_key() < GRACE_S);
    assert!(
        h.cells(6).is_empty(),
        "nothing on the caret row yet — the park is still held"
    );
    assert!(h.lit(5), "the followed band is flowing");
    assert_eq!(h.glow.in_flight_tally().park_flushed, 0);
    h.idle(150);
    assert_eq!(h.glow.in_flight_tally().park_flushed, 1);
    assert!(h.since_key() < GRACE_S);
    assert_eq!(
        h.cells(6),
        vec![(0, false)],
        "the flush's re-anchor lays the key's own cell at the landing"
    );
    assert!(h.lit(6), "…and it is on the glass");
    assert!(h.lit(5), "the followed band is still flowing at +0.35 s");
    h.idle((FLOW_TOTAL_S * 1000.0) as u64);
    assert!(
        h.cells(5).is_empty() && !h.lit(5),
        "…and gone by the flow's end: {:?}",
        h.cells(5)
    );
}

#[test]
fn a_re_anchor_onto_the_pane_s_first_column_lights_nothing_in_the_pane_beside_it() {
    const PANE_COL0: u16 = 40;
    let mut h = Host::at_row(6);
    h.glow.note_pane_columns(PANE_COL0, 40);
    h.term.process(b"\x1b[7;41H");
    h.frame();
    h.type_str(TEXT);
    let want: Vec<u16> = (PANE_COL0..PANE_COL0 + TEXT.len() as u16).collect();
    assert_eq!(
        h.live(6),
        want,
        "the band is laid under the pane's own text"
    );
    let retired_before = h.retired();
    h.key(b"\x1b[7;41H", 1);
    let c = h.term.cursor();
    assert_eq!(
        (c.row, c.col),
        (6, PANE_COL0),
        "the caret re-anchored onto the pane's first column"
    );
    h.idle(400);
    assert_eq!(
        h.glow.in_flight_tally().park_flushed,
        1,
        "the park was judged"
    );
    assert!(
        h.since_key() < GRACE_S,
        "inside the idle grace: this is the explicit re-anchor drain"
    );
    let strays: Vec<(u16, bool)> = h
        .cells(6)
        .into_iter()
        .filter(|&(col, _)| col < PANE_COL0)
        .collect();
    assert!(
        strays.is_empty(),
        "nothing is lit outside the focused pane; the re-anchor's landing put \
         {strays:?} in the split beside it"
    );
    assert!(
        h.live(6).is_empty(),
        "the old line is draining, not renewed"
    );
    assert_eq!(
        h.retired(),
        retired_before,
        "unchanged glyphs do not fire the content witness"
    );
    let draining = h.cells(6);
    assert!(
        !draining.is_empty(),
        "the explicit drain fades rather than dropping the whole band"
    );
    assert!(
        draining
            .iter()
            .all(|&(col, leaving)| want.contains(&col) && leaving),
        "only the original pane-local cells remain on their drain: {draining:?}"
    );
    h.idle(700);
    assert!(
        h.cells(6).is_empty(),
        "the re-anchored line finishes its bounded drain"
    );
    h.key(b"Z", 1);
    assert_eq!(
        h.live(6),
        vec![PANE_COL0],
        "fresh typing lights its own pane's first cell"
    );
}

#[test]
fn a_key_typed_over_an_abandoned_cohorts_cell_keeps_its_own_light() {
    let mut h = hello(5);
    h.nav(b"\x1b[A");
    h.nav(b"\x1b[B");
    for _ in 0..3 {
        h.nav(b"\x1b[D");
    }
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (5, 8), "Up, Down, Left×3 from (5,11)");
    let before = h.cells_at(5, 8);
    assert!(
        !before.is_empty() && !before[0].1,
        "the stale owner at (5,8) is resident and abandoned, not leaving: {before:?}"
    );
    let stale_born = before[0].0;
    let retired_before = h.retired();

    h.key(b"X", 1);
    let c = h.term.cursor();
    assert_eq!((c.row, c.col), (5, 9), "the key echoed at (5,8)");
    let at = h.cells_at(5, 8);
    assert!(
        at.len() >= 2,
        "the stale cell and the fresh one at (5,8): {at:?}"
    );
    assert_eq!(at[0].0, stale_born, "oldest first is the stale cell");
    let fresh = at[at.len() - 1];
    assert!(
        !fresh.1,
        "the key's own cell keeps its light on its birth frame: {at:?}"
    );
    let fresh_born = fresh.0;
    assert!(fresh_born > stale_born);
    assert_eq!(
        h.retired() - retired_before,
        0,
        "one glyph changed inside a standing run names nothing — not the stale cell, not the fresh one, not the run's blank"
    );
    assert!(
        h.cells(5).contains(&(5, false)),
        "the run is standing and its blank kept its light: {:?}",
        h.cells(5)
    );

    h.idle(200);
    assert!(h.since_key() < GRACE_S);
    let at = h.cells_at(5, 8);
    assert_eq!(
        at,
        vec![(fresh_born, false)],
        "200 ms on: the stale cell is out of the pool, the key's cell stands"
    );
    assert!(h.lit(5), "the key's light is on the glass");
}

#[test]
fn trail_status_appends_ribbon_retired_after_the_v2_rows() {
    let mut h = hello(5);
    h.idle(400);
    h.program(b"\x1b[6;1HHELLO WORLD");
    let status = h.glow.v2_status().expect("v2 owns the frame");
    assert_eq!(status.retired, 11);
    let line = trail_line(&h);
    assert!(
        line.ends_with(" ribbon_retired=11 ribbon_followed=0 ribbon_follow_missed=0"),
        "the three content counts are the last fields: {line}"
    );
    assert!(
        line.contains(" v2_meteors=0 v2_bridged=0 ribbon_retired=11 ribbon_followed=0"),
        "{line}"
    );
}

fn trail_line(h: &Host) -> String {
    aterm_effects::cursor_glow::TrailStatus {
        style_raw: "rainbow kitty",
        style: Some(GlowStyle::RainbowKitty),
        unknown: false,
        config_enabled: true,
        effective: true,
        focused: true,
        motion_stage: "full",
        motion_mode: "auto",
        shed: 1.0,
        intensity: 0.7,
        sound_seam: true,
        ribbon_look: "tall",
        tally: h.glow.admission_tally(),
        spawns: h.glow.spawns(),
        ribbon_segments: h.glow.ribbon_segments(),
        ribbon_hue_bands: h.glow.ribbon_hue_bands(),
        ribbon_drawn: h.glow.ribbon_drawn(),
        ribbon_curtain_ms: None,
        field: h.glow.rainbow_field(),
        sparks: h.glow.live_sparks(),
        momentum: 0.0,
        momentum_display: 0.0,
        momentum_glow: 0.0,
        glow_active: true,
        pet_active: false,
        pet_action: "none",
        pet_content: 0.0,
        pet_pending: 0,
        pet_focus: "none",
        pet_reason: "none",
        pet_anchor: None,
        pet_event_seq: 0,
        pet_pose: "none",
        pet_deep: false,
        pet_body: None,
        head_body: None,
        cat_active: false,
        block_fill: None,
        flow: h.glow.flow_status(),
        inserts: aterm_effects::cursor_glow::InsertTally::default(),
        in_flight: aterm_effects::cursor_glow::InFlightTally::default(),
        comet_cells: 0,
        comet_alpha_max: 0,
    }
    .line_v2(h.glow.v2_status())
}

fn follow_missed(h: &Host) -> u64 {
    h.glow.v2_status().map_or(0, |s| s.follow_missed)
}

#[test]
fn a_line_that_arrives_a_row_up_around_a_hole_is_counted_missed() {
    for (now, missed, followed) in [
        ("hel   world", 10, 0),
        ("hello world", 0, 11),
        ("jumpy frogs", 0, 0),
    ] {
        let mut h = hello(5);
        h.idle(64);
        let (missed0, followed0) = (follow_missed(&h), h.followed());
        assert_eq!((missed0, followed0), (0, 0), "the premise: nothing yet");
        h.program(format!("\x1b[6;1H\x1b[2K\x1b[5;1H{now}").as_bytes());
        assert_eq!(
            (follow_missed(&h), h.followed()),
            (missed, followed),
            "`{now}` a row up: (missed, followed)"
        );
        h.idle(300);
        assert_eq!(
            follow_missed(&h),
            missed,
            "`{now}` a row up: counted once, not per frame"
        );
    }
}

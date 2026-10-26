// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that word-scrubbing back and forth across a typed line through zsh or an Ink-style
//! composer never damages the rainbow kitty band or alters the classic trail.

use aterm_core::render::{GlowQuad, TrailCell};
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::cursor_trail::{CursorTrail, TrailConfig};
use aterm_effects::rainbow_kitty::ribbon::{Cohort, Layer, Ribbon, SWOOSH_TOTAL_S};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

const ROWS: usize = 30;
const COLS: usize = 56;
const CW: usize = 8;
const CH: usize = 16;
const LINE: &str = "the quick brown fox jumps over the lazy dogs";
const ROW: u16 = 2;
const KEY_MS: u64 = 83;
const HOP_MS: u64 = 150;
const FRAME_MS: u64 = 16;
const INK_SPLIT_MS: u64 = 30;

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
        audible: true,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
    }
}

fn trail_cfg() -> TrailConfig {
    TrailConfig {
        enabled: true,
        color: 0x0050_FA7B,
        duration: Duration::from_millis(240),
        max_len: 18,
        intensity: 0.7,
        warmth: 0.0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Composer {
    Zsh,
    Ink,
}

impl Composer {
    fn spent_ms(self) -> u64 {
        match self {
            Self::Zsh => 0,
            Self::Ink => INK_SPLIT_MS,
        }
    }
}

type Column = (Option<(u32, Instant, bool, u32)>, u8);

struct Host {
    term: Terminal,
    glow: CursorGlow,
    trail: CursorTrail,
    twin: CursorTrail,
    cfg: GlowConfig,
    tcfg: TrailConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    tout: Vec<TrailCell>,
    twin_out: Vec<TrailCell>,
    row_buf: Vec<char>,
    blink_seen: u64,
    composer: Composer,
    text: String,
    caret: usize,
    trail_disagreements: Vec<(u64, usize, usize)>,
}

impl Host {
    fn new(composer: Composer) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = crate::common::quads_glow();
        let mut trail = CursorTrail::default();
        let mut twin = CursorTrail::default();
        glow.note_pane_columns(0, COLS);
        trail.note_pane_columns(0, COLS);
        twin.note_pane_columns(0, COLS);
        match composer {
            Composer::Zsh => term.process(format!("\x1b[{};1H", ROW + 1).as_bytes()),
            Composer::Ink => term.process(b"\x1b[?1049h\x1b[2J\x1b[3;1H"),
        }
        let mut h = Self {
            term,
            glow,
            trail,
            twin,
            cfg: cfg(),
            tcfg: trail_cfg(),
            g: geom(),
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            tout: Vec::new(),
            twin_out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            composer,
            text: String::new(),
            caret: 0,
            trail_disagreements: Vec::new(),
        };
        h.frame();
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
            self.trail.note_repaint_blink(self.now);
            self.twin.note_repaint_blink(self.now);
        }
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
        self.trail.note_context(alt);
        self.twin.note_context(alt);
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
        self.trail.tick(cur, self.now, &self.tcfg, &mut self.tout);
        self.twin
            .tick(cur, self.now, &self.tcfg, &mut self.twin_out);
        if self.tout != self.twin_out {
            self.trail_disagreements
                .push((self.ms(), self.tout.len(), self.twin_out.len()));
        }
    }

    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        self.frame();
    }

    fn idle_to(&mut self, t: Instant) {
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.step();
        }
    }

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.idle_to(t);
    }

    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = t;
        self.last_event = t;
    }

    fn ink_redraw(&mut self) {
        let head = format!("\x1b[?25l\x1b[{};1H\x1b[2K{}", ROW + 1, self.text);
        self.term.process(head.as_bytes());
        self.frame();
        self.now += Duration::from_millis(INK_SPLIT_MS);
        let tail = format!("\x1b[{};{}H\x1b[?25h", ROW + 1, self.caret + 1);
        self.term.process(tail.as_bytes());
        self.frame();
    }

    fn key(&mut self, ch: char) {
        self.schedule(KEY_MS);
        self.glow.note_typed_cells(self.now, 1);
        self.trail.note_typed(self.now);
        self.twin.note_typed(self.now);
        match self.composer {
            Composer::Zsh => {
                let mut buf = [0u8; 4];
                self.term.process(ch.encode_utf8(&mut buf).as_bytes());
                self.caret += 1;
                self.frame();
            }
            Composer::Ink => {
                self.text.insert(self.caret, ch);
                self.caret += 1;
                self.ink_redraw();
            }
        }
    }

    fn word_key(&mut self, col: usize) {
        self.nav_after(col, HOP_MS);
    }

    fn nav_after(&mut self, col: usize, ms: u64) {
        self.schedule(ms);
        self.glow.note_motion(self.now);
        self.trail.note_navigation(self.now);
        match self.composer {
            Composer::Zsh => {
                let from = usize::from(self.term.cursor().col);
                assert_eq!(from, self.caret, "zsh: the caret is where the host left it");
                let bytes = if col < from {
                    format!("\x1b[{}D", from - col)
                } else {
                    format!("\x1b[{}C", col - from)
                };
                self.caret = col;
                self.term.process(bytes.as_bytes());
                self.frame();
            }
            Composer::Ink => {
                self.caret = col;
                self.ink_redraw();
            }
        }
        assert_eq!(
            usize::from(self.term.cursor().col),
            col,
            "{:?}: the word key landed the caret",
            self.composer
        );
    }

    fn census(&self) -> Vec<Column> {
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let cw = CW as f32;
        let ch = CH as f32;
        (0..LINE.len() as u16)
            .map(|col| {
                let owner = rib
                    .cells()
                    .iter()
                    .rposition(|c| c.row == ROW && c.col == col && !c.leaving())
                    .map(|i| {
                        let c = rib.cells()[i];
                        (c.cohort, c.born, c.typing, c.cov0.to_bits())
                    });
                let cov = rib
                    .plan_segments()
                    .iter()
                    .filter(|s| {
                        ((s.spine / ch) - 0.5).floor() as i64 == i64::from(ROW)
                            && (s.x / cw).floor() as i64 == i64::from(col)
                    })
                    .map(|s| s.cov)
                    .max()
                    .unwrap_or(0);
                (owner, cov)
            })
            .collect()
    }

    fn retired(&self) -> u64 {
        self.glow.v2_status().map_or(0, |s| s.retired)
    }

    fn ribbon(&self) -> &Ribbon {
        self.glow.v2_ribbon().expect("rainbow kitty owns the frame")
    }

    fn typed_cohort(&self) -> Option<Cohort> {
        self.ribbon().cohorts().iter().find(|k| !k.wake).copied()
    }

    fn columns_of(&self, cohort: u32) -> Vec<usize> {
        let rib = self.ribbon();
        (0..LINE.len() as u16)
            .filter(|&col| {
                rib.cells()
                    .iter()
                    .rposition(|c| {
                        c.row == ROW && c.col == col && c.layer == Layer::Base && !c.leaving()
                    })
                    .is_some_and(|i| rib.cells()[i].cohort == cohort)
            })
            .map(usize::from)
            .collect()
    }

    fn cells_of(&self, cohort: u32) -> Vec<CellPrint> {
        self.ribbon()
            .cells()
            .iter()
            .filter(|c| c.cohort == cohort)
            .map(|c| {
                (
                    c.col,
                    c.born,
                    c.attack_at,
                    c.life_s.to_bits(),
                    c.cov0.to_bits(),
                    c.retract_at,
                    c.retire_at,
                )
            })
            .collect()
    }

    fn census_before(&mut self, ms: u64) -> Vec<Column> {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.census()
    }
}

type CellPrint = (
    u16,
    Instant,
    Instant,
    u32,
    u32,
    Option<Instant>,
    Option<Instant>,
);

fn gaps(before: &[Column], now: &[Column]) -> Vec<(u16, u8)> {
    before
        .iter()
        .zip(now)
        .enumerate()
        .filter(|(_, (b, n))| b != n)
        .map(|(col, (_, n))| (col as u16, n.1))
        .collect()
}

fn words() -> Vec<usize> {
    let mut w = vec![0];
    for (i, ch) in LINE.char_indices() {
        if ch == ' ' && i + 1 < LINE.len() {
            w.push(i + 1);
        }
    }
    w.push(LINE.len());
    w
}

fn scrub(composer: Composer) {
    let mut h = Host::new(composer);
    for ch in LINE.chars() {
        h.key(ch);
    }
    let live: Vec<u16> = (0..LINE.len() as u16)
        .filter(|&col| h.census()[usize::from(col)].0.is_some())
        .collect();
    assert_eq!(
        live,
        (0..LINE.len() as u16).collect::<Vec<_>>(),
        "{composer:?}: the typed band is laid under every cell of the line"
    );
    h.idle_to(h.last_event + Duration::from_millis(HOP_MS - FRAME_MS));
    let before = h.census();
    assert!(
        before
            .iter()
            .all(|(owner, cov)| owner.is_some() && *cov > 100),
        "{composer:?}: the band is whole and hot before the scrub: {before:?}"
    );
    let tally0 = h.glow.admission_tally();
    let retired0 = h.retired();
    let words = words();
    let mut idx = words.len() - 1;
    let mut gap_frames = 0usize;
    let mut worst: Vec<(u16, u8)> = Vec::new();
    let mut first_gap_ms = None;
    for (k, step) in [-1i32, -1, -1, -1, 1, 1, 1, 1, -1, -1]
        .into_iter()
        .enumerate()
    {
        idx = (idx as i32 + step) as usize;
        h.word_key(words[idx]);
        let next = h.last_event + Duration::from_millis(HOP_MS);
        let mut landing = true;
        loop {
            let g = gaps(&before, &h.census());
            if !g.is_empty() {
                gap_frames += 1;
                first_gap_ms.get_or_insert(h.ms());
                if g.len() > worst.len() {
                    worst = g.clone();
                }
            }
            if landing {
                println!(
                    "{composer:?} hop {:>2} → col {:>2} at +{} ms after the last key: {} gap columns {:?}",
                    k + 1,
                    words[idx],
                    (k as u64 + 1) * HOP_MS + composer.spent_ms(),
                    g.len(),
                    g.iter().map(|(c, _)| *c).collect::<Vec<_>>()
                );
                landing = false;
            }
            if h.now + Duration::from_millis(FRAME_MS) > next {
                break;
            }
            h.step();
        }
    }
    let after = h.census();
    let tally = h.glow.admission_tally();
    assert_eq!(
        (
            tally.licensed - tally0.licensed,
            tally.declined - tally0.declined
        ),
        (10, 0),
        "{composer:?}: every word key is licensed ({:?})",
        tally.last_decline_reason
    );
    assert_eq!(
        h.retired() - retired0,
        0,
        "{composer:?}: a repaint that puts the same text back retires nothing (D2)"
    );
    assert!(
        h.trail_disagreements.is_empty(),
        "{composer:?}: the classic trail and its keyless twin drew different bytes on {} frames: {:?}",
        h.trail_disagreements.len(),
        h.trail_disagreements
    );
    assert_eq!(
        gap_frames,
        0,
        "{composer:?}: the scrub damaged the band on {gap_frames} frames (first at +{first_gap_ms:?} ms since t0); the worst frame's {} gap columns: {:?}",
        worst.len(),
        worst
    );
    assert_eq!(
        after, before,
        "{composer:?}: the census after the tenth hop is the census before the first"
    );
    h.idle(700);
    assert!(
        gaps(&before, &h.census()).is_empty(),
        "{composer:?}: 0.7 s after the last hop the band is whole"
    );
    h.idle((SWOOSH_TOTAL_S * 1000.0) as u64);
    assert!(
        h.census()
            .iter()
            .all(|(owner, cov)| owner.is_none() && *cov == 0),
        "{composer:?}: the band is out one swoosh after the last hop"
    );
}

#[test]
fn a_word_scrub_through_zsh_leaves_no_gap_and_both_engines_agree() {
    scrub(Composer::Zsh);
}

#[test]
fn a_word_scrub_through_an_ink_repaint_leaves_no_gap_and_both_engines_agree() {
    scrub(Composer::Ink);
}

#[test]
fn a_scrub_over_holes_a_hop_filled_leaves_an_abandoned_band_alone_at_the_host_seam() {
    let mut h = Host::new(Composer::Zsh);
    for ch in "hello world".chars() {
        h.key(ch);
    }
    let band = h.typed_cohort().expect("the typed band");
    h.nav_after(31, 200);
    h.nav_after(0, 350);
    let a_before = *h
        .ribbon()
        .cohorts()
        .iter()
        .find(|k| k.id == band.id)
        .expect("A is resident until its swoosh ends");
    assert!(
        a_before.abandoned && a_before.phase.is_retracting(),
        "the setup: A is abandoned and inside its swoosh ({:?})",
        a_before.phase
    );
    h.census_before(200);
    let a_cols = h.columns_of(band.id);
    assert!(
        !a_cols.is_empty(),
        "the setup: A still owns cells on its own layer: {a_cols:?}"
    );
    assert!(
        (0..2u16).all(|col| {
            let rib = h.ribbon();
            rib.cells()
                .iter()
                .rposition(|c| c.row == ROW && c.col == col && !c.leaving())
                .is_some_and(|i| rib.cells()[i].cohort != band.id)
        }),
        "the setup: the corridor's columns are owned by the wake, not by A"
    );
    let a_cells_pre = h.cells_of(band.id);
    h.nav_after(2, 200);
    let a_after = *h
        .ribbon()
        .cohorts()
        .iter()
        .find(|k| k.id == band.id)
        .expect("A is still resident on the scrub frame");
    assert!(
        a_after.abandoned
            && a_after.alive_at == a_before.alive_at
            && a_after.retract_col == a_before.retract_col
            && a_after.phase.is_retracting(),
        "the scrub left A exactly as it was, still leaving: {a_before:?} → {a_after:?}"
    );
    assert_eq!(
        h.cells_of(band.id),
        a_cells_pre,
        "the scrub re-priced or re-clocked a cell of the band it did not read"
    );
    let w2 = h
        .ribbon()
        .cohorts()
        .iter()
        .filter(|k| k.wake && !k.abandoned)
        .max_by_key(|k| k.alive_at)
        .copied()
        .expect("W2 owns the corridor");
    assert_eq!(
        w2.alive_at, h.now,
        "the scrub holds W2, which owns the corridor"
    );
    h.idle(200);
    assert!(
        h.ribbon().cohorts().iter().all(|k| k.id != band.id),
        "A is gone when its own swoosh ends — the scrub did not renew it"
    );
}

#[test]
fn a_hop_back_over_a_melting_stub_raises_no_cell_s_light_at_the_host_seam() {
    let mut h = Host::new(Composer::Zsh);
    for ch in "the quick".chars() {
        h.key(ch);
    }
    let band = h.typed_cohort().expect("the typed band");
    h.nav_after(15, 150);
    let stub = h
        .ribbon()
        .cohorts()
        .iter()
        .find(|k| k.wake)
        .copied()
        .expect("the hop lays its cold stub");
    let cov_pre = h.census_before(500);
    assert!(
        (10..15).all(|col| cov_pre[col].1 > 0 && cov_pre[col].1 < 200),
        "the setup: the stub is lit and inside its melt: {:?}",
        cov_pre.iter().map(|k| k.1).collect::<Vec<_>>()
    );
    h.nav_after(10, 500);
    let cov_now = h.census();
    for col in 0..LINE.len() {
        assert!(
            cov_now[col].1 <= cov_pre[col].1,
            "col {col}: the move over a melting stub raised its light ({} → {})",
            cov_pre[col].1,
            cov_now[col].1
        );
    }
    let clocks = |h: &Host, id: u32| {
        h.ribbon()
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .map(|k| k.alive_at)
    };
    assert_eq!(
        clocks(&h, stub.id),
        Some(stub.alive_at),
        "a melting stub's clock is not renewed"
    );
    assert_eq!(
        clocks(&h, band.id),
        Some(band.alive_at),
        "the band beside the corridor keeps its own clock"
    );
    h.idle(100);
    let cov_out = h.census();
    assert!(
        (10..16).all(|col| cov_out[col].1 == 0),
        "the stub finished melting under the hand: {:?}",
        cov_out.iter().map(|k| k.1).collect::<Vec<_>>()
    );
}

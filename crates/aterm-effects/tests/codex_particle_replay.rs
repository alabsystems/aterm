// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE BAND UNDER CODEX'S COMPOSER IS ONE RUN, ON EVERY FRAME** (2026-09-16
//! — the owner, on the shipped 0.86.0: *"there is still this gapping issue
//! that arises in codex, it seems to happen when doing backword and editing"*).
//!
//! Codex 0.154.0 (ratatui, per-cell diff) paints an ambient BRAILLE PARTICLE
//! field into the blank cells of its composer on every ~150 ms frame — `⠁ ⠄ ⠈
//! ⢀`, dim grey, inside a `?2026` synchronized-update bracket. The particles
//! drift, so a SPACE the user typed flips `' '` → `'⠈'` → `' '` while the words
//! either side of it stand unchanged; the caret parking on such a cell (an
//! Alt+Right word hop lands on the space after the word) clears the particle to
//! a plain space and the next frame paints it back. A typed space can even be
//! ECHOED as a particle, because Codex draws the particle in the cell the
//! moment it is blank.
//!
//! This file replays Codex's REAL bytes — `fixtures/codex-particles-2026-09-16.ptylog`,
//! recorded under a PTY wrapper inside a headless aterm instance while the
//! owner's gesture was driven through the keyboard seam (type a line, Alt+Left
//! ×3, type a word, Alt+Right ×3, type two letters) — through a real
//! [`Terminal`], sampled exactly as `app_render.rs`'s LOCK A samples it, fed to
//! [`CursorGlow`] with the hints the app stamps for those keys, and censuses the
//! PLAN's coverage on the composer row every 8 ms.
//!
//! Measured on glass before this, same fixture, same build: the band read
//! `#########.#.#######` for over a second — persistent one-cell holes EXACTLY
//! on the two spaces the hops had parked the caret on — and during plain typing
//! the band fell from 33 lit cells to 9 within 600 ms where zsh keeps 38.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

/// The take's grid and cell: `resize 32 100` in a headless instance, 7×14 px.
const ROWS: usize = 32;
const COLS: usize = 100;
const CW: usize = 7;
const CH: usize = 14;
/// The frame train.
const FRAME_MS: u64 = 8;
/// A cell counts as LIT at this planned coverage — the pixel census on glass
/// calls a cell lit at 20% of the bed.
const LIT_COV: u8 = 20;
/// Replay stops before the take's exit keys (Ctrl+C ×2, then Ctrl+D).
const END_MS: u64 = 9_500;

const FIXTURE: &str = include_str!("fixtures/codex-particles-2026-09-16.ptylog");

enum Event {
    /// Bytes the program wrote to the terminal.
    Out(Vec<u8>),
    /// Bytes the keyboard sent to the program.
    In(Vec<u8>),
}

fn unhex(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    (0..b.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

fn events() -> Vec<(u64, Event)> {
    FIXTURE
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(3, ' ');
            let ms: u64 = it.next().unwrap().parse().unwrap();
            let kind = it.next().unwrap();
            let bytes = unhex(it.next().unwrap());
            (
                ms,
                match kind {
                    "O" => Event::Out(bytes),
                    "I" => Event::In(bytes),
                    k => panic!("bad kind {k}"),
                },
            )
        })
        .collect()
}

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

/// The owner's config as the take ran it: rainbow kitty, tall body, the
/// default intensity 1.0, dark.
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
        intensity: 1.0,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
    }
}

/// One frame's census of one row: the lit columns as a map, with the span's
/// interior dark runs, and where the caret stood.
struct RowCensus {
    ms: u64,
    row: u16,
    caret: u16,
    map: String,
    lit: usize,
    holes: Vec<usize>,
    /// Interior gaps in the ribbon's own LIVE cells on the row — a column
    /// between two live cells that no live cell owns. The plan's coverage
    /// feathers across a one-cell gap, so the pixel census on glass sees a
    /// dark column the coverage census does not: this is the ribbon's own
    /// account of it.
    cell_gaps: Vec<usize>,
}

impl RowCensus {
    fn worst(&self) -> usize {
        self.holes.iter().copied().max().unwrap_or(0)
    }
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    scroll: Option<ContentScrollState>,
    /// The composer row, learnt from the caret at the first typed key.
    composer_row: Option<u16>,
    census: Vec<RowCensus>,
}

impl Host {
    fn new() -> Self {
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        Self {
            term: Terminal::new(ROWS as u16, COLS as u16),
            glow,
            cfg: cfg(),
            g: geom(),
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            scroll: None,
            composer_row: None,
            census: Vec::new(),
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// EXACTLY the host's own order: `sync_cursor_effect_scroll`, then LOCK
    /// A's samples, then the tick.
    fn frame(&mut self) {
        let scroll = self.term.content_scroll_state();
        match ContentScrollState::delta_since(self.scroll, scroll) {
            ContentScrollDelta::Baseline | ContentScrollDelta::Unchanged => {}
            ContentScrollDelta::Translate(rows) => {
                self.glow.note_scroll(rows);
                self.glow.drop_row_probe();
            }
            ContentScrollDelta::Bands { first_seq, count } => {
                for i in 0..u64::from(count) {
                    let m = scroll.band(first_seq + i);
                    self.glow.note_band_move(m.top, m.bottom, m.delta);
                }
                self.glow.drop_row_probe();
            }
            ContentScrollDelta::Invalidate => self.glow.curtain(self.now),
        }
        self.scroll = Some(scroll);
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink {
            self.blink = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
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
        self.record(c.row, c.col);
        self.dump_cells();
    }

    /// The frame train up to `ms`. While Codex's `?2026` bracket is open the
    /// host WITHHOLDS the present (SYNC-1, `app_render.rs`), so LOCK A never
    /// samples a torn frame: the train holds with it.
    /// `CODEX_REPLAY_CELLS=<ms-from>:<ms-to>` prints every ribbon cell on the
    /// composer row for the frames in that window: column, layer, cohort,
    /// whether it is leaving, and its cohort's clock.
    fn dump_cells(&self) {
        let Some(row) = self.composer_row else { return };
        let Some(win) = std::env::var("CODEX_REPLAY_CELLS").ok() else {
            return;
        };
        let Some((a, b)) = win.split_once(':') else {
            return;
        };
        let (a, b): (u64, u64) = (a.parse().unwrap_or(0), b.parse().unwrap_or(0));
        let ms = self.ms();
        if ms < a || ms > b {
            return;
        }
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let mut cov = [0u8; COLS];
        for sg in rib.plan_segments() {
            let r = ((sg.spine / CH as f32) - 0.5).floor();
            let col = (sg.x / CW as f32).floor();
            if r >= 0.0 && col >= 0.0 && r as usize == usize::from(row) && (col as usize) < COLS {
                let i = col as usize;
                cov[i] = cov[i].max(sg.cov);
            }
        }
        let mut cells: Vec<_> = rib.cells().iter().filter(|c| c.row == row).collect();
        cells.sort_by_key(|c| c.col);
        let mut s = format!("t={ms:6} cells:");
        for c in cells {
            let coh = rib.cohorts().iter().find(|k| k.id == c.cohort);
            let alive = coh
                .map(|k| k.alive_at.saturating_duration_since(self.t0).as_millis() as i64)
                .unwrap_or(-1);
            let ab = coh.map(|k| k.abandoned).unwrap_or(false);
            s.push_str(&format!(
                " {}:{:?}/c{}{}{}@{}={}b{}{}",
                c.col,
                c.layer,
                c.cohort,
                if c.leaving() { "L" } else { "" },
                if ab { "A" } else { "" },
                alive,
                cov[usize::from(c.col).min(COLS - 1)],
                c.born.saturating_duration_since(self.t0).as_millis(),
                if c.typing { "T" } else { "" }
            ));
        }
        if let Some(last) = self.glow.admission_log().last() {
            s.push_str(&format!("\n   ring: {}", last.line(self.now)));
        }
        // Every cell in the pool born in the last 60 ms, whatever its row.
        let fresh: Vec<String> = rib
            .cells()
            .iter()
            .filter(|c| self.now.saturating_duration_since(c.born).as_millis() < 60)
            .map(|c| {
                format!(
                    "({},{}) b{} {:?}",
                    c.row,
                    c.col,
                    c.born.saturating_duration_since(self.t0).as_millis(),
                    c.layer
                )
            })
            .collect();
        s.push_str(&format!(
            "\n   fresh: {fresh:?} pool={} cohorts={}",
            rib.cells().len(),
            rib.cohorts().len()
        ));
        eprintln!("{s}");
    }

    fn run_to(&mut self, ms: u64) {
        let t = self.t0 + Duration::from_millis(ms);
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.now += Duration::from_millis(FRAME_MS);
            if !self.term.sync_open_dirty() {
                self.frame();
            }
        }
        self.now = t;
    }

    /// The hint the app stamps for what the keyboard sent: a printable byte is
    /// a typed key, Backspace erases one glyph, an arrow (plain or Alt) is a
    /// navigation key. The terminal's replies to Codex's queries and the
    /// take's exit chords stamp nothing.
    fn hint(&mut self, bytes: &[u8]) {
        match bytes {
            [b] if (0x20..0x7f).contains(b) => {
                if self.composer_row.is_none() {
                    self.composer_row = Some(self.term.cursor().row);
                }
                self.glow.note_typed_cells(self.now, 1);
            }
            [0x7f] => self.glow.note_backspace_erasing(self.now, Some(1)),
            b if b.starts_with(b"\x1b[") && matches!(b.last(), Some(b'A' | b'B' | b'C' | b'D')) => {
                self.glow.note_motion(self.now);
            }
            _ => {}
        }
    }

    /// This frame's census of the PLAN's coverage on the composer row.
    fn record(&mut self, caret_row: u16, caret_col: u16) {
        let Some(row) = self.composer_row else { return };
        let mut cov = [0u8; COLS];
        {
            let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
            for sg in rib.plan_segments() {
                let r = ((sg.spine / CH as f32) - 0.5).floor();
                let col = (sg.x / CW as f32).floor();
                if r < 0.0 || col < 0.0 {
                    continue;
                }
                if r as usize == usize::from(row) && (col as usize) < COLS {
                    let i = col as usize;
                    cov[i] = cov[i].max(sg.cov);
                }
            }
        }
        let lit: Vec<usize> = (0..COLS).filter(|&i| cov[i] >= LIT_COV).collect();
        if lit.len() < 2 {
            return;
        }
        let mut holes = Vec::new();
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
        let caret = if caret_row == row {
            caret_col
        } else {
            u16::MAX
        };
        let mut live: Vec<u16> = self
            .glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cells()
            .iter()
            .filter(|c| c.row == row && !c.leaving())
            .map(|c| c.col)
            .collect();
        live.sort_unstable();
        live.dedup();
        let cell_gaps: Vec<usize> = live
            .windows(2)
            .filter(|w| w[1] > w[0] + 1)
            .map(|w| usize::from(w[1] - w[0] - 1))
            .collect();
        self.census.push(RowCensus {
            ms: self.ms(),
            row,
            caret,
            cell_gaps,
            map: cov
                .iter()
                .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                .collect(),
            lit: lit.len(),
            holes,
        });
    }

    fn broken(&self) -> Vec<&RowCensus> {
        self.census.iter().filter(|c| !c.holes.is_empty()).collect()
    }

    fn report(&self, what: &str) -> String {
        let broken = self.broken();
        let worst = broken.iter().map(|c| c.worst()).max().unwrap_or(0);
        let mut s = format!(
            "{what}: {} of {} row-frames carry an interior dark run, worst {worst} cells",
            broken.len(),
            self.census.len()
        );
        for c in broken.iter().take(12) {
            s.push_str(&format!(
                "\n  t={:6} r={:2} caret={:3} lit={:2} {}",
                c.ms, c.row, c.caret, c.lit, c.map
            ));
        }
        s
    }
}

fn replay() -> Host {
    let mut h = Host::new();
    let dump = std::env::var_os("CODEX_REPLAY_DUMP");
    for (ms, ev) in events() {
        if ms > END_MS {
            break;
        }
        h.run_to(ms);
        match ev {
            Event::Out(bytes) => {
                h.term.process(&bytes);
                if !h.term.sync_open_dirty() {
                    h.frame();
                }
            }
            Event::In(bytes) => h.hint(&bytes),
        }
    }
    h.run_to(END_MS);
    if let Some(path) = dump {
        let mut s = String::new();
        for c in &h.census {
            s.push_str(&format!(
                "t={:6} r={:2} caret={:3} lit={:2} holes={:?} {}\n",
                c.ms, c.row, c.caret, c.lit, c.holes, c.map
            ));
        }
        std::fs::write(path, s).expect("dump");
    }
    h
}

/// **THE OWNER'S OWN GESTURE, CODEX'S OWN BYTES.** Type a line, three word hops
/// back, a word typed inside it, three word hops forward, two letters at the
/// end. RED before the fix: the two spaces the forward hops parked the caret on
/// stay dark for over a second while the words either side of them are lit.
#[test]
fn a_backward_edit_in_codex_never_punches_a_hole_in_the_band() {
    let h = replay();
    assert!(
        h.census.iter().any(|c| c.lit >= 9),
        "the take never lit a band of 9 cells: {}",
        h.report("codex-g1")
    );
    assert!(h.broken().is_empty(), "{}", h.report("codex-g1"));
}

/// **A SPACE ECHOED AS A PARTICLE IS STILL THE USER'S SPACE.** While the line
/// is being typed, no navigation has happened and nothing was erased, so the
/// band has no reason to leave the row: the light under the first word must
/// still be standing when the last word is typed, as it is in zsh. RED before
/// the fix: the band fell from 33 cells to 9 in 600 ms of plain typing.
#[test]
fn plain_typing_under_codex_keeps_the_whole_line_lit() {
    let h = replay();
    // The line's last key lands at 5432 ms in the fixture; the first at 2356.
    let at_end: Vec<&RowCensus> = h
        .census
        .iter()
        .filter(|c| (5400..5500).contains(&c.ms))
        .collect();
    assert!(
        !at_end.is_empty(),
        "no census frames at the end of the line"
    );
    let widest = at_end.iter().map(|c| c.lit).max().unwrap_or(0);
    assert!(
        widest >= 30,
        "at the end of a 38-key line only {widest} cells were lit: {}",
        at_end
            .iter()
            .take(3)
            .map(|c| format!("\n  t={:6} lit={:2} {}", c.ms, c.lit, c.map))
            .collect::<String>()
    );
}

/// **THE HAND'S OWN CELLS ARE ONE INTERVAL WHILE IT TYPES.** The plan feathers
/// its coverage across a missing column, so the two tests above did not see
/// what the pixel census on glass saw: the first key of the mid-line insert —
/// typed over a lit cell the hops had brought the caret back onto — laid no
/// cell, and its column stood dark for the rest of the line. RED before
/// `Ribbon::sweep` re-laid a cell older than the key: every frame from the
/// insert to the last key carried a one-cell gap in the ribbon's live cells
/// at the insert column.
#[test]
fn a_key_typed_over_the_band_gets_its_own_cell() {
    let h = replay();
    // From the insert's first echo (6289 ms) to the last key of the take,
    // while the hand is on the row: the suffix's melting cells are
    // `leaving` and excluded, so the live set is the band under the hand.
    let broken: Vec<&RowCensus> = h
        .census
        .iter()
        .filter(|c| (6400..7900).contains(&c.ms) && !c.cell_gaps.is_empty())
        .collect();
    assert!(
        broken.is_empty(),
        "{} frames with a gap in the ribbon's live cells under the hand, first: t={} gaps={:?}",
        broken.len(),
        broken.first().map_or(0, |c| c.ms),
        broken.first().map_or(&Vec::new(), |c| &c.cell_gaps)
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SCRUBBING NEVER DAMAGES THE BAND (2026-09-14) — the owner, on the shipped
//! v0.85.0: *"the painting system for scrubbing back and forth with back
//! word and forward word doesn't work correctly and leaves gaps."*
//!
//! Reproduced at the HOST seam — a real `aterm_core::terminal::Terminal`
//! driven byte for byte, its rows sampled exactly as `app_render.rs`'s LOCK A
//! samples them, fed to `CursorGlow` and ticked through `CursorGlow::tick`
//! on a 16 ms frame train — on a 30 × 56 grid: the owner's line typed at
//! 12 cps, then Option+Left ×4, Option+Right ×4, Option+Left ×2 at 150 ms,
//! through two composers:
//!
//! * **zsh** — a word key moves the caret with one `CUB`/`CUF`, visibly, in
//!   one frame;
//! * **an Ink-shaped composer** (`docs/design/repro/inkish.py`) — every key
//!   and every hop redraws the whole line inside a DECTCEM bracket, split so
//!   a present lands while the caret is hidden; the hop reaches the engine
//!   through the nav hide-bridge, and the witness sees the same text put
//!   back.
//!
//! The band's cells are CENSUSED on every frame: the identity of the cell
//! that owns each column and the coverage the plan puts inside it. A gap is
//! a column under the line whose light is gone or is another cell's. Before
//! the fix the census showed the swoosh running on the last KEY's clock under
//! the scrubbing hand — the band mid-retract from the seventh hop, its
//! boundaries pulled off their columns and cold stubs laid in the holes.
//! After: 0 gap columns on every frame, and the census after the tenth hop
//! byte-identical to the census before the first.
//!
//! The CLASSIC TRAIL is driven through the identical frames beside the
//! kitty, against a twin that was never told a key was pressed: a scrub
//! mints nothing and clears nothing there either (`CursorTrail::spawn`'s nav
//! arm), so its frames are byte-identical to the twin's. Both engines agree.

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
/// The owner's line — 44 cells.
const LINE: &str = "the quick brown fox jumps over the lazy dogs";
/// The row the line is typed on (Ink draws it on its third row).
const ROW: u16 = 2;
/// 12 cps.
const KEY_MS: u64 = 83;
/// The scrub's cadence.
const HOP_MS: u64 = 150;
/// The frame train.
const FRAME_MS: u64 = 16;
/// `inkish.py`'s split between the redraw's head and its tail.
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

/// The owner's config: rainbow kitty, intensity 0.70, dark.
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
        pack: None,
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

/// Which program echoes the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Composer {
    Zsh,
    Ink,
}

impl Composer {
    /// How long a redraw holds the clock past the key: Ink's split.
    fn spent_ms(self) -> u64 {
        match self {
            Self::Zsh => 0,
            Self::Ink => INK_SPLIT_MS,
        }
    }
}

/// One column's census: `(owner identity, planned coverage)` — the owner as
/// `(cohort, born, typing, cov0 bits)`, `None` where no live cell owns the
/// column.
type Column = (Option<(u32, Instant, bool, u32)>, u8);

/// The host: one terminal, the kitty, the classic trail and its twin, one
/// clock.
struct Host {
    term: Terminal,
    glow: CursorGlow,
    trail: CursorTrail,
    /// The classic trail's control: the same frames, never told a key was
    /// pressed. A scrub mints nothing and clears nothing, so the two must
    /// draw the same bytes.
    twin: CursorTrail,
    cfg: GlowConfig,
    tcfg: TrailConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    /// The clock of the last key or hop — the next one is scheduled from it.
    last_event: Instant,
    out: Vec<GlowQuad>,
    tout: Vec<TrailCell>,
    twin_out: Vec<TrailCell>,
    row_buf: Vec<char>,
    blink_seen: u64,
    composer: Composer,
    /// The Ink composer's line so far, and its caret index into it.
    text: String,
    caret: usize,
    /// Every frame on which the trail and its twin disagreed, as `(ms since
    /// t0, cells, twin cells)`.
    trail_disagreements: Vec<(u64, usize, usize)>,
}

impl Host {
    fn new(composer: Composer) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        let mut trail = CursorTrail::default();
        let mut twin = CursorTrail::default();
        // The single full-window pane, as the host names it.
        glow.note_pane_columns(0, COLS);
        trail.note_pane_columns(0, COLS);
        twin.note_pane_columns(0, COLS);
        match composer {
            Composer::Zsh => term.process(format!("\x1b[{};1H", ROW + 1).as_bytes()),
            // `inkish.py`'s entry: the alt screen, cleared, the caret on the
            // line's row.
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

    /// EXACTLY LOCK A, then the tick: sample the cursor, the repaint blink,
    /// the caret row's probe and the rows the resident ribbon occupies —
    /// all from the terminal AFTER the last `process` — then advance every
    /// engine one frame.
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

    /// One frame of the train, 16 ms on.
    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        self.frame();
    }

    /// Frames at 16 ms while a whole frame still fits before `t`.
    fn idle_to(&mut self, t: Instant) {
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.step();
        }
    }

    /// Frames at 16 ms for `ms`.
    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.idle_to(t);
    }

    /// The next event's instant, `ms` after the last, with the frame train
    /// run up to it.
    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = t;
        self.last_event = t;
    }

    /// The Ink composer's redraw of its line with the caret at `self.caret`:
    /// the head (hide, home the row, clear it, the line) on one present, the
    /// tail (place the caret, show it) `INK_SPLIT_MS` later on the next.
    fn ink_redraw(&mut self) {
        let head = format!("\x1b[?25l\x1b[{};1H\x1b[2K{}", ROW + 1, self.text);
        self.term.process(head.as_bytes());
        self.frame();
        self.now += Duration::from_millis(INK_SPLIT_MS);
        let tail = format!("\x1b[{};{}H\x1b[?25h", ROW + 1, self.caret + 1);
        self.term.process(tail.as_bytes());
        self.frame();
    }

    /// One typed key, `KEY_MS` after the last event: the app_input seam arms
    /// the typed hint on every engine, the program echoes it, the frames
    /// present.
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

    /// One word key (Option+Left / Option+Right), `HOP_MS` after the last
    /// event: the seam arms the nav hint on the kitty and the trail — never
    /// on the twin — and the program moves the caret to `col`.
    fn word_key(&mut self, col: usize) {
        self.nav_after(col, HOP_MS);
    }

    /// One navigation key landing the caret at `col`, `ms` after the last
    /// event — `word_key` at any spacing.
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

    /// The census of the line's 44 columns on this frame.
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

    /// The typed band's cohort — the one cohort that is not a wake.
    fn typed_cohort(&self) -> Option<Cohort> {
        self.ribbon().cohorts().iter().find(|k| !k.wake).copied()
    }

    /// The columns of the line whose newest live cell is `cohort`'s.
    /// The columns whose newest un-leaving cell ON THE BAND'S OWN LAYER
    /// belongs to `cohort`. Read on `Base` because a jump's corridor is an
    /// overlay (Rainbow Path v3 §2.3): it lies OVER the band's cells without
    /// touching them, so the newest cell at a covered column is the
    /// corridor's and the band still owns its own.
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

    /// Everything about a cohort's own cells that its light is a function of
    /// — the clock, the price, the stamps — so "the scrub left them exactly
    /// as they were" is a byte comparison and not a coverage threshold a
    /// corridor above them could satisfy by accident.
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

    /// The frame train up to the frame BEFORE an event `ms` after the last,
    /// and the census on it.
    fn census_before(&mut self, ms: u64) -> Vec<Column> {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.census()
    }
}

/// Everything about one cell that its light is a function of, beside the
/// cohort clock — the column, the two birth stamps, the life, the price and
/// the two stamps that end it. Two of these equal is two cells the engine
/// will draw identically.
type CellPrint = (
    u16,
    Instant,
    Instant,
    u32,
    u32,
    Option<Instant>,
    Option<Instant>,
);

/// The columns of `now` that are not `before`'s — the gaps — as
/// `(col, cov now)`.
fn gaps(before: &[Column], now: &[Column]) -> Vec<(u16, u8)> {
    before
        .iter()
        .zip(now)
        .enumerate()
        .filter(|(_, (b, n))| b != n)
        .map(|(col, (_, n))| (col as u16, n.1))
        .collect()
}

/// The word starts of the line, `inkish.py`'s `WORDS`.
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

/// The scrub, censused on every frame.
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
    // The frame train to the first hop, and the census on its last frame.
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
    // Option+Left ×4, Option+Right ×4, Option+Left ×2 — over the word
    // starts, from the end of the line.
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
        // The census on the hop's landing frame and on every frame of the
        // train to the next hop.
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
    // The band leaves one swoosh after the hand's LAST hop — held by the
    // scrub, not by the last key.
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

/// (i) zsh: the caret moves visibly in one frame.
#[test]
fn a_word_scrub_through_zsh_leaves_no_gap_and_both_engines_agree() {
    scrub(Composer::Zsh);
}

/// (ii) the Ink-shaped composer: every hop is a hidden repaint the nav
/// bridge carries, and the witness sees the same text put back.
#[test]
fn a_word_scrub_through_an_ink_repaint_leaves_no_gap_and_both_engines_agree() {
    scrub(Composer::Ink);
}

/// **THE SCRUB HOLDS ONLY WHAT OWNS ITS CORRIDOR** (2026-09-14, the fix-up's
/// first guard at the host seam — the reviewer's counter-example through
/// zsh). `hello world`; +200 ms `CUF 20` (11 → 31: a jump, the wake W1 laid,
/// the band A abandoned); +350 ms `CUB 31` (31 → 0: W2 takes W1 over and
/// fills the holes A's drain has emptied at its far end, A's drained cells
/// staying resident under them); +200 ms `CUF 2` (0 → 2 over W2's live
/// cells: a scrub). A keeps its clock, its abandon and its retract target,
/// every one of A's own cells is byte-identical across the scrub, and A is
/// pruned when ITS swoosh ends. RED before: A went Fading → Laying on the
/// hop's clock and its cells came back from `0,0,0,0,0,0,0,111` to
/// `153…158` in one frame.
///
/// RESTATED 2026-09-14 at the Rainbow Path v3 merge, for the READING only.
/// The corridor is an overlay now (§2.3), so W2 lies over ALL of A rather
/// than taking over the cells A's drain emptied: A's remaining cells are no
/// longer "the columns beyond the corridor" — they are every column, one
/// layer down. The shadowing the guard is about is unchanged and is
/// asserted directly: the corridor's columns resolve to W2 the layer-blind
/// way `Ribbon::scrub` reads an owner, so the hold reads W2 and must leave
/// A exactly as it found it.
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
    // …and the corridor's own columns resolve, the layer-blind way
    // `Ribbon::scrub` reads an owner, to the WAKE and not to A: that is
    // what makes A "shadowed under the corridor" and what the hold must
    // therefore not reach.
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
    // A's own cells are BYTE-IDENTICAL across the scrub. (Before the
    // overlay this was read as coverage per column, which the corridor
    // above A now owns; A's light is a function of these fields and the
    // cohort clock asserted above, and neither moved.)
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

/// **A SCRUB NEVER RAISES A CELL'S LIGHT** (2026-09-14, the fix-up's second
/// guard at the host seam — the reviewer's counter-example through zsh).
/// `the quick`; +150 ms `CUF 6` (9 → 15: a hop, its 0.55 s cold stub at
/// 9..15); +600 ms `CUB 5` (15 → 10) back over the stub, 0.5 s into its
/// life and deep inside its melt. No column is brighter on the landing
/// frame than on the frame before, the stub's cohort keeps its clock, the
/// band beside the corridor keeps its own, and the stub is out within the
/// tenth of a second it had left. RED before: the stub snapped from ~21 to
/// ~157 levels in one frame.
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
    // 2026-09-14, the v3 merge: the corridor is born ON THE FLIGHT CLOCK
    // (cell `j` of `n` at `T·j/n`, `T` = the 60 ms flight floor for a hop)
    // where it used to be born one flat 100 ms lag after the key, so every
    // stub cell is ~0.1 s older at a given instant and the stub is OUT by
    // +600 ms. The probe moves with the births, not with the law: at +500 ms
    // every cell of cols 10..15 is past its melt opening and still lit.
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

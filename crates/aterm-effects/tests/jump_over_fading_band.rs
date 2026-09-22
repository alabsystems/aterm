// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A MOVE OVER A FADING BAND LEAVES NO HOLE IN THE LINE** (2026-09-22).
//! The owner, on the shipped 0.89, rainbow kitty under a Claude Code
//! composer: *"There is a rainbow trail gap in 0.89. I'm not sure what is
//! causing it but it seems like some kind of back cursor movement bug."*
//! The torn read (`torn_read_gaps.rs`) was most of it. These are the two
//! back-cursor-movement holes the reproducers found beside it, both in the
//! corridor `Ribbon::wake` lays behind a moving caret, and both opened only
//! when the band under the move is already on its way out:
//!
//! * **HOME / END OVER A FADING BAND** (the takeover arm). A Home inside the
//!   band's swoosh laid [`WAKE_MAX_CELLS`] of new light at the landing,
//!   covered the band's still-lit remnant by the origin, and left the
//!   stretch the drain had already emptied between them dark: 3 to 65 cells
//!   as the drain advanced, 31 at 1.5 s here. Once the band was gone
//!   (Home at 2.2 s or 4 s), the End back re-owned the Home's island
//!   (K4), laid the cap at its own landing, and skipped the 45 columns
//!   between. Either way the next keys joined the wake cohort and held both
//!   islands until the swoosh after the last key, about 1.7 s. The cap now
//!   binds at the corridor's FAR EDGE: light the corridor keeps past it
//!   moves that edge out, and the drained columns between are laid (v3
//!   §2.10, K2).
//! * **A WORD HOP AT THE END OF THE SWOOSH** (the hop arm). ⌥← ×2 (a scrub,
//!   the band held), a pause, then ⌥→ landing in the last ~20 ms of the
//!   band's fade: the scrub refused the move (the band was leaving) and the
//!   hop left every corridor cell with ANY light alone, which was five of
//!   six cells, all carrying an invisible residue. The retire one frame
//!   later left them owned by nobody. The result was the owner's picture:
//!   `IN` lit, `TO ` dark, `MAIN!!!!` held by the hand. Light whose band is
//!   leaving is now covered by the hop from the level it has, as the jump
//!   covers it. A visibility floor was measured and REFUTED as the fix: at
//!   20/255 the same `TO ` hole opened at a pause of 1412–1420 ms instead
//!   of 1481–1491, because the drain empties neighbouring columns ~3.7 ms
//!   apart and any floor splits the corridor at some instant.
//!
//! The host seam, exactly as `scrub_gaps.rs` drives it: a real
//! `aterm_core::terminal::Terminal` driven byte for byte, its rows sampled as
//! `app_render.rs`'s LOCK A samples them, fed to `CursorGlow`, ticked on a
//! 16 ms frame train. The owner's own line is typed at 12 cps into an
//! Ink-shaped composer (whole-line redraw inside a DECTCEM bracket, split so
//! a present lands while the caret is hidden) and into a Claude-Code-shaped
//! one (the composer bottom-pinned, a spinner row above it repainting every
//! 100 ms on its own clock and inside every redraw). Every frame censuses the
//! PLAN over the typed row, `#` at ≥ 20 coverage and `.` dark, and a HOLE is
//! a dark run strictly between two lit cells of that row. The laws read here
//! allow none, on any frame from the move to the end of the take.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::ribbon::{Layer, WAKE_MAX_CELLS};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

const COLS: usize = 140;
const CW: usize = 8;
const CH: usize = 16;
/// The owner's line up to the `!!!!` typed after the move: 108 cells, `INTO`
/// at 99, `TO ` at 101..104, `MAIN` at 104..108.
const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN";
const TAIL: &str = "!!!!";
/// 12 cps.
const KEY_MS: u64 = 83;
/// The scrub's cadence.
const HOP_MS: u64 = 150;
/// The frame train.
const FRAME_MS: u64 = 16;
/// The split between a redraw's head (caret hidden) and its tail.
const INK_SPLIT_MS: u64 = 30;
/// A cell counts as LIT at this planned coverage (`ribscan2.py`'s census).
const LIT_COV: u8 = 20;
/// The spinner's cadence in the Claude-shaped composer.
const SPINNER_MS: u64 = 100;
/// The window the owner's `TO ` hole opened in: the first ⌥→ this long
/// after the scrub's last hop lands inside the last ~20 ms of the band's
/// 1.69 s swoosh (grace 0.90 s at 12 cps + 0.15 reach + 0.40 retract + 0.24
/// fade), the replay frame 30 ms after the head.
const FADE_END_PAUSES_MS: [u64; 6] = [1481, 1483, 1485, 1487, 1489, 1491];

/// Which program echoes the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Composer {
    /// `inkish.py`: 30 rows, the line on row 3, prefix-less.
    Ink,
    /// Claude Code's shape: 60 rows, the composer bottom-pinned with a `> `
    /// prompt, a spinner row three rows above it.
    Claude,
}

impl Composer {
    fn rows(self) -> usize {
        match self {
            Self::Ink => 30,
            Self::Claude => 60,
        }
    }
    /// The 0-based row the line is drawn on.
    fn row(self) -> u16 {
        match self {
            Self::Ink => 2,
            Self::Claude => 57,
        }
    }
    /// The column the line's first glyph is drawn at.
    fn col0(self) -> u16 {
        match self {
            Self::Ink => 0,
            Self::Claude => 2,
        }
    }
    fn spinner_row(self) -> u16 {
        match self {
            Self::Ink => 0,
            Self::Claude => 54,
        }
    }
}

/// Which way the word keys move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordKeys {
    /// `inkish.py`: ⌥→ to the NEXT word's start (the end past the last word).
    Inkish,
    /// readline / Claude Code: ⌥→ to the end of the current-or-next word.
    Readline,
}

fn geom(c: Composer) -> Geom {
    Geom {
        cw: CW,
        ch: CH,
        rows: c.rows(),
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * CW) as u16,
        win_h: (c.rows() * CH) as u16,
        head: 0,
    }
}

/// The owner's config: rainbow kitty, tall body, intensity 0.70, dark.
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

/// One frame's census of the typed row.
struct RowFrame {
    ms: u64,
    /// What the host was doing when this frame presented.
    label: String,
    /// `#`/`.` per column of the line (`col0`-relative).
    map: String,
    /// Dark runs strictly inside the lit span, `(first col, width)`.
    holes: Vec<(usize, usize)>,
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
    composer: Composer,
    words: WordKeys,
    text: String,
    caret: usize,
    label: String,
    spinner_n: u32,
    next_spinner: Instant,
    frames: Vec<RowFrame>,
    /// Every same-row move's replay frame and span, `(frame, from, to)` in
    /// line columns, with the columns of its span no `Over` cell covers.
    moves: Vec<(usize, usize, usize, Vec<usize>)>,
}

impl Host {
    fn new(composer: Composer, words: WordKeys) -> Self {
        let mut term = Terminal::new(composer.rows() as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        term.process(b"\x1b[?1049h\x1b[2J");
        term.process(format!("\x1b[{};{}H", composer.row() + 1, composer.col0() + 1).as_bytes());
        let mut h = Self {
            term,
            glow,
            cfg: cfg(),
            g: geom(composer),
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            composer,
            words,
            text: String::new(),
            caret: 0,
            label: "start".into(),
            spinner_n: 0,
            next_spinner: now + Duration::from_millis(SPINNER_MS),
            frames: Vec::new(),
            moves: Vec::new(),
        };
        if composer == Composer::Claude {
            // The static chrome: the box's rules above and below the line and
            // the status row under them.
            let r = composer.row();
            let rule: String = "─".repeat(COLS);
            let s = format!(
                "\x1b[?25l\x1b[{r};1H{rule}\x1b[{};1H{rule}\x1b[{};1H  ? for shortcuts\x1b[{};{}H\x1b[?25h",
                r + 2,
                r + 3,
                r + 1,
                composer.col0() + 1
            );
            h.term.process(s.as_bytes());
        }
        h.frame();
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// EXACTLY LOCK A, then the tick.
    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
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
        self.record();
    }

    /// One frame of the train, 16 ms on, with the spinner repainting on its
    /// own clock in the Claude-shaped composer.
    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        if self.composer == Composer::Claude && self.now >= self.next_spinner {
            self.next_spinner += Duration::from_millis(SPINNER_MS);
            self.spinner_n += 1;
            let s = format!(
                "\x1b[?25l\x1b[{};1H\x1b[2K{}\x1b[{};{}H\x1b[?25h",
                self.composer.spinner_row() + 1,
                self.spinner(),
                self.composer.row() + 1,
                usize::from(self.composer.col0()) + self.caret + 1
            );
            self.term.process(s.as_bytes());
        }
        self.frame();
    }

    fn spinner(&self) -> String {
        let glyph = ['✻', '✽', '✶', '✳', '✢', '·'][(self.spinner_n as usize) % 6];
        format!("{glyph} Clauding… ({}s · ↓ 2.3k tokens)", self.ms() / 1000)
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

    /// The next event EXACTLY `ms` after the last one (not quantized to the
    /// train: the move's instant against the band's fade is the point).
    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = t;
        self.last_event = t;
    }

    /// The composer's redraw of its line with the caret at `self.caret`: the
    /// head (hide, the spinner row on the Claude shape, clear the row, the
    /// line) on one present, the tail (place the caret, show it)
    /// `INK_SPLIT_MS` later on the next.
    fn redraw(&mut self) {
        let r = self.composer.row() + 1;
        let c0 = usize::from(self.composer.col0());
        let prefix = if self.composer == Composer::Claude {
            "> "
        } else {
            ""
        };
        let mut head = String::from("\x1b[?25l");
        if self.composer == Composer::Claude {
            head.push_str(&format!(
                "\x1b[{};1H\x1b[2K{}",
                self.composer.spinner_row() + 1,
                self.spinner()
            ));
        }
        head.push_str(&format!("\x1b[{r};1H\x1b[2K{prefix}{}", self.text));
        self.term.process(head.as_bytes());
        self.frame();
        self.now += Duration::from_millis(INK_SPLIT_MS);
        let tail = format!("\x1b[{r};{}H\x1b[?25h", c0 + self.caret + 1);
        self.term.process(tail.as_bytes());
        self.frame();
    }

    fn key_after(&mut self, ch: char, ms: u64) {
        self.schedule(ms);
        self.label = format!("key '{ch}'");
        self.glow.note_typed_cells(self.now, 1);
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.redraw();
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key_after(ch, KEY_MS);
        }
    }

    /// One navigation key landing the caret at `col`, `ms` after the last
    /// event. The move is replayed on the redraw's tail frame; the columns of
    /// its span that no `Over` cell covers right then are recorded.
    fn nav_after(&mut self, what: &str, col: usize, ms: u64) {
        self.schedule(ms);
        self.label = format!("{what} -> {col}");
        self.glow.note_motion(self.now);
        let from = self.caret;
        self.caret = col;
        self.redraw();
        assert_eq!(
            usize::from(self.term.cursor().col),
            usize::from(self.composer.col0()) + col,
            "the nav key landed the caret"
        );
        let row = self.composer.row();
        let c0 = usize::from(self.composer.col0());
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let uncovered: Vec<usize> = (from.min(col)..=from.max(col))
            .filter(|&i| {
                !rib.cells()
                    .iter()
                    .any(|c| c.row == row && usize::from(c.col) == c0 + i && c.layer == Layer::Over)
            })
            .collect();
        self.moves
            .push((self.frames.len() - 1, from, col, uncovered));
    }

    fn word_left(&mut self) {
        let b = self.text.as_bytes();
        let mut i = self.caret;
        while i > 0 && b[i - 1] == b' ' {
            i -= 1;
        }
        while i > 0 && b[i - 1] != b' ' {
            i -= 1;
        }
        self.nav_after("opt-left", i, HOP_MS);
    }

    fn word_right(&mut self) {
        let b = self.text.as_bytes();
        let n = b.len();
        let mut i = self.caret;
        let col = match self.words {
            WordKeys::Inkish => (i + 1..n)
                .find(|&j| b[j - 1] == b' ' && b[j] != b' ')
                .unwrap_or(n),
            WordKeys::Readline => {
                while i < n && b[i] == b' ' {
                    i += 1;
                }
                while i < n && b[i] != b' ' {
                    i += 1;
                }
                i
            }
        };
        self.nav_after("opt-right", col, HOP_MS);
    }

    /// The census of the typed row on this frame.
    fn record(&mut self) {
        let row = self.composer.row();
        let c0 = usize::from(self.composer.col0());
        let n = self.text.len().max(LINE.len() + TAIL.len());
        let mut cov = [0u8; COLS];
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        for sg in rib.plan_segments() {
            let r = ((sg.spine / CH as f32) - 0.5).floor();
            let col = (sg.x / CW as f32).floor();
            if r < 0.0 || col < 0.0 {
                continue;
            }
            if r as usize == usize::from(row) && (col as usize) < COLS {
                let c = col as usize;
                cov[c] = cov[c].max(sg.cov);
            }
        }
        let lit = |i: usize| cov[c0 + i] >= LIT_COV;
        let map: String = (0..n).map(|i| if lit(i) { '#' } else { '.' }).collect();
        let mut holes = Vec::new();
        if let (Some(first), Some(last)) = ((0..n).find(|&i| lit(i)), (0..n).rfind(|&i| lit(i))) {
            let mut run = None;
            for i in first..=last {
                if lit(i) {
                    if let Some(s) = run.take() {
                        holes.push((s, i - s));
                    }
                } else if run.is_none() {
                    run = Some(i);
                }
            }
        }
        self.frames.push(RowFrame {
            ms: self.ms(),
            label: self.label.clone(),
            map,
            holes,
        });
    }

    /// The longest run of consecutive frames from `from` on that each carry
    /// a hole.
    fn longest_hole_run(&self, from: usize) -> usize {
        let (mut run, mut best) = (0, 0);
        for f in self.frames.iter().skip(from) {
            run = if f.holes.is_empty() { 0 } else { run + 1 };
            best = best.max(run);
        }
        best
    }

    /// The first frame whose label names `what` — the instant a nav's own
    /// corridor begins, as against the frame the idle before it began at.
    fn first_labelled(&self, what: &str) -> usize {
        self.frames
            .iter()
            .position(|f| f.label.starts_with(what))
            .unwrap_or(0)
    }

    /// The index of every frame from `from` on that carries a hole.
    fn hole_frames_from(&self, from: usize) -> Vec<usize> {
        self.frames
            .iter()
            .enumerate()
            .skip(from)
            .filter(|(_, f)| !f.holes.is_empty())
            .map(|(i, _)| i)
            .collect()
    }

    /// Every frame from `from` on that carries a hole, as the evidence a
    /// failure prints: the line's text over each such frame's map.
    fn holes_from(&self, from: usize) -> Vec<String> {
        let text: String = self.text.clone();
        self.frames
            .iter()
            .enumerate()
            .skip(from)
            .filter(|(_, f)| !f.holes.is_empty())
            .map(|(i, f)| {
                format!(
                    "frame {i:>4} t={:>6} ms [{}] holes={:?}\n      text |{text}|\n      lit  |{}|",
                    f.ms, f.label, f.holes, f.map
                )
            })
            .collect()
    }
}

/// S6: the owner's line to `…INTO MAIN`, Home (a 108-cell meteor to column
/// 0) `settle_ms` after the last key, End 300 ms later, then `!!!!`. Returns
/// the host and the Home's first frame.
fn home_end(composer: Composer, settle_ms: u64) -> (Host, usize) {
    let mut h = Host::new(composer, WordKeys::Inkish);
    h.type_str(LINE);
    let home = h.frames.len();
    h.nav_after("home", 0, settle_ms);
    h.nav_after("end", LINE.len(), 300);
    h.key_after('!', 200);
    h.type_str(&TAIL[1..]);
    h.idle(1500);
    (h, home)
}

/// S3: the owner's line, ⌥← ×2 (a scrub), a pause of EXACTLY `pause_ms`,
/// ⌥→ ×2 (then End if short of the end), then `!!!!`. Returns the host and
/// the first ⌥→'s first frame.
fn scrub_pause_hop(composer: Composer, words: WordKeys, pause_ms: u64) -> (Host, usize) {
    let mut h = Host::new(composer, words);
    h.type_str(LINE);
    h.word_left();
    h.word_left();
    let t = h.now + Duration::from_millis(pause_ms);
    h.idle_to(t);
    h.now = t;
    h.last_event = t;
    let hop = h.frames.len();
    h.word_right();
    h.word_right();
    if h.caret < h.text.len() {
        let n = h.text.len();
        h.nav_after("end", n, HOP_MS);
    }
    h.type_str(TAIL);
    h.idle(700);
    (h, hop)
}

/// The frames a LEAVING band's own head may still be sweeping the row in
/// after a jump has landed beside the caret — the band's clock, not the
/// corridor's. Measured at 11 on both composers at a 1.5 s settle; the bar
/// is 16 (256 ms), well inside the band's own 0.64 s exit, so the claim is
/// about a departure and not about a number. What those frames may hold is
/// pinned far more tightly below: one corridor, one departing cell.
const BAND_EXIT_FRAMES: usize = 16;

/// **HOME AND END OVER A FADING BAND, THEN TYPING, LEAVE THE LINE WHOLE.**
/// Home at 0.9 s (the band still in its grace: a scrub, the control), at
/// 1.5 s (inside the swoosh: RED before, 31 dark cells from col 33, the
/// drained stretch between the cap and the remnant, on 119 frames), at 2.2 s
/// and at 4.0 s (the band gone: RED before, the End's 44 dark cells between
/// the Home's island and its own, on 101 frames). On both composers, no
/// frame from the Home to the end of the take carries a hole — except, for
/// [`BAND_EXIT_FRAMES`] frames at the 1.5 s settle, the leaving band's OWN
/// head.
///
/// **A LEAVING BAND'S HEAD IS NOT A HOLE IN THE CORRIDOR** (2026-09-22, the
/// review round; this take was written on 2026-09-22 to pin the S6 fix and
/// is re-pinned here). At a 1.5 s settle the band's body has drained under
/// the glass census bar and only its crisp retract edge still reads as ink,
/// a single cell sweeping right at ~4 columns a frame. The first cut
/// bridged the corridor out to that band's INVISIBLE residue, which is the
/// whole-row paint the review measured and [`WAKE_MAX_CELLS`] forbids
/// ("a phrase, not a line"). The corridor now stops where the light stops,
/// so for the 64 ms its old head takes to sweep off the row the census sees
/// the corridor and that one departing cell as two islands. That is the
/// band finishing on its own clock, which is the abandoned-band law, not a
/// hole a key left: it is gone within [`BAND_EXIT_FRAMES`], and from the End
/// on — the jump back, and every key after it — the line carries no hole at
/// all, which is what S6 was about.
#[test]
fn home_then_end_over_a_fading_band_then_typing_leaves_no_hole_in_the_line() {
    let mut bad = Vec::new();
    for composer in [Composer::Ink, Composer::Claude] {
        for settle in [900u64, 1500, 2200, 4000] {
            let (h, home) = home_end(composer, settle);
            let jump = h.first_labelled("home").max(home);
            let late = h.hole_frames_from(jump + BAND_EXIT_FRAMES);
            if !late.is_empty() {
                bad.push(format!(
                    "{composer:?} settle={settle}: {} frame(s) with a hole more than \
                     {BAND_EXIT_FRAMES} frames after the Home\n{}",
                    late.len(),
                    h.holes_from(jump + BAND_EXIT_FRAMES)
                        .iter()
                        .take(6)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
            // What the early frames may carry is ONE leaving cell beyond the
            // corridor, never a gap the corridor itself left: every hole in
            // them has the corridor whole to its left.
            for i in h.hole_frames_from(jump) {
                let f = &h.frames[i];
                let lit = f.map.chars().filter(|&c| c == '#').count();
                let head = f.map.find('#').unwrap_or(0);
                let run = f.map[head..].chars().take_while(|&c| c == '#').count();
                if lit != run + 1 {
                    bad.push(format!(
                        "{composer:?} settle={settle}: frame {i} is not one corridor plus \
                         one departing cell\n{}",
                        h.holes_from(i).first().cloned().unwrap_or_default()
                    ));
                    break;
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

/// **THE CAP BINDS AT THE FAR EDGE, AND ONLY THERE.** The End back over
/// dark ground, 300 ms after a Home the band had already left, lays every
/// column from its landing to the Home's island it re-owns: the whole
/// 108-cell line, one interval, where it laid two islands of
/// [`WAKE_MAX_CELLS`] with 45 columns unlaid between them before. And the
/// Home itself, with nothing lit past the cap, still lays exactly the cap.
#[test]
fn the_end_back_over_dark_ground_lays_one_corridor_out_to_the_island_it_re_owns() {
    for composer in [Composer::Ink, Composer::Claude] {
        let (h, _) = home_end(composer, 4000);
        let (home, end) = (&h.moves[h.moves.len() - 2], &h.moves[h.moves.len() - 1]);
        assert_eq!(
            (home.1, home.2, end.1, end.2),
            (LINE.len(), 0, 0, LINE.len()),
            "{composer:?}: the setup: the Home and the End were traced"
        );
        let cap = usize::from(WAKE_MAX_CELLS);
        assert_eq!(
            home.3,
            (cap..=LINE.len()).collect::<Vec<_>>(),
            "{composer:?}: the Home over dark ground lays the cap at its landing and nothing past it"
        );
        assert!(
            end.3.is_empty(),
            "{composer:?}: the End's corridor is one interval from the Home's island to its \
             landing; uncovered columns {:?}",
            end.3
        );
    }
}

/// **THE OWNER'S `TO ` HOLE: A WORD HOP AT THE END OF THE SWOOSH.** ⌥← ×2,
/// then the first ⌥→ landing in the last ~20 ms of the band's fade (pause
/// 1481–1491 ms), a second ⌥→, `!!!!`. RED before at every pause in the
/// window: `IN` lit, `TO `/`O `/` ` dark, `MAIN!!!!` lit and held by the
/// hand. Now the first hop's corridor carries an `Over` cell at every column
/// of its span on its own replay frame, and no frame from the hop to the end
/// of the take carries a hole: both composers, both word-key semantics.
#[test]
fn a_word_hop_landing_in_the_last_twenty_ms_of_the_swoosh_lays_its_corridor_whole() {
    let mut bad = Vec::new();
    for composer in [Composer::Ink, Composer::Claude] {
        for words in [WordKeys::Inkish, WordKeys::Readline] {
            for pause in FADE_END_PAUSES_MS {
                let (h, hop) = scrub_pause_hop(composer, words, pause);
                let first = h
                    .moves
                    .iter()
                    .find(|m| m.1 < m.2)
                    .expect("the first opt-right was traced");
                if !first.3.is_empty() {
                    bad.push(format!(
                        "{composer:?} {words:?} pause={pause}: the hop {}->{} left corridor \
                         columns {:?} with no Over cell on its replay frame {}",
                        first.1, first.2, first.3, first.0
                    ));
                }
                let holes = h.holes_from(hop);
                if !holes.is_empty() {
                    bad.push(format!(
                        "{composer:?} {words:?} pause={pause}: {} frame(s) with a hole after the hop\n{}",
                        holes.len(),
                        holes.iter().take(4).cloned().collect::<Vec<_>>().join("\n")
                    ));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

/// **THE SAME HOP OVER A BAND STILL RETRACTING.** The first ⌥→ at a pause
/// of 1.0–1.2 s lands while the band's drain has not reached the corridor.
/// Before, the hop laid nothing over it, and the second hop lit only its
/// landing past the band's end: an isolated cell 2–6 columns right of the
/// draining band for four frames. Now the hop covers the leaving band from
/// its level, and no column of its span is left to the band's clock.
///
/// What remains is the hop's own FLIGHT and is not held by anything: the
/// second hop's cells are born 15 ms apart along its 60 ms flight (K3),
/// the band's retract has already drawn its lit end in to col 106, and
/// until col 107's corridor cell is born, the landing cell's left boundary
/// vertex reads 107's full coverage. That is one dark column for two frames,
/// measured. So the bar here is the reproducers' own: no hole outlives three
/// frames.
#[test]
fn a_word_hop_over_a_retracting_band_leaves_nothing_to_the_band_s_clock() {
    let mut bad = Vec::new();
    for pause in [1000u64, 1100, 1200] {
        let (h, hop) = scrub_pause_hop(Composer::Ink, WordKeys::Inkish, pause);
        let first = h
            .moves
            .iter()
            .find(|m| m.1 < m.2)
            .expect("the first opt-right was traced");
        if !first.3.is_empty() {
            bad.push(format!(
                "pause={pause}: the hop {}->{} left corridor columns {:?} with no Over cell",
                first.1, first.2, first.3
            ));
        }
        let run = h.longest_hole_run(hop);
        if run > 3 {
            bad.push(format!(
                "pause={pause}: a hole stood for {run} frames after the hop\n{}",
                h.holes_from(hop).join("\n")
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

/// **THE CONTROL: A HOP BESIDE A LIVE BAND LAYS NOTHING OVER IT.** The same
/// scrub with a 300 ms pause: the band is in its grace, the ⌥→ is a scrub
/// over it (nothing laid, no `Over` cell on the band), and the line stays
/// whole. The hop's cover reaches only light that is leaving.
#[test]
fn a_word_hop_over_a_live_band_is_a_scrub_and_lays_nothing() {
    for composer in [Composer::Ink, Composer::Claude] {
        let (h, hop) = scrub_pause_hop(composer, WordKeys::Inkish, 300);
        let first = h
            .moves
            .iter()
            .find(|m| m.1 < m.2)
            .expect("the first opt-right was traced");
        assert_eq!(
            first.3.len(),
            first.2 - first.1 + 1,
            "{composer:?}: a scrub over the live band lays no Over cell over it: uncovered {:?}",
            first.3
        );
        let holes = h.holes_from(hop);
        assert!(holes.is_empty(), "{composer:?}:\n{}", holes.join("\n"));
    }
}

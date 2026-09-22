// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PHRASE BOUNDARY'S SPACE KEEPS ITS LIGHT (2026-09-21) — the owner, on the
//! installed v0.90.0 inside Claude Code's composer, trail style "rainbow
//! kitty pet", tall, intensity 1.00: the line
//! `› Adding Fable. HEY! zoom out. git pull▮` carried the band under every
//! glyph EXCEPT three cells — the space after "Adding", the space after
//! "Fable." and the space after "HEY!" — while the spaces inside
//! "zoom out." and "git pull" were lit. One continuous walk (a single
//! reflected sweep, red at the caret), three single cells dark: the cells
//! at the boundaries of phrases typed in BURSTS with a thinking pause
//! between them.
//!
//! This is `scrub_gaps.rs`'s host-seam harness — a real
//! `aterm_core::terminal::Terminal` driven byte for byte, its rows sampled
//! exactly as `app_render.rs`'s LOCK A samples them, fed to `CursorGlow` and
//! ticked on a 16 ms frame train — pointed at that gesture: the owner's four
//! phrases typed at 12 cps with a pause of 0.3–3 s between them, the
//! boundary space placed either as the LAST key before the pause or the
//! FIRST key after it, through zsh and through an Ink-shaped composer
//! (`docs/design/repro/inkish.py`) — plus the things a real Ink composer
//! does that `inkish.py` does not: it draws its own inverse-video cursor
//! cell after the text, it coalesces keys into one repaint under load, it
//! repaints during a pause with no content change, and — the shape the
//! owner's ring pointed at (`declined=25 last_decline_reason=no-fresh-hint`,
//! a 3-column caret move licensed as one key) — its repaint of a space can
//! land LATE, past the typed hint's quarter-second freshness, with nothing
//! after it to rescue it at a burst's end.
//!
//! THE THIRD COMPOSER — CLAUDE CODE'S OWN BYTES (the second lane, the same
//! day): fifteen takes with the real Claude Code composer IDLE reproduced
//! nothing, and the owner's screenshot was taken MID-TURN — a response
//! streaming, the spinner animating three rows above the composer, the
//! machine at load ~58. [`Composer::ClaudeCode`] writes EXACTLY the frames
//! `ptyrec.py` recorded: a letter is one DEC 2026 bracket (hide, home, CR,
//! CUF to the column, CUD to the row, ONE glyph, park at the last row, CUP
//! back to the cell after the glyph, show), a space is a bracket with only
//! the CUP, and ~0.95 s into a pause one frame erases part of a status row.
//! Around them, as independent dials ([`CcShape`]): the turn's SPINNER
//! frame every 100 ms on `ROW - 3`; keys COALESCED into one frame; every
//! frame's bytes fed in PTY-sized CHUNKS with the host's present rule
//! between them; a key's frame SPLIT from its key by up to half a second
//! (so the engine HOLDS the key and its cell comes from the echo alone) —
//! and, with the spinner on, spinner frames landing between a key and its
//! frame. The host's present rule is `app_render.rs`'s own: LOCK A runs
//! when a bracket closes, when one has stayed open past `SYNC_HOLD_CAP`
//! (150 ms — the partial state sampled as it stands, the caret hidden and
//! parked wherever the bytes so far left it), and every 16 ms while no
//! bracket is open; a held frame runs NOTHING (no probe, no tick).
//!
//! Every take's census is printed: the columns under typed cells with no
//! owner (or no coverage on the settled final frame), and every cohort on
//! the row as `(id, col0, col1, phase, phrase, wake, abandoned)`. One
//! assertion per (composer, placement, pause) says the final frame has no
//! gap, and no settled post-burst frame had one either. A red assertion
//! here is a REPRODUCTION, not a broken test.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::cursor_glow::{AdmissionPhase, CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::{Phase, Ribbon};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

/// The owner's grid: 63 rows, the composer on row 61.
const ROWS: usize = 63;
const COLS: usize = 80;
const CW: usize = 8;
const CH: usize = 16;
const ROW: u16 = 61;
/// The composer's two-cell prefix (`› ` for Ink, `% ` for zsh): the text
/// starts at this column, as it did in the owner's screenshot.
const COL0: u16 = 2;
/// The owner's four phrases, in order; the boundary spaces are placed by the
/// scenario.
const BURSTS: [&str; 4] = ["Adding", "Fable.", "HEY!", "zoom out. git pull"];
/// 12 cps.
const KEY_MS: u64 = 83;
/// The frame train.
const FRAME_MS: u64 = 16;
/// `inkish.py`'s split between the redraw's head and its tail.
const INK_SPLIT_MS: u64 = 30;
/// The thinking pauses swept, ms — either side of the 900 ms phrase rest.
const PAUSES: [u64; 7] = [300, 700, 950, 1200, 1500, 2000, 3000];
/// How long after the last key the FINAL census is read: inside the grace,
/// so every cell of the line should be lit and past its attack.
const SETTLE_MS: u64 = 200;

// ---- Claude Code's recorded shapes -------------------------------------

/// `app_render.rs`'s `SYNC_HOLD_CAP`: a DEC 2026 bracket open longer than
/// this is presented mid-frame.
const SYNC_HOLD_CAP: Duration = Duration::from_millis(150);
/// The turn's spinner cadence.
const SPINNER_MS: u64 = 100;
/// The recorded status-row erase, ~0.95 s after the last key of a pause.
const IDLE_FRAME_MS: u64 = 950;
/// The spinner's glyphs, cycled one per frame.
const SPINNER: [char; 6] = ['✻', '✽', '✶', '✳', '✢', '·'];
/// The owner's turn had run 30 s at the screenshot.
const TURN_BASE_S: u64 = 30;
/// The PTY chunk gap of a starved reader thread (load ~58): two 64-byte
/// chunks keep a bracket open 80 ms, three keep it open past the cap.
const CHUNK_GAP_MS: u64 = 80;
/// The key cadences swept for Claude Code: 10 cps and 25 cps.
const CC_CADENCES: [u64; 2] = [100, 40];
/// The pauses swept for Claude Code.
const CC_PAUSES: [u64; 3] = [700, 1200, 2500];
/// The key→frame delays swept.
const CC_SPLITS: [u64; 4] = [0, 120, 300, 500];

const SYNC_ON: &str = "\x1b[?2026h";
const SYNC_OFF: &str = "\x1b[?2026l";
const HIDE: &str = "\x1b[?25l";
const SHOW: &str = "\x1b[?25h";

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

/// The owner's config: rainbow kitty, tall, intensity 1.00, dark.
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
        audible: true,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
    }
}

/// The full line, the bursts joined by their boundary spaces.
fn line() -> String {
    BURSTS.join(" ")
}

/// What an Ink composer does beyond `inkish.py`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Shape {
    /// Draw an inverse-video SPACE cell right after the text — Claude Code's
    /// own cursor — on every redraw.
    cursor_cell: bool,
    /// Never show the DEC caret again after the first hide (log-update
    /// hides it for the session and Ink's cursor cell stands in).
    hidden_caret: bool,
    /// Keys per repaint under load; `0` or `1` is one repaint per key. A
    /// burst's last key always flushes.
    coalesce: usize,
    /// A content-free repaint half-way through every pause.
    idle_repaint: bool,
    /// A SPACE's repaint lands this many ms after its key (the composer
    /// doing word-boundary work); keys typed meanwhile fold into it. `0` is
    /// on the key.
    late_space_ms: u64,
    /// A rewrite of the line starts [`MID_REWRITE_KEY_MS`] before every
    /// pause ends and is SAMPLED mid-way — the last letter typed read blank
    /// by one present — the next burst's first key lands while the letter
    /// is still owed (its redraw carries the blank), and the rewrite
    /// completes [`MID_REWRITE_DONE_MS`] after that key's caret is shown.
    /// The shape the content witness's partial release meets on a loaded
    /// composer (2026-09-21).
    mid_rewrite_blank: bool,
}

/// The mid-rewrite sample lands this long before the next burst's first key.
const MID_REWRITE_KEY_MS: u64 = 50;
/// …and the rewrite completes this long after that key's caret is shown.
const MID_REWRITE_DONE_MS: u64 = 50;

/// How a Claude Code frame's bytes reach the terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Chunk {
    /// The whole frame in one `process`.
    #[default]
    Whole,
    /// 64-byte PTY reads, [`CHUNK_GAP_MS`] apart.
    Pty64,
    /// 1024-byte PTY reads, [`CHUNK_GAP_MS`] apart (every recorded frame
    /// fits one).
    Pty1024,
}

impl Chunk {
    fn bytes(self) -> usize {
        match self {
            Self::Whole => 0,
            Self::Pty64 => 64,
            Self::Pty1024 => 1024,
        }
    }
}

/// Claude Code's recorded byte shapes and the mid-turn dials around them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CcShape {
    /// A turn is running: every [`SPINNER_MS`] a status-row frame on
    /// `ROW - 3` — a letter frame's shape writing the spinner glyph and the
    /// elapsed seconds, hidden throughout, parked at the last row, the caret
    /// returned to its cell.
    spinner: bool,
    /// Keys per frame: `0` or `1` is one frame per key; a burst's last key
    /// always flushes. One frame writes every glyph (home, CR, CUF, CUD,
    /// glyph — each) and places the caret after the last.
    coalesce: usize,
    /// How the frame's bytes arrive.
    chunk: Chunk,
    /// A key's frame lands this many ms after the key that flushed it (the
    /// composer busy); keys typed meanwhile fold into that frame.
    split_ms: u64,
}

/// Which program echoes the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Composer {
    Zsh,
    Ink(Shape),
    ClaudeCode(CcShape),
}

impl Composer {
    fn shape(self) -> Shape {
        match self {
            Self::Zsh | Self::ClaudeCode(_) => Shape::default(),
            Self::Ink(s) => s,
        }
    }

    fn cc(self) -> Option<CcShape> {
        match self {
            Self::ClaudeCode(s) => Some(s),
            _ => None,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Zsh => "zsh".to_string(),
            Self::Ink(s) if s == Shape::default() => "ink".to_string(),
            Self::Ink(s) => {
                let mut l = String::from("ink");
                if s.cursor_cell {
                    l.push_str("+cursor_cell");
                }
                if s.hidden_caret {
                    l.push_str("+hidden_caret");
                }
                if s.coalesce > 1 {
                    let _ = write!(l, "+coalesce{}", s.coalesce);
                }
                if s.idle_repaint {
                    l.push_str("+idle_repaint");
                }
                if s.late_space_ms > 0 {
                    let _ = write!(l, "+late_space{}", s.late_space_ms);
                }
                if s.mid_rewrite_blank {
                    l.push_str("+mid_rewrite_blank");
                }
                l
            }
            Self::ClaudeCode(s) => {
                let mut l = String::from("cc");
                if s.spinner {
                    l.push_str("+spin");
                }
                if s.coalesce > 1 {
                    let _ = write!(l, "+co{}", s.coalesce);
                }
                match s.chunk {
                    Chunk::Whole => {}
                    Chunk::Pty64 => l.push_str("+ch64"),
                    Chunk::Pty1024 => l.push_str("+ch1024"),
                }
                if s.split_ms > 0 {
                    let _ = write!(l, "+split{}", s.split_ms);
                }
                l
            }
        }
    }
}

/// How the host arms the typed hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seam {
    /// `note_typed_cells(now, 1)` — `scrub_gaps.rs`'s spelling.
    Plain,
    /// `note_typed_glyph(now, 1, shifted, class)` — `app_input.rs`'s: a
    /// space is `TypedClass::Space`, `!` is `Bang`, a capital is `Capital`
    /// and shifted.
    Classed,
}

/// Where the boundary space goes relative to the pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// The space is the last key of a burst, then the pause.
    SpaceBeforePause,
    /// The pause, then the space is the first key of the next burst.
    SpaceAfterPause,
}

/// One column's census: `(owner identity, planned coverage)` — the owner as
/// `(cohort, born, typing, cov0 bits)`, `None` where no live cell owns the
/// column.
type Column = (Option<(u32, Instant, bool, u32)>, u8);

/// Everything one cohort on the row says for the report.
type CohortRow = (u32, u16, u16, Phase, u32, bool, bool);

/// A frame the composer owes at an instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Job {
    /// The Ink composer's deferred redraw.
    InkRedraw,
    /// Claude Code's key frame (every pending key).
    KeyFrame,
    /// The turn's spinner frame.
    Spinner,
    /// The recorded mid-pause status-row erase.
    IdleFrame,
}

/// `app_render.rs`'s `sync_frame_hold`, verbatim in its logic: given the
/// pane's sync level, whether it was active at the previous present, the
/// armed deadline, the armed close counter, the terminal's close counter
/// and open-dirty bit, the clock and the cap — `(deadline, hold, armed
/// seq)`.
#[allow(clippy::too_many_arguments)]
fn sync_frame_hold(
    active: bool,
    open_dirty: bool,
    end_seq: u64,
    was_active: bool,
    armed: Option<Instant>,
    armed_end_seq: u64,
    now: Instant,
    timeout: Duration,
) -> (Option<Instant>, bool, u64) {
    if !active {
        return (None, false, end_seq);
    }
    if was_active && end_seq != armed_end_seq {
        return (Some(now + timeout), open_dirty, end_seq);
    }
    let mut deadline = if was_active {
        armed
    } else {
        Some(now + timeout)
    };
    let hold = deadline.is_some_and(|d| now < d);
    if !hold {
        deadline = None;
    }
    (deadline, hold, end_seq)
}

/// The host: one terminal, the kitty, one clock.
struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    /// The clock of the last key — the next one is scheduled from it.
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
    composer: Composer,
    seam: Seam,
    /// The Ink composer's line so far, and its caret index into it.
    text: String,
    caret: usize,
    /// Keys echoed into `text` but not yet repainted.
    pending: usize,
    /// A repaint the composer owes, and when it lands.
    defer_until: Option<Instant>,
    /// Every frame on which the caret was hidden, counted.
    hidden_frames: usize,
    // ---- Claude Code ----
    /// Keys typed and not yet framed: `(index into the line, glyph)`.
    cc_pending: Vec<(u16, char)>,
    /// When the owed key frame lands.
    cc_frame_at: Option<Instant>,
    /// When the next spinner frame lands.
    spinner_at: Option<Instant>,
    spinner_n: usize,
    /// When the mid-pause status erase lands (re-armed by every key).
    idle_frame_at: Option<Instant>,
    /// The NBSP after `❯` is written with the first glyph.
    nbsp_drawn: bool,
    /// The caret as the last KEY frame drew it: a spinner or idle frame
    /// returns the caret there — one render carries text and caret from
    /// one state, so a frame that lands before a key is processed knows
    /// nothing of that key.
    drawn: usize,
    /// A chunked write in progress: the train runs, no other frame lands.
    writing: bool,
    /// The Ink composer's rewrite in progress: the index into `text` of the
    /// letter it has blanked and still owes ([`Shape::mid_rewrite_blank`]).
    blank_idx: Option<usize>,
    // ---- the host's sync hold ----
    sync_was_active: bool,
    sync_hold_until: Option<Instant>,
    sync_armed_seq: u64,
    /// Presents the hold skipped (LOCK A never ran).
    held_frames: usize,
    /// Presents that sampled a bracket still open (past the cap).
    mid_bracket_frames: usize,
}

impl Host {
    fn new(composer: Composer, seam: Seam) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        match composer {
            // The shell's prompt, two cells, on the composer's row.
            Composer::Zsh => term.process(format!("\x1b[{};1H% ", ROW + 1).as_bytes()),
            // `inkish.py`'s entry: the alt screen, cleared.
            Composer::Ink(_) => term.process(b"\x1b[?1049h\x1b[2J"),
            // Claude Code's: the alt screen, the empty composer a bare `❯`,
            // the caret shown two cells in (where a cleared composer leaves
            // it).
            Composer::ClaudeCode(_) => term.process(
                format!(
                    "\x1b[?1049h\x1b[2J\x1b[{};1H❯\x1b[{};{}H",
                    ROW + 1,
                    ROW + 1,
                    COL0 + 1
                )
                .as_bytes(),
            ),
        }
        let sync_armed_seq = term.sync_end_seq();
        let mut h = Self {
            term,
            glow,
            cfg: cfg(),
            g: geom(),
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            composer,
            seam,
            text: String::new(),
            caret: 0,
            pending: 0,
            defer_until: None,
            hidden_frames: 0,
            cc_pending: Vec::new(),
            cc_frame_at: None,
            spinner_at: None,
            spinner_n: 0,
            idle_frame_at: None,
            nbsp_drawn: false,
            drawn: 0,
            writing: false,
            blank_idx: None,
            sync_was_active: false,
            sync_hold_until: None,
            sync_armed_seq,
            held_frames: 0,
            mid_bracket_frames: 0,
        };
        match composer {
            Composer::Zsh => h.frame(),
            // The empty composer on glass — its prefix and its cursor cell.
            Composer::Ink(_) => h.ink_redraw(),
            Composer::ClaudeCode(s) => {
                if s.spinner {
                    h.spinner_at = Some(now + Duration::from_millis(SPINNER_MS));
                }
                h.frame();
            }
        }
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// The terminal at the harness's clock — its mode-2026 timeout runs on
    /// `self.now`, not the wall.
    fn process(&mut self, bytes: &[u8]) {
        self.term.process_at(
            bytes,
            ClockReading {
                monotonic: self.now,
                wall_ms: None,
            },
        );
    }

    /// EXACTLY LOCK A, then the tick: sample the cursor, the repaint blink,
    /// the caret row's probe and the rows the resident ribbon occupies —
    /// all from the terminal AFTER the last `process` — then advance the
    /// engine one frame.
    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        if cur.is_none() {
            self.hidden_frames += 1;
        }
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
    }

    /// THE HOST'S PRESENT RULE (`app_render.rs`, SYNC-1): read the pane's
    /// sync level, close counter and open-dirty bit under LOCK A's lock,
    /// run the hold machine, and either skip this present entirely — a
    /// held frame runs no probe and no tick — or run LOCK A on the terminal
    /// as it stands, bracket open or not. For the composers that never
    /// bracket, this is exactly `frame`.
    fn present(&mut self) {
        let active = self.term.modes().synchronized_output();
        let (deadline, hold, seq) = sync_frame_hold(
            active,
            self.term.sync_open_dirty(),
            self.term.sync_end_seq(),
            self.sync_was_active,
            self.sync_hold_until,
            self.sync_armed_seq,
            self.now,
            SYNC_HOLD_CAP,
        );
        self.sync_was_active = active;
        self.sync_hold_until = deadline;
        self.sync_armed_seq = seq;
        if hold {
            self.held_frames += 1;
            return;
        }
        if active {
            self.mid_bracket_frames += 1;
        }
        self.frame();
    }

    /// One frame of the train, 16 ms on.
    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        self.present();
    }

    /// The earliest frame the composer owes.
    fn next_job(&self) -> Option<(Instant, Job)> {
        if self.writing {
            return None;
        }
        let mut best: Option<(Instant, Job)> = None;
        let mut offer = |at: Option<Instant>, job: Job| {
            if let Some(at) = at
                && best.is_none_or(|(b, _)| at < b)
            {
                best = Some((at, job));
            }
        };
        offer(self.defer_until, Job::InkRedraw);
        offer(self.cc_frame_at, Job::KeyFrame);
        offer(self.spinner_at, Job::Spinner);
        offer(self.idle_frame_at, Job::IdleFrame);
        best
    }

    fn run(&mut self, job: Job) {
        match job {
            Job::InkRedraw => {
                self.defer_until = None;
                self.ink_redraw();
            }
            Job::KeyFrame => {
                self.cc_frame_at = None;
                let f = self.cc_key_frame();
                self.cc_write(&f);
            }
            Job::Spinner => {
                self.spinner_at = Some(self.now + Duration::from_millis(SPINNER_MS));
                let f = self.cc_spinner_frame();
                self.cc_write(&f);
            }
            Job::IdleFrame => {
                self.idle_frame_at = None;
                let f = self.cc_idle_frame();
                self.cc_write(&f);
            }
        }
    }

    /// Frames at 16 ms while a whole frame still fits before `t` — and a
    /// frame the composer owes lands at its instant on the way.
    fn idle_to(&mut self, t: Instant) {
        loop {
            let next = self.now + Duration::from_millis(FRAME_MS);
            if let Some((at, job)) = self.next_job().filter(|&(at, _)| at <= next && at <= t) {
                self.now = self.now.max(at);
                self.run(job);
                continue;
            }
            if next > t {
                break;
            }
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
        // A repaint the composer owed may have carried the clock past `t`:
        // the key lands when the composer is free, never in the past.
        let t = t.max(self.now);
        self.now = t;
        self.last_event = t;
    }

    /// The Ink composer's redraw of its line with the caret at `self.caret`:
    /// the head (hide, home the row, clear it, the prefix, the line, and —
    /// per the shape — the inverse cursor cell) on one present; the tail
    /// (place the caret and, unless the shape keeps it hidden, show it)
    /// `INK_SPLIT_MS` later on the next.
    fn ink_redraw(&mut self) {
        let s = self.composer.shape();
        // A rewrite still in progress carries its blank: the owed letter is
        // not back until the rewrite completes below.
        let shown: String = self
            .text
            .chars()
            .enumerate()
            .map(|(i, ch)| if Some(i) == self.blank_idx { ' ' } else { ch })
            .collect();
        let mut head = format!("\x1b[?25l\x1b[{};1H\x1b[2K› {}", ROW + 1, shown);
        if s.cursor_cell {
            head.push_str("\x1b[7m \x1b[27m");
        }
        self.process(head.as_bytes());
        self.frame();
        self.now += Duration::from_millis(INK_SPLIT_MS);
        let mut tail = format!("\x1b[{};{}H", ROW + 1, usize::from(COL0) + self.caret + 1);
        if !s.hidden_caret {
            tail.push_str("\x1b[?25h");
        }
        self.process(tail.as_bytes());
        self.frame();
        self.pending = 0;
        if let Some(idx) = self.blank_idx.take() {
            // The rewrite completes: the letter put back where it was, the
            // caret placed again, one present.
            self.now += Duration::from_millis(MID_REWRITE_DONE_MS);
            let ch = self.text.chars().nth(idx).unwrap_or(' ');
            let done = format!(
                "\x1b[{};{}H{}\x1b[{};{}H",
                ROW + 1,
                usize::from(COL0) + idx + 1,
                ch,
                ROW + 1,
                usize::from(COL0) + self.caret + 1
            );
            self.process(done.as_bytes());
            self.frame();
        }
    }

    // ---- Claude Code's frames, byte for byte -----------------------------

    /// `ESC[H CR ESC[<col>C ESC[<row>B <text>`: home, carriage return, cursor
    /// forward to the column, cursor down to the row, the text.
    fn cc_put(row: u16, col: u16, text: &str) -> String {
        let mut s = String::from("\x1b[H\r");
        if col > 0 {
            let _ = write!(s, "\x1b[{col}C");
        }
        if row > 0 {
            let _ = write!(s, "\x1b[{row}B");
        }
        s.push_str(text);
        s
    }

    /// The park at the last row, column 1.
    fn cc_park() -> String {
        format!("\x1b[{ROWS};1H")
    }

    /// The caret placed at the cell one past `idx` glyphs of the line.
    fn cc_caret(idx: usize) -> String {
        format!("\x1b[{};{}H", ROW + 1, usize::from(COL0) + idx + 1)
    }

    /// The key frame for every pending key: a letter is home, CR, CUF, CUD
    /// and ONE glyph (a coalesced frame repeats that per glyph); a space
    /// writes nothing; any glyph written parks at the last row; then the
    /// caret is placed after the last key and shown. The recorded space
    /// frame — hide, CUP, show — is this frame with nothing to write.
    fn cc_key_frame(&mut self) -> String {
        let keys = std::mem::take(&mut self.cc_pending);
        let mut f = String::from(SYNC_ON);
        f.push_str(HIDE);
        let mut wrote = false;
        for (idx, ch) in keys {
            if ch == ' ' {
                continue;
            }
            if !self.nbsp_drawn {
                f.push_str(&Self::cc_put(ROW, 1, "\u{a0}"));
                self.nbsp_drawn = true;
            }
            let mut buf = [0u8; 4];
            f.push_str(&Self::cc_put(ROW, COL0 + idx, ch.encode_utf8(&mut buf)));
            wrote = true;
        }
        if wrote {
            f.push_str(&Self::cc_park());
        }
        self.drawn = self.caret;
        f.push_str(&Self::cc_caret(self.drawn));
        f.push_str(SHOW);
        f.push_str(SYNC_OFF);
        f
    }

    /// The turn's spinner frame on `ROW - 3`: the letter frame's shape with
    /// the spinner glyph and the elapsed seconds instead of one glyph.
    fn cc_spinner_frame(&mut self) -> String {
        let glyph = SPINNER[self.spinner_n % SPINNER.len()];
        self.spinner_n += 1;
        let secs = TURN_BASE_S + self.ms() / 1000;
        let text = format!("{glyph} Transmuting… ({secs}s · ↓ 1.3k tokens)");
        let mut f = String::from(SYNC_ON);
        f.push_str(HIDE);
        f.push_str(&Self::cc_put(ROW - 3, 0, &text));
        f.push_str(&Self::cc_park());
        f.push_str(&Self::cc_caret(self.drawn));
        f.push_str(SHOW);
        f.push_str(SYNC_OFF);
        f
    }

    /// The recorded mid-pause frame: erase to the end of a status row,
    /// park, return, show.
    fn cc_idle_frame(&self) -> String {
        let mut f = String::from(SYNC_ON);
        f.push_str(HIDE);
        f.push_str("\x1b[H\r\x1b[90C\x1b[34B\x1b[K");
        f.push_str(&Self::cc_park());
        f.push_str(&Self::cc_caret(self.drawn));
        f.push_str(SHOW);
        f.push_str(SYNC_OFF);
        f
    }

    /// The frame train for `ms` with no frame of the composer's landing —
    /// the reader thread between two chunks of one write.
    fn advance(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.step();
        }
        self.now = t;
    }

    /// One frame's bytes to the terminal — whole, or in the shape's PTY
    /// chunks with the gap and the train between them — and the host's
    /// present rule after every chunk.
    fn cc_write(&mut self, frame: &str) {
        let s = self.composer.cc().unwrap_or_default();
        let bytes = frame.as_bytes();
        let chunk = s.chunk.bytes();
        if chunk == 0 || bytes.len() <= chunk {
            self.process(bytes);
            self.present();
            return;
        }
        self.writing = true;
        for (i, part) in bytes.chunks(chunk).enumerate() {
            if i > 0 {
                self.advance(CHUNK_GAP_MS);
            }
            self.process(part);
            self.present();
        }
        self.writing = false;
    }

    /// The app_input seam's typed hint for `ch`.
    fn arm(&mut self, ch: char) {
        match self.seam {
            Seam::Plain => self.glow.note_typed_cells(self.now, 1),
            Seam::Classed => {
                let class = match ch {
                    ' ' => TypedClass::Space,
                    '!' => TypedClass::Bang,
                    c if c.is_uppercase() => TypedClass::Capital,
                    _ => TypedClass::Glyph,
                };
                let shifted = ch != ' ' && (ch.is_uppercase() || ch == '!');
                self.glow.note_typed_glyph(self.now, 1, shifted, class);
            }
        }
    }

    /// One typed key `ms` after the last event: the seam arms the typed
    /// hint, the program echoes it (now, or later, per the shape), the
    /// frames present. `last` marks a burst's last key, which always
    /// flushes a coalesced repaint.
    fn key_after(&mut self, ch: char, ms: u64, last: bool) {
        self.schedule(ms);
        self.arm(ch);
        match self.composer {
            Composer::Zsh => {
                let mut buf = [0u8; 4];
                self.process(ch.encode_utf8(&mut buf).as_bytes());
                self.caret += 1;
                self.frame();
            }
            Composer::Ink(s) => {
                self.text.insert(self.caret, ch);
                self.caret += 1;
                self.pending += 1;
                if ch == ' ' && s.late_space_ms > 0 {
                    // The composer is busy with the word boundary: this key
                    // and anything typed meanwhile repaint when it is done.
                    let d = self.now + Duration::from_millis(s.late_space_ms);
                    self.defer_until = Some(self.defer_until.map_or(d, |e| e.max(d)));
                }
                // …still busy (this key folds into the owed repaint), or
                // under load (the repaint waits for more keys): no repaint
                // on this key.
                let owed = self.defer_until.is_some();
                let waits = s.coalesce > 1 && self.pending < s.coalesce && !last;
                if !owed && !waits {
                    self.ink_redraw();
                }
            }
            Composer::ClaudeCode(s) => {
                self.text.insert(self.caret, ch);
                self.cc_pending.push((self.caret as u16, ch));
                self.caret += 1;
                self.idle_frame_at = Some(self.now + Duration::from_millis(IDLE_FRAME_MS));
                let flush = last || self.cc_pending.len() >= s.coalesce.max(1);
                if flush && self.cc_frame_at.is_none() {
                    if s.split_ms == 0 {
                        self.run(Job::KeyFrame);
                    } else {
                        // The composer is busy: the frame lands later, with
                        // every key typed meanwhile folded in.
                        self.cc_frame_at = Some(self.now + Duration::from_millis(s.split_ms));
                    }
                }
            }
        }
    }

    /// The pause between two bursts, run up to the frame before its end:
    /// the owed repaint lands, and an idle-repainting composer redraws the
    /// same content half-way through.
    fn pause_train(&mut self, ms: u64) {
        let s = self.composer.shape();
        if s.idle_repaint && ms > 2 * FRAME_MS {
            let mid = self.last_event + Duration::from_millis(ms / 2);
            self.idle_to(mid);
            self.now = self.now.max(mid);
            self.ink_redraw();
        }
        if s.mid_rewrite_blank && ms > MID_REWRITE_KEY_MS + FRAME_MS {
            // The composer starts rewriting the line: the present that
            // lands mid-way samples the row with the last letter typed
            // BLANK — one recorded glyph gone for a walk, the caret hidden
            // — and the next key comes before the rewrite completes.
            let at = self.last_event + Duration::from_millis(ms - MID_REWRITE_KEY_MS);
            self.idle_to(at);
            self.now = self.now.max(at);
            let trimmed = self.text.trim_end().chars().count();
            if let Some(idx) = trimmed.checked_sub(1) {
                self.blank_idx = Some(idx);
                let blank = format!("{HIDE}\x1b[{};{}H ", ROW + 1, usize::from(COL0) + idx + 1);
                self.process(blank.as_bytes());
                self.frame();
            }
        }
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
    }

    fn ribbon(&self) -> &Ribbon {
        self.glow.v2_ribbon().expect("rainbow kitty owns the frame")
    }

    /// The census of the line's columns on this frame.
    fn census(&self) -> Vec<Column> {
        let rib = self.ribbon();
        let cw = CW as f32;
        let ch = CH as f32;
        (COL0..COL0 + line().len() as u16)
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

    /// The columns of the `typed` cells typed so far with no owner — or,
    /// with `need_cov`, with no owner or no coverage either.
    fn gaps(&self, typed: usize, need_cov: bool) -> Vec<u16> {
        self.census()
            .iter()
            .take(typed)
            .enumerate()
            .filter(|(_, (owner, cov))| owner.is_none() || (need_cov && *cov == 0))
            .map(|(i, _)| COL0 + i as u16)
            .collect()
    }

    /// Every cohort on the composer's row.
    fn cohort_rows(&self) -> Vec<CohortRow> {
        self.ribbon()
            .cohorts()
            .iter()
            .filter(|k| k.row == ROW)
            .map(|k| (k.id, k.col0, k.col1, k.phase, k.phrase, k.wake, k.abandoned))
            .collect()
    }

    /// The body at every column of the line on this frame: `(col, glyph,
    /// cov, up/ch, dn/ch)` of the column's strongest segment.
    fn body(&self) -> Vec<(u16, char, u8, f32, f32)> {
        let rib = self.ribbon();
        let cw = CW as f32;
        let ch = CH as f32;
        let l = line();
        (COL0..COL0 + l.len() as u16)
            .map(|col| {
                let seg = rib
                    .plan_segments()
                    .iter()
                    .filter(|s| {
                        ((s.spine / ch) - 0.5).floor() as i64 == i64::from(ROW)
                            && (s.x / cw).floor() as i64 == i64::from(col)
                    })
                    .max_by_key(|s| s.cov);
                let glyph = l.chars().nth(usize::from(col - COL0)).unwrap_or(' ');
                match seg {
                    Some(s) => (col, glyph, s.cov, s.up / ch, s.dn / ch),
                    None => (col, glyph, 0, 0.0, 0.0),
                }
            })
            .collect()
    }

    /// The ring's declined and in-flight-licensed rows so far, oldest
    /// first: `origin->target reason/licence @ms`.
    fn ring_notes(&self) -> String {
        let mut s = String::new();
        for r in self.glow.admission_log() {
            let odd = r.phase == AdmissionPhase::Declined || r.licence != "key";
            if !odd {
                continue;
            }
            let at = r.at.saturating_duration_since(self.t0).as_millis();
            let _ = write!(
                s,
                " {},{}->{},{} {}/{}@{}",
                r.origin.0, r.origin.1, r.target.0, r.target.1, r.reason, r.licence, at
            );
        }
        s
    }
}

/// The glyphs at `cols` of the line, for the report.
fn glyphs_at(cols: &[u16]) -> String {
    let l = line();
    cols.iter()
        .map(|&c| {
            let g = l.chars().nth(usize::from(c - COL0)).unwrap_or('?');
            format!("{c}:{g:?}")
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// One take's verdict.
struct Take {
    label: String,
    /// Every typed column with no owner or no coverage on the settled final
    /// frame — including a whole phrase that finished its own swoosh during
    /// a pause longer than it (grace + reach + retract + fade, ~1.7 s at the
    /// 900 ms rest), which is the engine's law and not the defect.
    final_gaps: Vec<u16>,
    /// The owner's shape, and the assertion: a dark typed cell INSIDE a lit
    /// line — a burst partly lit, or a boundary space dark between two lit
    /// bursts. A REPRODUCTION when non-empty.
    holes: Vec<u16>,
    /// The same on every settled post-burst frame, on every take (a hole
    /// that opens and closes before the final settle is still a hole): the
    /// keys of the burst just typed that are on glass, dark while any of
    /// them is lit — `(burst, columns)`.
    post_holes: Vec<(usize, Vec<u16>)>,
    report: String,
}

/// The absolute column span of every burst, and the boundary spaces between
/// them, on the finished line.
fn spans() -> (Vec<(u16, u16)>, Vec<u16>) {
    let mut spans = Vec::new();
    let mut spaces = Vec::new();
    let mut col = COL0;
    for (i, b) in BURSTS.iter().enumerate() {
        let end = col + b.len() as u16;
        spans.push((col, end));
        if i + 1 < BURSTS.len() {
            spaces.push(end);
            col = end + 1;
        }
    }
    (spans, spaces)
}

fn lit(census: &[Column], col: u16) -> bool {
    census
        .get(usize::from(col - COL0))
        .is_some_and(|(owner, cov)| owner.is_some() && *cov > 0)
}

/// The holes of a final census: the dark cells of a burst any cell of which
/// is lit, and a boundary space dark between two bursts both lit.
fn holes(census: &[Column]) -> Vec<u16> {
    let (spans, spaces) = spans();
    let mut out = Vec::new();
    for &(c0, c1) in &spans {
        if (c0..c1).any(|c| lit(census, c)) {
            out.extend((c0..c1).filter(|&c| !lit(census, c)));
        }
    }
    for (i, &sp) in spaces.iter().enumerate() {
        let (a0, a1) = spans[i];
        let (b0, b1) = spans[i + 1];
        if (a0..a1).any(|c| lit(census, c)) && (b0..b1).any(|c| lit(census, c)) && !lit(census, sp)
        {
            out.push(sp);
        }
    }
    out.sort_unstable();
    out
}

/// The holes among the keys of one burst just typed (`cols`, its glyphs
/// and its attached boundary space): dark cells while any of them is lit.
fn burst_holes(census: &[Column], cols: std::ops::Range<u16>) -> Vec<u16> {
    if cols.clone().any(|c| lit(census, c)) {
        cols.filter(|&c| !lit(census, c)).collect()
    } else {
        Vec::new()
    }
}

/// One take: the four bursts, the pauses, the census after every burst
/// (its last key's landing frame), before every next burst (the frame
/// before its first key), on the last key's landing frame and `SETTLE_MS`
/// later.
fn take(composer: Composer, seam: Seam, placement: Placement, pause: u64) -> Take {
    take_at(composer, seam, placement, pause, KEY_MS)
}

/// [`take`] at a key cadence of `key_ms`; with a settled post-burst census
/// (past the composer's split, inside the grace) after every burst.
fn take_at(composer: Composer, seam: Seam, placement: Placement, pause: u64, key_ms: u64) -> Take {
    let mut h = Host::new(composer, seam);
    let cc = composer.cc();
    let label = format!(
        "{:<24} cad={:>3} seam={:<7} {:<16} pause={:>4}",
        composer.label(),
        key_ms,
        format!("{seam:?}"),
        format!("{placement:?}"),
        pause
    );
    let mut report = String::new();
    let mut typed = 0usize;
    let mut post_holes = Vec::new();
    let mut lic_seen = 0u64;
    let mut dec_seen = 0u64;
    let settle = SETTLE_MS.max(cc.map_or(0, |s| s.split_ms) + 100);
    for (i, burst) in BURSTS.iter().enumerate() {
        let mut keys: Vec<char> = burst.chars().collect();
        if i + 1 < BURSTS.len() && placement == Placement::SpaceBeforePause {
            keys.push(' ');
        }
        if i > 0 && placement == Placement::SpaceAfterPause {
            keys.insert(0, ' ');
        }
        let burst_col0 = COL0 + typed as u16;
        for (j, &ch) in keys.iter().enumerate() {
            let first = j == 0;
            let last = j + 1 == keys.len();
            let ms = if first && i > 0 { pause } else { key_ms };
            h.key_after(ch, ms, last);
            typed += 1;
        }
        let _ = write!(report, " b{}@{}:{:?}", i + 1, h.ms(), h.gaps(typed, false));
        // THE POST-BURST CENSUS, ON EVERY TAKE (2026-09-21, the review): a
        // hole that opens after a burst and closes before the final settle
        // is still a hole. A Claude Code take settles past its split; an
        // Ink or zsh take settles inside the grace and no later than the
        // pause's mid-point, so an idle-repainting composer's half-way
        // redraw keeps its instant and the next burst's key its own — and
        // its LAST burst is read by the final settle below, at the instant
        // it always was. Only the keys ON GLASS are read: a late composer's
        // owed repaint (`Shape::late_space_ms`) is not a hole yet.
        let last_burst = i + 1 == BURSTS.len();
        if cc.is_some() || !last_burst {
            let post = if cc.is_some() {
                settle
            } else {
                SETTLE_MS.min(pause / 2)
            };
            h.idle(post);
            let on_glass = if cc.is_some() {
                typed
            } else {
                typed.saturating_sub(h.pending)
            };
            let ph = burst_holes(&h.census(), burst_col0..COL0 + on_glass as u16);
            let tally = h.glow.admission_tally();
            let _ = write!(
                report,
                " post{}@{}:{:?} lic+{} dec+{}",
                i + 1,
                h.ms(),
                ph,
                tally.licensed - lic_seen,
                tally.declined - dec_seen
            );
            lic_seen = tally.licensed;
            dec_seen = tally.declined;
            if !ph.is_empty() {
                post_holes.push((i + 1, ph));
            }
        }
        if i + 1 < BURSTS.len() {
            h.pause_train(pause);
            let _ = write!(
                report,
                " pre{}@{}:{:?}",
                i + 2,
                h.ms(),
                h.gaps(typed, false)
            );
        }
    }
    if cc.is_none() {
        let landing = h.gaps(typed, false);
        let _ = write!(report, " land@{}:{:?}", h.ms(), landing);
        // Settle inside the grace — and past a late composer's owed
        // repaint, else the census reads text that is not on glass yet as
        // a hole. (A Claude Code take settled after its last burst above.)
        h.idle(SETTLE_MS.max(composer.shape().late_space_ms + 100));
    }
    let final_gaps = h.gaps(typed, true);
    let holes = holes(&h.census());
    let _ = write!(
        report,
        " final@{}:{:?} holes:{:?}{}",
        h.ms(),
        final_gaps,
        holes,
        if holes.is_empty() {
            String::new()
        } else {
            format!(" [{}]", glyphs_at(&holes))
        }
    );
    let tally = h.glow.admission_tally();
    let inflight = h.glow.in_flight_tally();
    let _ = write!(
        report,
        " | licensed={} declined={} ({:?}) inflight_licensed={} inflight_forgotten={} \
         park_returns={} park_flushed={} retired={} hidden_frames={} held={} mid_bracket={}",
        tally.licensed,
        tally.declined,
        tally.last_decline_reason,
        inflight.licensed,
        inflight.forgotten,
        inflight.park_returns,
        inflight.park_flushed,
        h.glow.v2_status().map_or(0, |s| s.retired),
        h.hidden_frames,
        h.held_frames,
        h.mid_bracket_frames
    );
    let _ = write!(report, " | cohorts={:?}", h.cohort_rows());
    if cc.is_some() {
        let _ = write!(report, " | ring:{}", h.ring_notes());
    }
    Take {
        label,
        final_gaps,
        holes,
        post_holes,
        report,
    }
}

/// The mark of one take: `REPRO` for a hole on the final or a post-burst
/// frame, `gone ` for a take whose only dark cells are whole phrases that
/// left on their own clock.
fn mark(t: &Take) -> &'static str {
    if !t.holes.is_empty() || !t.post_holes.is_empty() {
        "REPRO "
    } else if !t.final_gaps.is_empty() {
        "gone  "
    } else {
        "  ok  "
    }
}

/// The sweep for one composer and seam: both placements, every pause, every
/// take printed. `REPRO` marks a take with a hole; `gone ` a take whose only
/// dark cells are whole phrases that left on their own clock.
fn sweep(composer: Composer, seam: Seam) -> Vec<Take> {
    let mut takes = Vec::new();
    for placement in [Placement::SpaceBeforePause, Placement::SpaceAfterPause] {
        for &pause in &PAUSES {
            let t = take(composer, seam, placement, pause);
            println!("{} {}{}", mark(&t), t.label, t.report);
            takes.push(t);
        }
    }
    takes
}

/// One assertion per take, after every sweep of the test has printed: no
/// hole on the settled final frame, and none on any settled post-burst
/// frame either — a hole that opened after a burst and closed before the
/// final settle is still a hole (2026-09-21, the review).
fn assert_takes(takes: &[Take]) {
    for t in takes {
        assert!(
            t.holes.is_empty(),
            "{}: the settled final frame has holes in the lit line at {} —{}",
            t.label,
            glyphs_at(&t.holes),
            t.report
        );
        assert!(
            t.post_holes.is_empty(),
            "{}: a settled post-burst frame has holes in the lit line — {:?} —{}",
            t.label,
            t.post_holes,
            t.report
        );
    }
}

fn ink(shape: Shape) -> Composer {
    Composer::Ink(shape)
}

// ---- (1) the bare gesture through both composers, both seams ----------

#[test]
fn phrases_typed_in_bursts_through_zsh_keep_every_boundary_space_lit() {
    let mut takes = sweep(Composer::Zsh, Seam::Plain);
    takes.extend(sweep(Composer::Zsh, Seam::Classed));
    assert_takes(&takes);
}

#[test]
fn phrases_typed_in_bursts_through_an_ink_repaint_keep_every_boundary_space_lit() {
    let mut takes = sweep(ink(Shape::default()), Seam::Plain);
    takes.extend(sweep(ink(Shape::default()), Seam::Classed));
    assert_takes(&takes);
}

// ---- (2) what a real Ink composer does that inkish.py does not ----------

/// (i) Claude Code's own inverse-video cursor cell after the text, redrawn
/// with the whole line on every key — the DEC caret shown at the caret.
#[test]
fn an_ink_composer_with_its_own_cursor_cell_keeps_every_boundary_space_lit() {
    let takes = sweep(
        ink(Shape {
            cursor_cell: true,
            ..Shape::default()
        }),
        Seam::Classed,
    );
    assert_takes(&takes);
}

/// (i′) …and with the DEC caret never shown again: the cursor cell is the
/// only caret on glass.
#[test]
fn an_ink_composer_with_a_hidden_caret_keeps_every_boundary_space_lit() {
    let takes = sweep(
        ink(Shape {
            cursor_cell: true,
            hidden_caret: true,
            ..Shape::default()
        }),
        Seam::Classed,
    );
    // A caret never shown licenses nothing: the whole line stays dark, and
    // that is no hole — but it must be the WHOLE line, on every take. A
    // composer that never shows its caret is not the owner's, whose band
    // was lit; a take that lit part of the line under a hidden caret would
    // be a licence minted with no caret to license it. Asserted
    // explicitly, so the sweep cannot pass vacuously (2026-09-21).
    assert_takes(&takes);
    let dark = takes
        .iter()
        .filter(|t| t.final_gaps.len() == line().len())
        .count();
    println!(
        "hidden caret: {} of {} takes lit nothing at all",
        dark,
        takes.len()
    );
    assert_eq!(
        dark,
        takes.len(),
        "the licence-less composer lit part of a line —\n{}",
        takes
            .iter()
            .filter(|t| t.final_gaps.len() != line().len())
            .map(|t| format!("{}{}", t.label, t.report))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// (ii) keys coalesced into one repaint under load — two, and the three
/// the owner's ring showed licensed as one key.
#[test]
fn an_ink_composer_coalescing_keys_into_one_repaint_keeps_every_boundary_space_lit() {
    let mut takes = Vec::new();
    for n in [2, 3] {
        takes.extend(sweep(
            ink(Shape {
                cursor_cell: true,
                coalesce: n,
                ..Shape::default()
            }),
            Seam::Classed,
        ));
    }
    assert_takes(&takes);
}

/// (iii) a content-free repaint half-way through every pause.
#[test]
fn an_ink_composer_repainting_mid_pause_keeps_every_boundary_space_lit() {
    let takes = sweep(
        ink(Shape {
            cursor_cell: true,
            idle_repaint: true,
            ..Shape::default()
        }),
        Seam::Classed,
    );
    assert_takes(&takes);
}

/// (iv) the space's repaint lands late — the composer doing word-boundary
/// work — either side of the typed hint's 250 ms freshness, with nothing
/// after it at a burst's end to rescue it.
#[test]
fn an_ink_composer_whose_space_repaints_late_keeps_every_boundary_space_lit() {
    let mut takes = Vec::new();
    for late in [150, 240, 300, 500, 800] {
        takes.extend(sweep(
            ink(Shape {
                cursor_cell: true,
                late_space_ms: late,
                ..Shape::default()
            }),
            Seam::Classed,
        ));
    }
    assert_takes(&takes);
}

/// (v) the row sampled MID-REWRITE with one letter blank, the next burst's
/// first key 50 ms later — inside the partial release's melt, its own cell
/// laid before the letter is back — and the rewrite completing after it.
/// Before 2026-09-21 the content witness released the run for the one
/// blanked glyph and took every never-armed cell of the run — its SPACES —
/// with it onto the retract (`witness.rs`, the never-armed arm), and the
/// key typed inside the melt changed the cohort's cell count, so the
/// identical redraw's restore was refused by count (`Ribbon::
/// restore_releases`): the letters stood and every boundary space typed so
/// far went dark — the owner's exact shape.
#[test]
fn an_ink_composer_sampled_mid_rewrite_with_a_key_inside_the_melt_keeps_every_boundary_space_lit() {
    let takes = sweep(
        ink(Shape {
            cursor_cell: true,
            mid_rewrite_blank: true,
            ..Shape::default()
        }),
        Seam::Classed,
    );
    assert_takes(&takes);
}

// ---- (3) Claude Code's own bytes, mid-turn ------------------------------

/// The Claude Code sweep for one `(spinner, cadence)`: coalesce × chunk ×
/// split × pause × placement, every take one printed line, buffered so
/// parallel tests never interleave. `Pty1024` is swept only at coalesce 1,
/// split 0 (every recorded frame fits one chunk, so it is `Whole` by
/// construction — kept as the proof).
fn cc_sweep(spinner: bool, key_ms: u64, chunks: &[Chunk]) -> Vec<Take> {
    let mut takes = Vec::new();
    let mut lines = String::new();
    for coalesce in [1, 2, 3] {
        for &chunk in chunks {
            for &split_ms in &CC_SPLITS {
                if chunk == Chunk::Pty1024 && (coalesce > 1 || split_ms > 0) {
                    continue;
                }
                let shape = CcShape {
                    spinner,
                    coalesce,
                    chunk,
                    split_ms,
                };
                for placement in [Placement::SpaceBeforePause, Placement::SpaceAfterPause] {
                    for &pause in &CC_PAUSES {
                        let t = take_at(
                            Composer::ClaudeCode(shape),
                            Seam::Classed,
                            placement,
                            pause,
                            key_ms,
                        );
                        let _ = writeln!(lines, "{} {}{}", mark(&t), t.label, t.report);
                        takes.push(t);
                    }
                }
            }
        }
    }
    print!("{lines}");
    takes
}

/// One assertion per Claude Code take: no hole on the settled final frame
/// and none on any settled post-burst frame.
fn assert_cc_takes(takes: &[Take]) {
    let repro: Vec<&Take> = takes
        .iter()
        .filter(|t| !t.holes.is_empty() || !t.post_holes.is_empty())
        .collect();
    println!(
        "claude code: {} of {} takes reproduce a hole",
        repro.len(),
        takes.len()
    );
    for t in &repro {
        println!("REPRO {}{}", t.label, t.report);
    }
    assert!(
        repro.is_empty(),
        "{} of {} Claude Code takes have a hole in the lit line (final {:?}, post-burst {:?}) — the first: {}{}",
        repro.len(),
        takes.len(),
        repro.iter().map(|t| &t.holes).collect::<Vec<_>>(),
        repro.iter().map(|t| &t.post_holes).collect::<Vec<_>>(),
        repro[0].label,
        repro[0].report
    );
}

/// The recorded bytes, the composer idle (no turn): the letter, space and
/// mid-pause frames alone, then coalesced, chunked and split.
#[test]
fn claude_code_s_recorded_frames_idle_keep_every_boundary_space_lit_at_10cps() {
    let takes = cc_sweep(
        false,
        CC_CADENCES[0],
        &[Chunk::Whole, Chunk::Pty64, Chunk::Pty1024],
    );
    assert_cc_takes(&takes);
}

#[test]
fn claude_code_s_recorded_frames_idle_keep_every_boundary_space_lit_at_25cps() {
    let takes = cc_sweep(false, CC_CADENCES[1], &[Chunk::Whole, Chunk::Pty64]);
    assert_cc_takes(&takes);
}

/// The owner's moment: a turn running, the spinner frame every 100 ms on
/// `ROW - 3` between the keys and their frames.
#[test]
fn claude_code_s_recorded_frames_mid_turn_keep_every_boundary_space_lit_at_10cps() {
    let takes = cc_sweep(
        true,
        CC_CADENCES[0],
        &[Chunk::Whole, Chunk::Pty64, Chunk::Pty1024],
    );
    assert_cc_takes(&takes);
}

#[test]
fn claude_code_s_recorded_frames_mid_turn_keep_every_boundary_space_lit_at_25cps() {
    let takes = cc_sweep(true, CC_CADENCES[1], &[Chunk::Whole, Chunk::Pty64]);
    assert_cc_takes(&takes);
}

// ---- the body, for the owner's screenshot --------------------------------

/// The per-column body extents on the settled final frame of one take —
/// the cursor-cell Ink composer, the space before a 1200 ms pause — so the
/// thin-vs-tall bodies in the owner's screenshot can be compared. A REPORT,
/// not a test: it asserts nothing, so it is ignored by default and run by
/// name with `--ignored` when the numbers are wanted.
#[test]
#[ignore = "a report: prints one take's body extents and asserts nothing — run with --ignored to read it"]
fn the_final_frame_s_body_extents_are_printed_for_one_take() {
    let mut h = Host::new(
        ink(Shape {
            cursor_cell: true,
            ..Shape::default()
        }),
        Seam::Classed,
    );
    for (i, burst) in BURSTS.iter().enumerate() {
        let mut keys: Vec<char> = burst.chars().collect();
        if i + 1 < BURSTS.len() {
            keys.push(' ');
        }
        for (j, &ch) in keys.iter().enumerate() {
            let ms = if j == 0 && i > 0 { 1200 } else { KEY_MS };
            h.key_after(ch, ms, j + 1 == keys.len());
        }
        if i + 1 < BURSTS.len() {
            h.pause_train(1200);
        }
    }
    h.idle(SETTLE_MS);
    println!(
        "body at +{} ms, per column (col glyph cov up/ch dn/ch):",
        h.ms()
    );
    for (col, glyph, cov, up, dn) in h.body() {
        println!("  {col:>2} {glyph:?} cov={cov:>3} up={up:.2} dn={dn:.2}");
    }
    println!("cohorts: {:?}", h.cohort_rows());
    println!("slabs_per_cell={}", h.ribbon().slabs_per_cell());
}

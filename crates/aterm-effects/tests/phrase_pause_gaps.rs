// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A phrase boundary's space keeps its light.

use crate::common::{
    FrameHold, caret_sample, class_of, note_blink, observe_caret_row, observe_ribbon_rows,
    rainbow_cfg,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::cursor_glow::{AdmissionPhase, CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::ribbon::{Phase, Ribbon};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

/// The grid: 63 rows, the composer on row 61.
const ROWS: usize = 63;
const COLS: usize = 80;
const CW: usize = 8;
const CH: usize = 16;
const ROW: u16 = 61;
/// Two-cell composer prefix column (`› ` for Ink, `% ` for zsh).
const COL0: u16 = 2;
/// The four phrases, in order; the scenario places the boundary spaces.
const BURSTS: [&str; 4] = ["Adding", "Fable.", "HEY!", "zoom out. git pull"];
/// 12 cps.
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
/// The Ink-shaped composer's split between the redraw's head and its tail.
const INK_SPLIT_MS: u64 = 30;
/// The thinking pauses swept, ms — either side of the 900 ms phrase rest.
const PAUSES: [u64; 7] = [300, 700, 950, 1200, 1500, 2000, 3000];
/// Delay after the last key before reading the final census.
const SETTLE_MS: u64 = 200;

/// The turn's spinner cadence.
const SPINNER_MS: u64 = 100;
/// The recorded status-row erase, ~0.95 s after the last key of a pause.
const IDLE_FRAME_MS: u64 = 950;
/// The spinner's glyphs, cycled one per frame.
const SPINNER: [char; 6] = ['✻', '✽', '✶', '✳', '✢', '·'];
/// How long the turn has run when a take starts, for the spinner's elapsed count.
const TURN_BASE_S: u64 = 30;
/// PTY chunk gap of a starved reader thread.
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

/// The config under test: rainbow kitty, tall, intensity 1.00, dark.
fn cfg() -> GlowConfig {
    GlowConfig {
        intensity: 1.0,
        ..rainbow_cfg()
    }
}

fn line() -> String {
    BURSTS.join(" ")
}

/// Ink composer options beyond the default redraw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Shape {
    /// Draw an inverse-video SPACE cell right after the text on every redraw.
    cursor_cell: bool,
    /// Never show the DEC caret again after the first hide.
    hidden_caret: bool,
    /// Keys per repaint under load (`0` or `1` is one repaint per key).
    coalesce: usize,
    /// A content-free repaint half-way through every pause.
    idle_repaint: bool,
    /// Delay in ms before a space's repaint lands.
    late_space_ms: u64,
    /// Whether a line rewrite blanks the last letter mid-pause before the next burst.
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
}

impl Chunk {
    fn bytes(self) -> usize {
        match self {
            Self::Whole => 0,
            Self::Pty64 => 64,
        }
    }
}

/// Claude Code's recorded byte shapes and the mid-turn dials around them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CcShape {
    /// Whether a turn spinner frame runs on `ROW - 3` every [`SPINNER_MS`].
    spinner: bool,
    /// Keys per frame (`0` or `1` is one frame per key).
    coalesce: usize,
    /// How the frame's bytes arrive.
    chunk: Chunk,
    /// Delay in ms between a flushing key and its frame.
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
    /// `note_typed_glyph(now, 1, shifted, class)` as called by `app_input.rs`.
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

/// Column census entry: `(owner identity, planned coverage)`.
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
    /// Caret index as drawn by the last key frame.
    drawn: usize,
    /// A chunked write in progress: the train runs, no other frame lands.
    writing: bool,
    /// Index into `text` of the letter blanked by an in-progress rewrite.
    blank_idx: Option<usize>,
    hold: FrameHold,
    /// Presents the hold skipped (the frame hold never ran).
    held_frames: usize,
    /// Presents that sampled a bracket still open (past the cap).
    mid_bracket_frames: usize,
}

impl Host {
    fn new(composer: Composer, seam: Seam) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = crate::common::quads_glow();
        glow.note_pane_columns(0, COLS);
        match composer {
            Composer::Zsh => term.process(format!("\x1b[{};1H% ", ROW + 1).as_bytes()),
            Composer::Ink(_) => term.process(b"\x1b[?1049h\x1b[2J"),
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
        let mut h = Self {
            term,
            glow,
            cfg: cfg(),
            g: crate::common::geom(ROWS, COLS, CW, CH),
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
            hold: FrameHold::default(),
            held_frames: 0,
            mid_bracket_frames: 0,
        };
        match composer {
            Composer::Zsh => h.frame(),
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

    /// Feed `bytes` into the terminal at `self.now`.
    fn process(&mut self, bytes: &[u8]) {
        self.term.process_at(
            bytes,
            ClockReading {
                monotonic: self.now,
                wall_ms: None,
            },
        );
    }

    /// Sample the terminal and advance the glow engine by one frame.
    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        if cur.is_none() {
            self.hidden_frames += 1;
        }
        note_blink(&self.term, &mut self.glow, &mut self.blink_seen, self.now);
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
    }

    /// Run the host's sync-output present rule (`app_render.rs`, SYNC-1).
    fn present(&mut self) {
        let active = self.term.modes().synchronized_output();
        if self.hold.withholds(&self.term, self.now) {
            self.held_frames += 1;
            return;
        }
        if active {
            self.mid_bracket_frames += 1;
        }
        self.frame();
    }

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

    /// Advance 16 ms frames up to `t`, running due composer jobs along the way.
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

    fn idle(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        self.idle_to(t);
    }

    /// Schedule the next event `ms` after the last and advance frames up to it.
    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        let t = t.max(self.now);
        self.now = t;
        self.last_event = t;
    }

    /// Perform an Ink composer redraw with the caret at `self.caret`.
    fn ink_redraw(&mut self) {
        let s = self.composer.shape();
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

    /// Position at `(row, col)` via home, carriage return, CUF, and CUD, then write `text`.
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

    /// Build the key frame for all pending Claude Code keys.
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

    /// Build the turn spinner frame on `ROW - 3`.
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

    /// Build the recorded mid-pause status-row erase frame.
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

    /// Advance the frame train by `ms` without running composer jobs.
    fn advance(&mut self, ms: u64) {
        let t = self.now + Duration::from_millis(ms);
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.step();
        }
        self.now = t;
    }

    /// Write `frame` to the terminal, whole or in PTY chunks.
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
                let (shifted, class) = class_of(ch);
                self.glow.note_typed_glyph(self.now, 1, shifted, class);
            }
        }
    }

    /// Type `ch` `ms` after the last event and echo it per the composer shape.
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
                    let d = self.now + Duration::from_millis(s.late_space_ms);
                    self.defer_until = Some(self.defer_until.map_or(d, |e| e.max(d)));
                }
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
                        self.cc_frame_at = Some(self.now + Duration::from_millis(s.split_ms));
                    }
                }
            }
        }
    }

    /// Run the pause between bursts up to the frame before its end.
    fn pause_train(&mut self, ms: u64) {
        let s = self.composer.shape();
        if s.idle_repaint && ms > 2 * FRAME_MS {
            let mid = self.last_event + Duration::from_millis(ms / 2);
            self.idle_to(mid);
            self.now = self.now.max(mid);
            self.ink_redraw();
        }
        if s.mid_rewrite_blank && ms > MID_REWRITE_KEY_MS + FRAME_MS {
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

    /// Typed columns lacking an owner (or coverage when `need_cov` is set).
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

    /// Format declined and non-key-licensed admission log entries.
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
    /// Typed columns lacking an owner or coverage on the settled final frame.
    final_gaps: Vec<u16>,
    /// Dark typed cells inside a lit line.
    holes: Vec<u16>,
    /// Dark cells among the keys of a burst on settled post-burst frames.
    post_holes: Vec<(usize, Vec<u16>)>,
    report: String,
}

/// Column spans of each burst and the boundary spaces between them.
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

/// Dark cells within a partially lit burst or at a boundary space between lit bursts.
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

/// Dark cells within `cols` when at least one cell in `cols` is lit.
fn burst_holes(census: &[Column], cols: std::ops::Range<u16>) -> Vec<u16> {
    if cols.clone().any(|c| lit(census, c)) {
        cols.filter(|&c| !lit(census, c)).collect()
    } else {
        Vec::new()
    }
}

/// Run one four-burst take at the default key cadence.
fn take(composer: Composer, seam: Seam, placement: Placement, pause: u64) -> Take {
    take_at(composer, seam, placement, pause, KEY_MS)
}

/// Run one four-burst take at `key_ms` cadence.
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

/// Status tag for `t` (`REPRO `, `gone  `, or `  ok  `).
fn mark(t: &Take) -> &'static str {
    if !t.holes.is_empty() || !t.post_holes.is_empty() {
        "REPRO "
    } else if !t.final_gaps.is_empty() {
        "gone  "
    } else {
        "  ok  "
    }
}

/// Sweep both placements and all pauses for each `(composer, seam)` pair.
fn sweep(runs: &[(Composer, Seam)]) -> Vec<Take> {
    let mut grid = Vec::new();
    for &(composer, seam) in runs {
        for placement in [Placement::SpaceBeforePause, Placement::SpaceAfterPause] {
            for &pause in &PAUSES {
                grid.push((composer, seam, placement, pause));
            }
        }
    }
    let takes = run_takes(&grid, |&(composer, seam, placement, pause)| {
        take(composer, seam, placement, pause)
    });
    for t in &takes {
        println!("{} {}{}", mark(t), t.label, t.report);
    }
    takes
}

/// Run `f` across `grid` in parallel, preserving order.
fn run_takes<T: Sync>(grid: &[T], f: impl Fn(&T) -> Take + Sync) -> Vec<Take> {
    crate::common::par_map(grid, f)
}

/// Assert that no take in `takes` has final or post-burst holes.
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

#[test]
fn phrases_typed_in_bursts_keep_every_boundary_space_lit() {
    let cell = Shape {
        cursor_cell: true,
        ..Shape::default()
    };
    let mut runs = vec![
        (Composer::Zsh, Seam::Plain),
        (Composer::Zsh, Seam::Classed),
        (ink(Shape::default()), Seam::Plain),
        (ink(Shape::default()), Seam::Classed),
        (ink(cell), Seam::Classed),
        (
            ink(Shape {
                idle_repaint: true,
                ..cell
            }),
            Seam::Classed,
        ),
        (
            ink(Shape {
                mid_rewrite_blank: true,
                ..cell
            }),
            Seam::Classed,
        ),
    ];
    for n in [2, 3] {
        runs.push((
            ink(Shape {
                coalesce: n,
                ..cell
            }),
            Seam::Classed,
        ));
    }
    for late in [150, 240, 300, 500, 800] {
        runs.push((
            ink(Shape {
                late_space_ms: late,
                ..cell
            }),
            Seam::Classed,
        ));
    }
    assert_takes(&sweep(&runs));
}

#[test]
fn an_ink_composer_with_a_hidden_caret_keeps_every_boundary_space_lit() {
    let takes = sweep(&[(
        ink(Shape {
            cursor_cell: true,
            hidden_caret: true,
            ..Shape::default()
        }),
        Seam::Classed,
    )]);
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

/// Run the 36-take pairwise Claude Code sweep across splits, coalescing, pauses, and chunks.
fn cc_sweep() -> Vec<Take> {
    let mut grid = Vec::new();
    for (s, &split_ms) in CC_SPLITS.iter().enumerate() {
        for (c, coalesce) in [1, 2, 3].into_iter().enumerate() {
            for (p, &pause) in CC_PAUSES.iter().enumerate() {
                let odd = |n: usize| n % 2 == 1;
                let shape = CcShape {
                    spinner: odd(s + c),
                    coalesce,
                    chunk: if odd(s + p) {
                        Chunk::Pty64
                    } else {
                        Chunk::Whole
                    },
                    split_ms,
                };
                let key_ms = CC_CADENCES[usize::from(odd(c + p))];
                let placement = if odd(s + c + p) {
                    Placement::SpaceAfterPause
                } else {
                    Placement::SpaceBeforePause
                };
                grid.push((shape, placement, pause, key_ms));
            }
        }
    }
    let takes = run_takes(&grid, |&(shape, placement, pause, key_ms)| {
        take_at(
            Composer::ClaudeCode(shape),
            Seam::Classed,
            placement,
            pause,
            key_ms,
        )
    });
    let mut lines = String::new();
    for t in &takes {
        let _ = writeln!(lines, "{} {}{}", mark(t), t.label, t.report);
    }
    print!("{lines}");
    takes
}

/// Assert that no Claude Code take has final or post-burst holes.
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

#[test]
fn claude_code_s_recorded_frames_keep_every_boundary_space_lit() {
    assert_cc_takes(&cc_sweep());
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE WALK CENSUS HOST** (2026-09-27) — shared by
//! `tests/walk_continuity_census.rs` (the sweep) and the reproductions it
//! turned up (each declares `mod walk_census;`). The frame seam is
//! `tests/claude_wrap_band.rs`'s LOCK A; [`Shell`] is a readline-shaped line
//! editor on the primary screen, folding its line at the margin as the
//! program behind it does ([`Fold`]); [`Snap::judge`] reads one frame's text
//! cells in walk order and flags every discontinuity the walk's law
//! forbids; [`recolours`] reads the same cells across frames.
#![allow(dead_code)]

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{
    CursorGlow, Geom, GlowConfig, GlowStyle, InsertWidth, ProbeTrust,
};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::{WALK_FAST_CELLS, WALK_LAY_RATE};
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

pub const CW: usize = 8;
pub const CH: usize = 16;
pub const ROWS: usize = 24;
/// A column is LIT at this planned coverage (the glass census's 20/255).
pub const LIT_COV: u8 = 20;
/// `app_render.rs`'s `BLINK_RECENT_MAX`.
pub const BLINK_RECENT: Duration = Duration::from_secs(1);
/// Two `t`s closer than this are one number.
pub const EPS: f32 = 1e-3;
/// The walk's fast step.
pub const FAST: f32 = 1.0 / WALK_FAST_CELLS;
/// A flagged run standing this many frames or more is a defect; fewer is
/// reported as a transient.
pub const STANDING_FRAMES: usize = 3;

pub fn cfg() -> GlowConfig {
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

/// The host's `typed_class_for` and `glyph_shifted && !spacebar`.
pub fn class_of(ch: char) -> (bool, TypedClass) {
    match ch {
        ' ' => (false, TypedClass::Space),
        '!' => (true, TypedClass::Bang),
        c if c.is_uppercase() => (true, TypedClass::Capital),
        c => ("~!@#$%^&*()_+{}|:\"<>?".contains(c), TypedClass::Glyph),
    }
}

/// What the census flags between two text-adjacent lit cells, or at a
/// dark text cell between lit ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Flag {
    /// A backward colour step between text-adjacent lit cells.
    Back,
    /// The same stop twice.
    Repeat,
    /// More than one walk step, or a step that is neither walk step.
    Jump,
    /// A `1/16` step right of a `1/36` one on the same line: the fast leg
    /// restarted mid-line.
    Restart,
    /// A `1/36` step inside the line's first sixteen cells: the line did
    /// not start its walk where it started.
    NoFastLeg,
    /// A dark text cell between lit ones.
    Hole,
}

/// One text cell on one frame.
#[derive(Clone, Copy, Debug)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
    pub lit: bool,
    pub t: Option<f32>,
    /// Whether a not-leaving cell owns the position.
    pub live: bool,
}

/// One frame of the census: the line's text cells in walk order.
#[derive(Clone, Debug)]
pub struct Snap {
    pub ms: u64,
    pub label: String,
    pub text: String,
    /// Text index the line's walk started at (a line kill restarts it).
    pub start: usize,
    pub pos: Vec<Pos>,
    /// Indices `i` after which `pos[i + 1]` is NOT the walk's next cell (a
    /// composer's row end: its rows are judged apart).
    pub breaks: Vec<usize>,
    /// `(flag, text index)`: for a step flag, the index of the pair's left
    /// cell.
    pub flags: Vec<(Flag, usize)>,
}

impl Snap {
    pub fn judge(&mut self) {
        let n = self.pos.len();
        let mut a = 0;
        let mut ends: Vec<usize> = self.breaks.iter().map(|&b| b + 1).collect();
        ends.push(n);
        for b in ends {
            let b = b.min(n);
            if a < b {
                self.judge_segment(a, b);
            }
            a = b;
        }
    }

    /// The flags of the walk segment `a..b` of [`Snap::pos`].
    fn judge_segment(&mut self, a: usize, b: usize) {
        let lit: Vec<usize> = (a..b).filter(|&i| self.pos[i].lit).collect();
        if let (Some(&l), Some(&r)) = (lit.first(), lit.last()) {
            for i in l..=r {
                if !self.pos[i].lit {
                    self.flags.push((Flag::Hole, i));
                }
            }
        }
        // Whether a step at the lay rate has been read to the left on this
        // line: a fast step after it is the fast leg restarting mid-line.
        let mut laying = false;
        let fast_leg = (a..b).contains(&self.start);
        for i in a..b.saturating_sub(1) {
            let (p, q) = (self.pos[i], self.pos[i + 1]);
            if !(p.lit && q.lit) {
                continue;
            }
            let (Some(tp), Some(tq)) = (p.t, q.t) else {
                continue;
            };
            let dt = tq - tp;
            let flag = if dt < -EPS {
                Some(Flag::Back)
            } else if dt.abs() <= EPS {
                Some(Flag::Repeat)
            } else if dt > FAST + EPS {
                Some(Flag::Jump)
            } else if (dt - FAST).abs() <= EPS && laying {
                Some(Flag::Restart)
            } else if (dt - WALK_LAY_RATE).abs() <= EPS
                && fast_leg
                && i >= self.start
                && i + 1 - self.start <= WALK_FAST_CELLS as usize
            {
                Some(Flag::NoFastLeg)
            } else if (dt - FAST).abs() > EPS && (dt - WALK_LAY_RATE).abs() > EPS {
                Some(Flag::Jump)
            } else {
                None
            };
            if (dt - WALK_LAY_RATE).abs() <= EPS {
                laying = true;
            }
            if let Some(f) = flag {
                self.flags.push((f, i));
            }
        }
    }

    /// `#` lit and live, `+` lit by a leaving cell, `.` dark — per text cell.
    pub fn map(&self) -> String {
        self.pos
            .iter()
            .map(|p| match (p.lit, p.live) {
                (true, true) => '#',
                (true, false) => '+',
                _ => '.',
            })
            .collect()
    }

    pub fn ts(&self) -> String {
        self.pos
            .iter()
            .enumerate()
            .filter(|(_, p)| p.lit)
            .map(|(i, p)| format!("{i}:{:.4}", p.t.unwrap_or(f32::NAN)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The frame seam, the terminal and the engine.
pub struct Host {
    pub term: Terminal,
    pub glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    pub t0: Instant,
    pub now: Instant,
    next_frame: Instant,
    frame_ms: u64,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    above: Vec<char>,
    below: Vec<char>,
    blink: u64,
    last_blink: Option<Instant>,
    scroll: Option<ContentScrollState>,
    ring_seen: u64,
    /// The GUI's real pacing: a frame when output lands, then 16 ms while
    /// the engine asks for cadence, else at its next change deadline.
    pub paced: bool,
}

impl Host {
    pub fn new(cols: usize, frame_ms: u64) -> Self {
        Self::sized(ROWS, cols, CW, CH, frame_ms)
    }

    pub fn sized(rows: usize, cols: usize, cw: usize, ch: usize, frame_ms: u64) -> Self {
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, cols);
        glow.note_pane_rows(0, rows);
        Self {
            term: Terminal::new(rows as u16, cols as u16),
            glow,
            cfg: cfg(),
            g: Geom {
                cw,
                ch,
                rows,
                cols,
                origin_x: 0,
                origin_y: 0,
                win_w: (cols * cw) as u16,
                win_h: (rows * ch) as u16,
                head: 0,
            },
            t0: now,
            now,
            next_frame: now,
            frame_ms,
            out: Vec::new(),
            row_buf: Vec::new(),
            above: Vec::new(),
            below: Vec::new(),
            blink: 0,
            last_blink: None,
            scroll: None,
            ring_seen: 0,
            paced: false,
        }
    }

    pub fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// `tests/claude_wrap_band.rs`'s LOCK A, verbatim in order.
    pub fn frame(&mut self) {
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
            self.last_blink = Some(self.now);
            self.glow.note_repaint_blink(self.now);
        }
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
        let blink_recent = self
            .last_blink
            .is_some_and(|t| self.now.saturating_duration_since(t) <= BLINK_RECENT);
        let trust = if !alt || blink_recent {
            ProbeTrust::Full
        } else {
            ProbeTrust::ContentOnly
        };
        let r = usize::from(c.row);
        self.term.row_cols_into(r, &mut self.row_buf);
        self.glow
            .observe_row_with_trust(c.row, c.col, &self.row_buf, self.now, trust);
        if r > 0 {
            self.term.row_cols_into(r - 1, &mut self.above);
        }
        let rows = self.g.rows;
        if r + 1 < rows {
            self.term.row_cols_into(r + 1, &mut self.below);
        }
        self.glow.observe_neighbor_rows(
            (r > 0).then_some(self.above.as_slice()),
            (r + 1 < rows).then_some(self.below.as_slice()),
        );
        self.glow.observe_print_anchor(self.term.print_anchor());
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &rr in &rows[..n] {
            if rr == c.row {
                continue;
            }
            self.term.row_cols_into(usize::from(rr), &mut self.row_buf);
            self.glow.observe_ribbon_row(rr, &self.row_buf);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        self.debug();
    }

    /// `WALK_CENSUS_RING=<ms>:<ms>`: every new admission verdict, and the
    /// ribbon's cohorts and cells, per frame in that window.
    fn debug(&mut self) {
        let Some((a, b)) = std::env::var("WALK_CENSUS_RING").ok().and_then(|w| {
            let (a, b) = w.split_once(':')?;
            Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?))
        }) else {
            return;
        };
        let ms = self.ms();
        if ms < a || ms > b {
            return;
        }
        for rec in self.glow.admission_log() {
            if rec.seq > self.ring_seen {
                eprintln!("  t={ms} ring: {}", rec.line(self.now));
                self.ring_seen = rec.seq;
            }
        }
        let c = self.term.cursor();
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let mut cells: Vec<(u16, u16, u32, bool, String)> = rib
            .cells()
            .iter()
            .map(|x| {
                (
                    x.row,
                    x.col,
                    x.cohort,
                    x.leaving(),
                    format!("{:.4}{:?}", x.t, x.layer),
                )
            })
            .collect();
        cells.sort_by_key(|x| (x.0, x.1));
        eprintln!(
            "  t={ms} caret=({},{}) segs={} cells={cells:?}",
            c.row,
            c.col,
            rib.plan_segments().len()
        );
        eprintln!("  t={ms} cohorts:");
        for k in rib.cohorts() {
            let n = rib.cells().iter().filter(|x| x.cohort == k.id).count();
            let lv = rib
                .cells()
                .iter()
                .filter(|x| x.cohort == k.id && x.leaving())
                .count();
            eprintln!(
                "    id={} row={} cols={}..{} anchor={} t0={:.4} d0={} wake={} aband={} phrase={} phase={:?} flow={} cells={n} leaving={lv}",
                k.id,
                k.row,
                k.col0,
                k.col1,
                k.anchor_col,
                k.t0,
                k.d0,
                k.wake,
                k.abandoned,
                k.phrase,
                k.phase,
                k.flow.is_some()
            );
        }
    }

    /// Run the frame train through every frame due at or before `t`;
    /// returns how many ran. SYNC-1: no present while a `?2026` bracket is
    /// open and dirty.
    pub fn frames_to(&mut self, t: Instant, mut each: impl FnMut(&mut Self)) {
        if self.paced {
            let fd = Duration::from_millis(16);
            loop {
                let next = if self.glow.needs_frame_cadence() {
                    Some(self.now + fd)
                } else {
                    self.glow.next_change_deadline(self.now, fd)
                };
                match next {
                    Some(n) if n <= t => {
                        self.now = n.max(self.now + Duration::from_millis(1));
                        if !self.term.sync_open_dirty() {
                            self.frame();
                            each(self);
                        }
                    }
                    _ => break,
                }
            }
            self.now = self.now.max(t);
            return;
        }
        while self.next_frame <= t {
            self.now = self.next_frame;
            if !self.term.sync_open_dirty() {
                self.frame();
                each(self);
            }
            self.next_frame = self.now + Duration::from_millis(self.frame_ms);
        }
        self.now = self.now.max(t);
    }

    /// The planned coverage per column of `row`.
    pub fn cov_row(&self, row: u16) -> Vec<u8> {
        let mut cov = vec![0u8; self.g.cols];
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        for sg in rib.plan_segments() {
            let r = ((sg.spine / self.g.ch as f32) - 0.5).floor();
            let c = (sg.x / self.g.cw as f32).floor();
            if r >= 0.0 && c >= 0.0 && r as u16 == row && (c as usize) < self.g.cols {
                let i = c as usize;
                cov[i] = cov[i].max(sg.cov);
            }
        }
        cov
    }

    /// The census of the text cells at `cells`, in walk order.
    pub fn census(&self, cells: &[(u16, u16)]) -> Vec<Pos> {
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let mut covs: Vec<(u16, Vec<u8>)> = Vec::new();
        cells
            .iter()
            .map(|&(row, col)| {
                if !covs.iter().any(|(r, _)| *r == row) {
                    covs.push((row, self.cov_row(row)));
                }
                let cov = &covs.iter().find(|(r, _)| *r == row).expect("row").1;
                let lit = cov.get(usize::from(col)).is_some_and(|&v| v >= LIT_COV);
                let live = rib
                    .cells()
                    .iter()
                    .any(|c| c.row == row && c.col == col && !c.leaving());
                Pos {
                    row,
                    col,
                    lit,
                    t: rib.field_at(row, col),
                    live,
                }
            })
            .collect()
    }
}

/// How the shell's echo reaches the glass relative to its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Echo {
    /// The echo lands this many ms after the press.
    After(u64),
}

/// How the line folds at the margin. Captured over a pty at 60 columns
/// (`TERM=xterm-256color`), the key that fills the margin echoes:
///
/// * bash 3.2 readline: `<glyph> \r`, so the caret lands at the next row's
///   column 0 on that key and the fold glyph is an ordinary same-row key;
/// * zsh 5.9 zle: `<glyph> \r\x1b[K`, then the next glyph bare, then
///   `\r<glyph><glyph>` for the one after (the row reprinted from column 0);
///
/// while the bare shape is the kernel's cooked-mode echo (`cat`, `read`
/// without `-e`): every glyph as typed, folded by the terminal's deferred
/// autowrap. Only the key ending exactly at the margin differs; a reprint
/// ([`Shell::reprint`]) or a paste crossing the margin folds by autowrap in
/// every shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    /// bash readline: `<glyph> \r` on the key that fills the margin. The
    /// default ([`Shell::new`]): the census's shell is readline-shaped.
    Readline,
    /// zsh 5.9 zle: `<glyph> \r\x1b[K`, the next glyph bare, then
    /// `\r<glyph><glyph>`.
    Zle,
    /// The kernel's cooked-mode echo: every glyph bare, the terminal's
    /// deferred autowrap folds the line on the glyph AFTER the margin's.
    Kernel,
}

impl Fold {
    /// Every fold the harness models, the default first.
    pub const ALL: [Fold; 3] = [Fold::Readline, Fold::Zle, Fold::Kernel];

    /// The fold's name in a verdict.
    pub fn name(self) -> &'static str {
        match self {
            Fold::Readline => "readline",
            Fold::Zle => "zle",
            Fold::Kernel => "kernel",
        }
    }
}

thread_local! {
    /// The fold a [`Shell`] built on this thread takes when it names none
    /// ([`with_default_fold`]).
    static DEFAULT_FOLD: std::cell::Cell<Fold> = const { std::cell::Cell::new(Fold::Readline) };
}

/// Every shape of the fold: `f` runs with every [`Shell`] that names no fold
/// ([`Shell::folding`]) folding as `fold` does, so the census reads each
/// scenario behind bash readline, zsh's zle and the kernel's echo. A shell
/// that names its fold keeps it.
pub fn with_default_fold<T>(fold: Fold, f: impl FnOnce() -> T) -> T {
    let old = DEFAULT_FOLD.with(|d| d.replace(fold));
    let out = f();
    DEFAULT_FOLD.with(|d| d.set(old));
    out
}

/// A readline-shaped line editor at a `$ ` prompt on the primary screen,
/// folding its line at the margin as bash readline does unless
/// [`Shell::folding`] names another program's fold ([`Fold`]) or the shell
/// is built under [`with_default_fold`].
pub struct Shell {
    pub h: Host,
    cols: usize,
    row0: u16,
    prompt: usize,
    pub text: Vec<char>,
    pub caret: usize,
    /// Text index the current line's walk started at.
    start: usize,
    echo: Echo,
    /// How the line folds at the margin ([`Fold`]).
    fold: Fold,
    pub key_ms: u64,
    last: Instant,
    pub label: String,
    pub snaps: Vec<Snap>,
}

impl Shell {
    pub fn new(cols: usize, key_ms: u64, echo: Echo) -> Self {
        Self::with(cols, key_ms, echo, "$ ", false)
    }

    /// A shell at prompt `prompt`, framed at the GUI's real pacing when
    /// `paced`.
    pub fn with(cols: usize, key_ms: u64, echo: Echo, prompt: &str, paced: bool) -> Self {
        let mut h = Host::new(cols, 16);
        h.paced = paced;
        let row0 = 2u16;
        h.term
            .process(format!("\x1b[{};1H{prompt}", row0 + 1).as_bytes());
        let last = h.now;
        let mut s = Self {
            h,
            cols,
            row0,
            prompt: prompt.chars().count(),
            text: Vec::new(),
            caret: 0,
            start: 0,
            echo,
            fold: DEFAULT_FOLD.with(std::cell::Cell::get),
            key_ms,
            last,
            label: "start".into(),
            snaps: Vec::new(),
        };
        // The prompt's own output is presented, as the window presents it.
        s.h.frame();
        s.run_to(s.h.now + Duration::from_millis(32));
        s
    }

    /// The same shell folding its line as `fold` does ([`Fold`]; a new
    /// shell folds as readline, or as [`with_default_fold`] names).
    pub fn folding(mut self, fold: Fold) -> Self {
        self.fold = fold;
        self
    }

    /// The echo of the text appended from index `from` to the line's end
    /// (the caret at the end): the glyphs as typed, then the fold's own
    /// bytes when they end exactly at the margin ([`Fold`]).
    pub fn append_echo(&self, from: usize) -> String {
        let mut out: String = self.text[from..].iter().collect();
        let (row, col) = self.pos(from);
        // zle reprints the continuation row from its column 0 on the key
        // after the fold's bare glyph.
        if self.fold == Fold::Zle
            && from > 0
            && self.text.len() == from + 1
            && row > self.row0
            && col == 1
        {
            out = format!("\r{}{}", self.text[from - 1], self.text[from]);
        }
        if (self.prompt + self.text.len()).is_multiple_of(self.cols) {
            match self.fold {
                Fold::Readline => out.push_str(" \r"),
                Fold::Zle => out.push_str(" \r\x1b[K"),
                Fold::Kernel => {}
            }
        }
        out
    }

    /// Grid position of text index `i`.
    pub fn pos(&self, i: usize) -> (u16, u16) {
        let k = self.prompt + i;
        (self.row0 + (k / self.cols) as u16, (k % self.cols) as u16)
    }

    pub fn cup(&self, i: usize) -> String {
        let (r, c) = self.pos(i);
        format!("\x1b[{};{}H", r + 1, c + 1)
    }

    pub fn run_to(&mut self, t: Instant) {
        let cells: Vec<(u16, u16)> = (0..self.text.len()).map(|i| self.pos(i)).collect();
        let text: String = self.text.iter().collect();
        let label = self.label.clone();
        let start = self.start;
        let mut snaps = std::mem::take(&mut self.snaps);
        self.h.frames_to(t, |h| {
            let mut s = Snap {
                ms: h.ms(),
                label: label.clone(),
                text: text.clone(),
                start,
                pos: h.census(&cells),
                breaks: Vec::new(),
                flags: Vec::new(),
            };
            s.judge();
            snaps.push(s);
        });
        self.snaps = snaps;
    }

    /// Output landed: under the GUI's pacing the host presents at once.
    pub fn landed(&mut self) {
        if !self.h.paced || self.h.term.sync_open_dirty() {
            return;
        }
        self.h.frame();
        let cells: Vec<(u16, u16)> = (0..self.text.len()).map(|i| self.pos(i)).collect();
        let mut s = Snap {
            ms: self.h.ms(),
            label: self.label.clone(),
            text: self.text.iter().collect(),
            start: self.start,
            pos: self.h.census(&cells),
            breaks: Vec::new(),
            flags: Vec::new(),
        };
        s.judge();
        self.snaps.push(s);
    }

    /// `s` typed at the line's end `gap` ms apart, each key's echo `lag` ms
    /// after its press — past the gap, the next key is pressed before the
    /// last one's echo (type-ahead).
    pub fn type_lagged(&mut self, s: &str, gap: u64, lag: u64) {
        let mut evs: Vec<(Instant, bool, char)> = Vec::new();
        for (k, ch) in s.chars().enumerate() {
            let press = self.last + Duration::from_millis(gap * (k as u64 + 1));
            evs.push((press, true, ch));
            evs.push((press + Duration::from_millis(lag), false, ch));
        }
        evs.sort_by_key(|e| (e.0, !e.1));
        for (t, press, ch) in evs {
            self.run_to(t);
            self.h.now = self.h.now.max(t);
            if press {
                let (shifted, class) = class_of(ch);
                self.h
                    .glow
                    .note_typed_expected(t, 1, shifted && ch != ' ', class, ch);
                self.last = t;
            } else {
                self.text.push(ch);
                self.caret = self.text.len();
                let bytes = self.append_echo(self.text.len() - 1);
                self.h.term.process(bytes.as_bytes());
                self.landed();
            }
        }
    }

    /// Return: the line submitted, the next prompt printed on the row
    /// after it, and the census moved to the new line.
    pub fn ret(&mut self) {
        let gap = self.key_ms;
        self.press(
            gap,
            |g, t| g.note_return(t),
            |s| {
                let (r, _) = s.pos(s.text.len());
                s.row0 = r + 1;
                s.text.clear();
                s.caret = 0;
                s.start = 0;
                "\r\n$ ".to_string()
            },
        );
    }

    /// Let `ms` pass.
    pub fn idle(&mut self, ms: u64) {
        let t = self.h.now + Duration::from_millis(ms);
        self.run_to(t);
        self.last = self.h.now;
    }

    /// A press `gap` ms after the last event: `stamp` at the press, then the
    /// echo `bytes` after the host's echo lag, the text model updated at the
    /// echo.
    pub fn press(
        &mut self,
        gap: u64,
        stamp: impl FnOnce(&mut CursorGlow, Instant),
        echo: impl FnOnce(&mut Self) -> String,
    ) {
        let t = self.last + Duration::from_millis(gap);
        self.run_to(t);
        self.h.now = t;
        stamp(&mut self.h.glow, t);
        self.last = t;
        let Echo::After(lag) = self.echo;
        let te = t + Duration::from_millis(lag);
        self.run_to(te);
        self.h.now = self.h.now.max(te);
        let bytes = echo(self);
        self.h.term.process(bytes.as_bytes());
        self.landed();
    }

    /// The reprint of the tail from text index `from` and the caret put back
    /// (`\x1b[J`: the rows a shorter line left).
    pub fn reprint(&self, from: usize) -> String {
        let tail: String = self.text[from..].iter().collect();
        format!(
            "{}{tail}\x1b[K\x1b[J{}",
            self.cup(from),
            self.cup(self.caret)
        )
    }

    /// One printable key.
    pub fn key(&mut self, ch: char) {
        let gap = self.key_ms;
        let (shifted, class) = class_of(ch);
        self.press(
            gap,
            |g, t| g.note_typed_expected(t, 1, shifted && ch != ' ', class, ch),
            |s| {
                let at = s.caret;
                s.text.insert(at, ch);
                s.caret += 1;
                if at + 1 == s.text.len() {
                    s.append_echo(at)
                } else {
                    s.reprint(at)
                }
            },
        );
    }

    pub fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key(ch);
        }
    }

    /// One key `gap` ms after the last (a held key's auto-repeat, a burst).
    pub fn key_at(&mut self, ch: char, gap: u64) {
        let k = self.key_ms;
        self.key_ms = gap;
        self.key(ch);
        self.key_ms = k;
    }

    pub fn backspace(&mut self) {
        let gap = self.key_ms;
        self.press(
            gap,
            |g, t| g.note_backspace_erasing(t, Some(1)),
            |s| {
                if s.caret == 0 {
                    return String::new();
                }
                s.caret -= 1;
                let at = s.caret;
                s.text.remove(at);
                s.reprint(at)
            },
        );
    }

    /// ⌃W: the word before the caret, and the blanks after it.
    pub fn word_kill(&mut self) {
        let gap = self.key_ms;
        self.press(
            gap,
            |g, t| {
                g.clear_typed(t);
                g.note_word_kill(t, true);
            },
            |s| {
                let mut i = s.caret;
                while i > 0 && s.text[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && s.text[i - 1] != ' ' {
                    i -= 1;
                }
                s.text.drain(i..s.caret);
                s.caret = i;
                s.reprint(i)
            },
        );
    }

    /// ⌃U: the line before the caret — a new line (the gate restarts the
    /// pace; the census restarts the line's walk here).
    pub fn line_kill(&mut self) {
        let gap = self.key_ms;
        self.press(
            gap,
            |g, t| {
                g.clear_typed(t);
                g.note_kill(t, true);
            },
            |s| {
                s.text.drain(0..s.caret);
                s.caret = 0;
                s.start = 0;
                s.reprint(0)
            },
        );
    }

    pub fn nav_to(&mut self, to: usize) {
        let gap = self.key_ms;
        self.press(
            gap,
            |g, t| {
                g.clear_typed(t);
                g.note_motion(t);
            },
            |s| {
                s.caret = to;
                s.cup(to)
            },
        );
    }

    pub fn home(&mut self) {
        self.nav_to(0);
    }

    pub fn end(&mut self) {
        let n = self.text.len();
        self.nav_to(n);
    }

    pub fn left(&mut self) {
        let to = self.caret.saturating_sub(1);
        self.nav_to(to);
    }

    /// A Tab whose completion is `s`, inserted at the caret.
    pub fn tab(&mut self, s: &str) {
        let gap = self.key_ms;
        let s = s.to_string();
        self.press(
            gap,
            |g, t| {
                g.note_user_gesture(t);
                g.note_insert_armed(t, InsertWidth::Unknown);
            },
            move |sh| sh.insert_echo(&s),
        );
    }

    /// A ⌘V of `s`: the gesture stamped and revoked at the enqueue, the
    /// insert delivered at the echo.
    pub fn paste(&mut self, s: &str) {
        let gap = self.key_ms;
        let n = s.chars().count() as u16;
        let s = s.to_string();
        let press = self.last + Duration::from_millis(gap);
        self.press(
            gap,
            |g, t| {
                g.note_user_gesture(t);
                g.revoke_input_hints_at(t);
            },
            move |sh| {
                let now = sh.h.now;
                sh.h.glow
                    .note_insert_delivered_from(press, now, InsertWidth::Cells(n));
                sh.insert_echo(&s)
            },
        );
    }

    pub fn insert_echo(&mut self, s: &str) -> String {
        let at = self.caret;
        for (k, ch) in s.chars().enumerate() {
            self.text.insert(at + k, ch);
        }
        self.caret += s.chars().count();
        if self.caret == self.text.len() {
            self.append_echo(at)
        } else {
            self.reprint(at)
        }
    }

    pub fn runs(&self) -> Vec<(Flag, usize, usize, usize)> {
        runs(&self.snaps)
    }

    pub fn standing(&self) -> Vec<(Flag, usize, usize, usize)> {
        standing(&self.snaps)
    }

    pub fn dump(&self, from: usize, to: usize) -> String {
        dump(&self.snaps, from, to)
    }

    pub fn verdict(&self, name: &str) -> String {
        verdict(&self.snaps, name)
    }

    pub fn mark(&mut self, label: &str) {
        self.label = label.to_string();
    }
}

/// The flagged runs: consecutive snaps carrying the same `(flag, index)`,
/// `(flag, index, first snap, last snap)`.
pub fn runs(snaps: &[Snap]) -> Vec<(Flag, usize, usize, usize)> {
    let mut open: Vec<(Flag, usize, usize, usize)> = Vec::new();
    let mut done = Vec::new();
    for (k, s) in snaps.iter().enumerate() {
        let mut next = Vec::new();
        for &(f, i) in &s.flags {
            if let Some(r) = open.iter().find(|r| r.0 == f && r.1 == i && r.3 + 1 == k) {
                next.push((f, i, r.2, k));
            } else {
                next.push((f, i, k, k));
            }
        }
        for r in open {
            if !next.iter().any(|n| n.0 == r.0 && n.1 == r.1) {
                done.push(r);
            }
        }
        open = next;
    }
    done.extend(open);
    done.sort_by_key(|r| (r.2, r.1));
    done
}

/// The runs standing [`STANDING_FRAMES`] or more.
pub fn standing(snaps: &[Snap]) -> Vec<(Flag, usize, usize, usize)> {
    runs(snaps)
        .into_iter()
        .filter(|r| r.3 - r.2 + 1 >= STANDING_FRAMES)
        .collect()
}

pub fn dump(snaps: &[Snap], from: usize, to: usize) -> String {
    let mut s = String::new();
    for (k, f) in snaps
        .iter()
        .enumerate()
        .take(to.saturating_add(1))
        .skip(from)
    {
        s.push_str(&format!(
            "f={k:04} t={:>6}ms [{}] flags={:?}\n  txt |{}|\n  lit |{}|\n  t   {}\n",
            f.ms,
            f.label,
            f.flags,
            f.text,
            f.map(),
            f.ts()
        ));
    }
    s
}

/// A one-line verdict and, for each standing run, its first frames.
pub fn verdict(snaps: &[Snap], name: &str) -> String {
    let runs = runs(snaps);
    let standing = standing(snaps);
    let mut s = format!(
        "{name}: {} snaps, {} flagged runs ({} standing >= {STANDING_FRAMES} frames)\n",
        snaps.len(),
        runs.len(),
        standing.len()
    );
    let rc = recolours(snaps);
    if let Some(&(i, k, a, b)) = rc.first() {
        s.push_str(&format!(
            "  RECOLOUR: {} text cells lit again in another colour under unchanged text; \
             first text {i} at t={}ms [{}]: {a:.4} -> {b:.4}\n",
            rc.len(),
            snaps[k].ms,
            snaps[k].label
        ));
    }
    for (f, i, a, b) in runs.iter().take(12) {
        s.push_str(&format!(
            "  {f:?} at text {i} frames {a}..={b} ({} frames) t={}ms [{}]\n",
            b - a + 1,
            snaps[*a].ms,
            snaps[*a].label
        ));
    }
    s
}

/// The temporal reading: every text cell lit again in a different colour while
/// the text up to it stayed the same on every frame between (the band's
/// colours moving under unchanged text): `(text index, snap, t before, t after)`.
pub fn recolours(snaps: &[Snap]) -> Vec<(usize, usize, f32, f32)> {
    let n = snaps.iter().map(|s| s.pos.len()).max().unwrap_or(0);
    // Text index `i` stays "under unchanged text" only while EVERY snap's
    // first `i + 1` chars equal the one before's (any change resets `prev`),
    // so comparing a snap with its predecessor alone is comparing it with the
    // snap `prev` came from: each snap needs only the chars it shares with the
    // one before, not a prefix String per (index, snap): quadratic in the
    // line, and a tenth of `walk_continuity_census`'s CPU (2026-10-05).
    let texts: Vec<Vec<char>> = snaps.iter().map(|s| s.text.chars().collect()).collect();
    let shared: Vec<usize> = (0..texts.len())
        .map(|k| match k.checked_sub(1) {
            Some(j) => texts[j]
                .iter()
                .zip(&texts[k])
                .take_while(|(a, b)| a == b)
                .count(),
            None => 0,
        })
        .collect();
    let mut out = Vec::new();
    for i in 0..n {
        // The last lit stop seen under the current text, if the text up to
        // `i` has not changed since.
        let mut prev: Option<f32> = None;
        for (k, s) in snaps.iter().enumerate() {
            if shared[k] <= i {
                // The text up to `i` changed (or is gone): start over.
                prev = None;
            }
            if texts[k].len() <= i {
                continue;
            }
            let Some(p) = s.pos.get(i) else {
                continue;
            };
            // Unlit: nothing to compare yet; the text check above still
            // resets a later change.
            if !p.lit {
                continue;
            }
            let Some(t) = p.t else {
                continue;
            };
            if let Some(t0) = prev
                && (t - t0).abs() > EPS
            {
                out.push((i, k, t0, t));
            }
            prev = Some(t);
        }
    }
    out.sort_by_key(|r| (r.1, r.0));
    out
}

/// A line of `n` letters and spaces, word lengths 3..=7.
pub fn prose(n: usize) -> String {
    let words = [
        "the", "rainbow", "walks", "along", "every", "line", "kitty", "types", "smooth", "and",
        "bright",
    ];
    let mut s = String::new();
    let mut k = 0;
    while s.len() < n {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(words[k % words.len()]);
        k += 1;
    }
    s.truncate(n);
    if s.ends_with(' ') {
        s.pop();
        s.push('x');
    }
    s
}

// ---------------------------------------------------------------------------
// Real bytes: the captured Claude Code composer takes
// ---------------------------------------------------------------------------

pub enum Rec {
    Out(Vec<u8>),
    In(Vec<u8>),
}

pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

/// `(ms from the first line, event)`; `ptyrec.py` stamps nanoseconds.
pub fn recording(src: &str) -> Vec<(u64, Rec)> {
    let mut t0 = None;
    src.lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let mut it = l.splitn(3, ' ');
            let ns: u64 = it.next()?.parse().ok()?;
            let kind = it.next()?;
            let bytes = unhex(it.next().unwrap_or(""));
            let t0 = *t0.get_or_insert(ns);
            let ms = (ns - t0) / 1_000_000;
            match kind {
                "O" => Some((ms, Rec::Out(bytes))),
                "I" => Some((ms, Rec::In(bytes))),
                _ => None,
            }
        })
        .collect()
}

pub fn is_arrow(b: &[u8]) -> bool {
    b.starts_with(b"\x1b[") && matches!(b.last(), Some(b'A' | b'B' | b'C' | b'D'))
}

/// One captured take and the composer it types into: the rows its text
/// may stand on and the column every text row starts at.
pub struct Capture {
    pub name: &'static str,
    pub src: &'static str,
    pub rows: usize,
    pub cols: usize,
    pub cw: usize,
    pub ch: usize,
    pub text_rows: (u16, u16),
    pub indent: u16,
    /// A rest inserted before the `k`-th printable key: `(k, ms)`.
    pub pause: Option<(usize, u64)>,
}

/// The composer's text cells now: every row of `text_rows` whose first
/// text column is laid out as a composer row (`❯ ` or two blanks) with
/// text after it, from the indent to its last glyph (or the caret, a
/// typed Space's cell), each row its own walk segment.
pub fn composer_cells(h: &Host, cap: &Capture) -> (Vec<(u16, u16)>, Vec<usize>, String) {
    let mut cells = Vec::new();
    let mut breaks = Vec::new();
    let mut text = String::new();
    let mut buf = Vec::new();
    let c = h.term.cursor();
    for row in cap.text_rows.0..=cap.text_rows.1 {
        h.term.row_cols_into(usize::from(row), &mut buf);
        let head: String = buf.iter().take(usize::from(cap.indent)).collect();
        // The composer's first row carries its `❯ `; each continuation row
        // after it, two blanks. Rows before the `❯` (a status line) are not
        // the composer's.
        let first = head.starts_with('\u{276f}');
        if first {
            cells.clear();
            breaks.clear();
            text.clear();
        }
        let continued = !cells.is_empty() && head.chars().all(|ch| ch == ' ' || ch == '\0');
        if !(first || continued) {
            continue;
        }
        let last = buf
            .iter()
            .enumerate()
            .skip(usize::from(cap.indent))
            .filter(|(_, ch)| **ch != ' ' && **ch != '\0')
            .map(|(i, _)| i + 1)
            .next_back();
        let Some(mut end) = last else {
            continue;
        };
        if c.row == row {
            end = end.max(usize::from(c.col));
        }
        if !cells.is_empty() {
            breaks.push(cells.len() - 1);
        }
        for col in usize::from(cap.indent)..end {
            cells.push((row, col as u16));
        }
        let t: String = buf
            .get(usize::from(cap.indent)..end.min(buf.len()))
            .unwrap_or(&[])
            .iter()
            .map(|&ch| if ch == '\0' { ' ' } else { ch })
            .collect();
        if !text.is_empty() {
            text.push('/');
        }
        text.push_str(&t);
    }
    (cells, breaks, text)
}

/// A take replayed at the host seam, the census read every presented frame
/// from the first typed key on.
pub fn replay(cap: &Capture) -> Vec<Snap> {
    let mut rec = recording(cap.src);
    if let Some((k, pause)) = cap.pause {
        let at = rec
            .iter()
            .filter(|(_, e)| {
                matches!(e, Rec::In(b) if matches!(b.as_slice(), [c] if (0x20..0x7f).contains(c)))
            })
            .nth(k)
            .map(|(ms, _)| *ms)
            .expect("the capture has that many keys");
        for (ms, _) in &mut rec {
            if *ms >= at {
                *ms += pause;
            }
        }
    }
    let mut h = Host::sized(cap.rows, cap.cols, cap.cw, cap.ch, 8);
    let first_key = rec
        .iter()
        .find(|(_, e)| matches!(e, Rec::In(b) if matches!(b.as_slice(), [c] if (0x20..0x7f).contains(c))))
        .map_or(0, |(ms, _)| *ms);
    let mut snaps = Vec::new();
    let mut label = String::from("start");
    let snap = |h: &mut Host, label: &str, snaps: &mut Vec<Snap>| {
        if h.ms() < first_key {
            return;
        }
        let (cells, breaks, text) = composer_cells(h, cap);
        let mut s = Snap {
            ms: h.ms(),
            label: label.to_string(),
            text,
            start: 0,
            pos: h.census(&cells),
            breaks,
            flags: Vec::new(),
        };
        s.judge();
        snaps.push(s);
    };
    let t0 = h.t0;
    for (ms, ev) in rec {
        let t = t0 + Duration::from_millis(ms);
        let l = label.clone();
        h.frames_to(t, |h| snap(h, &l, &mut snaps));
        match ev {
            Rec::In(b) => {
                let now = h.now;
                match b.as_slice() {
                    [c] if (0x20..0x7f).contains(c) => {
                        let ch = char::from(*c);
                        let (shifted, class) = class_of(ch);
                        h.glow
                            .note_typed_expected(now, 1, shifted && ch != ' ', class, ch);
                        label = format!("key {ch:?}");
                    }
                    [0x7f] => {
                        h.glow.note_backspace_erasing(now, Some(1));
                        label = "bs".into();
                    }
                    b if is_arrow(b) => {
                        h.glow.clear_typed(now);
                        h.glow.note_motion(now);
                        label = "nav".into();
                    }
                    _ => {}
                }
            }
            Rec::Out(bytes) => h.term.process(&bytes),
        }
    }
    let end = h.now + Duration::from_millis(2_500);
    let l = label.clone();
    h.frames_to(end, |h| snap(h, &l, &mut snaps));
    snaps
}

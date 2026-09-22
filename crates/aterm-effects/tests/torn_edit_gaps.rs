// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A TORN READ OF A FRAME THAT ALSO ERASES IS STILL NOT A CLEARED LINE**
//! (2026-09-22). The owner's report is `torn_read_gaps.rs`'s: *"There is a
//! rainbow trail gap in 0.89. I'm not sure what is causing it but it seems
//! like some kind of back cursor movement bug."* The review round reached
//! the same class through the back cursor movement the owner actually
//! named — a Backspace, a ⌃W — and found the first cut did not close it.
//!
//! A Backspace's repaint arrives in two reads with a present between them.
//! The walk releases `boundary..end` as ONE part, and that part contains
//! the cell of the glyph the key really erased. On the complete present the
//! standing text is back exactly, but that one column stays blank — and the
//! part's lift demanded that NO cell of it be under a blank, so the part was
//! never lifted: every standing letter right of the boundary retracted while
//! its glyph stood (38 cells dark 160 ms after the key), and the next keys
//! landed past a dark stretch (a 29-cell hole for ~1 s, 85 frames with real
//! 1024-byte reads under the spinner). A column still blank when the part is
//! lifted is text that really went, and the walk that runs on that same
//! sample names it on its own — which is exactly what it is for.
//!
//! The seam is `review_regression.rs`'s, kept: every take drives a real
//! `aterm_core` [`Terminal`], samples it exactly as `app_render.rs`'s LOCK A
//! does (the caret's row probe, then every ribbon row, whatever the caret's
//! visibility), reads the content-scroll clock as `sync_cursor_effect_scroll`
//! does, and ticks [`CursorGlow`] on a 16 ms train. The census is the PLAN's
//! coverage per row: a HOLE is a dark column strictly between two lit ones.
//! `RR_CELLS=1` adds every watched row's cells to a failure's dump.
//!
//! WHAT IS NOT PINNED HERE, all of it pre-existing and measured identical on
//! `e05d7860e`, none of it a gap under a typing hand: a torn ⌃K leaves the
//! killed suffix lit over blank cells to +1.6 s because the kill's own
//! retract, not the release, owns those cells; a mid-line Backspace run
//! blinks one cell at the edit column, 1 frame untorn and 9 torn; and a
//! Claude Code Enter's exit fade carries a one-cell hole on 1 frame of 31
//! takes after a torn spinner present.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::SWOOSH_TOTAL_S;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

const CW: usize = 8;
const CH: usize = 16;
const ROWS: usize = 60;
const COL0: usize = 4;
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
const SPINNER_MS: u64 = 100;
const PTY_READ: usize = 1024;
const LIT_COV: u8 = 20;

const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN!!!!";

const FILLER: [&str; 8] = [
    "⏺ I'll start by reading the harness I'm told to copy, then the ribbon module doc.",
    "⏺ Bash(ls -la crates/aterm-effects/tests/ && wc -l crates/aterm-effects/tests/scrub_gaps.rs)",
    "  ⎿  total 1560 — 48 entries, 789 lines in scrub_gaps.rs, 486 in wrapped_composer_band.rs",
    "⏺ Read(crates/aterm-effects/src/rainbow_kitty/ribbon.rs)",
    "  ⎿  Read 420 lines: the module ledger of every prior gap defect and the law that closed it",
    "⏺ The event handler shows the insert path routes through Event::Rewrite and the witness.",
    "⏺ Bash(TRUST_NO_MIGRATE_WARN=1 targo --unverified test -p aterm-effects --test scrub_gaps)",
    "  ⎿  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; finished in 0.21s",
];
const SPIN: [char; 4] = ['✻', '✶', '✳', '✢'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Split {
    Whole,
    /// One present right after text index `j` of the composer was written
    /// (the rest of its row blank from `CSI 2K`, later rows not yet
    /// rewritten), the caret hidden.
    TextCol(usize),
    /// A present after every 1024-byte chunk of the frame.
    Chunks,
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
        pack: None,
    }
}

fn class_of(ch: char) -> (bool, TypedClass) {
    match ch {
        ' ' => (false, TypedClass::Space),
        '!' => (true, TypedClass::Bang),
        c if c.is_uppercase() => (true, TypedClass::Capital),
        _ => (false, TypedClass::Glyph),
    }
}

fn holes_of(cov: &[u8]) -> Vec<(usize, usize)> {
    let lit: Vec<usize> = (0..cov.len()).filter(|&i| cov[i] >= LIT_COV).collect();
    let mut holes = Vec::new();
    if lit.len() < 2 {
        return holes;
    }
    let mut start = None;
    for (i, &v) in cov
        .iter()
        .enumerate()
        .take(lit[lit.len() - 1] + 1)
        .skip(lit[0])
    {
        if v >= LIT_COV {
            if let Some(s) = start.take() {
                holes.push((s, i - s));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    holes
}

fn sgr_pad(n: usize) -> String {
    assert!(n == 0 || n >= 3);
    if n == 0 {
        return String::new();
    }
    let mut s = "\x1b[m".repeat(n / 3 - 1);
    s.push_str(["\x1b[m", "\x1b[0m", "\x1b[00m"][n % 3]);
    s
}

/// A frame with a hole: frame index, ms, label, row, holes.
type HoleAt = (usize, u64, String, u16, Vec<(usize, usize)>);

#[derive(Clone, Debug)]
struct RowC {
    row: u16,
    lit: String,
    holes: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
struct Frame {
    ms: u64,
    label: String,
    rows: Vec<RowC>,
    /// `(row, col)` of cells carrying a retire stamp.
    retiring: Vec<(u16, u16)>,
    /// `(row, col)` of cells carrying a release stamp.
    released: Vec<(u16, u16)>,
    /// `(row, col)` of live (not leaving) cells.
    live: Vec<(u16, u16)>,
    fp: u64,
    text: Vec<(u16, String)>,
    cells: String,
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    cols: usize,
    w: usize,
    t0: Instant,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    scroll: Option<ContentScrollState>,
    text: String,
    caret: usize,
    transcript: Vec<String>,
    working: bool,
    spinner: Option<Instant>,
    spin_i: usize,
    key_split: Split,
    spin_split: Split,
    pad: usize,
    min_rows: usize,
    height: usize,
    top: usize,
    label: String,
    frames: Vec<Frame>,
    torn: Vec<usize>,
    watch: Vec<u16>,
}

impl Host {
    fn new(cols: usize, min_rows: usize, spinner: bool, pad: usize) -> Self {
        let mut term = Terminal::new(ROWS as u16, cols as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, cols);
        term.process(b"\x1b[?1049h\x1b[2J");
        let mut h = Self {
            term,
            glow,
            cfg: cfg(),
            cols,
            w: cols - 6,
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            scroll: None,
            text: String::new(),
            caret: 0,
            transcript: FILLER.iter().map(|s| (*s).to_string()).collect(),
            working: spinner,
            spinner: spinner.then_some(now + Duration::from_millis(SPINNER_MS)),
            spin_i: 0,
            key_split: Split::Whole,
            spin_split: Split::Whole,
            pad,
            min_rows,
            height: 0,
            top: 0,
            label: "start".into(),
            frames: Vec::new(),
            torn: Vec::new(),
            watch: Vec::new(),
        };
        h.repaint(Split::Whole);
        h
    }

    fn geom(&self) -> Geom {
        Geom {
            cw: CW,
            ch: CH,
            rows: ROWS,
            cols: self.cols,
            origin_x: 0,
            origin_y: 0,
            win_w: (self.cols * CW) as u16,
            win_h: (ROWS * CH) as u16,
            head: 0,
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

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
        let g = self.geom();
        let fp = self.glow.tick(cur, self.now, &self.cfg, g, &mut self.out);
        self.record(fp);
    }

    fn record(&mut self, fp: u64) {
        let cols = self.cols;
        let mut cov = vec![0u8; ROWS * cols];
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        for sg in rib.plan_segments() {
            let r = ((sg.spine / CH as f32) - 0.5).floor();
            let c = (sg.x / CW as f32).floor();
            if r >= 0.0 && c >= 0.0 && (r as usize) < ROWS && (c as usize) < cols {
                let i = r as usize * cols + c as usize;
                cov[i] = cov[i].max(sg.cov);
            }
        }
        let mut rows = Vec::new();
        for r in 0..ROWS {
            let lane = &cov[r * cols..(r + 1) * cols];
            if lane.iter().all(|&v| v < LIT_COV) {
                continue;
            }
            rows.push(RowC {
                row: r as u16,
                lit: lane
                    .iter()
                    .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                    .collect(),
                holes: holes_of(lane),
            });
        }
        let mut retiring: Vec<(u16, u16)> = rib
            .cells()
            .iter()
            .filter(|c| c.retire_at.is_some())
            .map(|c| (c.row, c.col))
            .collect();
        let mut released: Vec<(u16, u16)> = rib
            .cells()
            .iter()
            .filter(|c| c.released_at.is_some())
            .map(|c| (c.row, c.col))
            .collect();
        let mut live: Vec<(u16, u16)> = rib
            .cells()
            .iter()
            .filter(|c| !c.leaving())
            .map(|c| (c.row, c.col))
            .collect();
        for v in [&mut retiring, &mut released, &mut live] {
            v.sort_unstable();
            v.dedup();
        }
        let mut text = Vec::new();
        for &r in &self.watch {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            let s: String = self
                .row_buf
                .iter()
                .map(|&c| if c == '\0' { ' ' } else { c })
                .collect();
            text.push((r, s.trim_end().to_string()));
        }
        let mut cells = String::new();
        if std::env::var_os("RR_CELLS").is_some() {
            let rib = self.glow.v2_ribbon().expect("rk");
            for &r in &self.watch {
                let mut v: Vec<_> = rib.cells().iter().filter(|c| c.row == r).collect();
                v.sort_by_key(|c| c.col);
                cells.push_str(&format!("    r{r} cells:"));
                for c in v {
                    let ms = |t: Instant| t.saturating_duration_since(self.t0).as_millis();
                    cells.push_str(&format!(
                        " {}{:?}c{}{}{}{}{}",
                        c.col,
                        c.layer,
                        c.cohort,
                        if c.leaving() { "L" } else { "" },
                        c.released_at
                            .map_or(String::new(), |t| format!("R{}", ms(t))),
                        c.retire_at.map_or(String::new(), |t| format!("T{}", ms(t))),
                        c.retract_at
                            .map_or(String::new(), |t| format!("X{}", ms(t))),
                    ));
                }
                cells.push('\n');
                for k in rib.cohorts().iter().filter(|k| k.row == r) {
                    let ms = |t: Instant| t.saturating_duration_since(self.t0).as_millis();
                    cells.push_str(&format!(
                        "      coh c{} ab={} wake={} alive={} phase={:?} rcol={:?} rej={} rf={}\n",
                        k.id,
                        k.abandoned,
                        k.wake,
                        ms(k.alive_at),
                        k.phase,
                        k.retract_col,
                        k.rejoinable,
                        k.drain_right_first
                    ));
                }
            }
            let c = self.term.cursor();
            cells.push_str(&format!(
                "    caret ({},{}) vis={}\n",
                c.row,
                c.col,
                self.term.cursor_visible()
            ));
        }
        self.frames.push(Frame {
            cells,
            ms: self.ms(),
            label: self.label.clone(),
            rows,
            retiring,
            released,
            live,
            fp,
            text,
        });
    }

    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        self.frame();
    }

    fn idle_to(&mut self, t: Instant) {
        loop {
            let next = self.now + Duration::from_millis(FRAME_MS);
            if let Some(due) = self.spinner
                && due <= t
                && due <= next
            {
                self.now = self.now.max(due);
                self.spinner = Some(due + Duration::from_millis(SPINNER_MS));
                self.spin_i += 1;
                let keep = std::mem::replace(&mut self.label, "spinner".into());
                self.repaint(self.spin_split);
                self.label = keep;
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

    fn schedule(&mut self, ms: u64) {
        let t = self.last_event + Duration::from_millis(ms);
        self.idle_to(t);
        self.now = self.now.max(t);
        self.last_event = self.now;
    }

    fn text_rows(&self) -> usize {
        let by_len = self.text.len().div_ceil(self.w);
        let by_caret = self.caret / self.w + 1;
        by_len.max(by_caret).max(self.min_rows).max(1)
    }

    /// The first text row's screen row (0-based) for the current layout.
    fn text_row0(&self) -> u16 {
        (self.top - 1 + self.transcript_shown() + 3) as u16
    }

    fn transcript_shown(&self) -> usize {
        FILLER.len()
    }

    /// Claude Code's frame. Returns bytes, byte offsets of each text row's
    /// first text char, and the region's top (1-based).
    fn frame_bytes(&self) -> (Vec<u8>, Vec<usize>, usize) {
        let cols = self.cols;
        let clip = |s: &str| -> String { s.chars().take(cols - 1).collect() };
        let shown = self.transcript_shown();
        let mut rows: Vec<String> = self.transcript[self.transcript.len() - shown..]
            .iter()
            .map(|s| clip(s))
            .collect();
        rows.push(if self.working {
            format!(
                "{} Clauding… ({}s · ↓ 2.3k tokens)",
                SPIN[self.spin_i % SPIN.len()],
                40 + (self.spin_i / 10) % 50
            )
        } else {
            String::new()
        });
        rows.push(String::new());
        rows.push(format!("╭{}╮", "─".repeat(cols - 2)));
        let first_text = rows.len();
        let n = self.text_rows();
        let b = self.text.as_bytes();
        for k in 0..n {
            let lo = (k * self.w).min(b.len());
            let hi = ((k + 1) * self.w).min(b.len());
            let chunk = &self.text[lo..hi];
            let pad = self.w - chunk.len();
            rows.push(format!(
                "│ {} {}{} │",
                if k == 0 { '>' } else { ' ' },
                chunk,
                " ".repeat(pad)
            ));
        }
        rows.push(format!("╰{}╯", "─".repeat(cols - 2)));
        rows.push("  ? for shortcuts".to_string());
        let height = rows.len();
        let top = ROWS - height + 1;
        let mut s = String::from("\x1b[?2026h\x1b[?25l");
        s.push_str(&sgr_pad(self.pad));
        if self.height > 0 && height > self.height {
            s.push_str(&format!("\x1b[{ROWS};1H"));
            s.push_str(&"\n".repeat(height - self.height));
        } else if self.height > 0 && height < self.height {
            let old_top = ROWS - self.height + 1;
            for r in old_top..top {
                s.push_str(&format!("\x1b[{r};1H\x1b[2K"));
            }
        }
        s.push_str(&format!("\x1b[{top};1H"));
        let mut offs = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            s.push_str("\x1b[2K");
            if i >= first_text && i < first_text + n {
                offs.push(s.len() + "│ > ".len());
            }
            s.push_str(r);
            if i + 1 < rows.len() {
                s.push_str("\r\n");
            }
        }
        let crow = top + first_text + self.caret / self.w;
        let ccol = COL0 + self.caret % self.w + 1;
        s.push_str(&format!("\x1b[{crow};{ccol}H\x1b[?25h\x1b[?2026l"));
        (s.into_bytes(), offs, top)
    }

    fn repaint(&mut self, split: Split) {
        let (bytes, offs, top) = self.frame_bytes();
        let n_rows = offs.len();
        let mut cuts: Vec<usize> = Vec::new();
        let len = self.text.len();
        let w = self.w;
        let text_cut = |j: usize| -> usize {
            if j >= len {
                let last = (len.saturating_sub(1) / w).min(n_rows - 1);
                offs[last] + (len - (last * w).min(len))
            } else {
                offs[j / w] + j % w
            }
        };
        let inside = |c: usize| -> bool {
            (0..n_rows).any(|k| {
                let lo = (k * w).min(len);
                let hi = ((k + 1) * w).min(len);
                hi > lo && c >= offs[k] && c < offs[k] + (hi - lo)
            }) || (n_rows > 1 && len > w && c > offs[0] && c < offs[n_rows - 1])
        };
        match split {
            Split::Whole => {}
            Split::TextCol(j) => cuts.push(text_cut(j)),
            Split::Chunks => {
                cuts.extend((1..).map(|k| k * PTY_READ).take_while(|&p| p < bytes.len()))
            }
        }
        let mut start = 0;
        for &c in &cuts {
            self.term.process(&bytes[start..c]);
            if inside(c) {
                self.torn.push(self.frames.len());
            }
            self.frame();
            self.now += Duration::from_millis(1);
            start = c;
        }
        self.term.process(&bytes[start..]);
        self.height = ROWS - top + 1;
        self.top = top;
        self.frame();
    }

    fn key_after(&mut self, ch: char, ms: u64) {
        self.schedule(ms);
        self.label = format!("key {ch:?}");
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.repaint(self.key_split);
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key_after(ch, KEY_MS);
        }
    }

    fn hop_left(&mut self, ms: u64) {
        self.schedule(ms);
        self.label = "opt-left".into();
        self.glow.note_motion(self.now);
        let b = self.text.as_bytes();
        let mut i = self.caret;
        while i > 0 && b[i - 1] == b' ' {
            i -= 1;
        }
        while i > 0 && b[i - 1] != b' ' {
            i -= 1;
        }
        self.caret = i;
        self.repaint(Split::Whole);
    }

    /// Ctrl-K: kill from the caret to the end of the line.
    fn kill_to_end(&mut self, ms: u64, split: Split) {
        self.schedule(ms);
        self.label = "ctrl-k".into();
        self.glow.note_kill(self.now, false);
        self.text.truncate(self.caret);
        self.repaint(split);
    }

    /// Frames whose watched rows carry an interior hole.
    fn holes_on(&self, rows: &[u16]) -> Vec<HoleAt> {
        let mut v = Vec::new();
        for (i, f) in self.frames.iter().enumerate() {
            for rc in &f.rows {
                if rows.contains(&rc.row) && !rc.holes.is_empty() {
                    v.push((i, f.ms, f.label.clone(), rc.row, rc.holes.clone()));
                }
            }
        }
        v
    }

    fn lit_of(&self, i: usize, row: u16) -> String {
        self.frames[i]
            .rows
            .iter()
            .find(|r| r.row == row)
            .map_or_else(|| ".".repeat(self.cols), |r| r.lit.clone())
    }

    fn dump(&self, i: usize, rows: &[u16]) -> String {
        let f = &self.frames[i];
        let mut s = format!(
            "  f{i:<5} t={:>6} {:<12} fp={}\n",
            f.ms,
            f.label,
            u64::from(f.fp != 0)
        );
        for &r in rows {
            let text = f
                .text
                .iter()
                .find(|(rr, _)| *rr == r)
                .map_or(String::new(), |(_, t)| t.clone());
            let rel: Vec<u16> = f
                .released
                .iter()
                .filter(|c| c.0 == r)
                .map(|c| c.1)
                .collect();
            let ret: Vec<u16> = f
                .retiring
                .iter()
                .filter(|c| c.0 == r)
                .map(|c| c.1)
                .collect();
            s.push_str(&format!(
                "    r{r:<3} text |{}\n    r{r:<3} lit  |{}  rel={rel:?} ret={ret:?}\n",
                text,
                self.lit_of(i, r)
                    .chars()
                    .take(text.chars().count() + 4)
                    .collect::<String>()
            ));
        }
        s.push_str(&f.cells);
        s
    }

    fn story(&self, rows: &[u16]) -> String {
        let holes = self.holes_on(rows);
        let mut s = format!(
            "{} frames, {} with a hole on {rows:?}; torn presents {:?}\n",
            self.frames.len(),
            holes.len(),
            &self.torn[..self.torn.len().min(20)]
        );
        if let Some(&(i, ..)) = holes.first() {
            for k in i.saturating_sub(3)..(i + 3).min(self.frames.len()) {
                s.push_str(&self.dump(k, rows));
            }
        }
        s
    }

    /// Idle long past every swoosh, then T6: nothing on glass, no cadence,
    /// no deadline, on EVERY one of the last frames.
    fn assert_idle_to_zero(&mut self, what: &str) {
        self.idle((SWOOSH_TOTAL_S * 1000.0) as u64 + 1500);
        for _ in 0..10 {
            self.step();
            let f = self.frames.last().expect("a frame");
            let cells = self
                .glow
                .v2_ribbon()
                .expect("rainbow kitty owns the frame")
                .cells()
                .len();
            assert_eq!(
                f.fp, 0,
                "{what}: T6 — fingerprint 0 at idle (cells {cells})"
            );
            assert!(
                !self.glow.needs_frame_cadence(),
                "{what}: T6 — no frame cadence at idle"
            );
            assert_eq!(
                self.glow
                    .next_change_deadline(self.now, Duration::from_millis(FRAME_MS)),
                None,
                "{what}: T6 — no deadline at idle"
            );
            assert!(
                f.rows.is_empty(),
                "{what}: nothing lit at idle: {:?}",
                f.rows.iter().map(|r| r.row).collect::<Vec<_>>()
            );
        }
    }
}

fn pad_for(cols: usize, min_rows: usize, spinner: bool, j: usize, text_len: usize) -> usize {
    let mut h = Host::new(cols, min_rows, spinner, 0);
    h.text = "x".repeat(text_len);
    h.caret = text_len;
    h.height = 0;
    let (_, offs, _) = h.frame_bytes();
    let off = offs[j / h.w] + j % h.w;
    let n = (PTY_READ - off % PTY_READ) % PTY_READ;
    if n < 3 { n + PTY_READ } else { n }
}

fn assert_clean(h: &Host, rows: &[u16], what: &str) {
    let holes = h.holes_on(rows);
    assert!(
        holes.is_empty(),
        "{what}: {} frames with an interior hole, first {:?}\n{}",
        holes.len(),
        holes.first().map(|x| (x.1, &x.2, x.3, &x.4)),
        h.story(rows)
    );
}

fn assert_no_melt(h: &Host, rows: &[u16], what: &str) {
    if let Some(i) = h
        .frames
        .iter()
        .position(|f| f.retiring.iter().any(|c| rows.contains(&c.0)))
    {
        let mut s = String::new();
        for k in i.saturating_sub(2)..(i + 2).min(h.frames.len()) {
            s.push_str(&h.dump(k, rows));
        }
        panic!("{what}: text that came straight back took the melt\n{s}");
    }
}

/// Run every take, collecting failures instead of stopping at the first.
fn run_takes<T>(takes: Vec<(String, T)>, f: impl Fn(&str, T)) {
    let mut failed = Vec::new();
    let n = takes.len();
    for (what, t) in takes {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&what, t)));
        if let Err(e) = r {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_default();
            failed.push(format!("{what}: {}", msg.lines().next().unwrap_or("")));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {n} takes failed:\n  {}",
        failed.len(),
        failed.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// (1) Codex particle replay, torn.
// ---------------------------------------------------------------------------

mod codex {
    use super::*;

    const C_ROWS: usize = 32;
    const C_COLS: usize = 100;
    const C_CW: usize = 7;
    const C_CH: usize = 14;
    const C_FRAME_MS: u64 = 8;
    const END_MS: u64 = 9_500;
    const FIXTURE: &str = include_str!("fixtures/codex-particles-2026-09-16.ptylog");

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
            .collect()
    }

    fn g() -> Geom {
        Geom {
            cw: C_CW,
            ch: C_CH,
            rows: C_ROWS,
            cols: C_COLS,
            origin_x: 0,
            origin_y: 0,
            win_w: (C_COLS * C_CW) as u16,
            win_h: (C_ROWS * C_CH) as u16,
            head: 0,
        }
    }

    struct C {
        term: Terminal,
        glow: CursorGlow,
        cfg: GlowConfig,
        t0: Instant,
        now: Instant,
        out: Vec<GlowQuad>,
        row_buf: Vec<char>,
        blink: u64,
        scroll: Option<ContentScrollState>,
        composer_row: Option<u16>,
        broken: Vec<(u64, String)>,
        lit_max: usize,
        honor_sync: bool,
        sync_since: Option<Instant>,
    }

    impl C {
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
            self.glow.tick(cur, self.now, &self.cfg, g(), &mut self.out);
            let Some(row) = self.composer_row else { return };
            let mut cov = [0u8; C_COLS];
            let rib = self.glow.v2_ribbon().expect("rk");
            for sg in rib.plan_segments() {
                let r = ((sg.spine / C_CH as f32) - 0.5).floor();
                let col = (sg.x / C_CW as f32).floor();
                if r >= 0.0
                    && col >= 0.0
                    && r as usize == usize::from(row)
                    && (col as usize) < C_COLS
                {
                    let i = col as usize;
                    cov[i] = cov[i].max(sg.cov);
                }
            }
            self.lit_max = self
                .lit_max
                .max(cov.iter().filter(|&&v| v >= LIT_COV).count());
            if !holes_of(&cov).is_empty() {
                let ms = self.now.saturating_duration_since(self.t0).as_millis() as u64;
                self.broken.push((
                    ms,
                    cov.iter()
                        .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                        .collect(),
                ));
            }
        }

        /// The host's SYNC-1 hold with its cap: a present through an open
        /// `?2026` bracket only once it has been open 150 ms.
        fn present_ok(&mut self) -> bool {
            if !self.term.sync_open_dirty() {
                self.sync_since = None;
                return true;
            }
            let since = *self.sync_since.get_or_insert(self.now);
            !self.honor_sync
                || self.now.saturating_duration_since(since) >= Duration::from_millis(150)
        }

        fn run_to(&mut self, ms: u64) {
            let t = self.t0 + Duration::from_millis(ms);
            while self.now + Duration::from_millis(C_FRAME_MS) <= t {
                self.now += Duration::from_millis(C_FRAME_MS);
                if self.present_ok() {
                    self.frame();
                }
            }
            self.now = t;
        }
    }

    /// Replay; `torn` splits every read in half with a present between the
    /// halves, and `honor_sync = false` presents through an open `?2026`
    /// bracket (the hold's cap).
    pub(super) fn replay(torn: bool, honor_sync: bool) -> (Vec<(u64, String)>, usize) {
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, C_COLS);
        let mut h = C {
            term: Terminal::new(C_ROWS as u16, C_COLS as u16),
            glow,
            cfg: GlowConfig {
                intensity: 1.0,
                audible: true,
                ..cfg()
            },
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            scroll: None,
            composer_row: None,
            broken: Vec::new(),
            lit_max: 0,
            honor_sync,
            sync_since: None,
        };
        for l in FIXTURE.lines().filter(|l| !l.is_empty()) {
            let mut it = l.splitn(3, ' ');
            let ms: u64 = it.next().unwrap().parse().unwrap();
            let kind = it.next().unwrap();
            let bytes = unhex(it.next().unwrap());
            if ms > END_MS {
                break;
            }
            h.run_to(ms);
            match kind {
                "O" => {
                    let cut = if torn { bytes.len() / 2 } else { bytes.len() };
                    h.term.process(&bytes[..cut]);
                    if h.present_ok() {
                        h.frame();
                    }
                    if cut < bytes.len() {
                        h.now += Duration::from_millis(1);
                        h.term.process(&bytes[cut..]);
                        if h.present_ok() {
                            h.frame();
                        }
                    }
                }
                "I" => match bytes.as_slice() {
                    [b] if (0x20..0x7f).contains(b) => {
                        if h.composer_row.is_none() {
                            h.composer_row = Some(h.term.cursor().row);
                        }
                        h.glow.note_typed_cells(h.now, 1);
                    }
                    [0x7f] => h.glow.note_backspace_erasing(h.now, Some(1)),
                    b if b.starts_with(b"\x1b[")
                        && matches!(b.last(), Some(b'A' | b'B' | b'C' | b'D')) =>
                    {
                        h.glow.note_motion(h.now);
                    }
                    _ => {}
                },
                k => panic!("bad kind {k}"),
            }
        }
        h.run_to(END_MS);
        (h.broken, h.lit_max)
    }
}

#[test]
fn codex_particle_replay_stays_one_run_whole_and_torn_in_half_under_the_capped_sync_hold() {
    for (torn, honor) in [(false, true), (true, true)] {
        let (broken, lit_max) = codex::replay(torn, honor);
        assert!(
            lit_max >= 9,
            "torn={torn} sync={honor}: band laid ({lit_max})"
        );
        assert!(
            broken.is_empty(),
            "torn={torn} honor_sync={honor}: {} frames with a hole, first t={} {}",
            broken.len(),
            broken[0].0,
            broken[0].1
        );
    }
}

// ---------------------------------------------------------------------------
// (2) A torn read inside a WRAPPED two-row composer.
// ---------------------------------------------------------------------------

/// Fixed two-row box (70 columns, 64-cell text rows): the owner's line wraps
/// at text index 64; every key's frame torn at a boundary around the wrap.
#[test]
fn a_torn_read_at_the_wrap_column_of_a_fixed_two_row_composer_keeps_both_rows_whole() {
    let cols = 70;
    let w = cols - 6;
    let takes = [w - 3, w - 1, w, w + 1, w + 3, w + 20]
        .into_iter()
        .map(|j| (format!("fixed 2-row composer torn at text index {j}"), j))
        .collect();
    run_takes(takes, |what, j| {
        let mut h = Host::new(cols, 2, false, 0);
        let r0 = h.text_row0();
        h.watch = vec![r0, r0 + 1];
        h.key_split = Split::TextCol(j);
        h.type_str(LINE);
        h.idle(600);
        assert!(!h.torn.is_empty(), "{what}: torn");
        assert_clean(&h, &[r0, r0 + 1], what);
        assert_no_melt(&h, &[r0, r0 + 1], what);
        h.assert_idle_to_zero(what);
    });
}

/// The same with real 1024-byte reads and the spinner, the boundary pinned
/// at the wrap.
#[test]
fn claude_code_1024_byte_reads_with_the_boundary_at_the_wrap_keep_both_rows_whole() {
    let cols = 70;
    let w = cols - 6;
    let mut takes = Vec::new();
    for j in [w - 1, w, w + 1] {
        for spinner in [false, true] {
            takes.push((
                format!("1024-byte reads, boundary at {j}, spinner {spinner}"),
                (j, spinner),
            ));
        }
    }
    run_takes(takes, |what, (j, spinner)| {
        let pad = pad_for(cols, 2, spinner, j, LINE.len());
        let mut h = Host::new(cols, 2, spinner, pad);
        let r0 = h.text_row0();
        h.watch = vec![r0, r0 + 1];
        h.key_split = Split::Chunks;
        h.spin_split = Split::Chunks;
        h.type_str(LINE);
        h.idle(600);
        assert!(h.torn.len() > 3, "{what}: torn {:?}", h.torn);
        assert_clean(&h, &[r0, r0 + 1], what);
        assert_no_melt(&h, &[r0, r0 + 1], what);
        h.assert_idle_to_zero(what);
    });
}

/// A composer that GROWS at the wrap (the box grows upward by a scroll, as
/// Ink's bottom-pinned region does), every key torn around the wrap column.
#[test]
fn a_growing_composer_torn_at_its_wrap_keeps_the_band_whole() {
    let cols = 70;
    let w = cols - 6;
    let all: Vec<u16> = (0..ROWS as u16).collect();
    let mut takes = vec![("growing composer, untorn control".to_string(), None)];
    for j in [w - 2, w, w + 2, w + 10] {
        takes.push((format!("growing composer torn at {j}"), Some(j)));
    }
    run_takes(takes, |what, j| {
        let mut h = Host::new(cols, 1, false, 0);
        if let Some(j) = j {
            h.key_split = Split::TextCol(j);
        }
        h.type_str(LINE);
        h.idle(600);
        assert_clean(&h, &all, what);
        h.assert_idle_to_zero(what);
    });
}

// ---------------------------------------------------------------------------
// (3) Enter clears the composer.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// (4) A torn read on EVERY key for 3 s at 12 cps, the boundary fixed.
// ---------------------------------------------------------------------------

/// The lit count on the row at frame `i`.
fn lit_count(h: &Host, i: usize, row: u16) -> usize {
    h.lit_of(i, row).chars().filter(|&c| c == '#').count()
}

#[test]
fn every_key_torn_for_three_seconds_at_one_fixed_boundary_keeps_the_band_whole() {
    let typed = &LINE[..36];
    let mut control = Host::new(140, 1, false, 0);
    let row = control.text_row0();
    control.type_str(typed);
    let c_end = control.frames.len() - 1;
    let control_lit = lit_count(&control, c_end, row);
    let mut takes = Vec::new();
    for j in [0usize, 1, 2, 3, 5, 10, 20, 30] {
        for spinner in [false, true] {
            takes.push((
                format!("every key torn at {j}, spinner {spinner}"),
                (j, spinner),
            ));
        }
    }
    run_takes(takes, |what, (j, spinner)| {
        let mut h = Host::new(140, 1, spinner, 0);
        h.watch = vec![row];
        h.key_split = Split::TextCol(j);
        h.spin_split = Split::TextCol(j);
        h.type_str(typed);
        let end = h.frames.len() - 1;
        assert!(!h.torn.is_empty(), "{what}: torn");
        let lit = lit_count(&h, end, row);
        h.idle(300);
        assert_clean(&h, &[row], what);
        assert!(
            lit + 2 >= control_lit,
            "{what}: at the last key {lit} cells lit against the untorn {control_lit}\n{}",
            h.dump(end, &[row])
        );
        assert_no_melt(&h, &[row], what);
        h.assert_idle_to_zero(what);
    });
}

#[test]
fn every_key_in_1024_byte_reads_for_three_seconds_at_one_fixed_boundary_keeps_the_band_whole() {
    let typed = &LINE[..36];
    let mut takes = Vec::new();
    for j in [0usize, 1, 2, 5, 10, 20, 30] {
        for spinner in [false, true] {
            takes.push((
                format!("1024-byte reads, boundary at {j}, spinner {spinner}"),
                (j, spinner),
            ));
        }
    }
    run_takes(takes, |what, (j, spinner)| {
        let pad = pad_for(140, 1, spinner, j, typed.len());
        let mut h = Host::new(140, 1, spinner, pad);
        let row = h.text_row0();
        h.watch = vec![row];
        h.key_split = Split::Chunks;
        h.spin_split = Split::Chunks;
        h.type_str(typed);
        let end = h.frames.len() - 1;
        let lit = lit_count(&h, end, row);
        h.idle(300);
        assert_clean(&h, &[row], what);
        assert!(
            lit >= typed.len() - 2,
            "{what}: {lit} lit at the last key\n{}",
            h.dump(end, &[row])
        );
        assert_no_melt(&h, &[row], what);
        h.assert_idle_to_zero(what);
    });
}

/// A Backspace run at the end of the line while the agent's spinner tears
/// every frame: the erased glyphs leave, the rest stands whole.
#[test]
fn a_backspace_run_under_torn_reads_keeps_the_band_whole() {
    let typed = &LINE[..60];
    let mut takes = vec![(
        "backspace run, untorn control".to_string(),
        (Split::Whole, 0usize, false),
    )];
    for j in [0usize, 20, 45, 52, 58] {
        takes.push((
            format!("backspace run, every frame torn at {j}"),
            (Split::TextCol(j), 0usize, false),
        ));
    }
    for j in [20usize, 45, 52] {
        takes.push((
            format!("backspace run, 1024-byte reads at {j}, spinner"),
            (Split::Chunks, pad_for(140, 1, true, j, 50), true),
        ));
    }
    run_takes(takes, |what, (split, pad, spinner)| {
        let mut h = Host::new(140, 1, spinner, pad);
        let row = h.text_row0();
        h.watch = vec![row];
        h.type_str(typed);
        h.key_split = split;
        h.spin_split = split;
        for k in 0..10 {
            h.schedule(if k == 0 { 250 } else { 110 });
            h.label = "backspace".into();
            h.glow.note_backspace_erasing(h.now, Some(1));
            h.caret -= 1;
            h.text.remove(h.caret);
            h.repaint(split);
        }
        h.type_str(" and on");
        let end = h.frames.len() - 1;
        h.idle(300);
        assert_clean(&h, &[row], what);
        let dark: Vec<usize> = (COL0..COL0 + h.text.len())
            .filter(|&c| !h.frames[end].live.contains(&(row, c as u16)))
            .collect();
        assert!(
            dark.is_empty(),
            "{what}: at the last key the standing line lost {} cells {dark:?}\n{}",
            dark.len(),
            h.dump(end, &[row])
        );
        h.assert_idle_to_zero(what);
    });
}

/// **REPRO — A TORN BACKSPACE FRAME.** One Backspace at the end of a typed
/// line whose repaint reaches the glass in two reads, a present between them
/// after text index `j`: the walk releases `j..end` as ONE part, and that
/// part includes the ERASED glyph's cell, which never comes back. The part
/// is lifted only once none of its glyphs is under a blank, so it is never
/// lifted: every cell of the standing text right of the boundary retracts
/// while the text stands, and the hand typing on lays its keys past a dark
/// stretch.
#[test]
fn a_torn_backspace_frame_keeps_the_standing_text_lit() {
    let typed = &LINE[..60];
    let mut takes = vec![(
        "one backspace, untorn control".to_string(),
        (Split::Whole, 0usize, false),
    )];
    for j in [10usize, 20, 40, 55] {
        takes.push((
            format!("one backspace torn at {j}"),
            (Split::TextCol(j), 0, false),
        ));
    }
    for j in [20usize, 40] {
        takes.push((
            format!("one backspace, 1024-byte reads at {j}, spinner"),
            (
                Split::Chunks,
                pad_for(140, 1, true, j, typed.len() - 1),
                true,
            ),
        ));
    }
    run_takes(takes, |what, (split, pad, spinner)| {
        let mut h = Host::new(140, 1, spinner, pad);
        let row = h.text_row0();
        h.watch = vec![row];
        h.type_str(typed);
        h.spin_split = split;
        h.schedule(250);
        h.label = "backspace".into();
        h.glow.note_backspace_erasing(h.now, Some(1));
        h.caret -= 1;
        h.text.remove(h.caret);
        h.repaint(split);
        let at = h.frames.len() - 1;
        h.idle(400);
        let standing = COL0..COL0 + h.text.len();
        let t_at = h.frames[at].ms;
        for (i, f) in h.frames.iter().enumerate().skip(at) {
            if f.ms < t_at + 150 {
                continue;
            }
            let lit = h.lit_of(i, row);
            let dark: Vec<usize> = standing
                .clone()
                .filter(|&c| lit.as_bytes()[c] != b'#')
                .collect();
            assert!(
                dark.is_empty(),
                "{what}: +{} ms after the Backspace the standing text is dark on glass at {} cells {}..={}\n{}",
                f.ms - h.frames[at].ms,
                dark.len(),
                dark.first().copied().unwrap_or(0),
                dark.last().copied().unwrap_or(0),
                h.dump(i, &[row])
            );
        }
        h.key_split = Split::Whole;
        h.type_str(" and on");
        h.idle(300);
        h.assert_idle_to_zero(what);
        assert_clean(&h, &[row], what);
    });
}

/// A word kill (Ctrl-W / Option-Backspace) at the end of the line whose
/// repaint is torn inside the standing text.
#[test]
fn a_torn_word_kill_frame_keeps_the_standing_text_lit() {
    let typed = &LINE[..60];
    let mut takes = vec![("word kill, untorn control".to_string(), Split::Whole)];
    for j in [10usize, 20, 40, 50] {
        takes.push((format!("word kill torn at {j}"), Split::TextCol(j)));
    }
    run_takes(takes, |what, split| {
        let mut h = Host::new(140, 1, false, 0);
        let row = h.text_row0();
        h.watch = vec![row];
        h.type_str(typed);
        h.schedule(250);
        h.label = "ctrl-w".into();
        h.glow.note_word_kill(h.now, true);
        let b = h.text.as_bytes();
        let mut i = h.caret;
        while i > 0 && b[i - 1] == b' ' {
            i -= 1;
        }
        while i > 0 && b[i - 1] != b' ' {
            i -= 1;
        }
        h.text.truncate(i);
        h.caret = i;
        h.repaint(split);
        let at = h.frames.len() - 1;
        h.idle(400);
        let standing = COL0..COL0 + h.text.len();
        let t_at = h.frames[at].ms;
        for (k, f) in h.frames.iter().enumerate().skip(at + 1) {
            if f.ms < t_at + 150 {
                continue;
            }
            let lit = h.lit_of(k, row);
            let dark: Vec<usize> = standing
                .clone()
                .filter(|&c| lit.as_bytes()[c] != b'#')
                .collect();
            assert!(
                dark.is_empty(),
                "{what}: +{} ms after the word kill the standing text is dark on glass at {} cells {}..={}\n{}",
                f.ms - h.frames[at].ms,
                dark.len(),
                dark.first().copied().unwrap_or(0),
                dark.last().copied().unwrap_or(0),
                h.dump(k, &[row])
            );
        }
        h.type_str(" and on");
        h.idle(300);
        assert_clean(&h, &[row], what);
    });
}

// ---------------------------------------------------------------------------
// (5) Ctrl-K mid-line.
// ---------------------------------------------------------------------------

fn kill_take(split: Split) -> (Host, usize, u16, usize, usize) {
    let mut h = Host::new(140, 1, false, 0);
    let row = h.text_row0();
    h.watch = vec![row];
    h.type_str(LINE);
    h.idle(80);
    for _ in 0..3 {
        h.hop_left(110);
    }
    let caret = h.caret;
    let end = h.text.len();
    h.kill_to_end(150, split);
    let at = h.frames.len() - 1;
    (h, at, row, caret, end)
}

const KILL_SPLITS: [Split; 5] = [
    Split::Whole,
    Split::TextCol(0),
    Split::TextCol(10),
    Split::TextCol(60),
    Split::TextCol(93),
];

/// Ctrl-K, then the hand types on at the caret: the new text joins the
/// standing prefix with no hole, torn or not.
#[test]
fn ctrl_k_then_typing_on_keeps_one_run() {
    let takes = KILL_SPLITS
        .into_iter()
        .map(|s| (format!("ctrl-k then typing, kill {s:?}"), s))
        .collect();
    run_takes(takes, |what, split| {
        let (mut h, _at, row, caret, _end) = kill_take(split);
        h.key_split = Split::Whole;
        h.key_after('x', 150);
        h.type_str(" and more words here");
        let end = h.frames.len() - 1;
        h.idle(300);
        assert_clean(&h, &[row], what);
        let dark: Vec<usize> = (COL0..COL0 + caret + 21)
            .filter(|&c| !h.frames[end].live.contains(&(row, c as u16)))
            .collect();
        assert!(
            dark.is_empty(),
            "{what}: at the last key the line lost {dark:?}\n{}",
            h.dump(end, &[row])
        );
        h.assert_idle_to_zero(what);
    });
}

/// Ctrl-K, then typing on with EVERY key torn at the same boundary inside
/// the standing prefix.
#[test]
fn ctrl_k_then_typing_on_with_every_frame_torn_keeps_one_run() {
    let takes = [0usize, 10, 60, 93]
        .into_iter()
        .map(|j| (format!("ctrl-k then typing, every frame torn at {j}"), j))
        .collect();
    run_takes(takes, |what, j| {
        let (mut h, _at, row, caret, _end) = kill_take(Split::TextCol(j));
        h.key_split = Split::TextCol(j);
        h.key_after('x', 150);
        h.type_str(" and more words here");
        let end = h.frames.len() - 1;
        h.idle(300);
        assert_clean(&h, &[row], what);
        let dark: Vec<usize> = (COL0..COL0 + caret + 21)
            .filter(|&c| !h.frames[end].live.contains(&(row, c as u16)))
            .collect();
        assert!(
            dark.is_empty(),
            "{what}: at the last key the line lost {dark:?}\n{}",
            h.dump(end, &[row])
        );
        h.assert_idle_to_zero(what);
    });
}

/// T6 on the defect's own path: a torn Backspace, typing on, then idle —
/// whatever the parts did, the engine still goes to zero.
#[test]
fn a_torn_backspace_then_typing_on_still_goes_idle_to_zero() {
    for spinner in [false, true] {
        let mut h = Host::new(140, 1, spinner, 0);
        let row = h.text_row0();
        h.watch = vec![row];
        h.type_str(&LINE[..60]);
        h.spin_split = Split::TextCol(20);
        h.schedule(250);
        h.label = "backspace".into();
        h.glow.note_backspace_erasing(h.now, Some(1));
        h.caret -= 1;
        h.text.remove(h.caret);
        h.repaint(Split::TextCol(20));
        h.type_str(" and on");
        h.assert_idle_to_zero(&format!("torn backspace then typing, spinner {spinner}"));
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A torn read is not a cleared line. A Claude Code frame is over 1 KiB and the macOS PTY
//! delivers it in 1024-byte reads. A present between two reads (the `?2026` hold is capped)
//! shows the composer row `CSI 2K`-cleared and rewritten only up to the read boundary, caret
//! hidden; the identical text returns 2 to 30 ms later. The band must show no hole:
//!
//! * moved-text arm: a blanked fragment still standing in place in its run has not moved, even
//!   if the same letters appear elsewhere on the line; it is released and the identical text
//!   lifts it on the next present;
//! * restore arm: the restore counts the cohort without the held key's cell the pre-witness
//!   tick just laid, so it succeeds;
//! * the comb: a released run does not release the spaces of its standing text with it.
//!
//! Each take feeds an `aterm_core` [`Terminal`], samples rows as `app_render.rs`'s frame hold
//! does and ticks [`CursorGlow`] every 16 ms with the app's hints per key. A hole is a dark
//! column strictly between two lit ones on the typed row.

use crate::common::{
    LIT_COV, Rec, caret_sample, class_of, coverage, holes_of, is_key, note_blink,
    observe_caret_row, observe_ribbon_rows, rainbow_cfg, recording,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::ribbon::{RETIRE_MELT_S, RETRACT_DUR_S, RETRACT_FADE_S};
use std::time::{Duration, Instant};

const CW: usize = 8;
const CH: usize = 16;
const ROWS: usize = 60;
const COLS: usize = 140;
/// The composer's text row (0-based) and the grid column of its first glyph
/// (`│ > ` before it).
const ROW: u16 = (ROWS - 3) as u16;
const COL0: usize = 4;
/// 12 cps.
const KEY_MS: u64 = 83;
const FRAME_MS: u64 = 16;
/// The spinner's cadence while the agent works.
const SPINNER_MS: u64 = 100;
/// One macOS PTY read.
const PTY_READ: usize = 1024;

/// The line the takes type.
const LINE: &str = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES! YOU NEED TO FUCKING MERGE ALL BEST WORK INTO MAIN!!!!";
/// Text index of the `T` of `INTO`: a read boundary here shows `…WORK IN`
/// and blanks `TO MAIN!!!!`.
const AT_TO: usize = 101;

/// Transcript rows above the box, so the frame is multi-KiB as Claude
/// Code's is.
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

/// Where a present lands inside one repaint of the composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Split {
    /// The whole frame in one read.
    Whole,
    /// One present right after text column `j` of the composer row was written,
    /// the rest of the row still blank from `CSI 2K`, the caret hidden: the torn
    /// read.
    TextCol(usize),
    /// A present after every [`PTY_READ`]-byte chunk of the frame.
    Chunks,
}

fn geom() -> Geom {
    crate::common::geom(ROWS, COLS, CW, CH)
}

/// Interior dark runs of one row as `(first col, len)`.
type Holes = Vec<(usize, usize)>;

/// One frame with a hole: its instant in ms, what it shows (the take's
/// label, or the replay's lit map), and the holes.
type HoleFrame = (u64, String, Holes);

/// One frame's census of one row.
#[derive(Clone, Debug)]
struct Census {
    ms: u64,
    label: String,
    /// The row's text, trimmed.
    text: String,
    /// `#` lit, `.` dark, per column.
    lit: String,
    /// Interior dark runs as `(first col, len)`.
    holes: Vec<(usize, usize)>,
    /// Columns of the row whose cell carries a retire stamp (the melt).
    retiring: Vec<u16>,
    /// Columns of the row whose cell carries a partial release's stamp.
    released: Vec<u16>,
    /// Columns owned by a live (not leaving) cell.
    live: Vec<u16>,
}

/// The host: one terminal, the kitty, one clock, the composer's state.
struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    t0: Instant,
    now: Instant,
    last_event: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    text: String,
    caret: usize,
    label: String,
    /// The spinner's next repaint, while the agent works.
    spinner: Option<Instant>,
    spin_i: usize,
    /// How every key repaint is delivered, and every spinner repaint.
    key_split: Split,
    spin_split: Split,
    /// Extra bytes at the head of every frame (SGR no-ops, so nothing wraps or
    /// scrolls) that shift every PTY read boundary, as the transcript streaming
    /// above the box does.
    pad: usize,
    /// Presents that showed the composer row torn inside its text.
    torn: Vec<usize>,
    census: Vec<Census>,
}

impl Host {
    fn new(spinner: bool) -> Self {
        Self::with_pad(spinner, 0)
    }

    fn with_pad(spinner: bool, pad: usize) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        term.process(b"\x1b[?1049h\x1b[2J");
        let mut h = Self {
            term,
            glow,
            cfg: rainbow_cfg(),
            t0: now,
            now,
            last_event: now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            text: String::new(),
            caret: 0,
            label: "start".into(),
            spinner: spinner.then_some(now + Duration::from_millis(SPINNER_MS)),
            spin_i: 0,
            key_split: Split::Whole,
            spin_split: Split::Whole,
            pad,
            torn: Vec::new(),
            census: Vec::new(),
        };
        h.repaint(Split::Whole);
        h
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// The frame hold's sampling (the caret's row and every ribbon row, caret visible or not),
    /// the tick, then the census.
    fn frame(&mut self) {
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
        let alt = self.term.is_alternate_screen();
        self.glow.note_context(alt);
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
        self.record(ROW);
    }

    fn record(&mut self, row: u16) {
        let cov = coverage(&self.glow, geom(), row);
        self.term.row_cols_into(usize::from(row), &mut self.row_buf);
        let text: String = self
            .row_buf
            .iter()
            .map(|&c| if c == '\0' { '_' } else { c })
            .collect();
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let on_row = || rib.cells().iter().filter(|c| c.row == row);
        let mut retiring: Vec<u16> = on_row()
            .filter(|c| c.retire_at.is_some())
            .map(|c| c.col)
            .collect();
        let mut released: Vec<u16> = on_row()
            .filter(|c| c.released_at.is_some())
            .map(|c| c.col)
            .collect();
        let mut live: Vec<u16> = on_row().filter(|c| !c.leaving()).map(|c| c.col).collect();
        for v in [&mut retiring, &mut released, &mut live] {
            v.sort_unstable();
            v.dedup();
        }
        self.census.push(Census {
            ms: self.ms(),
            label: self.label.clone(),
            text: text.trim_end().to_string(),
            lit: cov
                .iter()
                .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                .collect(),
            holes: holes_of(&cov),
            retiring,
            released,
            live,
        });
    }

    fn step(&mut self) {
        self.now += Duration::from_millis(FRAME_MS);
        self.frame();
    }

    /// Frames at 16 ms up to `t`, the spinner's repaints fired where they
    /// fall.
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

    /// Claude Code's frame: the dynamic region (transcript, spinner, a blank,
    /// the box, the hint row) rewritten row by row inside `?2026h` / `?25l`,
    /// then the caret's CUP, `?25h`, `?2026l`. Returns the bytes and the offset
    /// of the composer text's first char.
    fn frame_bytes(&self) -> (Vec<u8>, usize) {
        let mut rows: Vec<String> = FILLER.iter().map(|s| (*s).to_string()).collect();
        rows.push(format!(
            "{} Clauding… ({}s · ↓ 2.3k tokens)",
            SPIN[self.spin_i % SPIN.len()],
            40 + self.spin_i / 10
        ));
        rows.push(String::new());
        rows.push(format!("╭{}╮", "─".repeat(COLS - 2)));
        let text_row = rows.len();
        let pad = COLS.saturating_sub(6 + self.text.len());
        rows.push(format!("│ > {}{} │", self.text, " ".repeat(pad)));
        rows.push(format!("╰{}╯", "─".repeat(COLS - 2)));
        rows.push("  ? for shortcuts".to_string());
        let top = ROWS - rows.len() + 1;
        let mut s = String::from("\x1b[?2026h\x1b[?25l");
        s.push_str(&sgr_pad(self.pad));
        s.push_str(&format!("\x1b[{top};1H"));
        let mut text_off = 0;
        for (i, r) in rows.iter().enumerate() {
            s.push_str("\x1b[2K");
            if i == text_row {
                text_off = s.len() + "│ > ".len();
            }
            s.push_str(r);
            if i + 1 < rows.len() {
                s.push_str("\r\n");
            }
        }
        s.push_str(&format!(
            "\x1b[{};{}H\x1b[?25h\x1b[?2026l",
            usize::from(ROW) + 1,
            COL0 + self.caret + 1
        ));
        (s.into_bytes(), text_off)
    }

    /// One repaint, with a present wherever `split` lands one.
    fn repaint(&mut self, split: Split) {
        let (bytes, text_off) = self.frame_bytes();
        let mut cuts: Vec<usize> = Vec::new();
        match split {
            Split::Whole => {}
            Split::TextCol(j) => cuts.push(text_off + j.min(self.text.len())),
            Split::Chunks => {
                cuts.extend((1..).map(|k| k * PTY_READ).take_while(|&p| p < bytes.len()))
            }
        }
        let mut start = 0;
        for &c in &cuts {
            self.term.process(&bytes[start..c]);
            if c > text_off && c < text_off + self.text.len() {
                self.torn.push(self.census.len());
            }
            self.frame();
            self.now += Duration::from_millis(1);
            start = c;
        }
        self.term.process(&bytes[start..]);
        self.frame();
    }

    /// One typed key `ms` after the last event: the app_input seam stamps
    /// the typed hint with the host's own class, the composer echoes it.
    fn key_after(&mut self, ch: char, ms: u64) {
        self.schedule(ms);
        self.label = format!("key {ch:?}");
        let (shifted, class) = class_of(ch);
        self.glow.note_typed_glyph(self.now, 1, shifted, class);
        self.text.insert(self.caret, ch);
        self.caret += 1;
        self.repaint(self.key_split);
        assert!(self.term.cursor_visible(), "the repaint shows the caret");
    }

    /// A word hop (Option+Left / Option+Right as Claude Code moves: to the
    /// previous word's start, past the next word's end) `ms` after the last
    /// event: the navigation hint is stamped, the composer moves the caret and
    /// repaints.
    fn hop_after(&mut self, left: bool, ms: u64, split: Split) {
        self.schedule(ms);
        self.label = if left { "opt-left" } else { "opt-right" }.into();
        self.glow.note_motion(self.now);
        let b = self.text.as_bytes();
        let mut i = self.caret;
        if left {
            while i > 0 && b[i - 1] == b' ' {
                i -= 1;
            }
            while i > 0 && b[i - 1] != b' ' {
                i -= 1;
            }
        } else {
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            while i < b.len() && b[i] != b' ' {
                i += 1;
            }
        }
        self.caret = i;
        self.repaint(split);
    }

    fn type_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.key_after(ch, KEY_MS);
        }
    }

    /// The program writes `bytes` with no key behind it, and a present follows.
    fn program(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
        self.frame();
    }

    /// Every frame with an interior hole, as `(ms, label, holes)`.
    fn hole_frames(&self) -> Vec<HoleFrame> {
        self.census
            .iter()
            .filter(|c| !c.holes.is_empty())
            .map(|c| (c.ms, c.label.clone(), c.holes.clone()))
            .collect()
    }

    fn dump(&self, i: usize) -> String {
        let c = &self.census[i];
        let w = (COL0 + self.text.len() + 3).min(COLS);
        format!(
            "  f{i:<4} t={:>6} ms {:<12} holes={:?} retiring={:?} released={:?}\n    text |{}\n    lit  |{}",
            c.ms,
            c.label,
            c.holes,
            c.retiring,
            c.released,
            c.text.chars().take(w).collect::<String>(),
            c.lit.chars().take(w).collect::<String>()
        )
    }

    /// The whole take around the first hole, for a failure message.
    fn story(&self) -> String {
        let first = self.census.iter().position(|c| !c.holes.is_empty());
        let mut s = format!(
            "{} frames, {} with a hole; torn presents at {:?}\n",
            self.census.len(),
            self.hole_frames().len(),
            self.torn
        );
        if let Some(i) = first {
            for k in i.saturating_sub(3)..(i + 4).min(self.census.len()) {
                s.push_str(&self.dump(k));
                s.push('\n');
            }
        }
        s
    }
}

/// No interior dark column on the typed row on any frame of the take.
fn assert_no_hole(h: &Host, what: &str) {
    let holes = h.hole_frames();
    assert!(
        holes.is_empty(),
        "{what}: the band carried an interior hole on {} frames, first {:?}\n{}",
        holes.len(),
        holes.first(),
        h.story()
    );
}

/// [`assert_no_hole`], and no light on the row ever took the melt: text
/// that came straight back is never judged replaced or moved.
fn assert_whole(h: &Host, what: &str) {
    assert_no_hole(h, what);
    let melted = h.census.iter().position(|c| !c.retiring.is_empty());
    assert!(
        melted.is_none(),
        "{what}: text that came straight back was retired on the melt\n{}",
        {
            let i = melted.unwrap_or(0);
            (i.saturating_sub(3)..(i + 2).min(h.census.len()))
                .map(|k| h.dump(k))
                .collect::<Vec<_>>()
                .join("\n")
        }
    );
}

/// The moved-text arm: every key from `M` on torn after `…WORK IN`, blanking `TO`, which also
/// stands elsewhere on the line (`STOP`, `NEED TO`). The fragment still stands in place in its
/// run, so it is released, and the identical text lifts it on the next present.
#[test]
fn the_owners_line_torn_under_to_of_into_keeps_its_band_whole() {
    let mut h = Host::new(false);
    h.type_str(&LINE[..AT_TO + 3]);
    h.key_split = Split::TextCol(AT_TO);
    h.type_str(&LINE[AT_TO + 3..]);
    h.idle(400);
    assert!(!h.torn.is_empty(), "the torn read was presented");
    let torn = h.torn[0];
    assert!(
        h.census[torn]
            .released
            .contains(&(COL0 as u16 + AT_TO as u16)),
        "`T` was released on the torn present, not melted\n{}",
        h.story()
    );
    assert!(
        h.census[torn + 1].released.is_empty(),
        "…and lifted by the identical text on the next present\n{}",
        h.story()
    );
    assert_whole(&h, "the owner's line torn under TO");
}

/// The restore arm: a tail found nowhere else on the line is released in part, and the tick
/// before the witness lays the key's own cell into the same cohort; the restore still succeeds.
#[test]
fn a_released_tail_is_restored_although_the_key_is_laid_before_the_restore() {
    let line = "the quick brown fox jumps over the lazy dog while seven wizards quietly hex jumbo pyramids";
    let mut h = Host::new(false);
    h.type_str(&line[..60]);
    h.key_split = Split::TextCol(50);
    h.type_str(&line[60..]);
    h.idle(300);
    assert!(
        h.torn.len() >= 20,
        "every key from col 60 on was torn: {:?}",
        h.torn
    );
    for &t in &h.torn {
        assert!(
            !h.census[t].released.is_empty(),
            "the torn present released the blanked tail at f{t}\n{}",
            h.story()
        );
        assert!(
            h.census[t + 1].released.is_empty(),
            "and the complete present lifted all of it though the key's cell joined first (f{})\n{}",
            t + 1,
            h.story()
        );
    }
    assert_whole(&h, "a unique tail torn under every key");
}

/// Exactly `n` bytes of SGR no-ops (`n` is 0 or at least 3): a pad that moves the
/// read boundaries without writing a cell (a padded transcript row would wrap
/// and scroll the frame, moving the composer's text for real).
fn sgr_pad(n: usize) -> String {
    assert!(n == 0 || n >= 3, "an SGR pad is 0 or at least 3 bytes");
    if n == 0 {
        return String::new();
    }
    let mut s = "\x1b[m".repeat(n / 3 - 1);
    s.push_str(["\x1b[m", "\x1b[0m", "\x1b[00m"][n % 3]);
    debug_assert_eq!(s.len(), n);
    s
}

/// The frame pad that puts a 1024-byte read boundary right after text
/// column `j` of the composer row.
fn pad_for_boundary_at(j: usize) -> usize {
    let (_, text_off) = Host::new(false).frame_bytes();
    let n = (PTY_READ - (text_off + j) % PTY_READ) % PTY_READ;
    if n < 3 { n + PTY_READ } else { n }
}

/// Every key's frame and every spinner repaint (ten a second) presented after each 1024-byte
/// read, the frame padded so a boundary falls inside the composer's text at column 30, 60, 90
/// and under `TO`.
#[test]
fn claude_code_frames_in_1024_byte_reads_keep_the_band_whole_while_the_hand_types() {
    for j in [30usize, 60, 90, AT_TO] {
        let mut h = Host::with_pad(true, pad_for_boundary_at(j));
        h.key_split = Split::Chunks;
        h.spin_split = Split::Chunks;
        h.type_str(LINE);
        h.idle(500);
        assert!(
            h.torn.len() > 5,
            "the boundary at {j} tore the row: {:?}",
            h.torn
        );
        assert_whole(&h, &format!("1024-byte reads, a boundary at text col {j}"));
    }
}

/// The line typed without `INTO `, Option+Left to `MAIN`, `INTO ` inserted (each key shifts
/// `MAIN!!!!` right), Option+Right back, the insert's and hop's frames torn around the insert
/// point. The tail an insert pushes past the run's end is replaced text and may melt, so only
/// the band's continuity is asserted.
#[test]
fn an_insert_through_word_hops_under_torn_reads_keeps_the_band_whole() {
    let head = &LINE[..99];
    let tail = &LINE[104..];
    let mut takes = Vec::new();
    for j in [30usize, 60, 90, 98, 99, 100, 101, 102, 103] {
        takes.push((format!("text col {j}"), Split::TextCol(j), 0usize, false));
    }
    for j in [99usize, 100, 101] {
        for spinner in [false, true] {
            takes.push((
                format!("1024-byte reads, boundary at {j}, spinner {spinner}"),
                Split::Chunks,
                pad_for_boundary_at(j),
                spinner,
            ));
        }
    }
    // Each take owns its host and clock, so the takes run in parallel.
    crate::common::par_map(&takes, |(what, split, pad, spinner)| {
        let (split, pad, spinner) = (*split, *pad, *spinner);
        let mut h = Host::with_pad(spinner, pad);
        h.type_str(head);
        h.type_str(tail);
        h.hop_after(true, 400, Split::Whole);
        assert_eq!(h.caret, head.len(), "the hop landed at the start of MAIN");
        h.key_split = split;
        for (k, ch) in "INTO ".chars().enumerate() {
            h.key_after(ch, if k == 0 { 400 } else { KEY_MS });
        }
        assert_eq!(h.text, LINE);
        h.hop_after(false, 400, split);
        assert_eq!(h.caret, LINE.len(), "the hop landed at the end");
        h.idle(600);
        assert_no_hole(&h, &format!("insert through hops, {what}"));
    });
}

/// The spinner twin: no key in flight; each key's own frame arrives whole, but
/// the agent's spinner repaints the region ten times a second and one read of
/// each repaint ends after `…WORK IN`. The newest glyphs right of the boundary
/// (`T`, then `TO`), blanked by a spinner frame alone, must not melt.
#[test]
fn a_spinner_repaint_torn_under_to_with_no_key_in_flight_keeps_the_band_whole() {
    let mut h = Host::new(true);
    h.spin_split = Split::TextCol(AT_TO);
    h.type_str(LINE);
    h.idle(600);
    assert!(
        h.torn.len() >= 5,
        "the spinner's repaints were torn: {:?}",
        h.torn
    );
    assert_whole(&h, "the spinner torn under TO");
}

/// The comb, and the control: a program really clears the tail of a typed line (caret hidden,
/// never rewritten). The tail is released and leaves through the retract; the spaces of the
/// text still standing left of it stay lit.
#[test]
fn a_really_cleared_tail_leaves_and_takes_only_its_own_spaces() {
    let mut h = Host::new(false);
    let typed = "zoom out. first of all, STOP LANDING BRANCHES AND PATCHES";
    h.type_str(typed);
    h.idle(200);
    let cut = typed.find("STOP").expect("the cut");
    let at = COL0 + cut;
    h.label = "clear".into();
    h.program(format!("\x1b[?25l\x1b[{};{}H\x1b[K", ROW + 1, at + 1).as_bytes());
    let cleared = h.census.len() - 1;
    let released = &h.census[cleared].released;
    assert!(
        released.iter().all(|&c| usize::from(c) >= at),
        "only the cleared tail is released, never a space of the standing text: {released:?}\n{}",
        h.story()
    );
    assert!(
        released.contains(&(at as u16)),
        "the cleared tail was released: {released:?}"
    );
    h.program(format!("\x1b[{};{}H\x1b[?25h", ROW + 1, at + 1).as_bytes());
    let span_ms = ((RETRACT_DUR_S + RETRACT_FADE_S) * 1000.0) as u64;
    h.idle(span_ms + 3 * FRAME_MS);
    let last = h.census.last().expect("a frame");
    let lit_past: Vec<usize> = last
        .lit
        .char_indices()
        .filter(|&(i, c)| c == '#' && i > at)
        .map(|(i, _)| i)
        .collect();
    assert!(
        lit_past.is_empty(),
        "the cleared tail is gone within the retract, not held: lit {lit_past:?}\n{}",
        h.story()
    );
    let prefix: Vec<u16> = (COL0..at - 1).map(|c| c as u16).collect();
    for c in &h.census[cleared..] {
        let dark: Vec<_> = prefix.iter().filter(|p| !c.live.contains(p)).collect();
        assert!(
            dark.is_empty(),
            "the standing text keeps every cell, its spaces included, at t={}: {dark:?}",
            c.ms
        );
    }
    let holes = h.hole_frames();
    assert!(
        holes.is_empty(),
        "no comb in the standing text: {:?}\n{}",
        holes.first(),
        h.story()
    );
}

/// The other control: a whole run re-laid one or two rows away (the composer moved, its old row
/// blanked) is carried with its clocks to where its glyphs stand (`Witness::follow_runs`,
/// `Ribbon::translate_run`), so no light stays under moved text. Text that is gone rather than
/// moved still melts (`a_really_cleared_tail_leaves_and_takes_only_its_own_spaces`).
#[test]
fn a_whole_run_re_laid_on_another_row_follows_its_text() {
    let mut h = Host::new(false);
    h.type_str("hello brave new world");
    h.idle(200);
    // The box moves up two rows: the old row blank, the text above it.
    let text = h.text.clone();
    h.label = "moved".into();
    h.program(
        format!(
            "\x1b[?25l\x1b[{};1H\x1b[2K\x1b[{};1H\x1b[2K│ > {text}\x1b[{};{}H\x1b[?25h",
            ROW + 1,
            ROW - 1,
            ROW - 1,
            COL0 + text.len() + 1
        )
        .as_bytes(),
    );
    let moved = h.census.len() - 1;
    let c = &h.census[moved];
    // The old row owns no cell, live or melting.
    assert!(
        c.live.is_empty() && c.retiring.is_empty(),
        "the old row keeps no cell: live {:?}, retiring {:?}",
        c.live,
        c.retiring
    );
    // …and the band is on the row the text moved to, under its own glyphs.
    let up = coverage(&h.glow, geom(), ROW - 2);
    let lit_up: Vec<usize> = (0..COLS).filter(|&i| up[i] >= LIT_COV).collect();
    assert!(
        lit_up.len() >= text.len() / 2,
        "the band followed its text to the row above: lit {lit_up:?}\n{}",
        h.story()
    );
    h.idle((RETIRE_MELT_S * 1000.0) as u64 + 2 * FRAME_MS);
    let last = h.census.last().expect("a frame");
    assert!(
        !last.lit.contains('#'),
        "the old row is dark one melt later: {}",
        last.lit
    );
}

// Claude Code's real bytes.

/// Real Claude Code under a PTY wrapper in a headless aterm: a line typed at 12 cps with
/// Option+Left and `INTO ` inserted, then Option+Right / End back (`-end`), three hops each way
/// (`-scrub3`), an `@`-mention popup open while typing (`-popup`). Each `O` line is one PTY
/// read of at most 1024 bytes.
const FIXTURES: [(&str, &str); 4] = [
    (
        "insert",
        include_str!("fixtures/claude-composer-2026-09-21.ptylog"),
    ),
    (
        "end",
        include_str!("fixtures/claude-composer-2026-09-21-end.ptylog"),
    ),
    (
        "scrub3",
        include_str!("fixtures/claude-composer-2026-09-21-scrub3.ptylog"),
    ),
    (
        "popup",
        include_str!("fixtures/claude-composer-2026-09-21-popup.ptylog"),
    ),
];
/// The recording's grid (`resize 40 120`, 7×14 px cells) and composer row.
const REC_ROWS: usize = 40;
const REC_COLS: usize = 120;
const REC_CW: usize = 7;
const REC_CH: usize = 14;
const REC_ROW: u16 = 37;

/// Replay one recording: every read presented on its own (with `torn`, split in half with a
/// present between the halves), the app's hints for the keys, a 16 ms train between events.
/// Returns the frames with an interior hole on the composer row between the first and last
/// typed key, and how many frames were presented.
fn replay(src: &str, torn: bool) -> (Vec<HoleFrame>, usize) {
    let now0 = Instant::now();
    let mut term = Terminal::new(REC_ROWS as u16, REC_COLS as u16);
    let mut glow = CursorGlow::default();
    glow.note_pane_columns(0, REC_COLS);
    let g = crate::common::geom(REC_ROWS, REC_COLS, REC_CW, REC_CH);
    let cfg = rainbow_cfg();
    let mut out = Vec::new();
    let mut row_buf = Vec::new();
    let mut blink = 0u64;
    let mut now = now0;
    // The stamps are nanoseconds.
    let rec = recording(src, 1_000_000);
    // Typing runs from the first printable key to the exit chord.
    let first_key = rec
        .iter()
        .find(|(_, e)| matches!(e, Rec::In(b) if is_key(b)))
        .map(|(ms, _)| *ms)
        .expect("a typed key");
    let exit = rec
        .iter()
        .find(|(_, e)| matches!(e, Rec::In(b) if b.as_slice() == b"\x1b[99;5u"))
        .map_or(u64::MAX, |(ms, _)| *ms);
    let last_key = rec
        .iter()
        .rev()
        .find(|(ms, e)| *ms < exit && matches!(e, Rec::In(b) if is_key(b)))
        .map(|(ms, _)| *ms)
        .expect("a last key");
    let mut holes = Vec::new();
    let mut frames = 0usize;
    let mut frame = |term: &Terminal, glow: &mut CursorGlow, now: Instant, frames: &mut usize| {
        let (c, cur) = caret_sample(term);
        note_blink(term, glow, &mut blink, now);
        glow.note_context(term.is_alternate_screen());
        observe_caret_row(term, glow, &mut row_buf, c, now);
        observe_ribbon_rows(term, glow, &mut row_buf);
        glow.tick(cur, now, &cfg, g, &mut out);
        *frames += 1;
        let ms = now.saturating_duration_since(now0).as_millis() as u64;
        if ms >= first_key && ms <= last_key {
            let cov = coverage(glow, g, REC_ROW);
            let found = holes_of(&cov);
            if !found.is_empty() {
                let map: String = cov
                    .iter()
                    .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                    .collect();
                holes.push((ms, map, found));
            }
        }
    };
    for (ms, ev) in rec {
        if ms >= exit {
            break;
        }
        let t = now0 + Duration::from_millis(ms);
        while now + Duration::from_millis(FRAME_MS) <= t {
            now += Duration::from_millis(FRAME_MS);
            frame(&term, &mut glow, now, &mut frames);
        }
        now = now.max(t);
        match ev {
            Rec::In(b) => match b.as_slice() {
                [c] if (0x20..0x7f).contains(c) => {
                    let (shifted, class) = class_of(*c as char);
                    glow.note_typed_glyph(now, 1, shifted, class);
                }
                b"\x1b[1;3D" | b"\x1b[1;3C" | b"\x1b[F" => glow.note_motion(now),
                _ => {}
            },
            Rec::Out(bytes) => {
                let cut = if torn { bytes.len() / 2 } else { bytes.len() };
                term.process(&bytes[..cut]);
                frame(&term, &mut glow, now, &mut frames);
                if cut < bytes.len() {
                    now += Duration::from_millis(1);
                    term.process(&bytes[cut..]);
                    frame(&term, &mut glow, now, &mut frames);
                }
            }
        }
    }
    (holes, frames)
}

/// Claude Code's recorded frames, each read presented whole and again torn in half, leave no
/// interior hole on the composer row while typing. With nothing submitted the composer frames
/// are one-glyph diffs with no `CSI 2K`, so a torn read blanks nothing: this is a guard.
#[test]
fn claude_code_s_recorded_bytes_read_whole_or_torn_keep_the_band_whole_while_typing() {
    for (name, src) in FIXTURES {
        for torn in [false, true] {
            let (holes, frames) = replay(src, torn);
            assert!(frames > 500, "{name}: the replay presented {frames} frames");
            assert!(
                holes.is_empty(),
                "{name} (torn={torn}): {} frames with an interior hole while typing, first at {} ms {:?}\n{}",
                holes.len(),
                holes[0].0,
                holes[0].2,
                holes[0].1
            );
        }
    }
}

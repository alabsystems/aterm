// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE OWNER'S TWO COMPOSER GAPS, AND THE RAINBOW LIT THROUGH BOTH**
//! (the owner's screenshots of 2026-09-25).
//!
//! Claude Code's composer at the owner's 113 columns, modelled row for row:
//! the text wraps at `COLS - 4`, the box is bottom-anchored between its two
//! rules and grows UP a row when the text needs one more. Keys are stamped at
//! the press; the app echoes each one LATE and batched, and aterm's frames
//! run on a 16.7 ms train.
//!
//! * `wit hthe▯cu [Image #1]`: typing BEFORE a trailing `[Image #1]`, the
//!   Space after `hthe` pushed the placeholder onto a new row; the box grew in
//!   the SAME frame the Space echoed, and the caret moved `(R, c) → (R-1,
//!   c+1)` before the follow pass had seen the text move. That echo had no
//!   sweep to lay it (`rainbow_kitty::Engine::lay_crossed_echo`).
//! * `interrupted`, wrapped while the app STALLED: only its `i` had echoed
//!   when the word wrapped, and the rest came in ONE late repaint, so the
//!   relay placed a one-letter word before the caret and the batch's cells
//!   stayed dark (`rainbow_kitty::Ribbon::note_rewrap_start`: the landing
//!   row's own sample says where the relaid text starts).
//!
//! NON-VACUITY: each case asserts the move it exists for really happened.

#![allow(
    dead_code,
    reason = "the composer model and its simulator carry tracing the cases here do not read"
)]

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use std::time::{Duration, Instant};

const WROWS: usize = aterm_effects::cursor_glow::CURSOR_WITNESS_ROWS;

const ROWS: usize = 53;
const COLS: usize = 113;
const CW: usize = 15;
const CH: usize = 28;
/// Claude Code's text width: `COLS - 4` (1-based columns 3..=COLS-2).
const TW: usize = COLS - 4;
const FRAME_US: u64 = 16_667;

const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
const RULE_PEN: &str = "\x1b[38;2;136;136;136m";
const TEXT_PEN: &str = "\x1b[39m";

fn cfg() -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00D0_D0D0,
        theme_bg: 0x0011_1318,
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

/// Greedy word wrap at `TW`, a break's space dropped, the caret (an index
/// into `text`) mapped to (line, col-in-line).
fn wrap(text: &[char], caret: usize) -> (Vec<Vec<char>>, (usize, usize)) {
    let mut lines: Vec<Vec<char>> = vec![Vec::new()];
    // map from text index -> (line, col) (for the caret)
    let mut pos: Vec<(usize, usize)> = Vec::with_capacity(text.len() + 1);
    let mut i = 0;
    while i < text.len() {
        if text[i] == ' ' {
            let nl = lines.len();
            let ll = lines[nl - 1].len();
            if ll + 1 > TW {
                // the space breaks: dropped
                pos.push((nl, 0));
                lines.push(Vec::new());
            } else {
                pos.push((nl - 1, ll));
                lines[nl - 1].push(' ');
            }
            i += 1;
            continue;
        }
        let mut j = i;
        while j < text.len() && text[j] != ' ' {
            j += 1;
        }
        let word = &text[i..j];
        let line_len = lines.last().unwrap().len();
        if line_len + word.len() > TW && line_len > 0 {
            // move the word: drop a trailing space on the line (the break)
            let line = lines.last_mut().unwrap();
            if line.last() == Some(&' ') {
                line.pop();
                // the dropped space's position stays where it was recorded
            }
            lines.push(Vec::new());
        }
        for &ch in word {
            if lines[lines.len() - 1].len() >= TW {
                lines.push(Vec::new());
            }
            let nl = lines.len();
            pos.push((nl - 1, lines[nl - 1].len()));
            lines[nl - 1].push(ch);
        }
        i = j;
    }
    let caret_pos = if caret < pos.len() {
        pos[caret]
    } else {
        let l = lines.len() - 1;
        (l, lines[l].len())
    };
    (lines, caret_pos)
}

/// One screen row the composer draws: its row, its cells, and whether it is
/// one of the box's rules.
type ScreenRow = (usize, Vec<char>, bool);

/// The composer model: the text, the caret, what the screen holds.
struct Composer {
    text: Vec<char>,
    caret: usize,
    screen: Vec<Vec<char>>,
    label: &'static str,
}

impl Composer {
    fn rows_for(&self) -> (Vec<ScreenRow>, (usize, usize)) {
        let (lines, (cl, cc)) = wrap(&self.text, self.caret);
        let n = lines.len();
        let bottom_rule = ROWS - 2;
        let first_text = bottom_rule - n;
        let top_rule = first_text - 1;
        let mut out = Vec::new();
        out.push((top_rule, self.rule(), true));
        for (k, l) in lines.iter().enumerate() {
            let mut row: Vec<char> = if k == 0 {
                vec!['\u{276f}', '\u{a0}']
            } else {
                vec![' ', ' ']
            };
            row.extend_from_slice(l);
            out.push((first_text + k, row, false));
        }
        out.push((bottom_rule, "\u{2500}".repeat(COLS).chars().collect(), true));
        (out, (first_text + cl, 2 + cc))
    }

    fn rule(&self) -> Vec<char> {
        let mut r: Vec<char> = "\u{2500}".repeat(COLS).chars().collect();
        if !self.label.is_empty() {
            let l: Vec<char> = self.label.chars().collect();
            let start = COLS - 3 - l.len();
            r[start - 1] = ' ';
            for (k, &ch) in l.iter().enumerate() {
                r[start + k] = ch;
            }
            r[start + l.len()] = ' ';
        }
        r
    }

    fn new(label: &'static str) -> Self {
        Self {
            text: Vec::new(),
            caret: 0,
            screen: vec![vec![' '; COLS]; ROWS],
            label,
        }
    }

    /// Ink's diff of the whole box against the screen, one DEC 2026 chunk.
    fn render(&mut self) -> Vec<u8> {
        let (rows, (cr, cc)) = self.rows_for();
        let mut s = String::new();
        s.push_str(SYNC_BEGIN);
        s.push_str("\x1b[?25l");
        let top = rows[0].0;
        // every row from the top rule down to the bottom rule
        for (r, content, is_rule) in rows.iter() {
            let mut new = content.clone();
            new.resize(COLS, ' ');
            let old = self.screen[*r].clone();
            if old == new {
                continue;
            }
            s.push_str(&format!("\x1b[{};1H", r + 1));
            s.push_str(if *is_rule { RULE_PEN } else { TEXT_PEN });
            let mut cur = 0usize;
            for col in 0..COLS {
                if (col..COLS).all(|i| new[i] == ' ') {
                    if (col..COLS).any(|i| old[i] != ' ') {
                        if cur != col {
                            s.push_str(&format!("\x1b[{}G", col + 1));
                        }
                        s.push_str("\x1b[K");
                    }
                    break;
                }
                if old[col] == new[col] {
                    continue;
                }
                if cur != col {
                    s.push_str(&format!("\x1b[{}G", col + 1));
                }
                s.push(new[col]);
                cur = col + 1;
            }
            self.screen[*r] = new;
        }
        // rows above the new top rule that the old box used are untouched
        // (the box only grows here).
        let _ = top;
        s.push_str(&format!("\x1b[{};1H", ROWS));
        s.push_str(&format!("\x1b[{};{}H", cr + 1, cc + 1));
        s.push_str("\x1b[?25h");
        s.push_str(SYNC_END);
        s.into_bytes()
    }
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Char(char),
}

pub struct Sim {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    next_frame: Instant,
    out: Vec<GlowQuad>,
    buf: Vec<char>,
    blink_seen: u64,
    comp: Composer,
    /// keys pressed, not yet rendered
    pending: Vec<char>,
    /// the frame train: frames suppressed while `now` is inside one of these
    /// (ms since t0) windows — aterm's own frames starved.
    frame_holes: Vec<(u64, u64)>,
    pub log: Vec<String>,
    pub holes_seen: Vec<String>,
    pub drop_far: bool,
    pub verbose: bool,
    last_decl_seq: u64,
    pub trace_left: u32,
    pub trace_rows: Vec<u16>,
    /// The typed region at the end: (row, first col) — every cell from it
    /// to the caret should carry live light.
    pub cov: Option<(u16, u16)>,
}

impl Sim {
    pub fn new(label: &'static str) -> Self {
        let g = Geom {
            cw: CW,
            ch: CH,
            rows: ROWS,
            cols: COLS,
            origin_x: 0,
            origin_y: 0,
            win_w: (COLS * CW) as u16,
            win_h: (ROWS * CH) as u16,
            head: 0,
        };
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(b"\x1b[?1049h\x1b[2J\x1b[H");
        term.process(
            format!(
                "\x1b[{};1H  \u{23f5}\u{23f5} bypass permissions on (shift+tab to cycle)",
                ROWS
            )
            .as_bytes(),
        );
        let now = Instant::now();
        let mut s = Self {
            term,
            glow: CursorGlow::default(),
            cfg: cfg(),
            g,
            t0: now,
            now,
            next_frame: now,
            out: Vec::new(),
            buf: Vec::new(),
            blink_seen: 0,
            comp: Composer::new(label),
            pending: Vec::new(),
            frame_holes: Vec::new(),
            log: Vec::new(),
            holes_seen: Vec::new(),
            drop_far: false,
            verbose: false,
            last_decl_seq: 0,
            trace_left: 0,
            trace_rows: vec![49, 50],
            cov: None,
        };
        let b = s.comp.render();
        s.term.process(&b);
        s.frame();
        s
    }

    fn ms(&self) -> u64 {
        self.now.duration_since(self.t0).as_millis() as u64
    }

    fn frame(&mut self) {
        let t = self.ms();
        if self.frame_holes.iter().any(|&(a, b)| t >= a && t < b) {
            return;
        }
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        self.term.row_cols_into(usize::from(c.row), &mut self.buf);
        self.glow.observe_row(c.row, c.col, &self.buf, self.now);
        self.glow.observe_ribbon_row(c.row, &self.buf);
        let mut rows = [0u16; WROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            if r == c.row {
                continue;
            }
            if self.drop_far && r.abs_diff(c.row) > 1 {
                continue;
            }
            self.term.row_cols_into(usize::from(r), &mut self.buf);
            self.glow.observe_ribbon_row(r, &self.buf);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        self.scan_holes();
        self.drain_declines();
        if self.trace_left > 0 {
            self.trace_left -= 1;
            let rows = self.trace_rows.clone();
            for r in rows {
                let d = self.cells_detail(r);
                self.log.push(format!(
                    "{}ms FRAME row {r} caret {:?}: {d}",
                    self.ms(),
                    cur
                ));
            }
        }
    }

    /// Every ribbon cell on `row`: `col:cohort:t:born:flags`.
    pub fn cells_detail(&self, row: u16) -> String {
        let Some(rib) = self.glow.v2_ribbon() else {
            return String::new();
        };
        let mut v: Vec<_> = rib.cells().iter().filter(|c| c.row == row).collect();
        v.sort_by_key(|c| (c.col, c.born));
        let mut out = String::new();
        for c in v {
            let born = c.born.saturating_duration_since(self.t0).as_millis();
            let mut f = String::new();
            if c.leaving() {
                f.push('L');
            }
            if c.retire_at.is_some() {
                f.push('R');
            }
            if c.retract_at.is_some() {
                f.push('X');
            }
            if !c.typing {
                f.push('n');
            }
            if matches!(c.layer, aterm_effects::rainbow_kitty::ribbon::Layer::Over) {
                f.push('O');
            }
            out.push_str(&format!(
                "{}:c{}:{:.3}:{}{} ",
                c.col,
                c.cohort,
                c.t,
                born,
                if f.is_empty() {
                    String::new()
                } else {
                    format!(":{f}")
                }
            ));
        }
        let cos: Vec<String> = rib
            .cohorts()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| {
                format!(
                    "[co{} {}..{} a{} t0={:.3} alive{} ab{} w{} {:?}]",
                    c.id,
                    c.col0,
                    c.col1,
                    c.anchor_col,
                    c.t0,
                    c.alive_at.saturating_duration_since(self.t0).as_millis(),
                    c.abandoned as u8,
                    c.wake as u8,
                    c.phase
                )
            })
            .collect();
        out.push_str(&cos.join(""));
        out
    }

    fn drain_declines(&mut self) {
        let recs: Vec<String> = self
            .glow
            .admission_log()
            .filter(|r| r.seq > self.last_decl_seq)
            .map(|r| {
                format!(
                    "{}ms seq{} {:?} {} {} {:?}->{:?}",
                    r.at.duration_since(self.t0).as_millis(),
                    r.seq,
                    r.phase,
                    r.reason,
                    r.licence,
                    r.origin,
                    r.target
                )
            })
            .collect();
        if let Some(max) = self.glow.admission_log().map(|r| r.seq).max() {
            self.last_decl_seq = self.last_decl_seq.max(max);
        }
        for r in recs {
            if self.verbose || r.contains("Declined") {
                self.log.push(r);
            }
        }
    }

    /// Live (not leaving) ribbon columns on `row`.
    pub fn live(&self, row: u16) -> Vec<u16> {
        let mut v: Vec<u16> = self
            .glow
            .v2_ribbon()
            .map(|r| {
                r.cells()
                    .iter()
                    .filter(|c| c.row == row && !c.leaving())
                    .map(|c| c.col)
                    .collect()
            })
            .unwrap_or_default();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Interior holes on every row: a column between two live cells of the
    /// SAME row that has no live cell of its own.
    fn scan_holes(&mut self) {
        let t = self.ms();
        let Some(rib) = self.glow.v2_ribbon() else {
            return;
        };
        let mut rows: Vec<u16> = rib.cells().iter().map(|c| c.row).collect();
        rows.sort_unstable();
        rows.dedup();
        for r in rows {
            let live = self.live(r);
            if live.len() < 2 {
                continue;
            }
            for w in live.windows(2) {
                if w[1] > w[0] + 1 {
                    let mut row_chars = Vec::new();
                    self.term.row_cols_into(usize::from(r), &mut row_chars);
                    let hole: String = (w[0] + 1..w[1])
                        .map(|c| row_chars.get(c as usize).copied().unwrap_or('?'))
                        .collect();
                    let desc = format!(
                        "t={t}ms row {r} hole cols {}..{} [{}] between live {} and {}",
                        w[0] + 1,
                        w[1] - 1,
                        hole,
                        w[0],
                        w[1]
                    );
                    self.holes_seen.push(desc);
                }
            }
        }
    }

    fn advance_to_ms(&mut self, t: u64) {
        let target = self.t0 + Duration::from_millis(t);
        while self.next_frame <= target {
            self.now = self.next_frame;
            self.frame();
            self.next_frame = self.now + Duration::from_micros(FRAME_US);
        }
        self.now = self.now.max(target);
    }

    fn press(&mut self, ch: char) {
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        let shifted = ch.is_uppercase() || "!@#$%^&*()_+{}|:\"<>?~".contains(ch);
        self.glow
            .note_typed_expected(self.now, 1, shifted && ch != ' ', class, ch);
        self.pending.push(ch);
    }

    fn render(&mut self) {
        let keys: Vec<char> = std::mem::take(&mut self.pending);
        if keys.is_empty() {
            return;
        }
        for ch in keys.iter().copied() {
            self.comp.text.insert(self.comp.caret, ch);
            self.comp.caret += 1;
        }
        let b = self.comp.render();
        if self.verbose {
            self.log.push(format!(
                "{}ms RENDER {:?} caret->{:?}",
                self.ms(),
                keys.iter().collect::<String>(),
                self.comp.rows_for().1
            ));
        }
        self.term.process(&b);
        if self.verbose {
            self.trace_left = 3;
        }
    }

    /// Set the composer's text instantly (no keys), the caret at `caret`,
    /// rendered and one frame shown; the ribbon untouched.
    pub fn preload(&mut self, text: &str, caret: usize) {
        self.comp.text = text.chars().collect();
        self.comp.caret = caret;
        let b = self.comp.render();
        self.term.process(&b);
        let t = self.ms() + 17;
        self.advance_to_ms(t);
    }

    /// Run a schedule: `keys` (press ms, char), `renders` (render ms) — a
    /// render echoes every key pressed at or before it.
    pub fn run(&mut self, keys: &[(u64, char)], renders: &[u64]) {
        let mut ev: Vec<(u64, u8, usize)> = Vec::new();
        for (i, k) in keys.iter().enumerate() {
            ev.push((k.0, 0, i));
        }
        for (i, &r) in renders.iter().enumerate() {
            ev.push((r, 1, i));
        }
        ev.sort();
        for (t, kind, i) in ev {
            self.advance_to_ms(t);
            self.now = self.t0 + Duration::from_millis(t);
            if kind == 0 {
                self.press(keys[i].1);
            } else {
                self.render();
            }
        }
    }

    pub fn idle(&mut self, ms: u64) {
        let t = self.ms() + ms;
        self.advance_to_ms(t);
    }

    pub fn caret(&self) -> (u16, u16) {
        let c = self.term.cursor();
        (c.row, c.col)
    }

    pub fn row_text(&self, r: u16) -> String {
        let mut b = Vec::new();
        self.term.row_cols_into(usize::from(r), &mut b);
        b.into_iter().collect()
    }

    /// Dark cells of the typed region at the caret row: `[col0, caret)`.
    pub fn dark(&self) -> String {
        let Some((row, c0)) = self.cov else {
            return String::new();
        };
        let (cr, cc) = self.caret();
        if cr != row {
            return format!("caret row {cr} != {row}");
        }
        let live = self.live(row);
        let text = self.row_text(row);
        let chars: Vec<char> = text.chars().collect();
        let dark: Vec<String> = (c0..cc)
            .filter(|c| !live.contains(c))
            .map(|c| format!("{c}{:?}", chars.get(c as usize).copied().unwrap_or('?')))
            .collect();
        format!("dark[{}]={}", dark.len(), dark.join(","))
    }

    pub fn tally(&self) -> String {
        let t = self.glow.admission_tally();
        let s = self.glow.v2_status();
        format!(
            "licensed={} declined={} last={:?} followed={:?} retired={:?}",
            t.licensed,
            t.declined,
            t.last_decline_reason,
            s.map(|s| s.followed),
            s.map(|s| s.retired)
        )
    }

    /// Text and live-ribbon map of the composer rows.
    pub fn dump(&self, what: &str) {
        eprintln!(
            "   -- dump {what} at {}ms caret {:?}",
            self.ms(),
            self.caret()
        );
        for r in 44..(ROWS as u16 - 1) {
            let text = self.row_text(r);
            let live = self.live(r);
            let all: Vec<u16> = self
                .glow
                .v2_ribbon()
                .map(|rb| {
                    rb.cells()
                        .iter()
                        .filter(|c| c.row == r)
                        .map(|c| c.col)
                        .collect()
                })
                .unwrap_or_default();
            if text.trim().is_empty() && all.is_empty() {
                continue;
            }
            let map: String = (0..COLS as u16)
                .map(|c| {
                    if live.contains(&c) {
                        '#'
                    } else if all.contains(&c) {
                        'x'
                    } else {
                        '.'
                    }
                })
                .collect();
            eprintln!("   {r:2} |{}|", text.trim_end());
            eprintln!("      |{}|", map.trim_end_matches('.'));
        }
    }

    /// The live cells with their `t` on `row`, `(col, t)`.
    pub fn walk(&self, row: u16) -> Vec<(u16, f32)> {
        let mut v: Vec<(u16, f32)> = self
            .glow
            .v2_ribbon()
            .map(|r| {
                r.cells()
                    .iter()
                    .filter(|c| c.row == row && !c.leaving())
                    .map(|c| (c.col, c.t))
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by_key(|c| c.0);
        v
    }
}

/// Keys of `s` from `start` ms, `gap` ms apart.
fn keys_of(s: &str, start: u64, gap: u64) -> Vec<(u64, char)> {
    s.chars()
        .enumerate()
        .map(|(i, c)| (start + gap * i as u64, c))
        .collect()
}

/// A render per key at `delay` after it — but serialized: a render never
/// precedes the previous one (the app processes input in order), and a
/// render only ever happens once per `min_gap` (Ink's throttle).
fn renders_for(keys: &[(u64, char)], delay: impl Fn(usize) -> u64) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();
    let mut last = 0u64;
    for (i, k) in keys.iter().enumerate() {
        let r = (k.0 + delay(i)).max(last);
        if out.last() != Some(&r) {
            out.push(r);
        }
        last = r;
    }
    out
}

const L1: &str = "ok. I also noticed that there was a typing latency stall on this machine. there was a message alert about it.";
const L2: &str = "it said that the CPUs were busy. Keep designing superior solutions so that the end user experience is not";

/// The owner's second screen: typing BEFORE `[Image #1]`; the Space after
/// `hthe` pushes the placeholder onto a new row and the box grows.
fn typing_before_the_placeholder(gap: u64, delay: u64) -> Sim {
    let mut s = Sim::new("workspace");
    // Every admission logged: the non-vacuity check reads them.
    s.verbose = true;
    let head =
        format!("{L1} {L2} interrupted even under very heavy max load. also: see this screenshot.");
    let tail = " [Image #1]";
    s.preload(&format!("{head}{tail}"), head.chars().count());
    let typed = " There is a problem wit hthe cu";
    s.cov = Some((49, 72));
    let keys = keys_of(typed, s.ms() + 100, gap);
    let r = renders_for(&keys, |_| delay);
    s.run(&keys, &r);
    s.idle(120);
    s
}

/// Whether the log shows a licensed echo move that crossed rows — the shape
/// this file exists for (the non-vacuity check).
fn crossed_rows(s: &Sim) -> bool {
    s.log.iter().any(|l| {
        l.contains(" Licensed ")
            && l.split(" key (")
                .nth(1)
                .and_then(|m| m.split_once(")->("))
                .is_some_and(|(from, to)| {
                    let row = |p: &str| p.split(',').next().map(|x| x.trim().to_string());
                    row(from) != row(to)
                })
    })
}

#[test]
fn a_space_that_grows_the_box_lights_its_own_cell() {
    for gap in [60u64, 100, 150] {
        for delay in [2u64, 30, 100, 260, 1000] {
            let s = typing_before_the_placeholder(gap, delay);
            assert!(
                crossed_rows(&s),
                "gap {gap} delay {delay}: the Space's echo must cross rows, or this proves nothing"
            );
            assert!(
                s.holes_seen.is_empty(),
                "gap {gap} delay {delay}: a dark cell inside the typed run: {} | {:?}",
                s.dark(),
                s.holes_seen.first()
            );
        }
    }
}

/// The owner's first screen, stalled: `interrupted` typed at the end of the
/// second line; its `n`'s echo held `stall` ms, so the wrap's repaint brings
/// the whole word at once.
fn interrupted_wrapped_in_a_stall(gap: u64, stall: u64, at_key: usize) -> Sim {
    let mut s = Sim::new("workspace");
    s.verbose = true;
    let pre = format!("{L1} ");
    let n = pre.chars().count();
    s.preload(&pre, n);
    let lead = keys_of(L2, s.ms() + 100, gap);
    let rl = renders_for(&lead, |_| 2);
    s.run(&lead, &rl);
    s.idle(200);
    let typed = " interrupted even under";
    s.cov = Some((50, 2));
    let keys = keys_of(typed, s.ms() + 100, gap);
    let r = renders_for(&keys, |i| if i == at_key { stall } else { 2 });
    s.run(&keys, &r);
    s.idle(120);
    s
}

/// Whether the log shows a licensed same-row move that went back at least
/// `cells` — the wrap's re-anchor (the non-vacuity check).
fn re_anchored(s: &Sim, cells: u16) -> bool {
    s.log.iter().any(|l| {
        l.contains(" Licensed ")
            && l.split(" key (")
                .nth(1)
                .and_then(|m| m.split_once(")->("))
                .is_some_and(|(from, to)| {
                    let pair = |p: &str| -> Option<(u16, u16)> {
                        let p = p.trim_end_matches(')');
                        let (r, c) = p.split_once(',')?;
                        Some((r.trim().parse().ok()?, c.trim().parse().ok()?))
                    };
                    matches!((pair(from), pair(to)), (Some(f), Some(t)) if f.0 == t.0 && f.1 >= t.1 + cells)
                })
    })
}

#[test]
fn a_word_wrapped_in_a_stalled_repaint_is_lit_whole() {
    for (gap, stall, at_key) in [(60u64, 600u64, 2usize), (100, 1000, 2), (150, 2171, 3)] {
        let s = interrupted_wrapped_in_a_stall(gap, stall, at_key);
        assert!(
            re_anchored(&s, 40),
            "gap {gap} stall {stall}@k{at_key}: the wrap's re-anchor must happen, or this proves nothing"
        );
        assert!(
            s.holes_seen.is_empty(),
            "gap {gap} stall {stall}@k{at_key}: a dark cell inside the typed run: {} | {:?}",
            s.dark(),
            s.holes_seen.first()
        );
    }
}

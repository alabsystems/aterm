// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Three composer gaps, and the rainbow lit through each.
//!
//! Claude Code's composer at 113 columns, modelled row for row: the text wraps
//! at `COLS - 4`, the box is bottom-anchored between its two rules and grows up
//! a row when the text needs one. Keys are stamped at the press; the app echoes
//! each late and batched, and aterm's frames run on a 16.7 ms train.
//!
//! * `wit hthe▯cu [Image #1]`: typing before a trailing `[Image #1]`, the Space
//!   after `hthe` pushes the placeholder onto a new row; the box grows in the
//!   frame the Space echoes, and the caret moves `(R, c) → (R-1, c+1)` with its
//!   line. The hand rides its line (`rainbow_kitty::Engine::lift_hand`): the
//!   follow pass's verdict on the hand's own line is read before the move is
//!   judged, so the echo is the key's same-row `+1` in the text's frame, also
//!   when it comes long after the key (the unpaid press pays) and when a torn
//!   present shows the grown box before the caret (the follow pass carries the
//!   hand while the caret is withheld). The line is judged on its own typed
//!   glyphs, so it rides inside a hop's wake; a lone typed glyph rides with the
//!   caret as its second witness; and the box moving down a row carries the
//!   hand the same way.
//! * `interrupted`, wrapped while the app stalls: only its `i` has echoed when
//!   the word wraps, and the rest comes in one late repaint.
//!   `Engine::note_rewrap_starts` reads where the relaid text starts from the
//!   landing row's own sample, so the batch's cells are lit.
//! * `the▯subtrees` (2026-10-16): `thes`, Backspace and the Space rendered as
//!   one repaint by the lagging app, so the caret ends where it began and the
//!   typo's column goes blank. The Backspace's retreat was never seen and the
//!   column spells the key typed since, so the repaint is the erase's retreat
//!   and the Space's advance (`CursorGlow::merged_correction`); a replacement
//!   with no erase owed claims nothing. The owner's mid-line take, the typo's
//!   echo landing after the Backspace and multi-key corrections are
//!   `torn_repaint_hidden_caret.rs`'s.
//!
//! Each case asserts that the move it exists for happened.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::{Deref, DerefMut};
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
    }
}

/// Greedy word wrap at `TW`, a break's space dropped, the caret (an index
/// into `text`) mapped to (line, col-in-line).
fn wrap(text: &[char], caret: usize) -> (Vec<Vec<char>>, (usize, usize)) {
    let mut lines: Vec<Vec<char>> = vec![Vec::new()];
    // Text index -> (line, col).
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
    footer: usize,
}

impl Composer {
    fn rows_for(&self) -> (Vec<ScreenRow>, (usize, usize)) {
        let (lines, (cl, cc)) = wrap(&self.text, self.caret);
        let n = lines.len();
        let bottom_rule = ROWS - 2 - self.footer;
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
            footer: 0,
        }
    }

    /// Ink's diff of the whole box against the screen, one DEC 2026 chunk.
    fn render(&mut self) -> Vec<u8> {
        let (rows, (cr, cc)) = self.rows_for();
        let mut s = String::new();
        s.push_str(SYNC_BEGIN);
        s.push_str("\x1b[?25l");
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
        s.push_str(&format!("\x1b[{};1H", ROWS));
        s.push_str(&format!("\x1b[{};{}H", cr + 1, cc + 1));
        s.push_str("\x1b[?25h");
        s.push_str(SYNC_END);
        s.into_bytes()
    }
}

/// A Backspace in a key schedule.
const BACKSPACE: char = '\u{8}';

/// A fixture surface owns the production family. Existing light assertions read
/// that family's one glow engine; shipping presents use its canonical step.
#[derive(Default)]
pub(crate) struct PresentedGlow(aterm_effects::pipeline::EffectsPipeline);

impl Deref for PresentedGlow {
    type Target = CursorGlow;
    fn deref(&self) -> &CursorGlow {
        &self.0.cursor().glow
    }
}

impl DerefMut for PresentedGlow {
    fn deref_mut(&mut self) -> &mut CursorGlow {
        &mut self.0.cursor_mut().glow
    }
}

pub struct Sim {
    term: Terminal,
    glow: PresentedGlow,
    cfg: GlowConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    next_frame: Instant,
    out: Vec<GlowQuad>,
    buf: Vec<char>,
    blink_seen: u64,
    last_blink: Option<Instant>,
    comp: Composer,
    /// keys pressed, not yet rendered
    pending: Vec<char>,
    pub log: Vec<String>,
    pub holes_seen: Vec<String>,
    pub verbose: bool,
    last_decl_seq: u64,
    /// The typed region at the end: (row, first col) — every cell from it
    /// to the caret should carry live light.
    pub cov: Option<(u16, u16)>,
    /// `(key index, ms)`: the render that echoes that key ([`Sim::run`])
    /// stops before its park and the rest lands `ms` later (the app stalled
    /// mid-write; the host presents the half-written frame, the caret hidden).
    pub tear: Option<(usize, u64)>,
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
            glow: PresentedGlow::default(),
            cfg: cfg(),
            g,
            t0: now,
            now,
            next_frame: now,
            out: Vec::new(),
            buf: Vec::new(),
            blink_seen: 0,
            last_blink: None,
            comp: Composer::new(label),
            pending: Vec::new(),
            log: Vec::new(),
            holes_seen: Vec::new(),
            verbose: false,
            last_decl_seq: 0,
            cov: None,
            tear: None,
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
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.last_blink = Some(self.now);
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
            self.term.row_cols_into(usize::from(r), &mut self.buf);
            self.glow.observe_ribbon_row(r, &self.buf);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        self.scan_holes();
        self.drain_declines();
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
    /// same row that has no live cell of its own.
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
        if ch == BACKSPACE {
            self.glow.note_backspace_erasing(self.now, Some(1));
            self.pending.push(ch);
            return;
        }
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
        let _ = self.render_head(false);
    }

    /// [`Sim::render`], its bytes cut before the park and the rest landing
    /// `torn` ms later, the frames between presented; for a caller that
    /// schedules nothing meanwhile ([`Sim::run`] queues the rest as its own
    /// event).
    fn render_torn(&mut self, torn: Option<u64>) {
        let Some(ms) = torn else {
            self.render();
            return;
        };
        if let Some(rest) = self.render_head(true) {
            self.idle(ms);
            self.term.process(&rest);
        }
    }

    /// Echo every pending key: the composer repaints. With `torn` only the
    /// bytes before the park are processed and the rest is returned for the
    /// caller to land.
    fn render_head(&mut self, torn: bool) -> Option<Vec<u8>> {
        let keys: Vec<char> = std::mem::take(&mut self.pending);
        if keys.is_empty() {
            return None;
        }
        for ch in keys.iter().copied() {
            if ch == BACKSPACE {
                if self.comp.caret > 0 {
                    self.comp.caret -= 1;
                    self.comp.text.remove(self.comp.caret);
                }
                continue;
            }
            self.comp.text.insert(self.comp.caret, ch);
            self.comp.caret += 1;
        }
        let b = self.comp.render();
        if torn {
            let park = format!("\x1b[{ROWS};1H");
            let cut = b
                .windows(park.len())
                .rposition(|w| w == park.as_bytes())
                .expect("the park");
            self.term.process(&b[..cut]);
            return Some(b[cut..].to_vec());
        }
        if self.verbose {
            self.log.push(format!(
                "{}ms RENDER {:?} caret->{:?}",
                self.ms(),
                keys.iter().collect::<String>(),
                self.comp.rows_for().1
            ));
        }
        self.term.process(&b);
        None
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
    /// render echoes every key pressed at or before it. The render that
    /// echoes [`Sim::tear`]'s key lands its rest as its own event, and a render
    /// due before then waits for it (the app is stalled mid-write). The clock
    /// only moves forward.
    pub fn run(&mut self, keys: &[(u64, char)], renders: &[u64]) {
        const KEY: u8 = 0;
        const REST: u8 = 1;
        const RENDER: u8 = 2;
        let torn = self.tear.and_then(|(k, ms)| {
            let press = keys.get(k)?.0;
            let render = renders.iter().position(|&r| r >= press)?;
            Some((render, ms))
        });
        let mut ev: BinaryHeap<Reverse<(u64, u8, usize)>> = BinaryHeap::new();
        for (i, k) in keys.iter().enumerate() {
            ev.push(Reverse((k.0, KEY, i)));
        }
        for (i, &r) in renders.iter().enumerate() {
            ev.push(Reverse((r, RENDER, i)));
        }
        let mut rest: Option<(u64, Vec<u8>)> = None;
        while let Some(Reverse((t, kind, i))) = ev.pop() {
            assert!(t >= self.ms(), "the schedule ran the clock back to {t} ms");
            self.advance_to_ms(t);
            match kind {
                KEY => self.press(keys[i].1),
                REST => {
                    if let Some((_, bytes)) = rest.take() {
                        self.term.process(&bytes);
                    }
                }
                _ => {
                    if let Some(&(due, _)) = rest.as_ref() {
                        ev.push(Reverse((due, RENDER, i)));
                        continue;
                    }
                    let ms = torn.and_then(|(render, ms)| (render == i).then_some(ms));
                    if let Some(bytes) = self.render_head(ms.is_some()) {
                        let due = t + ms.unwrap_or(0);
                        ev.push(Reverse((due, REST, i)));
                        rest = Some((due, bytes));
                    }
                }
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
}

/// Keys of `s` from `start` ms, `gap` ms apart.
fn keys_of(s: &str, start: u64, gap: u64) -> Vec<(u64, char)> {
    s.chars()
        .enumerate()
        .map(|(i, c)| (start + gap * i as u64, c))
        .collect()
}

/// A render per key at `delay(i)` after it, serialized: a render never
/// precedes the previous one (the app processes input in order), and renders
/// that land on the same ms are one.
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

/// The second screen: typing before `[Image #1]`; the Space after `hthe`
/// pushes the placeholder onto a new row and the box grows.
fn typing_before_the_placeholder(gap: u64, delay: u64) -> (Sim, u16) {
    let lead = " There is a problem wit hthe";
    let keys = |s: &Sim| keys_of(&format!("{lead} cu"), s.ms() + 100, gap);
    placeholder_take(keys, |_| delay, None)
}

/// The composer before `[Image #1]` with the keys `keys` makes and their
/// renders `delay(i)` after them, render `tear` torn; returns the take and
/// the caret's screen row before the first key.
fn placeholder_take(
    keys: impl Fn(&Sim) -> Vec<(u64, char)>,
    delay: impl Fn(usize) -> u64,
    tear: Option<(usize, u64)>,
) -> (Sim, u16) {
    let mut s = Sim::new("workspace");
    s.verbose = true;
    let head =
        format!("{L1} {L2} interrupted even under very heavy max load. also: see this screenshot.");
    let tail = " [Image #1]";
    s.preload(&format!("{head}{tail}"), head.chars().count());
    let row0 = s.caret().0;
    s.cov = Some((49, 72));
    s.tear = tear;
    let keys = keys(&s);
    let r = renders_for(&keys, delay);
    s.run(&keys, &r);
    s.idle(120);
    (s, row0)
}

/// The Space after `hthe` and its index among the take's keys.
const GROWTH_KEY: usize = " There is a problem wit hthe".len();

/// Whether the box grew under the take: the caret's line is a row higher
/// than when typing began (the non-vacuity check).
fn grew(s: &Sim, row0: u16) -> bool {
    s.caret().0 + 1 == row0
}

#[test]
fn a_space_that_grows_the_box_lights_its_own_cell() {
    for gap in [60u64, 100, 150] {
        for delay in [2u64, 30, 100, 260, 1000] {
            let (s, row0) = typing_before_the_placeholder(gap, delay);
            assert!(
                grew(&s, row0),
                "gap {gap} delay {delay}: the Space's echo must grow the box, or this proves nothing"
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

/// The first screen, stalled: `interrupted` typed at the end of the second
/// line, key `at_key`'s echo held `stall` ms, so the wrap's repaint brings the
/// rest of the word at once.
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

/// The Space that grows the box, echoed `late` ms after its key while the
/// hand pauses: no stamp is fresh when the grown box shows, so only the
/// key's unpaid press can pay its `+1`, which it does in the text's frame.
#[test]
fn a_growth_echo_after_a_pause_lights_its_own_cell() {
    for late in [300u64, 600, 1_200] {
        let keys = |s: &Sim| {
            let mut k = keys_of(" There is a problem wit hthe ", s.ms() + 100, 100);
            let space = k.last().expect("the Space").0;
            k.push((space + late + 300, 'c'));
            k.push((space + late + 400, 'u'));
            k
        };
        let (s, row0) = placeholder_take(keys, |i| if i == GROWTH_KEY { late } else { 2 }, None);
        assert!(
            grew(&s, row0),
            "late {late}: the box must grow, or this proves nothing"
        );
        assert!(
            s.holes_seen.is_empty(),
            "late {late}: a dark cell inside the typed run: {} | {:?}",
            s.dark(),
            s.holes_seen.first()
        );
    }
}

/// The Space that grows the box, its render torn before the park: the host
/// presents the grown box with the caret hidden for `ms`, so the follow pass
/// carries the line a frame before the caret shows, and the hand rides with it.
#[test]
fn a_growth_echo_torn_before_its_caret_lights_its_own_cell() {
    for ms in [40u64, 200, 600] {
        let keys = |s: &Sim| {
            let mut k = keys_of(" There is a problem wit hthe ", s.ms() + 100, 100);
            let space = k.last().expect("the Space").0;
            k.push((space + ms + 300, 'c'));
            k.push((space + ms + 400, 'u'));
            k
        };
        let (s, row0) = placeholder_take(keys, |_| 2, Some((GROWTH_KEY, ms)));
        assert!(
            grew(&s, row0),
            "torn {ms} ms: the box must grow, or this proves nothing"
        );
        assert!(
            s.holes_seen.is_empty(),
            "torn {ms} ms: a dark cell inside the typed run: {} | {:?}",
            s.dark(),
            s.holes_seen.first()
        );
    }
}

/// The old text the hand edits inside of: one line, padded at its start to
/// `TW - typed` columns, so `typed` keys fill it and the next key pushes its
/// last word onto a new row.
fn old_line(typed: usize) -> String {
    let mut line = String::from(
        "alpha beta gamma delta epsilon zeta eta theta iota kapa lambda mu nu xi omicron pi rho sigma tau upsilonx",
    );
    while line.chars().count() < TW - typed {
        line.insert(0, 'q');
    }
    line
}

/// Where the hand edits the old text.
const EDIT_COL: u16 = 40;

/// An edit inside old text whose key grows the box. The hand reaches column
/// [`EDIT_COL`] by a hop from the line's end (Home, a word hop, a long arrow
/// run: one navigation, one jump) with `hop`, else with no key (a click);
/// then `typed`, the growth key and one more key, `gap` ms apart, each echoed
/// 3 ms after it except the growth key's, echoed `late` ms after it or torn
/// for `torn` ms. Returns the take, the row the hand's line moved to and the
/// growth key's press (ms).
fn edit_that_grows_the_box(
    typed: &str,
    hop: bool,
    gap: u64,
    late: u64,
    torn: Option<u64>,
) -> (Sim, u16, u64) {
    let mut s = Sim::new("");
    s.verbose = true;
    let n = typed.chars().count();
    let line = old_line(n);
    if hop {
        s.preload(&line, line.chars().count());
        s.idle(100);
        s.glow.note_navigation(s.now);
        s.comp.caret = usize::from(EDIT_COL) - 2;
        let bytes = s.comp.render();
        s.term.process(&bytes);
        s.idle(60);
    } else {
        s.preload(&line, usize::from(EDIT_COL) - 2);
    }
    let (row, col) = s.caret();
    assert_eq!(col, EDIT_COL, "the hand must stand inside the old text");
    let keys = keys_of(&format!("{typed}YW"), s.ms() + 20, gap);
    s.tear = torn.map(|ms| (n, ms));
    let renders = renders_for(&keys, |i| if i == n { late } else { 3 });
    s.run(&keys, &renders);
    s.idle(150);
    s.cov = Some((row - 1, EDIT_COL));
    (s, row - 1, keys[n].0)
}

/// Whether the hand's band rode with its line: the band under `col` on `row`
/// was born before `before` ms, carried with its walk and clocks rather than
/// begun again where the line now stands.
fn band_rode(s: &Sim, row: u16, col: u16, before: u64) -> bool {
    let before = s.t0 + Duration::from_millis(before);
    s.glow
        .v2_ribbon()
        .expect("rainbow kitty")
        .cohorts()
        .iter()
        .any(|c| c.row == row && (c.col0..c.col1).contains(&col) && c.born < before)
}

/// The hop's wake rides with the hand. Inside a hop's wake the typed keys'
/// band holds the wake's cells too (`Ribbon::fold_wakes_into_band`), and the
/// growth key's insert pushes that tail along, so its every record reads as
/// a hole one row up. The hand's line is judged on the hand's typed glyphs
/// alone, the wake riding with them: the growth echo, late or torn, lights
/// its own cell, and the band rides up with its line.
#[test]
fn a_growth_echo_inside_a_hops_wake_lights_its_own_cell() {
    for (late, torn) in [(600u64, None), (3, Some(200u64))] {
        let (s, row, grew_at) = edit_that_grows_the_box("xyZ", true, 150, late, torn);
        assert_eq!(
            s.caret(),
            (row, EDIT_COL + 5),
            "late {late} torn {torn:?}: the box must grow under the hand, or this proves nothing"
        );
        assert_eq!(
            s.dark(),
            "dark[0]=",
            "late {late} torn {torn:?}: the typed run must be lit | {:?}",
            s.holes_seen.first()
        );
        assert!(
            s.holes_seen.is_empty(),
            "late {late} torn {torn:?}: a dark cell inside the typed run: {:?}",
            s.holes_seen.first()
        );
        assert!(
            band_rode(&s, row, EDIT_COL, grew_at),
            "late {late} torn {torn:?}: the hand's band must ride up with its line"
        );
    }
}

/// One typed glyph on the hand's line before the growth key. With the caret
/// shown on the grown row the caret is the second witness to the glyph's
/// move; with the caret withheld by a torn present the glyph waits for it
/// (`Witness::await_caret`), so the growth echo lights its own cell either
/// way and the glyph's band rides up with its line.
#[test]
fn a_growth_echo_after_one_typed_glyph_lights_its_own_cell() {
    for (late, torn) in [(3u64, None), (600, None), (3, Some(200u64))] {
        let (s, row, grew_at) = edit_that_grows_the_box("Z", false, 400, late, torn);
        assert_eq!(
            s.caret(),
            (row, EDIT_COL + 3),
            "late {late} torn {torn:?}: the box must grow under the hand, or this proves nothing"
        );
        assert_eq!(
            s.dark(),
            "dark[0]=",
            "late {late} torn {torn:?}: the typed run must be lit | {:?}",
            s.holes_seen.first()
        );
        assert!(
            band_rode(&s, row, EDIT_COL, grew_at),
            "late {late} torn {torn:?}: the glyph's band must ride up with its line"
        );
    }
}

/// The box moves down a row under a torn edit (a footer below it shrank):
/// the hand, moved up onto the line above a freshly typed one, types one
/// glyph, then a key whose echo repaints the box a row lower, torn. Both of
/// the hand's lines ride down, and the hand rides its own line once: the
/// edit stays lit.
#[test]
fn a_torn_box_move_down_keeps_the_edit_lit() {
    for torn in [40u64, 200] {
        let mut s = Sim::new("");
        s.verbose = true;
        s.comp.footer = 1;
        let above = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma";
        let text = format!("{above} tauupsilonphichipsiomega");
        s.preload(&text, text.chars().count());
        let (row, end) = s.caret();
        let keys = keys_of("qwertyuiop", s.ms() + 100, 60);
        let renders = renders_for(&keys, |_| 2);
        s.run(&keys, &renders);
        s.idle(40);
        // Up onto the line above, inside the columns just typed below.
        let col = end + 5;
        s.glow.note_navigation(s.now);
        s.comp.caret = usize::from(col) - 2;
        let bytes = s.comp.render();
        s.term.process(&bytes);
        s.idle(40);
        assert_eq!(
            s.caret(),
            (row - 1, col),
            "the hand must stand on the line above"
        );
        s.idle(20);
        s.press('Z');
        s.idle(3);
        s.render();
        s.idle(50);
        let grew_at = s.ms();
        s.press('Y');
        s.idle(3);
        s.comp.footer = 0;
        s.render_torn(Some(torn));
        s.idle(60);
        s.press('W');
        s.idle(3);
        s.render();
        s.idle(150);
        s.cov = Some((row, col));
        assert_eq!(
            s.caret(),
            (row, col + 3),
            "torn {torn}: the box must move down a row, or this proves nothing"
        );
        assert_eq!(
            s.dark(),
            "dark[0]=",
            "torn {torn}: the edit must be lit | {:?}",
            s.holes_seen.first()
        );
        assert!(
            s.holes_seen.is_empty(),
            "torn {torn}: a dark cell inside the typed run: {:?}",
            s.holes_seen.first()
        );
        assert!(
            band_rode(&s, row, col, grew_at),
            "torn {torn}: the hand's band must ride down with its line"
        );
    }
}

/// `all thes`, the `s` echoed in the Space's column, then Backspace and the
/// Space 40 ms apart and `subtrees`. With `merged` the app renders the
/// Backspace and the Space in one repaint (it lags two keys): the caret ends
/// where it began, and the column goes from `s` to a blank.
fn retyped_over_a_typo(merged: bool) -> Sim {
    let mut s = Sim::new("");
    s.verbose = true;
    s.preload("", 0);
    let row = s.caret().0;
    s.cov = Some((row, 2));
    let start = s.ms() + 100;
    let mut keys = keys_of("all thes", start, 100);
    let typo = keys.last().expect("the typo").0;
    let erase = keys.len();
    keys.push((typo + 100, BACKSPACE));
    keys.push((typo + 140, ' '));
    keys.extend(keys_of("subtrees", typo + 250, 100));
    let r = renders_for(&keys, |i| if merged && i == erase { 42 } else { 2 });
    s.run(&keys, &r);
    s.idle(200);
    s
}

/// The derived same-caret licence's verdict for `evidence` after one press
/// (`aterm_models_effects::same_caret_typed_echo_model`).
fn same_caret_model_admits(evidence: &str) -> bool {
    let model = aterm_models_effects::same_caret_typed_echo_model();
    let mut state = model.init_state();
    for action in ["BankA", evidence, "Resolve"] {
        assert!(model.fire(action, &mut state));
    }
    state["admitted"] == 1
}

/// Whether the hand owns a live typed cell at `(row, col)` born after `at` ms.
fn laid_after(s: &Sim, row: u16, col: u16, at: u64) -> bool {
    let born = s.t0 + Duration::from_millis(at);
    s.glow
        .v2_ribbon()
        .expect("rainbow kitty")
        .cells()
        .iter()
        .any(|c| c.row == row && c.col == col && c.typing && !c.leaving() && c.born >= born)
}

/// The owner's `the▯subtrees` (2026-10-16): a Space retyped over a typo in its
/// own column, its echo merged with the Backspace's, is the Space's echo: the
/// Backspace's retreat was never seen and the column now spells the key typed
/// since, so the repaint is the erase's retreat and the Space's advance, and
/// the key's cell is laid there, as the derived licence admits
/// (`RetypedInPlace`). Control: the two echoes rendered apart. The typo's own
/// echo landing after the Backspace is
/// `torn_repaint_hidden_caret.rs`'s
/// `a_key_erased_before_its_echo_keeps_the_next_key_lit`.
#[test]
fn a_space_retyped_over_a_typo_in_one_repaint_lights_its_own_cell() {
    for merged in [false, true] {
        let s = retyped_over_a_typo(merged);
        assert_eq!(
            s.row_text(s.caret().0).trim_end(),
            "\u{276f}\u{a0}all the subtrees",
            "merged {merged}: the take must type the line, or this proves nothing"
        );
        assert_eq!(
            s.log.iter().any(|l| l.contains("RENDER \"\\u{8} \"")),
            merged,
            "merged {merged}: one repaint must carry the Backspace and the Space exactly when merged"
        );
        assert!(
            s.holes_seen.is_empty(),
            "merged {merged}: a dark cell inside the typed run: {} | {:?}",
            s.dark(),
            s.holes_seen.first()
        );
    }
    assert!(same_caret_model_admits("RetypedInPlace"));

    // The same correction crosses a box relocation: its last shown row remains
    // the comparison source after the hand rides onto the new row, including
    // when a torn repaint withholds the caret between those observations.
    for (before_footer, after_footer, torn) in [
        (0usize, 1usize, None),
        (1, 0, None),
        (0, 1, Some(200)),
        (1, 0, Some(200)),
    ] {
        let mut s = Sim::new("");
        s.comp.footer = before_footer;
        s.preload("", 0);
        let keys = keys_of("all thes", s.ms() + 100, 100);
        let renders = renders_for(&keys, |_| 2);
        s.run(&keys, &renders);
        s.idle(50);
        let (row, col) = s.caret();
        s.press(BACKSPACE);
        s.idle(20);
        let space_at = s.ms();
        s.press(' ');
        s.idle(3);
        s.comp.footer = after_footer;
        s.render_torn(torn);
        s.idle(100);
        let target = if after_footer > before_footer {
            row - 1
        } else {
            row + 1
        };
        assert_eq!(
            s.caret(),
            (target, col),
            "the correction moved with its box"
        );
        assert!(
            laid_after(&s, target, col - 1, space_at),
            "footer {before_footer}->{after_footer}, torn {torn:?}: the Space owns its echo on the moved row"
        );
    }
}

/// Negative control: with no Backspace pressed, the app replacing the hand's
/// glyph in place with the pending key's glyph under an unchanged caret is
/// no echo of that key, and the model refuses it too (`ReplacedNoErase`).
#[test]
fn a_glyph_replaced_in_place_without_an_erase_claims_no_key() {
    let mut s = Sim::new("");
    s.preload("", 0);
    let row = s.caret().0;
    let start = s.ms() + 100;
    let keys = keys_of("all thes", start, 100);
    let r = renders_for(&keys, |_| 2);
    s.run(&keys, &r);
    let at = s.ms() + 100;
    s.advance_to_ms(at);
    s.now = s.t0 + Duration::from_millis(at);
    s.press(' ');
    s.pending.clear();
    let col = s.comp.caret - 1;
    s.comp.text[col] = ' ';
    let b = s.comp.render();
    s.term.process(&b);
    s.idle(200);
    let glyph_col = u16::try_from(col + 2).expect("a column");
    assert_eq!(s.row_text(row).trim_end(), "\u{276f}\u{a0}all the");
    assert_eq!(
        laid_after(&s, row, glyph_col, at),
        same_caret_model_admits("ReplacedNoErase"),
        "the real lane disagrees with the derived licence"
    );
    assert!(
        !laid_after(&s, row, glyph_col, at),
        "a replaced glyph claimed the Space"
    );
}

/// Negative control: once the Backspace's own echo was seen apart (the caret
/// stepped back), the erase is past, so the app later replacing the glyph
/// before the unchanged caret with the pending Space is no correction and
/// claims no key, as the derived licence refuses it (`ReplacedNoErase`).
#[test]
fn a_glyph_replaced_after_its_erase_echoed_claims_no_key() {
    let mut s = Sim::new("");
    s.preload("", 0);
    let row = s.caret().0;
    let start = s.ms() + 100;
    let mut keys = keys_of("all thes", start, 100);
    let typo = keys.last().expect("the typo").0;
    keys.push((typo + 100, BACKSPACE));
    let r = renders_for(&keys, |_| 2);
    s.run(&keys, &r);
    let at = s.ms() + 100;
    s.advance_to_ms(at);
    s.now = s.t0 + Duration::from_millis(at);
    s.press(' ');
    s.pending.clear();
    let col = s.comp.caret - 1;
    s.comp.text[col] = ' ';
    let b = s.comp.render();
    s.term.process(&b);
    s.idle(200);
    let glyph_col = u16::try_from(col + 2).expect("a column");
    assert_eq!(s.row_text(row).trim_end(), "\u{276f}\u{a0}all th");
    assert_eq!(
        laid_after(&s, row, glyph_col, at),
        same_caret_model_admits("ReplacedNoErase"),
        "the real lane disagrees with the derived licence"
    );
    assert!(
        !laid_after(&s, row, glyph_col, at),
        "a replacement after the erase's echo claimed the Space"
    );
}

/// The shipping single-pane present as a harness, its live takes, and the
/// open families' reports.
pub(crate) mod shipping_present;

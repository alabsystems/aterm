// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A word typed into Claude Code's composer just before an `[Image #1]` chip keeps its band while the box re-wraps it.

use aterm_core::render::GlowQuad;
use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::ribbon::RETRACT_FADE_S;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::time::{Duration, Instant};

/// The recording: Claude Code v2.1.280 in a 64×20 headless aterm.
const REC: &str = include_str!("fixtures/wrap_code-cc-chip-reflow-2026-09-23.ptylog");
const ROWS: usize = 20;
const COLS: usize = 64;
const CW: usize = 7;
const CH: usize = 14;
const FRAME_MS: u64 = 16;
/// A column is lit at this planned coverage (the glass census's 20/255).
const LIT_COV: u8 = 20;

/// The composer's first text row (0-based) and its continuation row.
const ROW1: usize = 16;
const ROW2: usize = 17;
/// Text columns per row: 60 `x` fill columns 2..=61.
const TEXT_W: usize = 60;
/// `❯ ` on the first row, the continuation indent on the others.
const PREFIX: usize = 2;
/// Claude Code's image placeholder: glued to the letters typed before it.
const CHIP: &str = "[Image #1]";
/// What the recorded takes type before the chip.
const TYPED: &str = "already been runnignin ";
/// Take 3's geometry: `have [Image` fits row 1, `have a[Image` does not, so the
/// first key pushes the glued word down (A).
const ROW1_EXACT: &str = "the aterm window had this prompt appear. you did have";
/// One column more: `a` fits and `al[Image` wraps, so the second key moves the
/// first letter down (B1, B2); the Space after `already` lets it fit row 1
/// again (C).
const ROW1_ONE_FITS: &str = "the aterm window had this prompt appear. you do have";
/// Where the lifted `already` stands on row 1 in that geometry.
const LIFTED: std::ops::RangeInclusive<usize> = 55..=61;

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

/// The host's `typed_class_for` and `glyph_shifted && !spacebar`.
fn class_of(ch: char) -> (bool, TypedClass) {
    match ch {
        ' ' => (false, TypedClass::Space),
        '!' => (true, TypedClass::Bang),
        c if c.is_uppercase() => (true, TypedClass::Capital),
        _ => (false, TypedClass::Glyph),
    }
}

/// Plan coverage per column of `row` (`tests/torn_read_gaps.rs`).
fn coverage(glow: &CursorGlow, row: u16) -> Vec<u8> {
    let mut cov = vec![0u8; COLS];
    let rib = glow.v2_ribbon().expect("rainbow kitty owns the frame");
    for sg in rib.plan_segments() {
        let r = ((sg.spine / CH as f32) - 0.5).floor();
        let c = (sg.x / CW as f32).floor();
        if r >= 0.0 && c >= 0.0 && r as usize == usize::from(row) && (c as usize) < COLS {
            let i = c as usize;
            cov[i] = cov[i].max(sg.cov);
        }
    }
    cov
}

fn map(lit: &[bool]) -> String {
    lit.iter().map(|&l| if l { '#' } else { '.' }).collect()
}

/// One frame's census of the composer's two rows.
#[derive(Clone, Debug)]
struct Shot {
    ms: u64,
    label: String,
    caret: (u16, u16),
    text: [String; 2],
    lit: [Vec<bool>; 2],
    /// Columns owned by a live ribbon cell (not leaving), per row.
    live: [Vec<usize>; 2],
    /// Hand-typed glyph cells per row as `(col, typing order)`.
    typed: [Vec<(usize, u64)>; 2],
}

impl Shot {
    /// Live cells on the continuation row at or right of the caret, when the
    /// caret is on it: light over text this insert has not written yet.
    fn live_ahead_on_row2(&self) -> Vec<usize> {
        if usize::from(self.caret.0) != ROW2 {
            return Vec::new();
        }
        self.live[1]
            .iter()
            .copied()
            .filter(|&c| c >= usize::from(self.caret.1))
            .collect()
    }
}

/// A real terminal and the glow engine, sampled in the host's order (`app_render.rs`: the
/// caret row's probe and its two neighbours, the print anchor, every row the ribbon wants), a
/// frame after every PTY read and on a 16 ms train.
struct Host {
    term: Terminal,
    glow: CursorGlow,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    above: Vec<char>,
    below: Vec<char>,
    blink: u64,
    t0: Instant,
    now: Instant,
}

impl Host {
    fn new() -> Self {
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        Self {
            term: Terminal::new(ROWS as u16, COLS as u16),
            glow,
            out: Vec::new(),
            row_buf: Vec::new(),
            above: Vec::new(),
            below: Vec::new(),
            blink: 0,
            t0: now,
            now,
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn frame(&mut self) {
        let c = self.term.cursor();
        let cur = self.term.cursor_visible().then_some((c.row, c.col));
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink {
            self.blink = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        let r = usize::from(c.row);
        self.term.row_cols_into(r, &mut self.row_buf);
        self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
        if r > 0 {
            self.term.row_cols_into(r - 1, &mut self.above);
        }
        if r + 1 < ROWS {
            self.term.row_cols_into(r + 1, &mut self.below);
        }
        self.glow.observe_neighbor_rows(
            (r > 0).then_some(self.above.as_slice()),
            (r + 1 < ROWS).then_some(self.below.as_slice()),
        );
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        self.glow
            .observe_print_anchor(aterm_effects::host::motion_print_anchor(&self.term).0);
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &row in &rows[..n] {
            self.term.row_cols_into(usize::from(row), &mut self.row_buf);
            self.glow.observe_ribbon_row(row, &self.row_buf);
        }
        let cfg = cfg();
        self.glow.tick(cur, self.now, &cfg, geom(), &mut self.out);
    }

    /// The census of this frame: the caret, the two composer rows' text and
    /// lit columns.
    fn shot(&mut self, label: &str) -> Shot {
        let mut text = [String::new(), String::new()];
        let mut lit = [Vec::new(), Vec::new()];
        for (i, r) in [ROW1 as u16, ROW2 as u16].into_iter().enumerate() {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            text[i] = self
                .row_buf
                .iter()
                .map(|&c| if c == '\0' { ' ' } else { c })
                .collect();
            lit[i] = coverage(&self.glow, r)
                .iter()
                .map(|&v| v >= LIT_COV)
                .collect();
        }
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let live = [ROW1, ROW2].map(|r| {
            let mut cols: Vec<usize> = rib
                .cells()
                .iter()
                .filter(|c| usize::from(c.row) == r && !c.leaving())
                .map(|c| usize::from(c.col))
                .collect();
            cols.sort_unstable();
            cols.dedup();
            cols
        });
        let c = self.term.cursor();
        Shot {
            ms: self.ms(),
            label: label.to_string(),
            caret: (c.row, c.col),
            text,
            lit,
            live,
            typed: [Vec::new(), Vec::new()],
        }
    }

    /// Whether the admission ring holds a verdict for `origin -> target`.
    fn judged(&self, origin: (u16, u16), target: (u16, u16)) -> bool {
        self.glow
            .admission_log()
            .any(|a| a.origin == origin && a.target == target)
    }
}

enum Rec {
    Out(Vec<u8>),
    In(Vec<u8>),
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

/// `(ms from the first line, event)`; the stamps are microseconds.
fn recording(src: &str) -> Vec<(u64, Rec)> {
    let mut t0 = None;
    src.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(3, ' ');
            let us: u64 = it.next().expect("stamp").parse().expect("stamp");
            let kind = it.next().expect("kind");
            let bytes = unhex(it.next().expect("bytes"));
            let t0 = *t0.get_or_insert(us);
            let ms = (us - t0) / 1_000;
            match kind {
                "O" => (ms, Rec::Out(bytes)),
                "I" => (ms, Rec::In(bytes)),
                k => panic!("bad kind {k}"),
            }
        })
        .collect()
}

/// The typed keys of the take whose program text contains `marker`, as
/// `(key, ms of its echo)` — the echo is the next synchronized chunk.
fn take_keys(rec: &[(u64, Rec)], marker: &str) -> Vec<(char, u64)> {
    let start = rec
        .iter()
        .position(|(_, e)| {
            matches!(e, Rec::In(b) if b.windows(marker.len()).any(|w| w == marker.as_bytes()))
        })
        .expect("the take's setup text");
    let mut keys: Vec<(char, u64)> = Vec::new();
    for (i, (ms, e)) in rec.iter().enumerate().skip(start + 1) {
        let Rec::In(b) = e else { continue };
        if b.len() > 3 {
            // the next take's setup
            break;
        }
        let [c] = b.as_slice() else { continue };
        if !(0x20..0x7f).contains(c) {
            continue;
        }
        if keys
            .last()
            .is_some_and(|&(_, last)| ms.saturating_sub(last) > 5_000)
        {
            // a later probe, not this take's typing
            break;
        }
        let echo = rec[i + 1..]
            .iter()
            .find_map(|(m, e)| {
                matches!(e, Rec::Out(o) if o.starts_with(b"\x1b[?2026h")).then_some(*m)
            })
            .expect("an echo");
        keys.push((*c as char, echo));
    }
    keys
}

/// Replay the recording read by read with the hints the app stamps
/// (`note_typed_expected` for a printable key, `note_motion` for an arrow or
/// End, nothing for program text), censusing the composer rows on every frame
/// inside `window` (ms).
fn replay(window: (u64, u64)) -> Vec<Shot> {
    let mut h = Host::new();
    let mut shots = Vec::new();
    let shoot = |h: &mut Host, shots: &mut Vec<Shot>| {
        if (window.0..=window.1).contains(&h.ms()) {
            shots.push(h.shot(""));
        }
    };
    for (ms, ev) in recording(REC) {
        if ms > window.1 {
            break;
        }
        let t = h.t0 + Duration::from_millis(ms);
        while h.now + Duration::from_millis(FRAME_MS) <= t {
            h.now += Duration::from_millis(FRAME_MS);
            h.frame();
            shoot(&mut h, &mut shots);
        }
        h.now = h.now.max(t);
        match ev {
            Rec::In(b) => match b.as_slice() {
                [c] if (0x20..0x7f).contains(c) => {
                    let ch = *c as char;
                    let (shifted, class) = class_of(ch);
                    h.glow.note_typed_expected(h.now, 1, shifted, class, ch);
                }
                [0x7f] => h.glow.note_backspace(h.now),
                b"\x1b[D" | b"\x1b[C" | b"\x1b[F" | b"\x1b[H" => h.glow.note_motion(h.now),
                _ => {}
            },
            Rec::Out(bytes) => {
                h.term.process(&bytes);
                h.frame();
                shoot(&mut h, &mut shots);
            }
        }
    }
    shots
}

/// The shot at or just after `ms`.
fn at(shots: &[Shot], ms: u64) -> &Shot {
    shots
        .iter()
        .find(|s| s.ms >= ms)
        .unwrap_or_else(|| panic!("a frame at {ms} ms"))
}

/// How the composer breaks a word wider than its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wrap {
    /// Never: the word overflows (as in every recorded take).
    Word,
    /// wrap-ansi's `hard: true`, which Ink passes: a word wider than a row is
    /// broken at the row's end when moving it down would not save a row, so a
    /// token begun at the row's first text column stays and overflows.
    Hard,
}

/// Where each char of `text` sits `(text row, grid col)`, plus the caret's cell
/// at the end.
fn layout(text: &[char], wrap: Wrap) -> Vec<(usize, usize)> {
    let mut pos = Vec::with_capacity(text.len() + 1);
    let (mut row, mut len) = (0usize, 0usize);
    let hard = wrap == Wrap::Hard;
    let mut i = 0;
    while i < text.len() {
        if text[i] == ' ' {
            pos.push((row, PREFIX + len));
            len += 1;
            i += 1;
            continue;
        }
        let end = (i..text.len())
            .find(|&j| text[j] == ' ')
            .unwrap_or(text.len());
        let w = end - i;
        let down = if hard && w > TEXT_W {
            let here = 1 + (w - (TEXT_W - len) - 1) / TEXT_W;
            let there = (w - 1) / TEXT_W;
            there < here
        } else {
            len + w > TEXT_W
        };
        if down && len > 0 {
            row += 1;
            len = 0;
        }
        for _ in i..end {
            if hard && len == TEXT_W {
                row += 1;
                len = 0;
            }
            pos.push((row, PREFIX + len));
            len += 1;
        }
        i = end;
    }
    pos.push((row, PREFIX + len));
    pos
}

fn render(text: &[char], wrap: Wrap) -> [Vec<char>; 2] {
    let pos = layout(text, wrap);
    let mut rows = [vec![' '; COLS], vec![' '; COLS]];
    rows[0][0] = '\u{276f}';
    rows[0][1] = '\u{a0}';
    for (k, &ch) in text.iter().enumerate() {
        let (r, c) = pos[k];
        if r < 2 && c < COLS {
            rows[r][c] = ch;
        }
    }
    rows
}

/// Claude Code shows the caret on the cell of the char it stands before.
fn caret_cell(text: &[char], caret: usize, wrap: Wrap) -> (u16, u16) {
    let (r, c) = layout(text, wrap)[caret];
    ((ROW1 + r) as u16, c as u16)
}

/// Claude Code's chunk for `old -> new`: per changed row the changed cell runs,
/// the first addressed from home (`ESC[H CR ESC[nC ESC[mB`), the rest by
/// `ESC[nG`, `EL` where a row got shorter; then the status park and the caret,
/// inside the DEC 2026 bracket with the caret hidden.
fn frame_bytes(old: &[Vec<char>; 2], new: &[Vec<char>; 2], caret: (u16, u16)) -> Vec<u8> {
    let mut s = String::from("\x1b[?2026h\x1b[?25l");
    for r in 0..2 {
        let (o, n) = (&old[r], &new[r]);
        let n_len = n.iter().rposition(|&c| c != ' ').map_or(0, |i| i + 1);
        let o_len = o.iter().rposition(|&c| c != ' ').map_or(0, |i| i + 1);
        let mut first = true;
        let goto = |s: &mut String, col: usize, first: &mut bool| {
            if *first {
                s.push_str("\x1b[H\r");
                if col > 0 {
                    s.push_str(&format!("\x1b[{col}C"));
                }
                s.push_str(&format!("\x1b[{}B", ROW1 + r));
                *first = false;
            } else {
                s.push_str(&format!("\x1b[{}G", col + 1));
            }
        };
        let mut c = 0;
        while c < n_len {
            if o[c] == n[c] {
                c += 1;
                continue;
            }
            let start = c;
            while c < n_len && o[c] != n[c] {
                c += 1;
            }
            goto(&mut s, start, &mut first);
            s.extend(n[start..c].iter());
        }
        if o_len > n_len {
            goto(&mut s, n_len, &mut first);
            s.push_str("\x1b[K");
        }
    }
    s.push_str(&format!(
        "\x1b[{ROWS};1H\x1b[{};{}H\x1b[?25h\x1b[?2026l",
        caret.0 + 1,
        caret.1 + 1
    ));
    s.into_bytes()
}

/// A take's faults, frame by frame.
#[derive(Debug, Default)]
struct Faults {
    /// `(ms, row, col)`: a glyph the hand typed 20..=800 ms ago, dark.
    dark: Vec<(u64, usize, usize)>,
    /// `(ms, col)`: light on the continuation row's blank indent.
    indent: Vec<(u64, usize)>,
    /// `(ms, row, col)`: light on a row's blank tail one cell past its text (a
    /// band head at a caret parked after the text is allowed).
    stranded: Vec<(u64, usize, usize)>,
}

impl Faults {
    /// Grouped by cell: `(row, col) -> frames`.
    fn summary(&self) -> String {
        use std::collections::BTreeMap;
        let mut dark: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        for &(_, r, c) in &self.dark {
            *dark.entry((r, c)).or_default() += 1;
        }
        let mut strand: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        for &(_, r, c) in &self.stranded {
            *strand.entry((r, c)).or_default() += 1;
        }
        let mut ind: BTreeMap<usize, usize> = BTreeMap::new();
        for &(_, c) in &self.indent {
            *ind.entry(c).or_default() += 1;
        }
        format!("dark {dark:?} | indent {ind:?} | stranded {strand:?}")
    }
}

/// The synthetic take: the box on a real terminal, the composer model
/// driving it, a shot per frame.
struct Cc {
    h: Host,
    text: Vec<char>,
    /// Per char: the order the hand typed it in, `None` for program text.
    typed: Vec<Option<u64>>,
    keys: u64,
    /// The ms each typed key's echo was painted, by typing order.
    echoed: Vec<u64>,
    /// The typing order of the first key under census.
    burst: u64,
    caret: usize,
    shown: [Vec<char>; 2],
    label: String,
    shots: Vec<Shot>,
    /// PTY latency of every echo.
    echo_ms: u64,
    /// How the box breaks a word wider than its row.
    wrap: Wrap,
}

impl Cc {
    /// The box painted with `text` as program output (a paste, a recall:
    /// nothing typed), the caret at `caret`.
    fn new(text: &str, caret: usize) -> Self {
        Self::with_wrap(text, caret, Wrap::Word)
    }

    /// [`Cc::new`] with the box breaking over-wide words as `wrap` says.
    fn with_wrap(text: &str, caret: usize, wrap: Wrap) -> Self {
        let mut h = Host::new();
        h.term.process(b"\x1b[?1049h\x1b[2J\x1b[H");
        let rule = "\u{2500}".repeat(COLS);
        h.term.process(
            format!(
                "\x1b[{ROW1};1H{rule}\x1b[{};1H{rule}\x1b[{ROWS};3H\u{23f5}\u{23f5} auto mode on",
                ROW2 + 2
            )
            .as_bytes(),
        );
        let text: Vec<char> = text.chars().collect();
        let blank = [vec![' '; COLS], vec![' '; COLS]];
        let shown = render(&text, wrap);
        h.term
            .process(&frame_bytes(&blank, &shown, caret_cell(&text, caret, wrap)));
        let typed = vec![None; text.len()];
        let mut me = Self {
            h,
            text,
            typed,
            keys: 0,
            echoed: Vec::new(),
            burst: 0,
            caret,
            shown,
            label: "start".into(),
            shots: Vec::new(),
            echo_ms: 2,
            wrap,
        };
        me.frame();
        me.idle(400);
        me
    }

    fn frame(&mut self) {
        self.h.frame();
        let mut shot = self.h.shot(&self.label);
        let pos = layout(&self.text, self.wrap);
        for (k, &ch) in self.text.iter().enumerate() {
            if let (Some(order), false) = (self.typed[k], ch == ' ') {
                let (r, c) = pos[k];
                if r < 2 {
                    shot.typed[r].push((c, order));
                }
            }
        }
        self.shots.push(shot);
    }

    fn idle(&mut self, ms: u64) {
        let t = self.h.now + Duration::from_millis(ms);
        while self.h.now + Duration::from_millis(FRAME_MS) <= t {
            self.h.now += Duration::from_millis(FRAME_MS);
            self.frame();
        }
        self.h.now = self.h.now.max(t);
    }

    /// The box repainted to the model's state: one chunk, one frame.
    fn repaint(&mut self) {
        let new = render(&self.text, self.wrap);
        let bytes = frame_bytes(
            &self.shown,
            &new,
            caret_cell(&self.text, self.caret, self.wrap),
        );
        self.shown = new;
        self.h.term.process(&bytes);
        self.frame();
    }

    /// The hint the app stamps for a typed key (`app_input.rs`:
    /// `note_typed_expected`), `gap_ms` after the last event.
    fn press(&mut self, ch: char, gap_ms: u64) {
        self.idle(gap_ms);
        self.label = format!("key {ch:?}");
        let (shifted, class) = class_of(ch);
        self.h
            .glow
            .note_typed_expected(self.h.now, 1, shifted, class, ch);
    }

    /// The pressed key's echo `echo_ms` later: the model takes the glyph at
    /// the caret and the box is repainted.
    fn echo(&mut self, ch: char) {
        self.idle(self.echo_ms);
        self.text.insert(self.caret, ch);
        self.typed.insert(self.caret, Some(self.keys));
        self.keys += 1;
        self.caret += 1;
        self.echoed.push(self.h.ms());
        self.repaint();
    }

    /// One typed key `gap_ms` after the last event, and its echo.
    fn key(&mut self, ch: char, gap_ms: u64) {
        self.press(ch, gap_ms);
        self.echo(ch);
    }

    fn type_str(&mut self, s: &str, gap_ms: u64) {
        for ch in s.chars() {
            self.key(ch, gap_ms);
        }
    }

    /// Program text at the caret (a paste the host licensed nothing for).
    fn insert(&mut self, s: &str, gap_ms: u64) {
        self.idle(gap_ms);
        self.label = "insert".into();
        for ch in s.chars() {
            self.text.insert(self.caret, ch);
            self.typed.insert(self.caret, None);
            self.caret += 1;
        }
        self.repaint();
    }

    /// Program output outside the box model (a footer, an erase), one
    /// synchronized chunk, one frame.
    fn program(&mut self, bytes: &str) {
        self.label = "program".into();
        self.h
            .term
            .process(format!("\x1b[?2026h\x1b[?25l{bytes}\x1b[?25h\x1b[?2026l").as_bytes());
        self.frame();
    }

    /// A Left arrow: the nav hint, the caret's echo. The chip is one step.
    fn left(&mut self) {
        self.idle(120);
        self.label = "left".into();
        self.h.glow.note_motion(self.h.now);
        self.idle(self.echo_ms);
        let chip: Vec<char> = CHIP.chars().collect();
        let n = chip.len();
        self.caret -= if self.caret >= n && self.text[self.caret - n..self.caret] == chip[..] {
            n
        } else {
            1
        };
        self.repaint();
    }

    /// A Right arrow: the nav hint, the caret's echo.
    fn right(&mut self) {
        self.idle(120);
        self.label = "right".into();
        self.h.glow.note_motion(self.h.now);
        self.idle(self.echo_ms);
        self.caret += 1;
        self.repaint();
    }

    /// Walk the caret back with Left to just before the chip, pause, and
    /// type `typed` at `gap_ms`; returns the first shot of the typing.
    fn insert_before_chip(&mut self, typed: &str, gap_ms: u64) -> usize {
        let chip: Vec<char> = CHIP.chars().collect();
        while !self.text[self.caret..].starts_with(&chip) {
            self.left();
        }
        self.idle(1500);
        let from = self.shots.len();
        self.burst = self.keys;
        self.type_str(typed, gap_ms);
        self.idle(600);
        from
    }

    /// The first shot after the echo of the `k`-th key of the burst.
    fn after_key(&self, k: usize) -> &Shot {
        let echo = self.echoed[self.burst as usize + k];
        self.shots
            .iter()
            .find(|s| s.ms > echo)
            .expect("a frame after the key")
    }

    /// The two rows frame by frame, for the shots inside `ms` (a failure's
    /// picture).
    fn dump(&self, ms: (u64, u64)) -> String {
        let mut out = String::new();
        for s in self.shots.iter().filter(|s| (ms.0..=ms.1).contains(&s.ms)) {
            for r in 0..2 {
                out.push_str(&format!(
                    "{:>6} {:<9} r{} |{}|\n{:>16} r{} |{}|\n",
                    s.ms,
                    s.label,
                    ROW1 + r,
                    s.text[r].trim_end(),
                    "",
                    ROW1 + r,
                    map(&s.lit[r])
                ));
            }
        }
        out
    }

    /// What the take did wrong from shot `from` on.
    fn faults(&self, from: usize) -> Faults {
        let mut f = Faults::default();
        for s in &self.shots[from..] {
            for r in 0..2 {
                for &(col, order) in s.typed[r].iter().filter(|&&(_, o)| o >= self.burst) {
                    let age = s.ms.saturating_sub(self.echoed[order as usize]);
                    if !s.lit[r][col] && (20..=800).contains(&age) {
                        f.dark.push((s.ms, ROW1 + r, col));
                    }
                }
                let end = s.text[r].trim_end().chars().count();
                for c in end + 1..COLS {
                    if s.lit[r][c] {
                        f.stranded.push((s.ms, ROW1 + r, c));
                    }
                }
            }
            for c in 0..PREFIX {
                if s.lit[1][c] {
                    f.indent.push((s.ms, c));
                }
            }
        }
        f
    }
}

/// Row 1 painted as program text, the chip after it, the caret at the end.
fn painted(row1: &str) -> Cc {
    let full = format!("{row1} {CHIP} in the");
    Cc::new(&full, full.chars().count())
}

/// Row 1 typed by the hand up to its last word, with the Space after it and
/// the chip arriving as program text (a paste): the last typed Space is the
/// one before `have`.
fn typed_but_the_last_space(row1: &str) -> Cc {
    let mut cc = Cc::new("", 0);
    cc.type_str(row1, 110);
    cc.insert(&format!(" {CHIP} in the"), 300);
    cc
}

/// A key pressed mid-line on row 1 (`x` before `prompt`) answered first by a
/// program chunk that parks the caret one row down after writing `glyph` (a
/// footer), erasing row 1 from the caret when `erase_tail`; the key's echo
/// follows 60 ms later.
fn footer_before_the_echo(glyph: char, erase_tail: bool) -> (Cc, usize) {
    let text = "the aterm window had this prompt appear";
    let at = text.find("prompt").expect("the word");
    let mut cc = Cc::new(text, at);
    cc.press('x', 300);
    cc.idle(2);
    let erase = if erase_tail {
        format!("\x1b[{};{}H\x1b[K", ROW1 + 1, PREFIX + at + 1)
    } else {
        String::new()
    };
    cc.program(&format!("{erase}\x1b[{};30H{glyph}", ROW2 + 1));
    cc.idle(60);
    cc.shown = [vec![' '; COLS], vec![' '; COLS]];
    cc.echo_ms = 0;
    cc.echo('x');
    cc.idle(400);
    (cc, PREFIX + at)
}

/// `have [Image` fills row 1 and `a` sends `a[Image` to row 2: the fold `(16,56) → (17,3)`
/// reaches the admission ring and every glyph typed on row 2 is lit from 20 ms after its echo.
#[test]
fn the_first_key_that_pushes_a_glued_word_down_is_judged_not_parked() {
    let mut cc = painted(ROW1_EXACT);
    let from = cc.insert_before_chip(&TYPED[..1], 110);
    assert!(
        cc.shots[from..]
            .iter()
            .any(|s| s.text[1].starts_with("  a[Image #1] in the")),
        "the take reached the measured fold"
    );
    let judged = cc.h.judged((16, 56), (17, 3));
    let verdicts: Vec<_> =
        cc.h.glow
            .admission_log()
            .map(|a| (a.origin, a.target, a.reason))
            .collect();
    cc.type_str(&TYPED[1..], 110);
    cc.idle(600);
    let f = cc.faults(from);
    assert!(
        judged,
        "the fold (16,56) -> (17,3) never reached a verdict: {verdicts:?}\n{}",
        f.summary()
    );
    assert!(
        f.dark.is_empty() && f.indent.is_empty(),
        "{} dark typed-glyph frames — {}",
        f.dark.len(),
        f.summary()
    );
}

/// The same fold with the first echo 300 ms late: the press's glyph witness lasts
/// `IN_FLIGHT_PATIENCE_S`, so the fold is still judged, not held as a cross-row park whose
/// custody would swallow later keys.
#[test]
fn a_delayed_first_key_still_carries_the_glued_word_down() {
    let mut cc = painted(ROW1_EXACT);
    cc.echo_ms = 300;
    let from = cc.insert_before_chip(&TYPED[..1], 110);
    assert!(
        cc.h.judged((16, 56), (17, 3)),
        "the 300 ms fold was held rather than judged: {:?}",
        cc.h.glow.admission_log().collect::<Vec<_>>()
    );
    let faults = cc.faults(from);
    assert!(
        faults.dark.is_empty(),
        "{}; verdicts {:?}",
        faults.summary(),
        cc.h.glow.admission_log().collect::<Vec<_>>()
    );
}

/// A key's footer keeps the park's custody: a program chunk that parks the
/// caret one row down after a glyph, then the key's echo on row 1.
#[test]
fn a_footer_before_the_echo_keeps_the_parks_custody() {
    for (what, glyph, erase_tail) in [
        ("the key's own glyph", 'x', false),
        ("an erased tail under a foreign glyph", '.', true),
        ("an erased tail under the key's own glyph", 'x', true),
    ] {
        let (cc, col) = footer_before_the_echo(glyph, erase_tail);
        let footer = cc.shots.iter().filter(|s| s.lit[1].contains(&true)).count();
        let key = cc.echoed[0];
        assert_eq!(
            footer,
            0,
            "{what}: the footer spent the key\n{}",
            cc.dump((key.saturating_sub(80), key + 32))
        );
        let last = cc.shots.last().expect("a frame");
        assert!(
            last.text[0].contains("had this xprompt") && last.lit[0][col],
            "{what}: the key's own echo went dark: r16 |{}|",
            map(&last.lit[0])
        );
    }
}

/// B1: the reflowed word keeps its first glyph and the row it left keeps no
/// scrap.
#[test]
fn a_word_begun_at_a_navigated_caret_keeps_its_first_glyph_when_it_wraps_down() {
    let mut cc = painted(ROW1_ONE_FITS);
    let from = cc.insert_before_chip(TYPED, 110);
    let wrap = cc.echoed[cc.burst as usize + 1];
    let s = cc.after_key(1);
    assert!(
        s.text[1].starts_with("  al[Image #1] in the") && s.text[0].trim_end().ends_with("do have"),
        "the take reached the measured wrap: {:?}",
        s.text
    );
    let f = cc.faults(from);
    let first = f
        .dark
        .iter()
        .filter(|&&(_, r, c)| (r, c) == (ROW2, PREFIX))
        .count();
    assert_eq!(
        first,
        0,
        "the reflowed word's first glyph (17, 2) was dark in {first} frames — {}\n{}",
        f.summary(),
        cc.dump((wrap.saturating_sub(40), wrap + 80))
    );
    let late = wrap + (RETRACT_FADE_S * 1000.0) as u64 + 2 * FRAME_MS;
    let scraps: Vec<_> = f
        .stranded
        .iter()
        .filter(|&&(ms, r, _)| r == ROW1 && ms >= late)
        .collect();
    assert!(
        scraps.is_empty(),
        "light stayed on row 1's blank tail in {} frames after the word left it: {:?}",
        scraps.len(),
        &scraps[..scraps.len().min(6)]
    );
}

/// B2: nothing is laid on the continuation row's blank indent. The Space after
/// the last word came with the pasted chip, so the last typed Space is the one
/// before `have`.
#[test]
fn a_stale_typed_space_lays_nothing_on_the_continuation_indent() {
    let mut cc = typed_but_the_last_space(ROW1_ONE_FITS);
    let from = cc.insert_before_chip(TYPED, 110);
    let f = cc.faults(from);
    assert!(
        f.indent.is_empty(),
        "the continuation row's indent was lit in {} frames (first at {} ms) — {}",
        f.indent.len(),
        f.indent.first().map_or(0, |i| i.0),
        f.summary()
    );
    let first = f
        .dark
        .iter()
        .filter(|&&(_, r, c)| (r, c) == (ROW2, PREFIX))
        .count();
    assert_eq!(first, 0, "{}", f.summary());
}

/// Program text the hand types after: row 1's columns 2..=41.
const PROG: &str = "the aterm window had this prompt appear.";

/// Frames of a census, as `(ms after the event, columns)`.
type FrameCols = Vec<(u64, Vec<usize>)>;

/// A re-wrap's census from the wrap key's echo at `wrap` ms. `dark`: frames
/// 30..=400 ms after it where a column of the moved word (`word`) on row 2 is
/// dark.
fn rewrap_faults(
    cc: &Cc,
    wrap: u64,
    word: std::ops::RangeInclusive<usize>,
) -> (FrameCols, FrameCols) {
    let late = wrap + 2 * (RETRACT_FADE_S * 1000.0) as u64 + 3 * FRAME_MS;
    let mut dark = Vec::new();
    let mut stranded = Vec::new();
    for s in cc.shots.iter().filter(|s| s.ms > wrap) {
        if (wrap + 30..=wrap + 400).contains(&s.ms) {
            let cols: Vec<usize> = word.clone().filter(|&c| !s.lit[1][c]).collect();
            if !cols.is_empty() {
                dark.push((s.ms - wrap, cols));
            }
        }
        if (late..=wrap + 1_500).contains(&s.ms) {
            let end = s.text[0].trim_end().chars().count();
            let cols: Vec<usize> = (end + 1..COLS).filter(|&c| s.lit[0][c]).collect();
            if !cols.is_empty() {
                stranded.push((s.ms - wrap, cols));
            }
        }
    }
    (dark, stranded)
}

/// Type `word` after [`PROG`], let `edit` walk and finish it, then press `key`:
/// the word no longer fits row 1 and moves whole to row 2, caret after `key`.
fn word_then_wrap(word: &str, edit: impl FnOnce(&mut Cc), key: char) -> (Cc, u64) {
    let mut cc = Cc::new(PROG, PROG.chars().count());
    cc.burst = cc.keys;
    cc.type_str(word, 110);
    edit(&mut cc);
    let k = cc.echoed.len();
    cc.key(key, 110);
    let wrap = cc.echoed[k];
    cc.idle(1_600);
    let s = cc
        .shots
        .iter()
        .find(|s| s.ms > wrap)
        .expect("a frame after the wrap");
    assert!(
        s.text[1].starts_with("  crates/aterm-effects"),
        "the take reached the wrap: {:?}",
        s.text
    );
    (cc, wrap)
}

/// Assert [`rewrap_faults`] found nothing.
fn assert_rewrap_clean(cc: &Cc, wrap: u64, word: std::ops::RangeInclusive<usize>, what: &str) {
    let (dark, stranded) = rewrap_faults(cc, wrap, word);
    assert!(
        dark.is_empty() && stranded.is_empty(),
        "{what}: the moved word dark in {} frames {:?}; row 1 lit past its text in {} frames \
         {:?}\n{}",
        dark.len(),
        &dark[..dark.len().min(3)],
        stranded.len(),
        &stranded[..stranded.len().min(3)],
        cc.dump((wrap.saturating_sub(20), wrap + 120))
    );
}

/// B3: a typo fixed inside the word being typed does not cut its measure.
#[test]
fn a_typo_fixed_inside_the_word_keeps_the_word_lit_when_it_wraps_down() {
    let (cc, wrap) = word_then_wrap(
        " crates/aterm-efects",
        |cc| {
            for _ in 0..4 {
                cc.left();
            }
            cc.idle(80);
        },
        'f',
    );
    assert_rewrap_clean(&cc, wrap, 2..=17, "typo");
}

/// B3: a scrub over the word being typed does not cut its measure.
#[test]
fn a_scrub_over_the_word_keeps_the_word_lit_when_it_wraps_down() {
    let (cc, wrap) = word_then_wrap(
        " crates/aterm-eff",
        |cc| {
            cc.left();
            cc.left();
            cc.right();
            cc.right();
            cc.type_str("ect", 150);
        },
        's',
    );
    assert_rewrap_clean(&cc, wrap, 2..=21, "scrub");
}

/// B4: the hand walks Left to the start of row 1 (an arrival at (16, 2)) and types one unbroken
/// token glued to `hello`.
#[test]
fn a_token_the_box_hard_breaks_keeps_its_band_on_the_row_it_fills() {
    let mut cc = Cc::with_wrap("hello", 5, Wrap::Hard);
    for _ in 0..5 {
        cc.left();
    }
    cc.idle(1_500);
    let from = cc.shots.len();
    cc.burst = cc.keys;
    let token: String = (b'a'..=b'z').cycle().take(59).map(char::from).collect();
    cc.type_str(&token, 110);
    let k = cc.echoed.len();
    cc.key('q', 110);
    let wrap = cc.echoed[k];
    cc.idle(1_600);
    let s = cc
        .shots
        .iter()
        .find(|s| s.ms > wrap)
        .expect("a frame after the wrap");
    assert!(
        s.caret == (ROW2 as u16, PREFIX as u16)
            && s.text[0].trim_end().ends_with("gq")
            && s.text[1].starts_with("  hello"),
        "the take reached the hard break: caret {:?} {:?}",
        s.caret,
        s.text
    );
    let f = cc.faults(from);
    assert!(
        f.dark.is_empty() && f.indent.is_empty() && f.stranded.is_empty(),
        "{}\n{}",
        f.summary(),
        cc.dump((wrap.saturating_sub(20), wrap + 64))
    );
}

/// C: a Space that lets the word fit the row above again carries its band up.
#[test]
fn a_space_that_lets_the_word_fit_the_row_above_carries_its_band_up() {
    let mut cc = painted(ROW1_ONE_FITS);
    let from = cc.insert_before_chip(TYPED, 110);
    let space = TYPED.find(' ').expect("the Space after `already`");
    let lift = cc.echoed[cc.burst as usize + space];
    let s = cc.after_key(space);
    assert!(
        s.text[0].trim_end().ends_with("do have already")
            && s.text[1].starts_with("  [Image #1] in the"),
        "the take reached the measured lift: {:?}",
        s.text
    );
    let s = at(&cc.shots, lift + 2 * FRAME_MS);
    let dark: Vec<usize> = LIFTED.filter(|&c| !s.lit[0][c]).collect();
    assert!(
        dark.is_empty(),
        "the lifted word is dark at {dark:?} {} ms after the Space\n{}",
        s.ms - lift,
        cc.dump((lift.saturating_sub(20), lift + 2 * FRAME_MS))
    );
    let ahead: Vec<(u64, Vec<usize>)> = cc
        .shots
        .iter()
        .filter(|s| s.ms >= lift && s.ms <= lift + 400)
        .map(|s| (s.ms, s.live_ahead_on_row2()))
        .filter(|(_, cols)| !cols.is_empty())
        .collect();
    assert!(
        ahead.is_empty(),
        "a live cell stayed on row 2 at or right of the caret, over the chip the word \
         left: {:?}",
        &ahead[..ahead.len().min(4)]
    );
    let f = cc.faults(from);
    assert!(f.indent.is_empty(), "{}", f.summary());
}

/// Program text to row 1's column 59; ` I` typed after it puts the `I` on
/// row 1's last text column.
const PROG_58: &str = "the aterm window had this prompt appear. you do have a lot";

/// The soft-wrapped caret in the synthetic box: ` I` typed after [`PROG_58`],
/// the `I` drawn at (16, 61) and the caret alone wrapped to (17, 2).
fn soft_wrap_then(keys: &str, echo: &str) -> (Cc, u64) {
    let mut cc = Cc::new(PROG_58, PROG_58.chars().count());
    cc.key(' ', 110);
    cc.press('I', 110);
    cc.idle(2);
    cc.program(&format!(
        "\x1b[{};{}HI\x1b[{};{}H",
        ROW1 + 1,
        PREFIX + TEXT_W,
        ROW2 + 1,
        PREFIX + 1
    ));
    let s = cc.shots.last().expect("a frame");
    assert!(
        s.caret == (ROW2 as u16, PREFIX as u16) && s.text[0].trim_end().ends_with("a lot I"),
        "the take reached the soft-wrapped caret: {:?} {:?}",
        s.caret,
        s.text
    );
    let mut gap = 110;
    for ch in keys.chars() {
        cc.press(ch, gap);
        gap = 4;
    }
    cc.idle(2);
    cc.program(echo);
    let ms = cc.h.ms();
    cc.idle(300);
    (cc, ms)
}

/// D: the key that reflows the soft-wrapped word carries its light down. `b`
/// after the soft-wrapped `I` makes `Ib` one word: the `I` is erased from row
/// 1's end and `Ib` written from the indent, caret (17, 4).
#[test]
fn a_soft_wrapped_word_the_next_key_reflows_comes_down_with_its_light() {
    let echo = format!(
        "\x1b[{};{}H\x1b[K\x1b[{};{}HIb\x1b[{};{}H",
        ROW1 + 1,
        PREFIX + TEXT_W,
        ROW2 + 1,
        PREFIX + 1,
        ROW2 + 1,
        PREFIX + 3
    );
    let (cc, ms) = soft_wrap_then("b", &echo);
    let s = cc
        .shots
        .iter()
        .find(|s| s.ms >= ms)
        .expect("the echo's frame");
    assert!(
        s.text[1].starts_with("  Ib") && s.caret == (ROW2 as u16, PREFIX as u16 + 2),
        "the take reached the reflow: {:?} {:?}",
        s.caret,
        s.text
    );
    assert!(
        s.live[1].contains(&PREFIX) && !s.live[0].contains(&(PREFIX + TEXT_W - 1)),
        "the `I`'s light did not come down with it: row 1 live {:?}, row 2 live {:?}",
        s.live[0],
        s.live[1]
    );
}

/// D: a word that stayed on its row keeps its light there. The same
/// soft-wrapped `I`, then a Space and `b` answered by one chunk (a burst or
/// paste): `I` stays at row 1's end, the Space and `b` are written from the
/// indent, and the caret hops (17, 2) → (17, 4).
#[test]
fn a_soft_wrapped_word_that_stays_on_its_row_keeps_its_light_there() {
    let echo = format!(
        "\x1b[{};{}H b\x1b[{};{}H",
        ROW2 + 1,
        PREFIX + 1,
        ROW2 + 1,
        PREFIX + 3
    );
    let (cc, ms) = soft_wrap_then(" b", &echo);
    let s = cc
        .shots
        .iter()
        .find(|s| s.ms >= ms)
        .expect("the echo's frame");
    assert!(
        s.text[0].trim_end().ends_with("a lot I")
            && s.text[1].starts_with("   b")
            && s.caret == (ROW2 as u16, PREFIX as u16 + 2),
        "the take reached the burst: {:?} {:?}",
        s.caret,
        s.text
    );
    let gone: Vec<u64> = cc
        .shots
        .iter()
        .filter(|s| s.ms >= ms && s.ms <= ms + 100)
        .filter(|s| !s.live[0].contains(&(PREFIX + TEXT_W - 1)))
        .map(|s| s.ms - ms)
        .collect();
    assert!(
        gone.is_empty(),
        "the `I` that stayed at (16, 61) lost its live cell {gone:?} ms after the burst\n{}",
        cc.dump((ms.saturating_sub(40), ms + 48))
    );
}

/// Take 3 (`you did have`) and take 4 (one column more, `you do have`), replayed read by read
/// with the app's hints.
#[test]
fn claude_codes_recorded_reflow_keeps_every_typed_glyph_lit_and_strands_nothing() {
    let rec = recording(REC);
    let exact = take_keys(&rec, "you did have");
    let one = take_keys(&rec, "you do have");
    assert_eq!(exact.len(), TYPED.len(), "take 3's keys: {exact:?}");
    assert_eq!(one.len(), 13, "take 4's keys: {one:?}");
    let shots = replay((exact[0].1, one.last().expect("keys").1 + 1_000));
    let end = at(&shots, exact.last().expect("keys").1 + 100);
    assert!(
        end.text[1].starts_with("  already been runnignin [Image #1] in the"),
        "take 3 reached the owner's screen: {:?}",
        end.text
    );
    let glyphs: Vec<usize> = TYPED
        .char_indices()
        .filter(|&(_, c)| c != ' ')
        .map(|(i, _)| PREFIX + i)
        .collect();
    let dark: Vec<usize> = glyphs.iter().copied().filter(|&c| !end.lit[1][c]).collect();
    let wrap = one[1].1;
    let s = at(&shots, wrap + 50);
    let late = at(&shots, wrap + (RETRACT_FADE_S * 1000.0) as u64 + 50);
    assert!(
        s.text[1].starts_with("  al[Image #1] in the"),
        "take 4 reached the wrap: {:?}",
        s.text
    );
    let (space, lift) = one[7];
    assert_eq!(space, ' ', "take 4's 8th key");
    let lifted = at(&shots, lift + 50);
    assert!(
        lifted.text[0].trim_end().ends_with("do have already"),
        "take 4 reached the lift: {:?}",
        lifted.text
    );
    let unlit: Vec<usize> = LIFTED.filter(|&c| !lifted.lit[0][c]).collect();
    let faults = [
        (!dark.is_empty()).then(|| {
            format!(
                "take 3 (the owner's geometry): {} of {} typed glyphs dark 100 ms after the last \
                 key {dark:?}\n  r17 |{}|",
                dark.len(),
                glyphs.len(),
                map(&end.lit[1])
            )
        }),
        (!s.lit[1][PREFIX]).then(|| {
            format!(
                "take 4: the reflowed word's first glyph (17, 2) dark 50 ms after the wrap\n  \
                 r17 |{}|",
                map(&s.lit[1])
            )
        }),
        late.lit[0][55].then(|| {
            format!(
                "take 4: the moved `a`'s old cell (16, 55) still lit {} ms after its glyph left\n  \
                 r16 |{}|",
                late.ms - wrap,
                map(&late.lit[0])
            )
        }),
        (!unlit.is_empty()).then(|| {
            format!(
                "take 4: the lifted `already` dark at {unlit:?} 50 ms after the Space\n  r16 |{}|",
                map(&lifted.lit[0])
            )
        }),
    ];
    let faults: Vec<String> = faults.into_iter().flatten().collect();
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

/// The same wrap with the typed Space exactly before the moved word: the letter
/// is relaid, the indent stays dark, row 1 keeps no scrap past a retract fade.
#[test]
fn control_a_word_after_the_hands_own_space_is_relaid_exactly() {
    let mut cc = Cc::new("", 0);
    cc.type_str(&format!("{ROW1_ONE_FITS} "), 110);
    cc.insert(&format!("{CHIP} in the"), 300);
    let from = cc.insert_before_chip(TYPED, 110);
    let wrap = cc.echoed[cc.burst as usize + 1];
    let late = wrap + (RETRACT_FADE_S * 1000.0) as u64 + 2 * FRAME_MS;
    let f = cc.faults(from);
    assert!(
        !f.dark.iter().any(|&(ms, r, _)| r == ROW2 && ms > wrap),
        "{}",
        f.summary()
    );
    assert!(f.indent.is_empty(), "{}", f.summary());
    assert!(
        !f.stranded.iter().any(|&(ms, r, _)| r == ROW1 && ms >= late),
        "{}",
        f.summary()
    );
}

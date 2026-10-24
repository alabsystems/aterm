// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Holder fence oracle and unit tests.

use std::fmt::Write as _;
use std::sync::OnceLock;

use aterm_grid::{AnchorRole, CellFlags, FenceKnob, LineSize, with_knob};

use super::super::{CutKind, HolderAdoption, HolderCut, HolderView, Terminal};
use crate::grid::Grid;
use crate::scrollback::Scrollback;

const ROWS: u16 = 10;
const COLS: u16 = 16;
const DRAWERS: u8 = 3;

/// Who writes: the shell, a non-drawer tool (`vim`, `cat`), or a drawer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Who {
    Shell,
    Tool,
    Drawer(u8),
}

impl Who {
    fn glyph(self) -> char {
        match self {
            Self::Shell => 's',
            Self::Tool => 't',
            Self::Drawer(i) => ['Ж', 'Щ', 'Ю'][usize::from(i)],
        }
    }

    fn rich(self) -> [char; 2] {
        match self {
            Self::Drawer(i) => [
                ['龍', '鳳', '麟'][usize::from(i)],
                ['𝔄', '𝔅', '𝔇'][usize::from(i)],
            ],
            other => [other.glyph(); 2],
        }
    }

    fn bg(self) -> u8 {
        match self {
            Self::Shell => 160,
            Self::Tool => 161,
            Self::Drawer(i) => 170 + i,
        }
    }

    fn rgb(self) -> [u8; 3] {
        match self {
            Self::Shell => [2, 4, 6],
            Self::Tool => [3, 5, 7],
            Self::Drawer(i) => [1, 2, 3 + i],
        }
    }

    fn text(self, n: u64, rng: &mut Rng) -> String {
        (0..n)
            .map(|_| match (self, rng.below(8)) {
                (Self::Drawer(_), 0) => self.rich()[0],
                (Self::Drawer(_), 1) => self.rich()[1],
                _ => self.glyph(),
            })
            .collect()
    }
}

fn own(i: u8, n: usize) -> String {
    std::iter::repeat_n(Who::Drawer(i).glyph(), n).collect()
}

fn token(i: u8) -> u64 {
    1000 + u64::from(i)
}

fn group(to: Option<u64>, from_alive: bool) -> HolderCut {
    HolderCut {
        to,
        kind: CutKind::Group,
        from_alive,
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[usize::try_from(self.below(items.len() as u64)).unwrap_or(0)]
    }
}

fn term() -> Terminal {
    let scrollback = Scrollback::new(16, 256, 1_000_000);
    Terminal::with_scrollback(ROWS, COLS, 24, scrollback)
}

/// What the oracle reads in a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shows {
    /// What an erased cell shows, or a background of the writer asked about.
    Blank,
    /// One of that writer's glyphs, in its pen.
    Own,
    /// Anything else.
    Foreign,
}

fn stroked(flags: CellFlags) -> bool {
    flags.intersects(CellFlags(
        CellFlags::INVERSE.0 | CellFlags::ALL_UNDERLINES.0 | CellFlags::STRIKETHROUGH.0,
    )) || flags.contains(CellFlags::OVERLINE)
}

fn shows(grid: &Grid, r: u16, c: u16, who: Option<Who>) -> Shows {
    let (Some(row), Some(cell)) = (grid.row(r), grid.cell(r, c)) else {
        return Shows::Blank;
    };
    let extra = grid.cell_extra(r, c);
    let painted = extra.is_some_and(|e| {
        !e.images().is_empty() || !e.combining().is_empty() || e.hyperlink().is_some()
    });
    if row.is_cell_wide_continuation(c) {
        return if extra.is_some_and(|e| !e.images().is_empty()) {
            Shows::Foreign
        } else {
            Shows::Blank
        };
    }
    let flags = cell.flags();
    if painted || stroked(flags) {
        return Shows::Foreign;
    }
    let colors = cell.colors();
    let bg_rgb = colors.bg_is_rgb().then(|| grid.bg_rgb_at(r, c)).flatten();
    let bg_default = colors.bg_is_default() || (colors.bg_is_rgb() && bg_rgb.is_none());
    let fg_default =
        colors.fg_is_default() || (colors.fg_is_rgb() && grid.fg_rgb_at(r, c).is_none());
    let pen_bg = who.is_some_and(|w| {
        (colors.bg_is_indexed() && colors.bg_index() == w.bg()) || bg_rgb == Some(w.rgb())
    });
    let bg_ok = bg_default || pen_bg;
    if cell.is_complex() {
        let mut text = String::new();
        let held = grid.push_complex_char_at(r, c, &mut text);
        let mine = held && who.is_some_and(|w| text == w.rich()[1].to_string());
        return if mine && bg_ok && fg_default {
            Shows::Own
        } else {
            Shows::Foreign
        };
    }
    let ch = cell.char();
    if ch == ' ' || ch == '\0' {
        return if bg_ok { Shows::Blank } else { Shows::Foreign };
    }
    let style = flags.0 & !(CellFlags::WIDE.0 | CellFlags::PROTECTED.0 | CellFlags::COMPLEX.0);
    let mine = who.is_some_and(|w| ch == w.glyph() || ch == w.rich()[0]);
    if mine && bg_ok && style == 0 && fg_default {
        Shows::Own
    } else {
        Shows::Foreign
    }
}

fn foreign_in(grid: &Grid, r: u16, who: Option<Who>) -> Option<String> {
    if grid
        .row(r)
        .is_some_and(|row| row.line_size() != LineSize::SingleWidth)
    {
        return Some(format!("row {r} is double-size"));
    }
    (0..grid.cols())
        .find(|&c| shows(grid, r, c, who) == Shows::Foreign)
        .map(|c| {
            format!(
                "row {r} col {c} holds {:?} {:?} {:?} rgb {:?} extra {:?}",
                grid.resolved_char(r, c),
                grid.cell(r, c).map(|cell| cell.flags()),
                grid.cell(r, c).map(|cell| cell.colors()),
                (grid.fg_rgb_at(r, c), grid.bg_rgb_at(r, c)),
                grid.cell_extra(r, c),
            )
        })
}

fn drawn_from(grid: &Grid, r: u16) -> bool {
    (r..grid.rows()).any(|row| foreign_in(grid, row, None).is_some())
}

fn oracle(t: &Terminal) -> Result<u64, String> {
    let fence = t.holder_fence();
    if fence.marks.len() > super::MAX_HOLDER_MARKS {
        return Err(format!("{} marks", fence.marks.len()));
    }
    if let Some(m) = fence
        .marks
        .iter()
        .find(|m| m.spans.len() > super::MAX_HOLDER_SPANS)
    {
        return Err(format!("{} spans on {}", m.spans.len(), m.token));
    }
    let view = t.holder_view();
    if view.token == 0 {
        return Ok(0);
    }
    let who = Who::Drawer(u8::try_from(view.token - 1000).expect("a drawer token"));
    let main = t.main_grid();
    let rows = main.rows();
    let base = main.visible_to_absolute(0);
    let foreign: Vec<Option<String>> = (0..rows).map(|r| foreign_in(main, r, Some(who))).collect();
    for lo in 0..rows {
        for hi in lo + 1..=rows {
            if !view.admits_rows(base + u64::from(lo), base + u64::from(hi)) {
                continue;
            }
            if let Some((r, what)) =
                (lo..hi).find_map(|r| foreign[usize::from(r)].as_ref().map(|w| (r, w)))
            {
                return Err(format!(
                    "rows [{lo}, {hi}) (abs {}) admitted under {who:?}, main row {r}: {what}: {view:?}",
                    base + u64::from(lo)
                ));
            }
        }
    }
    let drawn = (0..rows)
        .filter(|&r| view.admits_rows(base + u64::from(r), base + u64::from(r) + 1))
        .filter(|&r| (0..main.cols()).any(|c| shows(main, r, c, Some(who)) == Shows::Own))
        .count();
    if t.modes().alternate_screen && view.admits_alt() {
        let alt = t.grid();
        if let Some(what) = (0..alt.rows()).find_map(|r| foreign_in(alt, r, Some(who))) {
            return Err(format!("alt screen admitted under {who:?}: {what}"));
        }
    }
    Ok(drawn as u64)
}

struct Corpora {
    /// Each unit whole, for slices cut anywhere.
    units: Vec<Vec<u8>>,
    /// The lines a line-oriented program prints (no cursor addressing, no
    /// clear), each with its line end.
    lines: Vec<Vec<u8>>,
}

fn corpora() -> &'static Corpora {
    static CORPORA: OnceLock<Corpora> = OnceLock::new();
    CORPORA.get_or_init(|| {
        let throughput: [&[u8]; 8] = [
            b"the quick brown fox jumps over the lazy dog 0123456789\r\n",
            b"\x1b[1;38;5;202mERROR\x1b[0m \x1b[4;48;5;19mwarning\x1b[0m normal text here\r\n",
            b"\x1b[2K\x1b[1Gredrawn line after erase-in-line and cursor-to-col-1\r\n",
            b"\x1b[7minverse\x1b[27m \x1b[3mitalic\x1b[23m \x1b[9mstrike\x1b[29m mixed sgr\r\n",
            b"\x1b[10;5Hpositioned via CUP \x1b[K then erased to end of line\r\n",
            b"plain throughput line with a tab\tand more ascii content padding 42\r\n",
            b"\x1b[38;2;120;200;255mtruecolor\x1b[0m run plus \x1b[48;2;10;10;10mbg\x1b[0m\r\n",
            b"\x1b[H\x1b[2Jfull clear then home, forcing a screen reset every frame\r\n",
        ];
        let mut storm = String::new();
        for i in 0u32..24 {
            let (row, col) = (1 + (i * 7) % 24, 1 + (i * 13) % 80);
            let _ = write!(
                storm,
                "\x1b7\x1b[{row};{col}H\x1b[1K*\x1b[{}A\x1b[{}Cx\x1b8",
                1 + i % 8,
                1 + i % 11
            );
        }
        storm.push_str("\x1b[H\x1b[2J");
        let mut churn = Vec::new();
        for line in 0u32..20 {
            let mut s = String::new();
            for cell in line * 80..(line + 1) * 80 {
                let (r, g, b) = ((cell * 7) & 0xff, (cell * 13) & 0xff, (cell * 29) & 0xff);
                let ch = char::from(b'a' + u8::try_from(cell % 26).unwrap_or(0));
                let _ = write!(s, "\x1b[38;2;{r};{g};{b}m\x1b[48;2;{b};{r};{g}m{ch}");
            }
            s.push_str("\x1b[0m\r\n");
            churn.push(s.into_bytes());
        }
        let mut url = String::from("https://example.invalid/");
        while url.len() < 900 {
            let _ = write!(url, "seg{:04x}/", url.len());
        }
        let mut title = String::from("pathological-title-");
        while title.len() < 600 {
            let _ = write!(title, "{:03x}", title.len());
        }
        let escapes = format!(
            "\x1b]8;;{url}\x1b\\link text under a kilobyte-long target\x1b]8;;\x1b\\ \
             \x1b]0;{title}\x07 plain tail\r\n"
        )
        .into_bytes();
        let wide = "漢字テスト混合行 한국어 텍스트 🚀👨\u{200d}👩\u{200d}👧\u{200d}👦🎨 \
                    e\u{301}e\u{301}e\u{301} Ω≈ç√∫ 中文寬字符壓力測試 🔥💧🌈\r\n"
            .as_bytes()
            .to_vec();
        let mut lines: Vec<Vec<u8>> = [0, 1, 2, 3, 5, 6]
            .iter()
            .map(|&i| throughput[i].to_vec())
            .collect();
        lines.extend([b"y\r\n".to_vec(), escapes.clone(), wide.clone()]);
        lines.extend(churn.iter().cloned());
        Corpora {
            units: vec![
                throughput.concat(),
                b"y\r\n".to_vec(),
                storm.into_bytes(),
                churn.concat(),
                escapes,
                wide,
            ],
            lines,
        }
    })
}

const RICH: &[&str] = &[
    "漢字テ",
    "👨\u{200d}👩\u{200d}👧 🚀",
    "e\u{301}a\u{308} \u{301}",
    "\x1b[38;2;9;9;9;48;2;200;10;10mrgb \x1b[49m ",
    "\x1b#6dw",
    "\x1b[1\"qprot\x1b[0\"q",
    "\x1b[7m  \x1b[27m",
    "\x1b[4m \x1b[4:3m \x1b[21m \x1b[24m",
    "\x1b[9m \x1b[53m \x1b[0m",
    "\x1b]8;;https://x.invalid/a\x1b\\  \x1b]8;;\x1b\\",
    KITTY_IMAGE,
    SIXEL_IMAGE,
    ITERM_IMAGE,
    "\x1b_Ga=T,f=24,s=1,v=1,c=3,r=1;AAAA\x1b\\",
];

struct Sim {
    t: Terminal,
    rng: Rng,
    fg: Who,
    /// The tool's parent, while a tool is foreground.
    parent: Option<Who>,
    stopped: Vec<u8>,
    alive: [bool; DRAWERS as usize],
    /// The seed's share of hostile writes, in percent: screen-addressed
    /// writes left where they ended, corpus slices, rectangle copies,
    /// margined edits, RIS, unscrolls.
    noise: u64,
    /// A double-size row was ever drawn: it is resized in place, never
    /// rewrapped, so a width change's lend cannot vouch for the lines around
    /// it and fails closed (the frontier law does not hold after one).
    double_size: bool,
    /// Resizes the frontier law judged (its premises held).
    frontier_resizes: u64,
    log: Vec<String>,
}

impl Sim {
    fn new(seed: u64) -> Self {
        let mut rng = Rng(seed);
        let noise = *rng.pick(&[3, 12, 40]);
        Self {
            t: term(),
            rng,
            noise,
            fg: Who::Shell,
            parent: None,
            stopped: Vec::new(),
            alive: [false; DRAWERS as usize],
            double_size: false,
            frontier_resizes: 0,
            log: Vec::new(),
        }
    }

    fn cut(&mut self, cut: HolderCut) {
        self.log.push(format!("cut {cut:?}"));
        self.t.note_holder_cut(cut);
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut out = self.pen();
        out.extend_from_slice(bytes);
        self.double_size |= out.windows(2).any(|w| w == b"\x1b#");
        self.log.push(format!(
            "{:?} writes {:?}",
            self.fg,
            String::from_utf8_lossy(&out)
        ));
        self.t.process(&out);
    }

    fn pen(&mut self) -> Vec<u8> {
        let mut b = b"\x1b]8;;\x1b\\\x1b[0\"q".to_vec();
        match self.rng.below(4) {
            0 => b.extend_from_slice(b"\x1b[0m"),
            1 => {
                let [r, g, bl] = self.fg.rgb();
                b.extend(format!("\x1b[0;48;2;{r};{g};{bl}m").into_bytes());
            }
            _ => b.extend(format!("\x1b[0;48;5;{}m", self.fg.bg()).into_bytes()),
        }
        b
    }

    fn glyphs(&mut self, n: u64) -> Vec<u8> {
        let mut out = self.pen();
        out.extend(self.fg.text(n, &mut self.rng).into_bytes());
        out
    }

    fn foreign_line(&mut self, n: u64) -> Vec<u8> {
        if matches!(self.fg, Who::Drawer(_)) {
            return self.glyphs(n);
        }
        match self.rng.below(4) {
            0 => {
                let mut b = self.pen();
                b.extend_from_slice(self.rng.pick(RICH).as_bytes());
                b
            }
            1 => {
                let mut b = self.pen();
                b.extend_from_slice(self.rng.pick::<Vec<u8>>(&corpora().lines));
                b
            }
            _ => self.glyphs(n),
        }
    }

    fn corpus_slice(&mut self) -> Vec<u8> {
        let unit = self.rng.pick(&corpora().units);
        let start = usize::try_from(self.rng.below(unit.len() as u64)).unwrap_or(0);
        let len = usize::try_from(self.rng.below(200) + 1).unwrap_or(1);
        let mut b = unit[start..(start + len).min(unit.len())].to_vec();
        b.push(0x18);
        b
    }

    fn dead_drawer(&mut self) -> Option<u8> {
        let dead: Vec<u8> = (0..DRAWERS)
            .filter(|&i| !self.alive[usize::from(i)])
            .collect();
        (!dead.is_empty()).then(|| *self.rng.pick(&dead))
    }

    fn open_span_screen_row(&self) -> Option<u16> {
        let view = self.t.holder_view();
        let main = self.t.main_grid();
        let (start, _) = view.spans().iter().copied().find(|&(_, e)| e == u64::MAX)?;
        let base = main.visible_to_absolute(0);
        start
            .checked_sub(base)
            .and_then(|r| u16::try_from(r).ok())
            .filter(|&r| r < main.rows())
    }

    fn suspended_screen_row(&mut self) -> Option<u16> {
        let fence = self.t.holder_fence();
        let main = self.t.main_grid();
        let base = main.visible_to_absolute(0);
        let rows: Vec<u16> = fence
            .marks
            .iter()
            .enumerate()
            .filter(|&(i, _)| fence.current.map(usize::from) != Some(i))
            .flat_map(|(_, m)| m.spans.iter())
            .filter_map(|span| {
                let anchors = main.anchors();
                let start = anchors.row(span.start)?;
                let end = span.end.and_then(|e| anchors.row(e))?;
                Some(start..end)
            })
            .flatten()
            .filter_map(|r| r.checked_sub(base).and_then(|s| u16::try_from(s).ok()))
            .filter(|&s| s < main.rows())
            .collect();
        (!rows.is_empty()).then(|| *self.rng.pick(&rows))
    }

    /// A suspended mark's span start, if one is on screen.
    fn suspended_start_screen_row(&mut self) -> Option<u16> {
        let fence = self.t.holder_fence();
        let main = self.t.main_grid();
        let base = main.visible_to_absolute(0);
        let rows: Vec<u16> = fence
            .marks
            .iter()
            .enumerate()
            .filter(|&(i, _)| fence.current.map(usize::from) != Some(i))
            .flat_map(|(_, m)| m.spans.iter())
            .filter_map(|span| main.anchors().row(span.start)?.checked_sub(base))
            .filter_map(|s| u16::try_from(s).ok())
            .filter(|&s| s < main.rows())
            .collect();
        (!rows.is_empty()).then(|| *self.rng.pick(&rows))
    }

    fn calm(&mut self) -> bool {
        !self.rng.chance(self.noise)
    }

    fn to_frontier(&mut self) {
        if self.t.modes().alternate_screen {
            return;
        }
        let main = self.t.main_grid();
        let rows = main.rows();
        let bytes = match (0..rows).find(|&r| !drawn_from(main, r)) {
            Some(r) => format!("\x1b[0m\x1b[{};1H", r + 1),
            None => format!("\x1b[0m\x1b[{rows};1H\r\n"),
        };
        self.log.push(format!("{:?} to the frontier", self.fg));
        self.t.process(bytes.as_bytes());
    }

    fn lines(&mut self, ended: u64) -> Vec<u8> {
        let cols = u64::from(self.t.grid().cols());
        let n = self.rng.below(cols * 2 + 1);
        let mut b = self.foreign_line(n);
        if self.rng.chance(ended) {
            b.extend_from_slice(b"\x1b[0m\r\n");
        }
        b
    }

    fn clear_and_cat(&mut self) -> Vec<u8> {
        let cols = u64::from(self.t.grid().cols());
        let mut b = self.pen();
        b.extend_from_slice(b"\x1b[H\x1b[2J");
        for _ in 0..=self.rng.below(4) {
            let n = self.rng.below(cols) + 1;
            b.extend(self.foreign_line(n));
            b.extend_from_slice(b"\x1b[0m\r\n");
        }
        b
    }

    fn insert_or_delete_lines(&mut self) -> Vec<u8> {
        let rows = u64::from(self.t.grid().rows());
        let row = match self.open_span_screen_row() {
            Some(r) if self.rng.chance(60) => self.rng.below(u64::from(r) + 1) + 1,
            _ => match self.suspended_start_screen_row() {
                Some(r) if self.rng.chance(30) => u64::from(r) + 1,
                _ => self.rng.below(rows) + 1,
            },
        };
        let op = if self.rng.chance(50) { 'L' } else { 'M' };
        let mut b = self.pen();
        b.extend(format!("\x1b[{row};1H\x1b[{}{op}", self.rng.below(3) + 1).into_bytes());
        b
    }

    fn region_scroll(&mut self) -> Vec<u8> {
        let rows = u64::from(self.t.grid().rows());
        let top = match self.open_span_screen_row() {
            Some(r) if self.rng.chance(40) => u64::from(r) + 1,
            _ => match self.suspended_start_screen_row() {
                Some(r) if self.rng.chance(30) => u64::from(r) + 1,
                _ => self.rng.below(rows) + 1,
            },
        };
        let bottom = (top + self.rng.below(rows)).min(rows);
        let mut b = format!("\x1b[{top};{bottom}r").into_bytes();
        b.extend(self.pen());
        match self.rng.below(4) {
            0 => b.extend(format!("\x1b[{bottom};1H\n").into_bytes()),
            1 => b.extend(format!("\x1b[{top};1H\x1bM").into_bytes()),
            2 => b.extend(format!("\x1b[{}S", self.rng.below(3) + 1).into_bytes()),
            _ => b.extend(format!("\x1b[{}T", self.rng.below(3) + 1).into_bytes()),
        }
        b.extend_from_slice(b"\x1b[r");
        b
    }

    /// Rows erased from at or above the open span's start: whole rows a row at a time down to
    /// the cursor (as Claude Code redraws after a resize), the display from a column of a row
    /// (ED 0), or from the top to a column of the cursor's row (ED 1).
    fn sweep(&mut self) -> Vec<u8> {
        let cols = u64::from(self.t.grid().cols());
        let cursor = u64::from(self.t.grid().cursor().row);
        let from = match self.open_span_screen_row() {
            Some(start) if self.rng.chance(50) => self.rng.below(u64::from(start) + 1),
            _ => self.rng.below(cursor + 1),
        };
        let mut b = Vec::new();
        match self.rng.below(4) {
            0 => {
                let col = self.rng.below(cols) + 1;
                b.extend(format!("\x1b[{};{col}H\x1b[J", from + 1).into_bytes());
            }
            1 => {
                let col = self.rng.below(cols) + 1;
                b.extend(format!("\x1b[{};{col}H\x1b[1J", cursor + 1).into_bytes());
            }
            _ => {
                b.extend(format!("\x1b[{};1H", from + 1).into_bytes());
                for _ in from..=cursor.max(from) {
                    b.extend_from_slice(b"\x1b[2K\x1b[1B");
                }
            }
        }
        b.extend(format!("\x1b[{};1H", from + 1).into_bytes());
        b
    }

    fn alt_step(&mut self) -> Result<(), String> {
        let mode = *self.rng.pick(&["1049", "47", "1047"]);
        let set = if self.rng.chance(55) { 'h' } else { 'l' };
        let held = !self.t.main_grid().anchors().lost();
        let mut b = self.pen();
        b.extend(format!("\x1b[?{mode}{set}").into_bytes());
        self.write(&b);
        if held && self.t.main_grid().anchors().lost() {
            return Err(format!("?{mode}{set} lost the anchors"));
        }
        Ok(())
    }

    fn calm_write_step(&mut self) -> Result<(), String> {
        let bytes = match self.rng.below(21) {
            0..=9 => self.lines(80),
            10..=12 => {
                let cursor = u64::from(self.t.grid().cursor().row);
                let aimed = match self.fg {
                    Who::Drawer(_) => self
                        .open_span_screen_row()
                        .map(u64::from)
                        .filter(|&start| start <= cursor)
                        .map(|start| start + self.rng.below(cursor - start + 1)),
                    _ => self
                        .suspended_screen_row()
                        .map(u64::from)
                        .filter(|&r| r <= cursor),
                };
                let row = aimed.unwrap_or_else(|| self.rng.below(cursor + 1));
                let cols = u64::from(self.t.grid().cols());
                let mut b = format!("\x1b7\x1b[{};1H", row + 1).into_bytes();
                if self.rng.chance(25) {
                    b.extend(self.pen());
                    b.extend_from_slice(b"\x1b[2K");
                } else {
                    let n = self.rng.below(cols) + 1;
                    b.extend(self.foreign_line(n));
                }
                b.extend_from_slice(b"\x1b8");
                b
            }
            13 => self.clear_and_cat(),
            14 | 15 => self.insert_or_delete_lines(),
            16 => return self.archival_step(),
            17 => self.region_scroll(),
            18 => return self.alt_step(),
            19 => self.sweep(),
            _ => b"\x1b[0m\r\n".to_vec(),
        };
        self.write(&bytes);
        Ok(())
    }

    fn step(&mut self) -> Result<u64, String> {
        match self.rng.below(100) {
            0..=11 => self.cut_step(),
            12..=88 => self.write_step()?,
            89..=94 => self.resize_step()?,
            95 | 96 => self.reveal_step()?,
            _ => self.checkpoint_step()?,
        }
        self.stopped_keep_marks()?;
        oracle(&self.t)
    }

    fn stopped_keep_marks(&self) -> Result<(), String> {
        let fence = self.t.holder_fence();
        if fence.marks.len() >= super::MAX_HOLDER_MARKS {
            return Ok(());
        }
        match self
            .stopped
            .iter()
            .find(|&&i| fence.tokens().all(|t| t != token(i)))
        {
            Some(i) => Err(format!("stopped drawer {i} lost its mark")),
            None => Ok(()),
        }
    }

    fn cut_step(&mut self) {
        if self.calm() {
            // A program leaves the alternate screen before it stops or
            // exits, and the next one starts below what it drew.
            if self.t.modes().alternate_screen {
                self.write(b"\x1b[?1049l");
            }
            self.to_frontier();
        }
        match self.fg {
            Who::Shell => match self.rng.below(6) {
                0 | 1 => {
                    if let Some(i) = self.dead_drawer() {
                        // Sometimes the shell left its prompt on the
                        // cursor's row (the drawer lays on the next), or
                        // returned under its text (the lay taints).
                        if self.rng.chance(30) {
                            let n = self.rng.below(6) + 1;
                            let mut bytes = self.glyphs(n);
                            if self.rng.chance(40) {
                                bytes.extend_from_slice(b"\r");
                            }
                            self.write(&bytes);
                        }
                        self.alive[usize::from(i)] = true;
                        self.fg = Who::Drawer(i);
                        self.cut(group(Some(token(i)), true));
                    }
                }
                2 => {
                    if let Some(i) = self.stopped.pop() {
                        // `fg` typed at the prompt, sometimes after
                        // `clear; cat card.txt` rewrote the drawer's rows.
                        if self.rng.chance(25) {
                            let b = self.clear_and_cat();
                            self.write(&b);
                        }
                        let mut b = self.glyphs(2);
                        b.extend_from_slice(b"\x1b[0m\r\n");
                        self.write(&b);
                        self.fg = Who::Drawer(i);
                        self.cut(group(Some(token(i)), true));
                    }
                }
                3 => {
                    self.parent = Some(Who::Shell);
                    self.fg = Who::Tool;
                    self.cut(group(None, true));
                }
                4 => {
                    if let Some(i) = self.dead_drawer() {
                        self.alive[usize::from(i)] = true;
                        self.fg = Who::Drawer(i);
                        self.cut(HolderCut {
                            to: Some(token(i)),
                            kind: CutKind::Exec,
                            from_alive: true,
                        });
                    }
                }
                _ => {
                    if !self.stopped.is_empty() {
                        let k =
                            usize::try_from(self.rng.below(self.stopped.len() as u64)).unwrap_or(0);
                        let i = self.stopped.remove(k);
                        self.alive[usize::from(i)] = false;
                        self.log.push(format!("kill {i}"));
                        self.t.retire_holder_mark(token(i));
                    }
                }
            },
            Who::Drawer(i) => match self.rng.below(4) {
                0 => {
                    self.alive[usize::from(i)] = false;
                    self.fg = Who::Shell;
                    self.cut(group(None, false));
                }
                1 => {
                    self.stopped.push(i);
                    self.fg = Who::Shell;
                    self.cut(group(None, true));
                }
                2 => {
                    if let Some(j) = self.dead_drawer() {
                        self.alive[usize::from(i)] = false;
                        self.alive[usize::from(j)] = true;
                        self.fg = Who::Drawer(j);
                        self.cut(HolderCut {
                            to: Some(token(j)),
                            kind: CutKind::Exec,
                            from_alive: true,
                        });
                    }
                }
                _ => {
                    self.parent = Some(Who::Drawer(i));
                    self.fg = Who::Tool;
                    self.cut(group(None, true));
                }
            },
            Who::Tool => {
                let parent = self.parent.take().unwrap_or(Who::Shell);
                self.fg = parent;
                match parent {
                    Who::Drawer(i) => self.cut(group(Some(token(i)), false)),
                    _ => self.cut(group(None, false)),
                }
            }
        }
    }

    fn write_step(&mut self) -> Result<(), String> {
        if self.calm() {
            return self.calm_write_step();
        }
        let rows = u64::from(self.t.grid().rows());
        let cols = u64::from(self.t.grid().cols());
        let bytes: Vec<u8> = match self.rng.below(32) {
            0..=7 => {
                let n = self.rng.below(cols * 2 + 1);
                let mut b = self.foreign_line(n);
                if self.rng.chance(70) {
                    b.extend_from_slice(b"\r\n");
                }
                b
            }
            8..=10 => {
                let row = match self.suspended_screen_row() {
                    Some(r) if self.rng.chance(50) => u64::from(r),
                    _ => self.rng.below(rows),
                };
                let mut b = format!("\x1b[{};{}H", row + 1, self.rng.below(cols) + 1).into_bytes();
                let n = self.rng.below(cols);
                b.extend(self.foreign_line(n));
                b
            }
            11 => {
                let mut b = self.pen();
                b.extend(format!("\x1b[{}K", self.rng.below(3)).into_bytes());
                b
            }
            12 => {
                let mut b = self.pen();
                b.extend(format!("\x1b[{}J", self.rng.below(4)).into_bytes());
                b
            }
            13 => {
                let mut b = self.pen();
                b.extend_from_slice(b"\x1b[H\x1b[2J");
                b
            }
            14 | 15 => self.insert_or_delete_lines(),
            16 | 17 => self.region_scroll(),
            18 | 19 => return self.archival_step(),
            20 => return self.alt_step(),
            21 => format!("\x1b[{}+T", self.rng.below(3) + 1).into_bytes(),
            22 => {
                let st = self.rng.below(rows) + 1;
                let dt = self.rng.below(rows) + 1;
                format!("\x1b[{st};1;{st};{cols};1;{dt};1;1$v").into_bytes()
            }
            23 => {
                // DECLRMM.
                format!(
                    "\x1b[?69h\x1b[2;{}s\x1b[{};2H\x1b[L\x1b[?69l",
                    cols - 1,
                    self.rng.below(rows) + 1
                )
                .into_bytes()
            }
            24 => {
                if self.rng.chance(20) {
                    b"\x1bc".to_vec()
                } else {
                    b"\x1b7".to_vec()
                }
            }
            25 => b"\x1b8".to_vec(),
            26 => format!("\x1b[{}A", self.rng.below(4) + 1).into_bytes(),
            27 => {
                let op = if self.rng.chance(50) { "'}" } else { "'~" };
                format!("\x1b[{}{op}", self.rng.below(3) + 1).into_bytes()
            }
            28 | 29 if !matches!(self.fg, Who::Drawer(_)) => self.corpus_slice(),
            _ => b"\r\n".to_vec(),
        };
        self.write(&bytes);
        Ok(())
    }

    fn archival_step(&mut self) -> Result<(), String> {
        if self.t.modes().alternate_screen {
            return Ok(());
        }
        let rows = self.t.grid().rows();
        let base = self.t.grid().visible_to_absolute(0);
        let end_on_screen = self
            .t
            .grid()
            .anchors()
            .slots()
            .iter()
            .filter(|a| a.live && a.role == AnchorRole::End)
            .filter_map(|a| a.row.checked_sub(base).and_then(|r| u16::try_from(r).ok()))
            .find(|&r| r >= 2 && r < rows);
        let bottom = match end_on_screen {
            Some(r) if self.rng.chance(70) => r,
            _ => u16::try_from(self.rng.below(u64::from(rows) - 2) + 2).unwrap_or(2),
        };
        let mut b = format!("\x1b[1;{bottom}r").into_bytes();
        b.extend(self.pen());
        self.write(&b);
        for _ in 0..=self.rng.below(2) {
            let grid = self.t.grid();
            let base = grid.visible_to_absolute(0);
            let at = base + u64::from(bottom);
            let before: Vec<_> = grid.anchors().slots().to_vec();
            let revision = self.t.absolute_row_revision();
            self.write(format!("\x1b[{bottom};1H\n").as_bytes());
            let spliced = self.t.absolute_row_revision() != revision;
            let after = self.t.grid().anchors().slots();
            for (slot, (was, now)) in before.iter().zip(after).enumerate() {
                if !was.live || !now.live || self.t.grid().anchors().lost() {
                    continue;
                }
                let want = if spliced {
                    let shifts = match was.role {
                        AnchorRole::Start => was.row >= at,
                        AnchorRole::End => was.row > at,
                    };
                    was.row + u64::from(shifts)
                } else if (base..at).contains(&was.row) {
                    was.row.saturating_sub(1).max(base)
                } else {
                    was.row
                };
                if now.row != want {
                    return Err(format!(
                        "splice at {at}: {:?} anchor {slot} at {} went to {}, want {want}",
                        was.role, was.row, now.row
                    ));
                }
            }
        }
        self.write(b"\x1b[r");
        Ok(())
    }

    fn resize_step(&mut self) -> Result<(), String> {
        let main = self.t.main_grid();
        let held = main.anchors().live_count() > 0 && !main.anchors().lost();
        let frontier =
            !drawn_from(main, main.cursor().row.saturating_add(1)) && main.pushed_rows() == 0;
        let readable = main.lend_reads_cursor_line();
        let (rows, cols) = (self.t.grid().rows(), self.t.grid().cols());
        let rows2 = u16::try_from(self.rng.below(6) + 6).unwrap_or(ROWS);
        let cols2 = u16::try_from(self.rng.below(12) + 8).unwrap_or(COLS);
        match self.rng.below(3) {
            0 => {
                self.log
                    .push(format!("rows-only resize {rows}x{cols} to {rows2}x{cols}"));
                self.t.resize(rows2, cols);
            }
            1 => {
                self.log
                    .push(format!("width resize {rows}x{cols} to {rows}x{cols2}"));
                self.t.resize(rows, cols2);
            }
            _ => {
                self.log
                    .push(format!("offloaded resize {rows}x{cols} to {rows2}x{cols2}"));
                let mut pending = self.t.resize_offloading_scrollback(rows2, cols2);
                while let Some(job) = pending {
                    pending = self.t.finish_resize_offload(job.reflow());
                }
            }
        }
        let after = self.t.main_grid();
        let lent = after.cols() != cols;
        let read_whole = !lent || (readable && after.lend_reads_cursor_line());
        if held && frontier && read_whole && !self.double_size {
            self.frontier_resizes += 1;
            if after.anchors().lost() {
                return Err("a resize with the cursor at the frontier lost the anchors".into());
            }
        }
        Ok(())
    }

    fn reveal_step(&mut self) -> Result<(), String> {
        if self.stopped.is_empty() || self.t.modes().alternate_screen {
            return self.resize_step();
        }
        let (rows, cols) = (self.t.grid().rows(), self.t.grid().cols());
        let grown = rows + u16::try_from(self.rng.below(4) + 1).unwrap_or(1);
        self.log.push(format!(
            "reveal: rows-only resize {rows}x{cols} to {grown}x{cols}"
        ));
        self.t.resize(grown, cols);
        let row = match self.suspended_screen_row() {
            Some(r) if self.rng.chance(70) => r,
            _ => u16::try_from(self.rng.below(u64::from(grown - rows) + 1)).unwrap_or(0),
        };
        let n = self.rng.below(u64::from(cols)) + 1;
        let mut b = format!("\x1b7\x1b[{};1H", row + 1).into_bytes();
        b.extend(self.foreign_line(n));
        b.extend_from_slice(b"\x1b8");
        self.write(&b);
        if self.rng.chance(50) {
            self.log
                .push(format!("rows-only resize {grown}x{cols} to {rows}x{cols}"));
            self.t.resize(rows, cols);
        }
        Ok(())
    }

    fn checkpoint_step(&mut self) -> Result<(), String> {
        let c = self.t.checkpoint();
        self.log.push("checkpoint round trip".into());
        let mut restored = Terminal::from_checkpoint(&c);
        if restored.checkpoint().holder != c.holder {
            return Err(format!(
                "the holder repr does not round-trip:\n  {:?}\n  {:?}\n  counters {} {} oldest {}",
                c.holder,
                restored.checkpoint().holder,
                c.absolute_row_counter,
                restored.grid().absolute_row_counter(),
                restored.grid().oldest_absolute_row(),
            ));
        }
        restored.restore_checkpoint(&c);
        let (holder, drawer) = match self.fg {
            Who::Drawer(i) => (token(i), true),
            _ => (0, false),
        };
        restored.adopt_holder_after_restore(holder, drawer);
        self.t = restored;
        Ok(())
    }
}

/// What a run exercised: the admitted drawer rows the oracle checked, and
/// the resizes the frontier law judged.
#[derive(Clone, Copy, Debug, Default)]
struct Coverage {
    drawn: u64,
    frontier: u64,
}

fn fuzz(seed: u64, steps: usize) -> Result<Coverage, String> {
    let mut sim = Sim::new(seed);
    let mut drawn = 0;
    for _ in 0..steps {
        match sim.step() {
            Ok(n) => drawn += n,
            Err(e) => {
                let tail = sim.log.len().saturating_sub(12);
                return Err(format!(
                    "seed {seed}: {e}\n  {}",
                    sim.log[tail..].join("\n  ")
                ));
            }
        }
    }
    Ok(Coverage {
        drawn,
        frontier: sim.frontier_resizes,
    })
}

const SEEDS: u64 = 1000;
const STEPS: usize = 220;
const DRAWN_FLOOR: u64 = 70_000;
const FRONTIER_FLOOR: u64 = 1_400;

#[test]
fn every_admitted_cell_is_the_current_drawers_or_blank() {
    let mut total = Coverage::default();
    for seed in 0..SEEDS {
        match fuzz(seed, STEPS) {
            Ok(c) => {
                total.drawn += c.drawn;
                total.frontier += c.frontier;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(
        total.drawn >= DRAWN_FLOOR && total.frontier >= FRONTIER_FLOOR,
        "the fuzz exercised too little: {total:?}"
    );
}

fn card(i: u8) -> Vec<u8> {
    let row = own(i, 6);
    format!(
        "\x1b[48;5;{}m{row}\r\n{row}\r\n{row}\x1b[0m\r\n",
        Who::Drawer(i).bg()
    )
    .into_bytes()
}

fn card_rows(t: &Terminal, i: u8) -> (u64, u64) {
    let main = t.main_grid();
    let glyph = Who::Drawer(i).glyph();
    let rows: Vec<u64> = (0..main.rows())
        .filter(|&r| main.cell(r, 0).is_some_and(|c| c.char() == glyph))
        .map(|r| main.visible_to_absolute(r))
        .collect();
    (rows[0], rows[rows.len() - 1] + 1)
}

fn ctrl_z_vim_fg_keeps_the_card() -> Result<(), String> {
    let mut t = Terminal::new(24, 40);
    t.process(b"$ claude\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    let rows = card_rows(&t, 0);
    if !t.holder_view().admits_rows(rows.0, rows.1) {
        return Err("the fresh card is not admitted".into());
    }
    t.note_holder_cut(group(None, true));
    t.process(b"\r\n[1]+ Stopped claude\r\n$ vim notes\r\n");
    t.note_holder_cut(group(None, true));
    t.process(b"\x1b[?1049h\x1b[2Jtttt\x1b[5;1Htttt\x1b[?1049l");
    t.note_holder_cut(group(None, false));
    for n in 0..5 {
        t.process(format!("$ true {n}\r\nok\r\n").as_bytes());
        t.note_holder_cut(group(None, true));
        t.note_holder_cut(group(None, false));
    }
    t.process(b"$ fg\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    let view = t.holder_view();
    if view.admits_rows(rows.0, rows.1) {
        Ok(())
    } else {
        Err(format!("after fg the card {rows:?} is bare: {view:?}"))
    }
}

fn three_stopped_drawers_then_fg_keeps_the_first() -> Result<(), String> {
    let mut t = Terminal::new(30, 40);
    let mut first = (0, 0);
    for i in 0..3 {
        t.process(b"$ claude\r\n");
        t.note_holder_cut(group(Some(token(i)), true));
        t.process(&card(i));
        if i == 0 {
            first = card_rows(&t, 0);
        }
        t.note_holder_cut(group(None, true));
        t.process(b"\r\n[1]+ Stopped\r\n");
    }
    t.process(b"$ fg %1\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    let view = t.holder_view();
    if view.admits_rows(first.0, first.1) {
        Ok(())
    } else {
        Err(format!(
            "the first drawer's card {first:?} is bare: {view:?}"
        ))
    }
}

/// Muse Code 1.4.12's launch after its launcher's clear: four newlines, then
/// three reverse indexes at the top of a region (rows 1-7) that starts on
/// its span's start, and its banner in the second row it opened (its own
/// bytes are `aterm-gui`'s `fixtures/muse-1.4.12-inline-startup.bin`).
fn muse_launch_keeps_its_banner() -> Result<(), String> {
    let mut t = Terminal::new(24, 40);
    t.process(b"$ muse\r\n\x1b[3J\x1b[H\x1b[2J");
    t.note_holder_cut(HolderCut {
        to: Some(token(0)),
        kind: CutKind::Exec,
        from_alive: true,
    });
    t.process(b"\n\n\n\n\x1b[5;1H\x1b[2K\x1b7\x1b[1;7r\x1b[1;1H\x1bM\x1bM\x1bM\x1b[r\x1b8");
    t.process(b"\x1b[2;1H  Muse Code 1.4.12\x1b[5;1H> ");
    let banner = t.main_grid().visible_to_absolute(1);
    let view = t.holder_view();
    if view.admits_rows(banner, banner + 1) {
        Ok(())
    } else {
        Err(format!("Muse Code's banner on {banner} is bare: {view:?}"))
    }
}

/// Claude Code 2.1.304's inline redraw after a resize: under the shell's
/// command line and its launcher's line, it erases every row from home down
/// (`\e[2K\e[1B`, row by row) and draws its card again from the top (its
/// own bytes are `aterm-gui`'s
/// `tests/fixtures/claude-card/claude-2.1.304-inline-resize-redraw-120col.out`).
fn redraw_from_home_keeps_the_card() -> Result<(), String> {
    let mut t = Terminal::new(24, 40);
    t.process(b"$ claude\r\naterm: yolo (default): running 'claude'\r\n");
    t.note_holder_cut(HolderCut {
        to: Some(token(0)),
        kind: CutKind::Exec,
        from_alive: true,
    });
    t.process(&card(0));
    let mut sweep = b"\x1b[?2026h\x1b[H".to_vec();
    for _ in 0..24 {
        sweep.extend_from_slice(b"\x1b[2K\x1b[1B");
    }
    sweep.extend_from_slice(b"\x1b[H");
    t.process(&sweep);
    t.process(&card(0));
    t.process(b"\x1b[?2026l");
    let rows = card_rows(&t, 0);
    let top = t.main_grid().visible_to_absolute(0);
    let view = t.holder_view();
    if rows.0 == top && view.admits_rows(rows.0, rows.1) {
        Ok(())
    } else {
        Err(format!(
            "the card redrawn at {rows:?} (top {top}) is bare: {view:?}"
        ))
    }
}

fn liveness() -> Result<(), String> {
    ctrl_z_vim_fg_keeps_the_card()?;
    three_stopped_drawers_then_fg_keeps_the_first()?;
    muse_launch_keeps_its_banner()?;
    redraw_from_home_keeps_the_card()
}

#[test]
fn a_stop_a_tool_and_fg_keep_the_card() {
    ctrl_z_vim_fg_keeps_the_card().unwrap();
    three_stopped_drawers_then_fg_keeps_the_first().unwrap();
}

#[test]
fn rows_a_drawer_opens_at_its_own_start_are_its() {
    muse_launch_keeps_its_banner().unwrap();
    assert!(
        with_knob(FenceKnob::StartFollows, muse_launch_keeps_its_banner).is_err(),
        "CONTROL: a start that follows its row down leaves the banner above it"
    );
}

#[test]
fn rows_a_drawer_erases_above_its_start_are_its() {
    redraw_from_home_keeps_the_card().unwrap();
    assert!(
        with_knob(
            FenceKnob::ErasedRowsStayOut,
            redraw_from_home_keeps_the_card
        )
        .is_err(),
        "CONTROL: erased rows that stay out leave the redrawn card above the span"
    );
}

/// The erase law takes in only blank rows directly above the open span's start, on the current drawer's erase.
#[test]
fn an_erase_above_the_start_takes_in_blank_rows_only() {
    let abs = |t: &Terminal, r: u64| t.main_grid().visible_to_absolute(0) + r;
    let mut t = Terminal::new(24, 40);
    t.process(b"tttt\r\ntttt\r\ntttt\r\n\r\n\r\n$ claude\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    assert!(!t.holder_view().tainted);
    t.process(b"\x1b[6;1H\x1b[2K\x1b[7;1H");
    let view = t.holder_view();
    assert!(view.admits_rows(abs(&t, 3), abs(&t, 7)), "{view:?}");
    assert!(
        !view.admits_rows(abs(&t, 2), abs(&t, 3)),
        "the card above: {view:?}"
    );

    let mut t = Terminal::new(24, 40);
    t.process(b"\r\n\r\n$ claude\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(b"\x1b[3;3H\x1b[K\x1b[4;1H");
    let view = t.holder_view();
    assert!(
        !view.admits_rows(abs(&t, 2), abs(&t, 3)),
        "the shell's `$ ` is left: {view:?}"
    );
    t.process(b"\x1b[3;1H\x1b[K\x1b[4;1H");
    let view = t.holder_view();
    assert!(view.admits_rows(abs(&t, 0), abs(&t, 4)), "{view:?}");

    let mut t = Terminal::new(24, 40);
    t.process(b"\r\n\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    t.process(b"\x1b[3;1H\x1b[2K\x1b[4;1H\x1b[2K\x1b[6;1H");
    let view = t.holder_view();
    assert!(
        !view.admits_rows(abs(&t, 0), abs(&t, 2)),
        "an erase inside the span: {view:?}"
    );

    let mut t = Terminal::new(24, 40);
    t.process(b"$ claude\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    t.note_holder_cut(group(None, true));
    t.process(b"\x1b[1;1H\x1b[2K\x1b[5;1H$ fg\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    let view = t.holder_view();
    assert!(!view.tainted, "{view:?}");
    assert!(
        !view.admits_rows(abs(&t, 0), abs(&t, 1)),
        "the shell's erase above a stopped start: {view:?}"
    );
}

/// The erase law on an erase in display: ED 0 from a row above the open span's start, and ED 1,
/// take in the blank rows directly above it and the card the drawer draws in them; a row the
/// erase left inked stops the rise.
#[test]
fn an_erase_in_display_above_the_start_takes_in_blank_rows_only() {
    let abs = |t: &Terminal, r: u64| t.main_grid().visible_to_absolute(0) + r;
    let launch = |shell: &[u8]| {
        let mut t = Terminal::new(24, 40);
        t.process(shell);
        t.note_holder_cut(group(Some(token(0)), true));
        assert!(!t.holder_view().tainted);
        t
    };
    let ed0 = || {
        let mut t = launch(b"tttt\r\ntttttttt\r\n$ claude\r\n");
        t.process(b"\x1b[2;1H\x1b[J");
        t.process(&card(0));
        let view = t.holder_view();
        let whole = (
            view.admits_rows(abs(&t, 1), abs(&t, 4)),
            view.admits_rows(abs(&t, 0), abs(&t, 1)),
        );
        let mut t = launch(b"tttt\r\ntttttttt\r\n$ claude\r\n");
        t.process(b"\x1b[2;3H\x1b[J\x1b[3;1H");
        t.process(&card(0));
        let view = t.holder_view();
        let part = (
            view.admits_rows(abs(&t, 2), abs(&t, 5)),
            view.admits_rows(abs(&t, 1), abs(&t, 2)),
        );
        (whole, part)
    };
    assert_eq!(
        ed0(),
        ((true, false), (true, false)),
        "ED 0 from row 1, then from row 1's third column"
    );
    assert_eq!(
        with_knob(FenceKnob::ErasedRowsStayOut, ed0),
        ((false, false), (false, false)),
        "CONTROL: the erased rows stay out"
    );

    let ed1 = || {
        let mut t = launch(b"tttt\r\ntttttttt\r\n$ claude\r\n");
        t.process(&card(0));
        t.process(b"\x1b[5;1H\x1b[1J\x1b[H");
        t.process(&card(0));
        let whole = t.holder_view().admits_rows(abs(&t, 0), abs(&t, 3));
        let mut t = launch(b"tttt\r\ntttttttt\r\n\r\n");
        t.process(b"\x1b[2;4H\x1b[1J\x1b[3;1H");
        t.process(&card(0));
        let view = t.holder_view();
        let part = (
            view.admits_rows(abs(&t, 2), abs(&t, 5)),
            view.admits_rows(abs(&t, 1), abs(&t, 2)),
        );
        (whole, part)
    };
    assert_eq!(
        ed1(),
        (true, (true, false)),
        "ED 1 from inside the span, then from row 1's fourth column"
    );
    assert_eq!(
        with_knob(FenceKnob::ErasedRowsStayOut, ed1),
        (false, (false, false)),
        "CONTROL: the erased rows stay out"
    );
}

/// A row the shell opens at a stopped drawer's start joins that drawer's span (every start on
/// an insertion's top holds its row), and the reselect's seal check cuts it at `fg`.
#[test]
fn a_row_another_writer_opens_at_a_stopped_start_is_cut_at_fg() {
    let run = || {
        let mut t = Terminal::new(24, 40);
        t.process(b"\x1b[3J\x1b[H\x1b[2J");
        t.note_holder_cut(group(Some(token(0)), true));
        let start = t.main_grid().visible_to_absolute(0);
        t.process(&card(0));
        t.note_holder_cut(group(None, true));
        t.process(b"\x1b7\x1b[1;7r\x1b[1;1H\x1bM\x1b[r\x1b[1;1Hsss\x1b8\r\n");
        t.note_holder_cut(group(Some(token(0)), true));
        let view = t.holder_view();
        assert!(!view.tainted, "the lay below the card: {view:?}");
        view.admits_rows(start, start + 1)
    };
    assert!(!run(), "the shell's row at the stopped start is admitted");
    assert!(
        with_knob(FenceKnob::NoSealCheck, run),
        "CONTROL: with no seal check the shell's row stays in the stopped span"
    );
}

#[test]
fn every_knob_is_caught() {
    let knobs = [
        FenceKnob::NoBandTransform,
        FenceKnob::SpliceEndAsStart,
        FenceKnob::NoReflowLend,
        FenceKnob::UnlentRenumberKeeps,
        FenceKnob::SwapLoses,
        FenceKnob::NoSealCheck,
        FenceKnob::NoLayTaint,
        FenceKnob::LayTaintSkipsCursorRow,
        FenceKnob::ExecInherits,
        FenceKnob::CountEviction,
        FenceKnob::StopKeepsAltFresh,
    ];
    for knob in knobs {
        let caught = with_knob(knob, || (0..SEEDS).any(|seed| fuzz(seed, STEPS).is_err()));
        assert!(caught, "{knob:?}: every seed of the fuzz passed");
    }
}

#[test]
fn the_probes_catch_the_knobs_that_lose_too_much() {
    for knob in [
        FenceKnob::SwapLoses,
        FenceKnob::CountEviction,
        FenceKnob::StartFollows,
        FenceKnob::ErasedRowsStayOut,
    ] {
        assert!(with_knob(knob, || liveness().is_err()), "{knob:?}");
    }
}

#[test]
fn the_lay_row_is_the_cursor_row_at_column_0_and_the_next_one_inside_a_row() {
    let mut t = Terminal::new(10, 20);
    t.process(b"$ ");
    t.note_holder_cut(group(Some(token(0)), true));
    let view = t.holder_view();
    let row = t.grid().visible_to_absolute(0);
    assert_eq!(
        view.spans(),
        &[(row + 1, u64::MAX)],
        "column 2: the next row"
    );
    assert!(!view.tainted);

    let mut t = Terminal::new(10, 20);
    t.process(b"$ claude\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    let row = t.grid().visible_to_absolute(1);
    assert_eq!(
        t.holder_view().spans(),
        &[(row, u64::MAX)],
        "column 0: its own row"
    );
}

#[test]
fn the_lay_taints_on_ink_from_the_cursor_on_and_not_before_it() {
    let mut t = Terminal::new(10, 20);
    t.process(b"sss\r");
    t.note_holder_cut(group(Some(token(0)), true));
    assert!(t.holder_view().tainted, "the cursor row from its column");
    let mut t = Terminal::new(10, 20);
    t.process(b"\r\n\r\nsss\x1b[1;1H");
    t.note_holder_cut(group(Some(token(0)), true));
    assert!(t.holder_view().tainted, "a row below");
    let mut t = Terminal::new(10, 20);
    t.process(b"\x1b[5;1H\x1b[41m\x1b[K\x1b[0m\x1b[1;1H");
    t.note_holder_cut(group(Some(token(0)), true));
    assert!(t.holder_view().tainted, "a background");
    let mut t = Terminal::new(10, 20);
    t.process(b"sss");
    t.note_holder_cut(group(Some(token(0)), true));
    assert!(!t.holder_view().tainted, "left of the cursor");
}

#[test]
fn the_lay_taint_scan_includes_the_cursor_row_and_its_knob_skips_it() {
    let lay = || {
        let mut t = Terminal::new(10, 20);
        t.process(b"sss\r");
        t.note_holder_cut(group(Some(token(0)), true));
        t.holder_view().tainted
    };
    assert!(lay());
    assert!(!with_knob(FenceKnob::LayTaintSkipsCursorRow, lay));
    assert!(!with_knob(FenceKnob::NoLayTaint, lay));
}

#[test]
fn a_span_ending_at_the_archival_insertion_point_keeps_its_end() {
    let run = || {
        let mut t = Terminal::new(10, 20);
        t.process(b"x\r\n");
        t.note_holder_cut(group(Some(token(0)), true));
        t.process(b"AAA\r\nAAA\r\n");
        t.note_holder_cut(group(None, true));
        let end = t.grid().visible_to_absolute(3);
        t.process(b"\x1b[1;3r\x1b[3;1H\n\x1b[r");
        let view_fence = t.holder_fence().marks[0].spans[0];
        let end_now = t.grid().anchors().row(view_fence.end.unwrap()).unwrap();
        (end, end_now)
    };
    let (end, end_now) = run();
    assert_eq!(end_now, end, "End == at stays");
    let (end, end_now) = with_knob(FenceKnob::SpliceEndAsStart, run);
    assert_eq!(end_now, end + 1, "the knob shifts it");
}

#[test]
fn a_width_change_inside_a_row_excludes_the_shared_row_for_start_and_end() {
    let mut t = Terminal::new(8, 10);
    t.process(b"$ one\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(b"\x1b[48;5;170mAAAAAAAAAAAA\x1b[0m");
    let (start_id, start_row) = {
        let fence = t.holder_fence();
        let span = fence.marks[0].spans[0];
        (span.start, t.grid().anchors().row(span.start).unwrap())
    };
    t.note_holder_cut(group(None, true));
    let end_id = t.holder_fence().marks[0].spans[0].end.unwrap();
    assert_eq!(t.grid().anchors().row(end_id), Some(start_row + 1));
    t.grid_mut().anchors_mut().set_row(start_id, start_row + 1);
    t.resize(8, 20);
    let anchors = t.grid().anchors();
    assert!(!anchors.lost());
    let start = anchors.row(start_id).unwrap();
    let end = anchors.row(end_id).unwrap();
    assert_eq!(end, start_row, "End: the shared line's head");
    assert_eq!(start, start_row + 1, "Start: the next line's head");
}

#[test]
fn an_offloaded_width_change_clamps_a_start_in_detached_history() {
    let scrollback = Scrollback::new(16, 256, 1_000_000);
    let mut t = Terminal::with_scrollback(6, 20, 400, scrollback);
    t.process(b"$ claude\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    let start_row = t.grid().visible_to_absolute(1);
    for n in 0..300 {
        t.process(format!("A{n}\r\n").as_bytes());
    }
    let reach = t.grid().anchor_reach_for_offload(6, 12);
    let aterm_grid::LendReach::Rows(reach) = reach else {
        panic!("a tiered grid offloads");
    };
    let floor = t.grid().visible_to_absolute(0) - reach;
    assert!(
        start_row < floor,
        "precondition: the Start is in history the job takes"
    );
    let mut pending = t.resize_offloading_scrollback(6, 12);
    assert!(pending.is_some());
    while let Some(job) = pending {
        pending = t.finish_resize_offload(job.reflow());
    }
    let fence = t.holder_fence();
    let span = fence.marks[0].spans[0];
    let start = t.grid().anchors().row(span.start).unwrap();
    assert!(!t.grid().anchors().lost());
    assert!(
        start > start_row,
        "clamped up to the history rewrapped with the screen ({start} > {start_row})"
    );
    assert!(
        t.holder_view()
            .admits_rows(start, t.grid().visible_to_absolute(0))
    );
}

#[test]
fn a_background_row_handed_back_under_the_cursor_is_not_admitted() {
    let other = Who::Drawer(2);
    let mut t = term();
    t.process(format!("\x1b[6;1H\x1b[48;5;{}m\x1b[2K\x1b[0m\x1b[H", other.bg()).as_bytes());
    t.resize(5, COLS);
    assert!(t.main_grid().pushed_rows() > 0, "precondition: rows owed");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(format!("\x1b[48;5;170m{}\x1b[0m\r\n", own(0, 4)).as_bytes());
    assert_eq!(oracle(&t), Ok(1));
    t.resize(5, COLS + 4);
    let main = t.main_grid();
    let back = (0..main.rows()).any(|r| {
        main.cell(r, 0)
            .is_some_and(|c| c.colors().bg_is_indexed() && c.colors().bg_index() == other.bg())
    });
    assert!(back, "precondition: the width change handed the row back");
    assert_eq!(oracle(&t), Ok(0), "the span admits none of it");
    assert!(main.anchors().lost(), "the lend saw a drawn line come in");
}

#[test]
fn a_pushed_row_rotated_onto_the_cursors_line_is_not_admitted() {
    let other = Who::Drawer(1);
    for (rows, cols, offloaded) in [
        (9, COLS + 4, false),
        (9, COLS + 4, true),
        (6, COLS + 4, true),
    ] {
        let mut t = term();
        t.process(
            format!(
                "\x1b[9;1H{}\x1b[48;5;{}m \x1b[0m\x1b[H",
                " ".repeat(usize::from(COLS)),
                other.bg()
            )
            .as_bytes(),
        );
        t.resize(9, COLS);
        assert_eq!(t.main_grid().pushed_rows(), 1, "precondition: a row owed");
        t.process(b"\x1b[9;1H");
        t.note_holder_cut(group(Some(token(0)), true));
        assert!(!t.holder_view().tainted, "precondition: nothing drawn");
        if offloaded {
            let mut pending = t.resize_offloading_scrollback(rows, cols);
            while let Some(job) = pending {
                pending = t.finish_resize_offload(job.reflow());
            }
        } else {
            t.resize(rows, cols);
        }
        let main = t.main_grid();
        let back = (0..main.rows()).any(|r| {
            (0..main.cols()).any(|c| {
                main.cell(r, c).is_some_and(|cell| {
                    cell.colors().bg_is_indexed() && cell.colors().bg_index() == other.bg()
                })
            })
        });
        assert!(back, "precondition: the resize handed the row back");
        assert_eq!(
            oracle(&t),
            Ok(0),
            "{rows}x{cols}: the span admits none of it"
        );
        assert!(main.anchors().lost(), "the lend refused the owed rows");
    }
}

#[test]
fn the_lay_taints_on_every_kind_of_ink() {
    let tainted = |ink: &str| {
        let mut t = term();
        t.process(format!("\x1b[5;1H{ink}\x1b[0m\x1b[2;1H").as_bytes());
        t.note_holder_cut(group(Some(token(0)), true));
        t.holder_view().tainted
    };
    for (ink, why) in [
        ("\x1b[7m   ", "reverse video"),
        ("\x1b[4m   ", "an underline"),
        ("\x1b[4:3m   ", "a curly underline"),
        ("\x1b[9m   ", "a strikethrough"),
        ("\x1b[53m   ", "an overline"),
        (KITTY_IMAGE, "a kitty image"),
        (SIXEL_IMAGE, "a sixel image"),
        (ITERM_IMAGE, "an iTerm2 image"),
        ("\x1b[41m   ", "a background"),
        ("x", "a glyph"),
        ("\x1b#6x\x1b[2K", "an erased double-width row"),
        ("\x1b#3\x1b[2K", "an erased double-height row"),
    ] {
        assert!(tainted(ink), "{why} below the cursor taints the lay");
    }
    for (ink, why) in [
        ("\x1b[1m   ", "bold"),
        ("\x1b[3m   ", "italic"),
        ("\x1b[31m   ", "a foreground colour"),
        ("\x1b[8m   ", "hidden"),
    ] {
        assert!(!tainted(ink), "{why} spaces show nothing");
    }
}

const KITTY_IMAGE: &str = "\x1b_Ga=T,f=24,s=1,v=1,c=4,r=2,C=1;AAAA\x1b\\";
const SIXEL_IMAGE: &str = "\x1bPq#0;2;100;0;0#0~~~~~~~~-~~~~~~~~-~~~~~~~~\x1b\\";
const ITERM_IMAGE: &str = "\x1b]1337;File=inline=1;width=4;height=2;preserveAspectRatio=0:\
    iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==\x07";

#[test]
fn an_image_placed_over_a_stopped_card_is_cut_at_fg() {
    let mut t = term();
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(
        format!(
            "\x1b]8;;https://example.com\x07{0}\x1b]8;;\x07\r\n{0}\r\n",
            own(0, 6)
        )
        .as_bytes(),
    );
    let card = card_rows(&t, 0);
    t.note_holder_cut(group(None, true));
    let row = card.0 - t.main_grid().visible_to_absolute(0);
    t.process(format!("\x1b7\x1b[{};1H{KITTY_IMAGE}\x1b8", row + 1).as_bytes());
    assert!(
        !t.images_row(usize::try_from(row).unwrap()).is_empty(),
        "precondition: the image covers the card's first row"
    );
    t.note_holder_cut(group(Some(token(0)), true));
    let view = t.holder_view();
    assert!(!view.admits_rows(card.0, card.0 + 1), "{view:?}");
    assert_eq!(oracle(&t), Ok(0));
}

#[test]
fn a_revealed_history_row_rewritten_while_stopped_is_cut_at_fg() {
    let mut t = term();
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    let card = card_rows(&t, 0);
    t.process(
        format!("{}\r\n", own(0, 6))
            .repeat(usize::from(ROWS))
            .as_bytes(),
    );
    assert!(
        t.main_grid().visible_to_absolute(0) >= card.1,
        "precondition: the card is history at the stop"
    );
    t.note_holder_cut(group(None, true));
    t.resize(ROWS + 8, COLS);
    let base = t.main_grid().visible_to_absolute(0);
    assert!(
        base <= card.0,
        "precondition: the card is on the screen again"
    );
    let row = card.0 - base;
    t.process(format!("\x1b7\x1b[{};1Hssssss\x1b8", row + 1).as_bytes());
    t.note_holder_cut(group(Some(token(0)), true));
    let view = t.holder_view();
    assert!(!view.admits_rows(card.0, card.0 + 1), "{view:?}");
    assert_eq!(oracle(&t).map(|_| ()), Ok(()));
}

#[test]
fn a_range_one_span_does_not_hold_is_not_admitted() {
    let mut t = Terminal::new(30, 20);
    t.note_holder_cut(group(Some(token(1)), true));
    t.process(&card(1));
    let other = card_rows(&t, 1);
    t.note_holder_cut(group(None, false));
    t.process(b"$ claude\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    t.note_holder_cut(group(None, true));
    t.process(b"$ ls\r\nsss\r\n$ fg\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(b"AAAA\r\n");
    let view = t.holder_view();
    let [first, open] = view.spans() else {
        panic!("precondition: two spans, {view:?}");
    };
    assert!(first.1 < open.0, "precondition: a gap between them");
    assert!(view.admits_rows(first.0, first.1), "a whole span");
    assert!(view.admits_rows(open.0, open.0 + 1));
    for (lo, hi, why) in [
        (other.0, first.1, "from another drawer's card into the span"),
        (first.0 - 1, first.0 + 1, "across the span's start"),
        (first.0, first.1 + 1, "past the span's end"),
        (first.0, open.0 + 1, "across the gap between two spans"),
    ] {
        assert!(!view.admits_rows(lo, hi), "{why}: [{lo}, {hi}) {view:?}");
    }
}

#[test]
fn a_tail_replayed_after_the_keyframe_does_not_continue_the_carried_mark() {
    let mut t = term();
    t.process(b"$ claude\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    let keyframe = t.checkpoint();
    let tail = [b"\x1b[H\x1b[2J".as_slice(), &card(1)].concat();
    t.note_holder_cut(group(None, true));
    t.process(&tail);
    t.note_holder_cut(group(Some(token(0)), true));
    let cat = card_rows(&t, 1);
    assert!(!t.holder_view().admits_rows(cat.0, cat.1), "live: cut");
    let mut fresh = Terminal::from_checkpoint(&keyframe);
    let mut in_place = term();
    in_place.restore_checkpoint(&keyframe);
    for restored in [&mut fresh, &mut in_place] {
        restored.process(&tail);
        assert_ne!(
            restored.adopt_holder_after_restore(token(0), true),
            HolderAdoption::Continued
        );
        let view = restored.holder_view();
        assert!(!view.admits_rows(cat.0, cat.1), "restored: {view:?}");
        assert_eq!(oracle(restored).map(|_| ()), Ok(()));
    }
    let mut restored = Terminal::from_checkpoint(&keyframe);
    assert_eq!(
        restored.adopt_holder_after_restore(token(0), true),
        HolderAdoption::Continued
    );
    let own = card_rows(&restored, 0);
    assert!(restored.holder_view().admits_rows(own.0, own.1));
    let mut again = Terminal::from_checkpoint(&keyframe);
    again.process(b"");
    assert_eq!(
        again.adopt_holder_after_restore(token(0), true),
        HolderAdoption::Continued,
        "an empty read is no byte"
    );

    let (signed, above, under) = session_with_card_under_c();
    let mut signed = Terminal::from_checkpoint(&signed.checkpoint());
    assert_eq!(
        signed.adopt_holder_after_restore(token(1), true),
        HolderAdoption::Seeded
    );
    assert!(signed.holder_view().admits_rows(under.0, under.1));
    signed.note_holder_cut(group(None, true));
    signed.note_holder_cut(group(Some(token(2)), true));
    signed.process(&card(2));
    signed.note_holder_cut(group(None, true));
    signed.note_holder_cut(group(Some(token(1)), true));
    let keyframe = signed.checkpoint();
    let stopped = keyframe
        .holder
        .marks
        .iter()
        .find(|m| m.token == token(2))
        .unwrap()
        .clone();
    let old_id = super::super::ShellIntegrationNonce([7u8; 32]).to_hex();
    let top = signed.main_grid().visible_to_absolute(0);
    let row = under.0 - top;
    let erase_above = (above.0..above.1)
        .map(|absolute| format!("\x1b[{};1H\x1b[2K", absolute - top + 1))
        .collect::<String>();
    let tail = format!(
        "\x1b]133;D;0;id={old_id}\x07\x1b]133;A;id={old_id}\x07\
         \x1b]133;B;id={old_id}\x07cat card\r\n\x1b]133;C;id={old_id}\x07\
         {erase_above}\x1b[{};1H{}",
        row + 1,
        String::from_utf8(card(0)).unwrap()
    );
    for recapture in [false, true] {
        let mut replayed = Terminal::from_checkpoint(&keyframe);
        replayed.authorize_shell_integration([9u8; 32]);
        assert!(replayed.shell.current_block.as_ref().is_some_and(|b| {
            b.state == super::super::BlockState::Executing && b.output_start_row == Some(under.0)
        }));
        let started = replayed.started_command_seq();
        replayed.process(tail.as_bytes());
        assert_eq!(
            replayed.started_command_seq(),
            started,
            "the old C was rejected"
        );
        assert_eq!(
            card_rows(&replayed, 0),
            under,
            "the foreign card occupies the old seed range"
        );
        assert!(
            replayed.shell.current_block.as_ref().is_some_and(|b| {
                b.state == super::super::BlockState::Executing
                    && b.output_start_row == Some(under.0)
            }),
            "precondition: the old signed block survived the new-nonce replay"
        );
        let after = replayed.checkpoint();
        assert!(after.holder.seed_untrusted);
        let mut forgotten = replayed.checkpoint();
        forgotten.holder.seed_untrusted = false;
        let mut unguarded = Terminal::from_checkpoint(&forgotten);
        assert_eq!(
            unguarded.adopt_holder_after_restore(token(0), true),
            HolderAdoption::Seeded
        );
        assert!(unguarded.holder_view().admits_rows(under.0, under.1));
        assert_eq!(
            after.holder.marks.iter().find(|m| m.token == token(2)),
            Some(&stopped)
        );
        let mut adopted = if recapture {
            Terminal::from_checkpoint(&after)
        } else {
            replayed
        };
        assert_eq!(
            adopted.adopt_holder_after_restore(token(1), true),
            HolderAdoption::Bare
        );
        assert!(!adopted.holder_view().admits_rows(under.0, under.1));
        assert!(adopted.holder_fence().tokens().any(|t| t == token(2)));
        assert_eq!(
            adopted.adopt_holder_after_restore(token(1), true),
            HolderAdoption::Bare,
            "a second adoption cannot resurrect the old C"
        );
        let mut again = Terminal::from_checkpoint(&adopted.checkpoint());
        assert_eq!(
            again.adopt_holder_after_restore(token(0), true),
            HolderAdoption::Bare,
            "neither another token nor another checkpoint restores seed authority"
        );
        adopted.process(b"\x1b[H\x1b[2J");
        adopted.process(&card(1));
        let owned = card_rows(&adopted, 1);
        let mut restored = Terminal::from_checkpoint(&adopted.checkpoint());
        assert_eq!(
            restored.adopt_holder_after_restore(token(1), true),
            HolderAdoption::Continued
        );
        assert!(restored.holder_view().admits_rows(owned.0, owned.1));
    }
    let (legacy, _, _) = session_with_card_under_c();
    let mut replayed = Terminal::from_checkpoint(&legacy.checkpoint());
    replayed.authorize_shell_integration([9u8; 32]);
    replayed.process(tail.as_bytes());
    let after = replayed.checkpoint();
    assert!(after.holder.marks.is_empty() && after.holder.seed_untrusted);
    assert!(
        !after.holder.is_empty(),
        "the wire must keep the uncertainty bit"
    );
    let mut restored = Terminal::from_checkpoint(&after);
    assert_eq!(
        restored.adopt_holder_after_restore(token(1), true),
        HolderAdoption::Bare
    );
}

#[test]
fn a_restore_seed_needs_a_running_block() {
    let (mut t, _, under) = session_with_card_under_c();
    let id = super::super::ShellIntegrationNonce([7u8; 32]).to_hex();
    t.process(format!("\x1b]133;D;0;id={id}\x07").as_bytes());
    let mut adopted = Terminal::from_checkpoint(&t.checkpoint());
    assert_eq!(
        adopted.adopt_holder_after_restore(token(1), true),
        HolderAdoption::Bare
    );
    assert!(!adopted.holder_view().admits_rows(under.0, under.1));
}

#[test]
fn a_new_drawer_at_full_anchor_capacity_gets_its_span() {
    let mut t = Terminal::new(150, 20);
    for i in 0..8u8 {
        t.note_holder_cut(group(Some(token(i)), true));
        t.process(b"AAAA\r\n");
        t.note_holder_cut(group(None, true));
    }
    for _ in 0..3 {
        for i in 0..8u8 {
            t.note_holder_cut(group(Some(token(i)), true));
            t.process(b"AAAA\r\n");
            t.note_holder_cut(group(None, true));
            t.process(b"$\r\n");
        }
    }
    assert_eq!(t.holder_fence().marks.len(), super::MAX_HOLDER_MARKS);
    assert_eq!(
        t.grid().anchors().live_count(),
        aterm_grid::MAX_ANCHORS,
        "precondition: every anchor is held"
    );
    t.note_holder_cut(group(Some(token(9)), true));
    let view = t.holder_view();
    assert_eq!(view.token, token(9));
    assert_eq!(view.n, 1, "the new drawer has its span: {view:?}");
    assert!(
        !t.holder_fence().tokens().any(|k| k == token(0)),
        "the oldest went"
    );
}

#[test]
fn once_every_drawer_is_gone_a_checkpoint_carries_nothing() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(b"AAAA\r\n");
    t.note_holder_cut(group(None, true));
    t.note_holder_cut(group(Some(token(1)), true));
    t.process(b"BBBB\r\n");
    t.note_holder_cut(group(None, false));
    t.retire_holder_mark(token(0));
    assert!(t.holder_fence().is_empty());
    assert_eq!(t.holder_fence(), &super::HolderFence::default());
    assert!(
        t.grid().anchors().slots().is_empty(),
        "the anchor slots were released"
    );
    assert!(t.checkpoint().holder.is_empty());
}

#[test]
fn a_full_clear_raises_the_open_span_and_untaints() {
    let mut t = Terminal::new(10, 20);
    t.process(b"\x1b[5;1Hsss\x1b[1;1H");
    t.note_holder_cut(group(Some(token(0)), true));
    assert!(t.holder_view().tainted);
    t.process(b"\x1b[2J");
    let view = t.holder_view();
    let top = t.grid().visible_to_absolute(0);
    assert!(!view.tainted, "a full clear re-anchors a tainted mark");
    assert_eq!(view.spans(), &[(top, u64::MAX)]);

    let mut t = Terminal::new(10, 20);
    t.process(b"x\r\nx\r\nx\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    let top = t.grid().visible_to_absolute(0);
    assert_eq!(t.holder_view().spans()[0].0, top + 3);
    t.process(b"\x1b[H\x1b[J");
    assert_eq!(
        t.holder_view().spans(),
        &[(top, u64::MAX)],
        "ED 0 from home"
    );
}

#[test]
fn a_full_clear_while_lost_re_anchors() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(b"A\r\nA\r\nA\r\nA\r\nA\r\nA\r\nA\r\nA\r\nA\r\nA\r\nA\r\nA\r\n\x1b[2+T");
    assert!(t.holder_view().tainted);
    t.process(b"\x1b[2J");
    assert!(!t.holder_view().tainted);
}

#[test]
fn only_a_1049_set_or_an_alt_clear_marks_the_alt_screen_fresh() {
    let fresh = |seq: &[u8]| {
        let mut t = Terminal::new(10, 20);
        t.note_holder_cut(group(Some(token(0)), true));
        t.process(seq);
        t.holder_view().alt_fresh
    };
    assert!(fresh(b"\x1b[?1049h"), "1049 set");
    assert!(
        fresh(b"\x1b[?1049h\x1b[?1049h"),
        "a repeated 1049 set (it clears)"
    );
    assert!(fresh(b"\x1b[?47h\x1b[?1049h"), "1049 over 47 clears");
    assert!(!fresh(b"\x1b[?47h"), "47 does not clear");
    assert!(!fresh(b"\x1b[?1047h"), "1047 does not clear");
    assert!(fresh(b"\x1b[?47h\x1b[2J"), "an ED 2 on the alt screen");
    assert!(!fresh(b"\x1b[?1049h\x1b[?1049l"), "the exit clears it");
    assert!(!fresh(b"\x1b[?1047h\x1b[2J\x1b[?1047l"), "a 1047 exit too");
    assert!(!fresh(b"\x1b[?1049h\x1bc"), "and RIS");
    let mut t = Terminal::new(10, 20);
    t.process(b"\x1b[?1049h");
    assert!(!t.holder_view().admits_alt());
}

#[test]
fn a_stop_clears_alt_fresh_and_fg_needs_a_new_clear() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(b"\x1b[?1049h");
    assert!(t.holder_view().admits_alt());
    t.note_holder_cut(group(None, true));
    t.process(b"ttt");
    t.note_holder_cut(group(Some(token(0)), true));
    assert!(
        !t.holder_view().admits_alt(),
        "the shell wrote on it meanwhile"
    );
    t.process(b"\x1b[?1049h");
    assert!(t.holder_view().admits_alt());
}

#[test]
fn an_exec_drops_the_mark_and_a_new_image_lays_its_own() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    t.note_holder_cut(HolderCut {
        to: Some(token(1)),
        kind: CutKind::Exec,
        from_alive: true,
    });
    let view = t.holder_view();
    assert_eq!(view.token, token(1));
    let top = t.grid().visible_to_absolute(0);
    assert!(
        !view.admits_rows(top, top + 1),
        "the old image's card is not the new one's"
    );
    assert_eq!(t.holder_fence().marks.len(), 1);

    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    let top = t.grid().visible_to_absolute(0);
    let original = t.holder_view();
    assert!(original.admits_rows(top, top + 1));
    t.note_holder_cut(group(Some(token(0)), true));
    assert_eq!(
        t.holder_view(),
        original,
        "a repeated group observation keeps the mark"
    );
    t.note_holder_cut(HolderCut {
        to: Some(token(0)),
        kind: CutKind::Exec,
        from_alive: true,
    });
    let next = t.holder_view();
    assert_eq!(next.token, token(0));
    assert!(
        !next.admits_rows(top, top + 1),
        "an explicit exec cannot inherit the old image's card"
    );
    assert_eq!(t.holder_fence().marks.len(), 1);
}

#[test]
fn a_row_keeps_its_fingerprint_when_it_scrolls_into_the_ring() {
    let mut t = term();
    t.process(
        "\x1b[38;2;200;100;50;48;2;10;20;30me\u{301}x \u{1F44D}\u{1F3FD} \u{1F600}\u{4E2D}\
         \x1b]8;;https://example.com\x07link\x1b]8;;\x07\x1b[0m\r\n"
            .as_bytes(),
    );
    t.process(b"plain\r\n");
    let main = t.main_grid();
    let rich = main.visible_to_absolute(0);
    let on_screen = main.row_fingerprint_at_absolute(rich);
    let plain = main.row_fingerprint_at_absolute(rich + 1);
    assert!(on_screen.is_some() && on_screen != plain);
    t.process(&b"\r\n".repeat(usize::from(ROWS) + 4));
    let main = t.main_grid();
    assert!(
        main.visible_to_absolute(0) > rich + 1,
        "both rows are history"
    );
    assert_eq!(main.row_fingerprint_at_absolute(rich), on_screen);
    assert_eq!(main.row_fingerprint_at_absolute(rich + 1), plain);
    t.process(&b"\r\n".repeat(64));
    assert_eq!(t.main_grid().row_fingerprint_at_absolute(rich), None);
}

#[test]
fn a_fingerprint_reads_a_true_colour_wherever_the_screen_keeps_it() {
    let mut t = term();
    t.process(b"\x1b[38;2;1;2;3mab\x1b[0m\r");
    let before = t.main_grid().live_row_fingerprint(0);
    t.process(b"\x1b[38;2;1;2;4mab\x1b[0m\r");
    assert_ne!(t.main_grid().live_row_fingerprint(0), before);
    t.process(b"\x1b[38;2;1;2;3mab\x1b[0m\r");
    assert_eq!(t.main_grid().live_row_fingerprint(0), before);
}

#[test]
fn a_stopped_drawers_card_that_scrolled_into_the_ring_survives_fg() {
    let after = |lines: usize| {
        let mut t = term();
        t.note_holder_cut(group(Some(token(0)), true));
        t.process(&card(0));
        let rows = card_rows(&t, 0);
        t.note_holder_cut(group(None, true));
        t.process(&b"ssssss\r\n".repeat(lines));
        assert!(
            t.main_grid().visible_to_absolute(0) >= rows.1,
            "the card is history"
        );
        t.note_holder_cut(group(Some(token(0)), true));
        t.holder_view().admits_rows(rows.0, rows.1)
    };
    assert!(after(usize::from(ROWS) + 2), "the ring vouches for it");
    assert!(!after(usize::from(ROWS) + 64));
}

#[test]
fn a_stopped_drawer_whose_rows_were_rewritten_loses_them_at_fg() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    let rows = card_rows(&t, 0);
    t.note_holder_cut(group(None, true));
    t.process(b"\x1b[H\x1b[2Jssssss\r\nssssss\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    let view = t.holder_view();
    assert!(!view.admits_rows(rows.0, rows.0 + 1), "{view:?}");
    let knob = with_knob(FenceKnob::NoSealCheck, || {
        let mut t = Terminal::new(10, 20);
        t.note_holder_cut(group(Some(token(0)), true));
        t.process(&card(0));
        t.note_holder_cut(group(None, true));
        t.process(b"\x1b[H\x1b[2Jssssss\r\nssssss\r\n");
        t.note_holder_cut(group(Some(token(0)), true));
        t.holder_view().admits_rows(rows.0, rows.0 + 1)
    });
    assert!(knob, "without the seal check the shell's rows are admitted");
}

#[test]
fn past_four_spans_the_launch_span_and_the_newest_stay() {
    let mut t = Terminal::new(30, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    let launch = t.holder_view().spans()[0].0;
    for _ in 0..5 {
        t.process(b"AAA\r\n");
        t.note_holder_cut(group(None, true));
        t.process(b"sss\r\n");
        t.note_holder_cut(group(Some(token(0)), true));
    }
    let view = t.holder_view();
    assert_eq!(view.n, 4);
    assert_eq!(view.spans()[0].0, launch, "span 0 holds the card");
    assert_eq!(view.spans()[3].1, u64::MAX, "the newest is open");
    assert_eq!(t.holder_fence().marks[0].spans.len(), 4);
}

#[test]
fn past_eight_marks_the_oldest_suspended_goes() {
    let mut t = Terminal::new(10, 20);
    for i in 0..9u8 {
        t.note_holder_cut(group(Some(token(i)), true));
        t.note_holder_cut(group(None, true));
    }
    let tokens: Vec<u64> = t.holder_fence().tokens().collect();
    assert_eq!(tokens.len(), 8);
    assert!(!tokens.contains(&token(0)), "the oldest stopped one went");
    assert!(tokens.contains(&token(8)));
}

#[test]
fn a_retired_mark_frees_its_anchors() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(b"AAA\r\n");
    t.note_holder_cut(group(None, true));
    assert_eq!(t.grid().anchors().live_count(), 2);
    t.retire_holder_mark(token(0));
    assert!(t.holder_fence().is_empty());
    assert!(t.grid().anchors().is_empty());
}

#[test]
fn no_drawer_costs_nothing() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(None, true));
    t.process(b"sss\r\n\x1b[2J\x1b[?1049hxx\x1b[?1049l");
    t.resize(10, 30);
    assert!(t.holder_fence().is_empty());
    assert!(
        t.grid().anchors().slots().is_empty(),
        "no anchor slot was ever allocated"
    );
    assert_eq!(t.holder_view(), HolderView::NONE);
    assert!(t.checkpoint().holder.is_empty());
}

#[test]
fn the_holder_round_trips_through_a_checkpoint_exactly() {
    let mut t = Terminal::new(10, 20);
    t.process(b"$ claude\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    t.note_holder_cut(group(None, true));
    t.process(b"$ claude\r\n");
    t.note_holder_cut(group(Some(token(1)), true));
    t.process(b"\x1b[?1049hBBB");
    let c = t.checkpoint();
    assert!(!c.holder.is_empty());
    let restored = Terminal::from_checkpoint(&c);
    assert_eq!(restored.checkpoint(), c, "the re-checkpoint identity");
    assert_eq!(restored.holder_fence(), t.holder_fence());
    assert_eq!(restored.holder_view(), t.holder_view());
    assert_eq!(
        restored.main_grid().anchors(),
        t.main_grid().anchors(),
        "the anchors"
    );
    let mut adopted = Terminal::new(10, 20);
    adopted.restore_checkpoint(&c);
    assert_eq!(adopted.holder_view(), t.holder_view());
}

#[test]
fn a_fence_that_does_not_install_whole_installs_as_none() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    let mut c = t.checkpoint();
    c.holder.anchors[0].end = true;
    let restored = Terminal::from_checkpoint(&c);
    assert!(restored.holder_fence().is_empty());
    assert!(restored.grid().anchors().is_empty());
    assert_eq!(restored.holder_view(), HolderView::NONE);
}

#[test]
fn a_restored_grid_numbered_otherwise_taints_the_carried_marks() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    let mut c = t.checkpoint();
    c.absolute_row_counter = 3;
    let restored = Terminal::from_checkpoint(&c);
    assert!(restored.holder_view().tainted);
}

fn session_with_card_under_c() -> (Terminal, (u64, u64), (u64, u64)) {
    let nonce = [7u8; 32];
    let mut t = Terminal::new(20, 40);
    t.authorize_shell_integration(nonce);
    t.set_require_shell_integration_nonce(true);
    let id = super::super::ShellIntegrationNonce(nonce).to_hex();
    t.process(
        format!(
            "\x1b]133;A;id={id}\x07$ \x1b]133;B;id={id}\x07cat card.txt\r\n\x1b]133;C;id={id}\x07"
        )
        .as_bytes(),
    );
    t.process(&card(0));
    let above = card_rows(&t, 0);
    t.process(format!("\x1b]133;D;0;id={id}\x07").as_bytes());
    t.process(
        format!("\x1b]133;A;id={id}\x07$ \x1b]133;B;id={id}\x07claude\r\n\x1b]133;C;id={id}\x07")
            .as_bytes(),
    );
    t.process(&card(1));
    let under = card_rows(&t, 1);
    (t, above, under)
}

#[test]
fn a_restore_without_a_fence_seeds_from_the_signed_output_start() {
    let (t, above, under) = session_with_card_under_c();
    let c = t.checkpoint();
    assert!(c.holder.is_empty());
    let mut adopted = Terminal::from_checkpoint(&c);
    assert_eq!(
        adopted.adopt_holder_after_restore(token(1), true),
        HolderAdoption::Seeded
    );
    let view = adopted.holder_view();
    assert!(
        view.admits_rows(under.0, under.1),
        "the card under C: {view:?}"
    );
    assert!(
        !view.admits_rows(above.0, above.1),
        "the card above C stays bare"
    );
}

#[test]
fn a_restore_continues_a_carried_current_mark_and_never_seeds_unsigned() {
    let (mut t, _, under) = session_with_card_under_c();
    t.note_holder_cut(group(Some(token(1)), true));
    let c = t.checkpoint();
    let mut adopted = Terminal::from_checkpoint(&c);
    assert_eq!(
        adopted.adopt_holder_after_restore(token(1), true),
        HolderAdoption::Continued
    );
    let mut refused = Terminal::from_checkpoint(&c);
    assert_eq!(
        refused.adopt_holder_after_restore(token(1), false),
        HolderAdoption::Bare
    );
    assert_eq!(
        refused.holder_view(),
        HolderView::NONE,
        "matching a carried token does not make an Exact drawer"
    );
    let mut t = Terminal::new(20, 40);
    t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]133;C\x07");
    t.process(&card(1));
    let mut adopted = Terminal::from_checkpoint(&t.checkpoint());
    assert_eq!(
        adopted.adopt_holder_after_restore(token(1), true),
        HolderAdoption::Bare
    );
    assert!(!adopted.holder_view().admits_rows(under.0, under.1));
    adopted.process(b"\x1b[2J");
    assert!(
        !adopted.holder_view().tainted,
        "its own full clear recovers it"
    );
    let mut adopted = Terminal::from_checkpoint(&c);
    assert_eq!(
        adopted.adopt_holder_after_restore(token(2), false),
        HolderAdoption::Bare
    );
    assert_eq!(adopted.holder_view(), HolderView::NONE);
}

#[test]
fn a_seed_on_the_alternate_screen_with_the_command_running_is_alt_fresh() {
    let (mut t, _, _) = session_with_card_under_c();
    t.process(b"\x1b[?1049hBBB");
    let mut adopted = Terminal::from_checkpoint(&t.checkpoint());
    assert_eq!(
        adopted.adopt_holder_after_restore(token(1), true),
        HolderAdoption::Seeded
    );
    assert!(adopted.holder_view().admits_alt());
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The holder fence against a writer oracle, and §7's unit cases
//! (docs/DESIGN-card-engine-2026-10-19.md).
//!
//! The oracle is black-box: every writer draws only its own glyph on its own
//! background, so a cell names who wrote it. After every step each main row
//! [`HolderView`] admits must hold only the current drawer's glyph or a
//! blank on the default or its background, and an admitted alternate screen
//! likewise. The archival scroll's splice law is checked beside it, since a
//! seal makes an End that wrongly absorbs inserted rows unobservable to the
//! oracle. Liveness probes say that what a stop, a full-screen tool and an
//! `fg` should keep, the fence keeps, and no resize loses the anchors while
//! the main cursor stands at the frontier (nothing drawn below its row).
//!
//! Each seed draws a noise level: that share of writes are hostile (screen
//! writes left where they ended, rectangle copies, margined edits, RIS,
//! unscrolls), the rest a line-oriented program's, and a calm cut finds the
//! cursor below what was drawn, as a shell's prompt is. Without the calm
//! share nearly every mark taints and the oracle has nothing to check; the
//! positive run counts the admitted rows it checked and holds a floor.
//!
//! Each law has a knob ([`FenceKnob`]) that switches it off; the negative
//! controls show the fuzz failing under every one, and the probes under
//! the two that only lose too much.

use aterm_grid::{AnchorRole, FenceKnob, with_knob};

use super::super::{CutKind, HolderAdoption, HolderCut, HolderView, Terminal};
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
    fn glyph(self) -> u16 {
        match self {
            Self::Shell => u16::from(b's'),
            Self::Tool => u16::from(b't'),
            Self::Drawer(i) => u16::from(b'A' + i),
        }
    }

    fn bg(self) -> u8 {
        match self {
            Self::Shell => 160,
            Self::Tool => 161,
            Self::Drawer(i) => 170 + i,
        }
    }
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

/// SplitMix64: deterministic per seed.
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
}

fn term() -> Terminal {
    let scrollback = Scrollback::new(16, 256, 1_000_000);
    Terminal::with_scrollback(ROWS, COLS, 24, scrollback)
}

/// Whether `cell` is blank or `who`'s.
fn is_theirs(cell: &crate::grid::Cell, who: Who) -> bool {
    let c = cell.char_data();
    let glyph_ok = c == u16::from(b' ') || c == 0 || c == who.glyph();
    let colors = cell.colors();
    let bg_ok = colors.bg_is_default() || (colors.bg_is_indexed() && colors.bg_index() == who.bg());
    glyph_ok && bg_ok
}

/// The oracle: every admitted cell is the current drawer's or blank.
/// `Ok` counts the admitted main rows holding the drawer's glyph (the
/// fuzz's measure of what it exercised).
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
    let mut drawn = 0;
    for r in 0..main.rows() {
        let abs = main.visible_to_absolute(r);
        if !view.admits_rows(abs, abs + 1) {
            continue;
        }
        if (0..main.cols()).any(|c| {
            main.cell(r, c)
                .is_some_and(|cell| cell.char_data() == who.glyph())
        }) {
            drawn += 1;
        }
        for c in 0..main.cols() {
            if let Some(cell) = main.cell(r, c)
                && !is_theirs(cell, who)
            {
                return Err(format!(
                    "main row {r} (abs {abs}) col {c} holds {:?} bg {:?} under {who:?}: {view:?}",
                    char::from_u32(u32::from(cell.char_data())),
                    cell.colors(),
                ));
            }
        }
    }
    if t.modes().alternate_screen && view.admits_alt() {
        let alt = t.grid();
        for r in 0..alt.rows() {
            for c in 0..alt.cols() {
                if let Some(cell) = alt.cell(r, c)
                    && !is_theirs(cell, who)
                {
                    return Err(format!("alt row {r} col {c} is not {who:?}'s"));
                }
            }
        }
    }
    Ok(drawn)
}

/// The process world around the terminal.
struct Sim {
    t: Terminal,
    rng: Rng,
    fg: Who,
    /// The tool's parent, while a tool is foreground.
    parent: Option<Who>,
    stopped: Vec<u8>,
    alive: [bool; DRAWERS as usize],
    /// The seed's share of hostile writes, in percent: screen-addressed
    /// writes left where they ended, rectangle copies, margined edits, RIS,
    /// unscrolls. The rest are a line-oriented program's.
    noise: u64,
    log: Vec<String>,
}

impl Sim {
    fn new(seed: u64) -> Self {
        let mut rng = Rng(seed);
        let noise = [3, 12, 40][usize::try_from(rng.below(3)).unwrap_or(0)];
        Self {
            t: term(),
            rng,
            noise,
            fg: Who::Shell,
            parent: None,
            stopped: Vec::new(),
            alive: [false; DRAWERS as usize],
            log: Vec::new(),
        }
    }

    fn cut(&mut self, cut: HolderCut) {
        self.log.push(format!("cut {cut:?}"));
        self.t.note_holder_cut(cut);
    }

    /// `bytes` from the foreground, after its own pen: the SGR state is the
    /// terminal's, and every blank a scroll or an erase makes takes its
    /// background, so a writer that did not set it would draw in the last
    /// writer's.
    fn write(&mut self, bytes: &[u8]) {
        let mut out = self.pen();
        out.extend_from_slice(bytes);
        self.log.push(format!(
            "{:?} writes {:?}",
            self.fg,
            String::from_utf8_lossy(&out)
        ));
        self.t.process(&out);
    }

    fn pen(&mut self) -> Vec<u8> {
        if self.rng.chance(25) {
            b"\x1b[0m".to_vec()
        } else {
            format!("\x1b[0;48;5;{}m", self.fg.bg()).into_bytes()
        }
    }

    fn glyphs(&mut self, n: u64) -> Vec<u8> {
        let g = u8::try_from(self.fg.glyph()).expect("an ASCII glyph");
        let mut out = self.pen();
        out.extend(std::iter::repeat_n(g, usize::try_from(n).unwrap_or(0)));
        out
    }

    fn dead_drawer(&mut self) -> Option<u8> {
        let dead: Vec<u8> = (0..DRAWERS)
            .filter(|&i| !self.alive[usize::from(i)])
            .collect();
        (!dead.is_empty())
            .then(|| dead[usize::try_from(self.rng.below(dead.len() as u64)).unwrap_or(0)])
    }

    /// The current holder's open span's first screen row, if on screen.
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

    /// A screen row inside a suspended mark's spans, if one is on screen
    /// (read to aim the generator, never by the oracle).
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
        (!rows.is_empty())
            .then(|| rows[usize::try_from(self.rng.below(rows.len() as u64)).unwrap_or(0)])
    }

    /// Whether this step's writer behaves (the seed's noise decides).
    fn calm(&mut self) -> bool {
        !self.rng.chance(self.noise)
    }

    /// Move the main cursor to the first row with nothing drawn from it on,
    /// as a shell's prompt stands below its output (scrolling one blank
    /// default row in when the bottom row is drawn). Nothing on the
    /// alternate screen, whose cursor is not the main one.
    fn to_frontier(&mut self) {
        if self.t.modes().alternate_screen {
            return;
        }
        let main = self.t.main_grid();
        let rows = main.rows();
        let bytes = match (0..rows).find(|&r| !main.has_ink_from(r, 0)) {
            Some(r) => format!("\x1b[0m\x1b[{};1H", r + 1),
            None => format!("\x1b[0m\x1b[{rows};1H\r\n"),
        };
        self.log.push(format!("{:?} to the frontier", self.fg));
        self.t.process(bytes.as_bytes());
    }

    /// `n` lines of the writer's glyphs, each ended (with a reset pen) at
    /// `ended` percent.
    fn lines(&mut self, ended: u64) -> Vec<u8> {
        let cols = u64::from(self.t.grid().cols());
        let n = self.rng.below(cols * 2 + 1);
        let mut b = self.glyphs(n);
        if self.rng.chance(ended) {
            b.extend_from_slice(b"\x1b[0m\r\n");
        }
        b
    }

    /// `clear; cat file`: a full clear from home, then a few lines.
    fn clear_and_cat(&mut self) -> Vec<u8> {
        let cols = u64::from(self.t.grid().cols());
        let mut b = self.pen();
        b.extend_from_slice(b"\x1b[H\x1b[2J");
        for _ in 0..=self.rng.below(4) {
            let n = self.rng.below(cols) + 1;
            b.extend(self.glyphs(n));
            b.extend_from_slice(b"\x1b[0m\r\n");
        }
        b
    }

    /// IL / DL, often above the current drawer's open span.
    fn insert_or_delete_lines(&mut self) -> Vec<u8> {
        let rows = u64::from(self.t.grid().rows());
        let row = match self.open_span_screen_row() {
            Some(r) if r > 0 && self.rng.chance(60) => self.rng.below(u64::from(r)) + 1,
            _ => self.rng.below(rows) + 1,
        };
        let op = if self.rng.chance(50) { 'L' } else { 'M' };
        let mut b = self.pen();
        b.extend(format!("\x1b[{row};1H\x1b[{}{op}", self.rng.below(3) + 1).into_bytes());
        b
    }

    /// A region scroll: LF at its bottom, RI at its top, SU/SD.
    fn region_scroll(&mut self) -> Vec<u8> {
        let rows = u64::from(self.t.grid().rows());
        let top = self.rng.below(rows) + 1;
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

    /// 1049, 47 or 1047, set or reset. A screen swap renumbers no main row,
    /// so it never loses the anchors (the swap's liveness).
    fn alt_step(&mut self) -> Result<(), String> {
        let mode = ["1049", "47", "1047"][usize::try_from(self.rng.below(3)).unwrap_or(0)];
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

    /// A well-behaved writer's step: output at the cursor, a row at or
    /// above it rewritten in place and the cursor put back (a progress line;
    /// the drawer's own, the shell's often a stopped drawer's), `clear;
    /// cat`, and the moves that keep every anchor.
    fn calm_write_step(&mut self) -> Result<(), String> {
        let bytes = match self.rng.below(20) {
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
                    b.extend(self.glyphs(n));
                }
                b.extend_from_slice(b"\x1b8");
                b
            }
            13 => self.clear_and_cat(),
            14 | 15 => self.insert_or_delete_lines(),
            16 => return self.archival_step(),
            17 => self.region_scroll(),
            18 => return self.alt_step(),
            _ => b"\x1b[0m\r\n".to_vec(),
        };
        self.write(&bytes);
        Ok(())
    }

    /// One step: a cut, a write by the foreground, or a host action.
    fn step(&mut self) -> Result<u64, String> {
        match self.rng.below(100) {
            0..=11 => self.cut_step(),
            12..=89 => self.write_step()?,
            90..=96 => self.resize_step()?,
            _ => self.checkpoint_step()?,
        }
        self.stopped_keep_marks()?;
        oracle(&self.t)
    }

    /// Below [`MAX_HOLDER_MARKS`](super::MAX_HOLDER_MARKS) marks nothing is
    /// evicted: every stopped drawer still has its own.
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
                        // `exec claude` from the shell.
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
        let bytes: Vec<u8> = match self.rng.below(30) {
            0..=7 => {
                let n = self.rng.below(cols * 2 + 1);
                let mut b = self.glyphs(n);
                if self.rng.chance(70) {
                    b.extend_from_slice(b"\r\n");
                }
                b
            }
            8..=10 => {
                // Anywhere on the screen, often over a stopped drawer's rows.
                let row = match self.suspended_screen_row() {
                    Some(r) if self.rng.chance(50) => u64::from(r),
                    _ => self.rng.below(rows),
                };
                let mut b = format!("\x1b[{};{}H", row + 1, self.rng.below(cols) + 1).into_bytes();
                let n = self.rng.below(cols);
                b.extend(self.glyphs(n));
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
                // A full clear from home.
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
                // DECCRA, often across rows.
                let st = self.rng.below(rows) + 1;
                let dt = self.rng.below(rows) + 1;
                format!("\x1b[{st};1;{st};{cols};1;{dt};1;1$v").into_bytes()
            }
            23 => {
                // A margined line insert (DECLRMM).
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
                // A column edit (DECIC / DECDC), which moves no row.
                let op = if self.rng.chance(50) { "'}" } else { "'~" };
                format!("\x1b[{}{op}", self.rng.below(3) + 1).into_bytes()
            }
            _ => b"\r\n".to_vec(),
        };
        self.write(&bytes);
        Ok(())
    }

    /// A top-anchored archival scroll (`ESC[1;kr`, LF at its bottom), each LF
    /// checked against the splice law. The region often ends just above a
    /// live End anchor, so a span ending at the footer is exercised.
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
                // Archived: the splice law. A fresh session's blank top row
                // is dropped instead, a band move of rows `0..bottom` up one.
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

    /// A resize, rows-only, width or offloaded (its job run and re-attached
    /// at once). With the main cursor at the frontier, as an inline drawer
    /// leaves it, no resize may lose the anchors: the liveness half of the
    /// lend. The frontier is nothing drawn below the cursor's row and nothing
    /// owed under the screen (a bottom-push's rows, which a width change
    /// hands back below the cursor); a width change must also find the
    /// cursor's line readable from its head, before and after, since the
    /// lend vouches for no line it cannot read whole.
    fn resize_step(&mut self) -> Result<(), String> {
        let main = self.t.main_grid();
        let held = main.anchors().live_count() > 0 && !main.anchors().lost();
        let frontier =
            !main.has_ink_from(main.cursor().row.saturating_add(1), 0) && main.pushed_rows() == 0;
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
        if held && frontier && read_whole && after.anchors().lost() {
            return Err("a resize with the cursor at the frontier lost the anchors".into());
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
        // In place, as the seamless adopt does.
        restored.restore_checkpoint(&c);
        self.t = restored;
        Ok(())
    }
}

/// Run one seed; `Err` names the first violation with the steps that led
/// to it, `Ok` the admitted drawer rows the oracle checked.
fn fuzz(seed: u64, steps: usize) -> Result<u64, String> {
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
    Ok(drawn)
}

const SEEDS: u64 = 1000;
const STEPS: usize = 220;

#[test]
fn every_admitted_cell_is_the_current_drawers_or_blank() {
    let mut drawn = 0;
    for seed in 0..SEEDS {
        match fuzz(seed, STEPS) {
            Ok(n) => drawn += n,
            Err(e) => panic!("{e}"),
        }
    }
    // Not vacuous: a fence that admitted nothing would pass the oracle.
    // The author reported 133 729 admitted drawer rows for these seeds.
    assert!(
        drawn >= 60_000,
        "the fuzz checked only {drawn} admitted drawer rows"
    );
}

// ---- liveness probes ----

/// Draw a card: three rows of the drawer's glyph on its background.
fn card(i: u8) -> Vec<u8> {
    let who = Who::Drawer(i);
    let glyph = char::from(u8::try_from(who.glyph()).expect("ASCII"));
    let row: String = std::iter::repeat_n(glyph, 6).collect();
    format!("\x1b[48;5;{}m{row}\r\n{row}\r\n{row}\x1b[0m\r\n", who.bg()).into_bytes()
}

/// The absolute rows of the last card drawn, `[start, end)`.
fn card_rows(t: &Terminal, i: u8) -> (u64, u64) {
    let main = t.main_grid();
    let glyph = Who::Drawer(i).glyph();
    let rows: Vec<u64> = (0..main.rows())
        .filter(|&r| main.cell(r, 0).is_some_and(|c| c.char_data() == glyph))
        .map(|r| main.visible_to_absolute(r))
        .collect();
    (rows[0], rows[rows.len() - 1] + 1)
}

/// Ctrl-Z, a full-screen tool and five commands, then `fg`: the card is
/// still admitted. `Err` names what was lost.
fn ctrl_z_vim_fg_keeps_the_card() -> Result<(), String> {
    let mut t = Terminal::new(24, 40);
    t.process(b"$ claude\r\n");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    let rows = card_rows(&t, 0);
    if !t.holder_view().admits_rows(rows.0, rows.1) {
        return Err("the fresh card is not admitted".into());
    }
    // Ctrl-Z.
    t.note_holder_cut(group(None, true));
    t.process(b"\r\n[1]+ Stopped claude\r\n$ vim notes\r\n");
    // vim: a full-screen tool on the alternate screen.
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

/// Three drawers stopped in turn (each with its card), then the first one
/// brought back: still admitted.
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

fn liveness() -> Result<(), String> {
    ctrl_z_vim_fg_keeps_the_card()?;
    three_stopped_drawers_then_fg_keeps_the_first()
}

#[test]
fn a_stop_a_tool_and_fg_keep_the_card() {
    ctrl_z_vim_fg_keeps_the_card().unwrap();
    three_stopped_drawers_then_fg_keeps_the_first().unwrap();
}

/// Each knob breaks the fuzz: its oracle, the splice law, or one of its
/// liveness laws (a resize at the frontier, a screen swap, no eviction below
/// capacity).
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

/// The liveness probes catch the knobs that only lose too much.
#[test]
fn the_probes_catch_the_knobs_that_lose_too_much() {
    for knob in [FenceKnob::SwapLoses, FenceKnob::CountEviction] {
        assert!(with_knob(knob, || liveness().is_err()), "{knob:?}");
    }
}

// ---- §7 unit cases ----

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
    // Ink to the right of the cursor, on its row.
    let mut t = Terminal::new(10, 20);
    t.process(b"sss\r");
    t.note_holder_cut(group(Some(token(0)), true));
    assert!(t.holder_view().tainted, "the cursor row from its column");
    // Ink below.
    let mut t = Terminal::new(10, 20);
    t.process(b"\r\n\r\nsss\x1b[1;1H");
    t.note_holder_cut(group(Some(token(0)), true));
    assert!(t.holder_view().tainted, "a row below");
    // A background alone is ink.
    let mut t = Terminal::new(10, 20);
    t.process(b"\x1b[5;1H\x1b[41m\x1b[K\x1b[0m\x1b[1;1H");
    t.note_holder_cut(group(Some(token(0)), true));
    assert!(t.holder_view().tainted, "a background");
    // Ink before the cursor is not.
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

/// The design's splice case: a span ending exactly at the archival scroll's
/// insertion point keeps its end; under the knob it absorbs the inserted row.
#[test]
fn a_span_ending_at_the_archival_insertion_point_keeps_its_end() {
    let run = || {
        let mut t = Terminal::new(10, 20);
        t.process(b"x\r\n");
        t.note_holder_cut(group(Some(token(0)), true));
        t.process(b"AAA\r\nAAA\r\n");
        // Ctrl-Z with the cursor on row 3: the span ends there.
        t.note_holder_cut(group(None, true));
        let end = t.grid().visible_to_absolute(3);
        // The shell's top-anchored region [1..=3]; its LF inserts at row 3.
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
    // A line that wraps at 10 columns, and a stop with the cursor mid-way
    // through its second row: the span closes inside a line.
    t.process(b"\x1b[48;5;170mAAAAAAAAAAAA\x1b[0m");
    let (start_id, start_row) = {
        let fence = t.holder_fence();
        let span = fence.marks[0].spans[0];
        (span.start, t.grid().anchors().row(span.start).unwrap())
    };
    t.note_holder_cut(group(None, true));
    let end_id = t.holder_fence().marks[0].spans[0].end.unwrap();
    // The End sits on the continuation row the cursor is on.
    assert_eq!(t.grid().anchors().row(end_id), Some(start_row + 1));
    // A Start on that continuation row too (moved there by hand).
    t.grid_mut().anchors_mut().set_row(start_id, start_row + 1);
    t.resize(8, 20);
    let anchors = t.grid().anchors();
    assert!(!anchors.lost());
    let start = anchors.row(start_id).unwrap();
    let end = anchors.row(end_id).unwrap();
    // The line is one row now: the End goes to its head, the Start past it.
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
    // Far more output than the hold-back keeps.
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

/// A bottom-push owes drawn rows under the screen, and a width change hands
/// them back below the cursor's line, inside the current drawer's open
/// span. A row of background alone (another writer's BCE erase) is drawn
/// too: the lend sees it come in and loses the anchors, so the span admits
/// none of it. Found by the writer oracle at 3 000 seeds.
#[test]
fn a_background_row_handed_back_under_the_cursor_is_not_admitted() {
    let other = Who::Drawer(2);
    let mut t = term();
    // The other writer's erased row, the cursor homed above it: the shrink
    // finds no blank tail to trim and pushes the bottom rows under.
    t.process(format!("\x1b[6;1H\x1b[48;5;{}m\x1b[2K\x1b[0m\x1b[H", other.bg()).as_bytes());
    t.resize(5, COLS);
    assert!(t.main_grid().pushed_rows() > 0, "precondition: rows owed");
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(b"\x1b[48;5;170mAAAA\x1b[0m\r\n");
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

    // Untainted: the open span's Start rises to the top.
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
    // Kitty unscroll renumbers rows: the anchors are lost.
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
    // A non-drawer's 1049 marks nothing.
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
    // A rich row: true colours, a combining mark, an emoji cluster, a wide
    // glyph, a hyperlink.
    let mut t = term();
    t.process(
        "\x1b[38;2;200;100;50;48;2;10;20;30me\u{301}x \u{1F44D}\u{1F3FD} \u{4E2D}\
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
    // Past the ring (the tiered store holds it): nothing to vouch with.
    t.process(&b"\r\n".repeat(64));
    assert_eq!(t.main_grid().row_fingerprint_at_absolute(rich), None);
}

#[test]
fn a_fingerprint_reads_a_true_colour_wherever_the_screen_keeps_it() {
    // The same glyphs redrawn in another true colour (the screen keeps a
    // cell whose only extra is its colour in a dense ring, not the map): a
    // seal must see the change.
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
    // `claude`, Ctrl-Z, `ls -l` with `lines` lines of output, `fg`: whether
    // the card is still admitted.
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
    // Past the ring the seal cannot read it back: cut.
    assert!(!after(usize::from(ROWS) + 64));
}

#[test]
fn a_stopped_drawer_whose_rows_were_rewritten_loses_them_at_fg() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    t.process(&card(0));
    let rows = card_rows(&t, 0);
    t.note_holder_cut(group(None, true));
    // `clear; cat card.txt` by the shell, then fg.
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
    let mut adopted = Terminal::new(10, 20);
    adopted.restore_checkpoint(&c);
    assert_eq!(adopted.holder_view(), t.holder_view());
}

#[test]
fn a_fence_that_does_not_install_whole_installs_as_none() {
    let mut t = Terminal::new(10, 20);
    t.note_holder_cut(group(Some(token(0)), true));
    let mut c = t.checkpoint();
    // A span naming a slot that is not a Start.
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
    c.absolute_row_counter = 3; // below the floor: the restore renumbers
    let restored = Terminal::from_checkpoint(&c);
    assert!(restored.holder_view().tainted);
}

/// A drawer running under a signed C, its card inline in its output.
fn session_with_card_under_c() -> (Terminal, (u64, u64), (u64, u64)) {
    let nonce = [7u8; 32];
    let mut t = Terminal::new(20, 40);
    t.authorize_shell_integration(nonce);
    t.set_require_shell_integration_nonce(true);
    let id = super::super::ShellIntegrationNonce(nonce).to_hex();
    // A same-build card above C: printed by `cat` before the drawer's command.
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
    // A predecessor without the fence: nothing carried.
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
    // The fence ran: the cut came before the card.
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
    // Unsigned marks: no seed, a tainted mark until the drawer clears.
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
    // Not a drawer with an Exact build: nothing at all.
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

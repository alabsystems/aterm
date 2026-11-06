// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The words presence prints.

use std::marker::PhantomData;

use super::{AgentPhase, CTX_WARN_PCT, Host, Slot};
use crate::Duration;
use crate::text::{is_stripped, truncate};

/// One cell of a string as the host's grid draws it ([`cells`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Cell {
    /// The byte the cell opens at.
    pub(super) at: usize,
    /// The character that opened it.
    pub(super) ch: char,
    /// The columns it takes.
    pub(super) width: usize,
}

/// The glyphs with an emoji form that the row pins to text presentation, one
/// cell each.
#[inline]
const fn text_presentation(c: char) -> bool {
    matches!(
        c,
        '\u{2713}' | '\u{2717}' | '\u{26a0}' | '\u{2709}' | '\u{2715}' | '\u{1f512}'
    )
}

/// The characters one grid cell holds: the one that opened it and at most 16
/// more (aterm-grid's `CellExtra::MAX_COMBINING`, aterm-grapheme's
/// `MAX_JOINED`).
const CELL_CHARS: usize = 1 + 16;

/// What one character does to the row's cells ([`CellWalk::step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellStep {
    /// It opens a cell of its own, this many columns wide.
    Opens(usize),
    /// It is drawn in the cell before it, which it widens by this many
    /// columns: a combining mark 0, VS16 after `©` 1.
    Joins(usize),
    /// Nothing of it is kept: a zero-width character with no cell to join
    /// (the grid keeps nothing of it either), or one more character for a
    /// cell that already holds [`CELL_CHARS`].
    Dropped,
}

/// The host's grid walked a character at a time, cell by cell, each cell
/// held to what the grid's cell holds ([`CELL_CHARS`]): the one walk the
/// measure ([`cells`]) and the token's own pass ([`sanitize_token`]) share.
struct CellWalk<V> {
    /// The current cell's characters, at most [`CELL_CHARS`] — stale while
    /// [`Self::opener`] holds the cell's one character.
    cell: String,
    /// The character that opened the current cell, not yet copied into
    /// [`Self::cell`]: copied only when a character is measured against it, so
    /// a row of letters and glyphs never touches the buffer.
    opener: Option<char>,
    /// How many characters the current cell holds (0 before the first).
    held: usize,
    /// `V::columns` of the current cell, once measured.
    cols: Option<usize>,
    host: PhantomData<V>,
}

impl<V: Host> CellWalk<V> {
    const fn new() -> Self {
        Self {
            cell: String::new(),
            opener: None,
            held: 0,
            cols: None,
            host: PhantomData,
        }
    }

    /// Lay out `c` after everything kept so far. Printable ASCII and the
    /// [`text_presentation`] glyphs open a one-cell cell wherever they stand
    /// (the grid's bulk path never folds them); a printing character opens its
    /// own after no cell or after a one-ASCII-character cell (the grid folds
    /// only into a cell holding a ZWJ, a regional indicator or a wide emoji
    /// base); everything else is measured.
    fn step(&mut self, c: char) -> CellStep {
        if c.is_ascii() || text_presentation(c) {
            self.open(c, None);
            return CellStep::Opens(1);
        }
        let own = V::columns(c.encode_utf8(&mut [0; 4]));
        let full = self.held >= CELL_CHARS;
        if own == 0 && (self.held == 0 || full) {
            return CellStep::Dropped;
        }
        if own > 0 && (self.held == 0 || self.opener.is_some_and(|o| o.is_ascii())) {
            self.open(c, Some(own));
            return CellStep::Opens(own);
        }
        if let Some(opener) = self.opener.take() {
            self.cell.clear();
            self.cell.push(opener);
        }
        let before = match self.cols {
            Some(cols) => cols,
            None => V::columns(&self.cell),
        };
        self.cell.push(c);
        let grew = V::columns(&self.cell).saturating_sub(before);
        if own == 0 || grew == 0 {
            // Drawn in the cell before it: a zero-width character (which may
            // widen that cell), or one the grid folds into it.
            if full {
                self.cell.pop();
                return CellStep::Dropped;
            }
            self.held += 1;
            self.cols = Some(before + grew);
            return CellStep::Joins(grew);
        }
        self.open(c, Some(own));
        CellStep::Opens(grew)
    }

    /// Open a cell with `c`, `cols` wide once measured.
    const fn open(&mut self, c: char, cols: Option<usize>) {
        self.opener = Some(c);
        self.held = 1;
        self.cols = cols;
    }
}

/// `s` laid out in the host's grid ([`Host::columns`]), cell by cell, as the painter draws it ([`CellWalk`]).
pub(super) fn cells<V: Host>(s: &str) -> Cells<'_, V> {
    Cells {
        chars: s.char_indices(),
        walk: CellWalk::new(),
        open: None,
    }
}

/// The cells of a string ([`cells`]), each yielded once the next one opens or the string ends.
pub(super) struct Cells<'a, V> {
    chars: std::str::CharIndices<'a>,
    walk: CellWalk<V>,
    /// The cell the walk is in, still open to the characters drawn in it.
    open: Option<Cell>,
}

impl<V: Host> Iterator for Cells<'_, V> {
    type Item = Cell;

    fn next(&mut self) -> Option<Cell> {
        for (at, ch) in self.chars.by_ref() {
            match self.walk.step(ch) {
                CellStep::Opens(width) => {
                    if let Some(done) = self.open.replace(Cell { at, ch, width }) {
                        return Some(done);
                    }
                }
                CellStep::Joins(grew) => {
                    if let Some(cell) = self.open.as_mut() {
                        cell.width += grew;
                    }
                }
                CellStep::Dropped => {}
            }
        }
        self.open.take()
    }
}

/// Display width in cells.
pub(super) fn width<V: Host>(s: &str) -> usize {
    if s.is_ascii() {
        return s.len();
    }
    cells::<V>(s).map(|c| c.width).sum()
}

/// `s` in at most `max` cells ([`cells`]), the last one `…` when it was cut.
pub(super) fn truncate_cells<V: Host>(s: &str, max: usize) -> String {
    if s.is_ascii() {
        return truncate(s, max);
    }
    if width::<V>(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut used = 0usize;
    let end = cells::<V>(s)
        .find(|c| {
            used += c.width;
            used > max - 1
        })
        .map_or(s.len(), |c| c.at);
    let mut t = s[..end].to_string();
    t.push('\u{2026}');
    t
}

/// Presence's own vocabulary (the glyphs that spell a need, the person's
/// hand, a hold, mail, the link, a trust verdict, a reset) and its clause
/// separator.
const BAND_GLYPHS: &[char] = &[
    '\u{25c6}',
    '\u{270b}',
    '\u{25c2}',
    '\u{25b8}',
    '\u{2298}',
    '\u{2709}',
    '\u{21af}',
    '\u{2191}',
    '\u{27df}',
    '~',
    '\u{2715}',
    '\u{25c7}',
    '\u{2713}',
    '\u{2717}',
    '\u{26a0}',
    '\u{2190}',
    '\u{2192}',
    '\u{00b7}',
    '\u{1f512}',
];

/// A wire-safe token.
#[must_use]
pub fn sanitize_token<V: Host>(s: &str, cap: usize) -> String {
    let mut clean = String::with_capacity(s.len());
    let mut walk = CellWalk::<V>::new();
    let mut used = 0usize;
    let mut at_space = true;
    for c in s.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if is_stripped(c) || BAND_GLYPHS.contains(&c) || (c == ' ' && at_space) {
            continue;
        }
        match walk.step(c) {
            CellStep::Dropped => continue,
            CellStep::Opens(cols) | CellStep::Joins(cols) => used += cols,
        }
        at_space = c == ' ';
        clean.push(c);
    }
    let token = clean.trim_end();
    if used - (clean.len() - token.len()) <= cap {
        return token.to_string();
    }
    truncate_cells::<V>(token, cap)
}

/// A hold's reason as every human-facing surface prints and speaks it.
#[must_use]
pub fn hold_reason_words<V: Host>(reason: &str) -> String {
    sanitize_token::<V>(&V::pct_decode(reason), 32)
}

/// A duration as the row prints it.
#[must_use]
pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86_400, (s % 86_400) / 3600)
    }
}

/// The short form of a session id for the hand slot.
#[must_use]
pub fn short_sid(sid: &str) -> String {
    let n = sid.chars().count().min(6);
    sid.chars().take(n).collect()
}

/// A box's kind in a person's words.
#[must_use]
pub fn prompt_kind_words(detail: Option<&str>) -> Option<&str> {
    let kind = detail
        .and_then(|d| d.split(':').next())
        .filter(|k| !k.is_empty() && *k != "other")?;
    Some(match kind {
        "plan-enter" => "plan mode",
        "plan-exit" => "plan",
        "held-message" => "held message",
        "goal-proposal" => "goal",
        "computer-use" => "computer use",
        "read-outside-setting" => "outside read",
        "model-switch" => "model switch",
        "rate-nudge" => "rate-limit model",
        "model-pick" => "model picker",
        "goal-resume" => "paused goal",
        "mcp-server" => "MCP server",
        "auto-mode-setup" => "auto-mode setup",
        "chrome-intro" => "Claude in Chrome intro",
        "trust" => "folder trust",
        "powershell" => "PowerShell",
        other => other,
    })
}

/// A box in the row's words.
#[must_use]
pub fn prompt_band_word(detail: Option<&str>) -> String {
    match prompt_kind_words(detail) {
        Some("question") => "question".to_string(),
        Some(kind) => format!("{kind} approval"),
        None => "approval".to_string(),
    }
}

/// The state line's context words for `pct` per cent left, warned at [`CTX_WARN_PCT`] or under.
fn context_left_words(pct: u8) -> String {
    let warn = if pct <= CTX_WARN_PCT { " \u{26a0}" } else { "" };
    format!("ctx {pct}% left{warn}")
}

/// The hover's state line for `slot` (`program`: the host's name for the agent in front, `claude`).
#[must_use]
pub fn state_line<V: Host>(slot: &Slot<V>, program: Option<&str>) -> Option<String> {
    let mut parts = Vec::with_capacity(3);
    if let Some(program) = program
        .map(|p| sanitize_token::<V>(p, 24))
        .filter(|p| !p.is_empty())
    {
        parts.push(program);
    }
    match slot
        .agent
        .as_ref()
        .filter(|a| !matches!(a.phase, AgentPhase::Unknown))
    {
        Some(agent) => {
            let word = match &agent.phase {
                AgentPhase::Prompt { detail } => prompt_band_word(detail.as_deref()),
                phase => phase.band_word().to_string(),
            };
            parts.push(word);
            if let Some(pct) = agent.context_pct {
                parts.push(context_left_words(pct));
            }
        }
        None => match slot.program {
            Some((word, _)) => {
                parts.clear();
                parts.push(word.to_string());
                if let Some(pct) = slot.program_ctx_left {
                    parts.push(context_left_words(pct));
                }
            }
            None => {
                if let Some((word, _)) = slot.shell {
                    parts.push(word.to_string());
                }
            }
        },
    }
    (!parts.is_empty()).then(|| parts.join(" \u{00b7} "))
}

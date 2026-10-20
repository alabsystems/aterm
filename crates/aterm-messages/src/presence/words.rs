// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The words presence prints: the needs pill's sentence ([`pill_words`]),
//! the hover's state line ([`state_line`]), the vocabulary every free-text
//! value is held to ([`sanitize_token`]), and the cells the host's grid lays
//! a string out in, which the token cap and the in-grid pill measure by.

use std::fmt::Write as _;
use std::marker::PhantomData;

use super::pill::{Pill, PillAct, PillKind};
use super::{AgentPhase, CTX_WARN_PCT, Host, NeedSeverity, Slot};
use crate::text::{is_stripped, truncate};
use crate::{Duration, Instant};

/// One cell of a string as the host's grid draws it ([`cells`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Cell {
    /// The byte the cell opens at: its own characters run from here to the
    /// next cell's `at` (or the string's end).
    pub(super) at: usize,
    /// The character that opened it — what a one-character cell paints.
    pub(super) ch: char,
    /// The columns it takes: 1, or 2 for a wide glyph (or one VS16 widened).
    pub(super) width: usize,
}

/// The glyphs with an emoji form that the row pins to text presentation, one
/// cell each. U+1F512 (the fleet lock) is emoji-presentation by default, and a
/// renderer would otherwise draw its colour face two cells wide. All are the
/// row's own ([`BAND_GLYPHS`]), so no free text carries one. The measure
/// ([`cells`]) and the painter's pin read this one list.
fn text_presentation(c: char) -> bool {
    matches!(
        c,
        '\u{2713}' | '\u{2717}' | '\u{26a0}' | '\u{2709}' | '\u{2715}' | '\u{1f512}'
    )
}

/// The characters one grid cell holds: the one that opened it and at most 16
/// more (aterm-grid's `CellExtra::MAX_COMBINING`, aterm-grapheme's
/// `MAX_JOINED`). The grid drops the rest, and so does the row ([`CellWalk`]).
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
///
/// Whether a character opens a cell or joins the one before it turns on that
/// cell alone, so each character is measured against the current cell only;
/// with a cell capped at [`CELL_CHARS`], a walk stays linear and a token
/// bounded even for `1` followed by thousands of combining marks, and nothing
/// the grid draws is lost. A VS16 past a full cell still widens the grid's
/// cell, but the row drops it and a token never carries it, so a token's grid
/// width is still the row's measure of it.
struct CellWalk<V> {
    /// The current cell's characters, at most [`CELL_CHARS`] — stale while
    /// [`Self::opener`] holds the cell's one character.
    cell: String,
    /// The character that opened the current cell, not yet copied into
    /// [`Self::cell`]: most cells hold nothing else, so it is copied only
    /// when a character is measured against it, and a row of letters and
    /// glyphs never touches the buffer.
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

    /// Lay out `c` after everything kept so far. Printable ASCII opens a cell
    /// of its own wherever it stands (the grid's bulk path never folds it),
    /// and the glyphs pinned to [`text_presentation`] take one cell each;
    /// a printing character opens its own after no cell or after a cell of
    /// one ASCII character (the grid folds a character only into a cell
    /// holding a ZWJ, a regional indicator or a wide emoji base); everything
    /// else is measured.
    fn step(&mut self, c: char) -> CellStep {
        if c.is_ascii() || text_presentation(c) {
            self.open(c, None);
            return CellStep::Opens(1);
        }
        let own = V::columns(c.encode_utf8(&mut [0; 4]));
        let full = self.held >= CELL_CHARS;
        // A zero-width character joins the cell before it: with none, or a
        // full one, nothing of it is kept, and it can open nothing.
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
        // What `c` adds to the cell it follows, in the grid's own walk.
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
    fn open(&mut self, c: char, cols: Option<usize>) {
        self.opener = Some(c);
        self.held = 1;
        self.cols = cols;
    }
}

/// `s` laid out in the host's grid ([`Host::columns`]), cell by cell, as the
/// painter draws it ([`CellWalk`]): each character either opens a cell (a
/// wide glyph's two columns, any other's one) or is drawn in the cell before
/// it (a combining mark, a variation selector, VS16 widening the cell it
/// joins as in `©️`, the rest of a ZWJ sequence or a flag); past a cell's
/// [`CELL_CHARS`], or with no cell to join, it is dropped. Linear in `s`. The
/// row's cells are the grid's for its free text and its own glyphs alike.
pub(super) fn cells<V: Host>(s: &str) -> Cells<'_, V> {
    Cells {
        chars: s.char_indices(),
        walk: CellWalk::new(),
        open: None,
    }
}

/// The cells of a string ([`cells`]), each yielded once the next one opens
/// or the string ends: a character after a cell may still widen it.
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

/// Display width in cells: the sum of [`cells`]. ASCII is its length (each
/// printable character is one cell of its own), without the walk.
pub(super) fn width<V: Host>(s: &str) -> usize {
    if s.is_ascii() {
        return s.len();
    }
    cells::<V>(s).map(|c| c.width).sum()
}

/// `s` in at most `max` cells ([`cells`]), the last one `…` when it was cut:
/// cut between two cells, never inside one, so a mark stays with its letter
/// and a wide glyph is never halved. `max == 0` is the empty string.
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
    // The first cell that would leave no room for the `…`.
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
/// separator. None of it may arrive inside a free-text value: a tab titled
/// `◆ frozen · Restart` would otherwise print as a need the session does not
/// have, and a `meta role` of `⊘ hold pause ◂ manager` as a hold and a hand.
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

/// The glyphs only the pill's own grammar prints: a label or a need's words
/// carrying one would forge a need or a hold ([`pill_text`]).
const PILL_GLYPHS: &[char] = &['\u{25c6}', '\u{25c7}', '\u{270b}'];

/// A wire-safe token: printable, single-spaced, none of presence's own
/// glyphs, at most `cap` cells of the host's grid, cut with `…` between two
/// cells (`truncate_cells`). Each cell holds what the grid's cell holds (its
/// opener and at most 16 more) and a zero-width character with no cell to
/// join is dropped (`CellWalk`), so a token is at most `17 × cap` characters
/// whatever its input. A free-text value presence prints (a holder, a
/// reason, a reset time, a box's kind) passes here, so a control byte, a
/// bidi override, an invisible character (the band's own strip,
/// `text::is_stripped`) or presence's grammar in an agent-chosen name never
/// reaches the chrome or forges a mark.
#[must_use]
pub fn sanitize_token<V: Host>(s: &str, cap: usize) -> String {
    sanitize_without::<V>(s, cap, BAND_GLYPHS)
}

/// A pill's free text (a tab's label, a need's words) held to
/// [`sanitize_token`]'s rule with only the pill's own glyphs refused
/// ([`PILL_GLYPHS`]): a path's `~` and a sentence's `·` are words there.
#[must_use]
pub fn pill_text<V: Host>(s: &str, cap: usize) -> String {
    sanitize_without::<V>(s, cap, PILL_GLYPHS)
}

/// [`sanitize_token`] refusing the glyphs `forbidden`.
fn sanitize_without<V: Host>(s: &str, cap: usize, forbidden: &[char]) -> String {
    let mut clean = String::with_capacity(s.len());
    let mut walk = CellWalk::<V>::new();
    let mut used = 0usize;
    let mut at_space = true;
    for c in s.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if is_stripped(c) || forbidden.contains(&c) || (c == ' ' && at_space) {
            continue;
        }
        // The token's own cells, as the grid holds them: what the grid
        // would keep nothing of is not kept.
        match walk.step(c) {
            CellStep::Dropped => continue,
            CellStep::Opens(cols) | CellStep::Joins(cols) => used += cols,
        }
        at_space = c == ' ';
        clean.push(c);
    }
    let token = clean.trim_end();
    // The one trailing space `trim_end` drops was a cell of its own.
    if used - (clean.len() - token.len()) <= cap {
        return token.to_string();
    }
    truncate_cells::<V>(token, cap)
}

/// A hold's reason as every human-facing surface prints and speaks it: the
/// wire token (`status hold=`'s pct-encoded `main%20broken`) decoded
/// ([`Host::pct_decode`]) and then held to [`sanitize_token`]'s rule, since a
/// bridge-supplied reason can encode a control byte or a bidi override.
#[must_use]
pub fn hold_reason_words<V: Host>(reason: &str) -> String {
    sanitize_token::<V>(&V::pct_decode(reason), 32)
}

/// A duration as the row prints it: `12s`, `3m12s`, `2h05m`, `1d 22h`.
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

/// The short form of a session id for the hand slot: `s-1e91`.
#[must_use]
pub fn short_sid(sid: &str) -> String {
    let n = sid.chars().count().min(6);
    sid.chars().take(n).collect()
}

/// A box's kind in a person's words: the kind before the colon of
/// `kind[:verdict]` (`bash:not-read-only` → `bash`), the hyphenated kinds
/// spelled out (`plan-exit` → `plan`). `None` for a box of no named kind.
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
        "trust" => "folder trust",
        "powershell" => "PowerShell",
        other => other,
    })
}

/// A box in the row's words: `bash approval`, `plan approval`; the agent's
/// question tool is a `question`, never an approval; a box of no named kind
/// is `approval`.
#[must_use]
pub fn prompt_band_word(detail: Option<&str>) -> String {
    match prompt_kind_words(detail) {
        Some("question") => "question".to_string(),
        Some(kind) => format!("{kind} approval"),
        None => "approval".to_string(),
    }
}

/// The hover's state line for `slot` (`program`: the host's name for the
/// agent in front, `claude`): `claude · busy · ctx 61% left`. The agent's
/// phase in a person's words, its context left where its screen shows it;
/// where no phase is read, the program's own report (`codex working`, OSC
/// 7501, display only), else the shell's status word. No age: a hover is
/// composed when its strip is pushed and shown any time after, so an age
/// there would read the push's, frozen. `None` with nothing to say. A
/// record, composed when a hover is built, never per frame.
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
                let warn = if pct <= CTX_WARN_PCT { " \u{26a0}" } else { "" };
                parts.push(format!("ctx {pct}% left{warn}"));
            }
        }
        // The program's own report (OSC 7501) where the host honours one: it
        // names its program (`codex working`), so it stands alone.
        None => match slot.program {
            Some((word, _)) => {
                parts.clear();
                parts.push(word.to_string());
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

/// The needs pill's words (§5), in the pieces its painters lay out and clip
/// apart ([`super::pill_layout`]): the glyph, never clipped; the tab's label,
/// clipped after the words; the words; the act; the count of the window's
/// other needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PillWords {
    /// `◆` stuck, `◇` decide, `✋` yours.
    pub glyph: char,
    /// A background tab's label and number (`fmt (tab 4)`), another pane of
    /// the front tab by its label alone; empty for the session in front,
    /// whose label the window already shows.
    pub label: String,
    /// The need's words and, for a background tab, its age; `You have this
    /// tab` for the person's hold.
    pub words: String,
    /// [`Self::words`] as wide as they get before the age moves to its next
    /// unit (the age at its unit's widest figure, [`pill_age_room`]; the
    /// words themselves with no age): what a painter measures the words'
    /// span by, so a pill's width — and the tabs beside it — holds still
    /// while its age ticks. Drawn never; `words` is.
    pub room: String,
    /// The one act the pill offers, if any.
    pub act: Option<PillAct>,
    /// `+2`: the window's other needs (a click goes to the next of them,
    /// `drive::next_need`).
    pub more: Option<String>,
}

/// The joint between the pill's pieces.
const PILL_SEP: &str = " \u{00b7} ";

impl PillWords {
    /// The whole sentence, as `chrome presence pill=` prints it and a screen
    /// reader speaks it: `◆ fmt (tab 4) · never started · 2m`, `◆ frozen —
    /// not reading input since 14:02 · Restart`, `✋ You have this tab · Hand
    /// Back`, with a trailing ` +2` for more needs.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        out.push(self.glyph);
        out.push(' ');
        if !self.label.is_empty() {
            out.push_str(&self.label);
            out.push_str(PILL_SEP);
        }
        out.push_str(&self.words);
        if let Some(act) = self.act {
            out.push_str(PILL_SEP);
            out.push_str(act.word());
        }
        if let Some(more) = &self.more {
            out.push(' ');
            out.push_str(more);
        }
        out
    }
}

/// `pill`'s words at `now` ([`PillWords`]).
#[must_use]
pub fn pill_words(pill: &Pill, now: Instant) -> PillWords {
    let glyph = match pill.kind {
        PillKind::Need(NeedSeverity::Stuck) => '\u{25c6}',
        PillKind::Need(NeedSeverity::Decide) => '\u{25c7}',
        PillKind::Yours => '\u{270b}',
    };
    let label = match pill.tab {
        // A tab nothing else names is named by its number (the host's
        // namer, `App::tab_label_of_session`), which the pill says once:
        // `tab 6`, never `tab 6 (tab 6)`.
        Some(tab) if !pill.label.is_empty() && pill.label != format!("tab {tab}") => {
            format!("{} (tab {tab})", pill.label)
        }
        Some(tab) => format!("tab {tab}"),
        // The front tab: another pane of it by its label, the front one bare.
        None => pill.label.clone(),
    };
    let mut words = match pill.kind {
        PillKind::Yours => "You have this tab".to_string(),
        PillKind::Need(_) => pill.what.clone(),
    };
    let mut room = words.clone();
    // The age is a background tab's alone ([`Pill::since`]): the tab in front
    // is in view, and a pill that prints no age arms no tick.
    if let Some(since) = pill.since {
        let age = now.saturating_duration_since(since);
        let _ = write!(words, "{PILL_SEP}{}", pill_age(age));
        let _ = write!(room, "{PILL_SEP}{}", pill_age_room(age));
    }
    PillWords {
        glyph,
        label,
        words,
        room,
        act: pill.act,
        more: (pill.more > 0).then(|| format!("+{}", pill.more)),
    }
}

/// A need's age as the pill prints it, coarse: `5s`, `2m`, `3h`, `1d`.
#[must_use]
pub fn pill_age(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// The widest figure [`pill_age`] prints in the unit it prints `d` in: `59s`,
/// `59m`, `23h`, and for days as many `9`s as the count has digits. With
/// every digit as wide as every other (the macOS pill's monospaced digits,
/// a grid's cells), no age in that unit is wider, so a pill measured by it
/// keeps its width until the unit moves — a few times in a need's life,
/// never once a second.
#[must_use]
pub fn pill_age_room(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        "59s".to_string()
    } else if s < 3600 {
        "59m".to_string()
    } else if s < 86_400 {
        "23h".to_string()
    } else {
        let digits = (s / 86_400).to_string().len();
        let mut out = "9".repeat(digits);
        out.push('d');
        out
    }
}

/// When a pill whose need began at `since` next prints another age, after
/// `now`: the next whole unit of the figure [`pill_age`] prints (a second
/// under a minute, a minute under an hour, an hour under a day, else a day),
/// so a figure is never stale by a step and never recomposed between two.
#[must_use]
pub fn pill_due(since: Instant, now: Instant) -> Instant {
    let age = now.saturating_duration_since(since);
    let step = match age.as_secs() {
        0..60 => 1,
        60..3600 => 60,
        3600..86_400 => 3600,
        _ => 86_400,
    };
    let next = (age.as_secs() / step + 1) * step;
    since + Duration::from_secs(next)
}

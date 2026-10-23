// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLAUDE CODE'S AGENTS PANEL.

/// The pointer `❯ ` a selected or hovered row opens with, in column 0 (`figures.pointer`).
pub const POINTER: char = '\u{276F}';

/// The bullets of the panel's rows.
pub const BULLETS: [char; 4] = ['\u{25EF}', '\u{25CB}', '\u{23FA}', '\u{25CF}'];

/// A workflow paused on a rate limit draws `⏸` for its bullet.
pub const PAUSED: char = '\u{23F8}';

/// The tree connectors before a nested agent's bullet (`├`, `└`).
pub const TREE: [char; 2] = ['\u{251C}', '\u{2514}'];

/// The progress bar's filled cell.
pub const BAR_FILL: [char; 2] = ['\u{25B0}', '\u{2588}'];

/// The progress bar's empty cell.
pub const BAR_EMPTY: [char; 2] = ['\u{25B1}', '\u{2591}'];

/// The arrow before a token count once the agent has used a tool (`↓`), and the one before it until then (`↑`).
pub const ARROW_DOWN: char = '\u{2193}';
/// See [`ARROW_DOWN`].
pub const ARROW_UP: char = '\u{2191}';

/// The warning sign before `Large workflow`.
pub const WARNING: char = '\u{26A0}';

/// The words builds before 2.1.301 drew after a workflow's `done/total` (`1/4 agents done`).
pub const PROGRESS_OLD: [&str; 2] = [" agents done", " agent done"];

/// The most rows an [`AgentsPanel`] holds.
pub const PANEL_ROWS_MAX: usize = 16;

/// How many pane columns a char takes, as the caller's text counts them ([`one_cell`], or `aterm_grapheme::char_width`).
pub type Width = fn(char) -> usize;

/// One column per char.
#[must_use]
pub fn one_cell(_: char) -> usize {
    1
}

/// Columns `start..end` of a pane row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u16,
    pub end: u16,
}

impl Span {
    /// The columns it takes.
    #[must_use]
    pub fn len(&self) -> u16 {
        self.end.saturating_sub(self.start)
    }

    /// Whether it takes none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// As a range of `usize` columns.
    #[must_use]
    pub fn range(&self) -> std::ops::Range<usize> {
        usize::from(self.start)..usize::from(self.end)
    }

    /// The span from this one's start to `other`'s end.
    #[must_use]
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// What a panel row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowKind {
    /// `⏺ main`: the main session's transcript.
    Main,
    /// A background agent (or teammate).
    Agent,
    /// A dynamic workflow.
    Workflow,
    /// `◯ N idle agents`.
    IdleSummary,
    /// ` ↓ N more`: task rows scrolled under the window.
    More,
}

/// The bullet's glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bullet {
    /// `◯` / `○`: a row not viewed.
    Hollow,
    /// `⏺` / `●`: the transcript being viewed.
    Filled,
    /// `⏸`: a workflow paused on a rate limit.
    Paused,
}

/// A row's elapsed time as drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Elapsed {
    /// Elapsed `ms` as drawn, quantized to `res_ms`.
    Clock { ms: u64, res_ms: u32 },
    /// `awaiting approval`.
    AwaitingApproval,
    /// `idle`: a teammate idle, or a finished agent whose result was read.
    Idle,
    /// `waiting`: a running agent with nothing in flight.
    Waiting,
}

impl Elapsed {
    /// The clock's `(ms, res_ms)`, `None` for a word.
    #[must_use]
    pub fn clock(&self) -> Option<(u64, u32)> {
        match *self {
            Elapsed::Clock { ms, res_ms } => Some((ms, res_ms)),
            _ => None,
        }
    }
}

/// A token count as drawn (`↓ 613.6k tokens`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tokens {
    /// The count the text says (`613.6k` = 613 600).
    pub count: u64,
    /// The step the text moves in.
    pub quantum: u64,
    /// `↓`: the agent has used a tool (`↑` before that).
    pub used_tools: bool,
}

/// A workflow's progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bar {
    pub span: Span,
    /// The filled cells.
    pub filled: u8,
    /// Every cell of it, filled and empty.
    pub cells: u8,
    /// Drawn in Ghostty's block glyphs (`█`/`░`), not the pill's.
    pub blocks: bool,
}

/// A workflow's `done/total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Progress {
    pub done: u32,
    pub total: u32,
}

/// One panel row as [`parse_row`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PanelRow {
    /// The pane row, as [`AgentsPanel::push`] placed it (`0` from [`parse_row`] alone).
    pub row: u16,
    pub kind: RowKind,
    /// The `❯` cell of a selected or hovered row.
    pub pointer: Option<Span>,
    /// A nested agent's depth (`0` at the top level).
    pub depth: u8,
    /// Its tree connector (`├` / `└`).
    pub connector: Option<Span>,
    /// The bullet and its glyph (none on a `↓ N more` row).
    pub bullet: Option<(Span, Bullet)>,
    /// The name (`main`; an agent's or workflow's name, its ` (+N)` left out; the idle summary's words; the `more` row's words).
    pub name: Span,
    /// A hash of the name's text.
    pub name_hash: u64,
    /// The `N` of a name's ` (+N)` (hidden descendants), else `0`.
    pub descendants: u16,
    /// The activity text between the name and the status (an old workflow's description), where any is drawn.
    pub activity: Option<Span>,
    /// A hash of the activity's text (`0` with none).
    pub activity_hash: u64,
    pub elapsed: Option<(Span, Elapsed)>,
    pub tokens: Option<(Span, Tokens)>,
    /// ` · N queued`.
    pub queued: Option<(Span, u16)>,
    pub progress: Option<(Span, Progress)>,
    pub bar: Option<Bar>,
    /// `⚠ Large workflow…`.
    pub warning: Option<Span>,
    /// `↑ N more` on the main row, `↓ N more` on the more row.
    pub count: Option<(Span, u16)>,
}

impl PanelRow {
    const NONE: PanelRow = PanelRow {
        row: 0,
        kind: RowKind::More,
        pointer: None,
        depth: 0,
        connector: None,
        bullet: None,
        name: Span { start: 0, end: 0 },
        name_hash: 0,
        descendants: 0,
        activity: None,
        activity_hash: 0,
        elapsed: None,
        tokens: None,
        queued: None,
        progress: None,
        bar: None,
        warning: None,
        count: None,
    };

    /// The status: the columns from the first of its parts (elapsed, tokens, queued, progress, warning) to the last.
    #[must_use]
    pub fn status(&self) -> Option<Span> {
        [
            self.progress.map(|p| p.0),
            self.elapsed.map(|e| e.0),
            self.tokens.map(|t| t.0),
            self.queued.map(|q| q.0),
            self.warning,
        ]
        .into_iter()
        .flatten()
        .reduce(Span::to)
    }

    /// An agent's or workflow's row.
    #[must_use]
    pub fn is_task(&self) -> bool {
        matches!(self.kind, RowKind::Agent | RowKind::Workflow)
    }
}

/// The panel as one locator placed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AgentsPanel {
    len: u8,
    rows: [PanelRow; PANEL_ROWS_MAX],
}

impl Default for AgentsPanel {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl AgentsPanel {
    /// A panel of no rows.
    pub const EMPTY: AgentsPanel = AgentsPanel {
        len: 0,
        rows: [PanelRow::NONE; PANEL_ROWS_MAX],
    };

    /// Its rows, top down.
    #[must_use]
    pub fn rows(&self) -> &[PanelRow] {
        &self.rows[..usize::from(self.len)]
    }

    /// Add `parsed` as pane row `row`.
    pub fn push(&mut self, row: u16, parsed: PanelRow) -> bool {
        let Some(slot) = self.rows.get_mut(usize::from(self.len)) else {
            return false;
        };
        *slot = PanelRow { row, ..parsed };
        self.len += 1;
        true
    }

    /// Whether it holds no row.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Its first pane row.
    #[must_use]
    pub fn top(&self) -> Option<u16> {
        self.rows().first().map(|r| r.row)
    }

    /// Its last pane row.
    #[must_use]
    pub fn bottom(&self) -> Option<u16> {
        self.rows().last().map(|r| r.row)
    }
}

/// The pane columns of byte offsets of one row, by the caller's [`Width`].
#[derive(Clone, Copy)]
struct Cols<'a> {
    row: &'a str,
    width: Width,
}

impl Cols<'_> {
    /// The column byte `at` (a char boundary) starts.
    fn at(&self, at: usize) -> u16 {
        let cols: usize = self.row[..at].chars().map(self.width).sum();
        u16::try_from(cols).unwrap_or(u16::MAX)
    }

    /// The columns of bytes `start..end`.
    fn span(&self, start: usize, end: usize) -> Span {
        let start_col = self.at(start);
        let len: usize = self.row[start..end].chars().map(self.width).sum();
        Span {
            start: start_col,
            end: start_col.saturating_add(u16::try_from(len).unwrap_or(u16::MAX)),
        }
    }
}

/// FNV-1a over `text`'s bytes.
#[must_use]
pub const fn text_hash(text: &str) -> u64 {
    let b = text.as_bytes();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < b.len() {
        h ^= b[i] as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    h
}

/// Read one row of the agents panel (module docs).
#[must_use]
pub fn parse_row(row: &str, width: Width) -> Option<PanelRow> {
    let cols = Cols { row, width };
    let end = crate::text::trim_end(row).len();
    let text = &row[..end];
    let (pointer, after_prefix) = if let Some(rest) = text.strip_prefix(POINTER) {
        (true, rest.strip_prefix(' ')?)
    } else {
        (false, text.strip_prefix("  ")?)
    };
    let mut at = end - after_prefix.len();
    let mut out = PanelRow {
        pointer: pointer.then(|| cols.span(0, POINTER.len_utf8())),
        ..PanelRow::NONE
    };
    if !pointer && let Some(more) = more_row(row, at, end, cols) {
        return Some(more);
    }
    let mut pairs = 0u8;
    while row[at..end].starts_with("  ") {
        pairs = pairs.saturating_add(1);
        at += 2;
    }
    let first = row[at..end].chars().next()?;
    if TREE.contains(&first) {
        let after = at + first.len_utf8();
        if !row[after..end].starts_with(' ') {
            return None;
        }
        out.connector = Some(cols.span(at, after));
        out.depth = pairs.saturating_add(1);
        at = after + 1;
    } else if pairs > 0 {
        return None;
    }
    let glyph = row[at..end].chars().next()?;
    let bullet = match glyph {
        '\u{25EF}' | '\u{25CB}' => Bullet::Hollow,
        '\u{23FA}' | '\u{25CF}' => Bullet::Filled,
        PAUSED => Bullet::Paused,
        _ => return None,
    };
    let after_bullet = at + glyph.len_utf8();
    out.bullet = Some((cols.span(at, after_bullet), bullet));
    if !row[after_bullet..end].starts_with(' ') {
        return None;
    }
    let body = after_bullet + 1;
    if body >= end {
        return None;
    }
    if bullet != Bullet::Paused && out.depth == 0 {
        if let Some(main) = main_row(row, body, end, cols, out) {
            return Some(main);
        }
        if let Some(idle) = idle_row(row, body, end, cols, out) {
            return Some(idle);
        }
    }
    task_row(row, body, end, cols, bullet, out)
}

/// `  ↓ N more` (the prefix's two spaces already passed at `at`).
fn more_row(row: &str, at: usize, end: usize, cols: Cols<'_>) -> Option<PanelRow> {
    let rest = row[at..end].strip_prefix(ARROW_DOWN)?.strip_prefix(' ')?;
    let n_at = end - rest.len();
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let words = &rest[digits..];
    if digits == 0 || words != crate::anchors::anchor_text("panel.more") {
        return None;
    }
    let n = rest[..digits].parse().ok()?;
    Some(PanelRow {
        kind: RowKind::More,
        name: cols.span(at, end),
        name_hash: text_hash(&row[at..end]),
        count: Some((cols.span(n_at, n_at + digits), n)),
        ..PanelRow::NONE
    })
}

/// The main row's body: `main`, alone or with `↑ N more` at the right.
fn main_row(row: &str, body: usize, end: usize, cols: Cols<'_>, out: PanelRow) -> Option<PanelRow> {
    let word = &crate::anchors::anchor_text("panel.main")[1..];
    let rest = row[body..end].strip_prefix(word)?;
    let name_end = body + word.len();
    let mut main = PanelRow {
        kind: RowKind::Main,
        name: cols.span(body, name_end),
        name_hash: text_hash(word),
        ..out
    };
    if rest.is_empty() {
        return Some(main);
    }
    if !rest.starts_with("  ") {
        return None;
    }
    let more = crate::text::trim_start(rest);
    let n = more.strip_prefix(ARROW_UP)?.strip_prefix(' ')?;
    let digits = n.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || &n[digits..] != crate::anchors::anchor_text("panel.more") {
        return None;
    }
    let n_at = end - n.len();
    main.count = Some((cols.span(n_at, n_at + digits), n[..digits].parse().ok()?));
    Some(main)
}

/// The idle summary's body: `N idle agent` or `N idle agents`.
fn idle_row(row: &str, body: usize, end: usize, cols: Cols<'_>, out: PanelRow) -> Option<PanelRow> {
    let text = &row[body..end];
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let words = text[digits..].strip_prefix(' ')?;
    let noun = crate::anchors::anchor_text("panel.idle_agents");
    if words != noun && words.strip_suffix('s') != Some(noun) {
        return None;
    }
    Some(PanelRow {
        kind: RowKind::IdleSummary,
        name: cols.span(body, end),
        name_hash: text_hash(text),
        count: Some((cols.span(body, body + digits), text[..digits].parse().ok()?)),
        ..out
    })
}

/// One segment of a status or a workflow's tail, read on its own.
#[derive(Debug, Clone, Copy)]
enum Seg {
    /// An elapsed time: what it reads as `formatDuration` (or a word) and as
    /// `formatBarElapsed` — `Ns` is both.
    Elapsed {
        duration: Option<Elapsed>,
        bar: Option<Elapsed>,
    },
    Tokens {
        tokens: Tokens,
        /// ` tokens` after the count (a narrow workflow row drops it).
        worded: bool,
    },
    Queued(u16),
    Progress(Progress),
    Warning,
}

/// `text` read as one segment.
fn segment(text: &str) -> Option<Seg> {
    let duration = duration(text).or_else(|| word_elapsed(text));
    let bar = bar_elapsed(text);
    if duration.is_some() || bar.is_some() {
        return Some(Seg::Elapsed { duration, bar });
    }
    if let Some((tokens, worded)) = tokens(text) {
        return Some(Seg::Tokens { tokens, worded });
    }
    if let Some(n) = queued(text) {
        return Some(Seg::Queued(n));
    }
    if let Some(p) = progress(text) {
        return Some(Seg::Progress(p));
    }
    warning(text).then_some(Seg::Warning)
}

/// Leading ASCII digits of `s` as a number, and the rest.
fn number(s: &str) -> Option<(u64, &str)> {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 12 {
        return None;
    }
    Some((s[..digits].parse().ok()?, &s[digits..]))
}

/// `formatDuration`: `Ns`, `Mm Ss`, `Hh Mm Ss` (one-second steps) or `Dd Hh
/// Mm` (one-minute steps), each lesser unit inside its range.
fn duration(text: &str) -> Option<Elapsed> {
    let mut parts = [(0u64, 0u8); 3];
    let mut n = 0;
    for token in text.split(' ') {
        let (value, unit) = number(token)?;
        let &[unit] = unit.as_bytes() else {
            return None;
        };
        *parts.get_mut(n)? = (value, unit);
        n += 1;
    }
    let (ms, res_ms) = match &parts[..n] {
        [(s, b's')] if *s < 60 => (s * 1000, 1000),
        [(m, b'm'), (s, b's')] if (1..60).contains(m) && *s < 60 => ((m * 60 + s) * 1000, 1000),
        [(h, b'h'), (m, b'm'), (s, b's')] if (1..24).contains(h) && *m < 60 && *s < 60 => {
            (((h * 60 + m) * 60 + s) * 1000, 1000)
        }
        [(d, b'd'), (h, b'h'), (m, b'm')] if *d > 0 && *h < 24 && *m < 60 => {
            (((d * 24 + h) * 60 + m) * 60_000, 60_000)
        }
        _ => return None,
    };
    Some(Elapsed::Clock { ms, res_ms })
}

/// `formatBarElapsed`: `Ns`, `MmSSs` (one-second steps), `HhMMm` (one
/// minute), `DdHHh` (one hour), the lesser unit two digits.
fn bar_elapsed(text: &str) -> Option<Elapsed> {
    let (major, rest) = number(text)?;
    let unit = *rest.as_bytes().first()?;
    let rest = &rest[1..];
    if rest.is_empty() {
        return (unit == b's' && major < 60).then_some(Elapsed::Clock {
            ms: major * 1000,
            res_ms: 1000,
        });
    }
    let b = rest.as_bytes();
    if b.len() != 3 || !b[0].is_ascii_digit() || !b[1].is_ascii_digit() {
        return None;
    }
    let minor = u64::from(b[0] - b'0') * 10 + u64::from(b[1] - b'0');
    let (ms, res_ms) = match (unit, b[2]) {
        (b'm', b's') if (1..60).contains(&major) && minor < 60 => {
            ((major * 60 + minor) * 1000, 1000)
        }
        (b'h', b'm') if (1..24).contains(&major) && minor < 60 => {
            ((major * 60 + minor) * 60_000, 60_000)
        }
        (b'd', b'h') if major > 0 && minor < 24 => ((major * 24 + minor) * 3_600_000, 3_600_000),
        _ => return None,
    };
    Some(Elapsed::Clock { ms, res_ms })
}

/// The status's words in place of an elapsed time.
fn word_elapsed(text: &str) -> Option<Elapsed> {
    use crate::anchors::anchor_text;
    if text == anchor_text("panel.awaiting") {
        Some(Elapsed::AwaitingApproval)
    } else if text == anchor_text("panel.idle") {
        Some(Elapsed::Idle)
    } else if text == anchor_text("panel.waiting") {
        Some(Elapsed::Waiting)
    } else {
        None
    }
}

/// `↓ 613.6k tokens`, `↑ 12 tokens`, or a narrow workflow row's bare `↓
/// 1.0m`: the count and whether ` tokens` follows it.
fn tokens(text: &str) -> Option<(Tokens, bool)> {
    let (used_tools, rest) = if let Some(rest) = text.strip_prefix(ARROW_DOWN) {
        (true, rest)
    } else {
        (false, text.strip_prefix(ARROW_UP)?)
    };
    let rest = rest.strip_prefix(' ')?;
    let words = crate::anchors::anchor_text("panel.tokens");
    let (figure, worded) = match rest.strip_suffix(words) {
        Some(figure) => (figure, true),
        None => (rest, false),
    };
    let (whole, after) = number(figure)?;
    let (count, quantum) = if after.is_empty() {
        (whole, 1)
    } else {
        let b = after.as_bytes();
        if b.len() != 3 || b[0] != b'.' || !b[1].is_ascii_digit() {
            return None;
        }
        let tenth = u64::from(b[1] - b'0');
        let unit: u64 = match b[2] {
            b'k' => 1_000,
            b'm' => 1_000_000,
            b'b' => 1_000_000_000,
            b't' => 1_000_000_000_000,
            _ => return None,
        };
        (whole * unit + tenth * (unit / 10), unit / 10)
    };
    Some((
        Tokens {
            count,
            quantum,
            used_tools,
        },
        worded,
    ))
}

/// `N queued`.
fn queued(text: &str) -> Option<u16> {
    let (n, words) = number(text)?;
    (words == crate::anchors::anchor_text("panel.queued"))
        .then(|| u16::try_from(n).ok())
        .flatten()
}

/// `done/total`, or an older build's `done/total agents done`.
fn progress(text: &str) -> Option<Progress> {
    let (done, rest) = number(text)?;
    let (total, words) = number(rest.strip_prefix('/')?)?;
    if !(words.is_empty() || PROGRESS_OLD.contains(&words)) || done > total {
        return None;
    }
    Some(Progress {
        done: u32::try_from(done).ok()?,
        total: u32::try_from(total).ok()?,
    })
}

/// `⚠ Large workflow` (the warning's ` · /workflows to stop` is read with
/// it, [`task_row`]), or the bare `⚠` of a narrow row.
fn warning(text: &str) -> bool {
    let Some(rest) = text.strip_prefix(WARNING) else {
        return false;
    };
    rest.is_empty()
        || rest
            .strip_prefix(' ')
            .is_some_and(|w| w == crate::anchors::anchor_text("panel.large_workflow"))
}

/// The run of bar cells `▰…▱` (or `█…░`) at the start of `text`: its filled
/// and total cells, its byte length, and whether it is Ghostty's.
fn bar_run(text: &str, width: Width) -> Option<(u8, u8, usize, bool)> {
    let mut chars = text.char_indices();
    let (_, first) = chars.next()?;
    let blocks = match first {
        '\u{25B0}' | '\u{25B1}' => false,
        '\u{2588}' | '\u{2591}' => true,
        _ => return None,
    };
    let (fill, empty) = if blocks {
        (BAR_FILL[1], BAR_EMPTY[1])
    } else {
        (BAR_FILL[0], BAR_EMPTY[0])
    };
    let (mut filled, mut cells, mut len, mut emptied) = (0usize, 0usize, 0usize, false);
    for (at, c) in text.char_indices() {
        if c == fill && !emptied {
            filled += width(c);
        } else if c == empty {
            emptied = true;
        } else {
            len = at;
            break;
        }
        cells += width(c);
        len = at + c.len_utf8();
    }
    if !(4..=40).contains(&cells) {
        return None;
    }
    Some((
        u8::try_from(filled).ok()?,
        u8::try_from(cells).ok()?,
        len,
        blocks,
    ))
}

/// The separator between a status's segments (`Ose`, ` \xB7 `).
const SEP: &str = " \u{00B7} ";

/// The most segments one row's status carries (progress, elapsed, tokens,
/// queued, the warning's two parts) with room to spare.
const SEGMENTS_MAX: usize = 8;

/// An agent's or a workflow's body from byte `body`: the name, its padding,
/// and the tail — the bar and the segments, or the activity and the status.
fn task_row(
    row: &str,
    body: usize,
    end: usize,
    cols: Cols<'_>,
    bullet: Bullet,
    out: PanelRow,
) -> Option<PanelRow> {
    let name_end = crate::text::find(&row[body..end], "  ").map_or(end, |k| body + k);
    let mut name_text = &row[body..name_end];
    let mut descendants = 0u16;
    if let Some(open) = name_text.rfind(" (+")
        && let Some(n) = name_text[open + 3..].strip_suffix(')')
        && let Ok(n) = n.parse::<u16>()
    {
        descendants = n;
        name_text = &name_text[..open];
    }
    let mut task = PanelRow {
        kind: RowKind::Agent,
        name: cols.span(body, body + name_text.len()),
        name_hash: text_hash(name_text),
        descendants,
        ..out
    };
    let tail =
        name_end + (row[name_end..end].len() - crate::text::trim_start(&row[name_end..end]).len());
    if tail >= end {
        if bullet == Bullet::Paused {
            task.kind = RowKind::Workflow;
        }
        return Some(task);
    }
    let mut seg_from = tail;
    if let Some((filled, cells, len, blocks)) = bar_run(&row[tail..end], cols.width) {
        task.kind = RowKind::Workflow;
        task.bar = Some(Bar {
            span: cols.span(tail, tail + len),
            filled,
            cells,
            blocks,
        });
        let after = tail + len;
        if after >= end {
            return Some(task);
        }
        if !row[after..end].starts_with("  ") {
            return None;
        }
        seg_from = after + 2;
    }
    let mut segs: [(usize, usize, Seg); SEGMENTS_MAX] = [(0, 0, Seg::Warning); SEGMENTS_MAX];
    let mut n = 0;
    let mut seg_end = end;
    let stop = crate::anchors::anchor_text("panel.workflows_stop");
    if let Some(head) = row[seg_from..seg_end]
        .strip_suffix(stop)
        .and_then(|h| h.strip_suffix(SEP))
        && let Some(k) = head.rfind(SEP)
        && warning(&head[k + SEP.len()..])
    {
        segs[0] = (seg_from + k + SEP.len(), seg_end, Seg::Warning);
        n = 1;
        seg_end = seg_from + k;
    }
    while n < SEGMENTS_MAX && seg_end > seg_from {
        let piece = &row[seg_from..seg_end];
        let Some(k) = piece.rfind(SEP) else {
            break;
        };
        let start = seg_from + k + SEP.len();
        let Some(seg) = segment(&row[start..seg_end]) else {
            break;
        };
        segs[n] = (start, seg_end, seg);
        n += 1;
        seg_end = seg_from + k;
    }
    let mut activity_end = seg_end;
    if n < SEGMENTS_MAX && seg_end > seg_from {
        let piece = &row[seg_from..seg_end];
        let b = piece.as_bytes();
        let found = (0..b.len())
            .filter(|&k| piece.is_char_boundary(k) && b[k] != b' ' && (k == 0 || b[k - 1] == b' '))
            .find_map(|k| segment(&piece[k..]).map(|seg| (k, seg)));
        if let Some((k, seg)) = found {
            segs[n] = (seg_from + k, seg_end, seg);
            n += 1;
            activity_end = seg_from + k;
        } else if n > 0 {
            return Some(PanelRow {
                kind: if task.bar.is_some() || bullet == Bullet::Paused {
                    RowKind::Workflow
                } else {
                    RowKind::Agent
                },
                ..task
            });
        }
    }
    let activity_text = crate::text::trim_end(&row[seg_from..activity_end]);
    if task.bar.is_none() && !activity_text.is_empty() {
        task.activity = Some(cols.span(seg_from, seg_from + activity_text.len()));
        task.activity_hash = text_hash(activity_text);
    }
    let segs = &mut segs[..n];
    segs.reverse();
    let bar_only = segs.iter().any(|s| {
        matches!(
            s.2,
            Seg::Elapsed {
                duration: None,
                bar: Some(_)
            }
        )
    });
    let workflow = task.bar.is_some()
        || bullet == Bullet::Paused
        || bar_only
        || segs
            .iter()
            .any(|s| matches!(s.2, Seg::Progress(_) | Seg::Warning));
    if workflow {
        task.kind = RowKind::Workflow;
        if workflow_order(segs) {
            for &(start, stop, seg) in segs.iter() {
                let span = cols.span(start, stop);
                match seg {
                    Seg::Progress(p) => task.progress = Some((span, p)),
                    Seg::Elapsed { duration, bar } => {
                        task.elapsed = bar.or(duration).map(|e| (span, e));
                    }
                    Seg::Tokens { tokens, .. } => task.tokens = Some((span, tokens)),
                    Seg::Warning => task.warning = Some(span),
                    Seg::Queued(_) => {}
                }
            }
        }
        return Some(task);
    }
    if agent_order(segs) {
        for &(start, stop, seg) in segs.iter() {
            let span = cols.span(start, stop);
            match seg {
                Seg::Elapsed { duration, .. } => task.elapsed = duration.map(|e| (span, e)),
                Seg::Tokens { tokens, .. } => task.tokens = Some((span, tokens)),
                Seg::Queued(q) => task.queued = Some((span, q)),
                Seg::Progress(_) | Seg::Warning => {}
            }
        }
    } else {
        task.activity = Some(cols.span(tail, end));
        task.activity_hash = text_hash(&row[tail..end]);
    }
    Some(task)
}

/// An agent's status in its order: `elapsed`, then `↓ N tokens`, then `N
/// queued`.
fn agent_order(segs: &[(usize, usize, Seg)]) -> bool {
    let mut rank = 0;
    for s in segs {
        let r = match s.2 {
            Seg::Elapsed {
                duration: Some(_), ..
            } => 1,
            Seg::Tokens { worded: true, .. } => 2,
            Seg::Queued(_) => 3,
            _ => return false,
        };
        if r <= rank {
            return false;
        }
        rank = r;
    }
    rank > 0 && matches!(segs.first().map(|s| s.2), Some(Seg::Elapsed { .. }))
}

/// A workflow's tail in its order: `done/total`, elapsed, tokens, the
/// warning — any of them left out.
fn workflow_order(segs: &[(usize, usize, Seg)]) -> bool {
    let mut rank = 0;
    for s in segs {
        let r = match s.2 {
            Seg::Progress(_) => 1,
            Seg::Elapsed { .. } => 2,
            Seg::Tokens { .. } => 3,
            Seg::Warning => 4,
            Seg::Queued(_) => return false,
        };
        if r <= rank {
            return false;
        }
        rank = r;
    }
    true
}

#[cfg(test)]
#[path = "agents_panel_tests.rs"]
mod tests;

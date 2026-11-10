// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The quota view (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §2, §5.1): line E's quota
//! row and the Accounts card's chart and lanes, painted from one [`Outlook`].
//!
//! It is pure. Every instant it says comes from the outlook (its `as_of`, its passes, its rows'
//! readings), and nothing here reads a clock, a file or a setting.
//!
//! [`Outlook::quota`] builds a [`QuotaView`]:
//! - the named accounts, at most three, in roster order, this tab's and the carrier always among
//!   them, the rest folded and never dropped ([`Named`], §2.5);
//! - one rolling chart over [now − 1 d, now + 7 d) ([`Chart`], §2.2): this tab's account as read
//!   on this Mac, `▕` for now, then the projected fill of whichever account carries the work,
//!   with its milestones;
//! - one burn-up lane per named account for the card ([`Burn`]);
//! - the tail's two phrases ([`Tail`], §2.3), the hover title and the spoken words (§2.6).
//!
//! [`QuotaView::lay`] fits the row to a room by the ladder of §2.4. [`QuotaView::fallback`] gives
//! line D's words-only unit, for a pane whose line E is not the glass's (§1.2, §5.2).

use std::ops::Range;

use super::accounts::{self, Class};
use super::outlook::{
    self, Basis, Figure, Knot, Offsets, Outlook, Pass, PassKind, Read, RowOutlook, Stop, StopEnd,
    Verdict, Window, round_down, round_up, span_dated, spoken_when, when,
};

const DAY: i64 = 86_400;
const WEEK: i64 = accounts::WEEK_WINDOW_S;

/// The chart's grains, finest first, in cells a day (§2.2): 2 to 8 hours a cell. Twelve is the
/// calm budget, and no rung is finer.
pub const PER_DAY: [u8; 5] = [12, 8, 6, 4, 3];
/// The most accounts the row names (§2.5); the others fold into `+N` and are never dropped.
pub const NAMED_MAX: usize = 3;
/// A name is cut to this many cells, its last an ellipsis (§2.5). A name is an account's email
/// (the roster label where its row has none, [`accounts::Row::shown_name`]), and 32 cells hold
/// a typical one whole; a longer email gives way to its local part before it is cut ([`cut`]).
pub const NAME_CELLS: usize = 32;
/// A name's narrow form ([`short`]) is cut to this many cells: an email's local part, or a
/// label, as the glass cut every name before names were emails.
pub const SHORT_CELLS: usize = 12;
/// The blanks between the row's groups (§4.3).
pub const GROUP_GAP: usize = 2;
/// Case (d)'s short words (§1.3): this tab's week has no figure yet.
pub const NO_FIGURE: &str = "no week figure yet";
/// Case (d)'s full words.
pub const NO_FIGURE_FULL: &str = "no week figure yet \u{b7} Claude reports it with its next reply";
/// The name of a login the roster cannot name (§2.5).
pub const THIS_LOGIN: &str = "this login";
/// The title's last words: what a click on the row does (§2.6).
pub const CLICK: &str = "click: accounts";

/// `▕` (U+2595): the now cell, between the measured past and the projected future.
pub const NOW_MARK: char = '\u{2595}';
/// `╳` (U+2573): a projected gap, every account out.
pub const GAP_MARK: char = '\u{2573}';
/// `→`: the work moves to another account.
pub const HANDOFF_MARK: char = '\u{2192}';
/// `↺` (U+21BA): a vendor reset.
pub const RESET_MARK: char = '\u{21ba}';
/// `$`: the work spends usage credits.
pub const CREDITS_MARK: char = '$';
/// `┄` (U+2504): the future is not projected (an unread account the chain reaches, or no pace).
pub const UNKNOWN_MARK: char = '\u{2504}';
/// `⧗`: the work waits at a limit now.
pub const WAIT_MARK: char = '\u{29d7}';
/// A drawn row's places a cell ([`Chart::midnights_at`]): a midnight's place is its offset from
/// the row's left edge in 86 400ths of a cell, so every grain of §2.2 (2 to 8 hours a cell) holds
/// it to the second, and a renderer `cw` pixels a cell puts it at `place · cw / CELL_PLACES`.
pub const CELL_PLACES: i64 = DAY;

const SEP: &str = " \u{b7} ";

/// The quota view of one pane ([`Outlook::quota`]): what line E and the Accounts card draw. It
/// hashes into the footer's fingerprint; `as_of` is the outlook's instant, never a raw clock.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct QuotaView {
    /// The outlook's instant.
    pub as_of: i64,
    /// The named accounts, at most [`NAMED_MAX`], in roster order.
    pub named: Vec<Named>,
    /// Every other roster row's label, in roster order: folded into `+N`, never dropped.
    pub folded: Vec<String>,
    /// Line E's chart and the card's work row.
    pub chart: Chart,
    /// The card's lanes: one burn-up per named roster account.
    pub lanes: Vec<Burn>,
    /// The tail's two phrases.
    pub tail: Tail,
    /// The hover title of the lead and the tail.
    pub title: String,
    /// What a reader hears: no box, block, legacy-computing or braille glyph.
    pub spoken: String,
}

/// A named account (§2.5).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Named {
    /// Its roster label ([`THIS_LOGIN`] for a login the roster cannot name), uncut: its key.
    pub label: String,
    /// What the glass names it by, uncut: its email, else its label ([`Outlook::name_of`]).
    pub name: String,
    /// It is this tab's account.
    pub this_tab: bool,
    /// It carries the work now.
    pub carrier: bool,
    /// Its week's figure.
    pub figure: Figure,
    /// Its week's vendor reset, where one is ahead.
    pub reset: Option<i64>,
    /// Its plan's title (`Max 20x`), where seen.
    pub plan: Option<String>,
}

/// The rolling chart over [now − 1 d, now + 7 d) (§2.2), drawn by [`Chart::cells`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Chart {
    /// Now: the `▕` between the past and the future.
    pub now: i64,
    /// This tab's account's week as read on this Mac, oldest first: the last reading before the
    /// span, those inside it, and the lead's own figure at now.
    pub past: Vec<Read>,
    /// The work's accounts, in order: whichever carries it, with its fill.
    pub runs: Vec<Run>,
    /// The stops: each one the passes project (a lean's where both its branches meet it) and a
    /// wall the pane waits at now, a stop from now until it ends: from, to (the chart's end past
    /// it for one nothing ends), and whether usage credits carry the work through it.
    pub stops: Vec<(i64, i64, bool)>,
    /// The milestones, in time order.
    pub marks: Vec<(i64, Mark)>,
    /// From here the future is not projected (`┄`): a lean, a cut replay, or no projection.
    pub unknown_from: Option<i64>,
    /// Each local midnight inside the span: the instant each local day begins, for the ticks of
    /// line E's chart and the card's work row and lanes ([`Chart::midnights_at`]).
    pub midnights: Vec<i64>,
}

/// One account carrying the work over `[from, to]`, its fill of its own week at its knots.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Run {
    /// The account.
    pub label: String,
    /// What the glass names it by ([`Outlook::name_of`]).
    pub name: String,
    /// From.
    pub from: i64,
    /// To.
    pub to: i64,
    /// Its fill: at `from` and `to`, and wherever it changes course between; linear between,
    /// and a drop (its reset) is two knots at one instant.
    pub knots: Vec<Knot>,
}

impl Run {
    /// Its fill at `t`, in tenths: from the left within `(from, to]` (before a drop at `t`), from
    /// the right at `from`; `None` outside the run or where the replay does not know it.
    #[must_use]
    pub fn at(&self, t: i64) -> Option<u16> {
        if t < self.from || t > self.to {
            return None;
        }
        fill_at(&self.knots, t, t > self.from).flatten()
    }
}

/// A milestone on the chart.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    /// `↺`: a vendor reset of the account the chart draws at that instant.
    Reset,
    /// `→name`: the work moves to this account, as the glass names it ([`Outlook::name_of`]).
    Handoff(String),
    /// `→name?`: the work moves to this account (its glass name), which aterm cannot read; `┄`
    /// follows.
    Lean(String),
}

/// One cell of a chart or a lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cell {
    /// Nothing to draw: before the first reading, across an epoch.
    Blank,
    /// A measured level, 1 to 8: `▁…█` (U+2581–2588), an area.
    Read(u8),
    /// A projected level, 1 to 8: a one-eighth line, `▁ 🭻 🭺 🭹 🭸 🭷 🭶 ▔`.
    Ahead(u8),
    /// `▕`, the now cell.
    Now,
    /// `╳`, a stop.
    Gap,
    /// `→`, a hand-off.
    Handoff,
    /// `↺`, a vendor reset.
    Reset,
    /// `$`, usage credits.
    Credits,
    /// `┄`, not projected.
    Unknown,
    /// One character of the name after a `→`.
    Name(char),
    /// A lane's week out: the projected line at the lane's ceiling ([`LANE_TOP`], `🭷`) in the
    /// warn ink, so out never rests on the line's height alone.
    Out,
}

impl Cell {
    /// Its glyph (§1.3, §2.2): every block and line lies in the renderer's procedural set.
    #[must_use]
    pub fn glyph(self) -> char {
        match self {
            Cell::Blank => ' ',
            Cell::Read(l) => char::from_u32(0x2580 + u32::from(l.clamp(1, 8))).unwrap_or(' '),
            Cell::Ahead(l) => match l {
                0 | 1 => '\u{2581}',
                8.. => '\u{2594}',
                l => char::from_u32(0x1fb7b - (u32::from(l) - 2)).unwrap_or(' '),
            },
            Cell::Now => NOW_MARK,
            Cell::Gap => GAP_MARK,
            Cell::Handoff => HANDOFF_MARK,
            Cell::Reset => RESET_MARK,
            Cell::Credits => CREDITS_MARK,
            Cell::Unknown => UNKNOWN_MARK,
            Cell::Name(c) => c,
            Cell::Out => Cell::Ahead(LANE_TOP).glyph(),
        }
    }

    /// Its ink (§1.3).
    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            Cell::Blank | Cell::Now => Role::Quiet,
            Cell::Read(_) => Role::Mark,
            Cell::Ahead(_) => Role::Line,
            Cell::Gap | Cell::Credits | Cell::Out => Role::Warn,
            Cell::Handoff | Cell::Reset => Role::Lit,
            Cell::Unknown => Role::Track,
            Cell::Name(_) => Role::Value,
        }
    }

    /// A midnight's tick may stand in it on line E's chart or the card's work row (§2.2), and on
    /// a lane but where it is blank ([`Cell::lane_ticks`]): it draws a level (a measured step,
    /// the projected line, a lane's out line), `┄` or nothing, never a letter or a mark (a name,
    /// `→`, `↺`, `╳`, `$`, `▕`), which a line of ink would strike through.
    #[must_use]
    pub const fn ticks(self) -> bool {
        matches!(
            self,
            Cell::Blank | Cell::Read(_) | Cell::Ahead(_) | Cell::Unknown | Cell::Out
        )
    }

    /// A midnight's tick may stand in it on a card lane (§1.4): only where the lane draws (a
    /// measured step, the projected line, its out line, `┄`). A lane's blank cell is the card's
    /// ground, before its week's first reading on this Mac, as the ground after its `↺` is; a
    /// tick there marks no day on any line and stands alone over the next item's words.
    #[must_use]
    pub const fn lane_ticks(self) -> bool {
        !matches!(self, Cell::Blank) && self.ticks()
    }
}

/// One account's own vendor week for the card: read, then projected, ending at its reset.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Burn {
    /// The account.
    pub label: String,
    /// What the glass names it by ([`Outlook::name_of`]).
    pub name: String,
    /// Its week began (`renews` − 7 d).
    pub start: i64,
    /// Its week resets; at or before now where no reset is known ahead (nothing projected).
    pub renews: i64,
    /// Its week's readings on this Mac, oldest first.
    pub read: Vec<Read>,
    /// The replay's fill of it from now to `renews`.
    pub ahead: Vec<Knot>,
    /// It is out from here to its reset ([`Cell::Out`]).
    pub out_at: Option<i64>,
}

/// A part's ink; the GUI maps each to a `GlassInks` token. A warn part is always bold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// This tab's name and figure, `→`, `↺`.
    Lit,
    /// Other names and figures, times, names inside the chart.
    Value,
    /// `used`, `·`, `+N`, `by`, `▕`, case (d)'s words.
    Quiet,
    /// The measured levels.
    Mark,
    /// The projected line.
    Line,
    /// `╳`, `⧗`, `$` and their words.
    Warn,
    /// `┄`.
    Track,
}

/// Line E as cells ([`QuotaView::lay`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct QuotaLine {
    /// Each cell: its character, its ink, bold.
    pub cells: Vec<(char, Role, bool)>,
    /// The lead's cells.
    pub lead: Range<usize>,
    /// The chart's cells (`8k + 1`, empty at the narrowest rung).
    pub chart: Range<usize>,
    /// The tail's cells.
    pub tail: Range<usize>,
    /// The chart's cells a day (0 where none is drawn), for [`Chart::span_of`].
    pub per_day: u8,
    /// The midnights' ticks over the chart ([`Chart::ticks`]): each one's place from the chart's
    /// left edge ([`CELL_PLACES`] a cell), in a cell that takes one.
    pub ticks: Vec<i64>,
}

impl QuotaLine {
    /// Its text.
    #[must_use]
    pub fn text(&self) -> String {
        self.cells.iter().map(|(c, _, _)| *c).collect()
    }

    /// The text of `range`.
    #[must_use]
    pub fn text_of(&self, range: &Range<usize>) -> String {
        self.cells
            .get(range.clone())
            .unwrap_or_default()
            .iter()
            .map(|(c, _, _)| *c)
            .collect()
    }

    fn clear(&mut self) {
        self.cells.clear();
        self.lead = 0..0;
        self.chart = 0..0;
        self.tail = 0..0;
        self.per_day = 0;
        self.ticks.clear();
    }

    fn put(&mut self, text: &str, role: Role, bold: bool) {
        self.cells
            .extend(text.chars().map(|c| (c, role, bold || heavy(c, role))));
    }

    fn gap(&mut self) {
        for _ in 0..GROUP_GAP {
            self.cells.push((' ', Role::Quiet, false));
        }
    }
}

/// The tail (§2.3): at most two phrases.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tail {
    /// Phrase 1, this tab's account: `~99% by ↺ Sat 10:00`, short `↺ Sat 10:00`. None where no
    /// reset is ahead, or where the work waits at a wall until this very reset (phrase 2's
    /// `⧗ until Sat 10:00` says it).
    pub own: Option<Own>,
    /// Phrase 2: the verdict, or case (d)'s words.
    pub verdict: Option<Phrase>,
}

/// Phrase 1 (§2.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Own {
    /// The curve at the reset, whole percent half up, at most 99 (case (b)'s 98.875% is `~99%`):
    /// none where the account fills or is left before its reset, or nothing is projected (the
    /// full form is the short one).
    pub by_reset: Option<u16>,
    /// The reset's words (`Sat 10:00`), exact.
    pub reset: String,
}

/// Phrase 2 (§2.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Phrase {
    /// The full form (`╳ ~Thu 12 Nov 04:00–Sat 14 Nov 10:00`).
    pub full: String,
    /// The short form (`╳ ~Thu 12 Nov 04:00`).
    pub short: String,
    /// Its ink: warn for `╳`, `⧗`, `$`.
    pub ink: Role,
    /// A stop, a wall, a lean or credits: it stays, at least short, while any figure shows.
    pub kept: bool,
}

/// One run of a fallback unit's words with its ink ([`QuotaView::fallback`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Piece {
    /// The words.
    pub text: String,
    /// Their ink.
    pub role: Role,
    /// Bold (a warn piece and the reset mark always are: [`heavy`]).
    pub bold: bool,
}

impl Piece {
    fn new(text: impl Into<String>, role: Role) -> Piece {
        let text = text.into();
        let bold = text.chars().any(|c| heavy(c, role));
        Piece { text, role, bold }
    }
}

/// The Accounts card's verdict line ([`Outlook::card_verdict`], §1.4).
pub(super) struct CardVerdict {
    /// Its forms, widest first.
    pub(super) forms: Vec<String>,
    /// What a reader hears: no drawing glyph.
    pub(super) spoken: String,
    /// Its ink: warn behind `╳`, `⧗` or `$`, value, or quiet for case (d)'s words.
    pub(super) ink: Role,
    /// A lean's worst case, where it stops: its forms (indented under the verdict) and what a
    /// reader hears.
    pub(super) worst: Option<(Vec<String>, String)>,
}

/// Whether a glyph of the glass is laid bold whatever its phrase says: every warn glyph, and
/// `↺`, whose font glyph has so little ink at 1x that, laid regular, the milestone §1.3 inks
/// `lit` reads fainter than the quiet `·` beside it.
fn heavy(c: char, role: Role) -> bool {
    role == Role::Warn || c == RESET_MARK
}

/// The lead forms of §2.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lead {
    /// Every named account with its figure, then `+N` for the folded rows.
    Full,
    /// This tab's name and figure, then `+N` for every other row.
    Fold,
    /// This tab's name and figure alone.
    Tab,
}

/// The tail forms of §2.3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TailForm {
    /// Phrase 1 full · phrase 2 full.
    T0,
    /// Phrase 1 short · phrase 2 short.
    T1,
    /// Phrase 2 short alone, else phrase 1 short.
    T2,
}

/// How a rung names its accounts (§2.5): each email whole ([`cut`]), or its local part
/// ([`short`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Names {
    /// The email, whole where it fits [`NAME_CELLS`].
    Whole,
    /// The email's local part.
    Local,
}

/// The ladder of §2.4, widest first: the lead, how it names, the chart's cells a day (0, none)
/// and the tail. The names give way to their local parts before the lead folds, the other
/// accounts' figures being worth more than their domains, and stay so on every narrower rung,
/// so each rung's parts are a subset of the rung before.
const LADDER: [(Lead, Names, u8, TailForm); 11] = [
    (Lead::Full, Names::Whole, 12, TailForm::T0),
    (Lead::Full, Names::Whole, 8, TailForm::T0),
    (Lead::Full, Names::Whole, 8, TailForm::T1),
    (Lead::Full, Names::Whole, 6, TailForm::T1),
    (Lead::Full, Names::Local, 6, TailForm::T1),
    (Lead::Fold, Names::Local, 6, TailForm::T1),
    (Lead::Fold, Names::Local, 4, TailForm::T1),
    (Lead::Fold, Names::Local, 4, TailForm::T2),
    (Lead::Fold, Names::Local, 3, TailForm::T2),
    (Lead::Tab, Names::Local, 3, TailForm::T2),
    (Lead::Tab, Names::Local, 0, TailForm::T2),
];

impl Names {
    /// `name` as this form lays it.
    fn lay(self, name: &str) -> String {
        match self {
            Names::Whole => cut(name),
            Names::Local => short(name),
        }
    }
}

/// The level of `x10` tenths used (§2.2): `clamp(round(x·8/100), 1, 7)`, and 8 only at 1000 (a
/// reading of 100, or a projected fill), so 63% and 98% are levels 5 and 7.
#[must_use]
pub fn level(x10: u16) -> u8 {
    if x10 >= 1000 {
        8
    } else {
        u8::try_from(((u32::from(x10) * 8 + 500) / 1000).clamp(1, 7)).unwrap_or(7)
    }
}

/// A lane's ceiling: the highest of its levels, its week out ([`Cell::Out`]). A lane sits
/// right under its item's words, so it draws on six of the cell's eight steps and keeps the
/// top quarter of the cell clear (two device pixels or more at any cell 8 px tall): neither
/// its line nor a full week's reading reads as the words' underline.
pub const LANE_TOP: u8 = 6;

/// The lane level of `x10` tenths used ([`LANE_TOP`]): `clamp(round(x·5/100), 1, 5)`, and the
/// ceiling only at 1000, so 63% is level 3 and 98% level 5.
#[must_use]
pub fn lane_level(x10: u16) -> u8 {
    if x10 >= 1000 {
        LANE_TOP
    } else {
        u8::try_from(((u32::from(x10) * 5 + 500) / 1000).clamp(1, 5)).unwrap_or(5)
    }
}

/// A name as the glass lays it in at most [`NAME_CELLS`] cells: whole where it fits; an email
/// that does not gives way to its local part ([`local`]) before anything is cut; what is still
/// too wide is cut, its last cell an ellipsis.
#[must_use]
pub fn cut(name: &str) -> String {
    if aterm_grapheme::str_grid_columns(name) <= NAME_CELLS {
        return name.to_owned();
    }
    cells(local(name), NAME_CELLS)
}

/// A name's narrow form (§2.4, §2.5): an email's local part, the words before its `@`, cut to
/// [`SHORT_CELLS`]; any other name cut so.
#[must_use]
pub fn short(name: &str) -> String {
    cells(local(name), SHORT_CELLS)
}

/// An email's local part (`owner` of `owner@example.com`); a name with no `@` past its first
/// character whole.
fn local(name: &str) -> &str {
    match name.split_once('@') {
        Some((head, _)) if !head.is_empty() => head,
        _ => name,
    }
}

/// `name` cut to at most `max` cells, its last an ellipsis, between whole grapheme clusters: an
/// accent stays on its letter and a joined emoji goes whole or not at all.
fn cells(name: &str, max: usize) -> String {
    if aterm_grapheme::str_grid_columns(name) <= max {
        return name.to_owned();
    }
    aterm_grapheme::grid_graphemes(name)
        .take_while(|cluster| cluster.end_col < max)
        .map(|cluster| cluster.text)
        .chain(std::iter::once("\u{2026}"))
        .collect()
}

/// Whole percent, half up, at most 99: a curve under 100 is never said `100%`.
fn whole_under_full(x10: u16) -> u16 {
    ((x10.min(1000) + 5) / 10).min(99)
}

/// The fill of `knots` at `t`, linear between them: from the left (`left`, before a drop at `t`)
/// or from the right (after it). `None` outside the knots; `Some(None)` where unknown.
fn fill_at(knots: &[Knot], t: i64, left: bool) -> Option<Option<u16>> {
    let (first, last) = (knots.first()?, knots.last()?);
    if t < first.t || t > last.t {
        return None;
    }
    let lo = knots.partition_point(|k| k.t < t);
    let hi = knots.partition_point(|k| k.t <= t);
    if lo < hi {
        return Some(if left {
            knots[lo].x10
        } else {
            knots[hi - 1].x10
        });
    }
    let (a, b) = (knots[lo - 1], knots[lo]);
    Some(match (a.x10, b.x10) {
        (Some(x), Some(y)) => {
            let span = i128::from(b.t - a.t).max(1);
            let x = i128::from(x);
            let v = x + (i128::from(y) - x) * i128::from(t - a.t) / span;
            u16::try_from(v.clamp(0, 1000)).ok()
        }
        _ => None,
    })
}

/// A cell index in `0..n` of the future cells of length `s` from `now` holding `t`.
fn future_index(now: i64, s: i64, n: usize, t: i64) -> Option<usize> {
    if t < now {
        return None;
    }
    usize::try_from((t - now) / s).ok().filter(|j| *j < n)
}

impl Chart {
    /// The chart at `per_day` cells a day (§2.2), into `out` (cleared first): `per_day` past
    /// cells, the `▕` cell, then `7 · per_day` future cells; none at 0.
    ///
    /// A past cell is the last reading at or before its end in the same window (a step, never
    /// interpolated), blank before the first reading and across an epoch, `↺` where a reset falls
    /// inside it. A future cell shows one glyph by priority, `╳` > `→` > `↺` > `$` > `┄` > level:
    /// `╳` where a stop covers at least half of it (a stop that covers no cell that much takes
    /// the cell that holds its start); `→` where a hand-off falls inside it, moved to the first
    /// cell after a gap that displaces it, the target's name after it in the widest of its forms
    /// ([`cut`], else [`short`]) the run up to the next mark holds with one cell more; `↺` at a reset; `$` under credits; `┄` past
    /// [`Chart::unknown_from`]; else the drawn account's fill at the cell's end.
    pub fn cells(&self, per_day: u8, out: &mut Vec<Cell>) {
        out.clear();
        if per_day == 0 {
            return;
        }
        let k = i64::from(per_day);
        let s = DAY / k;
        for i in 0..k {
            let a = self.now - DAY + i * s;
            let b = if i + 1 == k { self.now } else { a + s };
            out.push(self.past_cell(a, b));
        }
        out.push(Cell::Now);
        let n = usize::from(per_day) * 7;
        let first = out.len();
        let end = |j: usize| self.now + (i64::try_from(j).unwrap_or(0) + 1) * s;
        out.extend((0..n).map(|j| self.level_cell(end(j))));
        let mut gap = vec![false; n];
        let mut paid = vec![false; n];
        for &(from, to, credits) in &self.stops {
            self.cover(s, from, to, if credits { &mut paid } else { &mut gap });
        }
        let mut arrow: Vec<Option<(&str, bool)>> = vec![None; n];
        let mut reset = vec![false; n];
        for (t, mark) in &self.marks {
            let Some(mut j) = future_index(self.now, s, n, *t) else {
                continue;
            };
            match mark {
                Mark::Reset => reset[j] = true,
                Mark::Handoff(name) | Mark::Lean(name) => {
                    // A hand-off a gap displaces moves to the first cell after it.
                    while j < n && gap[j] {
                        j += 1;
                    }
                    if j < n {
                        arrow[j] = Some((name.as_str(), matches!(mark, Mark::Lean(_))));
                    }
                }
            }
        }
        let future = &mut out[first..];
        for j in 0..n {
            future[j] = if gap[j] {
                Cell::Gap
            } else if arrow[j].is_some() {
                Cell::Handoff
            } else if reset[j] {
                Cell::Reset
            } else if paid[j] {
                Cell::Credits
            } else {
                future[j]
            };
        }
        for j in 0..n {
            let Some((name, lean)) = &arrow[j] else {
                continue;
            };
            let room = future[j + 1..]
                .iter()
                .take_while(|c| matches!(c, Cell::Ahead(_) | Cell::Blank | Cell::Unknown))
                .count();
            // The name whole where it fits, else its narrow form (an email's local part,
            // §2.5): an email seldom fits between two marks, and an arrow with no name says
            // nothing of where the work goes.
            let fits = |name: &String| room > name.chars().count() + usize::from(*lean);
            if let Some(name) = [cut(name), short(name)].iter().find(|name| fits(name)) {
                let tail = lean.then_some('?');
                for (cell, c) in future[j + 1..].iter_mut().zip(name.chars().chain(tail)) {
                    *cell = Cell::Name(c);
                }
            }
        }
    }

    /// The span `[from, to)` of cell `cell` at `per_day` cells a day; `None` for the now cell
    /// and past the chart's end.
    #[must_use]
    pub fn span_of(&self, per_day: u8, cell: usize) -> Option<(i64, i64)> {
        if per_day == 0 {
            return None;
        }
        let k = usize::from(per_day);
        let s = DAY / i64::from(per_day);
        let i = i64::try_from(cell).ok()?;
        if cell < k {
            let a = self.now - DAY + i * s;
            Some((a, if cell + 1 == k { self.now } else { a + s }))
        } else if cell == k || cell > 8 * k {
            None
        } else {
            let j = i - i64::try_from(k).ok()? - 1;
            Some((self.now + j * s, self.now + (j + 1) * s))
        }
    }

    /// Each local midnight inside the chart at `per_day` cells a day ([`Chart::cells`]), as its
    /// cell and its exact place from the chart's left edge ([`CELL_PLACES`] a cell): a past cell
    /// spans `1/per_day` of the day before `▕`, a future one as much after it, and `▕` holds no
    /// time. Where the clocks spring forward over midnight the day begins at the change
    /// ([`Chart::midnights`]). None at 0.
    pub fn midnights_at(&self, per_day: u8) -> impl Iterator<Item = (usize, i64)> + '_ {
        let k = i64::from(per_day);
        self.midnights.iter().filter_map(move |&t| {
            let place = if t < self.now {
                (t - (self.now - DAY)) * k
            } else {
                (k + 1) * CELL_PLACES + (t - self.now) * k
            };
            let cell = place.div_euclid(CELL_PLACES);
            (k > 0 && place >= 0 && cell < 8 * k + 1)
                .then(|| usize::try_from(cell).ok().map(|cell| (cell, place)))
                .flatten()
        })
    }

    /// The midnights' ticks (§2.2) on a row whose cells from its `lead`-th are `cells`, drawn at
    /// `per_day` cells a day on this chart's clock (the chart itself, or a card lane on it,
    /// [`Burn::cells`]), into `out` (cleared first): each midnight's place from the row's left
    /// edge ([`Chart::midnights_at`]) where its cell takes a tick by `takes` ([`Cell::ticks`] on
    /// the chart and the work row, [`Cell::lane_ticks`] on a lane), so none past the row's last
    /// cell (a lane ends at its reset).
    pub fn ticks(
        &self,
        per_day: u8,
        (lead, takes): (usize, fn(Cell) -> bool),
        cells: &[Cell],
        out: &mut Vec<i64>,
    ) {
        out.clear();
        let lead = i64::try_from(lead).unwrap_or(0) * CELL_PLACES;
        out.extend(
            self.midnights_at(per_day)
                .filter(|&(cell, _)| cells.get(cell).is_some_and(|&c| takes(c)))
                .map(|(_, place)| lead + place),
        );
    }

    /// Marks into `into` each future cell a stop `[from, to)` covers at least half of, or, where
    /// it covers none that much, the cell that holds its start.
    fn cover(&self, s: i64, from: i64, to: i64, into: &mut [bool]) {
        let mut any = false;
        for (j, slot) in into.iter_mut().enumerate() {
            let a = self.now + i64::try_from(j).unwrap_or(0) * s;
            let over = to.min(a + s) - from.max(a);
            if over > 0 && over * 2 >= s {
                *slot = true;
                any = true;
            }
        }
        if !any
            && to > self.now
            && let Some(j) = future_index(self.now, s, into.len(), from.max(self.now))
        {
            into[j] = true;
        }
    }

    fn past_cell(&self, a: i64, b: i64) -> Cell {
        let starts = |r: &Read| r.epoch.unwrap_or(r.reset - WEEK);
        if self
            .past
            .iter()
            .any(|r| (a..b).contains(&r.reset) || (a..b).contains(&starts(r)))
        {
            return Cell::Reset;
        }
        let i = self.past.partition_point(|r| r.t <= b);
        let Some(r) = i.checked_sub(1).and_then(|i| self.past.get(i)) else {
            return Cell::Blank;
        };
        // Its window ended before the cell, or a later window began between it and the cell's
        // end (one that begins at the end is the next cell's): nothing is known of the cell.
        let later = self.past[i..]
            .iter()
            .any(|n| (n.reset, n.epoch) != (r.reset, r.epoch) && starts(n) > r.t && starts(n) < b);
        if r.reset <= a || later {
            Cell::Blank
        } else {
            Cell::Read(level(r.x10))
        }
    }

    fn level_cell(&self, b: i64) -> Cell {
        if self.unknown_from.is_some_and(|u| b > u) {
            return Cell::Unknown;
        }
        let run = self
            .runs
            .iter()
            .rev()
            .find(|r| r.from < b && b <= r.to)
            .or_else(|| self.runs.iter().rev().find(|r| r.from < b));
        match run {
            None => Cell::Blank,
            Some(run) => run
                .at(b.min(run.to))
                .map_or(Cell::Unknown, |x10| Cell::Ahead(level(x10))),
        }
    }
}

impl Burn {
    /// The replay's fill of it at its reset, in tenths, where it is projected and known there.
    #[must_use]
    pub fn by_renewal(&self) -> Option<u16> {
        fill_at(&self.ahead, self.renews, true).flatten()
    }

    /// The lane at `per_day` cells a day on the work row's clock ([`Chart::cells`] at `now`), into
    /// `out` (cleared first), so the card's day axis reads across it: `per_day` past cells over
    /// the last day as read on this Mac (the last reading at or before the cell's end, blank
    /// before this week's first, `↺` in the cell its week began), `▕`, then the projected line
    /// ([`Cell::Out`] once out, `┄` where nothing is projected) to the cell its reset falls in,
    /// which is `↺` and the lane's last. A reset past the chart's end draws its `7 · per_day`
    /// cells of line and no `↺`. Levels are the lane's ([`lane_level`]), under its ceiling.
    pub fn cells(&self, per_day: u8, now: i64, out: &mut Vec<Cell>) {
        out.clear();
        if per_day == 0 {
            return;
        }
        let k = i64::from(per_day);
        let s = DAY / k;
        for i in 0..k {
            let a = now - DAY + i * s;
            let b = if i + 1 == k { now } else { a + s };
            out.push(if (a..b).contains(&self.start) {
                Cell::Reset
            } else {
                let i = self.read.partition_point(|r| r.t <= b);
                i.checked_sub(1)
                    .and_then(|i| self.read.get(i))
                    .filter(|r| r.t >= self.start)
                    .map_or(Cell::Blank, |r| Cell::Read(lane_level(r.x10)))
            });
        }
        out.push(Cell::Now);
        if self.renews <= now {
            return;
        }
        for j in 0..7 * k {
            let a = now + j * s;
            let b = a + s;
            if self.renews < b {
                out.push(Cell::Reset);
                return;
            }
            out.push(if self.out_at.is_some_and(|o| b > o) {
                Cell::Out
            } else {
                match fill_at(&self.ahead, b, true) {
                    Some(Some(x10)) => Cell::Ahead(lane_level(x10)),
                    _ => Cell::Unknown,
                }
            });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The view from the outlook

/// The roster's display order (§2.5): `Class::Preferred` first, then file order; stable, so a
/// rotation never reshuffles it.
fn roster_order(rows: &[RowOutlook]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| (rows[i].class != Class::Preferred, i));
    order
}

/// The named of `order` (§2.5): its first [`NAMED_MAX`], except that each of `must` outside them
/// takes the place of the last named one that is none of `must`; kept in `order`'s order.
fn pick_named<T: Copy + PartialEq>(order: &[T], must: &[T]) -> Vec<T> {
    let mut named: Vec<T> = order.iter().take(NAMED_MAX).copied().collect();
    for m in must {
        if !order.contains(m) || named.contains(m) {
            continue;
        }
        if let Some(p) = named.iter().rposition(|n| !must.contains(n)) {
            named[p] = *m;
        }
        named.sort_by_key(|n| order.iter().position(|o| o == n));
    }
    named
}

/// A row's week reset, where one is ahead of `as_of` on an admitted reading and no further
/// ahead than a week's can be ([`accounts::believed_reset`], a week and a day): a stored
/// reading keeps the vendor's `resets_at` as sent, so one in the wrong unit, or `i64::MAX`, is
/// no reset to say, to draw a lane to, or to add a local offset to.
pub(super) fn reset_ahead(row: &RowOutlook, as_of: i64) -> Option<i64> {
    row.window(Window::Week)
        .filter(|w| {
            w.at.is_none_or(|at| super::profile::usage_read_age(at, as_of).is_some())
        })
        .and_then(|w| accounts::believed_reset(w.reset, as_of))
        .filter(|r| *r > as_of)
}

/// The tenths a figure says, where it says a number.
const fn figure_x10(figure: Figure) -> Option<u16> {
    match figure {
        Figure::Exact(x10) | Figure::AtLeast(x10) => Some(x10),
        Figure::Out | Figure::Unread => None,
    }
}

/// The instants of the week resets of a row whose reset is `reset`, in `[from, to)`.
fn resets_within(reset: i64, from: i64, to: i64) -> impl Iterator<Item = i64> {
    let first = (from - reset + WEEK - 1).div_euclid(WEEK);
    (first..)
        .map(move |n| reset + n * WEEK)
        .take_while(move |t| *t < to)
}

/// Each local midnight in `(from, to)`: the instant each local day begins. Where the clocks
/// spring forward over midnight (Santiago, Asunción, Havana, Beirut) the day has no 00:00, and
/// it begins at the change, its first instant on the new offset.
fn midnights(from: i64, to: i64, offsets: &Offsets) -> Vec<i64> {
    let mut out = Vec::new();
    let mut day = (from + offsets.at(from)).div_euclid(DAY) + 1;
    loop {
        let local = day * DAY;
        let mut t = local - offsets.at(local - offsets.at(local));
        if (t + offsets.at(t)).div_euclid(DAY) < day
            && let Some(change) = offsets.change_after(t)
        {
            t = change;
        }
        if t >= to {
            return out;
        }
        if t > from {
            out.push(t);
        }
        day += 1;
    }
}

/// A row's knots over `[from, to]`: its fill at `from` from the right, the knots between, its
/// fill at `to` from the left.
fn knots_over(knots: &[Knot], from: i64, to: i64) -> Vec<Knot> {
    let mut out = Vec::new();
    if let Some(x10) = fill_at(knots, from, false) {
        out.push(Knot { t: from, x10 });
    }
    out.extend(knots.iter().filter(|k| k.t > from && k.t < to).copied());
    if to > from
        && let Some(x10) = fill_at(knots, to, true)
    {
        out.push(Knot { t: to, x10 });
    }
    out
}

/// What the view's words say a stop with: its pass, from, end and lean.
struct StopSaid<'a> {
    pass: PassKind,
    from: i64,
    end: StopEnd,
    lean: Option<&'a str>,
}

/// Phrase 2 of `verdict` (§2.3) in `words`, `carrier` whether the pane is the carrier's, the
/// one a refused pace is said on: a wall the work waits at (`⧗ until Sat 10:00`, the vendor's
/// reset said exactly), a projected stop (`╳ ~Sat 03:00–Sat 10:00`), a lean on an unread row, the
/// work on credits, a refused pace; `None` for a run that goes on and for a wall the work moves
/// off at the turn's end.
fn phrase_of(words: &Words<'_>, verdict: &Verdict, carrier: bool) -> Option<Phrase> {
    let phrase = |full: String, short: String, ink: Role, kept: bool| {
        Some(Phrase {
            full,
            short,
            ink,
            kept,
        })
    };
    match verdict {
        Verdict::Walled { to: None, until } => {
            let until =
                until.map_or_else(|| "its reset".to_owned(), |u| words.when(words.resumes(u)));
            let words = format!("{WAIT_MARK} until {until}");
            phrase(words.clone(), words, Role::Warn, true)
        }
        Verdict::Stop { from, end, .. } => {
            let (full, short) = words.stop_phrase(*from, *end);
            phrase(full, short, Role::Warn, true)
        }
        Verdict::Lean { until, row, .. } => phrase(
            format!(
                "? {} unread from {}",
                cut(words.name(row)),
                words.about(*until)
            ),
            format!("? {} {}", cut(words.name(row)), words.about(*until)),
            Role::Value,
            true,
        ),
        Verdict::Credits { at } => {
            let words = format!("{CREDITS_MARK} {}", words.about(*at));
            phrase(words.clone(), words, Role::Warn, true)
        }
        Verdict::Refused(why) if carrier => phrase(why.short(), why.short(), Role::Quiet, false),
        Verdict::Walled { to: Some(_), .. } | Verdict::Continuous | Verdict::Refused(_) => None,
    }
}

/// The words of one view: the outlook's clock and this tab.
struct Words<'a> {
    out: &'a Outlook,
    /// The words are the glass's, which name each account by its email ([`Outlook::name_of`]);
    /// the CLI's ([`Outlook::verdict_phrase`]) name its roster label.
    glass: bool,
    tab: &'a str,
    tab_reset: Option<i64>,
    single: bool,
    /// The lead is a reading this replay has not seen: the replay read nothing of this tab's
    /// row, and the footer holds its figure now (`own`), its first reading having come since.
    unseen: bool,
}

impl Words<'_> {
    /// Whether `verdict` is the replay's refusal for want of the reading this view's lead holds
    /// ([`Self::unseen`]): the carrier's `no reading` beside its own figure. Such a refusal is
    /// not said, on the row, in its title or aloud (§2.2: one figure at now); the next replay,
    /// which the memo runs once the store holds the reading, draws its step and its projection.
    fn outdated(&self, verdict: &Verdict) -> bool {
        self.unseen && *verdict == Verdict::Refused(outlook::Refusal::NoFigure)
    }

    fn when(&self, t: i64) -> String {
        when(t, self.out.as_of, &self.out.offsets)
    }

    fn spoken_when(&self, t: i64) -> String {
        spoken_when(t, self.out.as_of, &self.out.offsets)
    }

    /// How these words name the row `label`: the glass by its email, the CLI by the label.
    fn name<'b>(&'b self, label: &'b str) -> &'b str {
        if self.glass {
            self.out.name_of(label)
        } else {
            label
        }
    }

    /// `~T` of a forecast start, rounded down by its lead.
    fn about(&self, t: i64) -> String {
        format!(
            "~{}",
            self.when(round_down(t, self.out.as_of, &self.out.offsets))
        )
    }

    fn spoken_about(&self, t: i64) -> String {
        format!(
            "about {}",
            self.spoken_when(round_down(t, self.out.as_of, &self.out.offsets))
        )
    }

    /// Whose week reset `r` is: `its reset` in a roster of one, else the account's
    /// (`main's reset`); `None` where `r` is no row's week reset (a 5-hour window's).
    fn whose(&self, r: i64) -> Option<String> {
        let row = self.out.rows.iter().find(|row| {
            row.window(Window::Week)
                .and_then(|w| w.reset)
                .is_some_and(|reset| (r - reset).rem_euclid(WEEK) == 0)
        })?;
        Some(if self.single {
            "its reset".to_owned()
        } else {
            format!("{}'s reset", self.name(&row.label))
        })
    }

    /// The instant the work resumes at a wall that waits to `until`: the vendor reset whose
    /// grace it ends ([`Outlook::wall_reset`]), said exactly, else `until`.
    fn resumes(&self, until: i64) -> i64 {
        self.out.wall_reset(until).unwrap_or(until)
    }

    /// Who is out in a stop from `from`: the one account, its next week, or every account.
    fn who_out(&self, from: i64) -> &'static str {
        if !self.single {
            "every account out"
        } else if self.tab_reset.is_some_and(|r| from >= r) {
            "next week out"
        } else {
            "out"
        }
    }

    /// Phrase 2 of a stop (§2.3).
    fn stop_phrase(&self, from: i64, end: StopEnd) -> (String, String) {
        let start = self.about(from);
        let short = format!("{GAP_MARK} {start}");
        let full = match end {
            StopEnd::Reset(r) => format!("{short}\u{2013}{}", self.when(r)),
            StopEnd::About(t) => format!(
                "{short}\u{2013}~{}",
                self.when(round_up(t, self.out.as_of, &self.out.offsets))
            ),
            StopEnd::Open => format!("{GAP_MARK} from {start}"),
        };
        let short = if end.at().is_none() {
            full.clone()
        } else {
            short
        };
        (full, short)
    }

    /// A stop's end as the title (`spoken` false) or a reader says it: ` until main's reset Sat
    /// 10:00`, ` until ~Sat 03:00`, ` with no end in reach`.
    fn stop_end(&self, end: StopEnd, spoken: bool) -> String {
        let at = |t: i64| {
            if spoken {
                self.spoken_when(t)
            } else {
                self.when(t)
            }
        };
        match end {
            StopEnd::Reset(r) => match self.whose(r) {
                Some(whose) => format!(" until {whose} {}", at(r)),
                None => format!(" until {}", at(r)),
            },
            StopEnd::About(t) => {
                let t = round_up(t, self.out.as_of, &self.out.offsets);
                if spoken {
                    format!(" until about {}", at(t))
                } else {
                    format!(" until ~{}", at(t))
                }
            }
            StopEnd::Open => " with no end in reach".to_owned(),
        }
    }

    /// A stop of `pass` as a reader hears it, `lean` the row a stop both branches of a lean meet
    /// leans on: usage credits where they carry the work through it, else every account out.
    fn stop_spoken(&self, pass: PassKind, stop: &Stop, lean: Option<&str>) -> String {
        if !stop.credits {
            return self.stop_said(
                &StopSaid {
                    pass,
                    from: stop.from,
                    end: stop.end,
                    lean,
                },
                true,
            );
        }
        let mut s = format!(
            "usage credits carry the work from {}{}",
            self.spoken_about(stop.from),
            self.stop_end(stop.end, true)
        );
        if pass == PassKind::Working {
            s.push_str(", a 5-hour limit");
        }
        if let Some(row) = lean {
            s.push_str(&format!(", whatever {} holds", self.name(row)));
        }
        s
    }

    /// A stop as the title (`spoken` false) or a reader says it.
    fn stop_said(&self, stop: &StopSaid<'_>, spoken: bool) -> String {
        let mut s = if spoken {
            format!(
                "{} from {}",
                self.who_out(stop.from),
                self.spoken_about(stop.from)
            )
        } else {
            format!("{} {}", self.who_out(stop.from), self.about(stop.from))
        };
        s.push_str(&self.stop_end(stop.end, spoken));
        if stop.pass == PassKind::Working {
            s.push_str(if spoken {
                ", a 5-hour limit"
            } else {
                " (a 5-hour limit)"
            });
        }
        if let Some(row) = stop.lean {
            s.push_str(&format!(", whatever {} holds", self.name(row)));
        }
        s
    }

    /// The week pass's chain as a reader hears it: its moves, each stop in its place (usage
    /// credits where they carry it), its lean, and the stop both of the lean's branches meet,
    /// which the chart draws as `╳` whatever the verdict (§2.6: the spoken row carries the stops
    /// the row shows).
    fn chain_spoken(&self, week: &Pass) -> String {
        let mut clauses: Vec<String> = Vec::new();
        let mut moves = String::new();
        let mut prev: Option<&str> = None;
        let mut stops = week.stops.iter().peekable();
        let say = |stop: &Stop| self.stop_spoken(PassKind::Week, stop, None);
        let mut after_stop = false;
        for link in week.chain.iter().filter(|l| l.to > l.from) {
            while let Some(stop) = stops.next_if(|stop| stop.from <= link.from) {
                if !moves.is_empty() {
                    clauses.push(std::mem::take(&mut moves));
                }
                clauses.push(say(stop));
                after_stop = true;
            }
            if prev == Some(link.label.as_str()) && !after_stop {
                continue;
            }
            let name = self.name(&link.label);
            if moves.is_empty() {
                moves = if clauses.is_empty() {
                    name.to_owned()
                } else {
                    format!("then {name}")
                };
            } else {
                moves.push_str(&format!(
                    ", then {name} from {}",
                    self.spoken_about(link.from)
                ));
            }
            prev = Some(&link.label);
            after_stop = false;
        }
        if let Some(lean) = &week.lean {
            let lean = format!(
                "{}, which aterm cannot read, from {}",
                self.name(&lean.row),
                self.spoken_about(lean.at)
            );
            if moves.is_empty() {
                moves = format!("then {lean}");
            } else {
                moves.push_str(&format!(", then {lean}"));
            }
        }
        if !moves.is_empty() {
            clauses.push(moves);
        }
        clauses.extend(stops.map(say));
        if let (Some(lean), Some(stop)) = (&week.lean, Outlook::lean_stop(week)) {
            clauses.push(self.stop_spoken(PassKind::Week, stop, Some(&lean.row)));
        }
        clauses.join("; ")
    }
}

impl Outlook {
    /// The quota view of the pane whose account is `tab` (its roster label; one the roster
    /// cannot name reads [`THIS_LOGIN`]), with the footer's own week figure `own` where it holds
    /// one (it is the lead's, and the projection is shifted to agree with it), for a tab the
    /// rotation can move (`movable`; else nothing is projected and no verdict is said). A wall
    /// standing on the tab's account keeps it [`Figure::Out`] whatever `own` says: `out` means
    /// a standing wall, and a wall never prints a percentage (§2.1, §3.2), while the footer's
    /// figure ([`Figure::of_weekly`]) is a reading, which is never `Out`. Where `own` holds a
    /// figure of a row this replay read nothing of, the reading came since the replay: the view
    /// says no refusal for want of it (`Words::outdated`), and has no step or projection to
    /// draw until a replay reads it.
    #[must_use]
    pub fn quota(&self, tab: &str, own: Option<Figure>, movable: bool) -> QuotaView {
        let as_of = self.as_of;
        let tab_row = self.rows.iter().position(|r| r.label == tab);
        let carrier_row = self
            .carrier
            .as_deref()
            .and_then(|c| self.rows.iter().position(|r| r.label == c));
        // `None` is a login the roster cannot name: always named, first.
        let order: Vec<Option<usize>> = tab_row
            .is_none()
            .then_some(None)
            .into_iter()
            .chain(roster_order(&self.rows).into_iter().map(Some))
            .collect();
        let must: Vec<Option<usize>> = std::iter::once(tab_row)
            .chain(carrier_row.filter(|c| Some(*c) != tab_row).map(Some))
            .collect();
        let picked = pick_named(&order, &must);
        let row_figure = tab_row.map_or(Figure::Unread, |i| self.rows[i].figure(as_of));
        let tab_figure = match own {
            Some(own) if row_figure != Figure::Out => own,
            _ => row_figure,
        };
        let named: Vec<Named> = picked
            .iter()
            .map(|pick| match pick {
                None => Named {
                    label: THIS_LOGIN.to_owned(),
                    name: THIS_LOGIN.to_owned(),
                    this_tab: true,
                    carrier: false,
                    figure: tab_figure,
                    reset: None,
                    plan: None,
                },
                Some(i) => {
                    let row = &self.rows[*i];
                    let this_tab = Some(*i) == tab_row;
                    Named {
                        label: row.label.clone(),
                        name: row.name.clone(),
                        this_tab,
                        carrier: Some(*i) == carrier_row,
                        figure: if this_tab {
                            tab_figure
                        } else {
                            row.figure(as_of)
                        },
                        reset: reset_ahead(row, as_of),
                        plan: row.plan.title(),
                    }
                }
            })
            .collect();
        let folded: Vec<String> = order
            .iter()
            .filter(|o| !picked.contains(o))
            .filter_map(|o| o.map(|i| self.rows[i].label.clone()))
            .collect();
        let tab_reset = tab_row.and_then(|i| reset_ahead(&self.rows[i], as_of));
        let drawn = movable.then_some(self.week.as_ref()).flatten();
        let wait = movable.then(|| self.wait_of(tab_row, tab_reset)).flatten();
        let chart = self.chart_of(tab, tab_row, tab_figure, tab_reset, drawn, wait);
        let lanes = picked
            .iter()
            .flatten()
            .map(|&i| self.lane_of(&self.rows[i], drawn.is_some()))
            .collect();
        let words = Words {
            unseen: row_figure == Figure::Unread && tab_figure != Figure::Unread,
            ..self.words_for(tab)
        };
        let tail = self.tail_of(&words, tab_row, tab_figure, &chart, movable);
        let mut view = QuotaView {
            as_of,
            named,
            folded,
            chart,
            lanes,
            tail,
            title: String::new(),
            spoken: String::new(),
        };
        view.title = self.title_of(&words, &view, drawn, movable);
        view.spoken = self.spoken_of(&words, &view, drawn, movable);
        view
    }

    /// The outlook's own verdict as the quota row's phrase 2 says it (§2.3), on the carrier's
    /// pane, whose pace it is: what `aterm accounts outlook` prints, so the CLI and the glass say
    /// one verdict in one grammar. `None` where the row says none: the work runs on, or a wall
    /// the work moves off at the turn's end (the chart's `→`; the CLI says that move itself).
    #[must_use]
    pub fn verdict_phrase(&self) -> Option<Phrase> {
        let words = Words {
            out: self,
            glass: false,
            tab: "",
            tab_reset: None,
            single: self.rows.len() <= 1,
            unseen: false,
        };
        phrase_of(&words, &self.verdict, true)
    }

    /// The words of a view of the pane whose account is `tab`.
    fn words_for<'a>(&'a self, tab: &'a str) -> Words<'a> {
        let tab_reset = self
            .rows
            .iter()
            .find(|r| r.label == tab)
            .and_then(|row| reset_ahead(row, self.as_of));
        Words {
            out: self,
            glass: true,
            tab,
            tab_reset,
            single: self.rows.len() <= 1,
            unseen: false,
        }
    }

    /// The Accounts card's verdict line for the pane whose account is `tab` and its `view`
    /// (§1.4): `╳ out ~Sat 05:00–Sat 10:00, then main`, widest form first, in the tail's own
    /// grammar (§2.3) with what the card has room to add (who is out, who carries the work
    /// after). A tab the rotation cannot move (`movable` false) has no verdict, as its row has
    /// none: the card says its reset, as the tail does, and that nothing is projected; a refused
    /// outlook says its refusal either way, the reason nothing is projected for anyone.
    pub(super) fn card_verdict(&self, view: &QuotaView, tab: &str, movable: bool) -> CardVerdict {
        let words = self.words_for(tab);
        if view.waiting() {
            return CardVerdict {
                forms: vec![NO_FIGURE_FULL.to_owned(), NO_FIGURE.to_owned()],
                spoken: format!("{NO_FIGURE}; Claude reports it with its next reply."),
                ink: Role::Quiet,
                worst: None,
            };
        }
        let tab_row = self.rows.iter().position(|r| r.label == tab);
        let verdict = self.verdict_for(tab_row);
        // A refused outlook says why nothing is projected, whoever the tab is.
        if !movable && !matches!(verdict, Verdict::Refused(_)) {
            const WHY: &str = "not projected: the rotation cannot move this tab";
            let (forms, spoken) = match words.tab_reset {
                Some(reset) => {
                    let at = format!("{RESET_MARK} {}", words.when(reset));
                    (
                        vec![
                            format!("{at}{SEP}{WHY}"),
                            format!("{at}{SEP}not projected"),
                            at,
                        ],
                        format!(
                            "Resets {}. Not projected: the rotation cannot move this tab.",
                            words.spoken_when(reset)
                        ),
                    )
                }
                None => (
                    vec![WHY.to_owned(), "not projected".to_owned()],
                    "Not projected: the rotation cannot move this tab.".to_owned(),
                ),
            };
            return CardVerdict {
                forms,
                spoken,
                ink: Role::Value,
                worst: None,
            };
        }
        let sentence = |s: String| {
            let mut chars = s.chars();
            let head: String = chars
                .next()
                .map(|c| c.to_uppercase().collect())
                .unwrap_or_default();
            format!("{head}{}.", chars.as_str())
        };
        let out_words =
            |phrase: &str| phrase.replacen(&format!("{GAP_MARK} "), &format!("{GAP_MARK} out "), 1);
        let stop = |from: i64, end: StopEnd, pass: PassKind, lean: Option<&str>| {
            let (full, short) = words.stop_phrase(from, end);
            let carried = match pass {
                PassKind::Week => self.week.as_ref(),
                PassKind::Working => self.working.as_ref(),
            };
            // Who carries the work after the stop, where more than one account could.
            let then = carried
                .filter(|_| !words.single)
                .and_then(|p| p.chain.iter().find(|l| l.from >= from && l.to > l.from))
                .map(|l| words.name(&l.label).to_owned());
            let mut forms = Vec::new();
            if let Some(next) = &then {
                forms.push(format!("{}, then {next}", out_words(&full)));
            }
            forms.push(out_words(&full));
            forms.push(out_words(&short));
            let mut spoken = words.stop_said(
                &StopSaid {
                    pass,
                    from,
                    end,
                    lean,
                },
                true,
            );
            if let Some(next) = then {
                spoken.push_str(&format!(", then {next}"));
            }
            (forms, sentence(spoken))
        };
        let (forms, spoken, ink, worst) = match verdict {
            Verdict::Stop {
                pass,
                from,
                end,
                lean,
            } => {
                let (forms, spoken) = stop(from, end, pass, lean.as_deref());
                (forms, spoken, Role::Warn, None)
            }
            Verdict::Walled { to: None, until } => {
                let spoken_until = until.map_or_else(
                    || "its reset".to_owned(),
                    |u| words.spoken_when(words.resumes(u)),
                );
                let until =
                    until.map_or_else(|| "its reset".to_owned(), |u| words.when(words.resumes(u)));
                (
                    vec![
                        format!("{WAIT_MARK} out now, until {until}"),
                        format!("{WAIT_MARK} until {until}"),
                    ],
                    format!("Out now: the work waits until {spoken_until}."),
                    Role::Warn,
                    None,
                )
            }
            Verdict::Walled { to: Some(to), .. } => {
                let to = words.name(&to);
                (
                    vec![
                        format!("{HANDOFF_MARK} out now: the work moves to {to} at the turn's end"),
                        format!("{HANDOFF_MARK} the work moves to {to}"),
                    ],
                    format!("Out now: the work moves to {to} at the turn's end."),
                    Role::Value,
                    None,
                )
            }
            Verdict::Lean {
                until,
                row,
                stop: s,
                ..
            } => {
                let row = words.name(&row).to_owned();
                let worst = s.map(|(from, end)| {
                    let (forms, spoken) = stop(from, end, PassKind::Week, None);
                    let (_, short) = words.stop_phrase(from, end);
                    (
                        forms
                            .into_iter()
                            .map(|f| format!("  if {} has nothing: {f}", cut(&row)))
                            .chain([format!("  if nothing: {short}")])
                            .collect(),
                        format!("If {row} has nothing: {spoken}"),
                    )
                });
                (
                    vec![
                        format!("? {} unread from {}", cut(&row), words.about(until)),
                        format!("? {} {}", cut(&row), words.about(until)),
                    ],
                    format!(
                        "The work reaches {row}, which aterm cannot read, from {}.",
                        words.spoken_about(until)
                    ),
                    Role::Value,
                    worst,
                )
            }
            Verdict::Credits { at } => (
                vec![
                    format!("{CREDITS_MARK} usage credits from {}", words.about(at)),
                    format!("{CREDITS_MARK} {}", words.about(at)),
                ],
                format!(
                    "Usage credits would carry the work from {}.",
                    words.spoken_about(at)
                ),
                Role::Warn,
                None,
            ),
            Verdict::Refused(why) => (
                vec![why.words(), why.short()],
                why.spoken(),
                Role::Value,
                None,
            ),
            Verdict::Continuous => (
                vec![
                    "continuous for the next 7 days".to_owned(),
                    "continuous 7 days".to_owned(),
                ],
                "Continuous for the next 7 days.".to_owned(),
                Role::Value,
                None,
            ),
        };
        CardVerdict {
            forms,
            spoken,
            ink,
            worst,
        }
    }

    /// The card's pace line (§1.4), widest form first (`at 36% a day (this Mac, the last 24 h)`,
    /// `at 36% a day`), and what a reader hears; `None` where no pace stands or nothing carries
    /// the work.
    pub(super) fn card_pace(&self) -> Option<(Vec<String>, String)> {
        let pace = self.demand.week.ok()?;
        self.week.as_ref().filter(|p| !p.chain.is_empty())?;
        let pct = self.day_pct()?;
        let (basis, spoken) = match pace.basis {
            Basis::Day => (
                "this Mac, the last 24 h",
                "this Mac's pace over the last 24 hours",
            ),
            Basis::Hour => (
                "this Mac, the last hour",
                "this Mac's pace over the last hour",
            ),
            Basis::WeekBegan | Basis::WindowOpened => {
                ("this week so far", "this week's pace so far")
            }
        };
        Some((
            vec![
                format!("at {pct}% a day ({basis})"),
                format!("at {pct}% a day"),
            ],
            format!("At {pct} percent a day: {spoken}."),
        ))
    }

    /// What a reader hears of the card's work row: the work as the view of `tab`'s pane says it
    /// (§2.6), after its named accounts, which the card's items say; nothing projected for a tab
    /// the rotation cannot move, as on its row.
    pub(super) fn card_work_said(&self, tab: &str, movable: bool) -> String {
        let words = self.words_for(tab);
        let drawn = movable.then_some(self.week.as_ref()).flatten();
        self.work_said(&words, drawn, movable)
    }

    /// The verdict this tab's pane says: its own wall, else the outlook's.
    fn verdict_for(&self, tab_row: Option<usize>) -> Verdict {
        tab_row
            .and_then(|i| self.rows[i].walled.as_ref())
            .map_or_else(
                || self.verdict.clone(),
                |w| Verdict::Walled {
                    to: w.to.clone(),
                    until: w.until,
                },
            )
    }

    /// The vendor reset a wall standing now to `until` is the grace after, where a standing wall
    /// on any row names one ([`outlook::Walled::reset`], a week's wall or a 5-hour one).
    fn wall_reset(&self, until: i64) -> Option<i64> {
        self.rows
            .iter()
            .filter_map(|row| row.walled.as_ref()?.reset)
            .find(|reset| reset.saturating_add(accounts::RESET_GRACE_S) == until)
    }

    /// Where the work this tab's pane runs waits at a wall standing now (`Verdict::Walled { to:
    /// None }`, `⧗` on the row), which is a stop from now until the wall ends (§2.2, §2.3): at
    /// the vendor reset whose grace it stands to, said exactly, else at its forecast end. A wall
    /// that names no end waits, as its words say, until this tab's reset, where one is ahead;
    /// else nothing ends it in reach. `None` where the pane does not wait now.
    fn wait_of(&self, tab_row: Option<usize>, tab_reset: Option<i64>) -> Option<StopEnd> {
        match self.verdict_for(tab_row) {
            Verdict::Walled {
                to: None,
                until: Some(until),
            } => Some(
                self.wall_reset(until)
                    .map_or(StopEnd::About(until), StopEnd::Reset),
            ),
            Verdict::Walled {
                to: None,
                until: None,
            } => Some(tab_reset.map_or(StopEnd::Open, StopEnd::Reset)),
            _ => None,
        }
    }

    /// The stop a lean's both branches meet, where they do: it stands whatever the row holds.
    fn lean_stop(pass: &Pass) -> Option<&Stop> {
        let lean = pass.lean.as_deref()?;
        outlook::best_stops(lean)
            .then(|| outlook::worst_stop(lean))
            .flatten()
    }

    /// Every stop `pass` projects, as the chart draws it: its own, then the stop both branches
    /// of its lean meet, with the row that one leans on.
    fn projected(pass: &Pass) -> impl Iterator<Item = (&Stop, Option<&str>)> {
        let leans = pass.lean.as_deref().map(|lean| lean.row.as_str());
        pass.stops
            .iter()
            .map(|stop| (stop, None))
            .chain(Outlook::lean_stop(pass).map(|stop| (stop, leans)))
    }

    fn chart_of(
        &self,
        tab: &str,
        tab_row: Option<usize>,
        tab_figure: Figure,
        tab_reset: Option<i64>,
        drawn: Option<&Pass>,
        wait: Option<StopEnd>,
    ) -> Chart {
        let as_of = self.as_of;
        let end = as_of + 7 * DAY;
        let mut chart = Chart {
            now: as_of,
            midnights: midnights(as_of - DAY, end, &self.offsets),
            ..Chart::default()
        };
        if let Some(row) = tab_row.map(|i| &self.rows[i]) {
            let start = row
                .read
                .partition_point(|r| r.t < as_of - DAY)
                .saturating_sub(1);
            chart.past = row.read.get(start..).unwrap_or_default().to_vec();
            // The last past cell holds the lead's figure (§2.2): one figure at now.
            if let (Some(x10), Some(reset)) = (figure_x10(tab_figure), tab_reset) {
                let epoch = chart
                    .past
                    .last()
                    .filter(|r| r.reset == reset)
                    .and_then(|r| r.epoch);
                chart.past.push(Read {
                    t: as_of,
                    x10,
                    reset,
                    epoch,
                });
            }
        }
        // A stop nothing ends in reach runs past the chart's end.
        let stop_end = |e: StopEnd| e.at().unwrap_or(end + DAY);
        let Some(week) = drawn else {
            // Nothing projected: `┄` across the future, and only this tab's resets; a wall
            // standing now is no projection, so its wait is drawn over them.
            chart.unknown_from = Some(as_of);
            if let Some(reset) = tab_reset {
                chart.marks = resets_within(reset, as_of, end)
                    .map(|t| (t, Mark::Reset))
                    .collect();
            }
            chart.stops = wait
                .map(|e| (as_of, stop_end(e), false))
                .into_iter()
                .collect();
            return chart;
        };
        let mut prev = tab.to_owned();
        for link in &week.chain {
            if link.label != prev {
                chart.marks.push((
                    link.from,
                    Mark::Handoff(self.name_of(&link.label).to_owned()),
                ));
                prev.clone_from(&link.label);
            }
            if link.to <= link.from {
                continue;
            }
            let knots = week
                .fills
                .get(&link.label)
                .map(|k| knots_over(k, link.from, link.to))
                .unwrap_or_default();
            chart.runs.push(Run {
                label: link.label.clone(),
                name: self.name_of(&link.label).to_owned(),
                from: link.from,
                to: link.to,
                knots,
            });
            if let Some(reset) = self
                .rows
                .iter()
                .find(|r| r.label == link.label)
                .and_then(|r| r.window(Window::Week))
                .and_then(|w| w.reset)
            {
                chart.marks.extend(
                    resets_within(reset, link.from, link.to.min(end)).map(|t| (t, Mark::Reset)),
                );
            }
        }
        // One figure at now: the first run, where it is this tab's, is shifted to the lead's
        // figure until its first drop, clamped below a fill; a fill stays a fill.
        if let (Some(first), Some(x10)) = (chart.runs.first_mut(), figure_x10(tab_figure))
            && first.label == tab
            && first.from == as_of
            && let Some(Some(replay)) = first.knots.first().map(|k| k.x10)
        {
            let shift = i32::from(x10) - i32::from(replay);
            let mut prev_t = None;
            for knot in &mut first.knots {
                if prev_t == Some(knot.t) {
                    break;
                }
                prev_t = Some(knot.t);
                knot.x10 = knot.x10.map(|v| {
                    if v >= 1000 {
                        1000
                    } else {
                        u16::try_from((i32::from(v) + shift).clamp(0, 999)).unwrap_or(999)
                    }
                });
            }
        }
        if let Some(lean) = &week.lean {
            chart
                .marks
                .push((lean.at, Mark::Lean(self.name_of(&lean.row).to_owned())));
        }
        chart.unknown_from = [week.lean.as_ref().map(|l| l.at), week.cut_at]
            .into_iter()
            .flatten()
            .min();
        // Every stop the passes project (each pass's own and the one both branches of its lean
        // meet), and a wall standing now, a stop from now until it ends, where no pass begins
        // one now already.
        let mut stops: Vec<(i64, StopEnd, bool)> = std::iter::once(week)
            .chain(self.working.as_ref())
            .flat_map(Outlook::projected)
            .map(|(s, _)| (s.from, s.end, s.credits))
            .collect();
        if let Some(e) = wait
            && !stops.iter().any(|(from, _, _)| *from <= as_of)
        {
            stops.insert(0, (as_of, e, false));
        }
        chart.stops = stops
            .iter()
            .map(|&(from, e, credits)| (from, stop_end(e), credits))
            .collect();
        // A stop that ends at a week's vendor reset draws it there: the work resumes on it.
        chart
            .marks
            .extend(stops.iter().filter_map(|&(_, e, _)| match e {
                StopEnd::Reset(r)
                    if r < end
                        && self.rows.iter().any(|row| {
                            row.window(Window::Week)
                                .and_then(|w| w.reset)
                                .is_some_and(|reset| (r - reset).rem_euclid(WEEK) == 0)
                        }) =>
                {
                    Some((r, Mark::Reset))
                }
                _ => None,
            }));
        chart.marks.sort_by_key(|(t, _)| *t);
        chart
    }

    /// The burn-up lane of `row`'s own vendor week: its readings on this Mac and, where the week
    /// pass is `drawn`, the replay's fill of it to its reset and the instant it runs out.
    ///
    /// Both come from the drawn pass's own fills ([`Pass::fills`]), which stop at its lean or its
    /// cut: the lane is out ([`Cell::Out`], `out ~T` on its item) only where those knots reach a fill, and
    /// is `┄` past them. A lean's worst case walls an unread row at the lean's instant as an
    /// assumption, never a figure ([`Pass::projections`] carries it for the bars), so it is never
    /// drawn or said as the account's own run-out: the card's verdict says it, conditionally
    /// (`if alt1 has nothing: ╳ …`).
    pub(super) fn lane_of(&self, row: &RowOutlook, drawn: bool) -> Burn {
        let as_of = self.as_of;
        let Some(renews) = reset_ahead(row, as_of) else {
            return Burn {
                label: row.label.clone(),
                name: row.name.clone(),
                start: as_of - WEEK,
                renews: as_of,
                read: Vec::new(),
                ahead: Vec::new(),
                out_at: None,
            };
        };
        let ahead: Vec<Knot> = self
            .week
            .as_ref()
            .filter(|_| drawn)
            .and_then(|p| p.fills.get(&row.label))
            .map(|k| knots_over(k, as_of, renews))
            .unwrap_or_default();
        let out_at = if row.figure(as_of) == Figure::Out {
            Some(as_of)
        } else {
            ahead
                .iter()
                .find(|k| k.x10.is_some_and(|x10| x10 >= 1000) && k.t < renews)
                .map(|k| k.t)
        };
        Burn {
            label: row.label.clone(),
            name: row.name.clone(),
            start: renews - WEEK,
            renews,
            read: row
                .read
                .iter()
                .filter(|r| r.reset == renews && r.t <= as_of)
                .copied()
                .collect(),
            ahead,
            out_at,
        }
    }

    fn tail_of(
        &self,
        words: &Words<'_>,
        tab_row: Option<usize>,
        tab_figure: Figure,
        chart: &Chart,
        movable: bool,
    ) -> Tail {
        if tab_figure == Figure::Unread {
            return Tail {
                own: None,
                verdict: Some(Phrase {
                    full: NO_FIGURE_FULL.to_owned(),
                    short: NO_FIGURE.to_owned(),
                    ink: Role::Quiet,
                    kept: false,
                }),
            };
        }
        let verdict = self.verdict_for(tab_row);
        let carrier = tab_row.is_some_and(|i| self.rows[i].carrier);
        let said = if movable && !words.outdated(&verdict) {
            phrase_of(words, &verdict, carrier)
        } else {
            None
        };
        // Phrase 1 is left out only where it would repeat phrase 2: a wait that resumes at this
        // tab's own reset (`⧗ until Sat 10:00` says it). A wait that ends at another account's
        // reset keeps it, so this tab's reset is said (`↺ Sun 10:00 · ⧗ until Sat 10:00`).
        let repeats = |reset: &i64| match &verdict {
            Verdict::Walled {
                to: None,
                until: Some(until),
            } => said.is_some() && words.resumes(*until) == *reset,
            _ => false,
        };
        let own = words.tab_reset.filter(|r| !repeats(r)).map(|reset| {
            // The curve at the reset, where this tab's account is drawn there and not full.
            let by_reset = chart
                .runs
                .iter()
                .find(|r| r.label == words.tab && r.from < reset && reset <= r.to)
                .and_then(|r| r.at(reset))
                .filter(|x10| *x10 < 1000)
                .map(whole_under_full);
            Own {
                by_reset,
                reset: words.when(reset),
            }
        });
        Tail { own, verdict: said }
    }

    /// The pace's words, where it stands: `this Mac's last-24 h pace, 21% a day`.
    fn pace_title(&self) -> Option<String> {
        let pace = self.demand.week.ok()?;
        let pct = self.day_pct()?;
        Some(match pace.basis {
            Basis::Day => format!("at this Mac's last-24 h pace, {pct}% a day"),
            Basis::Hour => format!("at this Mac's last-hour pace, {pct}% a day"),
            Basis::WeekBegan | Basis::WindowOpened => {
                format!("at this week's pace so far, {pct}% a day")
            }
        })
    }

    /// The hover title (§2.6). It names the verdict wherever the row says one, and a wall the
    /// work moves off at the turn's end, which has no phrase on the row (the chart's `→` draws
    /// it, and at the narrowest rung, with no chart, the title alone says it).
    fn title_of(
        &self,
        words: &Words<'_>,
        view: &QuotaView,
        drawn: Option<&Pass>,
        movable: bool,
    ) -> String {
        let Some(tab) = view.named.iter().find(|n| n.this_tab) else {
            return String::new();
        };
        let mut parts = vec![tab.name.clone()];
        parts.extend(tab.plan.clone());
        parts.push(match tab.figure {
            Figure::Exact(_) => format!("{} of its week used, read now", tab.figure.words()),
            Figure::AtLeast(x10) => format!(
                "at least {}% of its week used, read earlier",
                x10.min(1000) / 10
            ),
            Figure::Out => "out now".to_owned(),
            Figure::Unread => format!("{NO_FIGURE}, Claude reports it with its next reply"),
        });
        if let Some(own) = &view.tail.own {
            parts.push(match own.by_reset {
                Some(pct) => format!("~{pct}% by its reset {}", own.reset),
                None => format!("resets {}", own.reset),
            });
        }
        let tab_row = self.rows.iter().position(|r| r.label == tab.label);
        let verdict = self.verdict_for(tab_row);
        let moves_off = movable && matches!(verdict, Verdict::Walled { to: Some(_), .. });
        if (view.tail.verdict.is_some() || moves_off) && tab.figure != Figure::Unread {
            parts.extend(self.verdict_title(words, &verdict));
        } else if let (Some(week), Verdict::Continuous) = (drawn, &self.verdict) {
            let moves: Vec<String> = week
                .chain
                .windows(2)
                .filter(|w| w[0].label != w[1].label)
                .map(|w| format!("{} {}", words.name(&w[1].label), words.about(w[1].from)))
                .collect();
            if !moves.is_empty() {
                parts.push(format!("the work moves to {}", moves.join(", then ")));
            }
        }
        if drawn.is_some_and(|p| !p.chain.is_empty()) {
            parts.extend(self.pace_title());
        }
        for other in view.named.iter().filter(|n| !n.this_tab) {
            parts.push(format!("{} {}", other.name, other.figure.words()));
        }
        if !view.folded.is_empty() {
            parts.push(format!("+{} more", view.folded.len()));
        }
        parts.push(CLICK.to_owned());
        parts.join(SEP)
    }

    fn verdict_title(&self, words: &Words<'_>, verdict: &Verdict) -> Option<String> {
        match verdict {
            Verdict::Walled { to: None, until } => Some(format!(
                "the work waits until {}",
                until.map_or_else(|| "its reset".to_owned(), |u| words.when(words.resumes(u)))
            )),
            Verdict::Walled { to: Some(to), .. } => Some(format!(
                "the work moves to {} at the turn's end",
                words.name(to)
            )),
            Verdict::Stop {
                pass,
                from,
                end,
                lean,
            } => Some(words.stop_said(
                &StopSaid {
                    pass: *pass,
                    from: *from,
                    end: *end,
                    lean: lean.as_deref(),
                },
                false,
            )),
            Verdict::Lean {
                until, row, stop, ..
            } => {
                let mut s = format!("{} unread from {}", words.name(row), words.about(*until));
                if let Some((from, end)) = stop {
                    s.push_str(&format!(
                        "; if it has nothing, {}",
                        words.stop_said(
                            &StopSaid {
                                pass: PassKind::Week,
                                from: *from,
                                end: *end,
                                lean: None,
                            },
                            false,
                        )
                    ));
                }
                Some(s)
            }
            Verdict::Credits { at } => Some(format!("on usage credits from {}", words.about(*at))),
            Verdict::Refused(why) => Some(why.words()),
            Verdict::Continuous => None,
        }
    }

    fn spoken_of(
        &self,
        words: &Words<'_>,
        view: &QuotaView,
        drawn: Option<&Pass>,
        movable: bool,
    ) -> String {
        let mut s = Self::named_said(words, view);
        // Case (d): this tab's week unread, its first reading the only thing to say.
        if !view
            .named
            .iter()
            .any(|n| n.this_tab && n.figure == Figure::Unread)
        {
            s.push_str(&self.work_said(words, drawn, movable));
        }
        s
    }

    /// What a reader hears of the named accounts and the folded count: `Accounts. main, this
    /// tab, 75 percent of its week used, resets Saturday 10:00. alt1 80 percent. 1 more
    /// account.`
    fn named_said(words: &Words<'_>, view: &QuotaView) -> String {
        let mut s = String::from("Accounts.");
        for n in &view.named {
            let pct = |x10: u16| x10.min(1000) / 10;
            if n.this_tab {
                s.push_str(&format!(" {}, this tab, ", n.name));
                s.push_str(&match n.figure {
                    Figure::Exact(_) => format!(
                        "{} percent of its week used",
                        figure_x10(n.figure).map_or(0, whole_spoken)
                    ),
                    Figure::AtLeast(x10) => {
                        format!("at least {} percent of its week used", pct(x10))
                    }
                    Figure::Out => "out until its reset".to_owned(),
                    Figure::Unread => format!("{NO_FIGURE}; Claude reports it with its next reply"),
                });
                if let Some(reset) = n.reset.filter(|_| n.figure != Figure::Unread) {
                    s.push_str(&format!(", resets {}", words.spoken_when(reset)));
                }
                s.push('.');
            } else {
                s.push_str(&format!(
                    " {} {}.",
                    n.name,
                    match n.figure {
                        Figure::Exact(x10) => format!("{} percent", whole_spoken(x10)),
                        Figure::AtLeast(x10) => format!("at least {} percent", pct(x10)),
                        Figure::Out => "out".to_owned(),
                        Figure::Unread => "not read".to_owned(),
                    }
                ));
            }
        }
        match view.folded.len() {
            0 => {}
            1 => s.push_str(" 1 more account."),
            n => s.push_str(&format!(" {n} more accounts.")),
        }
        s
    }

    /// What a reader hears of the work: a wait now, the week pass's chain with its stops, the
    /// working pass's stops, a lean's worst case, credits, a refused pace. Each sentence starts
    /// with a blank; empty where there is nothing to say.
    fn work_said(&self, words: &Words<'_>, drawn: Option<&Pass>, movable: bool) -> String {
        let mut s = String::new();
        let tab_row = self.rows.iter().position(|r| r.label == words.tab);
        let verdict = self.verdict_for(tab_row);
        match &verdict {
            Verdict::Walled { to: None, until } if movable => s.push_str(&format!(
                " Out now: the work waits until {}.",
                until.map_or_else(
                    || "its reset".to_owned(),
                    |u| words.spoken_when(words.resumes(u))
                )
            )),
            Verdict::Walled { to: Some(to), .. } if movable => s.push_str(&format!(
                " Out now: the work moves to {} at the turn's end.",
                words.name(to)
            )),
            _ => {}
        }
        if let Some(week) = drawn.filter(|p| !p.chain.is_empty()) {
            let pace = self
                .day_pct()
                .map_or_else(String::new, |p| format!(" at {p} percent a day"));
            s.push_str(&format!(
                " Next 7 days{pace}: {}.",
                words.chain_spoken(week)
            ));
        }
        // Every stop the chart draws from the working pass, its lean's shared stop included,
        // whatever the verdict: one from now where the pane waits at its wall is the wait the
        // `Out now` sentence says.
        let waits = matches!(verdict, Verdict::Walled { to: None, .. });
        let working: Vec<String> = drawn
            .and(self.working.as_ref())
            .into_iter()
            .flat_map(Outlook::projected)
            .filter(|(stop, _)| !(waits && stop.from <= self.as_of))
            .map(|(stop, lean)| words.stop_spoken(PassKind::Working, stop, lean))
            .collect();
        if !working.is_empty() {
            s.push_str(&format!(" Within hours: {}.", working.join("; ")));
        }
        match &verdict {
            _ if !movable => {}
            Verdict::Lean {
                row,
                stop: Some((from, end)),
                ..
            } => {
                let said = words.stop_said(
                    &StopSaid {
                        pass: PassKind::Week,
                        from: *from,
                        end: *end,
                        lean: None,
                    },
                    true,
                );
                s.push_str(&format!(" If {} has nothing: {said}.", words.name(row)));
            }
            Verdict::Credits { at }
                if !drawn
                    .into_iter()
                    .chain(drawn.and(self.working.as_ref()))
                    .flat_map(Outlook::projected)
                    .any(|(st, _)| st.credits && st.from == *at) =>
            {
                s.push_str(&format!(
                    " Usage credits would carry the work from {}.",
                    words.spoken_about(*at)
                ));
            }
            Verdict::Refused(why)
                if tab_row.is_some_and(|i| self.rows[i].carrier) && !words.outdated(&verdict) =>
            {
                s.push(' ');
                s.push_str(&why.spoken());
            }
            _ => {}
        }
        s
    }
}

/// Whole percent, half up, `100` only for 1000: as [`Figure::words`] says an exact figure.
pub(super) fn whole_spoken(x10: u16) -> u16 {
    if x10 >= 1000 {
        100
    } else {
        ((x10 + 5) / 10).min(99)
    }
}

impl QuotaView {
    /// Case (b) of the glass design (§1) as a pane's view, built by hand for other crates' tests
    /// of what the window draws from a view: `main` at 63% of its week, its reset Sat 10:00
    /// (`~99%` by then), a gap projected from ~Thu 12 Nov 04:00 to its next reset, Sat 14 Nov
    /// 10:00; no chart. Its words are case (b)'s replay's (`case_b_lays_r2_r5_r7` holds them
    /// equal); the outlook's own tests build views from replays ([`Outlook::quota`]).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn sample() -> QuotaView {
        QuotaView {
            named: vec![Named {
                label: "main".into(),
                name: "main".into(),
                this_tab: true,
                carrier: true,
                figure: Figure::Exact(630),
                reset: None,
                plan: Some("Max 20x".into()),
            }],
            tail: Tail {
                own: Some(Own {
                    by_reset: Some(99),
                    reset: "Sat 10:00".into(),
                }),
                verdict: Some(Phrase {
                    full: format!("{GAP_MARK} ~Thu 12 Nov 04:00\u{2013}Sat 14 Nov 10:00"),
                    short: format!("{GAP_MARK} ~Thu 12 Nov 04:00"),
                    ink: Role::Warn,
                    kept: true,
                }),
            },
            title: format!(
                "main{SEP}Max 20x{SEP}63% of its week used, read now{SEP}~99% by its reset Sat \
                 10:00{SEP}next week out ~Thu 12 Nov 04:00 until its reset Sat 14 Nov 10:00{SEP}\
                 at this Mac's last-24 h pace, 21% a day{SEP}{CLICK}"
            ),
            spoken: "Accounts. main, this tab, 63 percent of its week used, resets Saturday \
                     10:00. Next 7 days at 21 percent a day: main; next week out from about \
                     Thursday 12 November 04:00 until its reset Saturday 14 November 10:00."
                .into(),
            ..QuotaView::default()
        }
    }

    /// The hover title with `clause` said last before the click's words (the rotation's,
    /// `footer::rotation_words`, §5.1); the title alone where there is none.
    #[must_use]
    pub fn title_with(&self, clause: Option<&str>) -> String {
        let Some(clause) = clause.filter(|c| !c.is_empty()) else {
            return self.title.clone();
        };
        match self.title.strip_suffix(CLICK) {
            Some(head) => format!("{head}{clause}{SEP}{CLICK}"),
            None if self.title.is_empty() => clause.to_owned(),
            None => format!("{}{SEP}{clause}", self.title),
        }
    }

    /// This tab's named account.
    #[must_use]
    pub fn tab(&self) -> Option<&Named> {
        self.named.iter().find(|n| n.this_tab)
    }

    /// Case (d): this tab's week has no figure yet, and no chart is drawn until its first reading.
    #[must_use]
    pub fn waiting(&self) -> bool {
        self.tab().is_none_or(|n| n.figure == Figure::Unread)
    }

    /// Lays the row into `out` at the widest rung of §2.4 that fits `room` cells: `false`, with
    /// `out` empty, where none does. Case (d) has its own three forms: this tab's name with its
    /// words full, then short, then its name's local part with the short words.
    pub fn lay(&self, room: usize, out: &mut QuotaLine) -> bool {
        let Some(tab) = self.tab() else {
            out.clear();
            return false;
        };
        if self.waiting() {
            for (full, names) in [
                (true, Names::Whole),
                (false, Names::Whole),
                (false, Names::Local),
            ] {
                out.clear();
                out.put(&names.lay(&tab.name), Role::Lit, true);
                out.lead = 0..out.cells.len();
                if let Some(words) = &self.tail.verdict {
                    out.gap();
                    let from = out.cells.len();
                    out.put(
                        if full { &words.full } else { &words.short },
                        words.ink,
                        false,
                    );
                    out.tail = from..out.cells.len();
                }
                if out.cells.len() <= room {
                    return true;
                }
            }
            out.clear();
            return false;
        }
        let mut chart = Vec::new();
        for (lead, names, per_day, tail) in LADDER {
            out.clear();
            self.write_lead(lead, names, out);
            out.lead = 0..out.cells.len();
            if per_day > 0 {
                out.gap();
                let from = out.cells.len();
                self.chart.cells(per_day, &mut chart);
                out.cells.extend(
                    chart
                        .iter()
                        .map(|c| (c.glyph(), c.role(), heavy(c.glyph(), c.role()))),
                );
                out.chart = from..out.cells.len();
                out.per_day = per_day;
                self.chart
                    .ticks(per_day, (0, Cell::ticks), &chart, &mut out.ticks);
            }
            if self.tail.own.is_some() || self.tail.verdict.is_some() {
                out.gap();
                let from = out.cells.len();
                self.write_tail(tail, out);
                out.tail = from..out.cells.len();
            }
            if out.cells.len() <= room {
                return true;
            }
        }
        out.clear();
        false
    }

    fn write_lead(&self, lead: Lead, names: Names, out: &mut QuotaLine) {
        let one = |n: &Named, out: &mut QuotaLine| {
            let role = if n.this_tab { Role::Lit } else { Role::Value };
            out.put(&names.lay(&n.name), role, n.this_tab);
            out.put(" ", Role::Quiet, false);
            out.put(&n.figure.words(), role, n.this_tab);
        };
        match lead {
            Lead::Full => {
                for (i, n) in self.named.iter().enumerate() {
                    if i > 0 {
                        out.put(SEP, Role::Quiet, false);
                    }
                    one(n, out);
                }
                if !self.folded.is_empty() {
                    out.put(&format!(" +{}", self.folded.len()), Role::Quiet, false);
                } else if self.named.len() == 1 {
                    out.put(" used", Role::Quiet, false);
                }
            }
            Lead::Fold | Lead::Tab => {
                if let Some(tab) = self.tab() {
                    one(tab, out);
                }
                let others = self.named.len().saturating_sub(1) + self.folded.len();
                if lead == Lead::Fold && others > 0 {
                    out.put(&format!(" +{others}"), Role::Quiet, false);
                }
            }
        }
    }

    fn write_own(own: &Own, full: bool, out: &mut QuotaLine) {
        if let Some(pct) = own.by_reset.filter(|_| full) {
            out.put(&format!("~{pct}%"), Role::Value, false);
            out.put(" by ", Role::Quiet, false);
        }
        out.put(&RESET_MARK.to_string(), Role::Lit, false);
        out.put(" ", Role::Quiet, false);
        out.put(&own.reset, Role::Value, false);
    }

    fn write_tail(&self, tail: TailForm, out: &mut QuotaLine) {
        let full = tail == TailForm::T0;
        let own = match tail {
            TailForm::T2 if self.tail.verdict.is_some() => None,
            _ => self.tail.own.as_ref(),
        };
        if let Some(own) = own {
            QuotaView::write_own(own, full, out);
        }
        if let Some(verdict) = &self.tail.verdict {
            if own.is_some() {
                out.put(SEP, Role::Quiet, false);
            }
            out.put(
                if full { &verdict.full } else { &verdict.short },
                verdict.ink,
                false,
            );
        }
    }

    /// What the pointer on cell `cell` of the chart laid at `per_day` cells a day names (§2.6),
    /// in local time by `offsets`: a past cell its span and the reading it holds (`Wed 4 Nov
    /// 20:00–23:00 · main 50% (read on this Mac)`); a future level its span and the drawn
    /// account's projected fill (`~Tue 10 Nov 14:00–17:00 · main ~69% (projected)`); a `→` the
    /// hand-off (`~Fri 08:00 · the work moves from main (98%) to alt1`; `(out)` for a fill the
    /// projection reaches, never 100%; a move where a wall lifts at a named account's reset said
    /// at that reset exactly, `Sat 10:00 · the work moves to main`), and a name after it the
    /// same; a `╳` its stop (`~Sat 05:00–Sat 10:00 · every account out until main's reset`); a
    /// `$` its span on credits; a `↺` the reset; `┄` that nothing is projected there. `None` for
    /// the now cell, which holds no time, and past the chart's end: the row's own title stands.
    #[must_use]
    pub fn column_title(&self, per_day: u8, cell: usize, offsets: &Offsets) -> Option<String> {
        let chart = &self.chart;
        let (a, b) = chart.span_of(per_day, cell)?;
        let mut cells = Vec::new();
        chart.cells(per_day, &mut cells);
        let k = usize::from(per_day);
        let s = DAY / i64::from(per_day);
        let now = chart.now;
        let tab = self.tab().map_or(THIS_LOGIN, |n| n.name.as_str());
        let span = span_dated(a, b, offsets);
        let about = |t: i64| format!("~{}", when(round_down(t, now, offsets), now, offsets));
        if cell < k {
            return Some(match cells.get(cell)? {
                Cell::Read(_) => {
                    let i = chart.past.partition_point(|r| r.t <= b);
                    let r = chart.past.get(i.checked_sub(1)?)?;
                    format!(
                        "{span}{SEP}{} {}% (read on this Mac)",
                        cut(tab),
                        whole_spoken(r.x10)
                    )
                }
                Cell::Reset => format!("{span}{SEP}{}'s week reset", cut(tab)),
                _ => format!("{span}{SEP}no reading on this Mac"),
            });
        }
        let j = cell - k - 1;
        // Whose week reset `t` is, among the named accounts.
        let reset_of = |t: i64| {
            self.named
                .iter()
                .find(|n| n.reset.is_some_and(|r| (t - r).rem_euclid(WEEK) == 0))
                .map(|n| cut(&n.name))
        };
        let run_at = |t: i64| {
            chart
                .runs
                .iter()
                .rev()
                .find(|r| r.from < t && t <= r.to)
                .or_else(|| chart.runs.iter().rev().find(|r| r.from < t))
        };
        let words = match cells.get(cell)? {
            Cell::Gap | Cell::Credits => {
                let credits = matches!(cells[cell], Cell::Credits);
                let &(from, to, _) = chart
                    .stops
                    .iter()
                    .filter(|(_, _, c)| *c == credits)
                    .find(|(from, to, _)| (to.min(&b) - from.max(&a)) * 2 >= s)
                    .or_else(|| {
                        chart
                            .stops
                            .iter()
                            .filter(|(_, _, c)| *c == credits)
                            .find(|(from, to, _)| *to > a && *from < b)
                    })?;
                if credits {
                    format!(
                        "{}\u{2013}~{}{SEP}the work runs on usage credits",
                        about(from),
                        when(round_up(to, now, offsets), now, offsets)
                    )
                } else if to >= now + 7 * DAY {
                    format!("from {}{SEP}every account out", about(from))
                } else if let Some(label) = reset_of(to) {
                    format!(
                        "{}\u{2013}{}{SEP}every account out until {label}'s reset",
                        about(from),
                        when(to, now, offsets)
                    )
                } else {
                    format!(
                        "{}\u{2013}~{}{SEP}every account out",
                        about(from),
                        when(round_up(to, now, offsets), now, offsets)
                    )
                }
            }
            Cell::Handoff | Cell::Name(_) => {
                let arrow = (0..=j)
                    .rev()
                    .find(|&i| matches!(cells[k + 1 + i], Cell::Handoff))?;
                // The hand-off it draws: the last whose own cell is the arrow's, or one a gap
                // moved there.
                let (t, mark) = chart
                    .marks
                    .iter()
                    .filter(|(_, m)| matches!(m, Mark::Handoff(_) | Mark::Lean(_)))
                    .rfind(|(t, _)| {
                        future_index(now, s, cells.len() - k - 1, *t).is_some_and(|at| {
                            at <= arrow
                                && (at..arrow).all(|i| matches!(cells[k + 1 + i], Cell::Gap))
                        })
                    })?;
                match mark {
                    Mark::Handoff(to) => {
                        let from = chart.runs.iter().find(|r| r.to == *t && &r.name != to);
                        // A projected fill is never said as 100% (§2.1): one that reaches it is
                        // `out`, as a level's title says. A move at a vendor reset is said
                        // exactly, as the vendor says it (§2.3); any other is a forecast.
                        let pct = from
                            .and_then(|r| r.at(*t))
                            .map(|x| match x {
                                1000.. => " (out)".to_owned(),
                                x => format!(" ({}%)", whole_under_full(x)),
                            })
                            .unwrap_or_default();
                        // The work resumes where a wall lifts, `RESET_GRACE_S` after the reset
                        // it waited for; the glass says that reset (§2.3).
                        let reset = self.named.iter().find_map(|n| {
                            let late = (*t - n.reset?).rem_euclid(WEEK);
                            (late <= accounts::RESET_GRACE_S).then_some(*t - late)
                        });
                        let at = reset.map_or_else(|| about(*t), |r| when(r, now, offsets));
                        match from {
                            Some(from) => format!(
                                "{at}{SEP}the work moves from {}{pct} to {}",
                                cut(&from.name),
                                cut(to)
                            ),
                            None => format!("{at}{SEP}the work moves to {}", cut(to)),
                        }
                    }
                    Mark::Lean(to) => format!(
                        "{}{SEP}the work moves to {}, which aterm has not read",
                        about(*t),
                        cut(to)
                    ),
                    Mark::Reset => return None,
                }
            }
            Cell::Reset => {
                let t = chart
                    .marks
                    .iter()
                    .find(|(t, m)| matches!(m, Mark::Reset) && (a..b).contains(t))
                    .map_or(a, |(t, _)| *t);
                let label = reset_of(t)
                    .or_else(|| run_at(t + 1).map(|r| cut(&r.name)))
                    .unwrap_or_else(|| cut(tab));
                format!("{}{SEP}{label}'s week resets", when(t, now, offsets))
            }
            Cell::Ahead(_) => {
                let run = run_at(b)?;
                match run.at(b.min(run.to)) {
                    Some(x10) if x10 >= 1000 => {
                        format!("~{span}{SEP}{} out (projected)", cut(&run.name))
                    }
                    Some(x10) => format!(
                        "~{span}{SEP}{} ~{}% (projected)",
                        cut(&run.name),
                        whole_under_full(x10)
                    ),
                    None => format!("~{span}{SEP}not projected"),
                }
            }
            Cell::Unknown => format!("~{span}{SEP}not projected"),
            // A lane's cell, which line E's chart never draws; said as what it means.
            Cell::Out => format!("~{span}{SEP}projected out"),
            Cell::Blank | Cell::Now | Cell::Read(_) => format!("~{span}{SEP}nothing projected"),
        };
        Some(words)
    }

    /// Line D's words-only unit (§1.2), for a pane whose line E is not the glass's: its forms,
    /// widest first: name, figure, `↺`, verdict; name, figure, verdict; name and figure; the
    /// name's local part and figure; the local part alone (§2.5). The facts drop before the unit
    /// shortens; each form is its runs of words with their inks.
    #[must_use]
    pub fn fallback(&self) -> Vec<Vec<Piece>> {
        let Some(tab) = self.tab() else {
            return Vec::new();
        };
        let name = Piece::new(cut(&tab.name), Role::Lit);
        let local = Piece::new(short(&tab.name), Role::Lit);
        if self.waiting() {
            let mut forms = vec![
                vec![
                    name.clone(),
                    Piece::new(format!(" {NO_FIGURE}"), Role::Quiet),
                ],
                vec![name],
            ];
            if forms.last() != Some(&vec![local.clone()]) {
                forms.push(vec![local]);
            }
            return forms;
        }
        let figure = [
            Piece::new(" ", Role::Quiet),
            Piece::new(tab.figure.words(), Role::Value),
        ];
        let reset = self.tail.own.as_ref().map(|own| {
            [
                Piece::new(SEP, Role::Quiet),
                Piece::new(RESET_MARK.to_string(), Role::Lit),
                Piece::new(format!(" {}", own.reset), Role::Value),
            ]
        });
        let verdict = self.tail.verdict.as_ref().map(|v| {
            [
                Piece::new(SEP, Role::Quiet),
                Piece::new(v.short.clone(), v.ink),
            ]
        });
        let mut forms: Vec<Vec<Piece>> = Vec::new();
        for (with_reset, with_verdict, name) in [
            (true, true, &name),
            (false, true, &name),
            (false, false, &name),
            (false, false, &local),
        ] {
            let mut form = vec![name.clone()];
            form.extend(figure.iter().cloned());
            if with_reset && let Some(r) = &reset {
                form.extend(r.iter().cloned());
            }
            if with_verdict && let Some(v) = &verdict {
                form.extend(v.iter().cloned());
            }
            if forms.last() != Some(&form) {
                forms.push(form);
            }
        }
        forms.push(vec![local]);
        forms
    }
}

#[cfg(test)]
#[path = "quota_view_tests.rs"]
mod tests;

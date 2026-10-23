// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The glass: ranking live rows and laying out each message as a single `cols`-wide row of cells over its meter.

use std::cmp::Reverse;

use crate::center::Live;
use crate::model::{ActionIndex, Hold, Intent, Load, Loads, MessageId, Severity};
use crate::text::{lead_clause, shape_detail};
use crate::{
    BEFORE_CAPSULES, CAPSULE_GAP, DETAIL_FLOOR, ELAPSED_SHORT_W, ELAPSED_W, ETA_SHORT_W, ETA_W,
    GLYPH_COL, Instant, MARGIN, PCT_W, TITLE_COL, TITLE_MIN,
};

/// The glass rank of a live row, highest first.
pub(crate) type Rank = (bool, u8, bool, Reverse<Instant>, Reverse<MessageId>);

/// The rank's severity term.
#[inline]
#[must_use]
pub(crate) const fn severity_class(severity: Severity) -> u8 {
    match severity {
        Severity::Success | Severity::Info => 0,
        Severity::Warn => 1,
        Severity::Error => 2,
    }
}

/// The rank tuple of one row.
#[inline]
#[must_use]
pub(crate) fn rank(live: &Live) -> Rank {
    (
        live.msg.is_ask(),
        severity_class(live.msg.severity).saturating_sub(u8::from(live.msg.retrospective)),
        matches!(live.msg.hold, Hold::Live { .. } | Hold::Standing(_)),
        Reverse(live.posted_at),
        Reverse(live.id),
    )
}

/// `true` when `a` outranks `b` strictly.
#[inline]
#[must_use]
pub(crate) fn outranks(a: &Live, b: &Live) -> bool {
    rank(a) > rank(b)
}

/// A laid-out band: `rows.len() ≤ committed`, top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presentation {
    /// The width every row was laid out at.
    pub cols: usize,
    /// The rows, top to bottom (the overflow row last when present).
    pub rows: Vec<RowLayout>,
}

/// What a row stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// One message.
    Message(MessageId),
    /// A retired live row's completion echo in the slot it held.
    Echo(MessageId),
    /// `N more messages` — one link to Settings ▸ Messages.
    Overflow {
        /// How many eligible rows are not on glass.
        hidden: usize,
    },
}

/// One row in cells.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent facts the painter reads — live ink, busy, and each time slot's short form — each set once by the width law"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowLayout {
    /// Message or overflow.
    pub kind: RowKind,
    /// For the ink.
    pub severity: Severity,
    /// A live/standing row (metered or unfinished).
    pub live: bool,
    /// The glyph and its column.
    pub glyph: (usize, char),
    /// The (possibly elided) title and its column.
    pub title: (usize, String),
    /// The title in full — what is spoken, width-independent.
    pub full_title: String,
    /// The excerpt text and its column, when it fits.
    pub detail: Option<(usize, String)>,
    /// `(col, width, fill_permille)` of the row's meter: always `(0, cols, fill)`, the whole row, whenever the message carries a fill.
    pub meter: Option<(usize, usize, u16)>,
    /// Work in flight with no fill ([`crate::Meter::busy`]): the motion layer sweeps the comet along the track.
    pub busy: bool,
    /// `(col, width)` of a busy row's track: always `(0, cols)`, the whole row, like a meter.
    pub track: Option<(usize, usize)>,
    /// The `NN%` text and its column (right-aligned in its four cells).
    pub pct: Option<(usize, String)>,
    /// The elapsed slot's column ([`Self::elapsed_width`] cells: [`ELAPSED_W`], or [`ELAPSED_SHORT_W`] where the long form did not fit) of a busy row.
    pub elapsed: Option<usize>,
    /// The elapsed slot is in its short form (`for 3m`, [`ELAPSED_SHORT_W`] cells).
    pub elapsed_short: bool,
    /// The ETA slot's column ([`Self::eta_width`] cells: [`ETA_W`], or [`ETA_SHORT_W`] where the long form did not fit).
    pub eta: Option<usize>,
    /// The ETA slot is in its short form (`59m left`, [`ETA_SHORT_W`] cells).
    pub eta_short: bool,
    /// The heavy-load words and their column, right-aligned in the load slot, when they show.
    pub load: Option<(usize, &'static str)>,
    /// The reserved load slot's column and width, words or not.
    pub load_slot: Option<(usize, usize)>,
    /// The stats text and its column.
    pub stats: Option<(usize, String)>,
    /// The capsules, left to right.
    pub capsules: Vec<CapsuleLayout>,
}

/// Which chip a capsule is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapsuleRole {
    /// A consequential authored intent (Install now, Install, a system pane, New window): accent fill.
    Primary,
    /// Every other authored intent.
    Secondary,
    /// The implicit `Details ›` (or the overflow row's `Messages ›`).
    Details,
}

/// One chip: ` text ` over `width` cells from `col`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapsuleLayout {
    /// First cell.
    pub col: usize,
    /// `text + 2` pad cells.
    pub width: usize,
    /// The long or short form, as fitted.
    pub text: String,
    /// The full label — what a screen reader says.
    pub full_label: &'static str,
    /// Its ink.
    pub role: CapsuleRole,
    /// What a press does.
    pub action: ActionIndex,
}

/// What a press at `(row, col)` lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The row body: opens Details.
    Body(MessageId),
    /// An authored capsule.
    Capsule(MessageId, ActionIndex),
    /// The implicit `Details ›`.
    Details(MessageId),
    /// The overflow row, anywhere on it.
    Overflow,
    /// Off the band.
    Nothing,
}

impl Presentation {
    /// What a press at `(row, col)` lands on.
    #[must_use]
    pub fn hit(&self, row: usize, col: usize) -> Hit {
        let Some(layout) = self.rows.get(row) else {
            return Hit::Nothing;
        };
        if col >= self.cols {
            return Hit::Nothing;
        }
        let capsule = layout
            .capsules
            .iter()
            .find(|c| col >= c.col && col < c.col + c.width);
        match (layout.kind, capsule) {
            (RowKind::Overflow { .. }, _) => Hit::Overflow,
            (RowKind::Echo(_), _) => Hit::Nothing,
            (RowKind::Message(id), Some(c)) if c.action.is_details() => Hit::Details(id),
            (RowKind::Message(id), Some(c)) => Hit::Capsule(id, c.action),
            (RowKind::Message(id), None) => Hit::Body(id),
        }
    }

    /// The repaint-key term: exactly `0` with no rows, else a nonzero FNV-1a over everything the painter reads.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        if self.rows.is_empty() {
            return 0;
        }
        let mut h = Fnv::new();
        h.num(self.cols as u64);
        for row in &self.rows {
            match row.kind {
                RowKind::Message(id) => {
                    h.byte(0);
                    h.num(id.raw());
                }
                RowKind::Overflow { hidden } => {
                    h.byte(1);
                    h.num(hidden as u64);
                }
                RowKind::Echo(id) => {
                    h.byte(2);
                    h.num(id.raw());
                }
            }
            h.byte(row.severity as u8);
            h.byte(u8::from(row.live));
            h.num(row.glyph.0 as u64);
            h.str(row.glyph.1.encode_utf8(&mut [0; 4]));
            h.num(row.title.0 as u64);
            h.str(&row.title.1);
            h.opt(row.detail.as_ref());
            h.opt(row.pct.as_ref());
            h.opt(row.stats.as_ref());
            match row.meter {
                Some((col, width, fill)) => {
                    h.byte(1);
                    h.num(col as u64);
                    h.num(width as u64);
                    h.num(u64::from(fill / 10));
                }
                None => h.byte(0),
            }
            h.byte(u8::from(row.busy));
            match row.track {
                Some((col, width)) => {
                    h.byte(1);
                    h.num(col as u64);
                    h.num(width as u64);
                }
                None => h.byte(0),
            }
            h.num(row.elapsed.map_or(0, |c| c as u64 + 1));
            h.num(row.eta.map_or(0, |c| c as u64 + 1));
            h.byte(u8::from(row.eta_short));
            h.byte(u8::from(row.elapsed_short));
            match row.load {
                Some((col, words)) => {
                    h.num(col as u64 + 1);
                    h.str(words);
                }
                None => h.byte(0),
            }
            match row.load_slot {
                Some((col, w)) => {
                    h.num(col as u64 + 1);
                    h.num(w as u64);
                }
                None => h.byte(0),
            }
            for c in &row.capsules {
                h.num(c.col as u64);
                h.num(c.width as u64);
                h.str(&c.text);
                h.byte(c.role as u8);
                h.byte(c.action.0);
            }
        }
        h.finish()
    }
}

/// FNV-1a over typed fields with separators.
pub(crate) struct Fnv(u64);

impl Fnv {
    pub(crate) const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub(crate) fn byte(&mut self, b: u8) {
        self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }

    pub(crate) fn num(&mut self, n: u64) {
        for b in n.to_le_bytes() {
            self.byte(b);
        }
    }

    pub(crate) fn str(&mut self, s: &str) {
        for b in s.bytes() {
            self.byte(b);
        }
        self.byte(0);
    }

    fn opt(&mut self, field: Option<&(usize, String)>) {
        match field {
            Some((col, text)) => {
                self.byte(1);
                self.num(*col as u64);
                self.str(text);
            }
            None => self.byte(0),
        }
    }

    pub(crate) const fn finish(&self) -> u64 {
        self.0 | 1
    }
}

impl RowLayout {
    /// The ETA slot's cells: [`ETA_W`], or [`ETA_SHORT_W`] in its short form; 0 with no slot.
    #[must_use]
    pub fn eta_width(&self) -> usize {
        match (self.eta, self.eta_short) {
            (None, _) => 0,
            (Some(_), false) => ETA_W,
            (Some(_), true) => ETA_SHORT_W,
        }
    }

    /// The elapsed slot's cells: [`ELAPSED_W`], or [`ELAPSED_SHORT_W`] in its short form; 0 with no slot.
    #[must_use]
    pub fn elapsed_width(&self) -> usize {
        match (self.elapsed, self.elapsed_short) {
            (None, _) => 0,
            (Some(_), false) => ELAPSED_W,
            (Some(_), true) => ELAPSED_SHORT_W,
        }
    }

    /// Width-independent spoken sentence (`title` or `title · <detail[0]>`) with pictographic joints spoken as words ([`speakable`]).
    #[must_use]
    pub fn spoken(&self, full_detail0: &str) -> String {
        if full_detail0.is_empty() {
            speakable(&self.full_title)
        } else {
            speakable(&format!("{} \u{00b7} {full_detail0}", self.full_title))
        }
    }
}

/// The painted words as a screen reader should say them.
#[must_use]
pub(crate) fn speakable(words: &str) -> String {
    let chars: Vec<char> = words.chars().collect();
    let mut out = String::with_capacity(words.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let spaced =
            |i: usize| i > 0 && chars.get(i - 1) == Some(&' ') && chars.get(i + 1) == Some(&' ');
        match c {
            '\u{2192}' if spaced(i) => {
                out.pop();
                out.push_str(", did you mean ");
                i += 2;
                continue;
            }
            '\u{25b8}' if spaced(i) => {
                out.pop();
                out.push_str(", ");
                i += 2;
                continue;
            }
            '\u{00d7}'
                if i > 0
                    && chars[i - 1].is_ascii_digit()
                    && chars.get(i + 1).is_some_and(char::is_ascii_digit) =>
            {
                out.push_str(" by ");
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// Whether the implicit links are laid out: a row's `Details ›` (`+N ›` on a single row) and the overflow row's `Messages ›`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Links {
    /// Authored capsules only; the overflow row is words alone.
    Withheld,
    /// Every row ends in its link.
    Painted,
}

/// One capsule before layout: both forms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapsuleSpec {
    /// The long form, as painted.
    pub long: String,
    /// The short form the width law falls back to; empty for a capsule with none, which the law drops instead (the implicit `Details ›`).
    pub short: String,
    /// The long form the row pays for.
    pub paid: String,
    /// The full label a screen reader says.
    pub full_label: &'static str,
    /// Its ink.
    pub role: CapsuleRole,
    /// What a press does.
    pub action: ActionIndex,
}

impl CapsuleSpec {
    /// The capsule for an authored intent at `index`.
    #[must_use]
    pub(crate) fn authored(intent: &Intent, index: u8) -> Self {
        Self {
            long: intent.label().to_string(),
            short: intent.short().to_string(),
            paid: intent.label().to_string(),
            full_label: intent.label(),
            role: if intent.is_consequential() {
                CapsuleRole::Primary
            } else {
                CapsuleRole::Secondary
            },
            action: ActionIndex(index),
        }
    }

    /// This capsule with its cells kept and nothing drawn in them.
    #[must_use]
    pub fn blanked(self) -> Self {
        let blank = |s: &str| " ".repeat(s.chars().count());
        Self {
            long: blank(&self.long),
            short: blank(&self.short),
            paid: blank(&self.paid),
            full_label: "",
            role: CapsuleRole::Details,
            action: self.action,
        }
    }

    /// The implicit `Details ›`; with `hidden > 0` behind a single committed row it reads `+N ›`.
    #[must_use]
    pub(crate) fn details(hidden: usize) -> Self {
        let (long, short) = if hidden > 0 {
            (format!("+{hidden} \u{203a}"), format!("+{hidden}\u{203a}"))
        } else {
            ("Details \u{203a}".to_string(), String::new())
        };
        Self {
            paid: long.clone(),
            long,
            short,
            full_label: Intent::Details.label(),
            role: CapsuleRole::Details,
            action: ActionIndex::DETAILS,
        }
    }
}

/// One row before layout — the pure input to the width law, so the law is testable on literal fixtures without a center.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "five independent inputs to the width law (live ink, busy, moving fill, ETA slot, load slot), each read once; a state enum would multiply them"
)]
pub(crate) struct RowSpec<'a> {
    /// Message or overflow.
    pub kind: RowKind,
    /// For the ink.
    pub severity: Severity,
    /// A live/standing row.
    pub live: bool,
    /// The glyph.
    pub glyph: char,
    /// The full title.
    pub title: &'a str,
    /// The title's short form ([`crate::Message`]'s `short_title`).
    pub title_short: Option<&'a str>,
    /// `detail[0]`, when any.
    pub detail0: Option<&'a str>,
    /// `detail0` IS THE ONE STEP the row asks for.
    pub step: bool,
    /// The step's short form ([`crate::need::Then::short`]).
    pub step_short: Option<&'a str>,
    /// `(fill_permille, stats)` when metered; a `None` fill draws no meter and no pct (its stats alone — or, with `busy`, the track).
    pub meter: Option<(Option<u16>, &'a str)>,
    /// Work in flight with no fill ([`crate::Meter::busy`]): the whole row is its track, and the elapsed slot is part of the fixed head.
    pub busy: bool,
    /// The row's fill moves (a live row's glide and glint): its percent may bring the ETA slot and the load slot.
    pub animated: bool,
    /// Reserve the ETA slot (a live determinate row whose reporter supplies an amount).
    pub eta: bool,
    /// The heavy-load words, when they show.
    pub load: Option<Load>,
    /// The loads the row reserves the load slot for (it has declared a load).
    pub load_slot: Loads,
    /// The capsules, left to right (authored first, `Details ›` last).
    pub capsules: Vec<CapsuleSpec>,
}

impl RowSpec<'_> {
    /// The activity cells the fixed head reserves (module doc, step 1).
    fn activity(&self) -> Activity {
        match (self.fill(), self.busy, self.animated) {
            (Some(_), _, true) => Activity::LiveBar,
            (Some(_), _, false) => Activity::HeldBar,
            (None, true, _) => Activity::LiveBusy,
            (None, false, _) => Activity::None,
        }
    }

    /// The fill, clamped.
    fn fill(&self) -> Option<u16> {
        self.meter.and_then(|(f, _)| f).map(|f| f.min(1000))
    }
}

/// What the fixed head reserves for a row's activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Activity {
    /// Nothing.
    None,
    /// A moving fill: the percent (and the ETA and load slots it may bring).
    LiveBar,
    /// A busy row: the elapsed slot (and the load slot it may bring).
    LiveBusy,
    /// A still fill: the percent.
    HeldBar,
}

impl Activity {
    const fn cells(self) -> usize {
        match self {
            Self::None => 0,
            Self::LiveBar | Self::HeldBar => PCT_W,
            Self::LiveBusy => 1 + ELAPSED_SHORT_W,
        }
    }
}

/// The overflow row's spec: one link to Settings ▸ Messages, its words the whole row, with no capsule of its own.
#[must_use]
pub(crate) fn overflow_spec(hidden: usize, words: &str) -> RowSpec<'_> {
    RowSpec {
        kind: RowKind::Overflow { hidden },
        severity: Severity::Info,
        live: false,
        glyph: OVERFLOW_GLYPH,
        title: words,
        title_short: None,
        detail0: None,
        step: false,
        step_short: None,
        meter: None,
        busy: false,
        animated: false,
        eta: false,
        load: None,
        load_slot: Loads::NONE,
        capsules: Vec::new(),
    }
}

/// The overflow row's glyph: `…` for `N more ›`.
const OVERFLOW_GLYPH: char = '\u{2026}';
/// The overflow row's glyph when it names a hidden progress row: `+`.
const OVERFLOW_PROGRESS_GLYPH: char = '+';

/// The link's arrow, painted with the links only.
fn overflow_arrow(links: Links) -> &'static str {
    match links {
        Links::Painted => " \u{203a}",
        Links::Withheld => "",
    }
}

/// `N more ›` — the overflow row's short form, and its only form when no
fn overflow_short(hidden: usize, arrow: &str) -> String {
    format!("{hidden} more{arrow}")
}

/// The overflow row's long form ([`overflow_spec`]).
#[must_use]
pub(crate) fn overflow_words(hidden: usize, links: Links, progress: Option<&str>) -> String {
    let arrow = overflow_arrow(links);
    match progress.filter(|p| !p.is_empty()) {
        Some(p) if hidden > 1 => format!("{p} \u{b7} {} more{arrow}", hidden - 1),
        Some(p) => format!("{p}{arrow}"),
        None => overflow_short(hidden, arrow),
    }
}

/// The overflow row's words at `budget` cells: the long form whole when it
fn overflow_fit(
    hidden: usize,
    long: &str,
    budget: usize,
    width: &dyn Fn(&str) -> usize,
) -> (String, char) {
    let arrow = if long.ends_with('\u{203a}') {
        " \u{203a}"
    } else {
        ""
    };
    let short = overflow_short(hidden, arrow);
    if long != short && 2 + width(long) <= budget {
        (long.to_string(), OVERFLOW_PROGRESS_GLYPH)
    } else {
        (short, OVERFLOW_GLYPH)
    }
}

/// What a screen reader says for the overflow row: its words without the
fn overflow_spoken(words: &str) -> String {
    let bare = words.trim_end_matches('\u{203a}').trim_end();
    match bare.strip_suffix(" more") {
        Some(head) => format!("{head} more messages"),
        None => bare.to_string(),
    }
}

/// What a cut title may leave trailing before its `…`: the space, and a
const TITLE_CUT_TRAIL: [char; 14] = [
    ' ', ',', ';', ':', '.', '\u{b7}', '\u{2014}', '\u{2013}', '\u{3001}', '\u{3002}', '\u{ff0c}',
    '\u{ff1a}', '\u{ff1b}', '\u{30fb}',
];

/// `title` in at most `max` cells under `width`, CUT ONLY AFTER A WHOLE
fn title_in(title: &str, max: usize, width: &dyn Fn(&str) -> usize) -> Option<String> {
    if width(title) <= max {
        return Some(title.to_string());
    }
    let mut kept = None;
    for brk in aterm_grapheme::word_breaks(title) {
        let head = title[..brk.at].trim_end_matches(TITLE_CUT_TRAIL);
        if head.is_empty() {
            continue;
        }
        let cut = format!("{head}\u{2026}");
        if width(&cut) > max {
            break;
        }
        kept = Some(cut);
    }
    kept
}

/// The first word of `title` ([`title_in`]'s words), `…` after it where
fn first_word(title: &str) -> String {
    match aterm_grapheme::word_breaks(title).first() {
        Some(brk) => format!(
            "{}\u{2026}",
            title[..brk.at].trim_end_matches(TITLE_CUT_TRAIL)
        ),
        None => title.to_string(),
    }
}

/// The first cell right of the title that another piece of `row` paints —
fn next_piece(row: &RowLayout, cols: usize) -> usize {
    let mut next = cols.saturating_sub(MARGIN);
    let mut at = |c: usize| next = next.min(c);
    if let Some((c, _)) = &row.detail {
        at(c.saturating_sub(2));
    }
    if let Some((c, _)) = &row.pct {
        at(*c);
    }
    row.elapsed.into_iter().chain(row.eta).for_each(&mut at);
    if let Some((c, _)) = row.load_slot {
        at(c);
    }
    if let Some((c, _)) = &row.stats {
        at(*c);
    }
    if let Some(cap) = row.capsules.first() {
        at(cap.col);
    }
    next
}

/// A Complete echo's title: `words` (the row's finished form) in the title's place, with nothing else on the row moved.
pub(crate) fn finish_title(
    row: &mut RowLayout,
    words: &str,
    cols: usize,
    width: &dyn Fn(&str) -> usize,
) {
    let laid = width(&row.title.1);
    if laid == 0 {
        return;
    }
    let room = next_piece(row, cols)
        .saturating_sub(row.title.0 + 1)
        .max(laid);
    let mut title = title_in(words, room, width).unwrap_or_default();
    let short = laid.saturating_sub(width(&title));
    title.extend(std::iter::repeat_n(' ', short));
    row.title.1 = title;
    row.full_title = words.to_string();
}

/// The capsules painted in the long or the short forms: every one long; the
fn painted(caps: &[CapsuleSpec], short: bool) -> impl Iterator<Item = &CapsuleSpec> {
    caps.iter().filter(move |c| !short || !c.short.is_empty())
}

/// The cells the capsules take together, as the row pays for them: the
fn capsules_width(caps: &[CapsuleSpec], short: bool, width: &dyn Fn(&str) -> usize) -> usize {
    let (texts, n) = painted(caps, short).fold((0usize, 0usize), |(w, n), c| {
        (w + width(if short { &c.short } else { &c.paid }) + 2, n + 1)
    });
    texts + CAPSULE_GAP * n.saturating_sub(1)
}

/// The clearance before the first capsule — nil when none is painted: it
fn clearance(caps: &[CapsuleSpec], short: bool) -> usize {
    if painted(caps, short).next().is_some() {
        BEFORE_CAPSULES
    } else {
        0
    }
}

/// The fitting decisions the law makes before placement.
struct Fit {
    short: bool,
    title: String,
    /// The glyph: the spec's, or the overflow row's for the form it took.
    glyph: char,
    /// The activity that survived (none in the degenerate step).
    activity: Activity,
    details_dropped: bool,
    room: usize,
    /// A moving row's painted excerpt ([`action_excerpt`]): the cells the
    excerpt_floor: usize,
    /// The row's excerpt is an action excerpt (whether or not its floor
    action_excerpt: bool,
    /// Whose words the excerpt shows: the step's, or its short form's where
    words: ExcerptWords,
}

/// Whose words a row's excerpt shows ([`Fit::words`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExcerptWords {
    /// The row's own first detail line, the ask's step for an ask.
    Detail,
    /// The ask's step's short form ([`RowSpec::step_short`]).
    Short,
}

impl ExcerptWords {
    /// The words of `spec` this names.
    fn of<'a>(self, spec: &RowSpec<'a>) -> Option<&'a str> {
        match self {
            Self::Detail => spec.detail0,
            Self::Short => spec.step_short,
        }
    }
}

/// The load slot's width: the widest words among the loads the row reserves
fn load_slot_width(spec: &RowSpec<'_>, width: &dyn Fn(&str) -> usize) -> usize {
    let reserved = match spec.load {
        Some(l) if !spec.load_slot.is_empty() => spec.load_slot.with(l),
        _ if !spec.load_slot.is_empty() => spec.load_slot,
        _ => Loads::ALL,
    };
    reserved.iter().map(|l| width(l.words())).max().unwrap_or(0)
}

/// Whether the row lays out the load slot: a moving activity on a row that
fn reserves_load(spec: &RowSpec<'_>, activity: Activity) -> bool {
    matches!(activity, Activity::LiveBar | Activity::LiveBusy)
        && (!spec.load_slot.is_empty() || spec.load.is_some())
}

/// The load slot's cells, its two-cell gap included, when the row lays it
fn load_cells(spec: &RowSpec<'_>, activity: Activity, width: &dyn Fn(&str) -> usize) -> usize {
    if reserves_load(spec, activity) && !action_excerpt(spec, activity) {
        2 + load_slot_width(spec, width)
    } else {
        0
    }
}

/// Whether the row's excerpt is an action excerpt: a painted `detail[0]` on
fn action_excerpt(spec: &RowSpec<'_>, activity: Activity) -> bool {
    (spec.step || matches!(activity, Activity::LiveBar | Activity::LiveBusy))
        && spec.detail0.is_some_and(|d| !d.is_empty())
}

/// The cells an action excerpt reserves in the fixed head: its joint and
fn excerpt_floor(spec: &RowSpec<'_>, activity: Activity, width: &dyn Fn(&str) -> usize) -> usize {
    match spec.detail0 {
        Some(d) if action_excerpt(spec, activity) && spec.step => 3 + width(d),
        Some(d) if action_excerpt(spec, activity) => 3 + width(d).min(DETAIL_FLOOR),
        _ => 0,
    }
}

/// The fixed head fitted around an excerpt reserved `ex` cells: step 1's
struct Head {
    title: String,
    /// The words `title` stands for: the whole title, or its short form
    words: String,
    /// The capsules' cells, and the clearance before them.
    caps: usize,
    before: usize,
    short: bool,
    details_dropped: bool,
    /// What the head takes of the budget, the excerpt's `ex` included.
    fixed: usize,
}

/// The title's forms a row may paint: the whole title, and its short form
#[derive(Clone, Copy)]
struct Titles<'t> {
    full: &'t str,
    short: Option<&'t str>,
}

impl<'t> Titles<'t> {
    /// `spec`'s forms, its whole title `full` (an overflow row's words have
    fn of(spec: &RowSpec<'t>, full: &'t str, width: &dyn Fn(&str) -> usize) -> Self {
        let short = spec
            .title_short
            .filter(|_| matches!(spec.kind, RowKind::Message(_) | RowKind::Echo(_)))
            .filter(|s| !s.is_empty() && width(s) < width(full));
        Self { full, short }
    }
}

/// [`Head`] for `spec` within `budget`: its `titles`, `a` cells of activity
fn fit_head(
    spec: &RowSpec<'_>,
    titles: Titles<'_>,
    budget: usize,
    a: usize,
    ex: usize,
    width: &dyn Fn(&str) -> usize,
) -> Head {
    let head = head_around(spec, titles.full, budget, a, ex, width);
    if head.fixed <= budget {
        return head;
    }
    let (title, mut head) = match titles.short {
        Some(short) => (short, head_around(spec, short, budget, a, ex, width)),
        None => (titles.full, head),
    };
    if head.fixed > budget {
        head.title = elide(
            title,
            budget.saturating_sub(2 + a + ex + head.before + head.caps),
            width,
        );
        head.fixed = 2 + width(&head.title) + a + ex + head.before + head.caps;
    }
    head
}

/// The head around `title` whole: the implicit `Details ›` dropped where
fn head_around(
    spec: &RowSpec<'_>,
    title: &str,
    budget: usize,
    a: usize,
    ex: usize,
    width: &dyn Fn(&str) -> usize,
) -> Head {
    let words = title.to_string();
    let title = title.to_string();
    let mut short = false;
    let mut caps = capsules_width(&spec.capsules, false, width);
    let mut before = clearance(&spec.capsules, false);
    let mut fixed = 2 + width(&title) + a + ex + before + caps;
    let mut details_dropped = false;
    if fixed > budget
        && let Some((last, kept)) = spec.capsules.split_last()
        && last.action.is_details()
        && last.short.is_empty()
    {
        let caps_kept = capsules_width(kept, false, width);
        let before_kept = clearance(kept, false);
        let fixed_kept = 2 + width(&title) + a + ex + before_kept + caps_kept;
        if fixed_kept <= budget {
            details_dropped = true;
            caps = caps_kept;
            before = before_kept;
            fixed = fixed_kept;
        }
    }
    if fixed > budget {
        short = true;
        caps = capsules_width(&spec.capsules, true, width);
        before = clearance(&spec.capsules, true);
        fixed = 2 + width(&title) + a + ex + before + caps;
    }
    Head {
        title,
        words,
        caps,
        before,
        short,
        details_dropped,
        fixed,
    }
}

/// `title` elided to the `allowed` cells in whole words ([`title_in`]),
fn elide(title: &str, allowed: usize, width: &dyn Fn(&str) -> usize) -> String {
    title_in(title, allowed.max(TITLE_MIN), width).unwrap_or_else(|| first_word(title))
}

/// Step 1: capsules long → short, then the title, then the degenerate step.
fn fit_fixed(spec: &RowSpec<'_>, cols: usize, width: &dyn Fn(&str) -> usize) -> Fit {
    let budget = cols.saturating_sub(2 * MARGIN);
    let (overflow_title, glyph) = match spec.kind {
        RowKind::Overflow { hidden } => {
            let (words, glyph) = overflow_fit(hidden, spec.title, budget, width);
            (Some(words), glyph)
        }
        RowKind::Message(_) | RowKind::Echo(_) => (None, spec.glyph),
    };
    let full_title = overflow_title.as_deref().unwrap_or(spec.title);
    let titles = Titles::of(spec, full_title, width);
    let mut activity = spec.activity();
    let action_excerpt = action_excerpt(spec, activity);
    let mut a = activity.cells() + load_cells(spec, activity, width);
    let mut ex = excerpt_floor(spec, activity, width);
    let room = budget.saturating_sub(
        2 + width(full_title)
            + a
            + ex
            + clearance(&spec.capsules, false)
            + capsules_width(&spec.capsules, false, width),
    );
    let mut head = fit_head(spec, titles, budget, a, ex, width);
    let mut words = ExcerptWords::Detail;
    if head.fixed > budget && spec.step && ex > 3 + DETAIL_FLOOR {
        // An ask's step that the title at its floor still cannot pay for
        if let Some(short) = spec.step_short {
            words = ExcerptWords::Short;
            ex = 3 + width(short);
            let kept = Titles {
                full: titles.short.unwrap_or(titles.full),
                short: None,
            };
            head = fit_head(spec, kept, budget, a, ex, width);
        }
        let left = budget.saturating_sub(2 + width(&head.title) + a + head.before + head.caps);
        ex = left.max(3 + DETAIL_FLOOR).min(ex);
        head.fixed = 2 + width(&head.title) + a + ex + head.before + head.caps;
    }
    if head.fixed > budget && ex > 0 {
        ex = 0;
        let alone = Titles {
            full: titles.short.unwrap_or(full_title),
            short: None,
        };
        head = fit_head(spec, alone, budget, a, ex, width);
    }
    let Head {
        mut title,
        words: title_words,
        mut caps,
        before,
        short,
        mut details_dropped,
        fixed,
    } = head;
    if fixed > budget {
        activity = Activity::None;
        a = 0;
        let mut clear = before;
        if 2 + a + clear + caps > budget
            && spec.capsules.last().is_some_and(|c| c.action.is_details())
        {
            details_dropped = true;
            let kept = &spec.capsules[..spec.capsules.len() - 1];
            caps = capsules_width(kept, true, width);
            clear = clearance(kept, true);
        }
        let left = budget.saturating_sub(2 + clear + caps);
        let spare = clear.saturating_sub(1);
        title = title_in(&title_words, left, width)
            .or_else(|| {
                if spare > 0 {
                    title_in(&title_words, left + spare, width)
                } else {
                    None
                }
            })
            .unwrap_or_default();
    }
    Fit {
        short,
        title,
        glyph,
        activity,
        details_dropped,
        room,
        excerpt_floor: ex,
        action_excerpt,
        words,
    }
}

/// `detail0` shaped to at most `target` cells under `width`. [`shape_detail`]
fn shape_to_cells(detail0: &str, target: usize, width: &dyn Fn(&str) -> usize) -> Option<String> {
    let whole = detail0.chars().count();
    let mut cap = target;
    loop {
        let shaped = shape_detail(detail0, cap);
        if shaped.is_empty() {
            return None;
        }
        if width(&shaped) <= target {
            return Some(shaped);
        }
        let n = shaped.chars().count();
        cap = if n == whole { n } else { cap }.checked_sub(1)?;
    }
}

/// The excerpt's lead clause where the floor would drop it: [`lead_clause`],
fn floor_clause(detail0: &str, width: &dyn Fn(&str) -> usize) -> Option<String> {
    let lead = lead_clause(detail0)?;
    let floor = shape_to_cells(detail0, DETAIL_FLOOR, width)?;
    (width(&lead) < DETAIL_FLOOR && floor.starts_with(&lead)).then_some(lead)
}

/// An excerpt that is not whole in `cells` (its joint included): shaped to
fn excerpt_cut(
    detail0: &str,
    cells: usize,
    step: bool,
    width: &dyn Fn(&str) -> usize,
) -> Option<String> {
    if cells >= 3 + DETAIL_FLOOR
        && let Some(shaped) = shape_to_cells(detail0, cells - 3, width)
    {
        let said = shaped
            .trim_end_matches('\u{2026}')
            .trim_end()
            .trim_end_matches([':', '\u{00b7}'])
            .trim_end();
        return Some(match lead_clause(detail0) {
            Some(lead) if said == lead && !step => lead,
            _ => shaped,
        });
    }
    floor_clause(detail0, width).filter(|lead| !step && 3 + width(lead) <= cells)
}

/// What the extras won, in cells, before placement.
struct Extras {
    /// The elapsed slot's cells ([`ELAPSED_W`] or [`ELAPSED_SHORT_W`]); 0
    elapsed_w: usize,
    /// The ETA slot's cells ([`ETA_W`] or [`ETA_SHORT_W`]); 0 for none.
    eta_w: usize,
    load_slot: bool,
    load: Option<&'static str>,
    detail: Option<String>,
    stats: Option<String>,
}

/// Lay one row out at `cols` under the injected cell measure.
#[must_use]
pub(crate) fn layout_row(
    spec: &RowSpec<'_>,
    cols: usize,
    width: &dyn Fn(&str) -> usize,
) -> RowLayout {
    layout_row_with(spec, cols, width, false)
}

/// [`layout_row`], with the band's time words forced to their short form where `short_time` (one row at this width took them short, so every row does).
pub(crate) fn layout_row_with(
    spec: &RowSpec<'_>,
    cols: usize,
    width: &dyn Fn(&str) -> usize,
    short_time: bool,
) -> RowLayout {
    let fit = fit_fixed(spec, cols, width);
    let extras = allocate_extras(spec, &fit, width, short_time);
    place(spec, cols, width, &fit, extras)
}

/// Step 2: the extras from the room the fixed head left, in allocation
fn allocate_extras(
    spec: &RowSpec<'_>,
    fit: &Fit,
    width: &dyn Fn(&str) -> usize,
    short_time: bool,
) -> Extras {
    let mut room = fit.room;
    let mut starved = false;
    let mut elapsed_w = 0;
    if matches!(fit.activity, Activity::LiveBusy) {
        if ELAPSED_W - ELAPSED_SHORT_W <= room {
            elapsed_w = if short_time {
                ELAPSED_SHORT_W
            } else {
                ELAPSED_W
            };
            room -= ELAPSED_W - ELAPSED_SHORT_W;
        } else {
            elapsed_w = ELAPSED_SHORT_W;
            starved = true;
        }
    }
    let mut eta_w = 0;
    let mut load_slot = reserves_load(spec, fit.activity) && !fit.action_excerpt;
    if spec.eta && fit.activity == Activity::LiveBar {
        if ETA_W < room {
            eta_w = if short_time { ETA_SHORT_W } else { ETA_W };
            room -= ETA_W - eta_w;
        } else if ETA_SHORT_W < room {
            eta_w = ETA_SHORT_W;
            starved = true;
        } else if load_slot && ETA_SHORT_W < room + 2 + load_slot_width(spec, width) {
            load_slot = false;
            room += 2 + load_slot_width(spec, width);
            eta_w = ETA_SHORT_W;
            starved = true;
        } else {
            starved = true;
        }
        room -= if eta_w > 0 { 1 + eta_w } else { 0 };
    }
    let mut detail: Option<String> = None;
    let mut detail_whole = !starved;
    let excerpt = if fit.action_excerpt {
        let own = if starved { 0 } else { room };
        room -= own;
        fit.words
            .of(spec)
            .filter(|_| fit.excerpt_floor > 0)
            .map(|d| (d, own + fit.excerpt_floor))
    } else {
        spec.detail0
            .filter(|d| !d.is_empty() && !starved)
            .map(|d| (d, room))
    };
    if let Some((d, mut cells)) = excerpt {
        let dw = width(d);
        detail_whole = 3 + dw <= cells;
        if detail_whole {
            detail = Some(d.to_string());
            cells -= 3 + dw;
        } else if let Some(cut) = excerpt_cut(d, cells, spec.step, width) {
            cells -= 3 + width(&cut);
            detail = Some(cut);
        }
        if fit.action_excerpt {
            // What the excerpt left is the row's again only beside a whole
            room += if detail_whole { cells } else { 0 };
        } else {
            room = cells;
        }
    }
    if fit.action_excerpt
        && detail_whole
        && !starved
        && reserves_load(spec, fit.activity)
        && 2 + load_slot_width(spec, width) <= room
    {
        load_slot = true;
        room -= 2 + load_slot_width(spec, width);
    }
    let load = spec.load.map(Load::words).filter(|_| load_slot);
    let mut stats: Option<String> = None;
    let painted = spec.meter.and_then(|(_, s)| {
        if matches!(fit.activity, Activity::LiveBar | Activity::HeldBar) {
            job_stats(spec.title, s)
        } else {
            Some(s.to_string()).filter(|s| !s.is_empty())
        }
    });
    if let Some(s) = painted.filter(|_| detail_whole && !starved) {
        let short = short_stats(&s);
        stats = [Some(s), short]
            .into_iter()
            .flatten()
            .find(|s| 2 + width(s) <= room);
    }
    Extras {
        elapsed_w,
        eta_w,
        load_slot,
        load,
        detail,
        stats,
    }
}

/// The stats' short form, where the long one does not fit.
#[must_use]
pub(crate) fn short_stats(stats: &str) -> Option<String> {
    let count = |w: &str| {
        w.starts_with(|c: char| c.is_ascii_digit())
            && w.chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
    };
    let mut words = stats.split(' ');
    if let (Some(done), Some("of"), Some(total), Some(unit)) =
        (words.next(), words.next(), words.next(), words.next())
        && count(done)
        && count(total)
        && unit.starts_with(char::is_alphabetic)
    {
        return Some(format!("{done}/{total} {unit}"));
    }
    let (_, last) = stats.rsplit_once(" \u{b7} ")?;
    (!last.trim().is_empty()).then(|| short_stats(last).unwrap_or_else(|| last.to_string()))
}

/// Placement: the left flow, then the capsules right-aligned, all over the
fn place(
    spec: &RowSpec<'_>,
    cols: usize,
    width: &dyn Fn(&str) -> usize,
    fit: &Fit,
    extras: Extras,
) -> RowLayout {
    let full_title = match spec.kind {
        RowKind::Overflow { .. } => overflow_spoken(&fit.title),
        RowKind::Message(_) | RowKind::Echo(_) => spec.title.to_string(),
    };
    let fill = spec.fill();
    let mut col = TITLE_COL + width(&fit.title);
    let detail = extras.detail.map(|d| {
        col += 3;
        let at = col;
        col += width(&d);
        (at, d)
    });
    let meter = fill.filter(|_| cols > 0).map(|f| (0, cols, f));
    let busy = spec.busy && fill.is_none();
    let track = (busy && cols > 0).then_some((0, cols));
    let mut pct = None;
    let mut elapsed = None;
    match (fit.activity, fill) {
        (Activity::LiveBar | Activity::HeldBar, Some(p)) => {
            let text = format!("{}%", p / 10);
            col += PCT_W - PCT_SLOT;
            pct = Some((col + PCT_SLOT.saturating_sub(width(&text)), text));
            col += PCT_SLOT;
        }
        (Activity::LiveBusy, _) => {
            col += 1;
            elapsed = Some(col);
            col += extras.elapsed_w.max(ELAPSED_SHORT_W);
        }
        _ => {}
    }
    let elapsed_short = elapsed.is_some() && extras.elapsed_w != ELAPSED_W;
    let eta = (extras.eta_w > 0).then(|| {
        col += 1;
        let at = col;
        col += extras.eta_w;
        at
    });
    let eta_short = extras.eta_w == ETA_SHORT_W;
    let stats = extras.stats.map(|s| {
        col += 2;
        let at = col;
        col += width(&s);
        (at, s)
    });
    let capsules = place_capsules(spec, cols, width, fit);
    let load_slot = extras.load_slot.then(|| {
        let w = load_slot_width(spec, width);
        let end = capsules.first().map_or(cols.saturating_sub(MARGIN), |c| {
            c.col.saturating_sub(BEFORE_CAPSULES)
        });
        (end.saturating_sub(w).max(col + 2), w)
    });
    let load = load_slot.and_then(|(at, w)| {
        extras
            .load
            .map(|words| (at + w.saturating_sub(width(words)), words))
    });
    RowLayout {
        kind: spec.kind,
        severity: spec.severity,
        live: spec.live,
        glyph: (GLYPH_COL, fit.glyph),
        title: (TITLE_COL, fit.title.clone()),
        full_title,
        detail,
        meter,
        busy,
        track,
        pct,
        elapsed,
        elapsed_short,
        eta,
        eta_short,
        load,
        load_slot,
        stats,
        capsules,
    }
}

/// The percent's own cells inside [`PCT_W`]: `100%` right-aligned in four.
const PCT_SLOT: usize = 4;

/// What a row with a percent paints in its stats slot, clause by clause (` · `).
#[must_use]
pub fn job_stats(title: &str, stats: &str) -> Option<String> {
    let zero = |a: &str| {
        let a = a.trim();
        let digits: String = a
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        !digits.is_empty() && digits.chars().all(|c| c == '0' || c == '.')
    };
    let clauses: Vec<String> = stats
        .split(crate::PIECE_SEP)
        .filter_map(|clause| {
            let clause = clause.trim();
            if clause.is_empty() {
                return None;
            }
            let (first, total) = if let Some((a, b)) = clause.split_once(" / ") {
                (a, b.trim().to_string())
            } else {
                let mut w = clause.splitn(3, ' ');
                match (w.next(), w.next(), w.next()) {
                    (Some(a), Some("of"), Some(rest)) => (a, rest.to_string()),
                    _ => ("", clause.to_string()),
                }
            };
            if zero(first) || says_whole(title, &total) {
                return None;
            }
            Some(match clause.split_once(" / ") {
                Some((done, _)) => size_of(done.trim(), &total),
                None => clause.to_string(),
            })
        })
        .collect();
    (!clauses.is_empty()).then(|| clauses.join(crate::PIECE_SEP))
}

/// A pair `done / total` as one count: `33 MB / 74 MB` → `33 of 74 MB` when
fn size_of(done: &str, total: &str) -> String {
    fn split(s: &str) -> Option<(&str, &str)> {
        s.rsplit_once(' ')
            .filter(|(n, u)| n.starts_with(|c: char| c.is_ascii_digit()) && !u.is_empty())
    }
    match (split(done), split(total)) {
        (Some((a, ua)), Some((b, ub))) if ua == ub => format!("{a} of {b} {ub}"),
        _ => format!("{done} of {total}"),
    }
}

/// Whether `title` states `words` as whole words: an occurrence with no
fn says_whole(title: &str, words: &str) -> bool {
    let joins = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '.' || c == ',');
    !words.is_empty()
        && title.match_indices(words).any(|(at, _)| {
            !joins(title[..at].chars().next_back())
                && !joins(title[at + words.len()..].chars().next())
        })
}

/// The capsules, right-aligned: long or short as fitted, the implicit
fn place_capsules(
    spec: &RowSpec<'_>,
    cols: usize,
    width: &dyn Fn(&str) -> usize,
    fit: &Fit,
) -> Vec<CapsuleLayout> {
    let kept = if fit.details_dropped {
        &spec.capsules[..spec.capsules.len() - 1]
    } else {
        &spec.capsules[..]
    };
    let caps: Vec<&CapsuleSpec> = painted(kept, fit.short).collect();
    let texts: Vec<String> = caps
        .iter()
        .map(|c| {
            if fit.short {
                c.short.clone()
            } else {
                c.long.clone()
            }
        })
        .collect();
    let total: usize = texts.iter().map(|t| width(t) + 2).sum::<usize>()
        + CAPSULE_GAP * caps.len().saturating_sub(1);
    let mut at = cols.saturating_sub(MARGIN + total);
    if MARGIN + total > cols {
        at = 0;
    }
    caps.iter()
        .zip(texts)
        .map(|(c, text)| {
            let w = width(&text) + 2;
            let layout = CapsuleLayout {
                col: at,
                width: w,
                text,
                full_label: c.full_label,
                role: c.role,
                action: c.action,
            };
            at += w + CAPSULE_GAP;
            layout
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::Decision;

    /// Paint a layout's words into exactly `cols` cells the way the host will.
    pub(crate) fn render(row: &RowLayout, cols: usize) -> String {
        let mut cells = vec![' '; cols];
        let mut put = |col: usize, s: &str| {
            for (i, ch) in s.chars().enumerate() {
                if let Some(cell) = cells.get_mut(col + i) {
                    *cell = ch;
                }
            }
        };
        put(row.glyph.0, &row.glyph.1.to_string());
        put(row.title.0, &row.title.1);
        if let Some((col, d)) = &row.detail {
            put(col - 2, "\u{00b7}");
            put(*col, d);
        }
        if let Some((col, p)) = &row.pct {
            put(*col, p);
        }
        if let Some((col, words)) = row.load {
            // At the tail, no joint.
            put(col, words);
        }
        if let Some((col, s)) = &row.stats {
            put(*col, s);
        }
        for c in &row.capsules {
            put(c.col, &format!(" {} ", c.text));
        }
        cells.into_iter().collect()
    }

    pub(crate) fn chars(s: &str) -> usize {
        s.chars().count()
    }

    /// Whether `shown` is `words` whole, or cut only after a whole word of them.
    pub(crate) fn whole_words_of(shown: &str, words: &str) -> bool {
        if shown == words {
            return true;
        }
        let Some(stem) = shown.strip_suffix('\u{2026}') else {
            return false;
        };
        !stem.is_empty()
            && aterm_grapheme::word_breaks(words)
                .iter()
                .any(|brk| words[..brk.at].trim_end_matches(TITLE_CUT_TRAIL) == stem)
    }

    /// Whether `row`'s title took the second clear cell before its capsules because a whole word needed it.
    pub(crate) fn spent_clear_cell(
        spec: &RowSpec<'_>,
        row: &RowLayout,
        width: &dyn Fn(&str) -> usize,
    ) -> bool {
        let shown = &row.title.1;
        let cells = width(shown);
        cells > 0
            && [Some(spec.title), spec.title_short]
                .into_iter()
                .flatten()
                .any(|words| {
                    title_in(words, cells, width).as_deref() == Some(shown.as_str())
                        && title_in(words, cells - 1, width).is_none()
                })
    }

    fn id(n: u64) -> MessageId {
        MessageId::from_raw(n).unwrap()
    }

    fn caps(intents: &[Intent]) -> Vec<CapsuleSpec> {
        let mut v: Vec<CapsuleSpec> = intents
            .iter()
            .enumerate()
            .map(|(i, it)| CapsuleSpec::authored(it, i as u8))
            .collect();
        v.push(CapsuleSpec::details(0));
        v
    }

    pub(crate) const CRASH_DETAIL: &str =
        "crash log at ~/Library/Logs/aterm/crash-signal-8123-1758470000.log.seen";
    pub(crate) const STAGED_MANUAL: &str = "build 1234 \u{2014} verified; auto-apply is off \u{2014} apply it from the Version menu or Software Update (in place; your shells keep running)";
    pub(crate) const STAGED_AUTO: &str = "build 1234 \u{2014} verified; applies by itself at the next quiet moment, and within 15 min regardless \u{2014} nothing to do; your shells keep running";

    /// The five width-law fixtures: crash, file access, staged (manual and auto), config warning, toolchain meter.
    pub(crate) fn fixtures() -> Vec<(&'static str, RowSpec<'static>)> {
        vec![
            (
                "crash",
                RowSpec {
                    kind: RowKind::Message(id(1)),
                    severity: Severity::Error,
                    live: false,
                    glyph: '\u{26a0}',
                    title: "aterm closed unexpectedly last time",
                    title_short: None,
                    detail0: Some(CRASH_DETAIL),
                    step: false,
                    step_short: None,
                    meter: None,
                    animated: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: Loads::NONE,
                    capsules: caps(&[Intent::OpenPath { path: "/x".into() }]),
                },
            ),
            (
                "file-access",
                RowSpec {
                    kind: RowKind::Message(id(2)),
                    severity: Severity::Info,
                    live: false,
                    glyph: '\u{2139}',
                    title: "File access not confirmed",
                    title_short: None,
                    detail0: Some("Full Disk Access may already be enabled"),
                    step: false,
                    step_short: None,
                    meter: None,
                    animated: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: Loads::NONE,
                    capsules: caps(&[
                        Intent::OpenSystemPane {
                            pane: "full-disk-access".into(),
                        },
                        Intent::NotNow {
                            decision: Decision::FileAccess,
                        },
                    ]),
                },
            ),
            (
                "staged-manual",
                RowSpec {
                    kind: RowKind::Message(id(3)),
                    severity: Severity::Success,
                    live: false,
                    glyph: '\u{2713}',
                    title: "aterm v0.91.0 is ready",
                    title_short: None,
                    detail0: Some(STAGED_MANUAL),
                    step: false,
                    step_short: None,
                    meter: None,
                    animated: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: Loads::NONE,
                    capsules: caps(&[Intent::ApplyUpdate { build: 1234 }]),
                },
            ),
            (
                "staged-auto",
                RowSpec {
                    kind: RowKind::Message(id(4)),
                    severity: Severity::Success,
                    live: false,
                    glyph: '\u{2713}',
                    title: "aterm v0.91.0 is ready",
                    title_short: None,
                    detail0: Some(STAGED_AUTO),
                    step: false,
                    step_short: None,
                    meter: None,
                    animated: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: Loads::NONE,
                    capsules: caps(&[]),
                },
            ),
            (
                "config",
                RowSpec {
                    kind: RowKind::Message(id(5)),
                    severity: Severity::Warn,
                    live: false,
                    glyph: '\u{26a0}',
                    title: "3 keybindings in aterm.toml were skipped",
                    title_short: None,
                    detail0: Some("skipping \"cmd+shift+k\": unknown action \"foo\""),
                    step: false,
                    step_short: None,
                    meter: None,
                    animated: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: Loads::NONE,
                    capsules: caps(&[Intent::OpenConfigEditor { line: None }]),
                },
            ),
            (
                "toolchain",
                RowSpec {
                    kind: RowKind::Message(id(6)),
                    severity: Severity::Info,
                    live: true,
                    glyph: '\u{21e3}',
                    title: "Installing ALab tools",
                    title_short: None,
                    detail0: Some("trust \u{00b7} extracting"),
                    step: false,
                    step_short: None,
                    meter: Some((Some(420), "512 MB / 1.2 GB")),
                    animated: true,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: Loads::NONE,
                    capsules: caps(&[Intent::OpenSettings {
                        route: "/packages".into(),
                    }]),
                },
            ),
        ]
    }

    /// The live indicator's fixtures.
    pub(crate) fn motion_fixtures() -> Vec<(&'static str, RowSpec<'static>)> {
        vec![
            (
                "download",
                RowSpec {
                    kind: RowKind::Message(id(7)),
                    severity: Severity::Info,
                    live: true,
                    glyph: '\u{21bb}',
                    title: "Downloading aterm v0.92.0",
                    title_short: None,
                    detail0: None,
                    step: false,
                    step_short: None,
                    meter: Some((Some(420), "31 MB / 74 MB")),
                    animated: true,
                    busy: false,
                    eta: true,
                    load: None,
                    load_slot: Loads::NONE,
                    capsules: caps(&[]),
                },
            ),
            (
                "first-run",
                RowSpec {
                    kind: RowKind::Message(id(8)),
                    severity: Severity::Info,
                    live: true,
                    glyph: '\u{21e3}',
                    title: "Installing ALab tools",
                    title_short: None,
                    detail0: None,
                    step: false,
                    step_short: None,
                    meter: Some((Some(420), "3 of 10 programs")),
                    animated: true,
                    busy: false,
                    eta: true,
                    load: Some(Load::Disk),
                    load_slot: Loads::ALL,
                    capsules: caps(&[]),
                },
            ),
            (
                "first-run-lull",
                RowSpec {
                    kind: RowKind::Message(id(12)),
                    severity: Severity::Info,
                    live: true,
                    glyph: '\u{21e3}',
                    title: "Installing ALab tools",
                    title_short: None,
                    detail0: None,
                    step: false,
                    step_short: None,
                    meter: Some((Some(420), "3 of 10 programs")),
                    animated: true,
                    busy: false,
                    eta: true,
                    load: None,
                    load_slot: Loads::ALL,
                    capsules: caps(&[]),
                },
            ),
            (
                "action-excerpt",
                RowSpec {
                    kind: RowKind::Message(id(9)),
                    severity: Severity::Info,
                    live: true,
                    glyph: '\u{21e3}',
                    title: "Installing Command Line Tools and Homebrew",
                    title_short: None,
                    detail0: Some("enter your password in the macOS dialog"),
                    step: false,
                    step_short: None,
                    meter: Some((None, "")),
                    animated: true,
                    busy: true,
                    eta: false,
                    load: Some(Load::Disk),
                    load_slot: Loads::ALL,
                    capsules: caps(&[]),
                },
            ),
            (
                "busy-excerpt",
                RowSpec {
                    kind: RowKind::Message(id(10)),
                    severity: Severity::Info,
                    live: true,
                    glyph: '\u{2191}',
                    title: "Finishing aterm v0.92.0",
                    title_short: None,
                    detail0: Some("keys typed now arrive in a moment"),
                    step: false,
                    step_short: None,
                    meter: Some((None, "")),
                    animated: true,
                    busy: true,
                    eta: false,
                    load: None,
                    load_slot: Loads::NONE,
                    capsules: caps(&[]),
                },
            ),
            (
                "held-meter",
                RowSpec {
                    kind: RowKind::Message(id(11)),
                    severity: Severity::Warn,
                    live: false,
                    glyph: '\u{26a0}',
                    title: "ALab tools",
                    title_short: None,
                    detail0: Some("trust \u{2014} extracting 120 MB / 900 MB"),
                    step: false,
                    step_short: None,
                    meter: Some((Some(430), "3 of 10")),
                    animated: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: Loads::NONE,
                    capsules: caps(&[Intent::OpenSettings {
                        route: "/packages".into(),
                    }]),
                },
            ),
        ]
    }

    /// Cols 8..=200 over the five fixtures, the live indicator's and the overdue upgrade
    #[test]
    fn width_law_degrades_in_order() {
        use crate::model::UpgradeWord::{NotToday, Now, Skip};
        let upgrade = |word| Intent::AgentUpgrade {
            tab: "s-1".into(),
            to: "2.1.283".into(),
            word,
        };
        let overdue = |name, title, detail0, capsules| {
            (
                name,
                RowSpec {
                    kind: RowKind::Message(id(9)),
                    severity: Severity::Warn,
                    live: true,
                    glyph: '\u{26a0}',
                    title,
                    title_short: None,
                    detail0: Some(detail0),
                    step: false,
                    step_short: None,
                    meter: None,
                    animated: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: Loads::NONE,
                    capsules,
                },
            )
        };
        let one_tab = "Couldn't upgrade Claude in tab 1 yet";
        let overdue_rows = [
            overdue(
                "overdue-upgrade",
                one_tab,
                "behind for 7 h: its turn is still running",
                caps(&[upgrade(NotToday), upgrade(Skip)]),
            ),
            overdue(
                "overdue-upgrade-26h",
                one_tab,
                "behind for 26 h: its turn is still running",
                caps(&[upgrade(NotToday), upgrade(Skip)]),
            ),
            overdue(
                "overdue-tabs",
                "Couldn't upgrade Claude in 2 tabs yet",
                "tab 1 for 7 h \u{b7} tab 3 for 2 h",
                caps(&[upgrade(Now), upgrade(NotToday)]),
            ),
        ];
        let mut clause_seen = std::collections::BTreeSet::new();
        for (name, spec) in fixtures()
            .into_iter()
            .chain(motion_fixtures())
            .chain(overdue_rows)
        {
            let authored = spec
                .capsules
                .iter()
                .filter(|c| !c.action.is_details())
                .count();
            let short_of = |r: &RowLayout| {
                r.capsules.len() < spec.capsules.len()
                    || r.capsules.iter().any(|c| c.text != c.full_label)
            };
            let mut prev: Option<RowLayout> = None;
            for cols in (8..=200).rev() {
                let row = layout_row(&spec, cols, &chars);
                let painted = render(&row, cols);
                assert_eq!(chars(&painted), cols, "{name}@{cols}");
                assert_eq!(
                    row.capsules
                        .iter()
                        .filter(|c| !c.action.is_details())
                        .count(),
                    authored,
                    "{name}@{cols}: authored capsules are never dropped"
                );
                let mut spans: Vec<(usize, usize)> = vec![(row.glyph.0, row.glyph.0 + 1)];
                if !row.title.1.is_empty() {
                    spans.push((row.title.0, row.title.0 + chars(&row.title.1)));
                }
                if let Some((c, d)) = &row.detail {
                    spans.push((c - 3, c + chars(d)));
                }
                let fill = spec.meter.and_then(|(f, _)| f).map(|f| f.min(1000));
                assert_eq!(
                    row.meter,
                    fill.map(|f| (0, cols, f)),
                    "{name}@{cols}: the meter is the whole row"
                );
                assert_eq!(
                    row.track,
                    (spec.busy && fill.is_none()).then_some((0, cols)),
                    "{name}@{cols}: the track is the whole row"
                );
                if let Some((c, p)) = &row.pct {
                    spans.push((c - 1, c + chars(p)));
                }
                if let Some(c) = row.elapsed {
                    spans.push((c - 1, c + row.elapsed_width()));
                }
                if let Some(c) = row.eta {
                    spans.push((c - 1, c + row.eta_width()));
                }
                if let Some((c, w)) = row.load_slot {
                    spans.push((c - 2, c + w));
                }
                if let (Some((c, w)), Some((slot, sw))) = (row.load, row.load_slot) {
                    assert!(
                        c + chars(w) == slot + sw && chars(w) <= sw,
                        "{name}@{cols}: the words sit right-aligned in their slot"
                    );
                }
                if let Some((c, s)) = &row.stats {
                    spans.push((c - 2, c + chars(s)));
                }
                let mut sorted = spans.clone();
                sorted.sort();
                for pair in sorted.windows(2) {
                    assert!(
                        pair[0].1 <= pair[1].0 || row.title.1.is_empty(),
                        "{name}@{cols}: {pair:?} overlap: {painted:?}"
                    );
                }
                let left_end = spans.iter().map(|s| s.1).max().unwrap();
                if let Some(first) = row.capsules.first()
                    && first.col > 0
                    && !row.title.1.is_empty()
                {
                    assert!(first.col >= left_end, "{name}@{cols}: overlap: {painted:?}");
                    assert!(
                        first.col >= left_end + BEFORE_CAPSULES
                            || first.col == left_end + 1 && spent_clear_cell(&spec, &row, &chars),
                        "{name}@{cols}: {painted:?}"
                    );
                    let last = row.capsules.last().unwrap();
                    assert_eq!(
                        last.col + last.width,
                        cols - MARGIN,
                        "{name}@{cols}: right-aligned"
                    );
                }
                let moving = spec.animated || spec.busy;
                let activity = row.pct.is_some() || row.elapsed.is_some();
                let action = moving && spec.detail0.is_some_and(|d| !d.is_empty());
                if moving && !activity {
                    assert!(
                        row.detail.is_none()
                            && row.stats.is_none()
                            && row.eta.is_none()
                            && row.load.is_none()
                            && row.load_slot.is_none(),
                        "{name}@{cols}: an extra outlived the activity: {painted:?}"
                    );
                }
                if moving && activity && action {
                    assert!(
                        row.load_slot.is_none()
                            || row.detail.as_ref().map(|d| d.1.as_str()) == spec.detail0,
                        "{name}@{cols}: the load slot only beside the whole action excerpt: {painted:?}"
                    );
                    assert!(
                        row.detail.is_some() || short_of(&row),
                        "{name}@{cols}: the action excerpt went before the capsules' long forms: {painted:?}"
                    );
                }
                let traded = spec.eta && row.eta_short && row.load_slot.is_none();
                if moving && activity && !action && !traded {
                    assert_eq!(
                        row.load.is_some(),
                        spec.load.is_some(),
                        "{name}@{cols}: the load words are the activity's: {painted:?}"
                    );
                    assert_eq!(
                        row.load_slot.is_some(),
                        spec.load.is_some() || !spec.load_slot.is_empty(),
                        "{name}@{cols}: the load slot is the activity's: {painted:?}"
                    );
                }
                if let Some(p) = &prev {
                    let short_now = short_of(&row);
                    let short_prev = short_of(p);
                    assert!(
                        !(short_prev && !short_now) || row.capsules.len() != p.capsules.len(),
                        "{name}@{cols}: capsules never go long again as the row narrows"
                    );
                    let elided_prev = p.title.1 != p.full_title;
                    if elided_prev && authored > 0 {
                        assert!(
                            short_now,
                            "{name}@{cols}: the title elides only after the capsules went short"
                        );
                    }
                    for (was, is, what) in [
                        (p.stats.is_some(), row.stats.is_some(), "stats"),
                        (p.detail.is_some(), row.detail.is_some(), "the excerpt"),
                        (p.eta.is_some(), row.eta.is_some(), "the ETA"),
                        (p.load.is_some(), row.load.is_some(), "the load words"),
                        (
                            p.load_slot.is_some(),
                            row.load_slot.is_some(),
                            "the load slot",
                        ),
                        (p.pct.is_some(), row.pct.is_some(), "the percent"),
                        (
                            p.elapsed.is_some(),
                            row.elapsed.is_some(),
                            "the elapsed slot",
                        ),
                    ] {
                        assert!(
                            was || !is,
                            "{name}@{cols}: {what} came back while narrowing"
                        );
                    }
                    if short_now {
                        assert!(
                            (row.detail.is_none() || action)
                                && row.stats.is_none()
                                && (row.eta.is_none() || traded)
                                && (row.load_slot.is_none() || !action),
                            "{name}@{cols}: an extra beside short capsules: {painted:?}"
                        );
                    }
                    let long_stats = spec.meter.map(|(_, s)| s);
                    let is_short = |r: &RowLayout| {
                        r.stats
                            .as_ref()
                            .is_some_and(|(_, s)| Some(s.as_str()) != long_stats)
                    };
                    assert!(
                        !(is_short(&p) && row.stats.is_some() && !is_short(&row)),
                        "{name}@{cols}: the stats went long again while narrowing"
                    );
                    assert!(
                        !(p.eta_short && row.eta.is_some() && !row.eta_short),
                        "{name}@{cols}: the ETA went long again while narrowing"
                    );
                    if row.eta_short {
                        assert!(
                            (row.detail.is_none() || action) && row.stats.is_none(),
                            "{name}@{cols}: an extra beside a short ETA: {painted:?}"
                        );
                    }
                    if row.eta.is_none() && spec.eta && row.pct.is_some() {
                        assert!(
                            (row.detail.is_none() || action) && row.stats.is_none(),
                            "{name}@{cols}: the ETA went first: {painted:?}"
                        );
                    }
                    if p.detail.is_some() && row.stats.is_some() {
                        assert!(
                            p.stats.is_some(),
                            "{name}@{cols}: stats go before the excerpt"
                        );
                    }
                }
                let clause = spec.detail0.and_then(crate::text::lead_clause);
                if let Some((dcol, d)) = &row.detail
                    && clause.as_ref() == Some(d)
                {
                    clause_seen.insert(name);
                    assert!(
                        !d.contains('\u{2026}') && *dcol == row.title.0 + chars(&row.title.1) + 3,
                        "{name}@{cols}: {d:?}"
                    );
                    let room = row
                        .capsules
                        .first()
                        .and_then(|c| c.col.checked_sub(BEFORE_CAPSULES + *dcol - 3));
                    assert!(
                        chars(d) < DETAIL_FLOOR || room.is_some_and(|r| r >= 3 + DETAIL_FLOOR),
                        "{name}@{cols}: a clause the floor's cut would carry: {d:?}"
                    );
                } else if let Some((dcol, d)) = &row.detail
                    && d != spec.detail0.unwrap()
                    && row.meter.is_none()
                    && row.pct.is_none()
                    && let Some(first) = row.capsules.first()
                    && first.col > 0
                    && let title_end = row.title.0 + chars(&row.title.1)
                    && let Some(room) = first.col.checked_sub(BEFORE_CAPSULES + title_end)
                {
                    assert!(
                        room >= 3 + DETAIL_FLOOR,
                        "{name}@{cols}: shaped with room {room}: {d:?}"
                    );
                    assert!(
                        d.contains('\u{2026}') && *dcol == title_end + 3,
                        "{name}@{cols}: {d:?}"
                    );
                    assert!(
                        chars(d) + 3 <= room,
                        "{name}@{cols}: {d:?} does not fit its room"
                    );
                }
                prev = Some(row);
            }
        }
        assert_eq!(
            clause_seen.into_iter().collect::<Vec<_>>(),
            [
                "config",
                "overdue-tabs",
                "overdue-upgrade",
                "overdue-upgrade-26h"
            ],
            "the control: each overdue row painted its lead clause somewhere"
        );
    }

    /// Every capsule column range maps back to its `ActionIndex`; the gaps map to
    #[test]
    fn hit_test_agrees_with_layout() {
        let fixtures = fixtures();
        for cols in 8..=160usize {
            let mut rows: Vec<RowLayout> = fixtures
                .iter()
                .map(|(_, s)| layout_row(s, cols, &chars))
                .collect();
            let words = overflow_words(3, Links::Painted, None);
            rows.push(layout_row(&overflow_spec(3, &words), cols, &chars));
            let p = Presentation { cols, rows };
            for (r, row) in p.rows.iter().enumerate() {
                let RowKind::Message(id) = row.kind else {
                    for col in 0..cols {
                        assert_eq!(p.hit(r, col), Hit::Overflow);
                    }
                    continue;
                };
                for col in 0..cols {
                    let want = match row
                        .capsules
                        .iter()
                        .find(|c| col >= c.col && col < c.col + c.width)
                    {
                        Some(c) if c.action.is_details() => Hit::Details(id),
                        Some(c) => Hit::Capsule(id, c.action),
                        None => Hit::Body(id),
                    };
                    assert_eq!(p.hit(r, col), want, "row {r} col {col} @ {cols}");
                }
                assert_eq!(p.hit(r, cols), Hit::Nothing);
            }
            assert_eq!(p.hit(p.rows.len(), 0), Hit::Nothing);
        }
    }

    /// `0` with no rows; stats fold into the key; the fill is quantized to whole percent.
    #[test]
    fn fingerprint_is_zero_when_empty_and_quantizes_the_fill() {
        assert_eq!(
            Presentation {
                cols: 80,
                rows: vec![]
            }
            .fingerprint(),
            0
        );
        let spec = fixtures()
            .into_iter()
            .find(|(n, _)| *n == "toolchain")
            .unwrap()
            .1;
        let at = |fill: u16, stats: &'static str| {
            let mut s = spec.clone();
            s.meter = Some((Some(fill), stats));
            Presentation {
                cols: 160,
                rows: vec![layout_row(&s, 160, &chars)],
            }
            .fingerprint()
        };
        let base = at(420, "512 MB / 1.2 GB");
        assert_ne!(base, 0);
        assert_eq!(
            base,
            at(424, "512 MB / 1.2 GB"),
            "a tick inside the percent is the same key"
        );
        assert_ne!(base, at(430, "512 MB / 1.2 GB"), "a whole percent moves it");
        assert_ne!(
            base,
            at(420, "513 MB / 1.2 GB"),
            "the bytes done are painted as part of the job (`512 MB of 1.2 GB`, ruling 259)"
        );
        assert_ne!(base, at(420, "513 MB / 1.3 GB"), "the job's size is folded");
        let other_cols = Presentation {
            cols: 120,
            rows: vec![layout_row(&spec, 120, &chars)],
        }
        .fingerprint();
        assert_ne!(base, other_cols);
        let busy_at = |busy: bool| {
            let mut s = spec.clone();
            s.meter = Some((None, ""));
            s.busy = busy;
            Presentation {
                cols: 160,
                rows: vec![layout_row(&s, 160, &chars)],
            }
            .fingerprint()
        };
        assert_ne!(busy_at(false), busy_at(true), "busy is folded");
    }

    /// A busy row carries `track = (0, cols)` at every width and never a meter or a pct;
    #[test]
    fn a_busy_row_is_a_whole_row_track_that_costs_the_words_nothing() {
        let flow = RowSpec {
            kind: RowKind::Message(id(9)),
            severity: Severity::Info,
            live: true,
            glyph: '\u{21bb}',
            title: "Installing aterm v0.92.0",
            title_short: None,
            detail0: Some("installs within a minute \u{2014} keep working"),
            step: false,
            step_short: None,
            meter: Some((None, "")),
            busy: true,
            animated: false,
            eta: false,
            load: None,
            load_slot: Loads::NONE,
            capsules: caps(&[]),
        };
        let with_stats = RowSpec {
            title_short: None,
            detail0: Some("downloading"),
            step: false,
            step_short: None,
            meter: Some((None, "45 MB")),
            ..flow.clone()
        };
        let bare = RowSpec {
            title_short: None,
            detail0: None,
            step: false,
            step_short: None,
            ..with_stats.clone()
        };
        for spec in [&flow, &with_stats, &bare] {
            let still = RowSpec {
                busy: false,
                ..spec.clone()
            };
            for cols in (1..=200).rev() {
                let row = layout_row(spec, cols, &chars);
                assert_eq!(
                    chars(&render(&row, cols)),
                    cols,
                    "{cols}: exactly cols wide"
                );
                assert_eq!(row.glyph.0, GLYPH_COL, "{cols}");
                assert!(row.busy, "{cols}: the comet runs wherever the row is");
                assert!(row.meter.is_none() && row.pct.is_none(), "{cols}: no fill");
                assert_eq!(row.track, Some((0, cols)), "{cols}: the track is the row");
                let plain = layout_row(&still, cols, &chars);
                assert!(!plain.busy && plain.track.is_none(), "{cols}: not busy");
                let slot = 1 + row.elapsed_width();
                if row.elapsed.is_some() && cols > slot {
                    let narrower = layout_row(&still, cols - slot, &chars);
                    let excerpt = |r: &RowLayout| r.detail.as_ref().map_or(0, |d| chars(&d.1));
                    assert!(
                        excerpt(&row) >= excerpt(&narrower),
                        "{cols}: the track took from the excerpt: {:?} vs {:?}",
                        row.detail,
                        narrower.detail
                    );
                    if spec.detail0.is_none() {
                        let words = |r: &RowLayout| {
                            (r.title.clone(), r.stats.as_ref().map(|s| s.1.clone()))
                        };
                        assert_eq!(
                            words(&row),
                            words(&narrower),
                            "{cols}: the track bought nothing from the words"
                        );
                    }
                }
            }
            assert_eq!(
                layout_row(spec, 0, &chars).track,
                None,
                "no columns, no track"
            );
        }
        let filled = RowSpec {
            meter: Some((Some(500), "")),
            ..flow
        };
        let row = layout_row(&filled, 160, &chars);
        assert!(!row.busy && row.track.is_none());
        assert_eq!(row.meter, Some((0, 160, 500)));
    }

    #[test]
    fn spoken_is_width_independent() {
        let spec = fixtures()
            .into_iter()
            .find(|(n, _)| *n == "crash")
            .unwrap()
            .1;
        let wide = layout_row(&spec, 160, &chars);
        let narrow = layout_row(&spec, 24, &chars);
        assert_ne!(wide.title.1, narrow.title.1, "the narrow title elides");
        assert_eq!(wide.spoken(CRASH_DETAIL), narrow.spoken(CRASH_DETAIL));
        assert_eq!(wide.spoken(""), "aterm closed unexpectedly last time");
        assert!(wide.spoken(CRASH_DETAIL).ends_with(".log.seen"));
    }

    /// A reader hears the excerpts' joints as words; the paint keeps them.
    #[test]
    fn spoken_says_the_band_pictographs_as_words() {
        assert_eq!(
            speakable("Unknown key \u{00b7} windw_padding \u{2192} window_padding?"),
            "Unknown key \u{00b7} windw_padding, did you mean window_padding?"
        );
        assert_eq!(
            speakable("Split refused \u{00b7} pane 20\u{00d7}5, needs 20\u{00d7}7"),
            "Split refused \u{00b7} pane 20 by 5, needs 20 by 7"
        );
        assert_eq!(
            speakable(
                "Enable aterm in Full Disk Access \u{00b7} Privacy & Security \u{25b8} Full Disk Access"
            ),
            "Enable aterm in Full Disk Access \u{00b7} Privacy & Security, Full Disk Access"
        );
        for plain in [
            "\u{2192}",
            "a\u{00d7}b",
            "x \u{25b8}",
            "Installing Homebrew",
        ] {
            assert_eq!(speakable(plain), plain, "no joint to say: {plain:?}");
        }
    }

    /// A wide-aware cell measure of the host's shape: CJK two cells,
    fn wide(s: &str) -> usize {
        s.chars()
            .map(|c| match c {
                '\u{300}'..='\u{36f}' => 0,
                '\u{6f22}' => 2,
                _ => 1,
            })
            .sum()
    }

    /// `shape_detail` cuts by chars while the law measures cells with the
    #[test]
    fn a_wide_excerpt_does_not_flicker_while_narrowing() {
        let detail = format!("{}{}", "ok ".repeat(12), "\u{6f22}".repeat(30));
        let spec = RowSpec {
            kind: RowKind::Message(id(1)),
            severity: Severity::Info,
            live: false,
            glyph: '\u{2139}',
            title: "Installing",
            title_short: None,
            detail0: Some(&detail),
            step: false,
            step_short: None,
            meter: None,
            animated: false,
            busy: false,
            eta: false,
            load: None,
            load_slot: Loads::NONE,
            capsules: vec![CapsuleSpec::details(0)],
        };
        let mut pattern = String::new();
        for cols in (30..=140).rev() {
            let row = layout_row(&spec, cols, &wide);
            pattern.push(if row.detail.is_some() { '#' } else { '.' });
        }
        let flips = pattern.matches(".#").count();
        assert_eq!(
            flips, 0,
            "the excerpt returned {flips} time(s) while narrowing 140→30 (# shown, . dropped):\n{pattern}"
        );
        assert!(
            pattern.starts_with("###") && pattern.ends_with("..."),
            "{pattern}"
        );
        let row = layout_row(&spec, 100, &wide);
        let (col, shown) = row.detail.as_ref().unwrap();
        let first = row.capsules.first().unwrap().col;
        assert!(col + wide(shown) + BEFORE_CAPSULES <= first, "{shown:?}");
        assert!(
            shown.ends_with('\u{2026}') && shown.starts_with("ok ok"),
            "{shown:?}"
        );
    }

    /// The meter spans every column at every width, held or live, and costs the words
    #[test]
    fn the_meter_is_the_whole_row_at_every_width_and_costs_the_words_nothing() {
        for animated in [false, true] {
            let spec = |fill: Option<u16>| RowSpec {
                kind: RowKind::Message(id(1)),
                severity: Severity::Info,
                live: animated,
                glyph: '\u{21e3}',
                title: "Installing ALab tools",
                title_short: None,
                detail0: Some("trust \u{00b7} extracting 120 MB of 900 MB"),
                step: false,
                step_short: None,
                meter: Some((fill, "")),
                busy: false,
                animated,
                eta: false,
                load: None,
                load_slot: Loads::NONE,
                capsules: vec![
                    CapsuleSpec::authored(&Intent::OpenConfigEditor { line: None }, 0),
                    CapsuleSpec::details(0),
                ],
            };
            for cols in (1..=120).rev() {
                for fill in [0u16, 1, 420, 999, 1000, 1500] {
                    let row = layout_row(&spec(Some(fill)), cols, &chars);
                    assert_eq!(
                        row.meter,
                        Some((0, cols, fill.min(1000))),
                        "@{cols} fill {fill}: {}",
                        render(&row, cols)
                    );
                }
                let metered = layout_row(&spec(Some(420)), cols, &chars);
                if metered.pct.is_some() && cols > PCT_W {
                    let unmetered = layout_row(&spec(None), cols - PCT_W, &chars);
                    assert_eq!(unmetered.meter, None, "no fill, no meter");
                    if animated {
                        let excerpt = |r: &RowLayout| r.detail.as_ref().map_or(0, |d| chars(&d.1));
                        assert!(
                            excerpt(&metered) >= excerpt(&unmetered),
                            "@{cols}: the meter took from the excerpt"
                        );
                    } else {
                        assert_eq!(
                            metered.detail, unmetered.detail,
                            "@{cols}: the meter bought nothing from the excerpt"
                        );
                    }
                }
            }
            assert_eq!(layout_row(&spec(Some(420)), 0, &chars).meter, None);
        }
    }

    /// At 80 columns a first run's row under disk load takes the ETA's short form
    #[test]
    fn the_eta_slot_goes_short_before_it_goes() {
        let fixtures = motion_fixtures();
        let spec = &fixtures.iter().find(|(n, _)| *n == "first-run").unwrap().1;
        let at = |cols: usize| layout_row(spec, cols, &chars);
        let wide = at(120);
        assert!(wide.eta.is_some() && !wide.eta_short, "long at 120");
        assert_eq!(wide.eta_width(), ETA_W);
        let short: Vec<usize> = (20..=120).filter(|c| at(*c).eta_short).collect();
        assert!(
            !short.is_empty(),
            "the short form is laid out at some width"
        );
        for &cols in &short {
            let narrow = at(cols);
            assert_eq!(narrow.eta_width(), ETA_SHORT_W, "@{cols}");
            assert!(narrow.detail.is_none() && narrow.stats.is_none(), "@{cols}");
        }
        assert!(
            short.iter().any(|c| at(*c).load.is_some()),
            "short beside the load words"
        );
        assert!(
            short.iter().any(|c| at(*c).load_slot.is_none()),
            "short in the load slot's cells"
        );
        for cols in 20..=120 {
            let row = at(cols);
            assert_eq!(
                row.eta.is_some(),
                row.pct.is_some(),
                "@{cols}: the ETA goes only with the activity's words"
            );
        }
    }

    /// An echo row is not pressable: every column of it is `Hit::Nothing`.
    #[test]
    fn an_echo_row_is_hit_nothing() {
        let mut spec = fixtures()
            .into_iter()
            .find(|(n, _)| *n == "toolchain")
            .unwrap()
            .1;
        spec.kind = RowKind::Echo(id(6));
        let p = Presentation {
            cols: 80,
            rows: vec![layout_row(&spec, 80, &chars)],
        };
        for col in 0..90 {
            assert_eq!(p.hit(0, col), Hit::Nothing, "col {col}");
        }
    }

    /// The load slot is sized to the row's own loads (`CPU busy` reserves 2 + 8 cells, not
    #[test]
    fn the_load_slot_is_sized_to_the_rows_declared_loads() {
        let build = |slot: Loads| RowSpec {
            kind: RowKind::Message(id(20)),
            severity: Severity::Info,
            live: true,
            glyph: '\u{2139}',
            title: "Building aterm and tools",
            title_short: None,
            detail0: None,
            step: false,
            step_short: None,
            meter: Some((None, "3 of 10 targets")),
            animated: false,
            busy: true,
            eta: false,
            load: Some(Load::Cpu),
            load_slot: slot,
            capsules: caps(&[]),
        };
        let cpu = Loads::NONE.with(Load::Cpu);
        let slot_w = |slot: Loads, cols: usize| {
            layout_row(&build(slot), cols, &chars)
                .load_slot
                .map(|(_, w)| w)
        };
        assert_eq!(slot_w(cpu, 80), Some(chars("CPU busy")));
        assert_eq!(slot_w(Loads::ALL, 80), Some(chars("network busy")));
        assert_eq!(
            slot_w(cpu.with(Load::Network), 80),
            Some(chars("network busy")),
            "a second kind widens it to the wider words"
        );
        let mut lull = build(cpu);
        lull.load = None;
        let (with, without) = (
            layout_row(&build(cpu), 80, &chars),
            layout_row(&lull, 80, &chars),
        );
        assert_eq!(with.load_slot, without.load_slot);
        assert_eq!(with.stats, without.stats);
        assert!(without.load.is_none());
        let pass = |cols: usize| {
            layout_row(
                &RowSpec {
                    kind: RowKind::Message(id(21)),
                    severity: Severity::Info,
                    live: true,
                    glyph: '\u{21e3}',
                    title: "Installing ALab tools",
                    title_short: None,
                    detail0: None,
                    step: false,
                    step_short: None,
                    meter: Some((Some(420), "3 of 10 programs")),
                    animated: true,
                    busy: false,
                    eta: true,
                    load: Some(Load::Disk),
                    load_slot: [Load::Network, Load::Disk, Load::Cpu].into_iter().collect(),
                    capsules: caps(&[]),
                },
                cols,
                &chars,
            )
        };
        assert_eq!(
            pass(80).load_slot.map(|(_, w)| w),
            Some(chars("network busy"))
        );
        assert!(pass(80).eta.is_some(), "80: the ETA first");
        assert_eq!(pass(80).stats, None);
        assert!(
            pass(120).eta.is_some()
                && pass(120).stats.map(|s| s.1).as_deref() == Some("3 of 10 programs"),
            "120: both, long"
        );
        assert!(pass(60).stats.is_none());
        assert_eq!(
            short_stats("3 of 10 programs").as_deref(),
            Some("3/10 programs")
        );
        assert_eq!(
            short_stats("5.9 of 8 cores").as_deref(),
            Some("5.9/8 cores")
        );
        assert_eq!(short_stats("23 of 24 GB used").as_deref(), Some("23/24 GB"));
        for none in [
            "31 MB / 74 MB",
            "  5 MB / 1.2 GB",
            "one of two",
            "3 of",
            "",
            "of 3 of 4",
            "10 programs \u{b7} ",
            "1,200 of 3,400",
            "3 of 10",
        ] {
            assert_eq!(short_stats(none), None, "{none:?}");
        }
        assert_eq!(
            short_stats("10 programs \u{b7} ~3 GB").as_deref(),
            Some("~3 GB")
        );
        assert_eq!(
            short_stats("16 GB \u{b7} 3 of 10 programs").as_deref(),
            Some("3/10 programs")
        );
        assert_eq!(
            short_stats("throttled \u{b7} 6 of 8 cores").as_deref(),
            Some("6/8 cores")
        );
        assert_eq!(
            short_stats("3 of 10 \u{b7} 512 MB / 1.2 GB").as_deref(),
            Some("512 MB / 1.2 GB")
        );
    }

    /// A moving row with an ETA and a load slot keeps both while the ETA fits; where it
    #[test]
    fn the_eta_outranks_the_load_words_as_the_row_narrows() {
        let spec = |load: Option<Load>| RowSpec {
            kind: RowKind::Message(id(22)),
            severity: Severity::Info,
            live: true,
            glyph: '\u{2193}',
            title: "Updating ALab tools",
            title_short: None,
            detail0: None,
            step: false,
            step_short: None,
            meter: Some((Some(660), "1 of 2 programs")),
            animated: true,
            busy: false,
            eta: true,
            load,
            load_slot: [Load::Network, Load::Disk, Load::Cpu].into_iter().collect(),
            capsules: caps(&[]),
        };
        let at60 = layout_row(&spec(Some(Load::Network)), 60, &chars);
        assert!(at60.eta.is_some() && at60.eta_short, "60: the short ETA");
        assert!(
            at60.load.is_none() && at60.load_slot.is_none(),
            "60: no load words"
        );
        let at80 = layout_row(&spec(Some(Load::Network)), 80, &chars);
        assert!(at80.eta.is_some() && !at80.eta_short, "80: the long ETA");
        assert!(at80.load.is_some(), "80: and the load words");
        let (mut eta_gone, mut load_gone, mut short) = (false, false, false);
        for cols in (1..=160).rev() {
            let with = layout_row(&spec(Some(Load::Network)), cols, &chars);
            let without = layout_row(&spec(None), cols, &chars);
            assert_eq!(
                with.load_slot, without.load_slot,
                "{cols}: words never re-lay"
            );
            assert_eq!(with.eta, without.eta, "{cols}");
            assert_eq!(with.stats, without.stats, "{cols}");
            if with.pct.is_some() {
                assert!(
                    with.eta.is_some() || with.load_slot.is_none(),
                    "{cols}: {with:?}"
                );
            }
            assert!(
                !(eta_gone && with.eta.is_some()),
                "{cols}: the ETA came back"
            );
            assert!(
                !(load_gone && with.load_slot.is_some()),
                "{cols}: the load came back"
            );
            assert!(
                !(short && with.eta.is_some() && !with.eta_short),
                "{cols}: long again"
            );
            eta_gone |= with.eta.is_none();
            load_gone |= with.load_slot.is_none();
            short |= with.eta_short;
            assert!(
                render(&with, cols).chars().count() == cols,
                "{cols}: exactly the row"
            );
        }
        let install = |cols: usize| {
            layout_row(
                &RowSpec {
                    kind: RowKind::Message(id(23)),
                    severity: Severity::Info,
                    live: true,
                    glyph: '\u{21e3}',
                    title: "Installing ALab tools",
                    title_short: None,
                    detail0: None,
                    step: false,
                    step_short: None,
                    meter: Some((None, "10 programs \u{b7} ~3 GB")),
                    animated: false,
                    busy: true,
                    eta: false,
                    load: Some(Load::Network),
                    load_slot: [Load::Network, Load::Disk, Load::Cpu].into_iter().collect(),
                    capsules: caps(&[]),
                },
                cols,
                &chars,
            )
        };
        assert_eq!(
            install(120).stats.map(|s| s.1).as_deref(),
            Some("10 programs \u{b7} ~3 GB")
        );
        assert_eq!(install(80).stats.map(|s| s.1).as_deref(), Some("~3 GB"));
        assert!(install(80).load.is_some(), "80: the load words too");
    }

    /// The cuts of a Latin title, detail and sentence, every width from
    fn latin_cuts() -> String {
        let corpus = [
            "Couldn't use your keybindings file",
            "Keybinding conflict: \u{2318}K",
            "Claude usage limit reached, resets at 4pm",
            "3 keybindings ignored",
            "aterm closed unexpectedly \u{b7} last session restored",
            "Update ready \u{2014} restart to finish",
            "A-very-long-hyphenated-title here",
            "word",
            "",
            "two  spaces, here.",
        ];
        let mut out = String::new();
        for text in corpus {
            let n = text.chars().count();
            for max in 0..=n + 1 {
                out.push_str(&format!(
                    "T {text:?} {max} => {:?}\n",
                    title_in(text, max, &crate::text::char_width)
                ));
                out.push_str(&format!(
                    "M {text:?} {max} => {:?}\n",
                    crate::text::trim_words(text, max)
                ));
                out.push_str(&format!(
                    "S {text:?} {max} => {:?}\n",
                    crate::text::split_sentence(text, max)
                ));
            }
            out.push_str(&format!("F {text:?} => {:?}\n", first_word(text)));
        }
        out
    }

    /// LATIN TEXT IS CUT WHERE IT ALWAYS WAS: breaking between CJK
    #[test]
    fn latin_text_is_cut_where_it_always_was() {
        let golden = include_str!("../tests/fixtures/latin-cuts.txt");
        let now = latin_cuts();
        let moved: Vec<(&str, &str)> = golden
            .lines()
            .zip(now.lines())
            .filter(|(was, is)| was != is)
            .take(8)
            .collect();
        assert!(moved.is_empty(), "moved cuts (was, is): {moved:#?}");
        assert_eq!(golden.lines().count(), now.lines().count());
    }

    /// AN ALL-CJK TITLE IS CUT BETWEEN ITS CHARACTERS, never left out: with
    #[test]
    fn an_all_cjk_title_is_cut_between_its_characters() {
        let cells = |s: &str| aterm_grapheme::str_width(s);
        let title = "設定ファイルを読み込めませんでした";
        let whole = cells(title);
        for max in 3..whole {
            let cut = title_in(title, max, &cells).expect("some characters fit");
            let stem = cut.strip_suffix('\u{2026}').expect("cut with an ellipsis");
            assert!(title.starts_with(stem) && !stem.is_empty(), "{max}: {cut}");
            assert!(cells(&cut) <= max, "{max}: {cut} is {} cells", cells(&cut));
            assert!(whole_words_of(&cut, title), "{max}: {cut}");
            let next = aterm_grapheme::word_breaks(title)
                .into_iter()
                .find(|brk| brk.at > stem.len())
                .map_or(title.len(), |brk| brk.at);
            if next < title.len() {
                assert!(
                    cells(&format!("{}\u{2026}", &title[..next])) > max,
                    "{max}: {cut}"
                );
            }
        }
        assert_eq!(title_in(title, whole, &cells).as_deref(), Some(title));
        assert_eq!(first_word(title), "設\u{2026}");
        let quoted = "「設定」を保存しました。";
        for max in 3..cells(quoted) {
            if let Some(cut) = title_in(quoted, max, &cells) {
                let stem = cut.trim_end_matches('\u{2026}');
                assert!(!stem.ends_with('「'), "{max}: {cut}");
                let after = quoted[stem.len()..]
                    .trim_start_matches(TITLE_CUT_TRAIL)
                    .chars()
                    .next();
                assert!(
                    !matches!(after, Some('」' | '。')),
                    "{max}: {cut} leaves {after:?} to start the rest"
                );
            }
        }
        let spaces_only = |title: &str, max: usize| {
            title
                .match_indices(' ')
                .map(|(at, _)| format!("{}\u{2026}", &title[..at]))
                .take_while(|cut| cells(cut) <= max)
                .last()
        };
        assert_eq!(spaces_only(title, whole - 1), None, "negative control");
    }
}

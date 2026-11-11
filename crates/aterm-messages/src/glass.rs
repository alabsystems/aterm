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
        severity_class(live.msg.severity),
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

/// The status band's layout for one frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presentation {
    /// The width every row was laid out at.
    pub cols: usize,
    /// The rows, top to bottom (the overflow row last when present).
    pub rows: Vec<RowLayout>,
}

/// Which tier of the status band produced a [`RowLayout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// One message.
    Message(MessageId),
    /// A retired live row's completion echo in the slot it held.
    Echo(MessageId),
    /// `k more ›` — one link to Settings ▸ Messages.
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

/// Laid-out capsule on a status row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapsuleLayout {
    /// First cell.
    pub col: usize,
    /// `text + 2` pad cells.
    pub width: usize,
    /// The long or short form, as fitted.
    pub text: String,
    /// The intent's whole label (`""` on an echo's blank capsule): the band reads it against
    /// [`Self::text`] so one label reads one way across the band at a width.
    pub full_label: &'static str,
    /// Its ink.
    pub role: CapsuleRole,
    /// What a press does.
    pub action: ActionIndex,
}

/// What a click at cell `(row, col)` inside the band hits.
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

    /// Content-addressed fingerprint of the laid-out band.
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

/// 64-bit FNV-1a hasher, a byte at a time through `aterm_hash`'s step.
pub(crate) struct Fnv(u64);

impl Fnv {
    pub(crate) const fn new() -> Self {
        Self(aterm_hash::FNV64_OFFSET)
    }

    pub(crate) fn byte(&mut self, b: u8) {
        self.0 = aterm_hash::fnv_mix_u64(self.0, u64::from(b));
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
    pub const fn eta_width(&self) -> usize {
        match (self.eta, self.eta_short) {
            (None, _) => 0,
            (Some(_), false) => ETA_W,
            (Some(_), true) => ETA_SHORT_W,
        }
    }

    /// The elapsed slot's cells: [`ELAPSED_W`], or [`ELAPSED_SHORT_W`] in its short form; 0 with no slot.
    #[must_use]
    pub const fn elapsed_width(&self) -> usize {
        match (self.elapsed, self.elapsed_short) {
            (None, _) => 0,
            (Some(_), false) => ELAPSED_W,
            (Some(_), true) => ELAPSED_SHORT_W,
        }
    }
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
    /// The intent's whole label (`""` once [`Self::blanked`]): the band lays an intent short by it
    /// on every row once one row paints it short.
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
    /// `detail[0]`, when any.
    pub detail0: Option<&'a str>,
    /// `detail0` IS THE ONE STEP the row asks for.
    pub step: bool,
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
    fn activity(&self) -> Activity {
        match (self.fill(), self.busy, self.animated) {
            (Some(_), _, true) => Activity::LiveBar,
            (Some(_), _, false) => Activity::HeldBar,
            (None, true, _) => Activity::LiveBusy,
            (None, false, _) => Activity::None,
        }
    }

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
pub(crate) const fn overflow_spec(hidden: usize, words: &str) -> RowSpec<'_> {
    RowSpec {
        kind: RowKind::Overflow { hidden },
        severity: Severity::Info,
        live: false,
        glyph: OVERFLOW_GLYPH,
        title: words,
        detail0: None,
        step: false,
        meter: None,
        busy: false,
        animated: false,
        eta: false,
        load: None,
        load_slot: Loads::NONE,
        capsules: Vec::new(),
    }
}

const OVERFLOW_GLYPH: char = '\u{2026}';
const OVERFLOW_PROGRESS_GLYPH: char = '+';

const fn overflow_arrow(links: Links) -> &'static str {
    match links {
        Links::Painted => " \u{203a}",
        Links::Withheld => "",
    }
}

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

const TITLE_CUT_TRAIL: [char; 14] = [
    ' ', ',', ';', ':', '.', '\u{b7}', '\u{2014}', '\u{2013}', '\u{3001}', '\u{3002}', '\u{ff0c}',
    '\u{ff1a}', '\u{ff1b}', '\u{30fb}',
];

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

fn first_word(title: &str) -> String {
    match aterm_grapheme::word_breaks(title).first() {
        Some(brk) => format!(
            "{}\u{2026}",
            title[..brk.at].trim_end_matches(TITLE_CUT_TRAIL)
        ),
        None => title.to_string(),
    }
}

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
}

fn painted(caps: &[CapsuleSpec], short: bool) -> impl Iterator<Item = &CapsuleSpec> {
    caps.iter().filter(move |c| !short || !c.short.is_empty())
}

fn capsules_width(caps: &[CapsuleSpec], short: bool, width: &dyn Fn(&str) -> usize) -> usize {
    let (texts, n) = painted(caps, short).fold((0usize, 0usize), |(w, n), c| {
        (w + width(if short { &c.short } else { &c.paid }) + 2, n + 1)
    });
    texts + CAPSULE_GAP * n.saturating_sub(1)
}

fn clearance(caps: &[CapsuleSpec], short: bool) -> usize {
    if painted(caps, short).next().is_some() {
        BEFORE_CAPSULES
    } else {
        0
    }
}

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
    /// fixed head reserved for it (its floor), 0 once even the elided title
    /// could not pay for them (and then it is gone, not an extra).
    excerpt_floor: usize,
    /// The row's excerpt is an action excerpt (whether or not its floor
    /// survived): the load slot is then an extra after it.
    action_excerpt: bool,
}

fn load_slot_width(spec: &RowSpec<'_>, width: &dyn Fn(&str) -> usize) -> usize {
    let reserved = match spec.load {
        Some(l) if !spec.load_slot.is_empty() => spec.load_slot.with(l),
        _ if !spec.load_slot.is_empty() => spec.load_slot,
        _ => Loads::ALL,
    };
    reserved.iter().map(|l| width(l.words())).max().unwrap_or(0)
}

const fn reserves_load(spec: &RowSpec<'_>, activity: Activity) -> bool {
    matches!(activity, Activity::LiveBar | Activity::LiveBusy)
        && (!spec.load_slot.is_empty() || spec.load.is_some())
}

fn load_cells(spec: &RowSpec<'_>, activity: Activity, width: &dyn Fn(&str) -> usize) -> usize {
    if reserves_load(spec, activity) && !action_excerpt(spec, activity) {
        2 + load_slot_width(spec, width)
    } else {
        0
    }
}

fn action_excerpt(spec: &RowSpec<'_>, activity: Activity) -> bool {
    (spec.step || matches!(activity, Activity::LiveBar | Activity::LiveBusy))
        && spec.detail0.is_some_and(|d| !d.is_empty())
}

fn excerpt_floor(spec: &RowSpec<'_>, activity: Activity, width: &dyn Fn(&str) -> usize) -> usize {
    match spec.detail0 {
        Some(d) if action_excerpt(spec, activity) && spec.step => 3 + width(d),
        Some(d) if action_excerpt(spec, activity) => 3 + width(d).min(DETAIL_FLOOR),
        _ => 0,
    }
}

struct Head {
    title: String,
    /// The capsules' cells, and the clearance before them.
    caps: usize,
    before: usize,
    short: bool,
    details_dropped: bool,
    /// What the head takes of the budget, the excerpt's `ex` included.
    fixed: usize,
}

fn fit_head(
    spec: &RowSpec<'_>,
    title: &str,
    budget: usize,
    a: usize,
    ex: usize,
    width: &dyn Fn(&str) -> usize,
) -> Head {
    let mut head = head_around(spec, title, budget, a, ex, width);
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

fn head_around(
    spec: &RowSpec<'_>,
    title: &str,
    budget: usize,
    a: usize,
    ex: usize,
    width: &dyn Fn(&str) -> usize,
) -> Head {
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
        caps,
        before,
        short,
        details_dropped,
        fixed,
    }
}

fn elide(title: &str, allowed: usize, width: &dyn Fn(&str) -> usize) -> String {
    title_in(title, allowed.max(TITLE_MIN), width).unwrap_or_else(|| first_word(title))
}

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
    let mut head = fit_head(spec, full_title, budget, a, ex, width);
    if head.fixed > budget && spec.step && ex > 3 + DETAIL_FLOOR {
        // An ask's step that the title at its floor still cannot pay for
        // whole takes the cells left beside the title, down to its floor
        // (`DETAIL_FLOOR`), and is cut there; below the floor it drops.
        let left = budget.saturating_sub(2 + width(&head.title) + a + head.before + head.caps);
        ex = left.max(3 + DETAIL_FLOOR).min(ex);
        head.fixed = 2 + width(&head.title) + a + ex + head.before + head.caps;
    }
    if head.fixed > budget && ex > 0 {
        ex = 0;
        head = fit_head(spec, full_title, budget, a, ex, width);
    }
    let Head {
        mut title,
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
        // The spare clear cell goes to the title where it keeps a word more.
        let left = budget.saturating_sub(2 + clear + caps);
        let spare = clear.saturating_sub(1);
        let tight = title_in(full_title, left, width);
        let roomy = if spare > 0 {
            title_in(full_title, left + spare, width)
        } else {
            None
        };
        title = match (tight, roomy) {
            (Some(tight), Some(roomy)) if roomy != tight => roomy,
            (tight, roomy) => tight.or(roomy).unwrap_or_default(),
        };
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
    }
}

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

fn floor_clause(detail0: &str, width: &dyn Fn(&str) -> usize) -> Option<String> {
    let lead = lead_clause(detail0)?;
    let floor = shape_to_cells(detail0, DETAIL_FLOOR, width)?;
    (width(&lead) < DETAIL_FLOOR && floor.starts_with(&lead)).then_some(lead)
}

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

struct Extras {
    /// The elapsed slot's cells ([`ELAPSED_W`] or [`ELAPSED_SHORT_W`]); 0
    /// for none.
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
        spec.detail0
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

fn place(
    spec: &RowSpec<'_>,
    cols: usize,
    width: &dyn Fn(&str) -> usize,
    fit: &Fit,
    extras: Extras,
) -> RowLayout {
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

fn says_whole(title: &str, words: &str) -> bool {
    let joins = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '.' || c == ',');
    !words.is_empty()
        && title.match_indices(words).any(|(at, _)| {
            !joins(title[..at].chars().next_back())
                && !joins(title[at + words.len()..].chars().next())
        })
}

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
#[path = "glass_tests.rs"]
pub(crate) mod tests;

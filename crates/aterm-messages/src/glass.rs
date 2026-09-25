// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The glass: which rows rank where (pure over the live set), and the
//! single-row WIDTH LAW that turns one message into one `cols`-wide row of
//! cells — glyph, bold title, ` · ` excerpt, percent or elapsed clock, ETA,
//! load words, stats, and the right-aligned capsules (design §1.5,
//! generalised from status_bars.rs:2365-2485) — over the row's METER.
//! Everything here is a pure function of its arguments; the host paints the
//! [`Presentation`] it gets back.
//!
//! THE METER IS THE ROW (owner, 2026-09-23 — design ruling 55, kept by the
//! merge's ruling 136). A metered row's meter is its whole width, `(0, cols,
//! fill)`: 0 % at the window's left edge, 100 % at its right edge, and
//! everything else on the row is written over it. It takes no room from the
//! words and is never sacrificed; the host maps the fill onto the window's
//! pixels, gutters included. The owner, of the 8–20-cell block that sat
//! after the excerpt: *"the progress bar doesn't go all the way across the
//! screen"*, and *"make sure that 0% and 100% and similar concepts are
//! mapped to the relative size of the screen with such top progress bars"*.
//! A BUSY row ([`crate::Meter::busy`]) carries `track = (0, cols)`: the same
//! surface, for the comet (ruling 75). The pill, its caps and the meter's
//! width constants retired with it (ruling 136).
//!
//! Fit order per row, `budget = cols − 2·MARGIN` (design §1.5, extended by
//! §10.4.5 for the live words, amended by ruling 136):
//!
//! 1. `fixed = head + A + BEFORE_CAPSULES + capsules(long)`, where `A` is the
//!    ACTIVITY's words: [`PCT_W`] for a row with a fill (the right-aligned
//!    percent), `1 + ELAPSED_W` for a busy one (the elapsed slot), 0
//!    otherwise — and, on a moving row that carries the load SLOT, `3 +` the
//!    widest load words' width more, whether its words show yet or not: for
//!    a routine pass the load is the one reason the row is on the glass at
//!    all, so the words are paid for by the capsules' short forms and the
//!    title's elision before they drop (review 2026-09-23), and the slot is
//!    reserved at ONE width for the row's life so words arriving, changing
//!    resource or leaving never re-lay a moving row (review round 2). Over
//!    budget → every capsule takes its SHORT form (a capsule with none, the
//!    implicit `Details ›`, goes: a lone `›` said nothing the row body does
//!    not do); still over → the title elides down to [`TITLE_MIN`]; still
//!    over (cols < ~24) → degenerate: `A` goes whole, the title takes what
//!    is left, the implicit `Details ›` goes, and the capsules stand from
//!    column 0 for the painter to clip. Rows are always exactly `cols`
//!    cells; never a panic; **authored capsules are never dropped**. The
//!    meter and the track cost nothing and outlive even the degenerate step.
//!
//!    An ACTION EXCERPT — a painted `detail[0]` on a moving row, which the
//!    host paints only when it changes what the person does (ruling 77):
//!    the flow row's typing hold — is the one thing on such a row the person
//!    must read. Its joint and its first [`DETAIL_FLOOR`] cells join `fixed`
//!    (`3 + min(width, DETAIL_FLOOR)`), so the capsules' short forms and the
//!    title's elision pay for it; only when the title at [`TITLE_MIN`] still
//!    cannot does it go, before the degenerate step. Its row's load slot is not in `fixed`:
//!    it is an extra after the excerpt (review 2026-09-24 — at 80 columns
//!    the slot reserved at `network busy` left the retired admin install's
//!    password line no room).
//! 2. `room = budget − fixed`, `fixed` measured with the LONG capsules
//!    whatever they became: the short forms fit the head, and the cells they
//!    free never re-buy an extra a wider row already gave up — so once the
//!    capsules are short the room is nil and every extra below is monotone
//!    as the row narrows. The extras, in allocation order — (a) the ETA
//!    slot, `1 + ETA_W` (else `1 + ETA_SHORT_W`), when a moving determinate
//!    row asks for one; (b) the excerpt (`detail[0]`): whole if `3 + width ≤
//!    room`, else shaped to `room − 3` CELLS if `room ≥ 3 + DETAIL_FLOOR`,
//!    else dropped — never a stub (an action excerpt adds its reserved
//!    floor to the room, and a starved one keeps just the floor); (b′) an
//!    action excerpt's row's load slot, `3 +` the widest load words, beside a
//!    WHOLE excerpt when it fits; (c) stats if `2 + len` fits after the
//!    excerpt — beside a WHOLE excerpt (or none at all). An extra asked for
//!    that does not fit starves every extra after it.
//!
//! Sacrifice order, therefore, as the row narrows: stats → excerpt (shaped,
//! then dropped) → ETA → capsule short forms → title elision → the activity
//! words with the load words (on an action excerpt's row: stats → load
//! slot → excerpt shaped → ETA → capsule short forms → title elision → the
//! excerpt → the activity words); each a monotone flag while the row narrows
//! (what is gone at one width is gone at every narrower one). The meter is
//! never on that list.
//!
//! Cells on a row: `glyph title · excerpt ␠pct|elapsed ␠eta ·␠load ␠␠stats …
//! capsules`, all over the meter. The percent is right-aligned in its four
//! cells and the time words are left-aligned in theirs, so nothing after
//! them moves as the numbers change.

use std::cmp::Reverse;

use crate::center::Live;
use crate::model::{ActionIndex, Hold, Intent, Load, MessageId, Severity};
use crate::text::{shape_detail, truncate};
use crate::{
    BEFORE_CAPSULES, CAPSULE_GAP, DETAIL_FLOOR, ELAPSED_W, ETA_SHORT_W, ETA_W, GLYPH_COL, Instant,
    MARGIN, PCT_W, TITLE_COL, TITLE_MIN,
};

/// The glass rank of a live row, highest first: asks, then the severity
/// CLASS ([`severity_class`]), then live/standing before held, then the
/// earlier post (ties by the lower id).
pub type Rank = (bool, u8, bool, Reverse<Instant>, Reverse<MessageId>);

/// The rank's severity term: Error above Warn above the quiet tones, and
/// Success WITH Info — one class, so the two order by `posted_at` alone.
/// A launch pass posts `✓ ALab tools installed` and then the `↻`
/// machine-settings rows; with Info over Success the pass's own result
/// sat behind the overflow row for the length of the machine rows' holds
/// (review, 2026-09-22), where today's lane shows it first. Design §2.4's
/// own consequence — "rank ties break on `posted_at`" for an `↻` row
/// before a `✓` row — presumes exactly this class.
#[must_use]
pub const fn severity_class(severity: Severity) -> u8 {
    match severity {
        Severity::Success | Severity::Info => 0,
        Severity::Warn => 1,
        Severity::Error => 2,
    }
}

/// The rank tuple of one row.
#[must_use]
pub fn rank(live: &Live) -> Rank {
    (
        live.msg.is_ask(),
        severity_class(live.msg.severity),
        matches!(live.msg.hold, Hold::Live { .. } | Hold::Standing),
        Reverse(live.posted_at),
        Reverse(live.id),
    )
}

/// `true` when `a` outranks `b` strictly.
#[must_use]
pub fn outranks(a: &Live, b: &Live) -> bool {
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
    /// A retired live row's completion ECHO in the slot it held (design
    /// §10.4.3): laid out as the row was, painted by the motion layer, not
    /// pressable, not announced, never counted as a row.
    Echo(MessageId),
    /// `N more messages` — one link to Settings ▸ Messages.
    Overflow {
        /// How many eligible rows are not on glass.
        hidden: usize,
    },
}

/// One row in cells. Columns are absolute (0-based); every string is
/// already shaped to fit; the ` · ` joint occupies the three cells before
/// `detail`'s column (and `load`'s), the space before `pct`'s, `elapsed`'s
/// and `eta`'s, and the two before `stats`'. The meter (or a busy row's
/// track) is the whole row, under every piece (ruling 136).
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
    /// `(col, width, fill_permille)` of the row's meter — always `(0, cols,
    /// fill)`, THE WHOLE ROW, whenever the message carries a fill (see the
    /// module docs): the host lays it under every other piece and maps the
    /// fill onto the window's full pixel width.
    pub meter: Option<(usize, usize, u16)>,
    /// Work in flight with no fill ([`crate::Meter::busy`]): the motion
    /// layer spins the glyph cell and sweeps the comet along the track.
    pub busy: bool,
    /// `(col, width)` of a busy row's track — always `(0, cols)`, THE WHOLE
    /// ROW, like a meter (ruling 75): the host lays it under every other
    /// piece and maps the comet onto the window's full pixel width. Never
    /// with `meter`.
    pub track: Option<(usize, usize)>,
    /// The `NN%` text and its column (right-aligned in its four cells).
    pub pct: Option<(usize, String)>,
    /// The elapsed slot's column ([`ELAPSED_W`] cells) of a busy row; the
    /// motion layer writes the words.
    pub elapsed: Option<usize>,
    /// The ETA slot's column ([`Self::eta_width`] cells: [`ETA_W`], or
    /// [`ETA_SHORT_W`] where the long form did not fit); the motion layer
    /// writes the words.
    pub eta: Option<usize>,
    /// The ETA slot is in its SHORT form (`59m left`, [`ETA_SHORT_W`] cells).
    pub eta_short: bool,
    /// The heavy-load words and their column (the ` · ` joint before them),
    /// when they show.
    pub load: Option<(usize, &'static str)>,
    /// The reserved load slot's column and width, words or not — the cells
    /// the row keeps for them (the joint in the three before).
    pub load_slot: Option<(usize, usize)>,
    /// The stats text and its column.
    pub stats: Option<(usize, String)>,
    /// The capsules, left to right.
    pub capsules: Vec<CapsuleLayout>,
}

/// Which chip a capsule is. The role follows the INTENT, not its position:
/// the accent marks a consequential press ([`Intent::is_consequential`]),
/// so a row of navigations wears quiet chips only and two accent chips never
/// stack on one band (Phase 1 review ruling 18, 2026-09-22).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapsuleRole {
    /// A consequential authored intent (Install now, Install, a system pane,
    /// New window): accent fill.
    Primary,
    /// Every other authored intent — a navigation or a decline (Packages,
    /// Software Update, Open log, Open aterm.toml, Not now): track fill.
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

    /// The repaint-key term: **exactly `0` with no rows**, else a nonzero
    /// FNV-1a over everything the painter reads (status_bars.rs:1024-1053).
    /// The fill is folded quantized to whole percent — the resolution the
    /// `NN%` readout shows — so a byte tick that cannot move a glyph does
    /// not re-present the frame.
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
            // The busy row's STATIC shape only: what moves is the motion
            // layer's, folded in its own term.
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

/// FNV-1a, the status bars' hash, over typed fields with separators.
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
    /// The ETA slot's cells: [`ETA_W`], or [`ETA_SHORT_W`] in its short
    /// form; 0 with no slot.
    #[must_use]
    pub fn eta_width(&self) -> usize {
        match (self.eta, self.eta_short) {
            (None, _) => 0,
            (Some(_), false) => ETA_W,
            (Some(_), true) => ETA_SHORT_W,
        }
    }

    /// The spoken sentence: `title` or `title · <detail[0] whole>` —
    /// width-independent, so a resize that re-shapes or drops the painted
    /// excerpt never re-announces a row (design §3.5) — with the band's
    /// pictographic joints said as words ([`speakable`]).
    #[must_use]
    pub fn spoken(&self, full_detail0: &str) -> String {
        if full_detail0.is_empty() {
            speakable(&self.full_title)
        } else {
            speakable(&format!("{} \u{00b7} {full_detail0}", self.full_title))
        }
    }
}

/// The painted words as a screen reader should say them. The band's terse
/// excerpts join their parts with pictographs a reader spells out
/// (`windw_padding right arrow window_padding?`, `pane 20 multiplication sign
/// 5`, review 2026-09-24): ` → ` (an unknown key's near miss) is said `, did
/// you mean `, ` ▸ ` (a Settings route) `, `, and `×` between two digits (a
/// pane's size) ` by `. The paint is unchanged.
#[must_use]
pub fn speakable(words: &str) -> String {
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

/// Whether the IMPLICIT links are laid out — a row's `Details ›` (`+N ›`
/// on a single row) and the overflow row's `Messages ›`. The authored
/// capsules are laid out either way; the host withholds the links until it
/// has the page they open (design §8, Phase 1: a link with no destination
/// is dishonest, and a press on it that only folded the row was worse).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Links {
    /// Authored capsules only; the overflow row is words alone.
    Withheld,
    /// Every row ends in its link (the design's end state, Phase 2 on).
    Painted,
}

/// One capsule before layout: both forms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapsuleSpec {
    /// The long form.
    pub long: String,
    /// The short form the width law falls back to; EMPTY for a capsule
    /// with none, which the law drops instead (the implicit `Details ›`).
    pub short: String,
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
    pub fn authored(intent: &Intent, index: u8) -> Self {
        Self {
            long: intent.label().to_string(),
            short: intent.short().to_string(),
            full_label: intent.label(),
            role: if intent.is_consequential() {
                CapsuleRole::Primary
            } else {
                CapsuleRole::Secondary
            },
            action: ActionIndex(index),
        }
    }

    /// The implicit `Details ›`; with `hidden > 0` behind a single committed
    /// row it reads `+N ›`. `Details ›` has NO short form: below its long
    /// width it goes, rather than painting a lone `›` — the row body
    /// performs Details (§2.2), and the cells go to the title and the
    /// activity (review round 2, 2026-09-23). `+N›` keeps its count.
    #[must_use]
    pub fn details(hidden: usize) -> Self {
        let (long, short) = if hidden > 0 {
            (format!("+{hidden} \u{203a}"), format!("+{hidden}\u{203a}"))
        } else {
            ("Details \u{203a}".to_string(), String::new())
        };
        Self {
            long,
            short,
            full_label: Intent::Details.label(),
            role: CapsuleRole::Details,
            action: ActionIndex::DETAILS,
        }
    }

    /// The overflow row's one capsule.
    #[must_use]
    pub fn messages() -> Self {
        Self {
            long: "Messages \u{203a}".to_string(),
            short: "\u{203a}".to_string(),
            full_label: "Messages \u{203a}",
            role: CapsuleRole::Details,
            action: ActionIndex::DETAILS,
        }
    }
}

/// One row before layout — the pure input to the width law, so the law is
/// testable on literal fixtures without a center.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "five independent inputs to the width law (live ink, busy, moving fill, ETA slot, load slot), each read once; a state enum would multiply them"
)]
pub struct RowSpec<'a> {
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
    /// `(fill_permille, stats)` when metered; a `None` fill draws no meter
    /// and no pct (its stats alone — or, with `busy`, the track).
    pub meter: Option<(Option<u16>, &'a str)>,
    /// Work in flight with no fill ([`crate::Meter::busy`]): the whole row
    /// is its track, and the elapsed slot is part of the fixed head.
    pub busy: bool,
    /// The row's FILL moves (a live row's glide and glint): its percent may
    /// bring the ETA slot and the load slot. A still fill (a held row's)
    /// brings neither.
    pub animated: bool,
    /// The fill is a measured LEVEL ([`crate::Meter::level`], ruling 208):
    /// the whole row still, but no percent — the elapsed clock (how long
    /// the strain has lasted) and the load slot in its place — and never
    /// an ETA.
    pub level: bool,
    /// Reserve the ETA slot (a live determinate row whose reporter supplies
    /// an amount).
    pub eta: bool,
    /// The heavy-load words, when they show.
    pub load: Option<Load>,
    /// The row reserves the load slot (it has declared a load): the slot is
    /// laid out whether or not `load` shows, at the widest words' width.
    pub load_slot: bool,
    /// The capsules, left to right (authored first, `Details ›` last).
    pub capsules: Vec<CapsuleSpec>,
}

impl RowSpec<'_> {
    /// The activity cells the fixed head reserves (module doc, step 1).
    fn activity(&self) -> Activity {
        match (self.fill(), self.busy, self.animated) {
            (Some(_), _, _) if self.level => Activity::LiveLevel,
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
    /// A measured level: the elapsed slot (and the load slot it may bring)
    /// over its fill — never a percent, never an ETA.
    LiveLevel,
    /// A still fill: the percent.
    HeldBar,
}

impl Activity {
    const fn cells(self) -> usize {
        match self {
            Self::None => 0,
            Self::LiveBar | Self::HeldBar => PCT_W,
            Self::LiveBusy | Self::LiveLevel => 1 + ELAPSED_W,
        }
    }
}

/// The overflow row's spec: `… N more messages`, with its `Messages ›`
/// link when links are painted.
#[must_use]
pub fn overflow_spec(hidden: usize, links: Links) -> RowSpec<'static> {
    RowSpec {
        kind: RowKind::Overflow { hidden },
        severity: Severity::Info,
        live: false,
        glyph: '\u{2026}',
        title: "",
        detail0: None,
        meter: None,
        busy: false,
        animated: false,
        level: false,
        eta: false,
        load: None,
        load_slot: false,
        capsules: match links {
            Links::Withheld => Vec::new(),
            Links::Painted => vec![CapsuleSpec::messages()],
        },
    }
}

/// `s` fitted into `max` cells under `width`, ending in `…` when cut.
fn fit_width(s: &str, max: usize, width: &dyn Fn(&str) -> usize) -> String {
    let mut t = truncate(s, max);
    while width(&t) > max && !t.is_empty() {
        let n = t.chars().count();
        t = truncate(&t, n - 1);
    }
    t
}

/// The first cell right of the title that another piece of `row` paints —
/// the excerpt's and the load slot's ` · ` joints count — else the row's
/// right margin.
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
        at(c.saturating_sub(2));
    }
    if let Some((c, _)) = &row.stats {
        at(*c);
    }
    if let Some(cap) = row.capsules.first() {
        at(cap.col);
    }
    next
}

/// A Complete echo's title (design ruling 154): `words` — the row's finished
/// form — in the title's place, with NOTHING else on the row moved. It takes
/// the laid title's cells, padded with blanks when shorter, and may run on
/// into the blank cells before the next piece (one kept clear) before it
/// elides; the spoken title is `words` whole.
pub fn finish_title(row: &mut RowLayout, words: &str, cols: usize, width: &dyn Fn(&str) -> usize) {
    let laid = width(&row.title.1);
    if laid == 0 {
        return;
    }
    let room = next_piece(row, cols)
        .saturating_sub(row.title.0 + 1)
        .max(laid);
    let mut title = if width(words) > room {
        fit_width(words, room, width)
    } else {
        words.to_string()
    };
    let short = laid.saturating_sub(width(&title));
    title.extend(std::iter::repeat_n(' ', short));
    row.title.1 = title;
    row.full_title = words.to_string();
}

/// The capsules painted in the long or the short forms: every one long; the
/// ones that HAVE a short form short.
fn painted(caps: &[CapsuleSpec], short: bool) -> impl Iterator<Item = &CapsuleSpec> {
    caps.iter().filter(move |c| !short || !c.short.is_empty())
}

/// The cells the capsules take together.
fn capsules_width(caps: &[CapsuleSpec], short: bool, width: &dyn Fn(&str) -> usize) -> usize {
    let (texts, n) = painted(caps, short).fold((0usize, 0usize), |(w, n), c| {
        (w + width(if short { &c.short } else { &c.long }) + 2, n + 1)
    });
    texts + CAPSULE_GAP * n.saturating_sub(1)
}

/// The clearance before the first capsule — nil when none is painted: it
/// exists for a capsule, and a row with none keeps the cells.
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
    /// The activity that survived (none in the degenerate step).
    activity: Activity,
    details_dropped: bool,
    room: usize,
    /// A moving row's PAINTED excerpt ([`action_excerpt`]): the cells the
    /// fixed head reserved for it (its floor), 0 once even the elided title
    /// could not pay for them — and then it is gone, not an extra.
    excerpt_floor: usize,
    /// The row's excerpt is an action excerpt (whether or not its floor
    /// survived): the load slot is then an extra after it.
    action_excerpt: bool,
}

/// The load slot's width: the widest load words under `width`, so a
/// resource change never re-lays the row.
fn load_slot_width(width: &dyn Fn(&str) -> usize) -> usize {
    Load::ALL
        .iter()
        .map(|l| width(l.words()))
        .max()
        .unwrap_or(0)
}

/// Whether the row lays out the load slot: a moving activity on a row that
/// reserves one (or shows words now).
fn reserves_load(spec: &RowSpec<'_>, activity: Activity) -> bool {
    matches!(
        activity,
        Activity::LiveBar | Activity::LiveBusy | Activity::LiveLevel
    ) && (spec.load_slot || spec.load.is_some())
}

/// The load slot's cells, joint included, when the row lays it out in the
/// fixed head (module doc, step 1) — not on a row whose excerpt is an ACTION
/// excerpt ([`action_excerpt`]), where the slot is an extra after it.
fn load_cells(spec: &RowSpec<'_>, activity: Activity, width: &dyn Fn(&str) -> usize) -> usize {
    if reserves_load(spec, activity) && !action_excerpt(spec, activity) {
        3 + load_slot_width(width)
    } else {
        0
    }
}

/// Whether the row's excerpt is an ACTION excerpt: a PAINTED `detail[0]`
/// on a MOVING row. The host paints a moving row's excerpt only when it
/// changes what the person does (ruling 77) — the flow row's typing hold —
/// so it is the one thing on the row the person must read: it outranks the load slot
/// and is paid for by the capsules' short forms and the title's elision,
/// never dropped while they can pay (module doc, step 1; review 2026-09-24).
fn action_excerpt(spec: &RowSpec<'_>, activity: Activity) -> bool {
    matches!(
        activity,
        Activity::LiveBar | Activity::LiveBusy | Activity::LiveLevel
    ) && spec.detail0.is_some_and(|d| !d.is_empty())
}

/// The cells an action excerpt reserves in the fixed head: its joint and
/// its first [`DETAIL_FLOOR`] cells (the whole excerpt when shorter).
fn excerpt_floor(spec: &RowSpec<'_>, activity: Activity, width: &dyn Fn(&str) -> usize) -> usize {
    match spec.detail0 {
        Some(d) if action_excerpt(spec, activity) => 3 + width(d).min(DETAIL_FLOOR),
        _ => 0,
    }
}

/// Step 1: capsules long → short, then the title, then the degenerate step.
fn fit_fixed(spec: &RowSpec<'_>, cols: usize, width: &dyn Fn(&str) -> usize) -> Fit {
    let budget = cols.saturating_sub(2 * MARGIN);
    let overflow_title = match spec.kind {
        RowKind::Overflow { hidden } => Some(format!("{hidden} more messages")),
        RowKind::Message(_) | RowKind::Echo(_) => None,
    };
    let full_title = overflow_title.as_deref().unwrap_or(spec.title);
    let mut title = full_title.to_string();
    let mut activity = spec.activity();
    let action_excerpt = action_excerpt(spec, activity);
    let mut a = activity.cells() + load_cells(spec, activity, width);
    // An action excerpt's floor is part of the fixed head: the capsules'
    // short forms and the title's elision pay for it before it drops.
    let mut ex = excerpt_floor(spec, activity, width);
    let mut short = false;
    let mut caps = capsules_width(&spec.capsules, false, width);
    // The clearance before the first capsule exists for a capsule: a row
    // with none keeps the two cells for its excerpt.
    let mut before = clearance(&spec.capsules, false);
    let mut fixed = 2 + width(&title) + a + ex + before + caps;
    // The extras' room is measured against the long capsules: the short
    // forms and the elided title fit the head only, and the cells they free
    // never re-buy an excerpt a wider row already gave up.
    let room = budget.saturating_sub(fixed);
    if fixed > budget {
        short = true;
        caps = capsules_width(&spec.capsules, true, width);
        before = clearance(&spec.capsules, true);
        fixed = 2 + width(&title) + a + ex + before + caps;
    }
    let elide = |title: &str, ex: usize| {
        let allowed = budget
            .saturating_sub(2 + a + ex + before + caps)
            .max(TITLE_MIN);
        if allowed < width(title) {
            fit_width(title, allowed, width)
        } else {
            title.to_string()
        }
    };
    if fixed > budget {
        title = elide(&title, ex);
        fixed = 2 + width(&title) + a + ex + before + caps;
    }
    if fixed > budget && ex > 0 {
        // Even the elided title cannot pay for the action excerpt: it goes
        // (for good — every narrower row is shorter still), and the title
        // takes back what it can of the cells.
        ex = 0;
        title = elide(full_title, 0);
        fixed = 2 + width(&title) + a + before + caps;
    }
    let mut details_dropped = false;
    if fixed > budget {
        // Degenerate: the activity's words go whole (pct or elapsed, and the
        // load slot, together — the meter and the track stay: they cost
        // nothing), the title takes what is left, and the implicit Details
        // capsule goes before any authored one is touched.
        activity = Activity::None;
        a = 0;
        let mut others = 2 + a + before + caps;
        if others > budget && spec.capsules.last().is_some_and(|c| c.action.is_details()) {
            details_dropped = true;
            let kept = &spec.capsules[..spec.capsules.len() - 1];
            caps = capsules_width(kept, true, width);
            others = 2 + clearance(kept, true) + caps;
        }
        title = fit_width(&title, budget.saturating_sub(others), width);
    }
    Fit {
        short,
        title,
        activity,
        details_dropped,
        room,
        excerpt_floor: ex,
        action_excerpt,
    }
}

/// `detail0` shaped to at most `target` CELLS under `width`. [`shape_detail`]
/// cuts by chars; under a wide measure its result can still be too wide, so
/// the char cap is lowered until the cells fit — to the LARGEST cap that
/// does, so what is shown at one width is shown at every wider one and the
/// excerpt never flickers during a resize. `None` when nothing fits.
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
        // A whole excerpt too wide for its cells comes back whole at every
        // cap down to its own char count: step past them at once.
        cap = if n == whole { n } else { cap }.checked_sub(1)?;
    }
}

/// What the extras won, in cells, before placement.
struct Extras {
    /// The ETA slot's cells ([`ETA_W`] or [`ETA_SHORT_W`]); 0 for none.
    eta_w: usize,
    load_slot: bool,
    load: Option<&'static str>,
    detail: Option<String>,
    stats: Option<String>,
}

/// Lay one row out at `cols` under the injected cell measure.
#[must_use]
pub fn layout_row(spec: &RowSpec<'_>, cols: usize, width: &dyn Fn(&str) -> usize) -> RowLayout {
    let fit = fit_fixed(spec, cols, width);
    let extras = allocate_extras(spec, &fit, width);
    place(spec, cols, width, &fit, extras)
}

/// Step 2: the extras from the room the fixed head left, in allocation
/// order (module doc). The meter is not one: it costs nothing.
fn allocate_extras(spec: &RowSpec<'_>, fit: &Fit, width: &dyn Fn(&str) -> usize) -> Extras {
    let mut room = fit.room;
    // An extra the row asked for that did not fit STARVES every extra after
    // it: they are sacrificed first as the row narrows, so none of them may
    // come back in the cells the starved one left.
    let mut starved = false;
    // a. the ETA slot: long, else short — and a short slot starves the
    //    extras after it, as a short capsule does, so the cells it saved
    //    never re-buy an excerpt or stats a wider row gave up.
    let mut eta_w = 0;
    if spec.eta && fit.activity == Activity::LiveBar {
        if ETA_W < room {
            eta_w = ETA_W;
        } else if ETA_SHORT_W < room {
            eta_w = ETA_SHORT_W;
            starved = true;
        } else {
            starved = true;
        }
        if eta_w > 0 {
            room -= 1 + eta_w;
        }
    }
    // The load slot and its words: paid for in the fixed head, so a moving
    // row that kept its activity keeps them — except beside an action
    // excerpt, where the slot is an extra after it (below).
    let mut load_slot = reserves_load(spec, fit.activity) && !fit.action_excerpt;
    // b. the excerpt: whole, shaped at or above the floor, else dropped. An
    //    action excerpt has its floor from the fixed head (a starved extra
    //    before it leaves it that floor, no more), and none once that floor
    //    went.
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
        } else if cells >= 3 + DETAIL_FLOOR
            && let Some(shaped) = shape_to_cells(d, cells - 3, width)
        {
            cells -= 3 + width(&shaped);
            detail = Some(shaped);
        }
        if fit.action_excerpt {
            // What the excerpt left is the row's again only beside a WHOLE
            // excerpt: a shaped one ends where its words did, and the cells
            // a word-boundary cut left must not re-buy what a wider row had.
            room += if detail_whole { cells } else { 0 };
        } else {
            room = cells;
        }
    }
    // b′. the load slot beside a WHOLE action excerpt, when it fits.
    if fit.action_excerpt
        && detail_whole
        && !starved
        && reserves_load(spec, fit.activity)
        && 3 + load_slot_width(width) <= room
    {
        load_slot = true;
        room -= 3 + load_slot_width(width);
    }
    let load = spec.load.map(Load::words).filter(|_| load_slot);
    // c. stats beside a whole excerpt (or none): a cut excerpt already
    //    said the row is short, and the stats never come back below that.
    let mut stats: Option<String> = None;
    if let Some((_, s)) = spec
        .meter
        .filter(|(_, s)| !s.is_empty() && detail_whole && !starved)
    {
        let sw = width(s);
        if 2 + sw <= room {
            stats = Some(s.to_string());
        }
    }
    Extras {
        eta_w,
        load_slot,
        load,
        detail,
        stats,
    }
}

/// Placement: the left flow, then the capsules right-aligned, all over the
/// meter (or the busy row's track) — the whole row.
fn place(
    spec: &RowSpec<'_>,
    cols: usize,
    width: &dyn Fn(&str) -> usize,
    fit: &Fit,
    extras: Extras,
) -> RowLayout {
    let full_title = match spec.kind {
        RowKind::Overflow { hidden } => format!("{hidden} more messages"),
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
    // THE METER IS THE ROW: every column, under everything else.
    let meter = fill.filter(|_| cols > 0).map(|f| (0, cols, f));
    // A BUSY ROW'S TRACK IS THE ROW TOO (ruling 75): the same whole-row
    // surface, for the comet — never with a meter.
    let busy = spec.busy && fill.is_none();
    let track = (busy && cols > 0).then_some((0, cols));
    let mut pct = None;
    let mut elapsed = None;
    match (fit.activity, fill) {
        (Activity::LiveBar | Activity::HeldBar, Some(p)) => {
            let text = format!("{}%", p / 10);
            col += 1;
            let slot = PCT_W - 1;
            pct = Some((col + slot.saturating_sub(width(&text)), text));
            col += slot;
        }
        (Activity::LiveBusy | Activity::LiveLevel, _) => {
            col += 1;
            elapsed = Some(col);
            col += ELAPSED_W;
        }
        _ => {}
    }
    let eta = (extras.eta_w > 0).then(|| {
        col += 1;
        let at = col;
        col += extras.eta_w;
        at
    });
    let eta_short = extras.eta_w == ETA_SHORT_W;
    let load_slot = extras.load_slot.then(|| {
        col += 3;
        let at = col;
        let w = load_slot_width(width);
        col += w;
        (at, w)
    });
    let load = load_slot.and_then(|(at, _)| extras.load.map(|words| (at, words)));
    let stats = extras.stats.map(|s| {
        col += 2;
        (col, s)
    });
    let capsules = place_capsules(spec, cols, width, fit);
    RowLayout {
        kind: spec.kind,
        severity: spec.severity,
        live: spec.live,
        glyph: (GLYPH_COL, spec.glyph),
        title: (TITLE_COL, fit.title.clone()),
        full_title,
        detail,
        meter,
        busy,
        track,
        pct,
        elapsed,
        eta,
        eta_short,
        load,
        load_slot,
        stats,
        capsules,
    }
}

/// The capsules, right-aligned: long or short as fitted, the implicit
/// Details dropped in the degenerate step.
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

    /// Paint a layout's WORDS into exactly `cols` cells the way the host
    /// will: text at its columns, the ` · ` joint before the excerpt and the
    /// load words, capsules as ` text ` — everything clipped at `cols`. The
    /// meter (and a busy row's track) is the row's surface, not text: it is
    /// `(0, cols, …)` whenever present, so it paints no character here and
    /// tests assert it field by field (ruling 136). The time slots are the
    /// motion layer's and stay blank here.
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
            put(col - 2, "\u{00b7}");
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

    /// The five §2.3 fixtures: crash, file access, staged (manual and auto),
    /// config warning, toolchain meter.
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
                    detail0: Some(CRASH_DETAIL),
                    meter: None,
                    animated: false,
                    level: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: false,
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
                    detail0: Some("Full Disk Access may already be enabled"),
                    meter: None,
                    animated: false,
                    level: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: false,
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
                    detail0: Some(STAGED_MANUAL),
                    meter: None,
                    animated: false,
                    level: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: false,
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
                    detail0: Some(STAGED_AUTO),
                    meter: None,
                    animated: false,
                    level: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: false,
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
                    detail0: Some("skipping \"cmd+shift+k\": unknown action \"foo\""),
                    meter: None,
                    animated: false,
                    level: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: false,
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
                    detail0: Some("trust \u{00b7} extracting"),
                    meter: Some((Some(420), "512 MB / 1.2 GB")),
                    animated: true,
                    level: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: false,
                    capsules: caps(&[Intent::OpenSettings {
                        route: "/packages".into(),
                    }]),
                },
            ),
        ]
    }

    /// The live indicator's fixtures (design §10.4.5): a download with its
    /// ETA, a first run's indeterminate pass declaring disk load, the admin
    /// install's comet, and a held metered row (a carried meter).
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
                    detail0: None,
                    meter: Some((Some(420), "45 MB / 74 MB")),
                    animated: true,
                    level: false,
                    busy: false,
                    eta: true,
                    load: None,
                    load_slot: false,
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
                    detail0: None,
                    meter: Some((Some(420), "3 of 10 programs")),
                    animated: true,
                    level: false,
                    busy: false,
                    eta: true,
                    load: Some(Load::Disk),
                    load_slot: true,
                    capsules: caps(&[]),
                },
            ),
            // The same pass between two heavy phases: its words are down,
            // its slot is not — nothing on the row moves.
            (
                "first-run-lull",
                RowSpec {
                    kind: RowKind::Message(id(12)),
                    severity: Severity::Info,
                    live: true,
                    glyph: '\u{21e3}',
                    title: "Installing ALab tools",
                    detail0: None,
                    meter: Some((Some(420), "3 of 10 programs")),
                    animated: true,
                    level: false,
                    busy: false,
                    eta: true,
                    load: None,
                    load_slot: true,
                    capsules: caps(&[]),
                },
            ),
            // The longest action excerpt the band was measured with — the
            // retired admin install's row (deleted 2026-09-24), kept as a
            // fixture — beside a load slot.
            (
                "action-excerpt",
                RowSpec {
                    kind: RowKind::Message(id(9)),
                    severity: Severity::Info,
                    live: true,
                    glyph: '\u{21e3}',
                    title: "Installing Command Line Tools and Homebrew",
                    detail0: Some("enter your password in the macOS dialog"),
                    meter: Some((None, "")),
                    animated: true,
                    level: false,
                    busy: true,
                    eta: false,
                    load: Some(Load::Disk),
                    load_slot: true,
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
                    detail0: Some("keys typed now arrive in a moment"),
                    meter: Some((None, "")),
                    animated: true,
                    level: false,
                    busy: true,
                    eta: false,
                    load: None,
                    load_slot: false,
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
                    detail0: Some("trust \u{2014} extracting 120 MB / 900 MB"),
                    meter: Some((Some(430), "3 of 10")),
                    animated: false,
                    level: false,
                    busy: false,
                    eta: false,
                    load: None,
                    load_slot: false,
                    capsules: caps(&[Intent::OpenSettings {
                        route: "/packages".into(),
                    }]),
                },
            ),
        ]
    }

    /// Invariant 14: the five rows at 60 / 80 / 120 / 160 columns, pinned
    /// string-for-string — the width law's output, checked row by row
    /// against design §2.3's per-row arithmetic (whole / shaped-to-N /
    /// dropped / short capsules / meter width). A chip is ` label `; every
    /// row is exactly `cols` cells; the doc's own literals are schematic
    /// (their spacing is illustrative, and the manual staged detail uses
    /// the macOS `apply it from the Version menu` phrase, longer than the
    /// doc's placeholder), so the cells pinned here are the law's.
    #[test]
    fn the_five_rows_at_60_80_120_160_are_pinned_string_for_string() {
        let pinned: &[(&str, usize, &str)] = &[
            (
                "crash",
                160,
                " ⚠ aterm closed unexpectedly last time · crash log at ~/Library/Logs/aterm/crash-signal-8123-1758470000.log.seen                          Open log   Details ›  ",
            ),
            (
                "crash",
                120,
                " ⚠ aterm closed unexpectedly last time · crash log at …/crash-signal-8123-1758470000.log.seen     Open log   Details ›  ",
            ),
            (
                "crash",
                80,
                " ⚠ aterm closed unexpectedly last time                    Open log   Details ›  ",
            ),
            (
                "crash",
                60,
                " ⚠ aterm closed unexpectedly last time                 Log  ",
            ),
            (
                "file-access",
                160,
                " ℹ File access not confirmed · Full Disk Access may already be enabled                                                     Open Settings   Not now   Details ›  ",
            ),
            (
                "file-access",
                120,
                " ℹ File access not confirmed · Full Disk Access may already be enabled             Open Settings   Not now   Details ›  ",
            ),
            (
                "file-access",
                80,
                " ℹ File access not confirmed               Open Settings   Not now   Details ›  ",
            ),
            (
                "file-access",
                60,
                " ℹ File access not confirmed            Settings   Not now  ",
            ),
            (
                "staged-manual",
                160,
                " ✓ aterm v0.91.0 is ready · build 1234 — verified; auto-apply is off — apply it from the Version menu or Software Update (in place;…   Install now   Details ›  ",
            ),
            (
                "staged-manual",
                120,
                " ✓ aterm v0.91.0 is ready · build 1234 — verified; auto-apply is off — apply it from the…      Install now   Details ›  ",
            ),
            (
                "staged-manual",
                80,
                " ✓ aterm v0.91.0 is ready · build 1234 — verified;…    Install now   Details ›  ",
            ),
            (
                "staged-manual",
                60,
                " ✓ aterm v0.91.0 is ready          Install now   Details ›  ",
            ),
            (
                "staged-auto",
                160,
                " ✓ aterm v0.91.0 is ready · build 1234 — verified; applies by itself at the next quiet moment, and within 15 min regardless — nothing to do; your…   Details ›  ",
            ),
            (
                "staged-auto",
                120,
                " ✓ aterm v0.91.0 is ready · build 1234 — verified; applies by itself at the next quiet moment, and within…   Details ›  ",
            ),
            (
                "staged-auto",
                80,
                " ✓ aterm v0.91.0 is ready · build 1234 — verified; applies by…       Details ›  ",
            ),
            (
                "staged-auto",
                60,
                " ✓ aterm v0.91.0 is ready                        Details ›  ",
            ),
            (
                "config",
                160,
                " ⚠ 3 keybindings in aterm.toml were skipped · skipping \"cmd+shift+k\": unknown action \"foo\"                                         Open aterm.toml   Details ›  ",
            ),
            (
                "config",
                120,
                " ⚠ 3 keybindings in aterm.toml were skipped · skipping \"cmd+shift+k\": unknown action…      Open aterm.toml   Details ›  ",
            ),
            (
                "config",
                80,
                " ⚠ 3 keybindings in aterm.toml were skipped        Open aterm.toml   Details ›  ",
            ),
            (
                "config",
                60,
                " ⚠ 3 keybindings in aterm.toml were skipped           Edit  ",
            ),
            (
                "toolchain",
                160,
                " ⇣ Installing ALab tools · trust · extracting  42%  512 MB / 1.2 GB                                                                       Packages   Details ›  ",
            ),
            (
                "toolchain",
                120,
                " ⇣ Installing ALab tools · trust · extracting  42%  512 MB / 1.2 GB                               Packages   Details ›  ",
            ),
            (
                "toolchain",
                80,
                " ⇣ Installing ALab tools · trust · extracting  42%        Packages   Details ›  ",
            ),
            // A moving row's painted excerpt is an ACTION excerpt (the host
            // paints one only where it changes what the person does, ruling
            // 77 — the real toolchain row paints none): at 60 the capsules'
            // short forms and the title's elision pay for it (review
            // 2026-09-24).
            (
                "toolchain",
                60,
                " ⇣ Installing ALab t… · trust · extracting  42%   Packages  ",
            ),
        ];
        let fixtures = fixtures();
        let mut failures = Vec::new();
        for (name, cols, want) in pinned {
            let spec = &fixtures.iter().find(|(n, _)| n == name).unwrap().1;
            let row = layout_row(spec, *cols, &chars);
            let got = render(&row, *cols);
            assert_eq!(chars(&got), *cols, "{name}@{cols}: {got:?}");
            if &got != want {
                failures.push(format!("(\"{name}\", {cols}, {got:?}),"));
            }
        }
        assert!(failures.is_empty(), "re-pin:\n{}", failures.join("\n"));
    }

    /// Invariant 13: cols 8..=200 over the five fixtures and the live
    /// indicator's — every row exactly `cols` wide, no panic; the sacrifices
    /// land in order (stats, the excerpt shaped then dropped below the
    /// floor, the ETA, then the capsules go short, then the title, then the
    /// activity's words whole with their load words); authored capsules
    /// present at every width; the METER (or a busy row's track) is the
    /// whole row at every width (ruling 136); a moving row's percent or
    /// elapsed slot outlives every extra, and its latched load words live
    /// exactly as long as it.
    #[test]
    fn width_law_degrades_in_order() {
        for (name, spec) in fixtures().into_iter().chain(motion_fixtures()) {
            let authored = spec
                .capsules
                .iter()
                .filter(|c| !c.action.is_details())
                .count();
            // Short mode shows on any capsule whose two forms differ, or on
            // a capsule gone — `Details ›` has no short form and goes
            // (`Packages` is its own short form and cannot tell).
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
                // Nothing the layout places overlaps anything else (the painter
                // never has to choose), except in the degenerate clip.
                let mut spans: Vec<(usize, usize)> = vec![(row.glyph.0, row.glyph.0 + 1)];
                if !row.title.1.is_empty() {
                    spans.push((row.title.0, row.title.0 + chars(&row.title.1)));
                }
                if let Some((c, d)) = &row.detail {
                    spans.push((c - 3, c + chars(d)));
                }
                // THE METER IS THE ROW: present exactly when the spec carries
                // a fill, at every width, spanning every column — and a busy
                // row's track likewise (ruling 136). Neither is a span.
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
                    spans.push((c - 1, c + crate::ELAPSED_W));
                }
                if let Some(c) = row.eta {
                    spans.push((c - 1, c + row.eta_width()));
                }
                if let Some((c, w)) = row.load_slot {
                    spans.push((c - 3, c + w));
                }
                if let (Some((c, w)), Some((slot, sw))) = (row.load, row.load_slot) {
                    assert!(
                        c == slot && chars(w) <= sw,
                        "{name}@{cols}: the words sit in their slot"
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
                // Below the degenerate step (the title still drawn) nothing
                // the layout places overlaps anything else and the capsules
                // keep their clear cells; in the degenerate clip the authored
                // capsules may reach under the glyph (the painter's last write
                // wins), which is the documented best effort under ~24 cols.
                if let Some(first) = row.capsules.first()
                    && first.col > 0
                    && !row.title.1.is_empty()
                {
                    assert!(first.col >= left_end, "{name}@{cols}: overlap: {painted:?}");
                    assert!(
                        first.col >= left_end + BEFORE_CAPSULES,
                        "{name}@{cols}: {painted:?}"
                    );
                    let last = row.capsules.last().unwrap();
                    assert_eq!(
                        last.col + last.width,
                        cols - MARGIN,
                        "{name}@{cols}: right-aligned"
                    );
                }
                // A moving row's activity words outlive every extra: with
                // them gone (the degenerate step) nothing else is left.
                let moving = spec.animated || spec.busy;
                let activity = row.pct.is_some() || row.elapsed.is_some();
                // A moving row's painted excerpt is an ACTION excerpt: part
                // of the fixed head, paid for by the capsules' short forms
                // and the title's elision, with the load slot an extra after
                // it (review 2026-09-24).
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
                if moving && activity && !action {
                    assert_eq!(
                        row.load.is_some(),
                        spec.load.is_some(),
                        "{name}@{cols}: the load words are the activity's: {painted:?}"
                    );
                    assert_eq!(
                        row.load_slot.is_some(),
                        spec.load.is_some() || spec.load_slot,
                        "{name}@{cols}: the load slot is the activity's: {painted:?}"
                    );
                }
                // Sacrifice order, read as monotone flags while narrowing.
                if let Some(p) = &prev {
                    let short_now = short_of(&row);
                    let short_prev = short_of(p);
                    assert!(
                        !(short_prev && !short_now) || row.capsules.len() != p.capsules.len(),
                        "{name}@{cols}: capsules never go long again as the row narrows"
                    );
                    // (The degenerate step below ~24 cols, which empties the
                    // title, drops `Details ›` and so reads as short too.)
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
                    // Every extra goes before the capsules' long forms do (the
                    // meter is not an extra: it is the row's surface) — an
                    // action excerpt is not one either: it is paid for there.
                    if short_now {
                        assert!(
                            (row.detail.is_none() || action)
                                && row.stats.is_none()
                                && row.eta.is_none()
                                && (row.load_slot.is_none() || !action),
                            "{name}@{cols}: an extra beside short capsules: {painted:?}"
                        );
                    }
                    // The ETA's long form goes before its short one, and
                    // never comes back while narrowing.
                    assert!(
                        !(p.eta_short && row.eta.is_some() && !row.eta_short),
                        "{name}@{cols}: the ETA went long again while narrowing"
                    );
                    // A short ETA starves what comes after it, as short
                    // capsules do.
                    if row.eta_short {
                        assert!(
                            (row.detail.is_none() || action) && row.stats.is_none(),
                            "{name}@{cols}: an extra beside a short ETA: {painted:?}"
                        );
                    }
                    // The extras go in order: the ETA outlives the excerpt
                    // and the stats.
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
                // The floor governs the ROOM: an excerpt is shaped only when
                // `3 + DETAIL_FLOOR` cells were free for it (a word-boundary
                // cut may then land a few cells under the floor — "Full Disk
                // Access…" is whole words, not a stub); below that it is
                // dropped. Non-metered rows expose the room exactly.
                if let Some((dcol, d)) = &row.detail
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
                    // Prose ends on the ellipsis; a path atom carries it in
                    // the middle (`…/aterm/crash.log` keeps the file name).
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
    }

    /// Invariant 17: every capsule column range maps back to its
    /// `ActionIndex`; the gaps map to `Body`; the overflow row is one link.
    #[test]
    fn hit_test_agrees_with_layout() {
        let fixtures = fixtures();
        for cols in [40usize, 60, 80, 120, 160] {
            let mut rows: Vec<RowLayout> = fixtures
                .iter()
                .map(|(_, s)| layout_row(s, cols, &chars))
                .collect();
            rows.push(layout_row(&overflow_spec(3, Links::Painted), cols, &chars));
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

    /// Invariant 18 (FL-1): `0` with no rows; stats fold into the key; the
    /// fill is quantized to whole percent.
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
        assert_ne!(base, at(420, "513 MB / 1.2 GB"), "stats are folded");
        let other_cols = Presentation {
            cols: 120,
            rows: vec![layout_row(&spec, 120, &chars)],
        }
        .fingerprint();
        assert_ne!(base, other_cols);
        // BUSY flips the key (the static shape: the flag and the track); the
        // motion layer folds what moves in its own term.
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

    /// A BUSY ROW IS A WHOLE-ROW TRACK (ruling 75, kept by rulings 136 and
    /// 139). A row with work in flight and no fill carries `track = (0,
    /// cols)` at every width — the same surface a meter is, under every
    /// piece, for the comet — and never a meter or a pct. It costs the words
    /// nothing but its elapsed slot: at every width the busy row's title and
    /// stats are the still row's with `1 + ELAPSED_W` cells less room. Its
    /// excerpt is an ACTION excerpt (a moving row's painted excerpt, which
    /// outranks the capsules' long forms and the title's length — review
    /// 2026-09-24), so the track never takes from it: it is at least the
    /// still row's there. A fill wins (a row with one is never busy), and a
    /// row that is not busy has no track.
    #[test]
    fn a_busy_row_is_a_whole_row_track_that_costs_the_words_nothing() {
        let flow = RowSpec {
            kind: RowKind::Message(id(9)),
            severity: Severity::Info,
            live: true,
            glyph: '\u{21bb}',
            title: "Installing aterm v0.92.0",
            detail0: Some("installs within a minute \u{2014} keep working"),
            meter: Some((None, "")),
            busy: true,
            animated: false,
            level: false,
            eta: false,
            load: None,
            load_slot: false,
            capsules: caps(&[]),
        };
        let with_stats = RowSpec {
            detail0: Some("downloading"),
            meter: Some((None, "45 MB")),
            ..flow.clone()
        };
        let bare = RowSpec {
            detail0: None,
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
                assert!(row.busy, "{cols}: the glyph cell spins wherever the row is");
                assert!(row.meter.is_none() && row.pct.is_none(), "{cols}: no fill");
                assert_eq!(row.track, Some((0, cols)), "{cols}: the track is the row");
                let plain = layout_row(&still, cols, &chars);
                assert!(!plain.busy && plain.track.is_none(), "{cols}: not busy");
                // The words pay for the elapsed slot and nothing else: the
                // busy row's words are the still row's at `1 + ELAPSED_W`
                // fewer columns, outside the degenerate clip.
                if row.elapsed.is_some() && cols > 1 + ELAPSED_W {
                    let narrower = layout_row(&still, cols - 1 - ELAPSED_W, &chars);
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
        // A fill wins: a row with one is never busy and draws the meter.
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
    /// combining marks zero.
    fn wide(s: &str) -> usize {
        s.chars()
            .map(|c| match c {
                '\u{300}'..='\u{36f}' => 0,
                '\u{6f22}' => 2,
                _ => 1,
            })
            .sum()
    }

    /// REVIEW (2026-09-22): `shape_detail` cuts by CHARS while the law
    /// measures CELLS with the injected width. With a wide-char tail the
    /// char cut used to keep a wide word that did not fit, so the excerpt
    /// was DROPPED at 125..=96 cols and CAME BACK at 95..=50 — a flicker
    /// during a resize under the host's grapheme measure. `shape_to_cells`
    /// lowers the char cap until the cells fit, to the largest cap that
    /// does, so the excerpt is monotone.
    #[test]
    fn a_wide_excerpt_does_not_flicker_while_narrowing() {
        let detail = format!("{}{}", "ok ".repeat(12), "\u{6f22}".repeat(30));
        let spec = RowSpec {
            kind: RowKind::Message(id(1)),
            severity: Severity::Info,
            live: false,
            glyph: '\u{2139}',
            title: "Installing",
            detail0: Some(&detail),
            meter: None,
            animated: false,
            level: false,
            busy: false,
            eta: false,
            load: None,
            load_slot: false,
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
        // The shaped excerpt fits its cells and is the longest cut that does:
        // one more char would not fit.
        let row = layout_row(&spec, 100, &wide);
        let (col, shown) = row.detail.as_ref().unwrap();
        let first = row.capsules.first().unwrap().col;
        assert!(col + wide(shown) + BEFORE_CAPSULES <= first, "{shown:?}");
        assert!(
            shown.ends_with('\u{2026}') && shown.starts_with("ok ok"),
            "{shown:?}"
        );
    }

    /// THE METER IS THE ROW (ruling 55, kept by ruling 136). It used to be an
    /// 8–20 cell pill after the excerpt, bought from the row's room (a
    /// review of 2026-09-22 caught it coming back at the long→short capsule
    /// transition, and the live pill was a fixed part of the head). It now
    /// spans every column at every width, from 120 cols down through the
    /// short capsules and the degenerate clip, held or live, costs the words
    /// nothing — the excerpt a held metered row shows at a width is the one
    /// the same row without a fill shows five columns narrower (the ` NN%`),
    /// and a live one's, an ACTION excerpt that outranks the capsules' long
    /// forms and the title's length (review 2026-09-24), is at least that —
    /// and carries the fill exactly.
    #[test]
    fn the_meter_is_the_whole_row_at_every_width_and_costs_the_words_nothing() {
        for animated in [false, true] {
            let spec = |fill: Option<u16>| RowSpec {
                kind: RowKind::Message(id(1)),
                severity: Severity::Info,
                live: animated,
                glyph: '\u{21e3}',
                title: "Installing ALab tools",
                detail0: Some("trust \u{00b7} extracting 120 MB of 900 MB"),
                meter: Some((fill, "")),
                busy: false,
                animated,
                level: false,
                eta: false,
                load: None,
                load_slot: false,
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

    /// The live indicator at 60 / 80 / 120 / 160 columns: the percent or the
    /// elapsed slot, the ETA slot, the load words, the stats, in that order,
    /// pinned string for string over the whole-row meter (the time slots are
    /// the motion layer's and render blank here; the meter is asserted field
    /// by field in the width-law test).
    #[test]
    fn the_live_indicator_rows_are_pinned_string_for_string() {
        let pinned: &[(&str, usize, &str)] = &[
            (
                "download",
                60,
                " ↻ Downloading aterm v0.92.0  42%                Details ›  ",
            ),
            (
                "download",
                80,
                " ↻ Downloading aterm v0.92.0  42%               45 MB / 74 MB        Details ›  ",
            ),
            (
                "download",
                120,
                " ↻ Downloading aterm v0.92.0  42%               45 MB / 74 MB                                                Details ›  ",
            ),
            (
                "download",
                160,
                " ↻ Downloading aterm v0.92.0  42%               45 MB / 74 MB                                                                                        Details ›  ",
            ),
            (
                "first-run",
                60,
                " ⇣ Installing ALab tools  42% · disk busy        Details ›  ",
            ),
            (
                "first-run",
                80,
                " ⇣ Installing ALab tools  42%              · disk busy               Details ›  ",
            ),
            (
                "first-run",
                120,
                " ⇣ Installing ALab tools  42%              · disk busy     3 of 10 programs                                  Details ›  ",
            ),
            (
                "first-run",
                160,
                " ⇣ Installing ALab tools  42%              · disk busy     3 of 10 programs                                                                          Details ›  ",
            ),
            (
                "first-run-lull",
                60,
                " ⇣ Installing ALab tools  42%                    Details ›  ",
            ),
            (
                "first-run-lull",
                80,
                " ⇣ Installing ALab tools  42%                                        Details ›  ",
            ),
            (
                "first-run-lull",
                120,
                " ⇣ Installing ALab tools  42%                              3 of 10 programs                                  Details ›  ",
            ),
            (
                "first-run-lull",
                160,
                " ⇣ Installing ALab tools  42%                              3 of 10 programs                                                                          Details ›  ",
            ),
            // An ACTION excerpt (M13, ruling 148) outranks the load slot and
            // `Details ›`, and the title elides for it — at 80 it used to be
            // the one word the row did not paint (review 2026-09-24).
            (
                "action-excerpt",
                60,
                " ⇣ Installing Command Line T… · enter your password…        ",
            ),
            (
                "action-excerpt",
                80,
                " ⇣ Installing Command Line Tools and Homebrew · enter your password…            ",
            ),
            (
                "action-excerpt",
                120,
                " ⇣ Installing Command Line Tools and Homebrew · enter your password in the macOS dialog                      Details ›  ",
            ),
            (
                "action-excerpt",
                160,
                " ⇣ Installing Command Line Tools and Homebrew · enter your password in the macOS dialog        · disk busy                                           Details ›  ",
            ),
            (
                "busy-excerpt",
                60,
                " ↑ Finishing aterm v0.92.0 · keys typed now…                ",
            ),
            (
                "busy-excerpt",
                80,
                " ↑ Finishing aterm v0.92.0 · keys typed now arrive in a…             Details ›  ",
            ),
            (
                "busy-excerpt",
                120,
                " ↑ Finishing aterm v0.92.0 · keys typed now arrive in a moment                                               Details ›  ",
            ),
            (
                "busy-excerpt",
                160,
                " ↑ Finishing aterm v0.92.0 · keys typed now arrive in a moment                                                                                       Details ›  ",
            ),
            (
                "held-meter",
                60,
                " ⚠ ALab tools  43%                    Packages   Details ›  ",
            ),
            (
                "held-meter",
                80,
                " ⚠ ALab tools · trust — extracting 120 MB / 900 MB  43%   Packages   Details ›  ",
            ),
            (
                "held-meter",
                120,
                " ⚠ ALab tools · trust — extracting 120 MB / 900 MB  43%  3 of 10                                  Packages   Details ›  ",
            ),
            (
                "held-meter",
                160,
                " ⚠ ALab tools · trust — extracting 120 MB / 900 MB  43%  3 of 10                                                                          Packages   Details ›  ",
            ),
        ];
        let fixtures = motion_fixtures();
        let mut failures = Vec::new();
        for name in [
            "download",
            "first-run",
            "first-run-lull",
            "action-excerpt",
            "busy-excerpt",
            "held-meter",
        ] {
            for cols in [60usize, 80, 120, 160] {
                let spec = &fixtures.iter().find(|(n, _)| *n == name).unwrap().1;
                let row = layout_row(spec, cols, &chars);
                let got = render(&row, cols);
                assert_eq!(chars(&got), cols);
                match pinned.iter().find(|(n, c, _)| *n == name && *c == cols) {
                    Some((_, _, want)) if *want == got => {}
                    _ => failures.push(format!("(\"{name}\", {cols}, {got:?}),")),
                }
            }
        }
        assert!(failures.is_empty(), "re-pin:\n{}", failures.join("\n"));
    }

    /// THE ETA KEEPS ITS SLOT WHERE THE LONG FORM DOES NOT FIT (review
    /// 2026-09-24): the remaining time says `left` (`~8 min left`), which
    /// cost five cells, and at 80 columns a first run's row under disk load
    /// had room for the old seven and not for twelve. There the slot takes
    /// its SHORT form (`8m left`, [`ETA_SHORT_W`]); wider it is long; the
    /// short form starves the excerpt and the stats after it.
    #[test]
    fn the_eta_slot_goes_short_before_it_goes() {
        let fixtures = motion_fixtures();
        let spec = &fixtures.iter().find(|(n, _)| *n == "first-run").unwrap().1;
        let at = |cols: usize| layout_row(spec, cols, &chars);
        let wide = at(120);
        assert!(wide.eta.is_some() && !wide.eta_short, "long at 120");
        assert_eq!(wide.eta_width(), ETA_W);
        // Narrowing: long, then short (beside the load words, starving the
        // excerpt and the stats), then gone.
        let short: Vec<usize> = (20..=120).filter(|c| at(*c).eta_short).collect();
        assert!(
            !short.is_empty(),
            "the short form is laid out at some width"
        );
        for cols in short {
            let narrow = at(cols);
            assert_eq!(narrow.eta_width(), ETA_SHORT_W, "@{cols}");
            assert!(narrow.detail.is_none() && narrow.stats.is_none(), "@{cols}");
            assert!(narrow.load.is_some(), "@{cols}: …beside the load words");
        }
        assert_eq!(at(40).eta, None, "and gone at 40");
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
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The center: every unretired message, the stable-ordered glass, the row
//! hysteresis, the settle law and the log. One instance per process; every
//! method that reads time takes `now`.
//!
//! # Attention and motion
//!
//! * **The reveal.** A row posted with `reveal_after(d)` is not eligible for
//!   the glass until `d` after its post: every read of the eligible count goes
//!   through [`MessageCenter::eligible`], so an invisible row never moves the
//!   overflow row, the `+N` or the row count, and a row resolved inside its
//!   grace never re-grids at all.
//! * **The echo.** A moving row on the glass that retires leaves a completion
//!   echo in its slot for its span ([`crate::animate::echo_span`]): the end of
//!   its indicator's animation, not a row. It never counts in
//!   [`MessageCenter::wanted_rows`], so it costs no re-grid; a quiet shrink
//!   waits out [`crate::SHRINK_QUIET`] and every echo still playing.
//! * **The motion.** [`MessageCenter::motion`] reads the frame at an injected
//!   instant on one grid anchored at the center's birth, and
//!   [`MessageCenter::motion_deadline`] names the next frame the band needs
//!   (`None` when nothing moving or echoing is on the glass). What moves is
//!   the meter's state (a fill draws the bar, `busy` the comet), never the
//!   hold.

use crate::animate::{
    Anim, BandMotion, Look, Pace, ROW, RowMotion, Surface, anim_ms, bar, bar_at, bar_glints, comet,
    comet_phase, echo, echo_span, glide, glide_q16, glide_span, glint_at, next_glint_start,
    stalled_bar, track,
};
use crate::carry::{CarriedMessage, Carry};
use crate::digest::{Digest, DigestView, Entry as DigestItem};
use crate::glass::{
    CapsuleRole, CapsuleSpec, Fnv, Links, Presentation, RowKind, RowLayout, RowSpec, finish_title,
    layout_row, layout_row_with, outranks, overflow_spec, overflow_words, rank,
};
use crate::log::{FinalWords, LogLine, LogRecord, MessageLog, Retired};
use crate::model::{
    ActionIndex, Admission, Glyph, Hold, Intent, Load, Loads, Message, MessageId, Meter,
    Restatement, Severity, Tag, WallStamp,
};
use crate::need::{
    DIGEST_EXPIRY, FILED_KEEP_MS, LaterAsk, Need, RESOLVE_AFTER, RESTATE_QUIET, Resurface,
};
use crate::progress::{
    Eta, ProgressTrack, elapsed_word_change, elapsed_words, eta_words, eta_words_short,
};
use crate::text::clip;
use crate::words::abbreviate_paths_in;
use crate::{
    ANIM_FRAME, DETAIL_LINE_CAP, DETAIL_LINES_CAP, DONE_WORD, Duration, ECHO_DRAIN,
    ECHO_FAULT_CROSS, ECHO_FAULT_FLASH, FAILED_WORD, GLINT_TRAVEL, Instant, LOAD_AFTER,
    MAX_ACTIONS, MAX_LIVE, MAX_ROWS, OVERFLOW_PATIENCE_MIN, REFRESH_DEFAULT, REVEAL_DEFER_MAX,
    REVEAL_MIN_LEFT, REVEAL_RECHECK, SHRINK_QUIET, STALE_HANDOFF, STALLED_WORD, TITLE_CAP,
};

/// How far ahead of the wall clock a filing may be stamped and still count:
/// the wall clock is stepped by NTP and by hand, and a filing stamped beyond
/// this was made under a clock that has since been corrected, so it keeps
/// nothing quiet (a fact returning once is cheaper than one hidden for years).
const FILING_SKEW_MS: u64 = 3_600_000;

/// A Standing row's patience in the queue — a standing condition is not
/// folded unseen for a day.
const STANDING_PATIENCE: Duration = Duration::from_hours(24);
/// The longest span any hold is honoured for (instant arithmetic stays
/// finite whatever a reporter types).
const MAX_SPAN: Duration = Duration::from_hours(365 * 24);

/// How a resolved row ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Fine: the work was delivered (a refusal or a failure is `Warn`).
    Ok,
    /// Not fine (`appstatus` says `outcome=warn`).
    Warn,
}

impl Outcome {
    /// The `appstatus` word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
        }
    }
}

/// How a retiring row's work ended, as its record says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// A live row that delivered: `resolve(Ok)`, or a withdraw that named
    /// the Complete echo.
    Delivered,
    /// A live row that failed or went silent: `resolve(Warn)`, stale, or a
    /// withdraw that named the Fault echo.
    Stopped,
    /// A live row withdrawn with no outcome to claim (a plain withdraw, or
    /// one that named the Vanish echo).
    Ended,
    /// A standing warning or error its reporter resolved Ok, with the words
    /// it declared for the fix: the problem is fixed (a clean reload of
    /// `aterm.toml`).
    Fixed,
    /// Anything else keeps its words and its mark: an ask, a row
    /// superseded, carried or evicted (its work goes on under the next
    /// row), a dismissal, a fold.
    Kept,
}

impl Ending {
    fn of(msg: &Message, how: &Retired, echo: Option<EchoKind>) -> Self {
        let live = matches!(msg.hold, Hold::Live { .. });
        if msg.is_ask() {
            return Self::Kept;
        }
        // Superseded, carried and evicted rows fall to `Kept`: their work
        // goes on under the next row.
        match how {
            Retired::Resolved(Outcome::Ok) if live => Self::Delivered,
            Retired::Withdrawn if live => match echo {
                Some(EchoKind::Complete) => Self::Delivered,
                Some(EchoKind::Fault) => Self::Stopped,
                Some(EchoKind::Vanish) | None => Self::Ended,
            },
            Retired::Resolved(Outcome::Warn) | Retired::Stale if live => Self::Stopped,
            // Fixed only where its reporter said in what words: a title such
            // as `Couldn't apply the font` under a ✓ would contradict itself.
            Retired::Resolved(Outcome::Ok)
                if msg.severity >= Severity::Warn && msg.finished.is_some() =>
            {
                Self::Fixed
            }
            _ => Self::Kept,
        }
    }
}

/// The title a live row or an ask is logged under when it retires, so the log
/// states the outcome. A live row that delivered is logged in its finished
/// words (`Checked ALab tools`, or its reporter's declared words); one that
/// failed or went silent as stopped (`Indexing stopped`); an ask in its
/// declared words (`Asked for Full Disk Access`). A withdrawn row is logged by
/// the `echo` its reporter named (a Complete in finished words, a Fault as
/// stopped), a plain withdraw as ended (`Indexing ended`). A standing warning
/// resolved Ok takes its declared fix words (`Misspelled setting fixed`).
/// `None` keeps the title: a superseded or carried row (its work goes on
/// under the next row), and a title with no such form.
fn outcome_title(msg: &Message, how: &Retired, echo: Option<EchoKind>) -> Option<String> {
    // A title its reporter also declared as its finished words is how the
    // work ended, in the reporter's own words: no table form replaces it.
    let own_end = msg.finished.as_deref() == Some(msg.title.as_str());
    let words = match Ending::of(msg, how, echo) {
        // A title the table has no past tense for says `— done`, as its echo
        // did: never its in-flight words under a ✓.
        Ending::Delivered => msg
            .finished_words()
            .or_else(|| crate::words::done_form(&msg.title)),
        Ending::Stopped | Ending::Ended if own_end => None,
        Ending::Stopped => crate::words::stopped_form(&msg.title),
        Ending::Ended => crate::words::ended_form(&msg.title),
        Ending::Fixed => msg.finished.clone(),
        Ending::Kept
            if msg.is_ask()
                && !matches!(
                    how,
                    Retired::Superseded { .. } | Retired::Carried | Retired::Evicted
                ) =>
        {
            msg.finished.clone()
        }
        Ending::Kept => None,
    };
    words.filter(|w| *w != msg.title)
}

/// The severity and glyph a retiring row's record takes, when they change: the
/// mark says how it ended. Delivered work and a fixed problem read `✓`
/// Success, never the activity glyph it moved under; stopped work reads `⚠`
/// Warn (an Error keeps its `✕`) and so joins Problems; work that ended with
/// no outcome keeps its severity under that severity's own icon. `None` keeps
/// the posted mark (an ask, a fold, a supersede, a dismissal).
fn outcome_mark(msg: &Message, how: &Retired, echo: Option<EchoKind>) -> Option<(Severity, Glyph)> {
    let own = |severity: Severity| (severity, Glyph::or_fallback(severity_icon(severity)));
    let mark = match Ending::of(msg, how, echo) {
        Ending::Delivered | Ending::Fixed => own(Severity::Success),
        Ending::Stopped => own(msg.severity.max(Severity::Warn)),
        Ending::Ended => own(msg.severity),
        Ending::Kept => return None,
    };
    (mark != (msg.severity, msg.glyph)).then_some(mark)
}

/// A severity's own icon: `ℹ` Info, `✓` Success, `⚠` Warn, `✕` Error.
const fn severity_icon(severity: Severity) -> char {
    match severity {
        Severity::Info => '\u{2139}',
        Severity::Success => '\u{2713}',
        Severity::Warn => '\u{26a0}',
        Severity::Error => '\u{2715}',
    }
}

/// How a live row's indicator ends on the glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EchoKind {
    /// The work was delivered: the fill wipes to 100 %, glows, fades under
    /// a ✓.
    Complete,
    /// It failed or was refused: a warn flash, then a fade.
    Fault,
    /// It left with no outcome to claim: a fade.
    Vanish,
}

impl EchoKind {
    /// How long the echo holds its slot from a bar at `from_permille` (a busy
    /// row's from 0): [`crate::animate::echo_span`]; a Complete's glide scales
    /// with the distance it wipes.
    #[must_use]
    pub fn span(self, from_permille: u16) -> Duration {
        echo_span(self, from_permille, false)
    }

    /// The echo a retirement leaves by default.
    #[must_use]
    pub(crate) fn for_retired(how: &Retired) -> Option<Self> {
        match how {
            Retired::Resolved(Outcome::Ok) => Some(Self::Complete),
            Retired::Resolved(Outcome::Warn) => Some(Self::Fault),
            Retired::Withdrawn | Retired::Stale | Retired::Folded => Some(Self::Vanish),
            Retired::Dismissed
            | Retired::Superseded { .. }
            | Retired::Answered { .. }
            | Retired::Evicted
            | Retired::Unseen
            | Retired::Recorded
            | Retired::Carried
            | Retired::Quit => None,
        }
    }
}

/// A retired live row's completion echo, holding the glass slot the row had.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Echo {
    /// The retired row's id.
    pub id: MessageId,
    /// Its last words, laid out as the row was so nothing reflows.
    pub msg: Message,
    /// Which flourish.
    pub kind: EchoKind,
    /// The fill shown at retirement (0 for a busy row).
    pub from_permille: u16,
    /// The row was busy (the comet): no fraction to wipe from.
    pub indeterminate: bool,
    /// A busy row's comet, `since` into its motion when the row retired
    /// (`None`: a bar, or a comet that never moved). A Fault or Vanish echo
    /// runs the comet on from there, so its first frame is the next live one.
    pub comet_since: Option<Duration>,
    /// How long the work ran (the frozen readout).
    pub elapsed: Duration,
    /// When the echo began.
    pub started: Instant,
    /// When it ends.
    pub until: Instant,
    /// The visual slot it holds.
    pub slot: u16,
    /// The load words the row showed. The echo draws none (the work ended); it
    /// keeps the slot they sat in ([`Self::slot`]), so the row does not reflow.
    pub load: Option<Load>,
    /// The loads the row reserved its slot for (kept so the row does not
    /// reflow); empty when it reserved none.
    pub load_slot: Loads,
}

impl Echo {
    /// The load slot the row laid out, for the echo to keep: the declared
    /// loads with the one it showed, or every load's width for a row that
    /// showed words it never declared ([`crate::glass::RowSpec::load_slot`]).
    #[must_use]
    pub fn slot(&self) -> Loads {
        match self.load {
            Some(l) if !self.load_slot.is_empty() => self.load_slot.with(l),
            Some(_) => Loads::ALL,
            None => self.load_slot,
        }
    }
}

/// One unretired message with its timers.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "five independent facts about one row (read, revealed, load words up, load slot reserved, carried), each set on its own clock; a state enum would multiply them"
)]
pub struct Live {
    /// The id.
    pub id: MessageId,
    /// The words (restated in place).
    pub msg: Message,
    /// The wall clock at ingress.
    pub stamp: WallStamp,
    /// When it was posted.
    pub posted_at: Instant,
    /// When the work began: the post, carried across a supersede by key while
    /// the activity is the same (as the motion epoch is). The band's elapsed
    /// words, an echo's and the screen reader's description all read it, so a
    /// re-worded row of the same work never restarts it.
    pub started_at: Instant,
    /// Bumped by every restate.
    pub revision: u32,
    /// The current action list's version. Exhaustion refuses versioned actions
    /// for this live row instead of reusing an earlier authorization.
    action_revision: Option<u64>,
    /// Duplicate posts folded in.
    pub repeats: u32,
    /// `None` while queued; the hold anchors when this is set.
    pub on_glass_since: Option<Instant>,
    /// Default/For/Ask rows: `None` until first glass.
    pub fold_at: Option<Instant>,
    /// Live rows: `posted_at` (or the last restate) + `stale_after`.
    pub stale_at: Option<Instant>,
    /// `reveal_at` (or the post) + `max(hold, OVERFLOW_PATIENCE_MIN)`: a row
    /// still queued past this folds Unseen. A demote re-bases it on the
    /// instant the row left the glass ([`Self::patience_from`]).
    pub patience_at: Instant,
    /// Where the patience clock runs from: the reveal (or the post), then the
    /// last demote; a restated hold changes the span, never this anchor.
    patience_from: Instant,
    /// Details was opened.
    pub seen: bool,
    /// The last capsule pressed, and when.
    pub acted: Option<(ActionIndex, Instant)>,
    /// Eligible for the glass: false inside the progress grace.
    pub revealed: bool,
    /// When an unrevealed row becomes eligible.
    pub reveal_at: Option<Instant>,
    /// The latest a held-back reveal may wait to: its grace's end plus
    /// [`REVEAL_DEFER_MAX`].
    reveal_cap: Option<Instant>,
    /// The motion epoch: set when the row reaches the glass, reset when its
    /// activity changes (none ↔ a fill ↔ busy).
    pub motion_since: Option<Instant>,
    /// The ETA estimator, fed while the meter carries an amount.
    pub track: ProgressTrack,
    /// The declared heavy load, and when it began.
    pub load: Option<(Load, Instant)>,
    /// The load words are up: at once for a load the row was posted with (such
    /// a row is on the glass because it is heavy), and [`LOAD_AFTER`] after it
    /// began for a load a later restate declared; flipped by `settle`, so the
    /// layout stays time-free.
    pub load_shown: bool,
    /// The loads the row's slot is reserved for: empty until it declares a
    /// load, then its reporter's declared kinds ([`Message::loads`], every load
    /// when undeclared) plus each load it shows. Its layout reserves the slot at
    /// the widest of their words from then on, so words arriving, changing or
    /// leaving never re-grid a moving row.
    pub load_slot: Loads,
    /// The fill the bar glides from, and when the glide began.
    pub glide: Option<(u16, Instant)>,
    /// Re-seeded from the carry and not yet past the handoff commit.
    carried_pending: bool,
    /// The mark (severity, glyph) the record's Posted line carries. A row
    /// restated to another mark retires under the one it shows, written on its
    /// Retired line; the ring's record reads the shown mark while the row
    /// lives, so this is what the file says. A seeded row takes it from the
    /// boot load's Posted line, and Commit's reload reads it again from the
    /// file.
    logged_mark: (Severity, Glyph),
    /// The wall clock at `posted_at`, the anchor [`Live::wall_at`] adds the
    /// monotonic life to: the stamp for a post, the host's wall at the seed for
    /// a carried row (whose stamp is the parent's ingress).
    wall_anchor: u64,
}

/// `now + d`, finite whatever `d` is.
fn at(now: Instant, d: Duration) -> Instant {
    now.checked_add(d.min(MAX_SPAN)).unwrap_or(now)
}

impl Live {
    fn new(id: MessageId, msg: Message, stamp: WallStamp, now: Instant) -> Self {
        let span = hold_span(msg.hold, msg.severity);
        let stale_at = match msg.hold {
            Hold::Live { stale_after } => Some(at(now, stale_after)),
            _ => None,
        };
        let reveal_at = msg.reveal_after.map(|d| at(now, d));
        let mut track = ProgressTrack::default();
        if let Some(a) = msg.meter.as_ref().and_then(|m| m.amount) {
            track.observe(now, a);
        }
        let load = msg.meter.as_ref().and_then(|m| m.load).map(|l| (l, now));
        Self {
            id,
            stamp,
            posted_at: now,
            started_at: now,
            revision: 0,
            action_revision: Some(0),
            repeats: 1,
            on_glass_since: None,
            fold_at: None,
            stale_at,
            patience_at: at(reveal_at.unwrap_or(now), span.max(OVERFLOW_PATIENCE_MIN)),
            patience_from: reveal_at.unwrap_or(now),
            seen: false,
            acted: None,
            revealed: reveal_at.is_none(),
            reveal_at,
            reveal_cap: reveal_at.map(|t| at(t, REVEAL_DEFER_MAX)),
            motion_since: None,
            track,
            load,
            load_shown: load.is_some(),
            load_slot: load.map_or(Loads::NONE, |(l, _)| msg.loads.reserved_with(l)),
            glide: None,
            carried_pending: false,
            wall_anchor: stamp.unix_ms,
            logged_mark: (msg.severity, msg.glyph),
            msg,
        }
    }

    /// The action-list revision to capture with this row's id before queuing
    /// an action. `None` after revision exhaustion; plain presentation updates
    /// keep it, but even changing the actions back retires the previous version.
    #[must_use]
    pub const fn action_revision(&self) -> Option<u64> {
        self.action_revision
    }

    /// `true` while not on glass.
    #[must_use]
    pub fn is_queued(&self) -> bool {
        self.on_glass_since.is_none()
    }

    /// `true` for work in flight whose indicator moves: a busy meter (the
    /// comet), or a fill on a [`Hold::Live`] row (the glide and the glint). A
    /// Live row with neither (blocked on the person) is still, and so is a
    /// held row's fill.
    #[must_use]
    pub(crate) fn is_animated(&self) -> bool {
        match &self.msg.meter {
            Some(m) if m.busy => true,
            Some(m) => m.fill_permille.is_some() && matches!(self.msg.hold, Hold::Live { .. }),
            None => false,
        }
    }

    /// Whether the row is busy ([`Meter::busy`]).
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.msg.meter.as_ref().is_some_and(|m| m.busy)
    }

    /// Live progress: work in flight with its animated indicator (a moving
    /// determinate fill or a busy comet). With two rows or more the glass
    /// keeps one row for the best of these
    /// ([`MessageCenter::reserved_progress`]).
    #[must_use]
    pub(crate) fn is_progress(&self) -> bool {
        self.is_animated()
    }

    /// The activity byte the fingerprint folds and the motion epoch keys
    /// on: 0 no indicator, 1 a fill, 2 busy.
    #[must_use]
    pub fn activity(&self) -> u8 {
        match &self.msg.meter {
            Some(m) if m.fill_permille.is_some() => 1,
            Some(m) if m.busy => 2,
            _ => 0,
        }
    }

    /// The fill the bar shows at `now`: the data, or the glide toward it.
    #[must_use]
    pub(crate) fn shown_fill(&self, now: Instant) -> Option<u16> {
        let to = self.msg.meter.as_ref()?.fill_permille?;
        match self.glide {
            Some((from, since)) => {
                let t = now.saturating_duration_since(since);
                Some(if t < glide_span(from, to) {
                    glide(from, to, t)
                } else {
                    to
                })
            }
            None => Some(to),
        }
    }

    /// The fill's edge at `now`, Q16 of the row, unquantized: the data, or the
    /// glide toward it read finer than a permille.
    #[must_use]
    pub fn shown_edge(&self, now: Instant) -> Option<i64> {
        let to = self.msg.meter.as_ref()?.fill_permille?;
        Some(match self.glide {
            Some((from, since)) if now.saturating_duration_since(since) < glide_span(from, to) => {
                glide_q16(from, to, now.saturating_duration_since(since))
            }
            _ => i64::from(to.min(1000)) * i64::from(ROW) / 1000,
        })
    }

    /// When the row's glide lands; `None` with none in flight at `now`.
    #[must_use]
    pub fn glide_end(&self, now: Instant) -> Option<Instant> {
        let to = self.msg.meter.as_ref()?.fill_permille?;
        let (from, since) = self.glide?;
        let end = since + glide_span(from, to);
        (now < end).then_some(end)
    }

    /// The hold's span: the severity's for `Default`, the named one for
    /// `For` / `Live` / `Ask`, a day for `Standing`, zero for `LogOnly`.
    #[must_use]
    pub(crate) fn hold_span(&self) -> Duration {
        hold_span(self.msg.hold, self.msg.severity)
    }

    /// The wall clock now: the wall at `posted_at` (the ingress stamp, or
    /// the seed's wall for a carried row) plus the monotonic elapsed — the
    /// engine reads no clock of its own.
    #[must_use]
    pub(crate) fn wall_at(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.posted_at).as_millis();
        self.wall_anchor
            .saturating_add(u64::try_from(elapsed).unwrap_or(u64::MAX))
    }

    /// Anchor the hold: the row reached (or re-reached) the glass now. A
    /// carried row keeps the handoff cap instead until the commit
    /// ([`MessageCenter::after_handoff_commit`] re-arms it).
    fn arm(&mut self, now: Instant) {
        if self.carried_pending {
            return;
        }
        match self.msg.hold {
            Hold::Default | Hold::For(_) | Hold::Ask { .. } => {
                self.fold_at = Some(at(now, self.hold_span()));
            }
            Hold::Live { stale_after } => self.stale_at = Some(at(now, stale_after)),
            Hold::Standing(_) | Hold::LogOnly => {}
        }
    }

    /// A duplicate of `msg`: the same owner class
    /// ([`crate::model::Origin::wire_owned`]: wire and host posts never fold
    /// into each other), then the same key, words, actions, severity and hold
    /// kind (a change of severity, or between work in flight and a held row,
    /// supersedes), or no key on either side and the same tag and words.
    pub(crate) fn duplicates(&self, msg: &Message) -> bool {
        let owner = |m: &Message| m.origin.wire_owned(m.key.as_deref());
        if owner(&self.msg) != owner(msg) {
            return false;
        }
        let same_words = self.msg.title == msg.title && self.msg.detail == msg.detail;
        let in_flight = |h: Hold| matches!(h, Hold::Live { .. });
        match (&self.msg.key, &msg.key) {
            (Some(a), Some(b)) => {
                a == b
                    && same_words
                    && self.msg.actions == msg.actions
                    && self.msg.severity == msg.severity
                    && in_flight(self.msg.hold) == in_flight(msg.hold)
            }
            (None, None) => self.msg.tag == msg.tag && same_words,
            _ => false,
        }
    }

    /// The load words the row shows now, if any.
    pub(crate) fn shown_load(&self) -> Option<Load> {
        self.load.filter(|_| self.load_shown).map(|(l, _)| l)
    }

    /// The load words the row says on the glass: its shown load, unless that
    /// is the work's own primary load. The band's layout and the wire's
    /// `load=` both read this.
    pub(crate) fn said_load(&self) -> Option<Load> {
        self.shown_load().filter(|l| self.msg.primary != Some(*l))
    }
}

/// The hold a restatement may give a row of class `need` that holds `current`
/// now, and whether the request had to be cut back. A persistent hold is only
/// ever kept on a classified row (a restatement never grants persistence); a
/// `For` is held to the class's span — a decision may be asked for up to ten
/// minutes, a notice for its kind's span, everything else, an unclassified row
/// included, for two minutes (the door's cut, [`Message::admit`]).
fn admit_restated_hold(need: &Need, current: Hold, asked: Hold) -> (Option<Hold>, bool) {
    match asked {
        Hold::Standing(_) | Hold::Ask { .. } => match need {
            // A standing ask restating itself keeps what it has.
            Need::Now(_) => (Some(current).filter(|h| h.persists()), false),
            _ => (None, true),
        },
        Hold::For(d) => {
            let cap = match need {
                Need::Now(_) => crate::need::Span::Ten.duration(),
                Need::Notice(k) => k.span(),
                _ => crate::need::MAX_TRANSIENT,
            };
            if d > cap {
                (Some(Hold::For(cap)), true)
            } else {
                (Some(asked), false)
            }
        }
        other => (Some(other), false),
    }
}

fn hold_span(hold: Hold, severity: Severity) -> Duration {
    match hold {
        Hold::Default => severity.default_hold(),
        Hold::For(d) | Hold::Ask { for_: d, .. } | Hold::Live { stale_after: d } => d.min(MAX_SPAN),
        Hold::Standing(_) => STANDING_PATIENCE,
        Hold::LogOnly => Duration::ZERO,
    }
}

/// What a post did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Posted {
    /// The row's id (the existing one for a duplicate).
    pub id: MessageId,
    /// New, superseded, or a duplicate.
    pub outcome: PostOutcome,
}

/// How a post landed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostOutcome {
    /// A new row.
    New,
    /// It replaced this live row by key.
    Superseded(MessageId),
    /// The same words were already live: `repeats` bumped, hold re-anchored.
    Duplicate,
    /// An exception's words changed in place: same entry, same place in the
    /// order, no log line.
    Restated,
    /// A dismissed exception raised again inside its quiet span: nothing
    /// shown, nothing logged. The id is the record that made it quiet.
    Suppressed,
}

/// What one family pass ([`MessageCenter::sync_digest`]) did.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DigestSync {
    /// Entries that were not open and now are.
    pub posted: usize,
    /// Their keys, in the order the pass named them (the host writes each one's
    /// `aterm.log` line once).
    pub posted_keys: Vec<String>,
    /// Entries whose words changed in place.
    pub restated: usize,
    /// Facts the owner filed that the pass named again inside their span.
    pub suppressed: usize,
    /// Entries that had gone unnamed for the whole clock
    /// ([`crate::need::RESOLVE_AFTER`]) and ended at this look.
    pub resolved: usize,
    /// Entries the pass did not name this look and whose clock is running.
    pub pending_end: usize,
}

/// What [`MessageCenter::catch_up`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CatchUp {
    /// Entries the parent filed while the two overlapped, ended here.
    pub closed: usize,
    /// Entries the parent opened meanwhile, adopted here.
    pub adopted: usize,
}

/// What a settle did.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Settled {
    /// Something painted changed: the visible id list, a visible row's
    /// revision or load words, an echo, the overflow row's count, or the
    /// `+N` a single row's Details capsule carries.
    pub glass_changed: bool,
    /// Rows that retired, in order.
    pub retired: Vec<(MessageId, Retired)>,
}

/// One visual row of the band, before the overflow row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visual {
    Live(MessageId),
    Echo(usize),
}

/// What [`Settled::glass_changed`] compares. Both row lists are bounded by
/// the committed band height; keeping these small snapshots inline avoids
/// allocating before and after every host settle.
type GlassSignature = (
    [Option<(MessageId, u32, bool)>; MAX_ROWS as usize],
    [Option<(u16, MessageId, EchoKind)>; MAX_ROWS as usize],
    Option<usize>,
    usize,
    Option<String>,
);

/// The center.
#[derive(Clone, Debug)]
pub struct MessageCenter {
    live: Vec<Live>,
    glass: Vec<MessageId>,
    echoes: Vec<Echo>,
    log: MessageLog,
    committed_rows: u16,
    shrink_since: Option<Instant>,
    /// The windows' chrome regrow floor the host handed at its last commit
    /// ([`crate::drive::Commit::floor`], [`crate::chrome`]): the quiet shrink
    /// waits it as well as [`SHRINK_QUIET`], so another chrome source's rise
    /// just before the shrink is never followed by the band's grow-back inside
    /// [`crate::chrome::CHROME_REGROW_SETTLE`]. `None`: no floor.
    regrow_floor: Option<Instant>,
    revision: u64,
    /// Restatements can move a future ETA boundary without changing the
    /// words painted now. Hosts use this to retire cached motion deadlines
    /// even when the Settings/glass revision correctly stays unchanged.
    motion_input_epoch: u64,
    born: Instant,
    /// The display's frame period, which glides and echoes are read on while
    /// they are in flight; [`REFRESH_DEFAULT`] until the host says
    /// ([`Self::set_refresh`]).
    refresh: Duration,
    /// Problems that left the glass unfixed in this process (folded by a press
    /// that took the person to fix it, read, dismissed, run out, or recorded as
    /// a repeat) whose reporter declared fix words: a later fix under the key
    /// still closes their records as fixed ([`Self::fix_closed`]). Bounded by
    /// [`UNFIXED_CAP`], oldest out.
    unfixed: Vec<Unfixed>,
    /// The exceptions ([`Need::Later`]): a store beside the live set, never in
    /// it, so nothing that lays out the band can seat one.
    digest: Digest,
    /// Messages the door demoted to a record ([`Message::admit`]): a persistent
    /// hold with no ask behind it, or a transient one held past its span.
    demotions: u32,
    /// The key (else the title) of the message the door demoted last, so the
    /// host's one warning names the builder to fix.
    last_demoted: Option<String>,
}

/// A closed record of a problem nobody has fixed yet ([`MessageCenter::unfixed`]).
#[derive(Clone, Debug)]
struct Unfixed {
    id: MessageId,
    key: String,
    fixed: String,
}

/// How many unfixed records the center remembers: far more than the
/// families any reporter keys (`config.` has ten).
const UNFIXED_CAP: usize = 64;

/// What a post of an exception does ([`MessageCenter::later_verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaterVerdict {
    /// The owner filed the fact inside its quiet span: this record keeps it quiet.
    Quiet(MessageId),
    /// The entry at this index already says it (or only its moving words changed,
    /// inside [`RESTATE_QUIET`]): a repeat.
    Repeat(usize),
    /// The entry at this index takes the new words in place.
    Restate(usize),
    /// A new entry.
    New,
}

impl MessageCenter {
    /// A center over a (possibly replayed) log; ids continue from
    /// `log.next_id()`. `now` anchors the motion frame grid.
    #[must_use]
    pub fn new(log: MessageLog, now: Instant) -> Self {
        Self {
            live: Vec::new(),
            glass: Vec::new(),
            echoes: Vec::new(),
            log,
            committed_rows: 0,
            shrink_since: None,
            regrow_floor: None,
            revision: 0,
            motion_input_epoch: 0,
            born: now,
            refresh: REFRESH_DEFAULT,
            unfixed: Vec::new(),
            digest: Digest::default(),
            demotions: 0,
            last_demoted: None,
        }
    }

    /// The display's frame period: a glide and an echo are read on it while in
    /// flight, nothing else is. Clamped to 240 Hz … the grid; a host that never
    /// calls this reads them at 60 Hz.
    pub fn set_refresh(&mut self, period: Duration) {
        self.refresh = period.clamp(Duration::from_micros(4_167), ANIM_FRAME);
    }

    /// The display frame period the fine motion is read on.
    #[must_use]
    pub fn refresh(&self) -> Duration {
        self.refresh
    }

    // ---- posting -----------------------------------------------------

    /// Post a message: normalized, minted, logged. Same key and different
    /// words → the live row is superseded in place (same slot, fresh anchor; an
    /// unrevealed row keeps the earlier reveal, a revealed one stays revealed);
    /// the same words → a duplicate (`repeats += 1`, hold re-anchored on glass,
    /// nothing logged); `Hold::LogOnly` → recorded, never live. Past
    /// [`MAX_LIVE`] a row is evicted ([`Self::evict_past_cap`]).
    pub fn post(&mut self, msg: Message, stamp: WallStamp, now: Instant) -> Posted {
        let mut msg = msg.normalized();
        // The door: a class that cannot stand is a record.
        match msg.admit() {
            Admission::Kept => {}
            Admission::Demoted => {
                self.demotions = self.demotions.saturating_add(1);
                self.last_demoted = Some(msg.key.clone().unwrap_or_else(|| msg.title.clone()));
            }
        }
        // An exception is never live: it goes to the digest, whose changes
        // move no revision the band or a window reads.
        if matches!(msg.need, Need::Later(_)) {
            return self.post_later(msg, stamp, now);
        }
        // A standing row the owner already read is not raised again inside
        // [`crate::need::STANDING_QUIET`]: nothing shown, nothing logged.
        if let (Need::Now(ask), Some(key)) = (&msg.need, msg.key.as_deref())
            && ask.is_standing()
            && !ask.is_adopted()
            && self.index_by_key(key).is_none()
            && let Some(filed) = self.quiet_standing(key, stamp)
        {
            return Posted {
                id: filed,
                outcome: PostOutcome::Suppressed,
            };
        }
        self.revision += 1;
        if msg.hold == Hold::LogOnly {
            let id = self.log.mint();
            self.log
                .record_posted(LogRecord::from_posted(id, stamp, &msg));
            // A classified record under an adopted row's key is a newer build
            // saying the row its parent carried is no longer its answer; left
            // alone it would stand for ever, adopted and unconfirmed.
            if matches!(msg.need, Need::Record)
                && let Some(i) = msg.key.as_deref().and_then(|k| self.index_by_key(k))
                && matches!(&self.live[i].msg.need, Need::Now(a) if a.is_adopted())
            {
                self.retire_at(i, Retired::Superseded { by: id }, now);
                self.rebalance(now);
            }
            self.log.record_retired(
                id,
                Retired::Recorded,
                stamp.unix_ms,
                now,
                FinalWords {
                    title: &msg.title,
                    detail: &msg.detail,
                    repeats: 1,
                    mark: None,
                },
            );
            self.note_unfixed(id, &msg, &Retired::Recorded, None);
            return Posted {
                id,
                outcome: PostOutcome::New,
            };
        }
        if let Some(row) = self.live.iter_mut().find(|l| l.duplicates(&msg)) {
            row.repeats = row.repeats.saturating_add(1);
            if row.on_glass_since.is_some() {
                row.arm(now);
            }
            return Posted {
                id: row.id,
                outcome: PostOutcome::Duplicate,
            };
        }
        let id = self.log.mint();
        self.log
            .record_posted(LogRecord::from_posted(id, stamp, &msg));
        // A new row under the key says the problem as it stands now: the
        // closed records before it are not the ones a fix closes.
        if let Some(key) = msg.key.as_deref() {
            self.unfixed.retain(|u| u.key != key);
        }
        // One fact, one place: a row that takes over a key retires the
        // digest's entry for it.
        if let Some(i) = msg.key.as_deref().and_then(|k| self.digest.find_key(k)) {
            self.file_digest_at(i, Retired::Superseded { by: id }, stamp.unix_ms, now);
        }
        let mut entry = Live::new(id, msg, stamp, now);
        let superseded = entry.msg.key.as_deref().and_then(|key| {
            self.live
                .iter()
                .position(|l| l.msg.key.as_deref() == Some(key))
        });
        let outcome = if let Some(i) = superseded {
            let old = self.live.remove(i);
            inherit(&mut entry, &old, now);
            if entry.on_glass_since.is_some() {
                entry.arm(now);
            }
            if let Some(slot) = self.glass.iter_mut().find(|g| **g == old.id) {
                *slot = id;
            }
            self.log_retired(&old, Retired::Superseded { by: id }, None, now);
            self.live.insert(i, entry);
            PostOutcome::Superseded(old.id)
        } else {
            self.live.push(entry);
            self.evict_past_cap(id, now);
            PostOutcome::New
        };
        self.rebalance(now);
        Posted { id, outcome }
    }

    /// Past [`MAX_LIVE`] the victim is, in this order:
    ///
    /// 1. a queued row: held (Default/For) before `Live`, then the lowest
    ///    severity, then the oldest post (ties by the lower id). An unrevealed
    ///    row is never on the glass, so it counts as queued;
    /// 2. only when nothing queued is evictable, a row on the glass, in the
    ///    same order;
    /// 3. only when every other row is an ask or a standing row, the newcomer
    ///    itself, whatever its hold.
    ///
    /// Another row's post never evicts an Ask or Standing row, so a flood of
    /// posts thins the queue and takes no row off the glass while a queued row
    /// is left to take.
    fn evict_past_cap(&mut self, newcomer: MessageId, now: Instant) {
        while self.live.len() > MAX_LIVE {
            let victim = self
                .live
                .iter()
                .enumerate()
                .filter(|(_, l)| l.id != newcomer)
                .filter_map(|(i, l)| {
                    let live = match l.msg.hold {
                        Hold::Default | Hold::For(_) => false,
                        Hold::Live { .. } => true,
                        Hold::Ask { .. } | Hold::Standing(_) | Hold::LogOnly => return None,
                    };
                    Some(((!l.is_queued(), live, l.msg.severity, l.posted_at, l.id), i))
                })
                .min_by_key(|(key, _)| *key)
                .map(|(_, i)| i)
                .or_else(|| self.index(newcomer));
            let Some(i) = victim else { break };
            self.retire_at(i, Retired::Evicted, now);
        }
    }

    /// Remove `live[i]`, drop it from the glass and log its final words,
    /// leaving the default echo for `how` in its slot when it was a live row
    /// on the glass.
    fn retire_at(&mut self, i: usize, how: Retired, now: Instant) -> Live {
        let echo = EchoKind::for_retired(&how);
        self.retire_at_with(i, how, echo, now)
    }

    /// [`Self::retire_at`] naming the echo. The echo is recorded at the row's
    /// visual slot before it leaves, only for a revealed live row on the glass
    /// (a held row, an ask, a standing, queued or unrevealed row has no
    /// indicator to end). The log and every watcher see the row gone at once.
    fn retire_at_with(
        &mut self,
        i: usize,
        how: Retired,
        echo: Option<EchoKind>,
        now: Instant,
    ) -> Live {
        if let Some(kind) = echo {
            self.record_echo(i, kind, now);
        }
        let row = self.live.remove(i);
        self.glass.retain(|g| *g != row.id);
        self.log_retired(&row, how, echo, now);
        self.revision += 1;
        row
    }

    fn record_echo(&mut self, i: usize, kind: EchoKind, now: Instant) {
        let row = &self.live[i];
        if !row.revealed || !row.is_animated() || !self.glass.contains(&row.id) {
            return;
        }
        let Some(slot) = self
            .glass_position(row.id)
            .and_then(|p| u16::try_from(p).ok())
        else {
            return;
        };
        let fill = row.msg.meter.as_ref().and_then(|m| m.fill_permille);
        let from_permille = row.shown_fill(now).unwrap_or(0);
        let echo = Echo {
            id: row.id,
            msg: row.msg.clone(),
            kind,
            from_permille,
            indeterminate: fill.is_none(),
            comet_since: fill
                .is_none()
                .then_some(row.motion_since)
                .flatten()
                .map(|epoch| now.saturating_duration_since(epoch)),
            elapsed: now.saturating_duration_since(row.started_at),
            started: now,
            until: at(now, echo_span(kind, from_permille, fill.is_none())),
            slot,
            load: row.shown_load(),
            load_slot: row.load_slot,
        };
        self.echoes.retain(|e| e.slot != slot);
        self.echoes.push(echo);
    }

    fn log_retired(&mut self, row: &Live, how: Retired, echo: Option<EchoKind>, now: Instant) {
        let outcome = outcome_title(&row.msg, &how, echo);
        // A row restated to another mark (an upgrade row answered by the
        // person's word: `ℹ`, not the stall's `⚠`) retires under the mark it
        // shows; the outcome's own mark wins where it has one.
        let mark = outcome_mark(&row.msg, &how, echo).or_else(|| {
            let shown = (row.msg.severity, row.msg.glyph);
            (row.logged_mark != shown).then_some(shown)
        });
        let detail: Vec<String> = match Ending::of(&row.msg, &how, echo) {
            // Delivered work keeps no in-flight frame: its live title and last
            // reading described a moment that passed; the record says what was
            // done, and the meta line how long it took.
            Ending::Delivered => Vec::new(),
            // Cut off by quitting: the words stand, and a determinate row says
            // where its work was (the percent the band painted, floored alike).
            _ if how == Retired::Quit => quit_reading(row)
                .into_iter()
                .chain(row.msg.detail.iter().cloned())
                .take(crate::DETAIL_LINES_CAP)
                .collect(),
            // Everything else that changed its title keeps the live title
            // as the first detail line: what stopped, and where.
            _ => match &outcome {
                Some(_) => std::iter::once(row.msg.title.clone())
                    .chain(row.msg.detail.iter().cloned())
                    .take(crate::DETAIL_LINES_CAP)
                    .collect(),
                None => row.msg.detail.clone(),
            },
        };
        let title = outcome.as_deref().unwrap_or(&row.msg.title);
        self.note_unfixed(row.id, &row.msg, &how, echo);
        // The ring's record reads the mark the row retires under, as the
        // file's replay does (the Posted line's, or this line's `mark`).
        self.log.show_mark(row.id, mark.unwrap_or(row.logged_mark));
        self.log.record_retired(
            row.id,
            how,
            row.wall_at(now),
            now,
            FinalWords {
                title,
                detail: &detail,
                repeats: row.repeats,
                mark,
            },
        );
    }

    /// Remember `id` when it closed a problem unfixed: a warning or error
    /// under a key, with declared fix words, that left by a fold, a dismissal,
    /// going unseen, an eviction or as a record; never by a supersede or a
    /// carry (its problem goes on under the next row), and never an ask.
    fn note_unfixed(
        &mut self,
        id: MessageId,
        msg: &Message,
        how: &Retired,
        echo: Option<EchoKind>,
    ) {
        let closed_unfixed = matches!(
            how,
            Retired::Folded
                | Retired::Dismissed
                | Retired::Unseen
                | Retired::Evicted
                | Retired::Recorded
        );
        if !closed_unfixed
            || msg.is_ask()
            || msg.severity < Severity::Warn
            || Ending::of(msg, how, echo) != Ending::Kept
        {
            return;
        }
        let (Some(key), Some(fixed)) = (msg.key.as_ref(), msg.finished.as_ref()) else {
            return;
        };
        if self.unfixed.len() == UNFIXED_CAP {
            self.unfixed.remove(0);
        }
        self.unfixed.push(Unfixed {
            id,
            key: key.clone(),
            fixed: fixed.clone(),
        });
    }

    /// Close as fixed every problem under a key starting with `prefix` that
    /// left the glass unfixed in this process: its record takes its reporter's
    /// declared fix words (`Misspelled setting fixed`) under `✓` Success, its
    /// old title becomes the first detail line, and a second `retired` line
    /// (`resolved-ok`) goes to the log, so a relaunch replays it fixed and
    /// Problems stops counting it. Returns how many. Only this process's
    /// records: a previous launch's warnings are what that launch saw.
    pub fn fix_closed(&mut self, prefix: &str, now: Instant) -> usize {
        self.fix_closed_as(prefix, None, now)
    }

    /// [`Self::fix_closed`] in `words` when given: the fix words a reporter
    /// decided when it saw HOW the problem ended (the setting taken out, or now
    /// taking), in place of the ones it declared when it posted the warning,
    /// before anyone knew.
    pub fn fix_closed_as(&mut self, prefix: &str, words: Option<&str>, now: Instant) -> usize {
        let (fix, keep): (Vec<Unfixed>, Vec<Unfixed>) = std::mem::take(&mut self.unfixed)
            .into_iter()
            .partition(|u| u.key.starts_with(prefix));
        self.unfixed = keep;
        let mut fixed = 0;
        for u in fix {
            let Some(rec) = self.log.get(u.id) else {
                continue;
            };
            let (Some(unix), Some(at)) = (rec.retired_unix_ms, rec.retired_at) else {
                continue;
            };
            let since = u64::try_from(now.saturating_duration_since(at).as_millis()).unwrap_or(0);
            self.close_fixed(
                u.id,
                words.unwrap_or(&u.fixed),
                unix.saturating_add(since),
                now,
            );
            fixed += 1;
        }
        if fixed > 0 {
            self.revision += 1;
        }
        fixed
    }

    /// Close one record that already left the glass as fixed by name: its
    /// reporter saw its cause settled after the row went (a tab's lost login
    /// back after its notice was read, or after the point it stood for moved
    /// on). The record takes `fixed` (the fix words its reporter declares)
    /// under `✓` Success at `wall`, never before it first retired, its old
    /// title the first detail line, as [`Self::fix_closed`] closes a problem
    /// by its key; one more `retired` line (`resolved-ok`) goes to the log,
    /// and its standing quiet ([`Self::quiet_standing`]) ends with it.
    /// `revision + 1`.
    ///
    /// `false` and nothing written for a live row (its reporter resolves it),
    /// an open exception, no such record, empty words, and a record whose
    /// story already ended otherwise: replaced, carried, answered by the owner
    /// or resolved.
    pub fn fix_record(
        &mut self,
        id: MessageId,
        fixed: &str,
        wall: WallStamp,
        now: Instant,
    ) -> bool {
        let fixed = clip(fixed, crate::TITLE_CAP);
        if fixed.is_empty() || self.index(id).is_some() || self.digest.find_id(id).is_some() {
            return false;
        }
        let Some(rec) = self.log.get(id) else {
            return false;
        };
        let (Some(how), Some(retired)) = (rec.retired(), rec.retired_unix_ms) else {
            return false;
        };
        if matches!(
            how,
            Retired::Superseded { .. }
                | Retired::Carried
                | Retired::Answered { .. }
                | Retired::Resolved(_)
        ) {
            return false;
        }
        self.close_fixed(id, &fixed, wall.unix_ms.max(retired), now);
        self.unfixed.retain(|u| u.id != id);
        self.revision += 1;
        true
    }

    /// Record `id` retired again `Resolved(Ok)` at wall `unix_ms` in `fixed`
    /// under `✓` Success, its old title the first detail line. `false` for no
    /// such record.
    fn close_fixed(&mut self, id: MessageId, fixed: &str, unix_ms: u64, now: Instant) -> bool {
        let Some(rec) = self.log.get(id) else {
            return false;
        };
        let detail: Vec<String> = std::iter::once(rec.title.clone())
            .chain(rec.detail.iter().cloned())
            .take(crate::DETAIL_LINES_CAP)
            .collect();
        let repeats = rec.repeats;
        let ok = (
            Severity::Success,
            Glyph::or_fallback(severity_icon(Severity::Success)),
        );
        let mark = ((rec.severity, rec.glyph) != ok).then_some(ok);
        self.log.record_retired(
            id,
            Retired::Resolved(Outcome::Ok),
            unix_ms,
            now,
            FinalWords {
                title: fixed,
                detail: &detail,
                repeats,
                mark,
            },
        );
        true
    }

    /// Re-word a record ([`Retired::Recorded`]) in place: it takes `msg`'s
    /// title and detail with no new entry, so one waiting upgrade stays one
    /// message, and one more `retired` line lets a relaunch replay it re-worded
    /// ([`MessageLog::replay`]). `revision + 1` when the words changed; its
    /// mark, key and capsules stay the record's.
    ///
    /// `false` and nothing written for a live row ([`Self::restate`]'s), any
    /// other retirement (it keeps the words it left with), words it says
    /// already, and a message whose capsules differ from the record's (the
    /// words would name buttons it lacks).
    pub fn reword_record(&mut self, id: MessageId, msg: &Message) -> bool {
        let msg = msg.clone().normalized();
        if self.index(id).is_some()
            || self.log.get(id).is_none_or(|rec| {
                rec.retired() != Some(&Retired::Recorded) || rec.actions != msg.actions
            })
        {
            return false;
        }
        let changed = self.log.record_reworded(id, &msg.title, &msg.detail);
        if changed {
            self.revision += 1;
        }
        changed
    }

    /// Replace a row or a record by the later row `by` that tells its story on
    /// — one fact told under another key (a tab's notice moving to its next
    /// state), or again after its row left the glass: it retires
    /// [`Retired::Superseded`] by it, as a keyed supersede does, so Settings ▸
    /// Messages lists the story once, as `by` ([`crate::page`]). A live row
    /// leaves the glass now; a retired record keeps its words and instant, and
    /// one more `retired` line goes to the log, so a relaunch replays it
    /// replaced. `revision + 1`.
    ///
    /// `false` and nothing written where `by` is no record posted after `id`
    /// (a story is told on, never back), for an open exception (the digest
    /// supersedes by its key), and for a record whose story already ended
    /// elsewhere: replaced already, carried to a successor, answered by the
    /// owner or resolved by its reporter (a new row is a new story).
    pub fn supersede_record(&mut self, id: MessageId, by: MessageId, now: Instant) -> bool {
        if by <= id || self.log.get(by).is_none() || self.digest.find_id(id).is_some() {
            return false;
        }
        if let Some(i) = self.index(id) {
            self.retire_at(i, Retired::Superseded { by }, now);
            self.rebalance(now);
            return true;
        }
        let tells_on = self
            .log
            .get(id)
            .and_then(LogRecord::retired)
            .is_some_and(|how| {
                !matches!(
                    how,
                    Retired::Superseded { .. }
                        | Retired::Carried
                        | Retired::Answered { .. }
                        | Retired::Resolved(_)
                )
            });
        if !tells_on
            || !self
                .log
                .record_retired_again(id, Retired::Superseded { by })
        {
            return false;
        }
        // Its problem goes on under the row (as a supersede never closes one
        // unfixed: [`Self::note_unfixed`]).
        self.unfixed.retain(|u| u.id != id);
        self.revision += 1;
        true
    }

    /// Turn a live row into its own record in place: it leaves the glass as a
    /// withdrawn row does (a vanish) and retires [`Retired::Recorded`] under
    /// `msg`'s mark, one entry that [`Self::reword_record`] re-words and
    /// [`Self::supersede_record`] replaces. It takes `msg`'s title and detail
    /// only when `msg` has the record's capsules ([`Self::reword_record`]'s
    /// rule). `false` when `id` is no live row.
    pub fn keep_as_record(&mut self, id: MessageId, msg: &Message, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        let msg = msg.clone().normalized();
        let for_its_capsules = self
            .log
            .get(id)
            .is_some_and(|rec| rec.actions == msg.actions);
        self.record_echo(i, EchoKind::Vanish, now);
        let row = &mut self.live[i];
        row.msg.severity = msg.severity;
        row.msg.glyph = msg.glyph;
        if for_its_capsules {
            row.msg.title = msg.title;
            row.msg.detail = msg.detail;
        }
        self.retire_at_with(i, Retired::Recorded, None, now);
        self.rebalance(now);
        true
    }

    // ---- lifecycle ---------------------------------------------------

    /// Change a live row in place: no log line, `revision + 1` when the row's
    /// words, meter, capsules, hold or look changed; a Live row re-arms its
    /// staleness cap; a new hold re-anchors on glass. A carried row restated
    /// before the handoff commit becomes the successor's own: the handoff cap
    /// comes off and its own lifetime starts now. A new meter feeds the
    /// estimator, glides from the fill shown and restarts the load clock when
    /// the load changes; a change of activity restarts the motion epoch.
    /// `false` when no such live row.
    pub fn restate(&mut self, id: MessageId, r: Restatement, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.motion_input_epoch = self.motion_input_epoch.wrapping_add(1);
        let row = &mut self.live[i];
        let activity = row.activity();
        let before = row.msg.clone();
        let eta_before = row.track.eta(now);
        if let Some(title) = r.title {
            let title = clip(&title, TITLE_CAP);
            if title != row.msg.title {
                // Declared finished words finished the old title, and its
                // short form shortened it.
                row.msg.finished = None;
                row.msg.short_title = None;
            }
            row.msg.title = title;
        }
        if let Some(finished) = r.finished {
            row.msg.finished = finished
                .map(|w| clip(&w, TITLE_CAP))
                .filter(|w| !w.is_empty());
        }
        if let Some(short) = r.short_title {
            row.msg.short_title = short.map(|w| clip(&w, TITLE_CAP)).filter(|w| !w.is_empty());
        }
        if let Some(detail) = r.detail {
            row.msg.detail = detail
                .iter()
                .map(|l| clip(l, DETAIL_LINE_CAP))
                .filter(|l| !l.is_empty())
                .take(DETAIL_LINES_CAP)
                .collect();
        }
        if let Some(meter) = r.meter {
            restate_meter(row, meter.map(Meter::normalized), now);
        }
        if let Some(mut actions) = r.actions {
            actions.retain(|a| *a != Intent::Details);
            actions.truncate(MAX_ACTIONS);
            row.msg.actions = actions;
        }
        if let Some(severity) = r.severity {
            row.msg.severity = severity;
        }
        if let Some(glyph) = r.glyph {
            row.msg.glyph = glyph;
        }
        if let Some(excerpt) = r.excerpt {
            row.msg.excerpt = excerpt;
        }
        let was_carried = std::mem::take(&mut row.carried_pending);
        // The door again: a restatement may shorten a row's hold or end it, but
        // it cannot make a row persistent, and it cannot stretch a transient
        // one past its span (the class decides the span).
        let admitted = r.hold.and_then(|h| {
            let (h, demoted) = admit_restated_hold(&row.msg.need, row.msg.hold, h);
            self.demotions = self.demotions.saturating_add(u32::from(demoted));
            h
        });
        if let Some(hold) = admitted {
            if hold == Hold::LogOnly {
                self.retire_at(i, Retired::Folded, now);
                self.rebalance(now);
                return true;
            }
            row.msg.hold = hold;
            row.fold_at = None;
            row.stale_at = None;
            // The new hold's span, on the clock the row already runs: its
            // reveal (or post), or the demote that last took it off the glass;
            // a heartbeat restating the same hold never moves a demoted row's
            // patience back to its post.
            row.patience_at = at(
                row.patience_from,
                row.hold_span().max(OVERFLOW_PATIENCE_MIN),
            );
            if row.on_glass_since.is_some() {
                row.arm(now);
            } else if let Hold::Live { stale_after } = hold {
                row.stale_at = Some(at(now, stale_after));
            }
        } else if let Hold::Live { stale_after } = row.msg.hold {
            row.stale_at = Some(at(now, stale_after));
        } else if was_carried {
            row.stale_at = None;
            if row.on_glass_since.is_some() {
                row.arm(now);
            }
        }
        if row.activity() != activity && row.on_glass_since.is_some() {
            row.motion_since = Some(now);
        }
        // A restatement that changed nothing the row says (a heartbeat
        // re-feeding the same reading) is not a change of the glass: no
        // revision, so no repaint and no Settings publish. An estimate that
        // moved counts as a change, since no motion deadline is armed for it.
        let eta_moved = row.track.eta(now) != eta_before;
        if row.msg.actions != before.actions {
            row.action_revision = row.action_revision.and_then(|rev| rev.checked_add(1));
        }
        let changed = row.msg != before || was_carried || eta_moved;
        if changed {
            row.revision = row.revision.wrapping_add(1);
            self.revision += 1;
        }
        // The record reads the mark the row shows while it lives, so
        // `messages` prints it and its `sev=` floor selects on it, as the page
        // and the glass do; no line is written.
        let shown = (row.msg.severity, row.msg.glyph);
        if shown != (before.severity, before.glyph) {
            self.log.show_mark(id, shown);
        }
        true
    }

    /// The reporter finished with it: retires now as `Resolved(outcome)`; a
    /// live row on the glass echoes Complete (`Ok`: the work was delivered)
    /// or Fault (`Warn`).
    pub fn resolve(&mut self, id: MessageId, outcome: Outcome, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at(i, Retired::Resolved(outcome), now);
        self.rebalance(now);
        true
    }

    /// Resolve every live row — and every exception in the digest — whose key
    /// starts with `prefix` (`config.` on a clean reload, `privacy.` on
    /// policy-off). Returns how many. An `Ok` also closes as fixed the problems
    /// under the prefix that already left the glass unfixed
    /// ([`Self::fix_closed`]), counted too.
    pub fn resolve_key_prefix(&mut self, prefix: &str, outcome: Outcome, now: Instant) -> usize {
        let ids: Vec<MessageId> = self
            .live
            .iter()
            .filter(|l| l.msg.key.as_deref().is_some_and(|k| k.starts_with(prefix)))
            .map(|l| l.id)
            .collect();
        for id in &ids {
            self.resolve(*id, outcome, now);
        }
        let closed = if outcome == Outcome::Ok {
            self.fix_closed(prefix, now)
        } else {
            0
        };
        ids.len() + closed + self.resolve_digest_prefix(prefix, outcome, now)
    }

    /// A person dismissed it.
    pub fn dismiss(&mut self, id: MessageId, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at(i, Retired::Dismissed, now);
        self.rebalance(now);
        true
    }

    /// The reporter withdrew it with no outcome to claim: a live meter whose
    /// source vanished before any marker said how the pass ended
    /// (`Retired::Withdrawn`: the log keeps the row; the `appstatus` face
    /// lists no finished activity for it). A live row on the glass fades
    /// ([`EchoKind::Vanish`]).
    pub fn withdraw(&mut self, id: MessageId, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at(i, Retired::Withdrawn, now);
        self.rebalance(now);
        true
    }

    /// Aterm is quitting: every open row (live work, a held row, one still
    /// queued) retires [`Retired::Quit`] with no echo (there is no next
    /// frame), so the log says the process ended it and, for a determinate
    /// row, where the work was. The host calls this on its graceful exit, then
    /// flushes the log. Returns how many rows it closed.
    pub fn quit(&mut self, now: Instant) -> usize {
        let mut closed = 0;
        while !self.live.is_empty() {
            self.retire_at_with(0, Retired::Quit, None, now);
            closed += 1;
        }
        self.echoes.clear();
        closed
    }

    /// [`Self::withdraw`] with the echo named: the record stays `Withdrawn`
    /// (no outcome on `appstatus`), while the glass shows how the pass really
    /// ended (the first-run and heavy toolchain rows end Complete or Fault by
    /// the child's exit).
    pub fn withdraw_with(&mut self, id: MessageId, echo: EchoKind, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at_with(i, Retired::Withdrawn, Some(echo), now);
        self.rebalance(now);
        true
    }

    /// [`Self::withdraw`] whose log entry keeps `title` and `detail` as its
    /// final words: an episode's live row becomes its own record (a stopped
    /// paste's), one log entry per episode. The glass sees the row it showed
    /// (its echo is taken first); `false` when no such live row.
    pub fn withdraw_as(
        &mut self,
        id: MessageId,
        title: &str,
        detail: &[String],
        now: Instant,
    ) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.record_echo(i, EchoKind::Vanish, now);
        let row = &mut self.live[i];
        row.msg.title = title.to_string();
        row.msg.detail = detail.to_vec();
        self.retire_at_with(i, Retired::Withdrawn, None, now);
        self.rebalance(now);
        true
    }

    /// The person answered the row with a word its reporter confirmed was
    /// taken: it retires `Answered { label }` with no echo, as a `NotNow` press
    /// does. For a capsule whose word is written off the host's thread and may
    /// be refused, so the press ([`Self::act`]) cannot close it. `false` when
    /// no such live row.
    pub fn answer(&mut self, id: MessageId, label: &str, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at(
            i,
            Retired::Answered {
                label: label.to_string(),
            },
            now,
        );
        self.rebalance(now);
        true
    }

    /// The row was read (Details opened, the body pressed, or a navigation
    /// capsule took the person to fix it): a Default/For/Standing row folds
    /// now; Live and Ask rows stay.
    pub fn mark_seen(&mut self, id: MessageId, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.live[i].seen = true;
        self.revision += 1;
        if matches!(
            self.live[i].msg.hold,
            Hold::Default | Hold::For(_) | Hold::Standing(_)
        ) {
            self.retire_at(i, Retired::Folded, now);
            self.rebalance(now);
        }
        true
    }

    /// A queued row nobody could have seen waits on: its patience starts again
    /// from `now`, as when it is released from the glass ([`Self::demote`]), so
    /// it never retires `Unseen` while the host knows no band is on screen (a
    /// host's row owed to the first window that can show it). A row on the glass
    /// has no patience running and is left as it is. `false` when `id` is no
    /// queued live row.
    pub fn restart_patience(&mut self, id: MessageId, now: Instant) -> bool {
        let Some(row) = self.live.iter_mut().find(|l| l.id == id && l.is_queued()) else {
            return false;
        };
        row.patience_from = now;
        row.patience_at = at(now, row.hold_span().max(OVERFLOW_PATIENCE_MIN));
        true
    }

    /// A capsule press: logs `Acted { label }` and returns the intent for the
    /// host to perform. The row retires only when [`Intent::closes_row`]
    /// (`NotNow` → `Answered`); every other intent leaves it for a supersede
    /// or a fold. The implicit Details capsule is
    /// [`MessageCenter::mark_seen`] and is not logged.
    pub fn act(&mut self, id: MessageId, action: ActionIndex, now: Instant) -> Option<Intent> {
        let i = self.index(id)?;
        if action.is_details() {
            self.mark_seen(id, now);
            return Some(Intent::Details);
        }
        let intent = self.live[i].msg.actions.get(usize::from(action.0))?.clone();
        let label = intent.label();
        let unix_ms = self.live[i].wall_at(now);
        self.log.record_acted(id, unix_ms, label);
        self.live[i].acted = Some((action, now));
        self.revision += 1;
        if intent.closes_row() {
            self.retire_at(
                i,
                Retired::Answered {
                    label: label.to_string(),
                },
                now,
            );
            self.rebalance(now);
        }
        Some(intent)
    }

    /// Perform an action only while the id and action-list revision captured
    /// by its publisher still stand. Refusal writes no acted/seen record.
    pub fn act_at_revision(
        &mut self,
        id: MessageId,
        revision: u64,
        action: ActionIndex,
        now: Instant,
    ) -> Option<Intent> {
        if self.live(id)?.action_revision() != Some(revision) {
            return None;
        }
        self.act(id, action, now)
    }

    // ---- settle ------------------------------------------------------

    /// Reveal the rows whose grace is over, retire what is due (holds only
    /// when `holds`: a handoff freeze suspends the holds, never the staleness
    /// and patience caps), end the echoes that are over, show the load words
    /// that have lasted, then re-rank the queue.
    pub fn settle(&mut self, now: Instant, holds: bool) -> Settled {
        let before = self.glass_signature();
        let mut revealed = false;
        for row in &mut self.live {
            if !row.revealed && row.reveal_at.is_some_and(|t| now >= t) {
                // No flash at the grace's end: work that projects to end
                // within REVEAL_MIN_LEFT is held back, looked at again every
                // REVEAL_RECHECK, until it ends unseen or reaches its cap.
                let ending = row
                    .track
                    .quick_left(now)
                    .is_some_and(|left| left < REVEAL_MIN_LEFT);
                if let Some(cap) = row.reveal_cap.filter(|cap| ending && now < *cap) {
                    row.reveal_at = Some(at(now, REVEAL_RECHECK).min(cap));
                } else {
                    row.revealed = true;
                    row.reveal_at = None;
                    revealed = true;
                }
            }
            if !row.load_shown
                && let Some((_, since)) = row.load
                && now >= at(since, LOAD_AFTER)
            {
                row.load_shown = true;
            }
        }
        let mut retired = Vec::new();
        let mut i = 0;
        while i < self.live.len() {
            let row = &self.live[i];
            let how = if holds && row.fold_at.is_some_and(|t| now >= t) {
                Some(Retired::Folded)
            } else if row.stale_at.is_some_and(|t| now >= t) {
                Some(Retired::Stale)
            } else if row.is_queued() && now >= row.patience_at {
                Some(Retired::Unseen)
            } else {
                None
            };
            match how {
                Some(how) => {
                    let id = row.id;
                    self.retire_at(i, how.clone(), now);
                    retired.push((id, how));
                }
                None => i += 1,
            }
        }
        self.echoes.retain(|e| now < e.until);
        self.rebalance(now);
        // A reveal counts as a change of the glass: the row count it wants
        // moves with it, and the host commits it in the same sweep.
        let glass_changed = revealed || before != self.glass_signature();
        // …and an observable one: a row that reached or left the glass, or an
        // echo that ended, reads another `on_glass` on the Settings page,
        // which republishes on the revision.
        if glass_changed {
            self.revision += 1;
        }
        Settled {
            glass_changed,
            retired,
        }
    }

    /// The next instant a settle would act on: the nearest fold (holds only),
    /// staleness, patience or reveal instant, an echo's end, a load word's
    /// arrival, or the pending shrink at the later of its quiet's end
    /// ([`Self::quiet_shrink_at`]) and the chrome regrow floor (holds only:
    /// [`crate::drive::commit`] commits no rows under a freeze, so a frozen
    /// band never re-arms an overdue shrink). `None` when nothing is armed. The
    /// motion's frames are [`Self::motion_deadline`]'s.
    #[must_use]
    pub fn deadline(&self, holds: bool) -> Option<Instant> {
        let rows = self.live.iter().flat_map(|l| {
            let load = (!l.load_shown && !l.is_queued())
                .then(|| l.load.map(|(_, since)| at(since, LOAD_AFTER)))
                .flatten();
            [
                if holds { l.fold_at } else { None },
                l.stale_at,
                l.is_queued().then_some(l.patience_at),
                l.reveal_at.filter(|_| !l.revealed),
                load,
            ]
        });
        rows.flatten()
            .chain(self.echoes.iter().map(|e| e.until))
            .chain(
                self.shrink_since
                    .filter(|_| holds)
                    .and_then(|_| self.quiet_shrink_at())
                    .map(|q| crate::chrome::at_floor(q, self.regrow_floor)),
            )
            .min()
    }

    // ---- rows --------------------------------------------------------

    /// The rows eligible for the glass: every live row past its grace.
    #[must_use]
    pub(crate) fn eligible(&self) -> usize {
        self.live.iter().filter(|l| l.revealed).count()
    }

    /// `min(MAX_ROWS, eligible)` (the overflow row counts); 0 when empty. An
    /// echo is never counted: it costs no re-grid.
    #[must_use]
    pub fn wanted_rows(&self) -> u16 {
        u16::try_from(self.eligible())
            .unwrap_or(u16::MAX)
            .min(MAX_ROWS)
    }

    /// Commit the row count with hysteresis over `min(wanted, afford)`: grow
    /// now, shrink only after [`SHRINK_QUIET`] of wanting less, so a burst of
    /// short rows folds the grid once. The window's own shrink (`afford` fell
    /// below the committed count) is not a burst: it gets the row back now,
    /// neither floored nor held for an echo, dropping the echoes whose slot is
    /// past the new count. A quiet shrink waits its own quiet
    /// ([`Self::quiet_shrink_at`], never while an echo plays) and the windows'
    /// chrome regrow floor ([`Self::set_regrow_floor`]). `Some(new)` exactly
    /// when the count moved. Rows that just reached the glass anchor their
    /// holds here; only rows within the committed count ever do. The revision
    /// moves when the count or the glass did.
    pub fn commit_rows(&mut self, now: Instant, afford: u16) -> Option<u16> {
        let want = self.wanted_rows().min(afford);
        let mut moved = None;
        match want.cmp(&self.committed_rows) {
            std::cmp::Ordering::Greater => {
                self.committed_rows = want;
                self.shrink_since = None;
                moved = Some(want);
            }
            std::cmp::Ordering::Less => {
                self.shrink_since.get_or_insert(now);
                let quiet = self.quiet_shrink_at().is_some_and(|q| now >= q)
                    && crate::chrome::fall_allowed(self.regrow_floor, now);
                if afford < self.committed_rows {
                    self.committed_rows = want;
                    self.shrink_since = None;
                    moved = Some(want);
                } else if quiet {
                    // Every echo has played to its end: drop the ones no
                    // settle has swept yet, so none holds a position here.
                    self.echoes.retain(|e| now < e.until);
                    self.committed_rows = want;
                    self.shrink_since = None;
                    moved = Some(want);
                }
            }
            std::cmp::Ordering::Equal => self.shrink_since = None,
        }
        let placed = self.glass_signature();
        self.rebalance(now);
        if moved.is_some() || placed != self.glass_signature() {
            self.revision += 1;
        }
        moved
    }

    /// Hand in the windows' chrome regrow floor ([`crate::chrome`]): the
    /// latest [`crate::chrome::RegrowFloor::floor`] of every window the band's
    /// re-grid resizes, which [`Self::commit_rows`]' quiet shrink and
    /// [`Self::deadline`]'s pending shrink then wait. The band's driver hands
    /// it at every step ([`crate::drive::commit`]); `None` clears it.
    pub fn set_regrow_floor(&mut self, floor: Option<Instant>) {
        self.regrow_floor = floor;
    }

    /// When the pending quiet shrink's own quiet runs out, before the chrome
    /// regrow floor ([`Self::set_regrow_floor`]): [`SHRINK_QUIET`] past the
    /// first commit that wanted fewer rows, and never while an echo still
    /// plays (the count wants no echo, so a shrink under one would cut it short
    /// or push a live row off the glass). `None` with no shrink pending.
    #[must_use]
    pub fn quiet_shrink_at(&self) -> Option<Instant> {
        self.shrink_since.map(|s| {
            self.echoes
                .iter()
                .map(|e| e.until)
                .fold(at(s, SHRINK_QUIET), Instant::max)
        })
    }

    /// The committed row count.
    #[must_use]
    pub fn committed_rows(&self) -> u16 {
        self.committed_rows
    }

    /// Whether the overflow row is up: more eligible rows than the band.
    fn overflow_up(&self) -> bool {
        let committed = usize::from(self.committed_rows);
        self.eligible() > committed && committed > 1
    }

    /// Message positions among the committed rows — live rows and echoes —
    /// before the overflow row.
    fn positions(&self) -> usize {
        let committed = usize::from(self.committed_rows);
        if self.overflow_up() {
            committed - 1
        } else {
            committed.min(self.eligible() + self.echoes.len())
        }
    }

    /// Live slots: the positions an echo has not pinned.
    fn slots(&self) -> usize {
        self.positions().saturating_sub(self.echoes.len())
    }

    /// How many eligible rows the overflow row stands for, when it is up.
    fn overflow_hidden(&self) -> Option<usize> {
        self.overflow_up()
            .then(|| self.eligible().saturating_sub(self.slots()))
    }

    /// Fill the glass, stable order: departures leave, survivors move up, the
    /// freed bottom slot takes the best queued row (anchoring its hold now); a
    /// newcomer goes above only when it outranks every row on glass (stepping
    /// over a slot an echo pins). A row the glass has no slot for any more
    /// (the overflow row took it, a newcomer outranked it, the window shrank,
    /// an echo holds it) is released ([`MessageCenter::demote`]): not painted,
    /// so not burning its hold. Only revealed rows are candidates.
    fn rebalance(&mut self, now: Instant) {
        let committed = usize::from(self.committed_rows);
        self.echoes.retain(|e| usize::from(e.slot) < committed);
        let cap = if self.overflow_up() {
            committed.saturating_sub(1)
        } else {
            committed
        };
        while self.echoes.len() > cap {
            if let Some(worst) = self
                .echoes
                .iter()
                .enumerate()
                .max_by_key(|(_, e)| e.slot)
                .map(|(i, _)| i)
            {
                self.echoes.remove(worst);
            }
        }
        let slots = self.slots();
        let live = &self.live;
        self.glass.retain(|id| live.iter().any(|l| l.id == *id));
        // The progress row: with two rows or more, the best live progress row
        // is never the one the overflow row hides. It is placed before
        // anything else competes, and nothing pushes it off.
        let reserved = self.reserved_progress();
        loop {
            let missing = reserved.filter(|r| !self.glass.contains(r));
            let best = missing.or_else(|| {
                self.live
                    .iter()
                    .filter(|l| l.revealed && !self.glass.contains(&l.id))
                    .max_by_key(|l| rank(l))
                    .map(|l| l.id)
            });
            let Some(best) = best else { break };
            let above = self
                .glass
                .iter()
                .all(|g| outranks(self.by_id(best), self.by_id(*g)));
            if self.glass.len() < slots {
                if above {
                    self.glass.insert(0, best);
                } else {
                    self.glass.push(best);
                }
            } else if slots > 0 && (above || missing.is_some()) {
                // The bottom-most row the reservation does not hold makes
                // room; an ask is never the one pushed off for progress.
                let victim = self.glass.iter().rposition(|g| {
                    Some(*g) != reserved && (missing.is_none() || !self.by_id(*g).msg.is_ask())
                });
                let Some(victim) = victim else { break };
                let pushed_off = self.glass.remove(victim);
                self.demote(pushed_off, now);
                if above {
                    self.glass.insert(0, best);
                } else {
                    self.glass.push(best);
                }
            } else {
                break;
            }
            if let Some(row) = self.live.iter_mut().find(|l| l.id == best)
                && row.on_glass_since.is_none()
            {
                row.on_glass_since = Some(now);
                // The motion epoch is the row's first glass: a row released and
                // re-placed (the transient the overflow row makes while the
                // band grows by a row) keeps its phase, so a comet never jumps
                // back to the start because another row arrived.
                row.motion_since.get_or_insert(now);
                row.arm(now);
            }
        }
        while self.glass.len() > slots {
            let victim = self
                .glass
                .iter()
                .rposition(|g| Some(*g) != reserved)
                .unwrap_or(self.glass.len() - 1);
            let pushed_off = self.glass.remove(victim);
            self.demote(pushed_off, now);
        }
    }

    /// The words the overflow row names a hidden live progress row by: the
    /// best such row the glass does not show, its title and, with a fill, its
    /// whole percent (`Downloading aterm v0.91.0 45%`). `None` when every
    /// progress row is on the glass. Only a band of two rows can hide one: the
    /// reservation keeps the best on the glass.
    fn hidden_progress_words(&self) -> Option<String> {
        let row = self
            .live
            .iter()
            .filter(|l| l.revealed && l.is_progress() && !self.glass.contains(&l.id))
            .max_by_key(|l| rank(l))?;
        Some(match row.msg.meter.as_ref().and_then(|m| m.fill_permille) {
            Some(p) => format!("{} {}%", row.msg.title, p.min(1000) / 10),
            None => row.msg.title.clone(),
        })
    }

    /// The live progress row the glass keeps a row for: with two committed
    /// rows or more (a single row is the best row, with `+N ›` beside it), the
    /// highest-ranked revealed row that is live progress
    /// ([`Live::is_progress`]). `None` when there is none, or when the only
    /// row it could take is an ask's: asks still rank first.
    fn reserved_progress(&self) -> Option<MessageId> {
        if self.committed_rows < 2 {
            return None;
        }
        let best = self
            .live
            .iter()
            .filter(|l| l.revealed && l.is_progress())
            .max_by_key(|l| rank(l))?;
        let asks = self
            .live
            .iter()
            .filter(|l| l.revealed && l.msg.is_ask() && l.id != best.id)
            .count();
        (asks < self.slots()).then_some(best.id)
    }

    /// A row left the glass without retiring: its anchor is released, it is
    /// queued again under the patience clock from now, and anchors afresh
    /// when it returns (holds anchor at first glass; a row not painted never
    /// burns its hold). A Live row keeps its staleness cap and a carried row
    /// its handoff cap.
    fn demote(&mut self, id: MessageId, now: Instant) {
        if let Some(row) = self.live.iter_mut().find(|l| l.id == id) {
            row.on_glass_since = None;
            row.fold_at = None;
            row.patience_from = now;
            row.patience_at = at(now, row.hold_span().max(OVERFLOW_PATIENCE_MIN));
        }
    }

    fn by_id(&self, id: MessageId) -> &Live {
        // Every id in `glass` is in `live` (the retain above), and `best`
        // was just read from `live`.
        self.live
            .iter()
            .find(|l| l.id == id)
            .unwrap_or(&self.live[0])
    }

    fn index(&self, id: MessageId) -> Option<usize> {
        self.live.iter().position(|l| l.id == id)
    }

    /// The visual rows before the overflow row: echoes at the slots they pin
    /// (pulled up past a gap no row fills), live ids in order everywhere
    /// else.
    fn visual(&self) -> Vec<Visual> {
        let mut order: Vec<usize> = (0..self.echoes.len()).collect();
        order.sort_by_key(|i| self.echoes[*i].slot);
        let mut echoes = order.into_iter().peekable();
        let mut glass = self.glass.iter();
        let total = self.glass.len() + self.echoes.len();
        let mut out = Vec::with_capacity(total);
        for pos in 0..total {
            if let Some(&e) = echoes.peek()
                && usize::from(self.echoes[e].slot) <= pos
            {
                out.push(Visual::Echo(e));
                echoes.next();
            } else if let Some(id) = glass.next() {
                out.push(Visual::Live(*id));
            } else if let Some(e) = echoes.next() {
                out.push(Visual::Echo(e));
            }
        }
        out
    }

    /// The visual row (0 = topmost) a live row is on, echoes counted.
    #[must_use]
    pub fn glass_position(&self, id: MessageId) -> Option<usize> {
        self.visual().iter().position(|v| *v == Visual::Live(id))
    }

    /// The echoes on the band.
    #[must_use]
    pub fn echoes(&self) -> &[Echo] {
        &self.echoes
    }

    /// What [`Settled::glass_changed`] compares: the visible ids with their
    /// revisions and load words, the echoes, the overflow row's count, the
    /// `+N` a single row's Details capsule carries, and the hidden progress
    /// row the overflow row names.
    fn glass_signature(&self) -> GlassSignature {
        debug_assert!(self.glass.len() <= usize::from(MAX_ROWS));
        debug_assert!(self.echoes.len() <= usize::from(MAX_ROWS));
        (
            std::array::from_fn(|i| {
                self.glass.get(i).map(|g| {
                    let row = self.by_id(*g);
                    (*g, row.revision, row.load_shown)
                })
            }),
            std::array::from_fn(|i| self.echoes.get(i).map(|e| (e.slot, e.id, e.kind))),
            self.overflow_hidden(),
            self.plus_hidden(),
            self.overflow_up()
                .then(|| self.hidden_progress_words())
                .flatten(),
        )
    }

    /// The `+N` on a single committed row's Details capsule: how many
    /// eligible rows stand behind it (0 when the band has more rows, or
    /// nothing is queued).
    fn plus_hidden(&self) -> usize {
        if self.committed_rows == 1 {
            self.eligible().saturating_sub(self.slots())
        } else {
            0
        }
    }

    // ---- reading -----------------------------------------------------

    /// The width law over the committed rows: one [`RowSpec`] per visual row,
    /// live rows and echoes (paths under `home` printed as `~/…`), the
    /// overflow row last when the eligible set is larger than the band, `+N ›`
    /// on a single row; the links only when `links` says they are painted
    /// ([`Links`]).
    #[must_use]
    pub fn presentation(
        &self,
        cols: usize,
        width: &dyn Fn(&str) -> usize,
        home: Option<&str>,
        links: Links,
    ) -> Presentation {
        let plus = self.plus_hidden();
        let mut inputs = Vec::with_capacity(usize::from(self.committed_rows));
        let mut echo_words = Vec::with_capacity(inputs.capacity());
        for v in self.visual() {
            let (kind, msg, indicator, load, load_slot, finished) =
                match v {
                    Visual::Live(id) => {
                        let row = self.by_id(id);
                        let fill = row.msg.meter.as_ref().and_then(|m| m.fill_permille);
                        (
                            RowKind::Message(id),
                            &row.msg,
                            Indicator {
                                fill,
                                busy: row.is_busy(),
                                moving: row.is_animated(),
                            },
                            row.said_load(),
                            row.load_slot,
                            None,
                        )
                    }
                    Visual::Echo(e) => {
                        let echo = &self.echoes[e];
                        // Laid out as the row was — it was moving — with a
                        // Complete fill already at the window's edge.
                        let fill =
                            echo.msg.meter.as_ref().and_then(|m| m.fill_permille).map(
                                |p| match echo.kind {
                                    EchoKind::Complete => 1000,
                                    EchoKind::Fault | EchoKind::Vanish => p,
                                },
                            );
                        (
                            RowKind::Echo(echo.id),
                            &echo.msg,
                            Indicator {
                                fill,
                                busy: echo.indeterminate,
                                moving: true,
                            },
                            None,
                            echo.slot(),
                            // Only a Complete echo says the finished form: a
                            // Fault's `Downloading … failed` is honest. A title
                            // with no past tense is `None` inside: it keeps its
                            // words, and the time slot says `done`.
                            (echo.kind == EchoKind::Complete).then(|| echo.msg.finished_words()),
                        )
                    }
                };
            inputs.push(RowIn {
                kind,
                msg,
                ind: indicator,
                load: (load, load_slot),
            });
            echo_words.push(finished);
        }
        let lay = LayIn {
            plus,
            links,
            cols,
            width,
            home,
        };
        let laid = lay_band(&inputs, &lay);
        let mut rows = Vec::with_capacity(laid.len());
        for (mut row, finished) in laid.into_iter().zip(echo_words) {
            match finished {
                Some(Some(words)) => {
                    finish_title(&mut row, &words, cols, width);
                    // A Complete echo is ✓ and the finished words only: its
                    // percent cells stay, blank (`✓ Pasted 4.2 MB`, never
                    // `100% done`).
                    row.pct = None;
                }
                // A title with no past tense: the time slot says `done`
                // (`echo_motion`), so the percent blanks all the same; a row
                // with no time slot keeps its `100%`, the one word left that
                // says the work finished.
                Some(None) if row.eta.is_some() || row.elapsed.is_some() => row.pct = None,
                Some(None) | None => {}
            }
            rows.push(row);
        }
        if let Some(hidden) = self.overflow_hidden() {
            let progress = self.hidden_progress_words();
            let words = overflow_words(hidden, links, progress.as_deref());
            rows.push(layout_row(&overflow_spec(hidden, &words), cols, width));
        }
        Presentation { cols, rows }
    }

    /// The committed live rows, top to bottom (the echoes and the overflow
    /// slot excluded).
    pub fn on_glass(&self) -> impl Iterator<Item = &Live> {
        self.glass.iter().map(|id| self.by_id(*id))
    }

    /// Whether a committed row is busy ([`Meter::busy`]), what the host asks
    /// before it animates a frame or arms a frame's wake. `false` with nothing
    /// committed and for a busy row still queued. Busy never arms a deadline
    /// here and never changes the rows the band wants.
    #[must_use]
    pub fn busy_on_glass(&self) -> bool {
        self.on_glass()
            .any(|l| l.msg.meter.as_ref().is_some_and(|m| m.busy))
    }

    /// Eligible rows not on glass (the `N more`).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.eligible().saturating_sub(self.glass.len())
    }

    /// Whether a new live row can be added without exceeding the live-row
    /// cap. This is a conservative admission snapshot, not a priority test:
    /// a full center may still accept a post by evicting another row. Hosts
    /// retaining an owed row can wait for capacity rather than repeatedly
    /// minting a newcomer that a protected queue immediately evicts.
    #[must_use]
    pub fn has_live_capacity(&self) -> bool {
        self.live.len() < MAX_LIVE
    }

    /// Every unretired row, post order (unrevealed rows included: the host
    /// restates and resolves them as any other).
    pub fn live_rows(&self) -> impl Iterator<Item = &Live> {
        self.live.iter()
    }

    /// One live row.
    #[must_use]
    pub fn live(&self, id: MessageId) -> Option<&Live> {
        self.live.iter().find(|l| l.id == id)
    }

    /// The live row with this supersede key.
    #[must_use]
    pub fn live_by_key(&self, key: &str) -> Option<&Live> {
        self.live.iter().find(|l| l.msg.key.as_deref() == Some(key))
    }

    /// The log.
    #[must_use]
    pub fn log(&self) -> &MessageLog {
        &self.log
    }

    /// The lines the host appends, oldest first; a trailing `Dropped` when
    /// the pending queue overflowed since the last drain.
    #[cfg(test)]
    pub(crate) fn drain_new_for_persist(&mut self) -> Vec<LogLine> {
        self.log.drain_pending()
    }

    /// The lines the host appends, oldest first, each with its file
    /// ([`crate::Shelf`]); a trailing `Dropped` when the pending queue
    /// overflowed since the last drain.
    pub fn drain_shelved_for_persist(&mut self) -> Vec<(LogLine, crate::Shelf)> {
        self.log.drain_shelved()
    }

    /// Bumps on every observable change — the Settings projection gate.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// A center that replaces another (the web's `messages_restore`, a fresh
    /// center over the kept log) moves its revision past the one it replaces,
    /// or a reader gating on the revision (the page's `rev=`) would read the
    /// same number over a log that went from nothing to N records.
    pub fn follow_revision(&mut self, replaced: u64) {
        self.revision = self.revision.max(replaced.saturating_add(1));
    }

    /// The gate a page reads to know it is out of date: the center's
    /// [`Self::revision`] plus the digest's ([`Self::digest_revision`]), which
    /// moves for an exception and for nothing the band draws. Both only grow,
    /// so the sum grows when either does. A host republishes Settings when it
    /// moves and repaints no window for a digest change.
    #[must_use]
    pub fn projection_revision(&self) -> u64 {
        self.revision.wrapping_add(self.digest.revision())
    }

    /// Changes on every live restatement, including a visually identical
    /// progress heartbeat that may re-anchor a future ETA tick.
    #[must_use]
    pub fn motion_input_epoch(&self) -> u64 {
        self.motion_input_epoch
    }

    /// The repaint-key term: `0` when `committed_rows() == 0`, else a nonzero
    /// FNV-1a over everything a presentation at `cols` depends on: the fill
    /// quantized to whole percent, the excerpt only when painted, the
    /// activity, whether an ETA slot is reserved, the load words shown, and
    /// the echoes. Nothing that moves is hashed: the motion layer has its own
    /// term.
    #[must_use]
    pub fn fingerprint(&self, cols: usize) -> u64 {
        if self.committed_rows == 0 {
            return 0;
        }
        let mut h = Fnv::new();
        h.num(cols as u64);
        h.num(u64::from(self.committed_rows));
        for row in self.on_glass() {
            h.num(row.id.raw());
            h.num(u64::from(row.revision));
            h.byte(row.msg.severity as u8);
            h.byte(u8::from(matches!(
                row.msg.hold,
                Hold::Live { .. } | Hold::Standing(_)
            )));
            h.str(row.msg.glyph.ch().encode_utf8(&mut [0; 4]));
            h.str(&row.msg.title);
            h.str(
                row.msg
                    .detail
                    .first()
                    .filter(|_| row.msg.excerpt)
                    .map_or("", String::as_str),
            );
            h.byte(row.activity());
            match &row.msg.meter {
                Some(m) => {
                    h.byte(1);
                    h.num(m.fill_permille.map_or(u64::MAX, |p| u64::from(p / 10)));
                    h.str(&m.stats);
                    h.byte(u8::from(m.amount.is_some()));
                    h.byte(u8::from(m.busy));
                }
                None => h.byte(0),
            }
            h.str(row.said_load().map_or("", Load::words));
            h.byte(row.load_slot.bits());
            for a in &row.msg.actions {
                h.str(a.label());
            }
        }
        for e in &self.echoes {
            h.num(u64::from(e.slot));
            h.num(e.id.raw());
            h.byte(e.kind as u8);
        }
        h.num(self.overflow_hidden().map_or(0, |n| n as u64));
        // The overflow row names a hidden progress row by its title and
        // percent: its words are painted, so they are hashed.
        if self.overflow_up() {
            h.str(self.hidden_progress_words().as_deref().unwrap_or(""));
        }
        h.num(self.eligible() as u64);
        h.finish()
    }

    // ---- motion ------------------------------------------------------

    /// The frame instant for `now`: `born + ⌊(now − born)/ANIM_FRAME⌋·ANIM_FRAME`
    /// — the one 30 fps grid every phase is read on.
    #[must_use]
    pub fn frame_instant(&self, now: Instant) -> Instant {
        let since = now.saturating_duration_since(self.born).as_millis();
        let frame = ANIM_FRAME.as_millis().max(1);
        self.born + Duration::from_millis(u64::try_from(since / frame * frame).unwrap_or(0))
    }

    /// The first grid instant at or after `d`, and after `now`, so the frame
    /// computed at the wake is exactly the frame the deadline promised. The
    /// wire's paced paint lands here too.
    pub(crate) fn grid_after(&self, now: Instant, d: Instant) -> Instant {
        let d = d.max(now);
        let since = d.saturating_duration_since(self.born).as_millis();
        let frame = ANIM_FRAME.as_millis().max(1);
        let snapped = self.born
            + Duration::from_millis(u64::try_from(since.div_ceil(frame) * frame).unwrap_or(0));
        if snapped <= now {
            snapped + ANIM_FRAME
        } else {
            snapped
        }
    }

    /// The fine frame instant for `now`: `born + ⌊(now − born)/refresh⌋·refresh`,
    /// the display's own cadence, which a glide and an echo are read on while
    /// in flight.
    #[must_use]
    pub fn fine_instant(&self, now: Instant) -> Instant {
        let since = now.saturating_duration_since(self.born).as_micros();
        let period = self.refresh.as_micros().max(1);
        self.born + Duration::from_micros(u64::try_from(since / period * period).unwrap_or(0))
    }

    /// The first fine instant at or after `d`, and after `now`.
    fn fine_after(&self, now: Instant, d: Instant) -> Instant {
        let d = d.max(now);
        let since = d.saturating_duration_since(self.born).as_micros();
        let period = self.refresh.as_micros().max(1);
        let snapped = self.born
            + Duration::from_micros(u64::try_from(since.div_ceil(period) * period).unwrap_or(0));
        if snapped <= now {
            snapped + self.refresh
        } else {
            snapped
        }
    }

    /// The first fine frame after `qf` at which `row`'s gliding bar draws
    /// something new (its cells, or its edge by a sixteenth of a cell; the
    /// host rasters the edge to the pixel), the landing frame included;
    /// `None` when not even the landing draws anything new.
    fn next_glide_frame(
        &self,
        row: &Live,
        (data, epoch): (u16, Instant),
        (qf, end): (Instant, Instant),
        look: Look,
        cols: usize,
    ) -> Option<Instant> {
        let sig = |g: Instant| {
            let s = moving_bar_at(row, data, (g, g), epoch, look.graded, cols);
            let edge = s
                .edge
                .map(|e| u64::from(e) * cols as u64 * 16 / u64::from(ROW));
            (s.cells(cols), edge)
        };
        let from = sig(qf);
        let landing = self.fine_after(qf, end);
        let mut g = self.fine_after(qf, qf + self.refresh);
        while g <= landing {
            if sig(g) != from {
                return Some(g);
            }
            g += self.refresh;
        }
        None
    }

    /// Whether the presentation `p` has anything read on the fine cadence at
    /// `qf` in `look`: a glide in flight, or a moving echo.
    fn fine_in_flight(&self, p: &Presentation, qf: Instant, look: Look) -> bool {
        look.pace == Pace::Moving
            && p.rows.iter().any(|layout| match layout.kind {
                RowKind::Message(id) => self
                    .live(id)
                    .is_some_and(|r| r.is_animated() && r.glide_end(qf).is_some()),
                RowKind::Echo(id) => self.echoes.iter().any(|e| e.id == id && qf < e.until),
                RowKind::Overflow { .. } => false,
            })
    }

    /// One frame of motion for the presentation `p` at `now` in `look`: every
    /// phase read at the frame instant (a glide, the glint of a bar mid-glide
    /// and an echo at the fine instant, [`Self::fine_instant`]), one
    /// [`RowMotion`] per row of `p` (default for a row that does not move).
    /// The frame's `at` is the later of the two instants.
    #[must_use]
    pub fn motion(&self, p: &Presentation, now: Instant, look: Look) -> BandMotion {
        let q = self.frame_instant(now);
        // A glide and an echo read the fine instant, everything else the
        // grid's; the frame's instant is the later of the two, so reading the
        // frame at it gives the same frame.
        let qf = self.fine_instant(now);
        let rows = p
            .rows
            .iter()
            .map(|layout| self.row_motion(layout, q, qf, look, p.cols))
            .collect();
        // The frame's instant: the grid's while nothing on the fine cadence is
        // in flight (at `qf`, or at the grid instant's own fine instant), else
        // the later of the two; reading the frame at it gives the same frame
        // either way.
        let settled = !self.fine_in_flight(p, qf, look)
            && !self.fine_in_flight(p, self.fine_instant(q), look);
        BandMotion {
            at: if settled { q } else { q.max(qf) },
            rows,
        }
    }

    fn row_motion(
        &self,
        layout: &RowLayout,
        q: Instant,
        qf: Instant,
        look: Look,
        cols: usize,
    ) -> RowMotion {
        match layout.kind {
            RowKind::Message(id) => match self.live(id) {
                Some(row) if row.is_animated() => live_motion(row, layout, (q, qf), look, cols),
                // A still fill (a held row's): the bar at its data.
                Some(_) => match layout.meter {
                    Some((_, _, shown)) => RowMotion {
                        surface: bar(shown, None, look.graded),
                        anim: Anim::Bar {
                            shown,
                            glint_ms: None,
                        },
                        ..RowMotion::default()
                    },
                    None => RowMotion::default(),
                },
                None => RowMotion::default(),
            },
            RowKind::Echo(id) => match self.echoes.iter().find(|e| e.id == id) {
                Some(e) => echo_motion(e, layout, qf, look, cols),
                None => RowMotion::default(),
            },
            RowKind::Overflow { .. } => RowMotion::default(),
        }
    }

    /// The next instant the band needs a frame for `p` in `look`, folded over
    /// the moving rows and echoes on it (`None` when there are none). Text
    /// ticks (an elapsed boundary, the ETA's next word or its running out, a
    /// stall's onset in the ETA slot or, where the width starved the slot, the
    /// bar's dim ink) arm in every look; frames for a comet, a glide, a
    /// travelling glint and a graded echo only while Moving, and a bar's only
    /// where the frame draws something new at cell resolution
    /// ([`Surface::cells`]). A resting glint arms its next start, a stalled
    /// bar's nothing. Instants snap up to the frame grid, or to the display's
    /// cadence for a glide or echo in flight. This is the band's only clock.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "one fold over every row kind's next change, each arm read against its motion recipe"
    )]
    pub fn motion_deadline(&self, p: &Presentation, now: Instant, look: Look) -> Option<Instant> {
        let q = self.frame_instant(now);
        let qf = self.fine_instant(now);
        let next_frame = q + ANIM_FRAME;
        let moving = look.pace == Pace::Moving;
        let cols = p.cols;
        let mut best: Option<Instant> = None;
        // A glide or an echo in flight asks the display's own cadence: the
        // next fine instant.
        let fine_next = self.fine_after(now, qf + self.refresh);
        let mut fold = |d: Instant, fine: bool| {
            let d = if fine { d } else { self.grid_after(now, d) };
            best = Some(best.map_or(d, |b| b.min(d)));
        };
        for layout in &p.rows {
            match layout.kind {
                RowKind::Message(id) => {
                    let Some(row) = self.live(id).filter(|r| r.is_animated()) else {
                        continue;
                    };
                    let epoch = row.motion_since.unwrap_or(q);
                    for d in text_ticks(row, layout, q, look).into_iter().flatten() {
                        fold(d, false);
                    }
                    if !moving {
                        continue;
                    }
                    if layout.track.is_some() {
                        // The comet never rests (the next enters as the last
                        // one leaves).
                        fold(
                            if q < epoch {
                                epoch
                            } else {
                                next_comet_frame(q, epoch, cols, look.graded)
                            },
                            false,
                        );
                        continue;
                    }
                    let Some((_, _, data)) = layout.meter else {
                        continue;
                    };
                    let stalled = matches!(row.track.eta(q), Eta::Stalled);
                    if !stalled
                        && let Some(end) = row.glide_end(qf)
                        && let Some(g) =
                            self.next_glide_frame(row, (data, epoch), (qf, end), look, cols)
                    {
                        // In flight: the next display frame that draws
                        // something new, the landing included.
                        fold(g, true);
                        continue;
                    }
                    // A travelling glint moves the bar until `busy_until`; a
                    // stalled bar is parked.
                    let glide_end = row.glide_end(q);
                    let glints = look.graded && !stalled;
                    let travel_end = glint_at(q, epoch)
                        .filter(|_| glints)
                        .and_then(|phase| q.checked_sub(phase))
                        .map(|start| start + GLINT_TRAVEL);
                    let busy_until = glide_end.into_iter().chain(travel_end).max();
                    // A frame is asked only where it draws something new at the
                    // row's cells (a short bar's glint can sit off the fill, a
                    // small glide inside one tone step); each scan stops at the
                    // first frame that differs. A frame's signature: its cells
                    // and its edge to a sixteenth of a cell, the glide read on
                    // the fine instant under `g`.
                    let sig = |g: Instant, gf: Instant| {
                        let s = moving_bar_at(row, data, (g, gf), epoch, look.graded, cols);
                        let edge = s
                            .edge
                            .map(|e| u64::from(e) * cols as u64 * 16 / u64::from(ROW));
                        (s.cells(cols), edge)
                    };
                    let drawn = sig(q, qf);
                    let first_change = |from: Instant, until: Instant| {
                        let mut g = self.grid_after(now, from);
                        while g <= until {
                            if sig(g, self.fine_instant(g)) != drawn {
                                return Some(g);
                            }
                            g += ANIM_FRAME;
                        }
                        None
                    };
                    // Through the glide and the travel, and one frame past
                    // them: the frame that draws the rest.
                    let changed =
                        busy_until.and_then(|end| first_change(next_frame, end + ANIM_FRAME));
                    match changed {
                        Some(g) => fold(g, false),
                        None if glints => {
                            // At rest: the next travel's first frame that
                            // shows its glint, not its start, where the centre
                            // is still off the fill. A fill under half the
                            // glint's radius carries none.
                            let rest = busy_until.map_or(q, |e| e.max(q));
                            let shown = row.shown_fill(rest).unwrap_or(data);
                            let start = next_glint_start(rest, epoch);
                            if bar_glints(shown)
                                && let Some(g) = first_change(start, start + GLINT_TRAVEL)
                            {
                                fold(g, false);
                            }
                        }
                        None => {}
                    }
                }
                RowKind::Echo(id) => {
                    let Some(e) = self.echoes.iter().find(|e| e.id == id) else {
                        continue;
                    };
                    if !moving {
                        continue;
                    }
                    let t = qf.saturating_duration_since(e.started);
                    let wiping = e.kind == EchoKind::Complete
                        && t < glide_span(if e.indeterminate { 0 } else { e.from_permille }, 1000);
                    // A graded Fault holds still between its cross (a bar's
                    // wash, a busy row's drain) and its fade: no frame there
                    // draws anything new, so the wake is the first display
                    // frame of the fade.
                    let settled = if e.indeterminate {
                        ECHO_DRAIN
                    } else {
                        ECHO_FAULT_CROSS
                    };
                    let holding = e.kind == EchoKind::Fault
                        && look.graded
                        && t >= settled
                        && t < ECHO_FAULT_FLASH;
                    if holding {
                        // The fade eases in: its first visible step is a
                        // frame or two past the flash.
                        let mut fade_at = self.fine_after(
                            now,
                            e.started + ECHO_FAULT_FLASH + Duration::from_micros(1),
                        );
                        while fade_at < e.until
                            && echo(e, fade_at.saturating_duration_since(e.started), look, cols).1
                                == 0
                        {
                            fade_at += self.refresh;
                        }
                        fold(fade_at, true);
                    } else if (look.graded || wiping) && qf < e.until {
                        fold(fine_next, true);
                    }
                }
                RowKind::Overflow { .. } => {}
            }
        }
        best
    }

    // ---- handoff -----------------------------------------------------

    /// Plain data for the successor: the next id and every live row (glass
    /// rows first, in order). No log lines: the parent's record is its own to
    /// the end (it flushes its writer before it execs), and the successor's
    /// begins at Commit. Not carried: an unrevealed row (its work ends with
    /// this process), the echoes, the estimator, the load and the motion epoch.
    #[must_use]
    pub fn carried(&self) -> Carry {
        let posted = |l: &Live| self.log.get(l.id).map(|rec| rec.posted_severity);
        let mut live: Vec<CarriedMessage> = self
            .on_glass()
            .map(|l| carried_message(l, true, posted(l)))
            .collect();
        live.extend(
            self.live
                .iter()
                .filter(|l| l.revealed && !self.glass.contains(&l.id))
                .map(|l| carried_message(l, false, posted(l))),
        );
        Carry {
            next_id: self.log.next_id().raw(),
            live,
        }
    }

    /// The rows a handoff did not carry end with the parent: at Commit, right
    /// before its last write, the parent retires every live row whose id is not
    /// in `carried` (the carry it handed over at the park, [`Self::carried`]):
    /// an unrevealed row, a row posted after the park. Each
    /// retires [`Retired::Withdrawn`] with no echo, so its record reads as ended
    /// rather than replayed open in the successor (`still open when aterm
    /// stopped`) or doubled beside the reporter's re-post there. A carried row
    /// stays open: it is the successor's now. Returns how many rows it closed.
    pub fn close_uncarried(&mut self, carried: &[u64], now: Instant) -> usize {
        let mut closed = 0;
        let mut i = 0;
        while i < self.live.len() {
            if carried.contains(&self.live[i].id.raw()) {
                i += 1;
            } else {
                self.retire_at_with(i, Retired::Withdrawn, None, now);
                closed += 1;
            }
        }
        if closed > 0 {
            self.rebalance(now);
        }
        closed
    }

    /// Never mint an id below `raw` from now on (ids only rise; a `raw` at or
    /// past the id ceiling is ignored). A seamless successor leaves its still
    /// running parent a band of ids to record in until Commit (the host's
    /// `PARKED_PARENT_ID_RESERVE`); raised before [`Self::seed_carried`], so
    /// the fresh id of a record the seed moves off a carried number is above
    /// the band too.
    pub fn raise_next_id(&mut self, raw: u64) {
        self.log.raise_to(raw);
    }

    /// Re-seed the carry: each row keeps its id, words, hold and duplicate
    /// count (a wire row stays the wire's) and takes the handoff cap
    /// ([`STALE_HANDOFF`]) in place of its anchors until
    /// [`MessageCenter::after_handoff_commit`]; unknown severities, tags and
    /// intents are dropped; `next_id` is raised. A seeded row is revealed and
    /// keeps its fill or `busy` (an older parent's carry has no `busy` and reads
    /// as still). `wall` is the host's wall clock at `now`: the row's life here
    /// is measured from it, not from the parent's ingress stamp.
    pub fn seed_carried(&mut self, carry: &Carry, wall: WallStamp, now: Instant) {
        // First, so an id the log mints while adopting (a squatter moved off
        // a carried number) is above every carried one.
        self.log.raise_to(carry.next_id);
        for c in &carry.live {
            let (Some(id), Some(severity), Ok(tag), Some(hold)) = (
                MessageId::from_raw(c.id),
                Severity::parse(&c.severity),
                Tag::try_new(&c.tag),
                Hold::decode(&c.hold),
            ) else {
                continue;
            };
            if hold == Hold::LogOnly || self.index(id).is_some() {
                continue;
            }
            let meter =
                (c.fill_permille.is_some() || !c.stats.is_empty() || c.busy).then(|| Meter {
                    fill_permille: c.fill_permille,
                    stats: c.stats.clone(),
                    busy: c.busy,
                    ..Meter::default()
                });
            let msg = Message {
                tag,
                severity,
                glyph: Glyph::or_fallback(c.glyph),
                title: c.title.clone(),
                short_title: c.short_title.clone(),
                detail: c.detail.clone(),
                actions: c.actions.iter().filter_map(|a| Intent::decode(a)).collect(),
                hold,
                meter,
                key: c.key.clone(),
                origin: crate::model::Origin::Carried,
                excerpt: c.excerpt,
                reveal_after: None,
                finished: c.finished.clone(),
                loads: Loads::NONE,
                primary: None,
                retrospective: c.retrospective,
                need: crate::need::Need::Unclassified,
            }
            .normalized();
            // The door reads a carried persistent row as adopted: its parent
            // admitted it, and its reporter re-asserts it at Commit. It runs
            // while the origin is still `Carried` (what it keys on).
            let mut msg = msg;
            let _ = msg.admit();
            // A wire row stays the wire's: a keyed one by its `wire.` key, a
            // keyless one by keeping its Wire origin, so its lines stay in the
            // wire's file, it counts against the wire's cap and it never folds
            // into a host row.
            let msg = if c.wire && !msg.origin.wire_owned(msg.key.as_deref()) {
                Message {
                    origin: crate::model::Origin::Wire,
                    ..msg
                }
            } else {
                msg
            };
            let stamp = WallStamp { unix_ms: c.unix_ms };
            let mut entry = Live::new(id, msg, stamp, now);
            entry.repeats = c.repeats.max(1);
            entry.wall_anchor = wall.unix_ms.max(stamp.unix_ms);
            entry.fold_at = None;
            entry.stale_at = Some(at(now, STALE_HANDOFF));
            entry.carried_pending = true;
            let mut record = LogRecord::from_posted(id, stamp, &entry.msg);
            record.posted_severity = c
                .posted_severity
                .as_deref()
                .and_then(Severity::parse)
                .unwrap_or(severity);
            self.log.adopt_carried(record);
            // The record may be the file's own Posted line, under the mark it
            // was posted with: that is what the Retired line is weighed
            // against, and the ring reads the mark the row shows.
            if let Some(rec) = self.log.get(id) {
                entry.logged_mark = (rec.severity, rec.glyph);
            }
            self.log
                .show_mark(id, (entry.msg.severity, entry.msg.glyph));
            if c.on_glass {
                entry.on_glass_since = Some(now);
                entry.motion_since = Some(now);
                self.glass.push(id);
            }
            self.live.push(entry);
        }
        self.log.raise_to(carry.next_id);
        self.committed_rows = self.committed_rows.max(self.wanted_rows());
        self.revision += 1;
        self.rebalance(now);
    }

    /// Re-read the parent's record after the handoff committed: `disk` is the
    /// log loaded from the file once the parent (flushed before Commit) is
    /// gone. What the parent recorded after this process's boot load, and the
    /// true ending of each row the boot read as `Stale`, merge in
    /// ([`MessageLog::merge_reloaded`]); a row live here keeps its record.
    pub fn merge_reloaded_log(&mut self, disk: &MessageLog) {
        self.log.merge_reloaded(disk);
        // A row its parent posted after this process's boot load was seeded
        // with the carried mark; take the file's, which its Retired line is
        // weighed against.
        for row in &mut self.live {
            if let Some(mark) = disk.open_posted_mark(row.id, row.stamp, &row.msg.tag) {
                row.logged_mark = mark;
            }
        }
        self.revision += 1;
    }

    /// At Commit, after [`Self::merge_reloaded_log`]: a carried row still in its
    /// carried words that its parent ended after the park's capture (a press or
    /// dismissal there, its reporter's resolve or withdrawal) ends here too;
    /// rows cross only in the carry captured at the park, while the parent runs
    /// on until Commit. It ends without a second log line, as an entry the
    /// parent filed meanwhile does ([`Self::catch_up`]): the parent's line is in
    /// the file, and the ring's record takes its ending
    /// ([`MessageLog::adopt_ending`]). Only those four endings: a fold, a quit
    /// or an eviction there is about the parent's glass, and a row this process
    /// restated since the seed is its own. `disk` is the file loaded again;
    /// returns the rows it ended with the parent's ending. It ends rows only: a
    /// host that keeps state beside a row decides it by its own record in
    /// `disk`, since the parent can change such state with no row left to end.
    pub fn end_carried_rows_the_parent_ended(
        &mut self,
        disk: &MessageLog,
        now: Instant,
    ) -> Vec<(Message, Retired)> {
        let ended_there = |how: &Retired| {
            matches!(
                how,
                Retired::Resolved(_)
                    | Retired::Dismissed
                    | Retired::Answered { .. }
                    | Retired::Withdrawn
            )
        };
        // The row's record by the number the parent wrote it under: another
        // writer of the shared file that posted under the same number first
        // has its record there, and the parent's moved to a fresh id
        // ([`MessageLog::record_of_file_id`]).
        let record = |row: &Live| disk.record_of_file_id(row.id, row.stamp, &row.msg.tag);
        let ended: Vec<MessageId> = self
            .live
            .iter()
            .filter(|row| row.carried_pending)
            .filter(|row| record(row).is_some_and(|rec| rec.retired().is_some_and(ended_there)))
            .map(|row| row.id)
            .collect();
        let mut gone = Vec::new();
        for id in ended {
            let Some(i) = self.live.iter().position(|row| row.id == id) else {
                continue;
            };
            let Some(rec) = record(&self.live[i]) else {
                continue;
            };
            let Some(how) = rec.retired().cloned() else {
                continue;
            };
            let row = self.live.remove(i);
            self.glass.retain(|g| *g != id);
            self.note_unfixed(id, &row.msg, &how, None);
            self.log.adopt_ending(id, rec);
            gone.push((row.msg, how));
        }
        if !gone.is_empty() {
            self.revision += 1;
            self.rebalance(now);
        }
        gone
    }

    /// The handoff committed: every carried row still in its carried words
    /// takes its own lifetime back (a Live row re-arms its cap, a Standing row
    /// stands, a held row on glass anchors its own hold now, a queued one waits
    /// unanchored under the patience clock) and stops animating
    /// ([`Meter::busy`] cleared): work that is not this process's own is shown
    /// still. A row restated before the commit already took its lifetime back
    /// and keeps what it said.
    pub fn after_handoff_commit(&mut self, now: Instant) {
        for row in &mut self.live {
            if !std::mem::take(&mut row.carried_pending) {
                continue;
            }
            if let Some(meter) = row.msg.meter.as_mut() {
                meter.busy = false;
            }
            row.stale_at = None;
            if row.on_glass_since.is_some() || matches!(row.msg.hold, Hold::Live { .. }) {
                row.arm(now);
            }
        }
        self.revision += 1;
    }
}

// ---- the digest ----------------------------------------------------------

/// The exceptions: the owner is needed, but not now ([`crate::need`]). Every
/// method here reads and writes [`MessageCenter::digest`] and the log, never
/// the live set's layout: none moves `revision`, `wanted_rows` or a hold.
impl MessageCenter {
    /// The open exceptions, worst first then longest waiting, for every
    /// observer to read (the Settings card, the menu-bar section, the
    /// palette, `aterm ctl messages need=later`).
    #[must_use]
    pub fn digest(&self) -> DigestView {
        crate::digest::view_of(&self.digest)
    }

    /// The keys of the open exceptions under `prefix` (a family's
    /// [`crate::exceptions::Family::prefix`]), without building the view: what a
    /// host that owns an event family reads to end the lines an earlier process
    /// raised and this one adopted.
    #[must_use]
    pub fn digest_keys_under(&self, prefix: &str) -> Vec<String> {
        (0..self.digest.len())
            .filter_map(|i| self.digest.get(i).msg.key.as_deref())
            .filter(|key| key.starts_with(prefix))
            .map(str::to_string)
            .collect()
    }

    /// One open exception by id, as every observer reads it.
    #[must_use]
    pub fn digest_entry(&self, id: MessageId) -> Option<crate::digest::DigestEntry> {
        self.digest
            .find_id(id)
            .map(|i| crate::digest::entry_view(self.digest.get(i)))
    }

    /// Moves only when an exception is added, changed or filed. A host
    /// republishes Settings and the status item on it, and repaints no window.
    #[must_use]
    pub fn digest_revision(&self) -> u64 {
        self.digest.revision()
    }

    /// How many messages the door demoted to a record: a persistent hold with
    /// no ask behind it (a carry that lost its need, a copied hold), or a
    /// transient one held past [`crate::need::MAX_TRANSIENT`]. Non-zero is a
    /// reporter to fix, and the words were kept in the log.
    #[must_use]
    pub fn demotions(&self) -> u32 {
        self.demotions
    }

    /// The key (else the title) of the message the door demoted last: the
    /// builder to fix. `None` while [`Self::demotions`] is zero.
    #[must_use]
    pub fn last_demoted(&self) -> Option<&str> {
        self.last_demoted.as_deref()
    }

    /// Post an exception. In order: a fact the owner filed inside its
    /// quiet span is suppressed (nothing shown, nothing logged); the same fact
    /// with the same words is a duplicate; with new words it is restated in
    /// place (same entry, same place in the order, no log line); otherwise it
    /// is a new entry, and past [`crate::need::DIGEST_CAP`] the oldest
    /// lowest-severity one files itself `Evicted` (with its log line).
    fn post_later(&mut self, msg: Message, stamp: WallStamp, now: Instant) -> Posted {
        let Need::Later(ask) = msg.need.clone() else {
            unreachable!("post_later is called with a Later");
        };
        match self.later_verdict(&msg, &ask, stamp, now) {
            LaterVerdict::Quiet(filed) => {
                return Posted {
                    id: filed,
                    outcome: PostOutcome::Suppressed,
                };
            }
            LaterVerdict::Repeat(i) => {
                let e = self.digest.get_mut(i);
                e.repeats = e.repeats.saturating_add(1);
                // An entry adopted after a restart carries a stand-in action (no
                // `Mover`); its reporter's next look brings the real one, and
                // the owner's press must file the entry `Answered` on it.
                e.ask.take_action_of(&ask);
                return Posted {
                    id: e.id,
                    outcome: PostOutcome::Duplicate,
                };
            }
            LaterVerdict::Restate(i) => {
                let e = self.digest.get_mut(i);
                let id = e.id;
                e.msg = msg;
                e.ask = ask;
                e.restated_at = now;
                self.digest.bump();
                return Posted {
                    id,
                    outcome: PostOutcome::Restated,
                };
            }
            LaterVerdict::New => {}
        }
        let id = self.log.mint();
        self.log
            .record_posted(LogRecord::from_posted(id, stamp, &msg));
        // One fact, one place: an exception that takes over a key retires the
        // live row that told it.
        if let Some(i) = msg.key.as_deref().and_then(|k| self.index_by_key(k)) {
            self.retire_at(i, Retired::Superseded { by: id }, now);
            self.rebalance(now);
        }
        self.digest.push(DigestItem {
            id,
            msg,
            ask,
            stamp,
            posted_at: now,
            anchor_wall: stamp.unix_ms,
            repeats: 1,
            missed_at: None,
            restated_at: now,
        });
        while let Some(v) = self.digest.overflow_victim(id) {
            self.file_digest_at(v, Retired::Evicted, stamp.unix_ms, now);
        }
        Posted {
            id,
            outcome: PostOutcome::New,
        }
    }

    /// What posting the exception `msg` (whose ask is `ask`) at `stamp` would do, decided
    /// without doing it: [`Self::post_later`] acts on it, and the wire reads it first
    /// ([`Self::later_outcome`]) to spend its budgets and caps only on what a post opens or
    /// changes.
    fn later_verdict(
        &self,
        msg: &Message,
        ask: &LaterAsk,
        stamp: WallStamp,
        now: Instant,
    ) -> LaterVerdict {
        let key = ask.fingerprint().as_str();
        if let Some(filed) = self.quiet_record(key, ask, stamp) {
            return LaterVerdict::Quiet(filed);
        }
        let Some(i) = self.digest.find_key(key) else {
            return LaterVerdict::New;
        };
        let e = self.digest.get(i);
        let same = e.msg.title == msg.title
            && e.msg.detail == msg.detail
            && e.msg.actions == msg.actions
            && e.msg.severity == msg.severity;
        // Words that move every look (a holder count, an age) restate an
        // entry at most once per RESTATE_QUIET; a new severity or capsule
        // is news at once, and so is a new step (the one thing to do: an
        // entry never tells the owner to do what no longer helps), and
        // words that move only with their fact
        // (`LaterAsk::restated_at_once`).
        let words_only = e.msg.severity == msg.severity
            && e.msg.actions == msg.actions
            && e.ask.then().words() == ask.then().words()
            && !ask.restates_at_once();
        if same || (words_only && now.saturating_duration_since(e.restated_at) < RESTATE_QUIET) {
            LaterVerdict::Repeat(i)
        } else {
            LaterVerdict::Restate(i)
        }
    }

    /// How [`Self::post`] would land the exception `msg` (normalized, as `post` makes it),
    /// without posting it: `New`, `Restated`, `Duplicate` or `Suppressed`. `None` for a
    /// message that is not an exception.
    pub(crate) fn later_outcome(
        &self,
        msg: &Message,
        stamp: WallStamp,
        now: Instant,
    ) -> Option<PostOutcome> {
        let Need::Later(ask) = &msg.need else {
            return None;
        };
        Some(match self.later_verdict(msg, ask, stamp, now) {
            LaterVerdict::Quiet(_) => PostOutcome::Suppressed,
            LaterVerdict::Repeat(_) => PostOutcome::Duplicate,
            LaterVerdict::Restate(_) => PostOutcome::Restated,
            LaterVerdict::New => PostOutcome::New,
        })
    }

    /// How many exceptions are open.
    pub(crate) fn digest_len(&self) -> usize {
        self.digest.len()
    }

    fn index_by_key(&self, key: &str) -> Option<usize> {
        self.live
            .iter()
            .position(|l| l.msg.key.as_deref() == Some(key))
    }

    /// The record that keeps `key` quiet at `stamp`: the latest word on the
    /// fact by retirement time, not ring order (`MessageLog::last_word`), among
    /// the digest's own records for the key (a `Record` or a row sharing the
    /// key is another fact). Filed by the owner (`Dismissed`, `Answered`), aged
    /// out (`Unseen`) or capped (`Evicted`) inside the ask's quiet span, which
    /// backs off with each dismissal inside [`crate::need::FILED_KEEP`]
    /// ([`crate::need::Resurface::backed_off`]). Read from the ring, which is
    /// replayed from disk and keeps a filing for `FILED_KEEP` whatever else is
    /// posted, so a dismissal holds across a restart, an update and any number
    /// of unrelated records.
    fn quiet_record(&self, key: &str, ask: &LaterAsk, stamp: WallStamp) -> Option<MessageId> {
        let last = self.log.last_word(key, true)?;
        if !last.filed_exception() {
            return None;
        }
        // The span is fixed by the filing that earned it: the filings in the
        // FILED_KEEP before that filing, not before now (which would shrink it
        // as older filings age out mid-span).
        let filed_at = last.retired_unix_ms?;
        if filed_at > stamp.unix_ms.saturating_add(FILING_SKEW_MS) {
            return None;
        }
        let n = self
            .log
            .filings_since(key, filed_at.saturating_sub(FILED_KEEP_MS))
            .max(1);
        let until = filed_at.saturating_add(ask.resurface().backed_off(n));
        (until > stamp.unix_ms).then_some(last.id)
    }

    /// Whether the owner's own Dismiss of the exception `key` still keeps it quiet at
    /// `stamp`, over the span `resurface` backs off to ([`Self::quiet_record`]'s
    /// arithmetic). An answer, a fold nobody saw or an eviction is no decline. A host
    /// reads it as the owner's "not now" to what the exception asks. Read-only.
    #[must_use]
    pub fn dismissed_quiet(&self, key: &str, resurface: Resurface, stamp: WallStamp) -> bool {
        let Some(last) = self.log.last_word(key, true) else {
            return false;
        };
        if !(last.filed_exception() && matches!(last.retired(), Some(Retired::Dismissed))) {
            return false;
        }
        let Some(filed_at) = last.retired_unix_ms else {
            return false;
        };
        if filed_at > stamp.unix_ms.saturating_add(FILING_SKEW_MS) {
            return false;
        }
        let n = self
            .log
            .filings_since(key, filed_at.saturating_sub(FILED_KEEP_MS))
            .max(1);
        filed_at.saturating_add(resurface.backed_off(n)) > stamp.unix_ms
    }

    /// The record that keeps a standing row's `key` quiet at `stamp`: the
    /// latest word on the key (by retirement time, as [`Self::quiet_record`])
    /// is a read (`Folded`) or an answer/dismissal inside
    /// [`crate::need::STANDING_QUIET`]. A row that was resolved or withdrawn
    /// (its cause ended) is news if it returns.
    fn quiet_standing(&self, key: &str, stamp: WallStamp) -> Option<MessageId> {
        let last = self
            .log
            .records()
            .filter(|r| r.key.as_deref() == Some(key) && !r.is_live())
            .max_by_key(|r| (r.retired_unix_ms.unwrap_or(0), r.id))?;
        let filed = matches!(
            last.retired(),
            Some(Retired::Folded | Retired::Dismissed | Retired::Answered { .. })
        );
        let span = u64::try_from(crate::need::STANDING_QUIET.as_millis()).unwrap_or(u64::MAX);
        let filed_at = last.retired_unix_ms?;
        if filed_at > stamp.unix_ms.saturating_add(FILING_SKEW_MS) {
            return None;
        }
        let until = filed_at.saturating_add(span);
        (filed && until > stamp.unix_ms).then_some(last.id)
    }

    /// File entry `i` under `how`, with its final words, at wall `unix_ms`.
    fn file_digest_at(&mut self, i: usize, how: Retired, unix_ms: u64, now: Instant) {
        let e = self.digest.remove(i);
        self.log.record_retired(
            e.id,
            how,
            unix_ms,
            now,
            FinalWords {
                title: &e.msg.title,
                detail: &e.msg.detail,
                repeats: e.repeats,
                mark: None,
            },
        );
    }

    /// Dismiss all: every open exception is filed `Dismissed` at one wall stamp
    /// (one press, one batch), each with its own log line and final words, and
    /// each fact is quiet for its ask's resurface span. Returns how many.
    pub fn file_digest(&mut self, stamp: WallStamp, now: Instant) -> usize {
        let n = self.digest.len();
        while self.digest.len() > 0 {
            self.file_digest_at(0, Retired::Dismissed, stamp.unix_ms, now);
        }
        n
    }

    /// Dismiss one exception. `false` when there is no such entry.
    pub fn file_digest_one(&mut self, id: MessageId, stamp: WallStamp, now: Instant) -> bool {
        let Some(i) = self.digest.find_id(id) else {
            return false;
        };
        self.file_digest_at(i, Retired::Dismissed, stamp.unix_ms, now);
        true
    }

    /// A capsule pressed on an exception: logs `Acted`, and a decline that
    /// closes its row (`Not now`) files the entry `Answered`. Returns the
    /// intent for the host to perform; `None` for `Details` or no such entry.
    pub fn act_digest(
        &mut self,
        id: MessageId,
        action: ActionIndex,
        stamp: WallStamp,
        now: Instant,
    ) -> Option<Intent> {
        let i = self.digest.find_id(id)?;
        let intent = self
            .digest
            .get(i)
            .msg
            .actions
            .get(usize::from(action.0))?
            .clone();
        self.log.record_acted(id, stamp.unix_ms, intent.label());
        // The owner did the entry's one action (its button) or declined it:
        // nothing is left to ask until the fact changes, so it is filed
        // `Answered`, not `Resolved` (the cause was not seen to end) and not
        // `Dismissed` (they acted). A navigation (`Show tab`) leaves it.
        let did_it = self
            .digest
            .get(i)
            .ask
            .then()
            .mover()
            .is_some_and(|m| *m.intent() == intent);
        if intent.closes_row() || did_it {
            self.file_digest_at(
                i,
                Retired::Answered {
                    label: intent.label().to_string(),
                },
                stamp.unix_ms,
                now,
            );
        }
        Some(intent)
    }

    /// The cause ended: retire every exception whose key starts with `prefix`
    /// `Resolved`. (The live set's own [`Self::resolve_key_prefix`] calls this
    /// too, so a reporter sweeping a family needs one call.) Returns how many.
    pub fn resolve_digest_prefix(&mut self, prefix: &str, outcome: Outcome, now: Instant) -> usize {
        let mut n = 0;
        loop {
            let hit = self
                .digest
                .iter()
                .position(|e| e.msg.key.as_deref().is_some_and(|k| k.starts_with(prefix)));
            let Some(i) = hit else { break };
            let wall = self.digest.get(i).wall_at(now);
            self.file_digest_at(i, Retired::Resolved(outcome), wall, now);
            n += 1;
        }
        n
    }

    /// The digest's own clock, called by the host's settle (`stamp` is the real
    /// wall clock): an entry a family pass stopped naming
    /// [`crate::need::RESOLVE_AFTER`] ago ends `Resolved`, and an open one files
    /// itself `Unseen` (the log's word for aged out) after
    /// [`crate::need::DIGEST_EXPIRY`] from its first ingress; the log keeps its
    /// words. `true` when any ended.
    pub fn settle_digest(&mut self, stamp: WallStamp, now: Instant) -> bool {
        let limit = u64::try_from(DIGEST_EXPIRY.as_millis()).unwrap_or(u64::MAX);
        let mut any = false;
        loop {
            let ended = self
                .digest
                .iter()
                .position(|e| e.ends_at().is_some_and(|t| now >= t));
            if let Some(i) = ended {
                self.file_digest_at(i, Retired::Resolved(Outcome::Ok), stamp.unix_ms, now);
                any = true;
                continue;
            }
            let aged = self
                .digest
                .iter()
                .position(|e| stamp.unix_ms.saturating_sub(e.stamp.unix_ms) >= limit);
            let Some(i) = aged else { break };
            self.file_digest_at(i, Retired::Unseen, stamp.unix_ms, now);
            any = true;
        }
        any
    }

    /// The next instant [`Self::settle_digest`] has something to end: the
    /// earliest unnamed entry's end. `None` with nothing pending, so an idle
    /// digest never wakes the loop. The 14-day expiry is not armed: settle and
    /// launch read it.
    #[must_use]
    pub fn digest_deadline(&self) -> Option<Instant> {
        self.digest.next_end()
    }

    /// A reporter's whole answer for one family of exceptions (level-triggered):
    /// `desired` is every exception that holds now under `prefix`. Each is
    /// posted (a new entry, a restatement, or the quiet a dismissal earned); an
    /// open entry of the family that `desired` no longer names is missed: its
    /// clock starts, and it ends `Resolved` once unnamed for
    /// [`crate::need::RESOLVE_AFTER`], at a later look or at
    /// [`Self::settle_digest`]. So a reporter cannot leave a stale entry by
    /// forgetting to end it, a flicker neither ends nor re-posts a fact, and a
    /// pass never touches another family. A family has one producer: the pass
    /// sweeps every entry under its prefix, so an event-raised sibling needs a
    /// prefix of its own ([`crate::exceptions::Family::mode`]).
    pub fn sync_digest(
        &mut self,
        prefix: &str,
        desired: Vec<Message>,
        stamp: WallStamp,
        now: Instant,
    ) -> DigestSync {
        self.sync_digest_keeping(prefix, desired, &[], stamp, now)
    }

    /// [`Self::sync_digest`], where the pass also names `kept`: keys of facts
    /// that still hold whose new words the host's door refused
    /// ([`crate::exceptions::Entry::message`]). An open entry under one stands
    /// in the words it was last shown in, and no clock starts for it: a wording
    /// fault is the reporter's to fix, never the end of a fact that still
    /// holds. A kept key with nothing open opens nothing.
    pub fn sync_digest_keeping(
        &mut self,
        prefix: &str,
        desired: Vec<Message>,
        kept: &[String],
        stamp: WallStamp,
        now: Instant,
    ) -> DigestSync {
        let mut out = DigestSync::default();
        let mut named: Vec<String> = kept
            .iter()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect();
        for key in &named {
            if let Some(i) = self.digest.find_key(key) {
                self.digest.get_mut(i).missed_at = None;
            }
        }
        for msg in desired {
            let key = msg.key.clone().unwrap_or_default();
            let ours = matches!(msg.need, Need::Later(_)) && key.starts_with(prefix);
            if !ours {
                // Not this family's exception: the door records it.
                self.demotions = self.demotions.saturating_add(1);
                continue;
            }
            named.push(key.clone());
            match self.post(msg, stamp, now).outcome {
                PostOutcome::New => {
                    out.posted += 1;
                    out.posted_keys.push(key.clone());
                }
                PostOutcome::Restated => out.restated += 1,
                PostOutcome::Suppressed => out.suppressed += 1,
                PostOutcome::Duplicate | PostOutcome::Superseded(_) => {}
            }
            if let Some(i) = self.digest.find_key(&key) {
                self.digest.get_mut(i).missed_at = None;
            }
        }
        let mut i = 0;
        while i < self.digest.len() {
            let key = self.digest.get(i).msg.key.clone().unwrap_or_default();
            if !key.starts_with(prefix) || named.contains(&key) {
                i += 1;
                continue;
            }
            let first = *self.digest.get_mut(i).missed_at.get_or_insert(now);
            if now.saturating_duration_since(first) >= RESOLVE_AFTER {
                let wall = self.digest.get(i).wall_at(now);
                self.file_digest_at(i, Retired::Resolved(Outcome::Ok), wall, now);
                out.resolved += 1;
            } else {
                out.pending_end += 1;
                i += 1;
            }
        }
        out
    }

    /// The cause ended and its reporter says so (a verified grant, a switch
    /// turned off): the entry under exactly `key` ends `Resolved` at once.
    /// `true` when there was one.
    pub fn clear_digest(&mut self, key: &str, now: Instant) -> bool {
        self.end_digest_key(key, Retired::Resolved(Outcome::Ok), None, now)
            .is_some()
    }

    /// The entry under exactly `key` ends `how` (an end: `Resolved` or `Withdrawn`, never
    /// a filing, so the fact's next post is news) at once, its record titled `words` when
    /// the reporter said how it ended (`notice done <key> ok <words>`). The id it had, or
    /// `None` with nothing open under the key.
    pub(crate) fn end_digest_key(
        &mut self,
        key: &str,
        how: Retired,
        words: Option<String>,
        now: Instant,
    ) -> Option<MessageId> {
        let i = self.digest.find_key(key)?;
        let e = self.digest.get_mut(i);
        let id = e.id;
        if let Some(words) = words {
            e.msg.title = words;
        }
        let wall = self.digest.get(i).wall_at(now);
        self.file_digest_at(i, how, wall, now);
        Some(id)
    }

    /// The cause ended and nothing is open under `key`: when the latest word on
    /// the key is a filing (answered, dismissed or aged out), it is rewritten
    /// `Resolved` at `wall`, so the fact's quiet span ([`Self::quiet_record`])
    /// ends with its cause and the next regression is news. A key whose latest
    /// word is already a resolution, or that has no word, is left alone. `true`
    /// when a filing was resolved.
    pub fn forget_filing(&mut self, key: &str, wall: WallStamp, now: Instant) -> bool {
        let changed =
            self.digest.find_key(key).is_none() && self.log.resolve_filing(key, wall.unix_ms, now);
        if changed {
            // The filed record's outcome changed, even though no digest entry moved.
            self.revision += 1;
        }
        changed
    }

    /// A standing row's fact is new and nothing is live under its `key`: when
    /// the latest word on the key keeps it quiet ([`Self::quiet_standing`]: the
    /// owner read, dismissed or answered an earlier occurrence inside
    /// [`crate::need::STANDING_QUIET`]), that word is rewritten `Withdrawn` at
    /// `wall` (a second `Retired` line, which replay applies in order), so the
    /// next post under the key is news: the standing row's counterpart of
    /// [`Self::forget_filing`]. For a reporter that knows the earlier occurrence
    /// ended, never for the one the owner read. A key with a live row, whose
    /// latest word keeps no quiet, or with no word, is left alone. `true` when
    /// a word was rewritten.
    pub fn end_standing_quiet(&mut self, key: &str, wall: WallStamp, now: Instant) -> bool {
        if self.index_by_key(key).is_some() {
            return false;
        }
        let Some(read) = self.quiet_standing(key, wall) else {
            return false;
        };
        self.log.withdraw_read(read, wall.unix_ms, now)
    }

    /// At Commit of a seamless update, in the successor: the lines the parent
    /// wrote while the two overlapped. The successor loaded the file at launch
    /// and the parent kept appending, so a dismissal the owner made in the
    /// parent after that load is in the file but not in this ring. `tail` is
    /// the file's tail read again, in file order (lines already in the ring
    /// change nothing): it is folded into the ring, the digest entries the
    /// parent filed meanwhile end here without a second log line, and the ones
    /// it opened are adopted. Ids the parent minted are raised past first, so
    /// none this process mints afterwards collides with them. Call it before
    /// the process's first digest sync.
    pub fn catch_up(
        &mut self,
        tail: impl IntoIterator<Item = LogLine>,
        wall: WallStamp,
        now: Instant,
    ) -> CatchUp {
        self.log.replay_all(tail);
        let mut out = CatchUp::default();
        let mut i = 0;
        while i < self.digest.len() {
            let id = self.digest.get(i).id;
            if self.log.get(id).is_some_and(|r| !r.is_live()) {
                // Filed by the parent: its line is already in the file.
                let _ = self.digest.remove(i);
                out.closed += 1;
            } else {
                i += 1;
            }
        }
        out.adopted = self.adopt_digest(wall, now);
        out
    }

    /// At launch: re-open the exceptions the replayed log left open (a
    /// `need=later` line with no retirement), each under its old id. Their
    /// reporters re-assert at their first look and replace the words in place;
    /// what they do not re-assert is swept by [`Self::resolve_digest_prefix`].
    /// Returns how many were adopted.
    pub fn adopt_digest(&mut self, wall: WallStamp, now: Instant) -> usize {
        let open: Vec<LogRecord> = self
            .log
            .records()
            .filter(|r| r.later && r.is_live())
            .cloned()
            .collect();
        let mut n = 0;
        for r in open {
            if self.digest.find_id(r.id).is_some() {
                continue;
            }
            let Some(key) = r.key.clone() else { continue };
            // A reporter that re-asserted first already owns the key.
            if self.digest.find_key(&key).is_some() {
                continue;
            }
            let mut msg = Message::new(r.tag.clone(), r.severity, &r.title);
            msg.detail.clone_from(&r.detail);
            msg.actions.clone_from(&r.actions);
            msg.key = Some(key.clone());
            msg.glyph = r.glyph;
            msg.hold = Hold::LogOnly;
            let why = r.detail.first().map_or("", String::as_str);
            let then = r.detail.get(1).map_or("", String::as_str);
            let ask = LaterAsk::adopted(why, then, &key, r.actions.first());
            msg.need = Need::Later(ask.clone());
            let msg = msg.normalized();
            self.digest.push(DigestItem {
                id: r.id,
                msg,
                ask,
                stamp: r.stamp,
                posted_at: now,
                // Adopted: the clock the entry's end is written with runs from
                // now, not from the old ingress stamp.
                anchor_wall: wall.unix_ms,
                repeats: r.repeats,
                missed_at: None,
                restated_at: now,
            });
            n += 1;
        }
        n
    }
}

/// What a supersede by key carries over from the row it replaces: the slot,
/// the reveal (a revealed row stays revealed; an unrevealed one keeps the
/// earlier clock, so Downloading → Verifying keeps its first grace), the
/// work's start and the motion epoch while the activity is the same, the
/// estimator while the series is, the load clock while a load is declared on
/// both sides, the reserved load slot, and a glide from the fill shown when the
/// fill moved.
fn inherit(entry: &mut Live, old: &Live, now: Instant) {
    entry.on_glass_since = old.on_glass_since;
    if old.revealed {
        entry.revealed = true;
        entry.reveal_at = None;
    } else if !entry.revealed {
        entry.reveal_at = match (old.reveal_at, entry.reveal_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        entry.reveal_cap = match (old.reveal_cap, entry.reveal_cap) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    if old.activity() == entry.activity() {
        entry.started_at = old.started_at;
    }
    if entry.on_glass_since.is_some() {
        entry.motion_since = if old.activity() == entry.activity() {
            old.motion_since
        } else {
            Some(now)
        };
    }
    if let Some(a) = entry.msg.meter.as_ref().and_then(|m| m.amount)
        && old.track.series() == Some(a.series)
    {
        entry.track = old.track.clone();
        entry.track.observe(now, a);
    }
    if let (Some((_, since)), Some((is, _))) = (old.load, entry.load) {
        entry.load = Some((is, since));
        entry.load_shown |= old.load_shown;
    }
    entry.load_slot = entry.load_slot.union(old.load_slot);
    if entry.on_glass_since.is_some()
        && let (Some(from), Some(to)) = (
            old.shown_fill(now),
            entry.msg.meter.as_ref().and_then(|m| m.fill_permille),
        )
        && from != to
    {
        entry.glide = Some((from, now));
    }
}

/// A restated meter: the estimator is fed (or cleared), a fill that moved
/// glides from the fill shown, and the load follows the phase — words that
/// are up stay up when only the resource changes (`network busy` → `disk
/// busy`: the machine stayed busy), leave the moment the load ends, and a
/// load declared after none waits [`LOAD_AFTER`]. The slot is reserved from
/// the first load on, so none of it re-grids the row.
fn restate_meter(row: &mut Live, meter: Option<Meter>, now: Instant) {
    let shown = row.shown_fill(now);
    let before = row.msg.meter.as_ref().and_then(|m| m.fill_permille);
    match meter.as_ref().and_then(|m| m.amount) {
        Some(a) => row.track.observe(now, a),
        None => row.track = ProgressTrack::default(),
    }
    let after = meter.as_ref().and_then(|m| m.fill_permille);
    if after.is_none() {
        row.glide = None;
    } else if after != before
        && let Some(from) = shown
    {
        row.glide = Some((from, now));
    }
    match (row.load, meter.as_ref().and_then(|m| m.load)) {
        (_, None) => {
            row.load = None;
            row.load_shown = false;
        }
        (None, Some(l)) => {
            row.load = Some((l, now));
            row.load_shown = false;
            row.load_slot = row.load_slot.union(row.msg.loads.reserved_with(l));
        }
        (Some((_, since)), Some(l)) => {
            row.load = Some((l, since));
            // A load outside the declaration widens the slot once: the
            // reporter declared less than it showed.
            row.load_slot = row.load_slot.union(row.msg.loads.reserved_with(l));
        }
    }
    row.msg.meter = meter;
}

/// What a row's indicator is, as the layout reads it: the fill, busy, and
/// whether it moves.
#[derive(Clone, Copy)]
struct Indicator {
    fill: Option<u16>,
    busy: bool,
    moving: bool,
}

/// One visual row's inputs to the width law ([`MessageCenter::presentation`]).
struct RowIn<'m> {
    kind: RowKind,
    msg: &'m Message,
    ind: Indicator,
    /// The load said, and the loads the slot is reserved for.
    load: (Option<Load>, Loads),
}

/// What every row of the band is laid out under.
struct LayIn<'a> {
    plus: usize,
    links: Links,
    cols: usize,
    width: &'a dyn Fn(&str) -> usize,
    home: Option<&'a str>,
}

/// What the band says one way at this width: the labels some row had to paint
/// short, and whether some row's time words went short.
#[derive(Default)]
struct BandForms {
    labels: Vec<&'static str>,
    short_time: bool,
}

impl BandForms {
    /// Read off a first layout of every row.
    fn of(laid: &[RowLayout]) -> Self {
        Self {
            labels: laid
                .iter()
                .flat_map(|row| row.capsules.iter())
                .filter(|c| c.role != CapsuleRole::Details && !c.full_label.is_empty())
                .filter(|c| c.text != c.full_label)
                .map(|c| c.full_label)
                .collect(),
            short_time: laid.iter().any(|row| {
                (row.eta.is_some() && row.eta_short) || (row.elapsed.is_some() && row.elapsed_short)
            }),
        }
    }

    /// Whether `row` says something another way than these forms do.
    fn differs(&self, row: &RowLayout) -> bool {
        let long_label = row.capsules.iter().any(|c| {
            c.role != CapsuleRole::Details
                && c.text == c.full_label
                && self.labels.contains(&c.full_label)
        });
        let long_time = self.short_time
            && ((row.eta.is_some() && !row.eta_short)
                || (row.elapsed.is_some() && !row.elapsed_short));
        long_label || long_time
    }
}

/// One wording per width across the band: every row is laid out on its own,
/// then each row that says a label or its time words another way than a row
/// that had to shorten them is laid out again with the short forms, so rows
/// stacked at one width never read `Not today` above `Not now`, or `10 s
/// left` above `15s left`, by title length alone.
fn lay_band(inputs: &[RowIn<'_>], lay: &LayIn<'_>) -> Vec<RowLayout> {
    let own = BandForms::default();
    let mut laid: Vec<RowLayout> = inputs
        .iter()
        .map(|i| message_layout(i, lay, &own))
        .collect();
    let forms = BandForms::of(&laid);
    for (row, input) in laid.iter_mut().zip(inputs) {
        if forms.differs(row) {
            *row = message_layout(input, lay, &forms);
        }
    }
    laid
}

/// One message's row: the excerpt only when the row paints it, its
/// authored capsules and its link, and the indicator it moves with — in the
/// band's `forms` at this width.
fn message_layout(row: &RowIn<'_>, lay: &LayIn<'_>, forms: &BandForms) -> RowLayout {
    let RowIn {
        kind,
        msg,
        ind,
        load: (load, load_slot),
    } = *row;
    let LayIn {
        plus,
        links,
        cols,
        width,
        home,
    } = *lay;
    let detail0 = msg
        .detail
        .first()
        .filter(|_| msg.excerpt)
        .map(|d| abbreviate_paths_in(d, home));
    let echo = matches!(kind, RowKind::Echo(_));
    let mut capsules: Vec<CapsuleSpec> = msg
        .actions
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let mut spec = CapsuleSpec::authored(it, u8::try_from(i).unwrap_or(u8::MAX));
            // A label another row at this width paints short is painted short
            // here too, and the row still pays its long form (`paid`), so the
            // cells it saves re-buy no excerpt and no link a wider band had
            // given up.
            if forms.labels.contains(&spec.full_label) && !spec.short.is_empty() {
                spec.long.clone_from(&spec.short);
            }
            // An echo is not pressable (a flash of well under a second cannot
            // be clicked): every capsule keeps its cells and draws nothing.
            if echo { spec.blanked() } else { spec }
        })
        .collect();
    if links == Links::Painted {
        let link = CapsuleSpec::details(plus);
        capsules.push(if echo { link.blanked() } else { link });
    }
    let stats = msg.meter.as_ref().map_or("", |m| m.stats.as_str());
    let moving_fill = ind.moving && ind.fill.is_some();
    // The excerpt is the ask's one step where the row leads with it (a
    // notice's ask writes its step first; one led by `lead_with` says its
    // reason first, and that excerpt is an extra as any other).
    let step = matches!(
        msg.need(),
        crate::need::Need::Now(ask)
            if msg.excerpt && msg.detail.first().map(String::as_str) == Some(ask.then().words())
    );
    // ...and its short form, which stands in its place where the step is not
    // whole beside the title (a command is never cut).
    let step_short = match msg.need() {
        crate::need::Need::Now(ask) if step => ask.then().short(),
        _ => None,
    };
    let spec = RowSpec {
        kind,
        severity: msg.severity,
        live: matches!(msg.hold, Hold::Live { .. } | Hold::Standing(_)),
        glyph: msg.glyph.ch(),
        title: &msg.title,
        title_short: msg.short_title.as_deref(),
        detail0: detail0.as_deref(),
        step,
        step_short,
        meter: msg.meter.as_ref().map(|_| (ind.fill, stats)),
        busy: ind.busy && ind.fill.is_none(),
        animated: moving_fill,
        eta: moving_fill && msg.meter.as_ref().is_some_and(|m| m.amount.is_some()),
        load: load.filter(|_| ind.moving),
        load_slot: if ind.moving { load_slot } else { Loads::NONE },
        capsules,
    };
    let mut layout = layout_row_with(&spec, cols, width, forms.short_time);
    if matches!(kind, RowKind::Echo(_)) {
        // An echo is the indicator ending, not a reading: the stats were
        // frozen at the last read before the resolve, so they go, laid out as
        // the row was, so nothing on it moves at the resolve.
        layout.stats = None;
    }
    layout
}

/// A moving row's text ticks at frame `q`, in every pace: its elapsed words'
/// next change; where its ETA slot is laid out, the estimate's next change
/// (the countdown's next word, the latch running out, the stall); and where
/// the width starved that slot, a graded bar's stall onset (the stall's dim
/// ink, `stalled_bar`, is then the only stall signal left in every pace, so
/// its onset repaints the bar).
fn text_ticks(row: &Live, layout: &RowLayout, q: Instant, look: Look) -> [Option<Instant>; 3] {
    let e = q.saturating_duration_since(row.started_at);
    let elapsed = layout
        .elapsed
        .map(|_| row.started_at + e + elapsed_word_change(e));
    if layout.eta.is_some() {
        // A hidden estimate leaves the slot blank: nothing ticks there until
        // the estimate latches.
        [elapsed, row.track.next_change(q), None]
    } else {
        let stall = (look.graded && layout.meter.is_some())
            .then(|| row.track.stall_onset(q))
            .flatten();
        [elapsed, stall, None]
    }
}

/// What a moving determinate bar draws at frame `t`: the one function the
/// frame ([`live_motion`]) and the deadline's look-ahead
/// ([`MessageCenter::motion_deadline`]) both read, so a deadline never asks a
/// frame the motion would not draw, nor misses one. The stalled ink once the
/// bytes stopped, else the fill shown under the glint. The glide is read at the fine instant `tf`, the glint at the grid's
/// `t` (at `tf` mid-glide), on a row `cols` wide.
fn moving_bar_at(
    row: &Live,
    data: u16,
    (t, tf): (Instant, Instant),
    epoch: Instant,
    graded: bool,
    cols: usize,
) -> Surface {
    let edge = row
        .shown_edge(tf)
        .unwrap_or(i64::from(data) * i64::from(ROW) / 1000);
    if matches!(row.track.eta(t), Eta::Stalled) {
        return stalled_bar(data, graded);
    }
    // Mid-glide the whole bar is read at the fine instant, its glint too, so
    // the row changes on one cadence.
    let tg = if row.glide_end(tf).is_some() { tf } else { t };
    let glint = if graded { glint_at(tg, epoch) } else { None };
    bar_at(edge, glint, graded, cols)
}

/// How far ahead [`next_comet_frame`] scans at most: one crossing's frames.
/// The comet moves a cell within a few frames at any width the band paints,
/// so the scan stops long before; the bound only keeps it finite.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a period over a frame: a few hundred, far inside u32"
)]
const COMET_SCAN_FRAMES: u32 = (crate::COMET_PERIOD.as_millis() / ANIM_FRAME.as_millis()) as u32;

/// The first grid frame after `q` at which a busy row's comet draws something
/// new at `cols` cells: the next frame, except where a crossing's faint last
/// sliver and the hand-over round to the same cells.
fn next_comet_frame(q: Instant, epoch: Instant, cols: usize, graded: bool) -> Instant {
    let at = |g: Instant| comet(g.saturating_duration_since(epoch), graded).cells(cols);
    let drawn = at(q);
    let mut g = q + ANIM_FRAME;
    for _ in 1..COMET_SCAN_FRAMES {
        if at(g) != drawn {
            break;
        }
        g += ANIM_FRAME;
    }
    g
}

/// A moving row's motion at frame `q`: the bar for a fill, the comet for a
/// busy row (the unlit track, still), and its time words.
fn live_motion(
    row: &Live,
    layout: &RowLayout,
    (q, qf): (Instant, Instant),
    look: Look,
    cols: usize,
) -> RowMotion {
    let epoch = row.motion_since.unwrap_or(q);
    let stalled = matches!(row.track.eta(q), Eta::Stalled);
    let (surface, anim, glyph) = match (layout.meter, layout.track.is_some(), look.pace) {
        (Some((_, _, data)), _, Pace::Moving) => {
            let shown = if stalled {
                data
            } else {
                row.shown_fill(qf).unwrap_or(data)
            };
            let tg = if row.glide_end(qf).is_some() { qf } else { q };
            let glint_ms = glint_at(tg, epoch)
                .filter(|_| look.graded && !stalled && bar_glints(shown))
                .map(anim_ms);
            (
                moving_bar_at(row, data, (q, qf), epoch, look.graded, cols),
                Anim::Bar { shown, glint_ms },
                None,
            )
        }
        (Some((_, _, data)), _, Pace::Still) => (
            if stalled {
                stalled_bar(data, look.graded)
            } else {
                bar(data, None, look.graded)
            },
            Anim::Bar {
                shown: data,
                glint_ms: None,
            },
            None,
        ),
        // The comet says the work moves; the glyph cell keeps the row's own
        // glyph (drawn icons, no spinner).
        (None, true, Pace::Moving) => (
            comet(q.saturating_duration_since(epoch), look.graded),
            Anim::Comet {
                phase_ms: anim_ms(comet_phase(q, epoch)),
            },
            None,
        ),
        (None, true, Pace::Still) => (track(look.graded), Anim::Track, None),
        (None, false, _) => (Surface::default(), Anim::None, None),
    };
    let readout = layout.elapsed.and_then(|_| {
        elapsed_words(
            q.saturating_duration_since(row.started_at),
            layout.elapsed_short,
        )
    });
    // A hidden estimate leaves the slot blank, its cells kept: elapsed time
    // beside a percent would read as time left (`35% 0:07`) in the slot the
    // estimate appears in later. A remaining time always ends in `left`.
    let eta = layout.eta.and_then(|_| match row.track.eta(q) {
        Eta::Remaining(r) if layout.eta_short => eta_words_short(r),
        Eta::Remaining(r) => eta_words(r),
        Eta::Stalled => Some(STALLED_WORD.to_string()),
        Eta::Hidden => None,
    });
    RowMotion {
        surface,
        readout,
        eta,
        fade: 0,
        glyph,
        anim,
    }
}

/// An echo's motion at frame `q`, over the whole row. A Complete echo wears ✓
/// and says nothing in its time slots, except a title with no past tense, which
/// says `done` there (`✓ Uploading the backup` must not read as still going).
/// A Fault echo wears ⚠ and says `failed` in the row's time slot (the ETA slot,
/// else the elapsed one). A Vanish keeps the frozen elapsed words. Nothing
/// re-grids: the slot is the row's own.
fn echo_motion(e: &Echo, layout: &RowLayout, q: Instant, look: Look, cols: usize) -> RowMotion {
    // Past its end an echo is at its end (the row goes at the next settle).
    let t = q.min(e.until).saturating_duration_since(e.started);
    let (surface, fade) = echo(e, t, look, cols);
    let said = |word: &str| {
        (
            layout
                .elapsed
                .filter(|_| layout.eta.is_none())
                .map(|_| word.to_string()),
            layout.eta.map(|_| word.to_string()),
        )
    };
    let (readout, eta) = match e.kind {
        EchoKind::Complete if e.msg.finished_words().is_none() => said(DONE_WORD),
        EchoKind::Complete => (None, None),
        EchoKind::Fault => said(FAILED_WORD),
        EchoKind::Vanish => (
            layout
                .elapsed
                .and_then(|_| elapsed_words(e.elapsed, layout.elapsed_short)),
            None,
        ),
    };
    RowMotion {
        surface: if layout.meter.is_some() || layout.track.is_some() {
            surface
        } else {
            Surface::default()
        },
        readout,
        eta,
        fade,
        glyph: match e.kind {
            EchoKind::Complete => Some('\u{2713}'),
            EchoKind::Fault => Some('\u{26a0}'),
            EchoKind::Vanish => None,
        },
        anim: Anim::Echo {
            kind: e.kind,
            t_ms: anim_ms(t),
        },
    }
}

/// The line a row's record gains when aterm quits under it: `28% when aterm
/// quit` for a determinate row (a busy row has no percent to name); `None`
/// otherwise.
fn quit_reading(row: &Live) -> Option<String> {
    let fill = row.msg.meter.as_ref()?.fill_permille?;
    Some(format!("{}% when aterm quit", fill.min(1000) / 10))
}

fn carried_message(l: &Live, on_glass: bool, posted_severity: Option<Severity>) -> CarriedMessage {
    CarriedMessage {
        id: l.id.raw(),
        unix_ms: l.stamp.unix_ms,
        tag: l.msg.tag.as_str().to_string(),
        severity: l.msg.severity.as_str().to_string(),
        posted_severity: posted_severity
            .filter(|severity| *severity != l.msg.severity)
            .map(|severity| severity.as_str().to_string()),
        glyph: l.msg.glyph.ch(),
        title: l.msg.title.clone(),
        short_title: l.msg.short_title.clone(),
        detail: l.msg.detail.clone(),
        actions: l.msg.actions.iter().map(Intent::encode).collect(),
        hold: l.msg.hold.encode(),
        key: l.msg.key.clone(),
        fill_permille: l.msg.meter.as_ref().and_then(|m| m.fill_permille),
        stats: l
            .msg
            .meter
            .as_ref()
            .map(|m| m.stats.clone())
            .unwrap_or_default(),
        busy: l.msg.meter.as_ref().is_some_and(|m| m.busy),
        on_glass,
        excerpt: l.msg.excerpt,
        finished: l.msg.finished.clone(),
        retrospective: l.msg.retrospective,
        wire: l.msg.origin.wire_owned(l.msg.key.as_deref()),
        repeats: l.repeats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{LogState, Retired};
    use crate::model::{Decision, Origin, tags};
    use crate::text::char_width;
    use crate::{HOLD_INFO, HOLD_SUCCESS, HOLD_WARN};

    fn t0() -> Instant {
        Instant::now()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn stamp(unix_ms: u64) -> WallStamp {
        WallStamp { unix_ms }
    }

    fn fresh(now: Instant) -> MessageCenter {
        MessageCenter::new(MessageLog::empty(), now)
    }

    fn info(title: &str) -> Message {
        Message::new(tags::SYSTEM, Severity::Info, title)
    }

    fn standing(title: &str) -> Message {
        Message::new(tags::RENDER, Severity::Info, title).hold(Hold::STANDING)
    }

    fn ask(title: &str) -> Message {
        Message::new(tags::PRIVACY, Severity::Info, title)
            .action(Intent::OpenSystemPane {
                pane: "full-disk-access".into(),
            })
            .action(Intent::NotNow {
                decision: Decision::FileAccess,
            })
            .hold(Hold::asking(crate::HOLD_ASK))
    }

    fn glass_ids(c: &MessageCenter) -> Vec<MessageId> {
        c.on_glass().map(|l| l.id).collect()
    }

    fn pending_retired(c: &MessageCenter) -> Vec<(MessageId, Retired, String, u32)> {
        c.log()
            .pending_lines()
            .filter_map(|l| match l {
                LogLine::Retired {
                    id,
                    how,
                    title,
                    repeats,
                    ..
                } => Some((*id, how.clone(), title.clone(), *repeats)),
                _ => None,
            })
            .collect()
    }

    /// Invariant 2: post MAX_LIVE+50 held rows: `live.len() == MAX_LIVE`,
    /// evictions logged `Evicted`, an Ask row and a Standing row posted
    /// first survive.
    #[test]
    fn bounded_live_set() {
        let now = t0();
        let mut c = fresh(now);
        let ask_id = c.post(ask("File access not confirmed"), stamp(1), now).id;
        let standing_id = c.post(standing("GPU lost"), stamp(2), now).id;
        for i in 0..(MAX_LIVE + 50) {
            c.post(
                info(&format!("notice {i}")),
                stamp(10 + i as u64),
                now + ms(i as u64),
            );
        }
        assert_eq!(c.live_rows().count(), MAX_LIVE);
        assert!(c.live(ask_id).is_some(), "the ask survives");
        assert!(c.live(standing_id).is_some(), "the standing row survives");
        let evicted: Vec<_> = pending_retired(&c)
            .into_iter()
            .filter(|(_, how, _, _)| *how == Retired::Evicted)
            .collect();
        assert_eq!(
            evicted.len(),
            52,
            "two more than MAX_LIVE + 50 - MAX_LIVE: the ask and standing rows hold two seats"
        );
        assert_eq!(evicted[0].2, "notice 0", "the oldest held row goes first");
    }

    /// Past [`MAX_LIVE`] every queued evictable row goes before any row on the
    /// glass, whatever its hold: a queue of `Live` rows gives up its
    /// lowest-severity, oldest row while the held warning on the glass stays.
    /// A row on glass goes only when nothing queued is evictable (the held one
    /// before the `Live` one), and the newcomer last of all.
    #[test]
    fn a_flood_takes_every_queued_row_before_one_on_glass() {
        let now = t0();
        let tail = |severity, title: &str| {
            Message::new(tags::SYSTEM, severity, title).hold(Hold::Live {
                stale_after: crate::STALE_TAILED,
            })
        };
        let evicted = |c: &MessageCenter| -> Vec<String> {
            pending_retired(c)
                .into_iter()
                .filter(|(_, how, _, _)| *how == Retired::Evicted)
                .map(|(_, _, title, _)| title)
                .collect()
        };
        let mut c = fresh(now);
        let warn = c
            .post(
                Message::new(tags::PACKAGES, Severity::Warn, "Disk nearly full"),
                stamp(1),
                now,
            )
            .id;
        c.commit_rows(now, 1);
        assert_eq!(glass_ids(&c), vec![warn]);
        for i in 0..MAX_LIVE {
            let (severity, title) = if i == 30 {
                (Severity::Success, "quiet tail".to_string())
            } else {
                (Severity::Info, format!("tail {i}"))
            };
            c.post(
                tail(severity, &title),
                stamp(10 + i as u64),
                now + ms(1 + i as u64),
            );
        }
        assert_eq!(c.live_rows().count(), MAX_LIVE);
        assert_eq!(
            glass_ids(&c),
            vec![warn],
            "the held warning on the glass outlives a flood of queued Live rows"
        );
        assert_eq!(
            evicted(&c),
            ["quiet tail"],
            "the lowest-severity queued row goes first, though `tail 0` is older"
        );
        c.post(tail(Severity::Info, "one more"), stamp(999), now + ms(999));
        assert_eq!(glass_ids(&c), vec![warn]);
        assert_eq!(
            evicted(&c),
            ["quiet tail", "tail 0"],
            "then the oldest queued row"
        );

        // Nothing queued is evictable: the rows on glass go, held first.
        let mut c = fresh(now);
        let g_live = c.post(tail(Severity::Info, "g live"), stamp(1), now).id;
        let g_held = c.post(info("g held"), stamp(2), now + ms(1)).id;
        let x = c
            .post(
                Message::new(tags::PACKAGES, Severity::Success, "x"),
                stamp(3),
                now + ms(2),
            )
            .id;
        c.commit_rows(now + ms(2), 3);
        assert_eq!(glass_ids(&c), vec![g_live, g_held, x]);
        for i in 0..(MAX_LIVE - 3) {
            c.post(
                standing(&format!("standing {i}")),
                stamp(10 + i as u64),
                now + ms(3 + i as u64),
            );
        }
        assert_eq!(
            glass_ids(&c),
            vec![g_live, g_held],
            "the overflow row took the third slot; `x` is queued"
        );
        let later = |c: &mut MessageCenter, n: u64| {
            c.post(
                standing(&format!("late {n}")),
                stamp(900 + n),
                now + ms(900 + n),
            );
        };
        later(&mut c, 1);
        assert_eq!(evicted(&c), ["x"], "the queued held row first");
        assert_eq!(glass_ids(&c), vec![g_live, g_held]);
        later(&mut c, 2);
        assert_eq!(
            evicted(&c),
            ["x", "g held"],
            "nothing queued is evictable: the held row on glass before the Live one"
        );
        later(&mut c, 3);
        assert_eq!(evicted(&c), ["x", "g held", "g live"]);
        later(&mut c, 4);
        assert_eq!(
            evicted(&c),
            ["x", "g held", "g live", "late 4"],
            "only asks and standing rows are left: the newcomer itself goes"
        );
        assert!(!c.has_live_capacity(), "the protected queue has no vacancy");
        let retired = c.live_rows().next().expect("a protected row").id;
        assert!(c.withdraw(retired, now + ms(1000)));
        assert!(
            c.has_live_capacity(),
            "an actual retirement opens admission"
        );
        let next = c
            .post(
                info("owed row after a vacancy"),
                stamp(1001),
                now + ms(1001),
            )
            .id;
        assert!(
            c.live(next).is_some(),
            "the admitted new row survives the cap"
        );
        assert!(!c.has_live_capacity(), "the vacancy has been consumed");
    }

    /// Within the queue a held row goes before a `Live` one, whatever their
    /// severities and ages: the hold kind ranks before severity. The queued
    /// `Live` row is the lowest severity and oldest post, yet the queued held
    /// Info rows go instead, the `Default` one then the `For` one.
    #[test]
    fn a_queued_held_row_goes_before_a_queued_live_row_of_lower_severity() {
        let now = t0();
        let mut c = fresh(now);
        let glass = c
            .post(
                Message::new(tags::PACKAGES, Severity::Error, "Install failed"),
                stamp(1),
                now,
            )
            .id;
        c.commit_rows(now, 1);
        let live_success = c
            .post(
                Message::new(tags::SYSTEM, Severity::Success, "Indexing done soon").hold(
                    Hold::Live {
                        stale_after: crate::STALE_TAILED,
                    },
                ),
                stamp(2),
                now + ms(1),
            )
            .id;
        let held_info = c.post(info("held info"), stamp(3), now + ms(2)).id;
        let route_info = c
            .post(
                info("route info").hold(Hold::For(crate::HOLD_ROUTE)),
                stamp(4),
                now + ms(3),
            )
            .id;
        for i in 0..(MAX_LIVE - 4) {
            c.post(
                standing(&format!("standing {i}")),
                stamp(10 + i as u64),
                now + ms(4 + i as u64),
            );
        }
        assert_eq!(c.live_rows().count(), MAX_LIVE);
        assert_eq!(glass_ids(&c), vec![glass]);
        for id in [live_success, held_info, route_info] {
            assert!(c.live(id).unwrap().is_queued());
        }
        let evicted = |c: &MessageCenter| -> Vec<MessageId> {
            pending_retired(c)
                .into_iter()
                .filter(|(_, how, _, _)| *how == Retired::Evicted)
                .map(|(id, _, _, _)| id)
                .collect()
        };
        c.post(standing("one more"), stamp(998), now + ms(998));
        assert_eq!(
            evicted(&c),
            vec![held_info],
            "the queued held row goes before the queued Live row, though that one is \
             lower in severity and older"
        );
        c.post(standing("two more"), stamp(999), now + ms(999));
        assert_eq!(
            evicted(&c),
            vec![held_info, route_info],
            "a `For` row is held: it goes before the Live row too"
        );
        assert!(c.live(live_success).is_some());
        assert_eq!(glass_ids(&c), vec![glass]);
    }

    /// A row still inside its reveal grace has never been on the glass, so
    /// eviction counts it as queued whatever its severity: an unrevealed
    /// `Live` Error goes before the held warning on the glass.
    #[test]
    fn an_unrevealed_row_counts_as_queued_and_goes_before_a_row_on_glass() {
        let now = t0();
        let mut c = fresh(now);
        // A Warn, not an Info: a standing Info newcomer outranks a held Info
        // on a one-row glass and would push it off before the eviction.
        let warn = c
            .post(
                Message::new(tags::PACKAGES, Severity::Warn, "Disk nearly full"),
                stamp(1),
                now,
            )
            .id;
        c.commit_rows(now, 1);
        let unrevealed = c
            .post(
                Message::new(tags::SYSTEM, Severity::Error, "Build failing")
                    .hold(Hold::Live {
                        stale_after: crate::STALE_TAILED,
                    })
                    // The door records a Warn-or-worse Live row that shows no
                    // indicator: work in flight shows one.
                    .in_flight()
                    .reveal_after(Duration::from_secs(5)),
                stamp(2),
                now + ms(1),
            )
            .id;
        for i in 0..(MAX_LIVE - 2) {
            c.post(
                standing(&format!("standing {i}")),
                stamp(10 + i as u64),
                now + ms(2 + i as u64),
            );
        }
        assert_eq!(c.live_rows().count(), MAX_LIVE);
        assert_eq!(glass_ids(&c), vec![warn]);
        let row = c.live(unrevealed).unwrap();
        assert!(
            !row.revealed && row.is_queued(),
            "still in its reveal grace"
        );
        c.post(standing("one more"), stamp(999), now + ms(999));
        let evicted: Vec<MessageId> = pending_retired(&c)
            .into_iter()
            .filter(|(_, how, _, _)| *how == Retired::Evicted)
            .map(|(id, _, _, _)| id)
            .collect();
        assert_eq!(
            evicted,
            vec![unrevealed],
            "the unrevealed Error is queued: it goes before the held warning on the glass"
        );
        assert_eq!(glass_ids(&c), vec![warn]);
    }

    /// Invariant 5: a row queued behind three others gets `fold_at =
    /// promotion + hold`; a row the overflow row displaces is released.
    #[test]
    fn hold_anchors_at_first_glass_and_a_displaced_row_is_released() {
        let now = t0();
        let mut c = fresh(now);
        let ids: Vec<MessageId> = (0..4)
            .map(|i| c.post(info(&format!("r{i}")), stamp(i), now + ms(i)).id)
            .collect();
        assert_eq!(c.wanted_rows(), 3);
        assert_eq!(c.commit_rows(now + ms(10), 3), Some(3));
        assert_eq!(
            glass_ids(&c),
            vec![ids[0], ids[1]],
            "two rows plus the overflow row"
        );
        assert_eq!(c.queued(), 2);
        assert_eq!(
            c.live(ids[0]).unwrap().fold_at,
            Some(now + ms(10) + HOLD_INFO),
            "anchored at commit, not at post"
        );
        assert_eq!(c.live(ids[2]).unwrap().fold_at, None, "queued: no anchor");
        assert!(c.live(ids[2]).unwrap().is_queued());
        // The first row resolves; the third is promoted and anchors then.
        let promotion = now + ms(500);
        assert!(c.resolve(ids[0], Outcome::Ok, promotion));
        assert_eq!(
            glass_ids(&c),
            vec![ids[1], ids[2], ids[3]],
            "three eligible rows fill the three committed rows; the overflow row is gone"
        );
        assert_eq!(c.live(ids[2]).unwrap().on_glass_since, Some(promotion));
        assert_eq!(c.live(ids[2]).unwrap().fold_at, Some(promotion + HOLD_INFO));
        assert_eq!(c.live(ids[3]).unwrap().fold_at, Some(promotion + HOLD_INFO));
        assert_eq!(
            c.live(ids[1]).unwrap().fold_at,
            Some(now + ms(10) + HOLD_INFO),
            "the survivor keeps its anchor"
        );
        // A row the glass displaces (the overflow row takes its slot when a
        // fourth arrives) is released: queued again, no anchor, and it
        // anchors afresh when it returns.
        let mut c = fresh(now);
        let ids: Vec<MessageId> = (0..3)
            .map(|i| c.post(info(&format!("r{i}")), stamp(i), now).id)
            .collect();
        c.commit_rows(now, 3);
        assert_eq!(glass_ids(&c), ids);
        let fourth = c.post(info("r3"), stamp(3), now + ms(1)).id;
        assert_eq!(
            glass_ids(&c),
            vec![ids[0], ids[1]],
            "the third slot is the overflow row"
        );
        let displaced = c.live(ids[2]).unwrap();
        assert!(displaced.is_queued() && displaced.fold_at.is_none());
        assert_eq!(displaced.on_glass_since, None);
        assert_eq!(
            c.settle(now + HOLD_INFO, true).retired,
            vec![(ids[0], Retired::Folded), (ids[1], Retired::Folded)],
            "the displaced row does not fold hidden"
        );
        assert_eq!(glass_ids(&c), vec![ids[2], fourth]);
        assert_eq!(
            c.live(ids[2]).unwrap().fold_at,
            Some(now + HOLD_INFO + HOLD_INFO),
            "re-anchored on its return"
        );
    }

    /// Invariant 6: key replacement: one row, same slot, old record
    /// `Superseded{by}` with its final title.
    #[test]
    fn supersede_keeps_the_slot_and_logs_the_last_words() {
        let now = t0();
        let mut c = fresh(now);
        let top = c.post(info("above"), stamp(1), now).id;
        let a = c
            .post(
                Message::new(tags::UPDATE, Severity::Info, "Checking for updates")
                    .key("update.progress"),
                stamp(2),
                now,
            )
            .id;
        let below = c.post(info("below"), stamp(3), now).id;
        c.commit_rows(now, 3);
        assert_eq!(glass_ids(&c), vec![top, a, below]);
        c.restate(
            a,
            Restatement {
                title: Some("Downloading aterm v0.91.0".into()),
                ..Restatement::default()
            },
            now + ms(5),
        );
        let posted = c.post(
            Message::new(tags::UPDATE, Severity::Success, "aterm v0.91.0 is ready")
                .key("update.progress"),
            stamp(4),
            now + ms(10),
        );
        assert_eq!(posted.outcome, PostOutcome::Superseded(a));
        assert_eq!(glass_ids(&c), vec![top, posted.id, below], "the same slot");
        assert!(c.live(a).is_none());
        assert_eq!(
            c.live(posted.id).unwrap().fold_at,
            Some(now + ms(10) + HOLD_SUCCESS),
            "a fresh anchor"
        );
        let old = c.log().get(a).unwrap();
        assert_eq!(
            old.state,
            LogState::Retired(Retired::Superseded { by: posted.id })
        );
        assert_eq!(
            old.title, "Downloading aterm v0.91.0",
            "the record keeps the final words"
        );
        assert_eq!(c.live_by_key("update.progress").unwrap().id, posted.id);
        // A queued row superseded stays queued (no anchor).
        let mut c = fresh(now);
        let q = c.post(info("q").key("k"), stamp(1), now).id;
        let q2 = c.post(info("q2").key("k"), stamp(2), now).id;
        assert!(c.live(q).is_none() && c.live(q2).unwrap().fold_at.is_none());
    }

    /// Invariant 7: the same post twice: one row, `repeats == 2`, `fold_at`
    /// moved; the Retired line carries `rep=2`.
    #[test]
    fn duplicates_bump_repeats_and_reanchor_the_hold() {
        let now = t0();
        let mut c = fresh(now);
        let first = c.post(info("Same words").line("same detail"), stamp(1), now);
        c.commit_rows(now, 3);
        let anchored = c.live(first.id).unwrap().fold_at.unwrap();
        let again = c.post(
            info("Same words").line("same detail"),
            stamp(2),
            now + ms(700),
        );
        assert_eq!(
            again,
            Posted {
                id: first.id,
                outcome: PostOutcome::Duplicate
            }
        );
        assert_eq!(
            c.live(first.id).unwrap().fold_at,
            Some(anchored + ms(700)),
            "the hold re-anchors"
        );
        assert_eq!(c.log().pending_len(), 1, "a duplicate logs nothing new");
        // Different detail is not a duplicate; a keyed duplicate needs the
        // same actions too.
        assert_eq!(
            c.post(info("Same words").line("other"), stamp(3), now)
                .outcome,
            PostOutcome::New
        );
        let k = c.post(
            info("K").key("dup").action(Intent::NewWindow),
            stamp(4),
            now,
        );
        assert_eq!(
            c.post(
                info("K").key("dup").action(Intent::NewWindow),
                stamp(5),
                now
            )
            .outcome,
            PostOutcome::Duplicate
        );
        assert_eq!(
            c.post(info("K").key("dup"), stamp(6), now).outcome,
            PostOutcome::Superseded(k.id)
        );
        let folds = c.settle(anchored + ms(700), true);
        assert!(
            folds
                .retired
                .iter()
                .any(|(id, how)| *id == first.id && *how == Retired::Folded),
            "{folds:?}"
        );
        let line = pending_retired(&c)
            .into_iter()
            .find(|(id, _, _, _)| *id == first.id)
            .unwrap();
        assert_eq!(line.3, 2, "rep=2 on the Retired line");
    }

    /// Invariant 8: 1000 meter restates → 0 new lines; the final words land
    /// in the Retired line.
    #[test]
    fn restatements_never_write_the_log() {
        let now = t0();
        let mut c = fresh(now);
        let id = c
            .post(
                Message::new(tags::TOOLCHAIN, Severity::Info, "Installing")
                    .line("trust \u{00b7} downloading")
                    .meter(Meter {
                        fill_permille: Some(0),
                        stats: String::new(),
                        ..Meter::default()
                    })
                    .hold(Hold::Live {
                        stale_after: crate::STALE_TAILED,
                    }),
                stamp(1),
                now,
            )
            .id;
        c.commit_rows(now, 3);
        let before = c.log().pending_len();
        for i in 0..1000u16 {
            let at = now + ms(u64::from(i));
            assert!(c.restate(
                id,
                Restatement {
                    meter: Some(Some(Meter {
                        fill_permille: Some(i),
                        stats: format!("{i} / 1000"),
                        ..Meter::default()
                    })),
                    detail: Some(vec![format!("trust \u{00b7} step {i}")]),
                    ..Restatement::default()
                },
                at
            ));
        }
        assert_eq!(c.log().pending_len(), before, "no lines for restatements");
        c.restate(
            id,
            Restatement {
                title: Some("Installed".into()),
                ..Restatement::default()
            },
            now + ms(1000),
        );
        let settled = c.settle(now + ms(1000) + crate::STALE_TAILED, true);
        assert_eq!(settled.retired, vec![(id, Retired::Stale)]);
        let line = c.log().pending_lines().last().cloned().unwrap();
        match line {
            LogLine::Retired { title, detail, .. } => {
                assert_eq!(title, "Installed");
                assert_eq!(detail, vec!["trust \u{00b7} step 999".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        assert!(!c.restate(id, Restatement::default(), now), "gone");
    }

    /// An identical restatement is not a change of the glass: the same words
    /// and meter (a reporter's heartbeat) leave the revision and the
    /// fingerprint alone, and still re-arm the staleness cap.
    #[test]
    fn an_identical_restatement_moves_no_revision() {
        let now = t0();
        let mut c = fresh(now);
        let words = Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab toolchain")
            .line("trust")
            .meter(Meter {
                fill_permille: Some(420),
                stats: "3 of 10 programs".into(),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: crate::STALE_TAILED,
            });
        let id = c.post(words.clone(), stamp(1), now).id;
        c.commit_rows(now, 3);
        let (rev, fp) = (c.revision(), c.fingerprint(120));
        let same = Restatement {
            title: Some(words.title.clone()),
            detail: Some(words.detail.clone()),
            meter: Some(words.meter.clone()),
            hold: Some(words.hold),
            ..Restatement::default()
        };
        assert!(
            c.restate(id, same.clone(), now + ms(2000)),
            "the row is live"
        );
        assert_eq!(c.revision(), rev, "nothing the row says changed");
        assert_eq!(c.fingerprint(120), fp);
        assert_eq!(
            c.live(id).unwrap().stale_at,
            Some(now + ms(2000) + crate::STALE_TAILED),
            "the heartbeat still re-arms the cap"
        );
        let moved = Restatement {
            meter: Some(Some(Meter {
                fill_permille: Some(430),
                stats: "4 of 10 programs".into(),
                ..Meter::default()
            })),
            ..same
        };
        assert!(c.restate(id, moved, now + ms(4000)));
        assert!(c.revision() > rev, "a moved meter is a change");
        assert_ne!(c.fingerprint(120), fp);
    }

    /// …but an estimate that moved is: the same reading fed at a later instant
    /// moves the estimator (a re-anchor, then an unlatch), and no motion
    /// deadline was armed for that, so a heartbeat that moves `eta(now)` bumps
    /// the revision and one that moves nothing does not.
    #[test]
    fn a_heartbeat_that_moves_the_estimate_is_a_change() {
        use crate::model::{Amount, Unit};
        let now = t0();
        let mut c = fresh(now);
        // A total the pace nears within the count's own patience (a count
        // stalls after four of its gaps, here 10 s): the heartbeat's re-votes
        // move the estimate before the stall does.
        let meter = |done: u64| Meter {
            fill_permille: Some(u16::try_from(done * 1000 / 40).unwrap()),
            stats: "3 of 10 programs".into(),
            amount: Some(Amount {
                series: 1,
                done,
                total: 40,
                unit: Unit::Steps,
            }),
            ..Meter::default()
        };
        let id = c
            .post(
                Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab toolchain")
                    .meter(meter(0))
                    .hold(Hold::Live {
                        stale_after: crate::STALE_TAILED,
                    }),
                stamp(1),
                now,
            )
            .id;
        c.commit_rows(now, 3);
        let restate = |c: &mut MessageCenter, done: u64, at: Instant| {
            c.restate(
                id,
                Restatement {
                    meter: Some(Some(meter(done))),
                    ..Restatement::default()
                },
                at,
            )
        };
        // A steady pace latches an estimate.
        let mut t = now;
        for k in 1..=20u64 {
            t += ms(500);
            assert!(restate(&mut c, k, t));
        }
        assert!(
            matches!(c.live(id).unwrap().track.eta(t), Eta::Remaining(_)),
            "latched"
        );
        // Then the reporter stalls and only its heartbeat restates the same
        // reading every 2 s: the revision moves exactly when `eta(now)` does.
        let (mut moved, mut still) = (0, 0);
        for _ in 0..30 {
            t += ms(2000);
            let (rev, eta) = (c.revision(), c.live(id).unwrap().track.eta(t));
            assert!(restate(&mut c, 20, t));
            let after = c.live(id).unwrap().track.eta(t);
            if after == eta {
                still += 1;
                assert_eq!(c.revision(), rev, "nothing moved: no repaint");
            } else {
                moved += 1;
                assert!(c.revision() > rev, "{eta:?} → {after:?}: a repaint");
            }
        }
        assert!(moved > 0 && still > 0, "moved {moved}, still {still}");
    }

    /// Invariant 9: `settle(now, false)` folds a Live row past `stale_at` and
    /// leaves an expired Default row; `deadline(false)` agrees.
    #[test]
    fn frozen_holds_only_retire_on_staleness() {
        let now = t0();
        let mut c = fresh(now);
        let held = c
            .post(
                Message::new(tags::UPDATE, Severity::Success, "Updated")
                    .line("your shells kept running"),
                stamp(1),
                now,
            )
            .id;
        let live = c
            .post(
                Message::new(tags::UPDATE, Severity::Info, "Installing aterm v0.76.0")
                    .in_flight()
                    .hold(Hold::Live {
                        stale_after: STALE_HANDOFF,
                    }),
                stamp(2),
                now,
            )
            .id;
        c.commit_rows(now, 3);
        assert_eq!(c.deadline(true), Some(now + HOLD_SUCCESS));
        assert_eq!(
            c.deadline(false),
            Some(now + STALE_HANDOFF),
            "frozen: only the cap is armed"
        );
        let frozen = c.settle(now + HOLD_SUCCESS, false);
        assert!(
            frozen.retired.is_empty() && !frozen.glass_changed,
            "the freeze keeps the words: {frozen:?}"
        );
        assert!(c.live(held).is_some());
        let capped = c.settle(now + STALE_HANDOFF, false);
        assert_eq!(capped.retired, vec![(live, Retired::Stale)]);
        assert!(capped.glass_changed);
        assert!(c.live(held).is_some(), "still frozen, still up");
        // The live row that went silent leaves its fade in its slot — an
        // echo is not a hold, so the freeze does not stop it ending.
        let faded = now + STALE_HANDOFF + EchoKind::Vanish.span(0);
        assert_eq!(c.deadline(false), Some(faded), "the fade's end");
        assert!(c.settle(faded, false).glass_changed, "the fade ended");
        assert_eq!(
            c.deadline(false),
            None,
            "nothing a frozen settle would act on"
        );
        assert_eq!(c.deadline(true), Some(now + HOLD_SUCCESS));
        let thawed = c.settle(faded, true);
        assert_eq!(thawed.retired, vec![(held, Retired::Folded)]);
        assert_eq!(c.deadline(true), None);
    }

    /// Invariant 10: want 2→3 commits immediately; 3→1 only at
    /// `shrink_since + SHRINK_QUIET`; a re-grow inside the window cancels
    /// the shrink; `deadline` names the shrink instant.
    #[test]
    fn hysteresis_grows_now_and_shrinks_after_quiet() {
        let now = t0();
        let mut c = fresh(now);
        let a = c.post(standing("a"), stamp(1), now).id;
        let b = c.post(standing("b"), stamp(2), now).id;
        assert_eq!(c.commit_rows(now, 3), Some(2));
        let d = c.post(standing("c"), stamp(3), now + ms(1)).id;
        assert_eq!(c.commit_rows(now + ms(1), 3), Some(3), "grow is immediate");
        assert_eq!(c.deadline(true), None);
        c.resolve(b, Outcome::Ok, now + ms(2));
        c.resolve(d, Outcome::Ok, now + ms(2));
        assert_eq!(c.wanted_rows(), 1);
        assert_eq!(c.commit_rows(now + ms(2), 3), None, "shrink waits");
        assert_eq!(c.committed_rows(), 3);
        assert_eq!(
            c.deadline(true),
            Some(now + ms(2) + SHRINK_QUIET),
            "the deadline names the shrink"
        );
        // A re-grow inside the window cancels the shrink.
        let e = c.post(standing("e"), stamp(4), now + ms(500)).id;
        let f = c.post(standing("f"), stamp(5), now + ms(500)).id;
        assert_eq!(
            c.commit_rows(now + ms(500), 3),
            None,
            "3 wanted, 3 committed: nothing moves"
        );
        assert_eq!(c.deadline(true), None, "no shrink pending");
        c.resolve(e, Outcome::Ok, now + ms(600));
        c.resolve(f, Outcome::Ok, now + ms(600));
        assert_eq!(c.commit_rows(now + ms(600), 3), None);
        assert_eq!(
            c.commit_rows(now + ms(600) + SHRINK_QUIET, 3),
            Some(1),
            "the shrink lands on time"
        );
        assert_eq!(c.committed_rows(), 1);
        assert_eq!(glass_ids(&c), vec![a]);
    }

    /// Invariant 11: three rows; a fourth Info appends/queues, a Warn goes
    /// above, a departure moves rows up and never reorders survivors.
    #[test]
    fn stable_order_a_row_never_reshuffles_under_the_pointer() {
        let now = t0();
        let mut c = fresh(now);
        let r: Vec<MessageId> = (0..3)
            .map(|i| c.post(standing(&format!("r{i}")), stamp(i), now + ms(i)).id)
            .collect();
        c.commit_rows(now + ms(3), 3);
        assert_eq!(glass_ids(&c), vec![r[0], r[1], r[2]]);
        let r3 = c.post(standing("r3"), stamp(3), now + ms(4)).id;
        c.commit_rows(now + ms(4), 3);
        assert_eq!(
            glass_ids(&c),
            vec![r[0], r[1]],
            "the fourth queues behind the overflow row"
        );
        assert!(c.live(r3).unwrap().is_queued());
        assert_eq!(c.queued(), 2);
        let warn = c
            .post(
                Message::new(tags::UPDATE, Severity::Warn, "Update failed").hold(Hold::STANDING),
                stamp(4),
                now + ms(5),
            )
            .id;
        c.commit_rows(now + ms(5), 3);
        assert_eq!(
            glass_ids(&c),
            vec![warn, r[0]],
            "a Warn goes above; the survivor keeps its place"
        );
        c.resolve(warn, Outcome::Warn, now + ms(6));
        assert_eq!(
            glass_ids(&c),
            vec![r[0], r[1]],
            "rows move up in order; the best queued row fills the bottom"
        );
        c.resolve(r[0], Outcome::Ok, now + ms(7));
        assert_eq!(
            glass_ids(&c),
            vec![r[1], r[2], r3],
            "three eligible rows: no overflow row, the queue drains in rank order"
        );
        c.resolve(r[2], Outcome::Ok, now + ms(8));
        assert_eq!(glass_ids(&c), vec![r[1], r3]);
        // A later Info never goes above an earlier one, however it is posted.
        let late = c.post(standing("late"), stamp(9), now + ms(9)).id;
        assert_eq!(glass_ids(&c), vec![r[1], r3, late]);
    }

    /// Invariant 12: a Success queued behind two long rows folds `Unseen`
    /// at `max(hold, 60 s)`.
    #[test]
    fn overflow_patience_folds_an_unseen_row() {
        let now = t0();
        let mut c = fresh(now);
        c.post(standing("a"), stamp(1), now);
        c.post(standing("b"), stamp(2), now);
        c.commit_rows(now, 2);
        let s = c
            .post(
                Message::new(tags::PACKAGES, Severity::Success, "installed"),
                stamp(3),
                now + ms(1),
            )
            .id;
        c.commit_rows(now + ms(1), 2);
        assert!(c.live(s).unwrap().is_queued());
        assert_eq!(
            c.live(s).unwrap().patience_at,
            now + ms(1) + OVERFLOW_PATIENCE_MIN,
            "max(8 s, 60 s)"
        );
        assert_eq!(c.deadline(true), Some(now + ms(1) + OVERFLOW_PATIENCE_MIN));
        assert!(
            c.settle(now + ms(1) + OVERFLOW_PATIENCE_MIN - ms(1), true)
                .retired
                .is_empty()
        );
        let settled = c.settle(now + ms(1) + OVERFLOW_PATIENCE_MIN, true);
        assert_eq!(settled.retired, vec![(s, Retired::Unseen)]);
        assert_eq!(
            c.log().get(s).unwrap().state,
            LogState::Retired(Retired::Unseen),
            "the log keeps it"
        );
        // A long hold extends the patience.
        let w = c
            .post(
                Message::new(tags::PACKAGES, Severity::Success, "long")
                    .hold(Hold::For(Duration::from_secs(100))),
                stamp(4),
                now,
            )
            .id;
        assert_eq!(
            c.live(w).unwrap().patience_at,
            now + Duration::from_secs(100),
            "a long hold extends the patience (a transient hold is at most two minutes: the door)"
        );
    }

    /// A queued row whose patience the host restarts while no band is on
    /// screen never folds `Unseen`; one it stops restarting folds at its
    /// patience from the last restart. A row on the glass has no patience to
    /// restart. Negative control: without the restart it folds as ever.
    #[test]
    fn a_restarted_patience_keeps_a_queued_row_waiting() {
        let now = t0();
        let mut c = fresh(now);
        let on_glass = c.post(standing("a"), stamp(1), now).id;
        c.post(standing("b"), stamp(2), now);
        c.commit_rows(now, 2);
        let s = c
            .post(
                Message::new(tags::PACKAGES, Severity::Success, "installed"),
                stamp(3),
                now,
            )
            .id;
        c.commit_rows(now, 2);
        assert!(c.live(s).unwrap().is_queued(), "PRECONDITION: queued");
        assert!(
            !c.restart_patience(on_glass, now),
            "a row on the glass has none"
        );
        let mut t = now;
        for _ in 0..10 {
            t += OVERFLOW_PATIENCE_MIN - ms(1);
            assert!(c.restart_patience(s, t));
            assert!(c.settle(t, true).retired.is_empty(), "it waits on");
        }
        assert_eq!(c.live(s).unwrap().patience_at, t + OVERFLOW_PATIENCE_MIN);
        let settled = c.settle(t + OVERFLOW_PATIENCE_MIN, true);
        assert_eq!(
            settled.retired,
            vec![(s, Retired::Unseen)],
            "left alone, it folds"
        );
        assert!(!c.restart_patience(s, t), "no live row any more");
    }

    /// Invariant 21: Ask row: `act(Open Settings)` → `Some(intent)`, row
    /// stays, `Acted` logged; an action it lacks is `None`. The implicit
    /// Details capsule marks seen: a held row folds, an ask stays.
    #[test]
    fn act_returns_the_intent_and_details_marks_seen() {
        let now = t0();
        let mut c = fresh(now);
        let id = c.post(ask("File access not confirmed"), stamp(1), now).id;
        c.commit_rows(now, 3);
        let intent = c.act(id, ActionIndex(0), now + ms(1));
        assert_eq!(
            intent,
            Some(Intent::OpenSystemPane {
                pane: "full-disk-access".into()
            })
        );
        assert!(c.live(id).is_some(), "Open Settings leaves the row (R16)");
        assert!(
            matches!(c.log().pending_lines().last(), Some(LogLine::Acted { label, .. }) if label == "Open Settings")
        );
        assert_eq!(c.act(id, ActionIndex(5), now), None, "no such action");
        let held = c.post(info("read me"), stamp(2), now).id;
        c.commit_rows(now, 3);
        for (row, folds) in [(held, true), (id, false)] {
            assert_eq!(
                c.act(row, ActionIndex::DETAILS, now + ms(4)),
                Some(Intent::Details)
            );
            assert_eq!(c.live(row).is_none(), folds, "{row}");
        }
        assert_eq!(
            c.log().get(held).unwrap().state,
            LogState::Retired(Retired::Folded)
        );
        assert!(c.live(id).unwrap().seen);

        // A delayed press keeps both the row id and the action list it heard.
        let original = c.live(id).unwrap().action_revision().unwrap();
        let changed = Intent::OpenSettings {
            route: "/packages".into(),
        };
        assert!(c.restate(
            id,
            Restatement {
                actions: Some(vec![changed.clone()]),
                ..Restatement::default()
            },
            now + ms(5)
        ));
        let replacement = c.live(id).unwrap().action_revision().unwrap();
        assert_ne!(replacement, original);
        let before = c.log().pending_lines().count();
        assert_eq!(
            c.act_at_revision(id, original, ActionIndex(0), now + ms(6)),
            None
        );
        assert_eq!(
            c.log().pending_lines().count(),
            before,
            "refusal writes nothing"
        );
        assert_eq!(
            c.act_at_revision(id, replacement, ActionIndex(0), now + ms(7)),
            Some(changed)
        );
        // Returning to the old intent does not revive its previous authorization.
        assert!(c.restate(
            id,
            Restatement {
                actions: Some(vec![Intent::OpenSystemPane {
                    pane: "full-disk-access".into()
                }]),
                ..Restatement::default()
            },
            now + ms(8)
        ));
        assert_eq!(
            c.act_at_revision(id, original, ActionIndex(0), now + ms(9)),
            None
        );
        let current = c.live(id).unwrap().action_revision().unwrap();
        assert_eq!(
            c.act_at_revision(held, current, ActionIndex(0), now),
            None,
            "retired id"
        );
        // Presentation-only changes leave the same actions authorized.
        assert!(c.restate(
            id,
            Restatement {
                title: Some("File access still needs attention".into()),
                ..Restatement::default()
            },
            now + ms(10)
        ));
        assert_eq!(c.live(id).unwrap().action_revision(), Some(current));
        assert!(
            c.act_at_revision(id, current, ActionIndex(0), now + ms(11))
                .is_some()
        );
        // Exhaustion never wraps to an old action version, even if the list returns.
        c.live
            .iter_mut()
            .find(|l| l.id == id)
            .unwrap()
            .action_revision = Some(u64::MAX);
        assert!(c.restate(
            id,
            Restatement {
                actions: Some(vec![Intent::NewWindow]),
                ..Restatement::default()
            },
            now + ms(12)
        ));
        assert_eq!(c.live(id).unwrap().action_revision(), None);
        let before = c.log().pending_lines().count();
        assert_eq!(c.act_at_revision(id, u64::MAX, ActionIndex(0), now), None);
        assert_eq!(c.log().pending_lines().count(), before);
    }

    /// A busy flow row, as the update lane posts one.
    fn busy(title: &str) -> Message {
        Message::new(tags::UPDATE, Severity::Info, title)
            .line("installs within a minute")
            .meter(Meter::busy(""))
            .hold(Hold::Live {
                stale_after: crate::STALE_UPDATE,
            })
            .key("update.progress")
    }

    /// `busy_on_glass` follows the committed rows: false with nothing
    /// committed (an idle band asks for nothing), false for a busy row still
    /// queued behind the band, true once it is on glass. A busy restate is a
    /// repaint, never a re-grid or a wake: the glass changes, the committed
    /// rows and the deadline do not (the engine owns no frame).
    #[test]
    fn busy_on_glass_follows_the_committed_rows() {
        let now = t0();
        let mut c = fresh(now);
        assert!(!c.busy_on_glass(), "nothing posted");
        let id = c.post(busy("Updating to aterm v1"), stamp(1), now).id;
        assert!(!c.busy_on_glass(), "posted, not committed");
        c.commit_rows(now, 3);
        assert!(c.busy_on_glass());
        let (deadline, mut fp) = (c.deadline(true), c.fingerprint(80));
        for (meter, busy) in [(Meter::default(), false), (Meter::busy(""), true)] {
            assert!(c.restate(
                id,
                Restatement {
                    meter: Some(Some(meter)),
                    ..Restatement::default()
                },
                now,
            ));
            assert_eq!(c.busy_on_glass(), busy);
            assert_ne!(c.fingerprint(80), fp, "busy is folded in the glass");
            fp = c.fingerprint(80);
            assert_eq!(c.commit_rows(now, 3), None, "no re-grid");
            assert_eq!(c.deadline(true), deadline, "no wake of its own");
        }
        // Queued: three held Warn rows outrank it; one committed row shows
        // the top of the rank only.
        let mut q = fresh(now);
        for n in 0..3 {
            q.post(
                Message::new(tags::CONFIG, Severity::Warn, format!("w{n}")),
                stamp(n),
                now,
            );
        }
        q.post(busy("Updating to aterm v1"), stamp(9), now);
        q.commit_rows(now, 1);
        assert!(!q.busy_on_glass(), "a queued busy row does not animate");
    }

    /// One id, one record across the handoff: the successor loads the file's
    /// tail before the carry, and a collision in it mints an id above the
    /// file's top, which can be a carried row's id whose Posted line the
    /// parent had not flushed yet. The carried row takes its number; the other
    /// writer's record moves to a fresh id above the carry's and keeps its own
    /// words and state. The parent's record the tail did hold (same stamp and
    /// tag) is re-opened in place. Negative control: the minted record is not
    /// re-opened under the carried row's words.
    #[test]
    fn a_carried_row_never_reopens_another_writers_record_minted_at_load() {
        let now = t0();
        let live = |title: &str, key: &str| {
            Message::new(tags::TOOLCHAIN, Severity::Info, title)
                .meter(Meter::busy(""))
                .hold(Hold::Live {
                    stale_after: crate::STALE_ANNOUNCE,
                })
                .key(key)
        };
        let mut parent = fresh(now);
        let a = parent
            .post(live("Installing ALab tools", "a"), stamp(1), now)
            .id;
        let b = parent.post(live("Indexing docs", "b"), stamp(2), now).id;
        let c = parent.post(live("Building docs", "c"), stamp(3), now).id;
        parent.commit_rows(now, 3);
        let carry = parent.carried();
        assert!(
            carry.live.iter().any(|m| m.id == c.raw()),
            "{:?}",
            carry.live
        );
        let rec = |id: MessageId| parent.log().get(id).cloned().unwrap();
        // The tail on disk: the parent's a and b, and another writer's own
        // `b` — a collision, minted at c's number (c is not on disk yet).
        let other = LogRecord {
            stamp: stamp(9),
            title: "Another writer's work".into(),
            ..rec(b)
        };
        let mut log = MessageLog::empty();
        log.replay_all([
            LogLine::Posted(rec(a)),
            LogLine::Posted(rec(b)),
            LogLine::Posted(other),
        ]);
        let squatter = log.records().find(|r| r.stamp == stamp(9)).map(|r| r.id);
        assert_eq!(squatter, Some(c), "the collision minted the carried id");
        let mut successor = MessageCenter::new(log, now);
        successor.seed_carried(&carry, stamp(10), now);
        let log = successor.log();
        let mut ids: Vec<u64> = log.records().map(|r| r.id.raw()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), log.len(), "one id, one record");
        let at_c = log.get(c).unwrap();
        assert_eq!(at_c.title, "Building docs");
        assert_eq!(at_c.state, LogState::Posted);
        let moved = log.records().find(|r| r.stamp == stamp(9)).unwrap();
        assert_eq!(moved.title, "Another writer's work", "keeps its own words");
        assert_eq!(moved.state, LogState::Retired(Retired::Stale));
        assert!(moved.id.raw() >= carry.next_id, "above every carried id");
        // The parent's own b, which the tail held, is re-opened in place.
        let at_b = log.get(b).unwrap();
        assert_eq!(
            (at_b.title.as_str(), &at_b.state),
            ("Indexing docs", &LogState::Posted)
        );
    }

    /// The carry keeps busy, and Commit stills what nobody restated: a busy
    /// meter crosses the handoff (an older build's carry reads as still); at
    /// Commit a carried row no one here restated stops animating, while a row
    /// restated before Commit keeps what it said.
    #[test]
    fn carry_round_trips_busy_and_commit_stills_unrestated_rows() {
        let now = t0();
        let mut parent = fresh(now);
        let flow = parent.post(busy("Updating to aterm v1"), stamp(1), now).id;
        let other = parent
            .post(
                Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
                    .meter(Meter::busy(""))
                    .hold(Hold::Live {
                        stale_after: crate::STALE_ANNOUNCE,
                    })
                    .key("toolchain.pass"),
                stamp(2),
                now,
            )
            .id;
        parent.commit_rows(now, 3);
        let carry = parent.carried();
        assert!(carry.live.iter().all(|m| m.busy), "{:?}", carry.live);
        let mut successor = fresh(now);
        successor.seed_carried(&carry, stamp(0), now);
        assert!(successor.busy_on_glass());
        assert!(successor.restate(
            flow,
            Restatement {
                detail: Some(vec!["almost done".into()]),
                meter: Some(Some(Meter::busy(""))),
                ..Restatement::default()
            },
            now,
        ));
        successor.after_handoff_commit(now);
        let busy_of = |c: &MessageCenter, id| {
            c.live(id)
                .and_then(|l| l.msg.meter.as_ref())
                .is_some_and(|m| m.busy)
        };
        assert!(busy_of(&successor, flow), "restated: this process's own");
        assert!(!busy_of(&successor, other), "not restated: shown still");
        // An older build's carry has no busy flag: a still row.
        let mut old = carry.clone();
        for m in &mut old.live {
            m.busy = false;
        }
        let mut older = fresh(now);
        older.seed_carried(&old, stamp(0), now);
        assert!(!busy_of(&older, flow));
    }

    /// Invariant 24a: the carry round-trips through a successor, and every
    /// carried held row folds after the handoff commit.
    #[test]
    fn carry_round_trips_and_folds_after_commit() {
        let now = t0();
        let mut parent = fresh(now);
        let held = parent
            .post(
                Message::new(tags::UPDATE, Severity::Success, "Updated")
                    .line("your shells kept running"),
                stamp(1),
                now,
            )
            .id;
        let live = parent
            .post(
                Message::new(tags::TOOLCHAIN, Severity::Info, "Installing")
                    .line("trust \u{00b7} extracting")
                    .meter(Meter {
                        fill_permille: Some(420),
                        stats: "512 MB / 1.2 GB".into(),
                        ..Meter::default()
                    })
                    .action(Intent::OpenSettings {
                        route: "/packages".into(),
                    })
                    .hold(Hold::Live {
                        stale_after: crate::STALE_TAILED,
                    })
                    .key("toolchain.pass"),
                stamp(2),
                now,
            )
            .id;
        let standing = parent.post(standing("GPU lost"), stamp(3), now).id;
        let queued = parent
            .post(
                Message::new(tags::PACKAGES, Severity::Warn, "queued warn"),
                stamp(4),
                now + ms(1),
            )
            .id;
        parent.commit_rows(now + ms(1), 2);
        let carry = parent.carried();
        assert_eq!(carry.next_id, 5);
        assert_eq!(carry.live.len(), 4);
        assert_eq!(
            carry
                .live
                .iter()
                .map(|m| (m.id, m.on_glass))
                .collect::<Vec<_>>(),
            vec![
                (live.raw(), true),
                (held.raw(), false),
                (standing.raw(), false),
                (queued.raw(), false),
            ],
            "the live progress row keeps the one slot at 2 rows (ruling 259); \
             the Warn waits behind the overflow row"
        );
        let carried_live = &carry.live[0];
        assert_eq!(carried_live.hold, "live:30");
        assert_eq!(carried_live.actions, vec!["open-settings:/packages"]);
        assert_eq!(carried_live.fill_permille, Some(420));

        let later = now + Duration::from_secs(5);
        let mut child = fresh(later);
        child.seed_carried(&carry, stamp(5_000), later);
        assert_eq!(child.live_rows().count(), 4);
        assert_eq!(
            child.log().next_id().raw(),
            5,
            "ids continue past the parent's"
        );
        assert_eq!(
            child.log().pending_len(),
            0,
            "no line crosses: the parent's record is its own to the end"
        );
        for id in [held, live, standing, queued] {
            let row = child.live(id).unwrap();
            assert_eq!(
                row.stale_at,
                Some(later + STALE_HANDOFF),
                "{id}: the handoff cap"
            );
            assert_eq!(row.fold_at, None);
            assert_eq!(row.msg.origin, Origin::Carried);
            assert!(child.log().get(id).is_some_and(|r| r.is_live()));
        }
        assert_eq!(
            child.committed_rows(),
            3,
            "seeded so the rows are there before the first window is sized"
        );
        assert_eq!(
            glass_ids(&child),
            vec![queued, live],
            "the carried glass row keeps its slot; the second slot takes the best queued row; the third is the overflow row"
        );
        // The round trip: the child's carry says the same rows.
        let said = |c: &Carry| {
            let mut v: Vec<_> = c
                .live
                .iter()
                .map(|m| {
                    let m = CarriedMessage {
                        on_glass: false,
                        ..m.clone()
                    };
                    (m.id, m)
                })
                .collect();
            v.sort_by_key(|(id, _)| *id);
            (c.next_id, v)
        };
        assert_eq!(said(&child.carried()), said(&carry));
        // Junk in the carry is dropped, not guessed; glass claims past the
        // band (a build with more rows) are clamped.
        let mut junk = carry.clone();
        junk.live[1].severity = "loud".into();
        junk.live[2].tag = "Not A Tag".into();
        junk.live[3].hold = "forever".into();
        junk.next_id = 900;
        let mut c = fresh(later);
        c.seed_carried(&junk, stamp(5_000), later);
        assert_eq!(c.live_rows().count(), 1);
        assert_eq!(c.log().next_id().raw(), 900);
        let mut claims = carry.clone();
        for m in &mut claims.live {
            m.on_glass = true;
        }
        let mut c = fresh(later);
        c.seed_carried(&claims, stamp(5_000), later);
        assert!(claims.live.len() > usize::from(MAX_ROWS));
        assert!(glass_ids(&c).len() <= usize::from(MAX_ROWS));
        // Before commit the cap is the only exit. A restate of a carried row's
        // words before the commit makes it the successor's own at once: the
        // handoff cap comes off and its own hold anchors on glass there and
        // then.
        let commit = later + Duration::from_secs(20);
        assert!(child.settle(commit, true).retired.is_empty());
        let restated = later + Duration::from_secs(1);
        assert!(child.restate(
            queued,
            Restatement {
                title: Some("queued warn, restated".into()),
                ..Restatement::default()
            },
            restated
        ));
        assert_eq!(
            child.live(queued).unwrap().stale_at,
            None,
            "the handoff cap is gone"
        );
        assert_eq!(
            child.live(queued).unwrap().fold_at,
            Some(restated + HOLD_WARN),
            "its own hold, anchored at the restate"
        );
        child.after_handoff_commit(commit);
        // After it each row takes its own lifetime back; the restated row
        // keeps the anchor it already took.
        assert_eq!(
            child.live(queued).unwrap().fold_at,
            Some(restated + HOLD_WARN)
        );
        assert_eq!(
            child.live(live).unwrap().stale_at,
            Some(commit + crate::STALE_TAILED)
        );
        for id in [held, standing] {
            let row = child.live(id).unwrap();
            assert_eq!((row.stale_at, row.fold_at), (None, None), "{id}");
        }
    }

    /// The fix reaches a closed record: a config warning folded by the press
    /// that sent the person to fix it, and a repeat of it recorded while still
    /// broken, both read fixed once the fix lands (under `✓`, the fix's words,
    /// the old title the first detail line), and a relaunch replaying the log
    /// reads them fixed too. Controls: a warning with no fix words, one under
    /// another prefix, and one a changed row took over keep their `⚠`; a
    /// second fix is a no-op.
    #[test]
    fn a_problem_folded_before_its_fix_still_closes_as_fixed() {
        let now = t0();
        let mut c = fresh(now);
        let warn = |title: &str, key: &str| {
            Message::new(tags::CONFIG, Severity::Warn, title)
                .line("windw_padding \u{2192} window_padding")
                .finished_as("Misspelled setting fixed")
                .key(key)
        };
        let post = |c: &mut MessageCenter, m: Message, n: u64| c.post(m, stamp(n), now).id;
        let folded = post(&mut c, warn("Misspelled setting", "config.ignored-keys"), 1);
        let no_words = post(
            &mut c,
            Message::new(tags::CONFIG, Severity::Warn, "Font not found").key("config.fonts"),
            2,
        );
        let elsewhere = post(&mut c, warn("Misspelled setting", "privacy.x"), 3);
        let taken_over = post(
            &mut c,
            warn("Misspelled setting", "config.unaccepted-values"),
            4,
        );
        c.commit_rows(now, 3);
        for id in [folded, no_words, elsewhere, taken_over] {
            assert!(c.mark_seen(id, now + ms(1)), "the press folds it");
            assert_eq!(
                c.log().get(id).unwrap().state,
                LogState::Retired(Retired::Folded)
            );
        }
        // Still broken on the next reload: a repeat is a record.
        let repeat = post(
            &mut c,
            warn("Misspelled setting", "config.ignored-keys").hold(Hold::LogOnly),
            5,
        );
        // The problem became another: a new row under that key.
        let changed = post(
            &mut c,
            warn("2 misspelled settings", "config.unaccepted-values"),
            6,
        );
        // A live row under another prefix is not in the family.
        let bystander = post(&mut c, warn("Misspelled setting", "privacy.fda"), 7);
        let fixed_at = now + ms(10);
        assert_eq!(
            c.resolve_key_prefix("config.", Outcome::Ok, fixed_at),
            3,
            "the changed row (live) and the folded warning and its repeat (closed)"
        );
        assert!(c.live(bystander).is_some());
        let words = |c: &MessageCenter, id| {
            let rec = c.log().get(id).unwrap();
            (
                rec.title.clone(),
                rec.detail.first().cloned(),
                rec.severity,
                rec.glyph.ch(),
            )
        };
        let fixed = (
            "Misspelled setting fixed".to_string(),
            Some("Misspelled setting".to_string()),
            Severity::Success,
            '\u{2713}',
        );
        assert_eq!(words(&c, folded), fixed);
        assert_eq!(words(&c, repeat), fixed);
        assert_eq!(words(&c, changed).2, Severity::Success, "fixed while live");
        for id in [no_words, elsewhere, taken_over] {
            assert_eq!(words(&c, id).2, Severity::Warn, "{id:?} keeps its \u{26a0}");
        }
        assert_eq!(
            c.log().get(folded).unwrap().state,
            LogState::Retired(Retired::Resolved(Outcome::Ok))
        );
        assert_eq!(c.fix_closed("config.", fixed_at + ms(1)), 0, "once");
        // The relaunch: the lines this process wrote, replayed.
        let lines = c.log.drain_pending();
        let mut replayed = MessageLog::empty();
        replayed.replay_all(lines);
        let rec = replayed
            .records()
            .find(|r| r.stamp == stamp(1))
            .expect("the folded warning's record");
        assert_eq!(
            (rec.title.as_str(), rec.severity),
            ("Misspelled setting fixed", Severity::Success)
        );
        let rec = replayed.records().find(|r| r.stamp == stamp(3)).unwrap();
        assert_eq!(rec.severity, Severity::Warn, "the control replays unfixed");
    }

    /// A record is re-worded in place: it takes the new words, still one
    /// record, its retirement and instant unchanged, and a relaunch replays the
    /// new words. Negative controls: the same words write nothing and move no
    /// revision; a live row, a dismissed row and an unknown id are never
    /// re-worded here.
    #[test]
    fn a_record_is_reworded_in_place_and_replays_so() {
        let now = t0();
        let mut c = fresh(now);
        let waits = |age: &str| {
            info("Couldn't upgrade Claude in its tab yet")
                .key("harness.upgrade.s-a")
                .line(format!("behind for {age}: its own work runs"))
                .hold(Hold::LogOnly)
        };
        let id = c.post(waits("6 h"), stamp(1), now).id;
        let before = c.log().get(id).unwrap().clone();
        let lines_before = c.log().pending_len();
        let revision = c.revision();
        assert!(!c.reword_record(id, &waits("6 h")), "the same words");
        assert_eq!(c.log().pending_len(), lines_before, "nothing written");
        assert_eq!(c.revision(), revision, "nothing moved");

        assert!(c.reword_record(id, &waits("9 h")));
        assert_ne!(c.revision(), revision);
        let rec = c.log().get(id).unwrap();
        assert_eq!(rec.detail, vec!["behind for 9 h: its own work runs"]);
        assert_eq!(rec.title, before.title);
        assert_eq!(rec.state, LogState::Retired(Retired::Recorded));
        assert_eq!(rec.retired_unix_ms, before.retired_unix_ms);
        assert_eq!(rec.retired_at, before.retired_at, "nothing finished again");

        // The relaunch reads the newest words, one record.
        let lines = c.log.drain_pending();
        let mut replayed = MessageLog::empty();
        replayed.replay_all(lines.iter().map(|l| LogLine::decode(&l.encode()).unwrap()));
        let keyed: Vec<&LogRecord> = replayed
            .records()
            .filter(|r| r.key.as_deref() == Some("harness.upgrade.s-a"))
            .collect();
        assert_eq!(keyed.len(), 1);
        assert_eq!(keyed[0].detail, vec!["behind for 9 h: its own work runs"]);
        assert_eq!(keyed[0].state, LogState::Retired(Retired::Recorded));

        // Negative controls: a live row is restate's, a dismissed row keeps
        // the words it left with.
        let live = c.post(info("Rebuilding"), stamp(2), now).id;
        assert!(!c.reword_record(live, &info("Rebuilt")));
        assert_eq!(c.live(live).unwrap().msg.title, "Rebuilding");
        c.commit_rows(now, 3);
        assert!(c.dismiss(live, now + ms(1)));
        assert!(!c.reword_record(live, &info("Rebuilt")));
        assert_eq!(c.log().get(live).unwrap().title, "Rebuilding");
        let unknown = c.log().next_id();
        assert!(!c.reword_record(unknown, &info("x")), "no such record");
    }

    /// The person's word on tab `s-a`'s upgrade, as a stall's capsule.
    fn upgrade_word(word: crate::UpgradeWord) -> Intent {
        Intent::AgentUpgrade {
            tab: "s-a".to_string(),
            to: "2.1.284".to_string(),
            word,
        }
    }

    /// A record is never re-worded for capsules it does not have: words drawn
    /// for `Upgrade now` and `Not today` on a record written with `Not today`
    /// and `Skip version` are refused, nothing written, no revision (a relaunch
    /// would replay them beside the old capsules). Negative control: the words
    /// for its own capsules are taken.
    #[test]
    fn a_record_is_never_reworded_for_capsules_it_does_not_have() {
        use crate::UpgradeWord::{NotToday, Now, Skip};
        let now = t0();
        let mut c = fresh(now);
        let said = |age: &str, remedy: &str, words: [crate::UpgradeWord; 2]| {
            words.into_iter().fold(
                info("Couldn't upgrade Claude in its tab yet")
                    .key("harness.upgrade.s-a")
                    .line(format!("behind for {age}: its own work runs"))
                    .line(remedy)
                    .hold(Hold::LogOnly),
                |m, w| m.action(upgrade_word(w)),
            )
        };
        let own_work = "Upgrade now cannot move it past its own work; Skip version keeps it";
        let id = c
            .post(said("7 h", own_work, [NotToday, Skip]), stamp(1), now)
            .id;
        let lines = c.log().pending_len();
        let revision = c.revision();
        let other = said(
            "8 h",
            "Upgrade now asks it at its next pause; Skip This Version in the tab's menu keeps it",
            [Now, NotToday],
        );
        assert!(!c.reword_record(id, &other), "words for other capsules");
        assert_eq!(c.log().pending_len(), lines, "nothing written");
        assert_eq!(c.revision(), revision, "nothing moved");
        assert_eq!(
            c.log().get(id).unwrap().detail[0],
            "behind for 7 h: its own work runs"
        );
        // Negative control: its own capsules' words are taken.
        assert!(c.reword_record(id, &said("8 h", own_work, [NotToday, Skip])));
        assert_eq!(
            c.log().get(id).unwrap().detail[0],
            "behind for 8 h: its own work runs"
        );
    }

    /// A record replaced by its row says so, and a relaunch reads it so: the
    /// record retires `Superseded { by }` (words, instant and retirement
    /// instant kept, one more log line, the replay superseded too). Negative
    /// controls: a live row and a withdrawn row by an earlier record (a story
    /// is told on, never back), an unknown id, a record by itself and a record
    /// already replaced write nothing.
    #[test]
    fn a_record_replaced_by_its_row_says_so_and_replays_so() {
        let now = t0();
        let mut c = fresh(now);
        let record = c
            .post(
                info("Couldn't upgrade Claude in its tab yet")
                    .key("harness.upgrade.s-a")
                    .line("behind for 23 h: its own work runs")
                    .hold(Hold::LogOnly),
                stamp(1),
                now,
            )
            .id;
        let before = c.log().get(record).unwrap().clone();
        let row = c
            .post(
                Message::new(tags::HARNESS, Severity::Warn, "Couldn't upgrade Claude yet")
                    .key("harness.upgrade.s-a")
                    .line("behind for 24 h: its own work runs")
                    .hold(Hold::STANDING),
                stamp(2),
                now,
            )
            .id;
        let revision = c.revision();
        assert!(c.supersede_record(record, row, now));
        assert_ne!(c.revision(), revision);
        let rec = c.log().get(record).unwrap();
        assert_eq!(
            rec.state,
            LogState::Retired(Retired::Superseded { by: row })
        );
        assert_eq!((&rec.title, &rec.detail), (&before.title, &before.detail));
        assert_eq!(rec.retired_unix_ms, before.retired_unix_ms);
        assert_eq!(rec.retired_at, before.retired_at, "nothing finished again");
        let lines = c.log().pending_len();
        assert!(!c.supersede_record(record, row, now), "replaced already");
        assert!(
            !c.supersede_record(row, record, now),
            "a live row, by an earlier record: never told back"
        );
        assert!(!c.supersede_record(record, record, now));
        assert!(
            !c.supersede_record(c.log().next_id(), row, now),
            "no such record"
        );
        c.commit_rows(now, 3);
        assert!(c.withdraw(row, now + ms(1)));
        assert!(
            !c.supersede_record(row, record, now),
            "a row withdrawn, by an earlier record"
        );
        assert_eq!(c.log().pending_len(), lines + 1, "only the withdraw's line");

        let lines = c.log.drain_pending();
        let mut replayed = MessageLog::empty();
        replayed.replay_all(lines.iter().map(|l| LogLine::decode(&l.encode()).unwrap()));
        let rec = replayed
            .records()
            .find(|r| r.stamp == stamp(1))
            .expect("the record");
        assert_eq!(
            rec.state,
            LogState::Retired(Retired::Superseded { by: row })
        );
        assert_eq!(rec.detail, before.detail);
    }

    /// ONE STORY, TOLD ON UNDER ANOTHER KEY: a tab's notice that moves to its
    /// next state is a row of another key, and the row it replaces — live, or
    /// one the owner read (a standing row folds on its read) — retires
    /// `Superseded` by it, so the story is one row on the page. NEGATIVE
    /// CONTROLS: a story that ended elsewhere (resolved by its reporter,
    /// answered by the owner) or an open exception is never folded into a new
    /// row; a successor no record names writes nothing.
    #[test]
    fn a_row_or_a_read_record_is_told_on_under_another_key_and_an_ended_story_is_not() {
        let now = t0();
        let mut c = fresh(now);
        let warn = |title: &str, key: &str| {
            Message::new(tags::HARNESS, Severity::Warn, title)
                .key(key)
                .hold(Hold::STANDING)
        };
        let si = c.post(warn("Login expired", "n.s-1.si"), stamp(1), now).id;
        c.commit_rows(now, 3);
        let ca = c
            .post(warn("Sign-in cancelled", "n.s-1.ca"), stamp(2), now)
            .id;
        assert!(c.supersede_record(si, ca, now + ms(1)), "a live row");
        assert!(c.live(si).is_none(), "it left the glass");
        assert_eq!(
            c.log().get(si).unwrap().retired(),
            Some(&Retired::Superseded { by: ca })
        );
        assert!(c.live(ca).is_some());
        // The owner reads the cancelled notice; the story moves on.
        assert!(c.mark_seen(ca, now + ms(2)));
        assert_eq!(c.log().get(ca).unwrap().retired(), Some(&Retired::Folded));
        let nr = c.post(warn("Login expired", "n.s-1.nr"), stamp(3), now).id;
        assert!(c.supersede_record(ca, nr, now + ms(3)), "a read record");
        assert_eq!(
            c.log().get(ca).unwrap().retired(),
            Some(&Retired::Superseded { by: nr })
        );
        // NEGATIVE CONTROLS.
        assert!(c.resolve(nr, Outcome::Ok, now + ms(4)));
        let next = c.post(warn("Login expired", "n.s-1.si"), stamp(4), now).id;
        assert!(
            !c.supersede_record(nr, next, now),
            "a story its reporter resolved"
        );
        let asked = c.post(ask("Allow access?"), stamp(5), now).id;
        assert!(c.answer(asked, "Not now", now + ms(5)));
        assert!(!c.supersede_record(asked, next, now), "the owner's answer");
        let entry = c
            .post(
                info("A fact for later").later(crate::need::LaterAsk::person(
                    crate::need::Why::OnlyYou(crate::need::Reason::new("only you can")),
                    crate::need::Then::step("Do the thing"),
                    crate::need::Fingerprint::new("n", "", "later"),
                    crate::need::Resurface::WAIT,
                )),
                stamp(6),
                now,
            )
            .id;
        let after = c.post(info("Later still"), stamp(7), now).id;
        assert!(!c.supersede_record(entry, after, now), "an open exception");
        assert!(
            !c.supersede_record(next, c.log().next_id(), now),
            "no such successor"
        );
        assert!(c.live(next).is_some(), "nothing moved");
    }

    /// A RECORD FIXED BY NAME: a standing warning whose row left the glass —
    /// withdrawn as its point moved on, or read — and whose cause its
    /// reporter then saw settled reads fixed: `Resolved(Ok)` in the fix words
    /// under `✓`, its old title the first detail line, one more log line that
    /// replays so, and the read's standing quiet over (the next occurrence is
    /// news). NEGATIVE CONTROLS: before the fix the read keeps a repost quiet;
    /// a live row, a replaced record, a resolved one, empty words and an
    /// unknown id write nothing.
    #[test]
    fn a_record_fixed_by_name_reads_fixed_and_ends_its_quiet() {
        let now = t0();
        let mut c = fresh(now);
        let warn = |key: &str| {
            use crate::need::{Ask, Reason, StandingWhy, Then};
            Message::new(tags::HARNESS, Severity::Warn, "Login expired")
                .now(Ask::standing(
                    StandingWhy::OnlyYou(Reason::new("Claude Code's login expired")),
                    Then::step("Type /login in the tab"),
                ))
                .key(key)
        };
        let gone = c.post(warn("n.s-1.nr"), stamp(1_000), now).id;
        assert!(c.withdraw(gone, now + ms(1)));
        assert!(c.fix_record(gone, "Login back", stamp(9_000), now + ms(2)));
        let rec = c.log().get(gone).unwrap();
        assert_eq!(rec.retired(), Some(&Retired::Resolved(Outcome::Ok)));
        assert_eq!(rec.title, "Login back");
        assert_eq!(
            rec.detail,
            [
                "Login expired",
                "Claude Code's login expired",
                "Type /login in the tab"
            ]
        );
        assert_eq!(rec.severity, Severity::Success);
        assert_eq!(rec.retired_unix_ms, Some(9_000));
        assert!(
            !c.fix_record(gone, "Login back", stamp(9_500), now),
            "resolved"
        );
        // A read keeps the key quiet until the fix ends it.
        let read = c.post(warn("n.s-1.si"), stamp(10_000), now).id;
        c.commit_rows(now, 3);
        assert!(c.mark_seen(read, now + ms(3)));
        assert_eq!(
            c.post(warn("n.s-1.si"), stamp(11_000), now).outcome,
            PostOutcome::Suppressed,
            "NEGATIVE CONTROL: the read keeps it quiet"
        );
        assert!(c.fix_record(read, "Login back", stamp(12_000), now + ms(4)));
        assert_eq!(
            c.post(warn("n.s-1.si"), stamp(13_000), now).outcome,
            PostOutcome::New,
            "the next loss is news"
        );
        // NEGATIVE CONTROLS.
        let live = c.live_by_key("n.s-1.si").unwrap().id;
        assert!(
            !c.fix_record(live, "Login back", stamp(14_000), now),
            "live"
        );
        let told = c.post(warn("n.s-1.ca"), stamp(15_000), now).id;
        assert!(c.supersede_record(live, told, now));
        assert!(
            !c.fix_record(live, "Login back", stamp(16_000), now),
            "replaced"
        );
        assert!(c.withdraw(told, now + ms(5)));
        assert!(!c.fix_record(told, "", stamp(17_000), now), "no words");
        assert!(!c.fix_record(c.log().next_id(), "Login back", stamp(17_000), now));
        // The fix replays.
        let lines = c.log.drain_pending();
        let mut replayed = MessageLog::empty();
        replayed.replay_all(lines.iter().map(|l| LogLine::decode(&l.encode()).unwrap()));
        let rec = replayed.get(gone).expect("the record");
        assert_eq!(rec.state, LogState::Retired(Retired::Resolved(Outcome::Ok)));
        assert_eq!(rec.title, "Login back");
        assert_eq!(rec.severity, Severity::Success);
    }

    /// A live row becomes its own record in place: it leaves the glass and
    /// retires `Recorded` under the new words and mark. Negative controls:
    /// words drawn for other capsules leave the row's own words; an id no
    /// longer live is refused.
    #[test]
    fn a_live_row_becomes_its_own_record_in_place() {
        use crate::UpgradeWord::{NotToday, Now, Skip};
        let now = t0();
        let mut c = fresh(now);
        let warn = |age: &str| {
            Message::new(tags::HARNESS, Severity::Warn, "Couldn't upgrade Claude yet")
                .key("harness.upgrade.s-a")
                .line(format!("behind for {age}: a draft waits in its prompt"))
                .hold(Hold::STANDING)
                .action(upgrade_word(NotToday))
                .action(upgrade_word(Skip))
        };
        let asks = |age: &str, words: [crate::UpgradeWord; 2]| {
            words.into_iter().fold(
                info("Couldn't upgrade Claude in its tab yet")
                    .key("harness.upgrade.s-a")
                    .line(format!("behind for {age}: its own work runs"))
                    .hold(Hold::LogOnly),
                |m, w| m.action(upgrade_word(w)),
            )
        };
        let id = c.post(warn("8 h"), stamp(1), now).id;
        c.commit_rows(now, 3);
        assert!(c.keep_as_record(id, &asks("8 h", [NotToday, Skip]), now + ms(1)));
        assert!(c.live(id).is_none(), "off the glass");
        let rec = c.log().get(id).unwrap();
        assert_eq!(rec.state, LogState::Retired(Retired::Recorded));
        assert_eq!(rec.title, "Couldn't upgrade Claude in its tab yet");
        assert_eq!(rec.detail, vec!["behind for 8 h: its own work runs"]);
        assert_eq!(rec.severity, Severity::Info, "the record's mark");
        assert!(!c.keep_as_record(id, &asks("9 h", [NotToday, Skip]), now + ms(2)));

        // Negative control: words for other capsules; the row keeps its own.
        let other = c
            .post(warn("8 h").key("harness.upgrade.s-b"), stamp(2), now)
            .id;
        assert!(c.keep_as_record(other, &asks("8 h", [Now, NotToday]), now + ms(3)));
        let rec = c.log().get(other).unwrap();
        assert_eq!(rec.state, LogState::Retired(Retired::Recorded));
        assert_eq!(
            rec.detail,
            vec!["behind for 8 h: a draft waits in its prompt"]
        );
        assert_eq!(
            rec.severity,
            Severity::Info,
            "the record's mark all the same"
        );
    }

    /// The log states the outcome the reporter claimed, and only it: a live
    /// row withdrawn with no echo keeps its title, one withdrawn Complete is
    /// logged finished, one withdrawn Fault, one resolved Warn and one gone
    /// stale are logged stopped, and one resolved Ok is logged finished; the
    /// live title is the record's first detail line when the title changed.
    #[test]
    fn a_retired_live_rows_record_names_only_the_outcome_it_claimed() {
        let now = t0();
        let mut c = fresh(now);
        let live = || {
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools").hold(
                Hold::Live {
                    stale_after: crate::STALE_TAILED,
                },
            )
        };
        let post = |c: &mut MessageCenter, n: u64| {
            c.post(live().key(&format!("pass.{n}")), stamp(n), now).id
        };
        let plain = post(&mut c, 1);
        let complete = post(&mut c, 2);
        let fault = post(&mut c, 3);
        let warned = post(&mut c, 4);
        let delivered = post(&mut c, 5);
        c.commit_rows(now, 3);
        assert!(c.withdraw(plain, now + ms(1)));
        assert!(c.withdraw_with(complete, EchoKind::Complete, now + ms(2)));
        assert!(c.withdraw_with(fault, EchoKind::Fault, now + ms(3)));
        assert!(c.resolve(warned, Outcome::Warn, now + ms(4)));
        assert!(c.resolve(delivered, Outcome::Ok, now + ms(5)));
        let stale = post(&mut c, 6);
        let settled = c.settle(now + crate::STALE_TAILED + ms(1), true);
        assert_eq!(settled.retired, vec![(stale, Retired::Stale)]);
        // A standing warning its reporter resolved Ok: fixed, with its
        // declared words, its old title kept as the first detail line.
        let warning = c
            .post(
                Message::new(tags::CONFIG, Severity::Warn, "Misspelled setting")
                    .line("scrollbak_lines \u{2192} scrollback_lines")
                    .finished_as("Misspelled setting fixed")
                    .key("config.aterm"),
                stamp(7),
                now,
            )
            .id;
        assert!(c.resolve(warning, Outcome::Ok, now + crate::STALE_TAILED + ms(2)));
        // The mark is how it ended: ✓ Success delivered and fixed, ⚠ Warn
        // stopped, the severity's own icon ended (a plain withdraw claims no
        // outcome). Delivered work keeps no in-flight frame.
        let ok = (Severity::Success, '\u{2713}');
        let warn = (Severity::Warn, '\u{26a0}');
        let ended = ("Installing ended", Some("Installing ALab tools"));
        let finished = ("Installed ALab tools", None);
        let stopped = ("Installing stopped", Some("Installing ALab tools"));
        let fixed = ("Misspelled setting fixed", Some("Misspelled setting"));
        // The Retired line carries them: a relaunch replaying the file reads
        // what the ring does.
        let lines = c.log.drain_pending();
        let mut file = MessageLog::empty();
        file.replay_all(lines.iter().map(|l| LogLine::decode(&l.encode()).unwrap()));
        for (case, id, want_words, want_mark) in [
            ("plain", plain, ended, (Severity::Info, '\u{2139}')),
            ("complete", complete, finished, ok),
            ("fault", fault, stopped, warn),
            ("warned", warned, stopped, warn),
            ("delivered", delivered, finished, ok),
            ("stale", stale, stopped, warn),
            ("fixed", warning, fixed, ok),
        ] {
            for (log, side) in [(c.log(), "ring"), (&file, "file")] {
                let rec = log.get(id).unwrap();
                let words = (rec.title.as_str(), rec.detail.first().map(String::as_str));
                assert_eq!(words, want_words, "{case} ({side})");
                assert_eq!((rec.severity, rec.glyph.ch()), want_mark, "{case} ({side})");
            }
        }
    }

    /// A row a Warn pushes off the band is released: `is_queued()` agrees with
    /// `on_glass()`, `settle` does not fold it `Folded` while it sits in the
    /// queue, and a re-promotion re-anchors (a row queued behind a long hold
    /// would otherwise arrive already expired).
    #[test]
    fn a_row_pushed_off_glass_by_a_warn_does_not_burn_its_hold() {
        let now = t0();
        let mut c = fresh(now);
        let r: Vec<MessageId> = (0..3)
            .map(|i| c.post(info(&format!("r{i}")), stamp(i), now + ms(i)).id)
            .collect();
        c.commit_rows(now + ms(3), 3);
        assert_eq!(glass_ids(&c), r);
        let warn = c
            .post(
                Message::new(tags::UPDATE, Severity::Warn, "Update failed"),
                stamp(9),
                now + ms(10),
            )
            .id;
        c.commit_rows(now + ms(10), 3);
        assert_eq!(
            glass_ids(&c),
            vec![warn, r[0]],
            "two slots plus the overflow row"
        );
        assert_eq!(c.queued(), 2);
        for id in [r[1], r[2]] {
            let row = c.live(id).unwrap();
            assert!(!glass_ids(&c).contains(&id));
            assert!(
                row.is_queued() || row.fold_at.is_none(),
                "{id}: off glass yet is_queued() is {} and its hold still runs (fold_at {:?})",
                row.is_queued(),
                row.fold_at
            );
        }
        let settled = c.settle(now + ms(3) + HOLD_INFO, true);
        for (id, how) in &settled.retired {
            assert!(
                *id == r[0] || *how != Retired::Folded,
                "{id} folded as {how:?} after spending {:?} of its {HOLD_INFO:?} hold off glass",
                HOLD_INFO - ms(7)
            );
        }
    }

    /// At one committed row the single row's `+N ›` capsule counts the queue;
    /// when a queued row folds Unseen the painted capsule changes and `settle`
    /// says so, so a host keyed on `glass_changed` repaints exactly when
    /// `fingerprint(cols)` moves.
    #[test]
    fn settle_reports_the_plus_n_capsule_change() {
        let now = t0();
        let mut c = fresh(now);
        let a = c.post(info("a").hold(Hold::STANDING), stamp(1), now).id;
        c.post(
            Message::new(tags::PACKAGES, Severity::Success, "b"),
            stamp(2),
            now,
        );
        c.post(
            Message::new(tags::PACKAGES, Severity::Success, "c"),
            stamp(3),
            now,
        );
        c.commit_rows(now, 1);
        assert_eq!(glass_ids(&c), vec![a]);
        let before = c.presentation(80, &char_width, None, Links::Painted);
        assert_eq!(before.rows[0].capsules.last().unwrap().text, "+2 \u{203a}");
        let settled = c.settle(now + OVERFLOW_PATIENCE_MIN, true);
        assert_eq!(settled.retired.len(), 2);
        let after = c.presentation(80, &char_width, None, Links::Painted);
        assert_ne!(before, after, "the capsule now reads Details ›");
        assert!(
            settled.glass_changed,
            "the visible capsule changed ({:?} → {:?}) but glass_changed is false",
            before.rows[0].capsules.last().unwrap().text,
            after.rows[0].capsules.last().unwrap().text
        );
    }
}

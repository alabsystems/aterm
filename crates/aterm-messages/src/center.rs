// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-wide [`MessageCenter`]: active rows, hysteresis, completion echoes, and log.

use std::sync::Arc;

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

const FILING_SKEW_MS: u64 = 3_600_000;

const STANDING_PATIENCE: Duration = Duration::from_hours(24);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// A live row that delivered.
    Delivered,
    /// A live row that failed or went silent.
    Stopped,
    /// A live row withdrawn with no outcome to claim.
    Ended,
    /// A standing warning or error its reporter resolved Ok.
    Fixed,
    /// Anything else keeps its words and its mark.
    Kept,
}

impl Ending {
    fn of(msg: &Message, how: &Retired, echo: Option<EchoKind>) -> Self {
        let live = matches!(msg.hold, Hold::Live { .. });
        if msg.is_ask() {
            return Self::Kept;
        }
        match how {
            Retired::Resolved(Outcome::Ok) if live => Self::Delivered,
            Retired::Withdrawn if live => match echo {
                Some(EchoKind::Complete) => Self::Delivered,
                Some(EchoKind::Fault) => Self::Stopped,
                Some(EchoKind::Vanish) | None => Self::Ended,
            },
            Retired::Resolved(Outcome::Warn) | Retired::Stale if live => Self::Stopped,
            Retired::Resolved(Outcome::Ok)
                if msg.severity >= Severity::Warn && msg.finished.is_some() =>
            {
                Self::Fixed
            }
            _ => Self::Kept,
        }
    }
}

fn outcome_title(msg: &Message, how: &Retired, echo: Option<EchoKind>) -> Option<String> {
    let own_end = msg.finished.as_deref() == Some(msg.title.as_str());
    let words = match Ending::of(msg, how, echo) {
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
    /// The work was delivered.
    Complete,
    /// It failed or was refused.
    Fault,
    /// It left with no outcome to claim.
    Vanish,
}

impl EchoKind {
    /// How long the echo holds its slot from a bar at `from_permille` (a busy row's from 0).
    #[must_use]
    pub fn span(self, from_permille: u16) -> Duration {
        echo_span(self, from_permille, false)
    }

    /// The echo a retirement leaves by default.
    #[must_use]
    pub(crate) const fn for_retired(how: &Retired) -> Option<Self> {
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
    /// The row was busy (the comet).
    pub indeterminate: bool,
    /// A busy row's comet, `since` into its motion when the row retired.
    pub comet_since: Option<Duration>,
    /// How long the work ran (the frozen readout).
    pub elapsed: Duration,
    /// When the echo began.
    pub started: Instant,
    /// When it ends.
    pub until: Instant,
    /// The visual slot it holds.
    pub slot: u16,
    /// The load words the row showed.
    pub load: Option<Load>,
    /// The loads the row reserved its slot for (kept so the row does not reflow).
    pub load_slot: Loads,
}

impl Echo {
    /// The load slot the row laid out, for the echo to keep.
    #[must_use]
    pub const fn slot(&self) -> Loads {
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
    /// When the work began: the post, kept across a supersede by key while the activity stays.
    pub started_at: Instant,
    /// Bumped by every restate.
    pub revision: u32,
    /// The current action list's version.
    action_revision: Option<u64>,
    /// Duplicate posts folded into it.
    pub repeats: u32,
    /// When the row reached the glass, where its hold anchors; `None` while queued.
    pub on_glass_since: Option<Instant>,
    /// Default/For/Ask rows: `None` until first glass.
    pub fold_at: Option<Instant>,
    /// Live rows: `posted_at` (or the last restate) + `stale_after`.
    pub stale_at: Option<Instant>,
    /// `reveal_at` (or the post) + `max(hold, OVERFLOW_PATIENCE_MIN)`.
    pub patience_at: Instant,
    /// Where the patience clock runs from: the reveal (or the post), then the
    /// last demote; a restated hold changes the span, never this anchor.
    patience_from: Instant,
    /// Details was opened.
    pub seen: bool,
    /// The last capsule pressed.
    pub acted: Option<(ActionIndex, Instant)>,
    /// Eligible for the glass: false inside the progress grace.
    pub revealed: bool,
    /// When an unrevealed row becomes eligible.
    pub reveal_at: Option<Instant>,
    /// The latest a held-back reveal may wait to: its grace's end plus [`REVEAL_DEFER_MAX`].
    reveal_cap: Option<Instant>,
    /// The motion epoch: set when the row reaches the glass, reset when its activity changes.
    pub motion_since: Option<Instant>,
    /// The ETA estimator, fed while the meter carries an amount.
    pub track: ProgressTrack,
    /// The declared heavy load, and when it began.
    pub load: Option<(Load, Instant)>,
    /// The load words are up: at once for a load the row was posted with, and [`LOAD_AFTER`]
    /// after it began for a load a later restate declared.
    pub load_shown: bool,
    /// The loads the row's slot is reserved for ([`Message::loads`]).
    pub load_slot: Loads,
    /// The fill the bar glides from, and when the glide began.
    pub glide: Option<(u16, Instant)>,
    /// Re-seeded from the carry and not yet past the handoff commit.
    carried_pending: bool,
    /// The mark (severity, glyph) the record's Posted line carries.
    logged_mark: (Severity, Glyph),
    /// The wall clock at `posted_at` ([`Live::wall_at`]).
    wall_anchor: u64,
}

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

    /// The action-list revision to capture with this row's id before queuing an action.
    #[must_use]
    pub const fn action_revision(&self) -> Option<u64> {
        self.action_revision
    }

    /// `true` while not on glass.
    #[must_use]
    pub const fn is_queued(&self) -> bool {
        self.on_glass_since.is_none()
    }

    /// `true` for work in flight whose indicator moves.
    #[must_use]
    pub(crate) const fn is_animated(&self) -> bool {
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

    /// Live progress: work in flight with its animated indicator.
    #[must_use]
    pub(crate) const fn is_progress(&self) -> bool {
        self.is_animated()
    }

    /// The activity byte the fingerprint folds and the motion epoch keys on: 0 no indicator,
    /// 1 a fill, 2 busy.
    #[must_use]
    pub const fn activity(&self) -> u8 {
        match &self.msg.meter {
            Some(m) if m.fill_permille.is_some() => 1,
            Some(m) if m.busy => 2,
            _ => 0,
        }
    }

    /// The fill the bar shows at `now`.
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

    /// The fill's edge at `now`, Q16 of the row, unquantized.
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

    /// When the row's glide lands.
    #[must_use]
    pub fn glide_end(&self, now: Instant) -> Option<Instant> {
        let to = self.msg.meter.as_ref()?.fill_permille?;
        let (from, since) = self.glide?;
        let end = since + glide_span(from, to);
        (now < end).then_some(end)
    }

    /// The hold's span.
    #[must_use]
    pub(crate) fn hold_span(&self) -> Duration {
        hold_span(self.msg.hold, self.msg.severity)
    }

    /// The wall clock now: the wall at `posted_at` plus the monotonic elapsed.
    #[must_use]
    pub(crate) fn wall_at(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.posted_at).as_millis();
        self.wall_anchor
            .saturating_add(u64::try_from(elapsed).unwrap_or(u64::MAX))
    }

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

    /// Whether this row duplicates `msg`.
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

    /// The load words the row shows now.
    pub(crate) fn shown_load(&self) -> Option<Load> {
        self.load.filter(|_| self.load_shown).map(|(l, _)| l)
    }

    /// The load words the row says on the glass.
    pub(crate) fn said_load(&self) -> Option<Load> {
        self.shown_load().filter(|l| self.msg.primary != Some(*l))
    }
}

fn admit_restated_hold(need: &Need, current: Hold, asked: Hold) -> (Option<Hold>, bool) {
    match asked {
        Hold::Standing(_) | Hold::Ask { .. } => match need {
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
    /// The same words were already live.
    Duplicate,
    /// An exception's words changed in place.
    Restated,
    /// A dismissed exception raised again inside its quiet span.
    Suppressed,
}

/// What one family pass did ([`MessageCenter::sync_digest`]).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DigestSync {
    /// Entries that were not open and now are.
    pub posted: usize,
    /// Their keys, in the order the pass named them.
    pub posted_keys: Vec<String>,
    /// Entries whose words changed in place.
    pub restated: usize,
    /// Facts the owner filed that the pass named again inside their span.
    pub suppressed: usize,
    /// Entries that had gone unnamed for the whole clock ([`crate::need::RESOLVE_AFTER`]).
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
    /// Something painted changed.
    pub glass_changed: bool,
    /// Rows that retired, in order.
    pub retired: Vec<(MessageId, Retired)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visual {
    Live(MessageId),
    Echo(usize),
}

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
    confirmations: Arc<crate::ConfirmationSource>,
    live: Vec<Live>,
    glass: Vec<MessageId>,
    echoes: Vec<Echo>,
    log: MessageLog,
    committed_rows: u16,
    shrink_since: Option<Instant>,
    /// The windows' chrome regrow floor the host handed at its last commit.
    regrow_floor: Option<Instant>,
    revision: u64,
    /// Restatements can move a future eta boundary without changing the words painted now.
    motion_input_epoch: u64,
    born: Instant,
    /// The display's frame period, which glides and echoes are read on while they are in flight.
    refresh: Duration,
    /// Problems that left the glass unfixed in this process.
    unfixed: Vec<Unfixed>,
    /// The exceptions ([`Need::Later`]).
    digest: Digest,
    /// Messages the door demoted to a record ([`Message::admit`]).
    demotions: u32,
    /// The key (else the title) of the message the door demoted last.
    last_demoted: Option<String>,
}

#[derive(Clone, Debug)]
struct Unfixed {
    id: MessageId,
    key: String,
    fixed: String,
}

const UNFIXED_CAP: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaterVerdict {
    /// The owner filed the fact inside its quiet span.
    Quiet(MessageId),
    /// The entry at this index already says it.
    Repeat(usize),
    /// The entry at this index takes the new words in place.
    Restate(usize),
    /// A new entry.
    New,
}

impl MessageCenter {
    /// A center over a (possibly replayed) log without conditional settlement.
    /// Hosts that can obtain entropy use [`Self::with_confirmations`] instead.
    #[must_use]
    pub fn new(log: MessageLog, now: Instant) -> Self {
        Self::with_confirmations(log, now, Arc::new(crate::ConfirmationSource::new(None)))
    }

    /// A center sharing the host incarnation's clock-independent confirmation source.
    /// Clones keep sharing this source; restoring a log must not reset it.
    #[must_use]
    pub fn with_confirmations(
        log: MessageLog,
        now: Instant,
        confirmations: Arc<crate::ConfirmationSource>,
    ) -> Self {
        Self {
            confirmations,
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

    /// Set the display's frame period, which glides and echoes are read on while in flight:
    /// clamped to 240 Hz at the fastest and [`ANIM_FRAME`] at the slowest.
    pub fn set_refresh(&mut self, period: Duration) {
        self.refresh = period.clamp(Duration::from_micros(4_167), ANIM_FRAME);
    }

    /// The display frame period the fine motion is read on.
    #[must_use]
    pub const fn refresh(&self) -> Duration {
        self.refresh
    }

    /// Post a message: normalized, minted, logged.
    pub fn post(&mut self, msg: Message, stamp: WallStamp, now: Instant) -> Posted {
        let mut msg = msg.normalized();
        match msg.admit() {
            Admission::Kept => {}
            Admission::Demoted => {
                self.demotions = self.demotions.saturating_add(1);
                self.last_demoted = Some(msg.key.clone().unwrap_or_else(|| msg.title.clone()));
            }
        }
        if matches!(msg.need, Need::Later(_)) {
            return self.post_later(msg, stamp, now);
        }
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
        if let Some(key) = msg.key.as_deref() {
            self.unfixed.retain(|u| u.key != key);
        }
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

    fn retire_at(&mut self, i: usize, how: Retired, now: Instant) -> Live {
        let echo = EchoKind::for_retired(&how);
        self.retire_at_with(i, how, echo, now)
    }

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
        let mark = outcome_mark(&row.msg, &how, echo).or_else(|| {
            let shown = (row.msg.severity, row.msg.glyph);
            (row.logged_mark != shown).then_some(shown)
        });
        let detail: Vec<String> = match Ending::of(&row.msg, &how, echo) {
            Ending::Delivered => Vec::new(),
            _ if how == Retired::Quit => quit_reading(row)
                .into_iter()
                .chain(row.msg.detail.iter().cloned())
                .take(crate::DETAIL_LINES_CAP)
                .collect(),
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

    /// Close as fixed every problem under `prefix` that left the glass unfixed in this process.
    pub fn fix_closed(&mut self, prefix: &str, now: Instant) -> usize {
        self.fix_closed_as(prefix, None, now)
    }

    /// [`Self::fix_closed`] in `words` when given.
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

    /// Close as fixed, in the words `fixed`, one record that already left the glass, at `wall` but
    /// never before it first retired. `false` and nothing written for a live row, an open
    /// exception, empty words, or a record already resolved, answered, superseded or carried.
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

    /// Re-word a record ([`Retired::Recorded`]) in place.
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

    /// Replace a row or a record by the later row `by` that tells its story.
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
        self.unfixed.retain(|u| u.id != id);
        self.revision += 1;
        true
    }

    /// Turn a live row into its own record in place.
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

    /// Change a live row in place.
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
        let eta_moved = row.track.eta(now) != eta_before;
        if row.msg.actions != before.actions {
            row.action_revision = row.action_revision.and_then(|rev| rev.checked_add(1));
        }
        let changed = row.msg != before || was_carried || eta_moved;
        if changed {
            row.revision = row.revision.wrapping_add(1);
            self.revision += 1;
        }
        let shown = (row.msg.severity, row.msg.glyph);
        if shown != (before.severity, before.glyph) {
            self.log.show_mark(id, shown);
        }
        true
    }

    /// The reporter finished with it.
    pub fn resolve(&mut self, id: MessageId, outcome: Outcome, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at(i, Retired::Resolved(outcome), now);
        self.rebalance(now);
        true
    }

    /// Resolve every live row and every digest exception whose key starts with `prefix`.
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

    /// Withdraw every live row and every digest exception whose key starts with
    /// `prefix`: their reporter stops saying them without having read their cause end
    /// (a switch turned off, a check that read nothing), so none is filed as fixed and
    /// no filing is closed.
    pub fn withdraw_key_prefix(&mut self, prefix: &str, now: Instant) -> usize {
        let ids: Vec<MessageId> = self
            .live
            .iter()
            .filter(|l| l.msg.key.as_deref().is_some_and(|k| k.starts_with(prefix)))
            .map(|l| l.id)
            .collect();
        for id in &ids {
            self.withdraw(*id, now);
        }
        ids.len() + self.end_digest_prefix(prefix, &Retired::Withdrawn, now)
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

    /// The reporter withdrew it with no outcome to claim.
    pub fn withdraw(&mut self, id: MessageId, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at(i, Retired::Withdrawn, now);
        self.rebalance(now);
        true
    }

    /// Aterm is quitting: every open row retires [`Retired::Quit`] with no echo (there is no next
    /// frame). Returns how many rows it closed.
    pub fn quit(&mut self, now: Instant) -> usize {
        let mut closed = 0;
        while !self.live.is_empty() {
            self.retire_at_with(0, Retired::Quit, None, now);
            closed += 1;
        }
        self.echoes.clear();
        closed
    }

    /// [`Self::withdraw`] with the echo named.
    pub fn withdraw_with(&mut self, id: MessageId, echo: EchoKind, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at_with(i, Retired::Withdrawn, Some(echo), now);
        self.rebalance(now);
        true
    }

    /// [`Self::withdraw`] whose log entry keeps `title` and `detail` as its final words.
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

    /// The person answered the row with a word its reporter confirmed was taken.
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

    /// The row was read: a Default, For or Standing row folds now; Live and Ask rows stay.
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

    /// Restart the patience of a queued row nobody could have seen: it waits a whole span from
    /// `now`.
    pub fn restart_patience(&mut self, id: MessageId, now: Instant) -> bool {
        let Some(row) = self.live.iter_mut().find(|l| l.id == id && l.is_queued()) else {
            return false;
        };
        row.patience_from = now;
        row.patience_at = at(now, row.hold_span().max(OVERFLOW_PATIENCE_MIN));
        true
    }

    /// A capsule press.
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

    /// Perform an action only while the id and action-list revision captured by its caller stand.
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

    /// Reveal the rows whose grace is over, show the load words that have lasted, retire what is
    /// due (folds only when `holds`: a handoff freeze suspends them, never staleness or patience),
    /// end the echoes that are over, and re-rank the queue.
    pub fn settle(&mut self, now: Instant, holds: bool) -> Settled {
        let before = self.glass_signature();
        let mut revealed = false;
        for row in &mut self.live {
            if !row.revealed && row.reveal_at.is_some_and(|t| now >= t) {
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
        let glass_changed = revealed || before != self.glass_signature();
        if glass_changed {
            self.revision += 1;
        }
        Settled {
            glass_changed,
            retired,
        }
    }

    /// The next instant a settle would act.
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

    /// The rows eligible for the glass.
    #[must_use]
    pub(crate) fn eligible(&self) -> usize {
        self.live.iter().filter(|l| l.revealed).count()
    }

    /// `min(MAX_ROWS, eligible)` (the overflow row counts).
    #[must_use]
    pub fn wanted_rows(&self) -> u16 {
        u16::try_from(self.eligible())
            .unwrap_or(u16::MAX)
            .min(MAX_ROWS)
    }

    /// Commit the row count with hysteresis over `min(wanted, afford)`.
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

    /// Hand in the windows' chrome regrow floor ([`crate::chrome`]).
    pub const fn set_regrow_floor(&mut self, floor: Option<Instant>) {
        self.regrow_floor = floor;
    }

    /// When the pending quiet shrink's own quiet runs out, before the chrome regrow floor.
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
    pub const fn committed_rows(&self) -> u16 {
        self.committed_rows
    }

    fn overflow_up(&self) -> bool {
        let committed = usize::from(self.committed_rows);
        self.eligible() > committed && committed > 1
    }

    fn positions(&self) -> usize {
        let committed = usize::from(self.committed_rows);
        if self.overflow_up() {
            committed - 1
        } else {
            committed.min(self.eligible() + self.echoes.len())
        }
    }

    fn slots(&self) -> usize {
        self.positions().saturating_sub(self.echoes.len())
    }

    fn overflow_hidden(&self) -> Option<usize> {
        self.overflow_up()
            .then(|| self.eligible().saturating_sub(self.slots()))
    }

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

    fn demote(&mut self, id: MessageId, now: Instant) {
        if let Some(row) = self.live.iter_mut().find(|l| l.id == id) {
            row.on_glass_since = None;
            row.fold_at = None;
            row.patience_from = now;
            row.patience_at = at(now, row.hold_span().max(OVERFLOW_PATIENCE_MIN));
        }
    }

    fn by_id(&self, id: MessageId) -> &Live {
        self.live
            .iter()
            .find(|l| l.id == id)
            .unwrap_or(&self.live[0])
    }

    fn index(&self, id: MessageId) -> Option<usize> {
        self.live.iter().position(|l| l.id == id)
    }

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

    fn plus_hidden(&self) -> usize {
        if self.committed_rows == 1 {
            self.eligible().saturating_sub(self.slots())
        } else {
            0
        }
    }

    /// The width law over the committed rows.
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
                    row.pct = None;
                }
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

    /// The committed live rows, top to bottom (the echoes and the overflow slot excluded).
    pub fn on_glass(&self) -> impl Iterator<Item = &Live> {
        self.glass.iter().map(|id| self.by_id(*id))
    }

    /// Whether a committed row is busy ([`Meter::busy`]).
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

    /// Whether a new live row can be added without exceeding the live-row cap.
    #[must_use]
    pub const fn has_live_capacity(&self) -> bool {
        self.live.len() < MAX_LIVE
    }

    /// Every unretired row, post order.
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
    pub const fn log(&self) -> &MessageLog {
        &self.log
    }

    /// The lines the host appends, oldest first.
    #[cfg(test)]
    pub(crate) fn drain_new_for_persist(&mut self) -> Vec<LogLine> {
        self.log.drain_pending()
    }

    /// The lines the host appends, oldest first, each with its file ([`crate::Shelf`]).
    pub fn drain_shelved_for_persist(&mut self) -> Vec<(LogLine, crate::Shelf)> {
        self.log.drain_shelved()
    }

    /// Bumps on every observable change.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// A center that replaces another moves its revision past the one it replaces.
    pub fn follow_revision(&mut self, replaced: u64) {
        self.revision = self.revision.max(replaced.saturating_add(1));
    }

    /// The gate a page reads to know it is out of date.
    #[must_use]
    pub const fn projection_revision(&self) -> u64 {
        self.revision.wrapping_add(self.digest.revision())
    }

    /// Changes on every live restatement.
    #[must_use]
    pub const fn motion_input_epoch(&self) -> u64 {
        self.motion_input_epoch
    }

    /// The repaint-key term.
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
        if self.overflow_up() {
            h.str(self.hidden_progress_words().as_deref().unwrap_or(""));
        }
        h.num(self.eligible() as u64);
        h.finish()
    }

    /// The frame instant for `now`.
    #[must_use]
    pub fn frame_instant(&self, now: Instant) -> Instant {
        let since = now.saturating_duration_since(self.born).as_millis();
        let frame = ANIM_FRAME.as_millis().max(1);
        self.born + Duration::from_millis(u64::try_from(since / frame * frame).unwrap_or(0))
    }

    /// The first grid instant at or after `d`, and after `now`.
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

    /// The fine frame instant for `now`.
    #[must_use]
    pub fn fine_instant(&self, now: Instant) -> Instant {
        let since = now.saturating_duration_since(self.born).as_micros();
        let period = self.refresh.as_micros().max(1);
        self.born + Duration::from_micros(u64::try_from(since / period * period).unwrap_or(0))
    }

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

    /// One frame of motion for the presentation `p` at `now` in `look`.
    #[must_use]
    pub fn motion(&self, p: &Presentation, now: Instant, look: Look) -> BandMotion {
        let q = self.frame_instant(now);
        let qf = self.fine_instant(now);
        let rows = p
            .rows
            .iter()
            .map(|layout| self.row_motion(layout, q, qf, look, p.cols))
            .collect();
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

    /// The next instant the band needs a frame for `p` in `look`.
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
                        fold(g, true);
                        continue;
                    }
                    let glide_end = row.glide_end(q);
                    let glints = look.graded && !stalled;
                    let travel_end = glint_at(q, epoch)
                        .filter(|_| glints)
                        .and_then(|phase| q.checked_sub(phase))
                        .map(|start| start + GLINT_TRAVEL);
                    let busy_until = glide_end.into_iter().chain(travel_end).max();
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
                    let changed =
                        busy_until.and_then(|end| first_change(next_frame, end + ANIM_FRAME));
                    match changed {
                        Some(g) => fold(g, false),
                        None if glints => {
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

    /// Plain data for the successor.
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

    /// The rows a handoff did not carry end with the parent.
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

    /// Never mint an id below `raw` from now on.
    pub fn raise_next_id(&mut self, raw: u64) {
        self.log.raise_to(raw);
    }

    /// Re-seed the carry.
    pub fn seed_carried(&mut self, carry: &Carry, wall: WallStamp, now: Instant) {
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
            let mut msg = msg;
            let _ = msg.admit();
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

    /// Re-read the parent's record after the handoff committed.
    pub fn merge_reloaded_log(&mut self, disk: &MessageLog) {
        self.log.merge_reloaded(disk);
        for row in &mut self.live {
            if let Some(mark) = disk.open_posted_mark(row.id, row.stamp, &row.msg.tag) {
                row.logged_mark = mark;
            }
        }
        self.revision += 1;
    }

    /// At Commit, after [`Self::merge_reloaded_log`].
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

    /// The handoff committed.
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

impl MessageCenter {
    /// The open exceptions, worst first then longest waiting, for every observer to read.
    #[must_use]
    pub fn digest(&self) -> DigestView {
        crate::digest::view_of(&self.digest)
    }

    /// The keys of the open exceptions under `prefix`.
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

    /// Moves only when an exception is added, changed or filed.
    #[must_use]
    pub const fn digest_revision(&self) -> u64 {
        self.digest.revision()
    }

    /// The opaque freshness of one open entry, independent of digest painting.
    pub(crate) fn digest_confirmation(&self, id: MessageId) -> Option<u64> {
        self.digest
            .find_id(id)
            .map(|i| self.digest.get(i).confirmation)
    }

    /// How many messages the door demoted to a record.
    #[must_use]
    pub const fn demotions(&self) -> u32 {
        self.demotions
    }

    /// The key (else the title) of the message the door demoted last.
    #[must_use]
    pub fn last_demoted(&self) -> Option<&str> {
        self.last_demoted.as_deref()
    }

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
                e.confirm(&self.confirmations);
                e.repeats = e.repeats.saturating_add(1);
                e.ask.take_action_of(&ask);
                return Posted {
                    id: e.id,
                    outcome: PostOutcome::Duplicate,
                };
            }
            LaterVerdict::Restate(i) => {
                let e = self.digest.get_mut(i);
                e.confirm(&self.confirmations);
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
            confirmation: self.confirmations.take(),
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

    /// How [`Self::post`] would land the exception `msg` (normalized, as `post` makes it).
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
    pub(crate) const fn digest_len(&self) -> usize {
        self.digest.len()
    }

    fn index_by_key(&self, key: &str) -> Option<usize> {
        self.live
            .iter()
            .position(|l| l.msg.key.as_deref() == Some(key))
    }

    fn quiet_record(&self, key: &str, ask: &LaterAsk, stamp: WallStamp) -> Option<MessageId> {
        let last = self.log.last_word(key, true)?;
        if !last.filed_exception() {
            return None;
        }
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

    /// Whether the owner's own Dismiss of the exception `key` still keeps it quiet at `stamp`.
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

    /// Dismiss all: every open exception is filed `Dismissed` at one wall stamp.
    pub fn file_digest(&mut self, stamp: WallStamp, now: Instant) -> usize {
        let n = self.digest.len();
        while self.digest.len() > 0 {
            self.file_digest_at(0, Retired::Dismissed, stamp.unix_ms, now);
        }
        n
    }

    /// Dismiss one exception.
    pub fn file_digest_one(&mut self, id: MessageId, stamp: WallStamp, now: Instant) -> bool {
        let Some(i) = self.digest.find_id(id) else {
            return false;
        };
        self.file_digest_at(i, Retired::Dismissed, stamp.unix_ms, now);
        true
    }

    /// A capsule pressed on an exception.
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

    /// Resolve every digest exception under `prefix` as `outcome`: its cause ended.
    pub fn resolve_digest_prefix(&mut self, prefix: &str, outcome: Outcome, now: Instant) -> usize {
        self.end_digest_prefix(prefix, &Retired::Resolved(outcome), now)
    }

    /// Every digest exception whose key starts with `prefix` ends `how`.
    fn end_digest_prefix(&mut self, prefix: &str, how: &Retired, now: Instant) -> usize {
        let mut n = 0;
        loop {
            let hit = self
                .digest
                .iter()
                .position(|e| e.msg.key.as_deref().is_some_and(|k| k.starts_with(prefix)));
            let Some(i) = hit else { break };
            let wall = self.digest.get(i).wall_at(now);
            self.file_digest_at(i, how.clone(), wall, now);
            n += 1;
        }
        n
    }

    /// The digest's own clock, called by the host's settle (`stamp` is the real wall clock).
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

    /// The next instant [`Self::settle_digest`] has something to end.
    #[must_use]
    pub fn digest_deadline(&self) -> Option<Instant> {
        self.digest.next_end()
    }

    /// A reporter's whole answer for one family of exceptions (level-triggered).
    pub fn sync_digest(
        &mut self,
        prefix: &str,
        desired: Vec<Message>,
        stamp: WallStamp,
        now: Instant,
    ) -> DigestSync {
        self.sync_digest_keeping(prefix, desired, &[], stamp, now)
    }

    /// [`Self::sync_digest`], where the pass also names `kept`.
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

    /// The cause ended and its reporter says so (a verified grant, the setting the
    /// entry was about set right). A reporter that only stops saying it withdraws it
    /// ([`Self::withdraw_digest`]).
    pub fn clear_digest(&mut self, key: &str, now: Instant) -> bool {
        self.end_digest_key(key, Retired::Resolved(Outcome::Ok), None, now)
            .is_some()
    }

    /// The entry under exactly `key` ends with no outcome to claim (`Withdrawn`): its
    /// reporter stops saying it (a kind this build no longer reports, or a fact another
    /// surface now carries) without having read its cause end.
    pub fn withdraw_digest(&mut self, key: &str, now: Instant) -> bool {
        self.end_digest_key(key, Retired::Withdrawn, None, now)
            .is_some()
    }

    /// The entry under exactly `key` ends `how`.
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

    /// The cause ended and nothing is open under `key`.
    pub fn forget_filing(&mut self, key: &str, wall: WallStamp, now: Instant) -> bool {
        let changed =
            self.digest.find_key(key).is_none() && self.log.resolve_filing(key, wall.unix_ms, now);
        if changed {
            self.revision += 1;
        }
        changed
    }

    /// A standing row's fact is new and nothing is live under its `key`.
    pub fn end_standing_quiet(&mut self, key: &str, wall: WallStamp, now: Instant) -> bool {
        if self.index_by_key(key).is_some() {
            return false;
        }
        let Some(read) = self.quiet_standing(key, wall) else {
            return false;
        };
        self.log.withdraw_read(read, wall.unix_ms, now)
    }

    /// At Commit of a seamless update, in the successor.
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
                let _ = self.digest.remove(i);
                out.closed += 1;
            } else {
                i += 1;
            }
        }
        out.adopted = self.adopt_digest(wall, now);
        out
    }

    /// At launch: re-open the exceptions the replayed log left open.
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
                anchor_wall: wall.unix_ms,
                repeats: r.repeats,
                confirmation: self.confirmations.take(),
                missed_at: None,
                restated_at: now,
            });
            n += 1;
        }
        n
    }
}

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
            row.load_slot = row.load_slot.union(row.msg.loads.reserved_with(l));
        }
    }
    row.msg.meter = meter;
}

#[derive(Clone, Copy)]
struct Indicator {
    fill: Option<u16>,
    busy: bool,
    moving: bool,
}

struct RowIn<'m> {
    kind: RowKind,
    msg: &'m Message,
    ind: Indicator,
    /// The load said, and the loads the slot is reserved for.
    load: (Option<Load>, Loads),
}

struct LayIn<'a> {
    plus: usize,
    links: Links,
    cols: usize,
    width: &'a dyn Fn(&str) -> usize,
    home: Option<&'a str>,
}

#[derive(Default)]
struct BandForms {
    labels: Vec<&'static str>,
    short_time: bool,
}

impl BandForms {
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
            if forms.labels.contains(&spec.full_label) && !spec.short.is_empty() {
                spec.long.clone_from(&spec.short);
            }
            if echo { spec.blanked() } else { spec }
        })
        .collect();
    if links == Links::Painted {
        let link = CapsuleSpec::details(plus);
        capsules.push(if echo { link.blanked() } else { link });
    }
    let stats = msg.meter.as_ref().map_or("", |m| m.stats.as_str());
    let moving_fill = ind.moving && ind.fill.is_some();
    let step = matches!(
        msg.need(),
        crate::need::Need::Now(ask)
            if msg.excerpt && msg.detail.first().map(String::as_str) == Some(ask.then().words())
    );
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
        layout.stats = None;
    }
    layout
}

fn text_ticks(row: &Live, layout: &RowLayout, q: Instant, look: Look) -> [Option<Instant>; 3] {
    let e = q.saturating_duration_since(row.started_at);
    let elapsed = layout
        .elapsed
        .map(|_| row.started_at + e + elapsed_word_change(e));
    if layout.eta.is_some() {
        [elapsed, row.track.next_change(q), None]
    } else {
        let stall = (look.graded && layout.meter.is_some())
            .then(|| row.track.stall_onset(q))
            .flatten();
        [elapsed, stall, None]
    }
}

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
    let tg = if row.glide_end(tf).is_some() { tf } else { t };
    let glint = if graded { glint_at(tg, epoch) } else { None };
    bar_at(edge, glint, graded, cols)
}

/// How far ahead [`next_comet_frame`] scans at most.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a period over a frame: a few hundred, far inside u32"
)]
const COMET_SCAN_FRAMES: u32 = (crate::COMET_PERIOD.as_millis() / ANIM_FRAME.as_millis()) as u32;

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

fn echo_motion(e: &Echo, layout: &RowLayout, q: Instant, look: Look, cols: usize) -> RowMotion {
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

    #[test]
    fn exhausted_entry_confirmations_never_wrap_or_allow_conditional_settlement() {
        use crate::wire::{NoticeRequest, WireGate};
        let now = t0();
        let mut c = MessageCenter::with_confirmations(
            MessageLog::empty(),
            now,
            Arc::new(crate::ConfirmationSource::new(Some(u64::MAX - 1))),
        );
        let NoticeRequest::Later(later) = NoticeRequest::parse(
            "later k Renew the key -- only you hold its passphrase -- Renew the key",
        )
        .unwrap() else {
            panic!("a later entry");
        };
        let msg = later.into_message();
        let id = c.post(msg.clone(), stamp(0), now).id;
        assert_eq!(c.digest_confirmation(id), Some(u64::MAX - 1));
        let before = (c.revision(), c.digest_revision(), c.projection_revision());
        for _ in 0..2 {
            assert_eq!(
                c.post(msg.clone(), stamp(0), now).outcome,
                PostOutcome::Duplicate
            );
        }
        let rows = crate::wire::message_rows(
            &c,
            &crate::wire::parse_read_args("live need=later").unwrap(),
            0,
            &str::to_string,
        );
        assert!(rows[0].ends_with(&format!(" rev={}", u64::MAX)));
        let mut gate = WireGate::default();
        for revision in [u64::MAX - 1, u64::MAX] {
            let req = NoticeRequest::parse(&format!("settle k {id} {revision}")).unwrap();
            let result = crate::wire::apply(&mut c, &mut gate, req, stamp(0), now);
            assert_eq!(result.reply, "OK settled=- how=gone");
            assert_eq!(
                (result.paint, result.note),
                (crate::wire::Paint::None, false)
            );
        }
        assert!(c.digest_entry(id).is_some());
        assert_eq!(
            (c.revision(), c.digest_revision(), c.projection_revision()),
            before
        );
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

    #[test]
    fn an_unrevealed_row_counts_as_queued_and_goes_before_a_row_on_glass() {
        let now = t0();
        let mut c = fresh(now);
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
        let mut c = fresh(now);
        let q = c.post(info("q").key("k"), stamp(1), now).id;
        let q2 = c.post(info("q2").key("k"), stamp(2), now).id;
        assert!(c.live(q).is_none() && c.live(q2).unwrap().fold_at.is_none());
    }

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

    #[test]
    fn a_heartbeat_that_moves_the_estimate_is_a_change() {
        use crate::model::{Amount, Unit};
        let now = t0();
        let mut c = fresh(now);
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
        let mut t = now;
        for k in 1..=20u64 {
            t += ms(500);
            assert!(restate(&mut c, k, t));
        }
        assert!(
            matches!(c.live(id).unwrap().track.eta(t), Eta::Remaining(_)),
            "latched"
        );
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
        let late = c.post(standing("late"), stamp(9), now + ms(9)).id;
        assert_eq!(glass_ids(&c), vec![r[1], r3, late]);
    }

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

    fn busy(title: &str) -> Message {
        Message::new(tags::UPDATE, Severity::Info, title)
            .line("installs within a minute")
            .meter(Meter::busy(""))
            .hold(Hold::Live {
                stale_after: crate::STALE_UPDATE,
            })
            .key("update.progress")
    }

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
        let at_b = log.get(b).unwrap();
        assert_eq!(
            (at_b.title.as_str(), &at_b.state),
            ("Indexing docs", &LogState::Posted)
        );
    }

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
        let mut old = carry.clone();
        for m in &mut old.live {
            m.busy = false;
        }
        let mut older = fresh(now);
        older.seed_carried(&old, stamp(0), now);
        assert!(!busy_of(&older, flow));
    }

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
        let repeat = post(
            &mut c,
            warn("Misspelled setting", "config.ignored-keys").hold(Hold::LogOnly),
            5,
        );
        let changed = post(
            &mut c,
            warn("2 misspelled settings", "config.unaccepted-values"),
            6,
        );
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

    fn upgrade_word(word: crate::UpgradeWord) -> Intent {
        Intent::AgentUpgrade {
            tab: "s-a".to_string(),
            to: "2.1.284".to_string(),
            word,
        }
    }

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
        assert!(c.reword_record(id, &said("8 h", own_work, [NotToday, Skip])));
        assert_eq!(
            c.log().get(id).unwrap().detail[0],
            "behind for 8 h: its own work runs"
        );
    }

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
        assert!(c.mark_seen(ca, now + ms(2)));
        assert_eq!(c.log().get(ca).unwrap().retired(), Some(&Retired::Folded));
        let nr = c.post(warn("Login expired", "n.s-1.nr"), stamp(3), now).id;
        assert!(c.supersede_record(ca, nr, now + ms(3)), "a read record");
        assert_eq!(
            c.log().get(ca).unwrap().retired(),
            Some(&Retired::Superseded { by: nr })
        );
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
        let lines = c.log.drain_pending();
        let mut replayed = MessageLog::empty();
        replayed.replay_all(lines.iter().map(|l| LogLine::decode(&l.encode()).unwrap()));
        let rec = replayed.get(gone).expect("the record");
        assert_eq!(rec.state, LogState::Retired(Retired::Resolved(Outcome::Ok)));
        assert_eq!(rec.title, "Login back");
        assert_eq!(rec.severity, Severity::Success);
    }

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
        let ok = (Severity::Success, '\u{2713}');
        let warn = (Severity::Warn, '\u{26a0}');
        let ended = ("Installing ended", Some("Installing ALab tools"));
        let finished = ("Installed ALab tools", None);
        let stopped = ("Installing stopped", Some("Installing ALab tools"));
        let fixed = ("Misspelled setting fixed", Some("Misspelled setting"));
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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The center: every unretired message, the stable-ordered glass, the D1
//! row hysteresis, the settle law and the log. One instance per process;
//! every method that reads time takes `now`.
//!
//! # Attention and motion (design §10.4.3)
//!
//! * **The reveal.** A row posted with `reveal_after(d)` is not ELIGIBLE for
//!   the glass until `d` after its post: every read of the eligible count
//!   goes through [`MessageCenter::eligible`], so an invisible row never
//!   moves the overflow row, the `+N` or the row count, and a row resolved
//!   inside its grace never re-grids at all.
//! * **The echo.** A MOVING row on the glass that retires leaves a completion
//!   ECHO in its slot for at most 850 ms ([`EchoKind::span`]): the end of its
//!   own indicator's animation, not a row — never counted in
//!   [`MessageCenter::wanted_rows`], so it costs no re-grid (the shrink clock
//!   starts at the resolve and [`crate::SHRINK_QUIET`] outlasts the echo).
//! * **The motion.** [`MessageCenter::motion`] reads the frame at an injected
//!   instant on one grid anchored at the center's birth, and
//!   [`MessageCenter::motion_deadline`] names the next frame the band needs —
//!   `None` when nothing moving or echoing is on the glass. What moves is the
//!   METER'S state (design ruling 139): a fill draws the bar, `busy` the
//!   comet and the spinner, neither nothing — never the hold.

use crate::animate::{
    Anim, BandMotion, Look, Pace, RowMotion, Surface, anim_ms, bar, bar_glints, comet, comet_phase,
    echo, glide, glint_at, next_glint_start, spin_at, stalled_bar, track,
};
use crate::carry::{CarriedMessage, Carry};
use crate::glass::{
    CapsuleSpec, Fnv, Links, Presentation, RowKind, RowLayout, RowSpec, finish_title, layout_row,
    outranks, overflow_spec, rank,
};
use crate::log::{FinalWords, LogLine, LogRecord, MessageLog, Retired};
use crate::model::{
    ActionIndex, Glyph, Hold, Intent, Load, Message, MessageId, Meter, Restatement, Severity, Tag,
    WallStamp,
};
use crate::progress::{
    Eta, ProgressTrack, clock_word_change, clock_words, elapsed_word_change, elapsed_words,
    eta_words, eta_words_short,
};
use crate::text::clip;
use crate::words::abbreviate_paths_in;
use crate::{
    ANIM_FRAME, DETAIL_LINE_CAP, DETAIL_LINES_CAP, DONE_WORD, Duration, ECHO_FILL, FAILED_WORD,
    FILL_GLIDE, GLINT_TRAVEL, Instant, LOAD_AFTER, MAX_ACTIONS, MAX_LIVE, MAX_ROWS,
    OVERFLOW_PATIENCE_MIN, SHRINK_QUIET, STALE_HANDOFF, STALLED_WORD, TITLE_CAP,
};

/// A Standing row's patience in the queue — a standing condition is not
/// folded unseen for a day.
const STANDING_PATIENCE: Duration = Duration::from_hours(24);
/// The longest span any hold is honoured for (instant arithmetic stays
/// finite whatever a reporter types).
const MAX_SPAN: Duration = Duration::from_hours(365 * 24);

/// How a resolved row ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Fine: the work was DELIVERED (a refusal or a failure is `Warn`).
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
    /// How long the echo holds its slot: 850 / 600 / 250 ms.
    #[must_use]
    pub const fn span(self) -> Duration {
        match self {
            Self::Complete => Duration::from_millis(850),
            Self::Fault => Duration::from_millis(600),
            Self::Vanish => Duration::from_millis(250),
        }
    }

    /// The echo a retirement leaves by default (design §10.4.3's table).
    #[must_use]
    pub fn for_retired(how: &Retired) -> Option<Self> {
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
            | Retired::Carried => None,
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
    /// runs the comet on from there, so its first frame is the next live one
    /// (design ruling 162).
    pub comet_since: Option<Duration>,
    /// How long the work ran (the frozen readout).
    pub elapsed: Duration,
    /// When the echo began.
    pub started: Instant,
    /// When it ends.
    pub until: Instant,
    /// The visual slot it holds.
    pub slot: u16,
    /// The load words the row showed, kept so the row does not reflow.
    pub load: Option<Load>,
    /// The row reserved the load slot (kept so the row does not reflow).
    pub load_slot: bool,
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
    /// When the WORK began: the post, carried across a supersede by key while
    /// the activity is the same (as the motion epoch is). The one clock the
    /// band's elapsed words, an echo's and the screen reader's description
    /// all read — a re-worded row of the same work never restarts it, and
    /// the description never lags the band by a reveal grace (review
    /// 2026-09-24).
    pub started_at: Instant,
    /// Bumped by every restate.
    pub revision: u32,
    /// Duplicate posts folded in.
    pub repeats: u32,
    /// `None` while queued; the hold anchors when this is set.
    pub on_glass_since: Option<Instant>,
    /// Default/For/Ask rows: `None` until first glass.
    pub fold_at: Option<Instant>,
    /// Live rows: `posted_at` (or the last restate) + `stale_after`.
    pub stale_at: Option<Instant>,
    /// `reveal_at` (or the post) + `max(hold, OVERFLOW_PATIENCE_MIN)`: a row
    /// still queued past this folds Unseen.
    pub patience_at: Instant,
    /// Details was opened.
    pub seen: bool,
    /// The last capsule pressed, and when.
    pub acted: Option<(ActionIndex, Instant)>,
    /// Eligible for the glass (design §10.4.3, E2): false inside the
    /// progress grace.
    pub revealed: bool,
    /// When an unrevealed row becomes eligible.
    pub reveal_at: Option<Instant>,
    /// The motion epoch: set when the row reaches the glass, reset when its
    /// activity changes (none ↔ a fill ↔ busy).
    pub motion_since: Option<Instant>,
    /// The ETA estimator, fed while the meter carries an amount.
    pub track: ProgressTrack,
    /// The declared heavy load, and when it began.
    pub load: Option<(Load, Instant)>,
    /// The load words are up: AT ONCE for a load the row was posted with —
    /// such a row is on the glass because it is heavy (review round 2,
    /// 2026-09-23) — and [`LOAD_AFTER`] after it began for a load a restate
    /// declared after none; flipped by `settle`, so the layout stays
    /// time-free.
    pub load_shown: bool,
    /// The row has declared a load at some point in its life: its layout
    /// RESERVES the load slot from then on (glass.rs, step 1), so words
    /// arriving, changing resource or leaving never re-grid a moving row.
    pub load_slot: bool,
    /// The fill the bar glides FROM, and when the glide began.
    pub glide: Option<(u16, Instant)>,
    /// Re-seeded from the carry and not yet past the handoff commit.
    carried_pending: bool,
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
            repeats: 1,
            on_glass_since: None,
            fold_at: None,
            stale_at,
            patience_at: at(reveal_at.unwrap_or(now), span.max(OVERFLOW_PATIENCE_MIN)),
            seen: false,
            acted: None,
            revealed: reveal_at.is_none(),
            reveal_at,
            motion_since: None,
            track,
            load,
            load_shown: load.is_some(),
            load_slot: load.is_some(),
            glide: None,
            carried_pending: false,
            msg,
        }
    }

    /// `true` while not on glass.
    #[must_use]
    pub fn is_queued(&self) -> bool {
        self.on_glass_since.is_none()
    }

    /// `true` for work in flight whose indicator MOVES (design ruling 139):
    /// a busy meter (the comet), or a fill on a [`Hold::Live`] row (the glide
    /// and the glint). A Live row with neither — blocked on the person — is
    /// still, and so is a held row's fill.
    #[must_use]
    pub fn is_animated(&self) -> bool {
        match &self.msg.meter {
            Some(m) if m.busy => true,
            Some(m) => m.fill_permille.is_some() && matches!(self.msg.hold, Hold::Live { .. }),
            None => false,
        }
    }

    /// Whether the row is BUSY ([`Meter::busy`]).
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.msg.meter.as_ref().is_some_and(|m| m.busy)
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
    pub fn shown_fill(&self, now: Instant) -> Option<u16> {
        let to = self.msg.meter.as_ref()?.fill_permille?;
        match self.glide {
            Some((from, since)) => {
                let t = now.saturating_duration_since(since);
                Some(if t < FILL_GLIDE {
                    glide(from, to, t)
                } else {
                    to
                })
            }
            None => Some(to),
        }
    }

    /// The hold's span: the severity's for `Default`, the named one for
    /// `For` / `Live` / `Ask`, a day for `Standing`, zero for `LogOnly`.
    #[must_use]
    pub fn hold_span(&self) -> Duration {
        hold_span(self.msg.hold, self.msg.severity)
    }

    /// The wall clock now, from the ingress stamp plus the monotonic
    /// elapsed — the engine reads no clock of its own.
    #[must_use]
    pub fn wall_at(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.posted_at).as_millis();
        self.stamp
            .unix_ms
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
            Hold::Standing | Hold::LogOnly => {}
        }
    }

    /// A duplicate of `msg`: the same key AND the same words, or no key on
    /// either side and the same tag and words.
    fn duplicates(&self, msg: &Message) -> bool {
        let same_words = self.msg.title == msg.title && self.msg.detail == msg.detail;
        match (&self.msg.key, &msg.key) {
            (Some(a), Some(b)) => a == b && same_words && self.msg.actions == msg.actions,
            (None, None) => self.msg.tag == msg.tag && same_words,
            _ => false,
        }
    }

    /// The load words the row shows now, if any.
    fn shown_load(&self) -> Option<Load> {
        self.load.filter(|_| self.load_shown).map(|(l, _)| l)
    }
}

fn hold_span(hold: Hold, severity: Severity) -> Duration {
    match hold {
        Hold::Default => severity.default_hold(),
        Hold::For(d) | Hold::Ask { for_: d } | Hold::Live { stale_after: d } => d.min(MAX_SPAN),
        Hold::Standing => STANDING_PATIENCE,
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

/// What [`Settled::glass_changed`] compares.
type GlassSignature = (
    Vec<(MessageId, u32, bool)>,
    Vec<(u16, MessageId, EchoKind)>,
    Option<usize>,
    usize,
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
    revision: u64,
    born: Instant,
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
            revision: 0,
            born: now,
        }
    }

    // ---- posting -----------------------------------------------------

    /// Post a message: normalized, minted, logged. Same key and different
    /// words → the live row is superseded in place (same slot, fresh
    /// anchor; an unrevealed row keeps the EARLIER reveal, a revealed one
    /// stays revealed); the same words → a duplicate (`repeats += 1`, hold
    /// re-anchored on glass, nothing logged); `Hold::LogOnly` → recorded,
    /// never live. Past [`MAX_LIVE`] a queued held row is evicted (lowest
    /// severity, oldest first), then a queued Live one, then the same on
    /// glass; asks and standing rows are never evicted.
    pub fn post(&mut self, msg: Message, stamp: WallStamp, now: Instant) -> Posted {
        let msg = msg.normalized();
        self.revision += 1;
        if msg.hold == Hold::LogOnly {
            let id = self.log.mint();
            self.log
                .record_posted(LogRecord::from_posted(id, stamp, &msg));
            self.log.record_retired(
                id,
                Retired::Recorded,
                stamp.unix_ms,
                now,
                FinalWords {
                    title: &msg.title,
                    detail: &msg.detail,
                    repeats: 1,
                },
            );
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
            self.log_retired(&old, Retired::Superseded { by: id }, now);
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

    /// Past [`MAX_LIVE`] the victim is a held (Default/For) row — QUEUED
    /// before on glass, lowest severity first, oldest first: the queue's own
    /// drain order, and the pending queue's drop-oldest policy, so a flood of
    /// posts thins the queue and never takes a row off the glass under the
    /// pointer (§1.4: stable order). Then a Live row the same way. Ask and
    /// Standing rows are never evicted, so when nothing else is left the
    /// newcomer itself goes. An unrevealed row counts as queued.
    fn evict_past_cap(&mut self, newcomer: MessageId, now: Instant) {
        while self.live.len() > MAX_LIVE {
            let others = || {
                self.live
                    .iter()
                    .enumerate()
                    .filter(move |(_, l)| l.id != newcomer)
            };
            let held = others()
                .filter(|(_, l)| matches!(l.msg.hold, Hold::Default | Hold::For(_)))
                .min_by_key(|(_, l)| {
                    (
                        l.on_glass_since.is_some(),
                        l.msg.severity,
                        l.posted_at,
                        l.id,
                    )
                })
                .map(|(i, _)| i);
            let live = || {
                others()
                    .filter(|(_, l)| matches!(l.msg.hold, Hold::Live { .. }))
                    .min_by_key(|(_, l)| (l.on_glass_since.is_some(), l.posted_at, l.id))
                    .map(|(i, _)| i)
            };
            let victim = held.or_else(live).or_else(|| self.index(newcomer));
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

    /// [`Self::retire_at`] naming the echo: the ECHO is recorded at the row's
    /// visual slot BEFORE it leaves — only for a revealed LIVE row on the
    /// glass (a held row, an ask, a standing row, a queued or unrevealed one
    /// has no indicator to end). The log line and every watcher see exactly
    /// what they saw before: the row is gone at once.
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
        self.log_retired(&row, how, now);
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
        let echo = Echo {
            id: row.id,
            msg: row.msg.clone(),
            kind,
            from_permille: row.shown_fill(now).unwrap_or(0),
            indeterminate: fill.is_none(),
            comet_since: fill
                .is_none()
                .then_some(row.motion_since)
                .flatten()
                .map(|epoch| now.saturating_duration_since(epoch)),
            elapsed: now.saturating_duration_since(row.started_at),
            started: now,
            until: at(now, kind.span()),
            slot,
            load: row.shown_load(),
            load_slot: row.load_slot,
        };
        self.echoes.retain(|e| e.slot != slot);
        self.echoes.push(echo);
    }

    fn log_retired(&mut self, row: &Live, how: Retired, now: Instant) {
        self.log.record_retired(
            row.id,
            how,
            row.wall_at(now),
            now,
            FinalWords {
                title: &row.msg.title,
                detail: &row.msg.detail,
                repeats: row.repeats,
            },
        );
    }

    // ---- lifecycle ---------------------------------------------------

    /// Change a live row in place: no log line, `revision + 1` when the
    /// row's words, meter, capsules, hold or look changed; a Live row
    /// re-arms its staleness cap; a new hold re-anchors on glass. A carried
    /// row restated before the handoff commit is the successor's own from
    /// here: the handoff cap comes off with the carried flag and its own
    /// lifetime starts now (on glass its hold anchors; queued, the patience
    /// clock already runs). A new meter feeds the estimator, starts a glide
    /// from the fill shown, and restarts the load clock when the declared
    /// load changes; a change of activity restarts the motion epoch.
    /// `false` when no such live row.
    pub fn restate(&mut self, id: MessageId, r: Restatement, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        let row = &mut self.live[i];
        let activity = row.activity();
        let before = row.msg.clone();
        let eta_before = row.track.eta(now);
        if let Some(title) = r.title {
            let title = clip(&title, TITLE_CAP);
            if title != row.msg.title {
                // Declared finished words finished the OLD title.
                row.msg.finished = None;
            }
            row.msg.title = title;
        }
        if let Some(finished) = r.finished {
            row.msg.finished = finished
                .map(|w| clip(&w, TITLE_CAP))
                .filter(|w| !w.is_empty());
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
        if let Some(hold) = r.hold {
            if hold == Hold::LogOnly {
                self.retire_at(i, Retired::Folded, now);
                self.rebalance(now);
                return true;
            }
            row.msg.hold = hold;
            row.fold_at = None;
            row.stale_at = None;
            row.patience_at = at(row.posted_at, row.hold_span().max(OVERFLOW_PATIENCE_MIN));
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
        // A restatement that changed nothing the row says — a reporter's
        // heartbeat re-feeding the same reading (atpkg's, every 2 s) — is not
        // a change of the glass: no revision, so no repaint of every window
        // and no Settings publish (review 2026-09-24). Its staleness cap was
        // still re-armed above: the reporter is alive. The same reading fed
        // at a later instant CAN move the estimate, though — a re-anchor, an
        // unlatch — and no motion deadline was armed for that: the ETA slot
        // would keep its old words until something unrelated repainted it, so
        // an estimate that moved is a change of the glass too.
        let eta_moved = row.track.eta(now) != eta_before;
        if row.msg != before || was_carried || eta_moved {
            row.revision = row.revision.wrapping_add(1);
            self.revision += 1;
        }
        true
    }

    /// The reporter finished with it: retires now as `Resolved(outcome)` —
    /// a live row on the glass echoes Complete (`Ok`: the work was
    /// delivered) or Fault (`Warn`).
    pub fn resolve(&mut self, id: MessageId, outcome: Outcome, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at(i, Retired::Resolved(outcome), now);
        self.rebalance(now);
        true
    }

    /// Resolve every live row whose key starts with `prefix` (`config.` on
    /// a clean reload, `privacy.` on policy-off). Returns how many.
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
        ids.len()
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

    /// The reporter withdrew it with NO outcome to claim — a live meter
    /// whose source vanished before any marker said how the pass ended
    /// (`Retired::Withdrawn`: the log keeps the row; the `appstatus` face
    /// lists no finished activity for it, as the status bars listed none).
    /// A live row on the glass fades ([`EchoKind::Vanish`]).
    pub fn withdraw(&mut self, id: MessageId, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at(i, Retired::Withdrawn, now);
        self.rebalance(now);
        true
    }

    /// [`Self::withdraw`] with the echo named: the record stays `Withdrawn`
    /// (no outcome on `appstatus`), while the glass shows how the pass
    /// really ended — the first-run and heavy toolchain rows end Complete or
    /// Fault by the child's exit.
    pub fn withdraw_with(&mut self, id: MessageId, echo: EchoKind, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.retire_at_with(i, Retired::Withdrawn, Some(echo), now);
        self.rebalance(now);
        true
    }

    /// The row was READ — Details was opened, the body was pressed, or a
    /// navigation capsule took the person to fix it: a Default/For/Standing
    /// row folds now (E3); Live and Ask rows stay.
    pub fn mark_seen(&mut self, id: MessageId, now: Instant) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.live[i].seen = true;
        self.revision += 1;
        if matches!(
            self.live[i].msg.hold,
            Hold::Default | Hold::For(_) | Hold::Standing
        ) {
            self.retire_at(i, Retired::Folded, now);
            self.rebalance(now);
        }
        true
    }

    /// A capsule press: logs `Acted { label }` and returns the intent for
    /// the host to perform. The row retires ONLY when
    /// [`Intent::closes_row`] (`NotNow` → `Answered`); every other intent
    /// leaves it for a supersede or a fold (the R16 rule). The implicit
    /// Details capsule is [`MessageCenter::mark_seen`] and is not logged.
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

    // ---- settle ------------------------------------------------------

    /// Reveal the rows whose grace is over, retire what is due — holds only
    /// when `holds` (a handoff freeze suspends the holds, never the
    /// staleness and patience caps) — end the echoes that are over, show
    /// the load words that have lasted, then re-rank the queue.
    pub fn settle(&mut self, now: Instant, holds: bool) -> Settled {
        let before = self.glass_signature();
        let mut revealed = false;
        for row in &mut self.live {
            if !row.revealed && row.reveal_at.is_some_and(|t| now >= t) {
                row.revealed = true;
                row.reveal_at = None;
                revealed = true;
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
        Settled {
            glass_changed,
            retired,
        }
    }

    /// The next instant a settle would act on: the nearest fold (holds
    /// only), staleness, patience or reveal instant, an echo's end, a load
    /// word's arrival on the glass, or the pending shrink. `None` when
    /// nothing is armed — an armed deadline the settle then declines to act
    /// on is a wake that does nothing. These are STATE wakes; the motion's
    /// frames are [`Self::motion_deadline`]'s.
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
            .chain(self.shrink_since.map(|s| at(s, SHRINK_QUIET)))
            .min()
    }

    // ---- rows (D1) ---------------------------------------------------

    /// The rows eligible for the glass: every live row past its grace.
    #[must_use]
    pub fn eligible(&self) -> usize {
        self.live.iter().filter(|l| l.revealed).count()
    }

    /// `min(MAX_ROWS, eligible)` — the overflow row counts; 0 when empty. An
    /// echo is NEVER counted: it costs no re-grid.
    #[must_use]
    pub fn wanted_rows(&self) -> u16 {
        u16::try_from(self.eligible())
            .unwrap_or(u16::MAX)
            .min(MAX_ROWS)
    }

    /// Commit the row count with hysteresis over `min(wanted, afford)`: grow
    /// now, shrink only after [`SHRINK_QUIET`] of wanting less — unless the
    /// shrink is the WINDOW's (`afford` fell below the committed count):
    /// the quiet exists so a burst of short rows folds the grid once, and a
    /// window that just lost its last terminal row to the band is not a
    /// burst; it gets the row back now. `Some(new)` exactly when the count
    /// moved. Rows that just reached the glass anchor their holds here —
    /// only rows within the committed count ever do. A shrink drops the
    /// echoes whose slot is past the new count.
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
                let since = *self.shrink_since.get_or_insert(now);
                if afford < self.committed_rows || now >= at(since, SHRINK_QUIET) {
                    self.committed_rows = want;
                    self.shrink_since = None;
                    moved = Some(want);
                }
            }
            std::cmp::Ordering::Equal => self.shrink_since = None,
        }
        if moved.is_some() {
            self.revision += 1;
        }
        self.rebalance(now);
        moved
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

    /// LIVE slots: the positions an echo has not pinned.
    fn slots(&self) -> usize {
        self.positions().saturating_sub(self.echoes.len())
    }

    /// How many eligible rows the overflow row stands for, when it is up.
    fn overflow_hidden(&self) -> Option<usize> {
        self.overflow_up()
            .then(|| self.eligible().saturating_sub(self.slots()))
    }

    /// Fill the glass, stable order: departures leave, survivors move up,
    /// the freed bottom slot takes the best queued row (anchoring its hold
    /// now); a newcomer goes ABOVE only when it outranks every row on glass
    /// (stepping over a slot an echo pins). A row the glass has no slot for
    /// any more — the overflow row took it, a newcomer outranked it, the
    /// window shrank, an echo holds it — is released
    /// ([`MessageCenter::demote`]): not painted, so not burning its hold.
    /// Only revealed rows are candidates.
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
        loop {
            let best = self
                .live
                .iter()
                .filter(|l| l.revealed && !self.glass.contains(&l.id))
                .max_by_key(|l| rank(l))
                .map(|l| l.id);
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
            } else if slots > 0 && above {
                self.glass.insert(0, best);
                if let Some(pushed_off) = self.glass.pop() {
                    self.demote(pushed_off, now);
                }
            } else {
                break;
            }
            if let Some(row) = self.live.iter_mut().find(|l| l.id == best)
                && row.on_glass_since.is_none()
            {
                row.on_glass_since = Some(now);
                // The motion epoch is the row's FIRST glass: a row released
                // and re-placed — the transient the overflow row makes while
                // the band grows by a row — keeps its phase, so a comet never
                // jumps back to the start because another row arrived.
                row.motion_since.get_or_insert(now);
                row.arm(now);
            }
        }
        while self.glass.len() > slots {
            if let Some(pushed_off) = self.glass.pop() {
                self.demote(pushed_off, now);
            }
        }
    }

    /// A row left the glass without retiring: its anchor is released — it
    /// is queued again, under the patience clock from now, and anchors
    /// afresh when it returns (§1.4: holds anchor at first glass; a row that
    /// is not painted never burns its hold). A Live row keeps its staleness
    /// cap — silence is silence wherever the row stands — and a carried row
    /// its handoff cap.
    fn demote(&mut self, id: MessageId, now: Instant) {
        if let Some(row) = self.live.iter_mut().find(|l| l.id == id) {
            row.on_glass_since = None;
            row.fold_at = None;
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
    /// revisions and load words, the echoes, the overflow row's count, and
    /// the `+N` a single row's Details capsule carries.
    fn glass_signature(&self) -> GlassSignature {
        (
            self.glass
                .iter()
                .map(|g| {
                    let row = self.by_id(*g);
                    (*g, row.revision, row.load_shown)
                })
                .collect(),
            self.echoes.iter().map(|e| (e.slot, e.id, e.kind)).collect(),
            self.overflow_hidden(),
            self.plus_hidden(),
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

    /// The width law over the committed rows: one [`RowSpec`] per visual
    /// row — live rows and echoes — (paths under `home` printed as `~/…`),
    /// the overflow row last when the eligible set is larger than the band,
    /// `+N ›` on a single row — the links only when `links` says they are
    /// painted ([`Links`]).
    #[must_use]
    pub fn presentation(
        &self,
        cols: usize,
        width: &dyn Fn(&str) -> usize,
        home: Option<&str>,
        links: Links,
    ) -> Presentation {
        let plus = self.plus_hidden();
        let mut rows = Vec::with_capacity(usize::from(self.committed_rows));
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
                            row.shown_load(),
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
                            echo.load,
                            echo.load_slot,
                            // Only a Complete echo says the finished form: a
                            // Fault's `Downloading … failed` is honest.
                            (echo.kind == EchoKind::Complete).then(|| echo.msg.finished_title()),
                        )
                    }
                };
            let mut row = message_layout(
                kind,
                msg,
                indicator,
                (load, load_slot),
                plus,
                links,
                cols,
                width,
                home,
            );
            if let Some(words) = finished {
                finish_title(&mut row, &words, cols, width);
            }
            rows.push(row);
        }
        if let Some(hidden) = self.overflow_hidden() {
            rows.push(layout_row(&overflow_spec(hidden, links), cols, width));
        }
        Presentation { cols, rows }
    }

    /// The committed LIVE rows, top to bottom (the echoes and the overflow
    /// slot excluded).
    pub fn on_glass(&self) -> impl Iterator<Item = &Live> {
        self.glass.iter().map(|id| self.by_id(*id))
    }

    /// Whether a committed row is BUSY ([`Meter::busy`]) — what the host
    /// asks before it animates a frame or arms a frame's wake. `false` with
    /// nothing committed and for a busy row still queued. Busy never arms a
    /// deadline here and never changes the rows the band wants: a frame is
    /// the host's, and a busy row is a row like any other.
    #[must_use]
    pub fn busy_on_glass(&self) -> bool {
        self.on_glass()
            .any(|l| l.msg.meter.as_ref().is_some_and(|m| m.busy))
    }

    /// Eligible rows not on glass (the `N more`).
    #[must_use]
    pub fn queued(&self) -> usize {
        self.eligible().saturating_sub(self.glass.len())
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

    /// The lines the host appends (D4), oldest first; a trailing
    /// `Dropped` when the pending queue overflowed since the last drain.
    pub fn drain_new_for_persist(&mut self) -> Vec<LogLine> {
        self.log.drain_pending()
    }

    /// Bumps on every observable change — the Settings projection gate.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The repaint-key term: **`0` when `committed_rows() == 0`** (FL-1),
    /// else a nonzero FNV-1a over everything a presentation at `cols`
    /// depends on — the fill quantized to whole percent, the excerpt only
    /// when painted, the activity, whether an ETA slot is reserved, the load
    /// words shown, and the echoes. Nothing that MOVES is hashed: the motion
    /// layer has its own term.
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
                Hold::Live { .. } | Hold::Standing
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
            h.str(row.shown_load().map_or("", Load::words));
            h.byte(u8::from(row.load_slot));
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
        h.num(self.eligible() as u64);
        h.finish()
    }

    // ---- motion (design §10.4.6) --------------------------------------

    /// The frame instant for `now`: `born + ⌊(now − born)/ANIM_FRAME⌋·ANIM_FRAME`
    /// — the one 30 fps grid every phase is read on.
    #[must_use]
    pub fn frame_instant(&self, now: Instant) -> Instant {
        let since = now.saturating_duration_since(self.born).as_millis();
        let frame = ANIM_FRAME.as_millis().max(1);
        self.born + Duration::from_millis(u64::try_from(since / frame * frame).unwrap_or(0))
    }

    /// The first grid instant at or after `d` — and after `now`, so the
    /// frame computed at the wake is exactly the frame the deadline promised.
    fn grid_after(&self, now: Instant, d: Instant) -> Instant {
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

    /// One frame of motion for the presentation `p` at `now` in `look`:
    /// every phase read at the frame instant, one [`RowMotion`] per row of
    /// `p` (default for a row that does not move).
    #[must_use]
    pub fn motion(&self, p: &Presentation, now: Instant, look: Look) -> BandMotion {
        let q = self.frame_instant(now);
        let rows = p
            .rows
            .iter()
            .map(|layout| self.row_motion(layout, q, look))
            .collect();
        BandMotion { at: q, rows }
    }

    fn row_motion(&self, layout: &RowLayout, q: Instant, look: Look) -> RowMotion {
        match layout.kind {
            RowKind::Message(id) => match self.live(id) {
                Some(row) if row.is_animated() => live_motion(row, layout, q, look),
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
                Some(e) => echo_motion(e, layout, q, look),
                None => RowMotion::default(),
            },
            RowKind::Overflow { .. } => RowMotion::default(),
        }
    }

    /// The next instant the band needs a frame for the presentation `p` in
    /// `look`: folded ONLY over the moving rows and echoes on it — `None`
    /// when there are none (FL-1: an idle band arms nothing). The text ticks
    /// (an elapsed boundary, the ETA's next word or its running out, the
    /// onset of a stall — in the ETA slot, or in the bar's dim ink where the
    /// width starved the slot) in every look; frames for a comet and its
    /// spinner, a glide, a travelling glint and a graded echo only while
    /// Moving, and for a bar only where the frame draws something new at
    /// the row's cell resolution ([`Surface::cells`]) — a resting glint arms
    /// its next start, not a frame, and a STALLED bar's glint arms nothing
    /// (it is parked until the bytes move). Every instant snaps UP to the
    /// grid. This is the band's ONLY clock (design ruling 140).
    #[must_use]
    pub fn motion_deadline(&self, p: &Presentation, now: Instant, look: Look) -> Option<Instant> {
        let q = self.frame_instant(now);
        let next_frame = q + ANIM_FRAME;
        let moving = look.pace == Pace::Moving;
        let cols = p.cols;
        let mut best: Option<Instant> = None;
        let mut fold = |d: Instant| {
            let d = self.grid_after(now, d);
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
                        fold(d);
                    }
                    if !moving {
                        continue;
                    }
                    if layout.track.is_some() {
                        // The comet never rests (the next enters as the last
                        // one leaves) and the spinner turns with it.
                        fold(if q < epoch {
                            epoch
                        } else {
                            next_comet_frame(q, epoch, cols, look.graded)
                        });
                        continue;
                    }
                    let Some((_, _, data)) = layout.meter else {
                        continue;
                    };
                    let stalled = matches!(row.track.eta(q), Eta::Stalled);
                    // The glide and a travelling glint move the bar until
                    // `busy_until`; a STALLED bar is parked.
                    let glide_end = row
                        .glide
                        .map(|(_, since)| since + FILL_GLIDE)
                        .filter(|end| q < *end);
                    let travel_end = glint_at(q, epoch)
                        .filter(|_| look.graded && !stalled)
                        .and_then(|phase| q.checked_sub(phase))
                        .map(|start| start + GLINT_TRAVEL);
                    let busy_until = glide_end.into_iter().chain(travel_end).max();
                    // A frame is asked only where it DRAWS something new at
                    // the row's cells: at a short bar's end the glint's
                    // centre is off the fill, and a small glide can stay
                    // inside one tone step (review 2026-09-24: 29 % of a
                    // 10 % download's wakes drew nothing). Each scan stops at
                    // the first frame that differs.
                    let drawn = moving_bar(row, data, q, epoch, look.graded).cells(cols);
                    let first_change = |from: Instant, until: Instant| {
                        let mut g = self.grid_after(now, from);
                        while g <= until {
                            if moving_bar(row, data, g, epoch, look.graded).cells(cols) != drawn {
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
                        Some(g) => fold(g),
                        None if look.graded && !stalled => {
                            // At rest: the next travel's first frame that
                            // shows its glint — not its start, where the
                            // centre is still off the fill. A fill under half
                            // the glint's radius carries none.
                            let rest = busy_until.map_or(q, |e| e.max(q));
                            let shown = row.shown_fill(rest).unwrap_or(data);
                            let start = next_glint_start(rest, epoch);
                            if bar_glints(shown)
                                && let Some(g) = first_change(start, start + GLINT_TRAVEL)
                            {
                                fold(g);
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
                    let t = q.saturating_duration_since(e.started);
                    let wiping = e.kind == EchoKind::Complete && t < ECHO_FILL;
                    if (look.graded || wiping) && q < e.until {
                        fold(next_frame);
                    }
                }
                RowKind::Overflow { .. } => {}
            }
        }
        best
    }

    // ---- handoff -----------------------------------------------------

    /// Plain data for the successor: the next id and every live row (glass
    /// rows first, in order). No log lines: the parent's record is its own
    /// to the end (it flushes its writer before it execs), and the
    /// successor's begins at Commit. An unrevealed row is not carried (its
    /// work ends with this process), nor are the echoes, the estimator, the
    /// load or the motion epoch.
    #[must_use]
    pub fn carried(&self) -> Carry {
        let mut live: Vec<CarriedMessage> =
            self.on_glass().map(|l| carried_message(l, true)).collect();
        live.extend(
            self.live
                .iter()
                .filter(|l| l.revealed && !self.glass.contains(&l.id))
                .map(|l| carried_message(l, false)),
        );
        Carry {
            next_id: self.log.next_id().raw(),
            live,
        }
    }

    /// Re-seed the carry: each row keeps its id, words and hold, and takes
    /// the handoff staleness cap ([`STALE_HANDOFF`]) in place of its
    /// anchors until [`MessageCenter::after_handoff_commit`]; unknown
    /// severities, tags and intents are dropped silently; `next_id` is
    /// raised. A seeded row is revealed, and keeps its indicator: its fill,
    /// or `busy` (until the commit stills it unless restated,
    /// [`MessageCenter::after_handoff_commit`]); an older parent's carry has
    /// no `busy` and reads as still (design ruling 139).
    pub fn seed_carried(&mut self, carry: &Carry, now: Instant) {
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
                detail: c.detail.clone(),
                actions: c.actions.iter().filter_map(|a| Intent::decode(a)).collect(),
                hold,
                meter,
                key: c.key.clone(),
                origin: crate::model::Origin::Carried,
                excerpt: c.excerpt,
                reveal_after: None,
                finished: c.finished.clone(),
            }
            .normalized();
            let stamp = WallStamp { unix_ms: c.unix_ms };
            let mut entry = Live::new(id, msg, stamp, now);
            entry.fold_at = None;
            entry.stale_at = Some(at(now, STALE_HANDOFF));
            entry.carried_pending = true;
            self.log
                .adopt_carried(LogRecord::from_posted(id, stamp, &entry.msg));
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

    /// The handoff committed: every carried row still saying its carried
    /// words takes its own lifetime back — a Live row re-arms its cap, a
    /// Standing row stands, a held row ON GLASS anchors its own hold now
    /// (`Default`: the severity's; `For` / `Ask`: the named span — a
    /// question that had ten minutes keeps them), and a queued one waits for
    /// the glass unanchored, under the patience clock, as any queued row
    /// does (`fold_at: None until first glass`). A row restated before the
    /// commit already took its lifetime back then. A row no one here
    /// restated also stops animating: work that is not this process's own
    /// is shown still ([`Meter::busy`] cleared) — a restated row (the
    /// update's own "finishing") keeps what its restatement said.
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

/// What a supersede by key carries over from the row it replaces: the
/// slot, the reveal (a revealed row stays revealed; an unrevealed one keeps
/// the EARLIER clock, so Downloading → Verifying keeps its first grace), the
/// work's start (the elapsed clock) and the motion epoch while the activity
/// is the same, the estimator while the
/// series is, the load clock while a load is declared on both sides (the
/// resource may change: the machine stayed busy), the reserved load slot,
/// and a glide from the fill shown when the fill moved.
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
    entry.load_slot |= old.load_slot;
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
            row.load_slot = true;
        }
        (Some((_, since)), Some(l)) => row.load = Some((l, since)),
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

/// One message's row: the excerpt only when the row paints it, its
/// authored capsules and its link, and the indicator it moves with.
#[allow(
    clippy::too_many_arguments,
    reason = "the presentation's per-row inputs, each read once here"
)]
fn message_layout(
    kind: RowKind,
    msg: &Message,
    ind: Indicator,
    (load, load_slot): (Option<Load>, bool),
    plus: usize,
    links: Links,
    cols: usize,
    width: &dyn Fn(&str) -> usize,
    home: Option<&str>,
) -> RowLayout {
    let detail0 = msg
        .detail
        .first()
        .filter(|_| msg.excerpt)
        .map(|d| abbreviate_paths_in(d, home));
    let mut capsules: Vec<CapsuleSpec> = msg
        .actions
        .iter()
        .enumerate()
        .map(|(i, it)| CapsuleSpec::authored(it, u8::try_from(i).unwrap_or(u8::MAX)))
        .collect();
    if links == Links::Painted {
        capsules.push(CapsuleSpec::details(plus));
    }
    let stats = msg.meter.as_ref().map_or("", |m| m.stats.as_str());
    let moving_fill = ind.moving && ind.fill.is_some();
    let spec = RowSpec {
        kind,
        severity: msg.severity,
        live: matches!(msg.hold, Hold::Live { .. } | Hold::Standing),
        glyph: msg.glyph.ch(),
        title: &msg.title,
        detail0: detail0.as_deref(),
        meter: msg.meter.as_ref().map(|_| (ind.fill, stats)),
        busy: ind.busy && ind.fill.is_none(),
        animated: moving_fill,
        eta: moving_fill && msg.meter.as_ref().is_some_and(|m| m.amount.is_some()),
        load: load.filter(|_| ind.moving),
        load_slot: load_slot && ind.moving,
        capsules,
    };
    let mut layout = layout_row(&spec, cols, width);
    if matches!(kind, RowKind::Echo(_)) {
        // An echo is the indicator ending, not a reading: the stats were
        // frozen at the last read before the resolve ("198 MB / 200 MB"
        // beside "100%", review 2026-09-23), so they go — laid out as the
        // row was, so nothing on it moves at the resolve.
        layout.stats = None;
    }
    layout
}

/// A moving row's TEXT ticks at frame `q`, in every pace: its elapsed
/// words' next change; where its ETA slot is laid out, the estimate's next
/// change (the countdown's next word, the latch running out, the stall)
/// and, while the estimate is hidden, the slot's clock's next second; and
/// where the width starved that slot, a graded bar's stall onset — the
/// stall's dim ink (`stalled_bar`) is then the only stall signal left, in
/// EVERY pace, so its onset repaints the bar (review 2026-09-24).
fn text_ticks(row: &Live, layout: &RowLayout, q: Instant, look: Look) -> [Option<Instant>; 3] {
    let e = q.saturating_duration_since(row.started_at);
    let elapsed = layout
        .elapsed
        .map(|_| row.started_at + e + elapsed_word_change(e));
    if layout.eta.is_some() {
        let clock = matches!(row.track.eta(q), Eta::Hidden)
            .then(|| row.started_at + e + clock_word_change(e));
        [elapsed, row.track.next_change(q), clock]
    } else {
        let stall = (look.graded && layout.meter.is_some())
            .then(|| row.track.stall_onset(q))
            .flatten();
        [elapsed, stall, None]
    }
}

/// What a MOVING determinate bar draws at frame `t` — the one function the
/// frame ([`live_motion`]) and its deadline's look-ahead
/// ([`MessageCenter::motion_deadline`]) both read, so a deadline never asks a
/// frame the motion would not draw, nor misses one it would: the stalled ink
/// once the bytes stopped, else the fill shown (the glide) under the glint.
fn moving_bar(row: &Live, data: u16, t: Instant, epoch: Instant, graded: bool) -> Surface {
    if matches!(row.track.eta(t), Eta::Stalled) {
        return stalled_bar(data, graded);
    }
    let shown = row.shown_fill(t).unwrap_or(data);
    let glint = if graded { glint_at(t, epoch) } else { None };
    bar(shown, glint, graded)
}

/// The first grid frame after `q` at which a busy row's comet or spinner
/// DRAWS something new at `cols` cells — the next frame, except where a
/// crossing's faint last sliver and the hand-over round to the same cells;
/// the spinner turns at least every [`crate::SPIN_FRAMES`] frames, which
/// bounds the scan.
fn next_comet_frame(q: Instant, epoch: Instant, cols: usize, graded: bool) -> Instant {
    let at = |g: Instant| {
        (
            comet(g.saturating_duration_since(epoch), graded).cells(cols),
            spin_at(g, epoch),
        )
    };
    let drawn = at(q);
    let mut g = q + ANIM_FRAME;
    for _ in 1..crate::SPIN_FRAMES {
        if at(g) != drawn {
            break;
        }
        g += ANIM_FRAME;
    }
    g
}

/// A moving row's motion at frame `q`: the bar for a fill, the comet and the
/// spinner for a busy row (the unlit track, still), and its time words.
fn live_motion(row: &Live, layout: &RowLayout, q: Instant, look: Look) -> RowMotion {
    let epoch = row.motion_since.unwrap_or(q);
    let stalled = matches!(row.track.eta(q), Eta::Stalled);
    let (surface, anim, glyph) = match (layout.meter, layout.track.is_some(), look.pace) {
        (Some((_, _, data)), _, Pace::Moving) => {
            let shown = if stalled {
                data
            } else {
                row.shown_fill(q).unwrap_or(data)
            };
            let glint_ms = glint_at(q, epoch)
                .filter(|_| look.graded && !stalled && bar_glints(shown))
                .map(anim_ms);
            (
                moving_bar(row, data, q, epoch, look.graded),
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
        (None, true, Pace::Moving) => {
            let since = q.saturating_duration_since(epoch);
            let spin = spin_at(q, epoch);
            (
                comet(since, look.graded),
                Anim::Comet {
                    phase_ms: anim_ms(comet_phase(q, epoch)),
                    spin,
                },
                crate::SPINNER.get(usize::from(spin)).copied(),
            )
        }
        (None, true, Pace::Still) => (track(look.graded), Anim::Track, None),
        (None, false, _) => (Surface::default(), Anim::None, None),
    };
    let readout = layout
        .elapsed
        .and_then(|_| elapsed_words(q.saturating_duration_since(row.started_at)));
    // A hidden estimate leaves the slot the work's elapsed CLOCK, so the
    // slot always says how long (review round 3, 2026-09-24); a remaining
    // time always ends in `left`, so the two never read alike.
    let eta = layout.eta.and_then(|_| match row.track.eta(q) {
        Eta::Remaining(r) if layout.eta_short => eta_words_short(r),
        Eta::Remaining(r) => eta_words(r),
        Eta::Stalled => Some(STALLED_WORD.to_string()),
        Eta::Hidden => Some(clock_words(q.saturating_duration_since(row.started_at))),
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

/// An echo's motion at frame `q`, over the WHOLE row (ruling 141). A
/// Complete echo wears ✓ and says `done` in the row's time slot — the ETA
/// slot, else the elapsed one — for its whole life, so the payoff of a wait
/// says it finished where the title still names the work (review round 3,
/// 2026-09-24). A Fault echo wears ⚠ and says `failed` there, in the fault
/// hue, so a bar that flashes and fades reads as a failure and not as a
/// render glitch (review round 2, 2026-09-23). A Vanish claims no outcome
/// and keeps the frozen clock. Nothing re-grids: the slot is the row's own.
fn echo_motion(e: &Echo, layout: &RowLayout, q: Instant, look: Look) -> RowMotion {
    let t = q.saturating_duration_since(e.started);
    let (surface, fade) = echo(e, t, look);
    let said = match e.kind {
        EchoKind::Complete => Some(DONE_WORD),
        EchoKind::Fault => Some(FAILED_WORD),
        EchoKind::Vanish => None,
    };
    RowMotion {
        surface: if layout.meter.is_some() || layout.track.is_some() {
            surface
        } else {
            Surface::default()
        },
        readout: layout.elapsed.and_then(|_| match said {
            Some(word) if layout.eta.is_none() => Some(word.to_string()),
            _ => elapsed_words(e.elapsed),
        }),
        eta: layout.eta.and_then(|_| said.map(str::to_string)),
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

fn carried_message(l: &Live, on_glass: bool) -> CarriedMessage {
    CarriedMessage {
        id: l.id.raw(),
        unix_ms: l.stamp.unix_ms,
        tag: l.msg.tag.as_str().to_string(),
        severity: l.msg.severity.as_str().to_string(),
        glyph: l.msg.glyph.ch(),
        title: l.msg.title.clone(),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glass::{CapsuleRole, RowKind};
    use crate::log::{LogState, Retired};
    use crate::model::{Decision, Origin, tags};
    use crate::text::char_width;
    use crate::{HOLD_INFO, HOLD_SUCCESS, HOLD_WARN, LOG_CAP, PENDING_PERSIST_CAP};

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
        Message::new(tags::RENDER, Severity::Info, title).hold(Hold::Standing)
    }

    fn ask(title: &str) -> Message {
        Message::new(tags::PRIVACY, Severity::Info, title)
            .action(Intent::OpenSystemPane {
                pane: "full-disk-access".into(),
            })
            .action(Intent::NotNow {
                decision: Decision::FileAccess,
            })
            .hold(Hold::Ask {
                for_: crate::HOLD_ASK,
            })
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
        assert!(c.live_rows().all(|l| l.msg.title != "notice 0"));
        // A Live row is evicted only after every held row is gone.
        let mut c = fresh(now);
        let live_id = c
            .post(
                info("tail").hold(Hold::Live {
                    stale_after: crate::STALE_TAILED,
                }),
                stamp(1),
                now,
            )
            .id;
        for i in 0..MAX_LIVE {
            c.post(
                standing(&format!("standing {i}")),
                stamp(10 + i as u64),
                now + ms(i as u64),
            );
        }
        assert_eq!(c.live_rows().count(), MAX_LIVE);
        assert!(
            c.live(live_id).is_none(),
            "the one Live row was the only candidate"
        );
        // All asks: the newcomer itself is the one evicted.
        let mut c = fresh(now);
        for i in 0..MAX_LIVE {
            c.post(ask(&format!("ask {i}")), stamp(10 + i as u64), now);
        }
        let last = c.post(ask("one too many"), stamp(999), now);
        assert_eq!(c.live_rows().count(), MAX_LIVE);
        assert!(c.live(last.id).is_none());
        // A flood never moves the glass: the victims are QUEUED rows first
        // (lowest severity, oldest), so the rows under the pointer stay.
        let mut c = fresh(now);
        let g: Vec<MessageId> = (0..3)
            .map(|i| c.post(info(&format!("g{i}")), stamp(i), now).id)
            .collect();
        c.commit_rows(now, 3);
        assert_eq!(glass_ids(&c), g);
        for i in 0..(MAX_LIVE + 10) {
            c.post(
                Message::new(tags::PACKAGES, Severity::Success, &format!("s {i}")),
                stamp(10 + i as u64),
                now + ms(1 + i as u64),
            );
        }
        assert_eq!(c.live_rows().count(), MAX_LIVE);
        assert_eq!(
            glass_ids(&c),
            vec![g[0], g[1]],
            "the glass is untouched by the flood (the third slot is the overflow row)"
        );
        assert!(
            g.iter().all(|id| c.live(*id).is_some()),
            "the earlier Info rows outrank every queued Success (one class, posted first)"
        );
        let evicted: Vec<_> = pending_retired(&c)
            .into_iter()
            .filter(|(_, how, _, _)| *how == Retired::Evicted)
            .collect();
        assert_eq!(evicted.len(), 13);
        assert_eq!(evicted[0].2, "s 0", "the oldest queued Success goes first");
        // …and a flood of HIGHER-ranked rows takes the displaced Infos before
        // any of its own, still never a row on glass.
        let mut c = fresh(now);
        let g: Vec<MessageId> = (0..3)
            .map(|i| c.post(info(&format!("g{i}")), stamp(i), now).id)
            .collect();
        c.commit_rows(now, 3);
        for i in 0..(MAX_LIVE + 10) {
            c.post(
                Message::new(tags::PACKAGES, Severity::Warn, &format!("w {i}")),
                stamp(10 + i as u64),
                now + ms(1 + i as u64),
            );
        }
        let top = glass_ids(&c);
        assert_eq!(top.len(), 2);
        assert_eq!(
            top[1], g[0],
            "g0 kept its slot under the Warn that went above"
        );
        assert!(c.live(top[0]).unwrap().msg.title == "w 0");
        assert!(
            c.live(g[1]).is_none() && c.live(g[2]).is_none(),
            "the displaced Infos went first"
        );
        let evicted: Vec<_> = pending_retired(&c)
            .into_iter()
            .filter(|(_, how, _, _)| *how == Retired::Evicted)
            .map(|(_, _, title, _)| title)
            .collect();
        assert_eq!(
            evicted[..3],
            ["g1".to_string(), "g2".into(), "w 1".into()],
            "then the oldest queued Warn"
        );
    }

    /// Invariant 3: 3·LOG_CAP posts: ring == LOG_CAP, pending ==
    /// PENDING_PERSIST_CAP and the drain ends with `Dropped{count}`.
    #[test]
    fn bounded_log_ring_and_pending() {
        let now = t0();
        let mut c = fresh(now);
        for i in 0..(3 * LOG_CAP) {
            c.post(info(&format!("n {i}")), stamp(i as u64), now);
        }
        assert_eq!(c.log().len(), LOG_CAP);
        assert_eq!(c.log().pending_len(), PENDING_PERSIST_CAP);
        let lines = c.drain_new_for_persist();
        assert_eq!(lines.len(), PENDING_PERSIST_CAP + 1);
        assert!(
            matches!(lines.last(), Some(LogLine::Dropped { count }) if *count > 0),
            "{:?}",
            lines.last()
        );
        assert!(c.drain_new_for_persist().is_empty());
        assert_eq!(
            c.log().next_id().raw() as usize,
            3 * LOG_CAP + 1,
            "ids never reuse"
        );
    }

    fn script(c: &mut MessageCenter, now: Instant) -> Vec<String> {
        let a = c
            .post(info("first").line("with a detail"), stamp(1), now)
            .id;
        c.commit_rows(now, 3);
        let b = c
            .post(
                Message::new(tags::TOOLCHAIN, Severity::Info, "Installing")
                    .meter(Meter {
                        fill_permille: Some(100),
                        stats: "1 / 10".into(),
                        ..Meter::default()
                    })
                    .hold(Hold::Live {
                        stale_after: crate::STALE_TAILED,
                    })
                    .key("toolchain.pass"),
                stamp(2),
                now + ms(10),
            )
            .id;
        c.commit_rows(now + ms(10), 3);
        c.restate(
            b,
            Restatement {
                meter: Some(Some(Meter {
                    fill_permille: Some(500),
                    stats: "5 / 10".into(),
                    ..Meter::default()
                })),
                ..Restatement::default()
            },
            now + ms(20),
        );
        c.post(
            Message::new(tags::UPDATE, Severity::Warn, "Update failed").key("update.outcome"),
            stamp(3),
            now + ms(30),
        );
        c.commit_rows(now + ms(30), 3);
        c.post(info("first").line("with a detail"), stamp(4), now + ms(40));
        c.act(a, ActionIndex::DETAILS, now + ms(50));
        c.settle(now + HOLD_INFO + ms(100), true);
        c.commit_rows(now + HOLD_INFO + ms(100), 3);
        c.drain_new_for_persist()
            .iter()
            .map(LogLine::encode)
            .collect()
    }

    /// Invariant 4: the same script at the same injected instants yields
    /// byte-identical `presentation(80)` and log lines in two fresh centers.
    #[test]
    fn settle_is_deterministic() {
        let now = t0();
        let mut a = fresh(now);
        let mut b = fresh(now);
        let lines_a = script(&mut a, now);
        let lines_b = script(&mut b, now);
        assert_eq!(lines_a, lines_b);
        assert!(lines_a.len() >= 5, "{lines_a:?}");
        let pa = a.presentation(80, &char_width, None, Links::Painted);
        let pb = b.presentation(80, &char_width, None, Links::Painted);
        assert_eq!(pa, pb);
        assert_eq!(a.fingerprint(80), b.fingerprint(80));
        assert_eq!(a.revision(), b.revision());
        assert_eq!(glass_ids(&a), glass_ids(&b));
        assert!(!pa.rows.is_empty());
    }

    /// Invariant 5: a row queued behind three others gets `fold_at =
    /// promotion + hold`; with `afford = 2` the third row never anchors.
    #[test]
    fn hold_anchors_at_first_glass_and_only_within_afford() {
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
        // The first row resolves; the third is promoted and anchors THEN.
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
        // Afford clamps: the third row never burns its hold unpainted.
        let mut c = fresh(now);
        let ids: Vec<MessageId> = (0..3)
            .map(|i| c.post(info(&format!("r{i}")), stamp(i), now).id)
            .collect();
        assert_eq!(c.commit_rows(now, 2), Some(2));
        assert_eq!(c.committed_rows(), 2);
        assert_eq!(
            glass_ids(&c),
            vec![ids[0]],
            "one row plus the overflow row within afford 2"
        );
        assert!(
            c.live(ids[2]).unwrap().fold_at.is_none()
                && c.live(ids[2]).unwrap().on_glass_since.is_none()
        );
        assert!(
            c.settle(now + HOLD_INFO + ms(1), true)
                .retired
                .iter()
                .all(|(id, _)| *id == ids[0]),
            "only the anchored row folds"
        );
        assert!(c.live(ids[2]).is_some());
        // A row the glass DISPLACES — the overflow row takes its slot when
        // a fourth arrives — is released: queued again under the patience
        // clock, no anchor, and it anchors afresh when it returns.
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
            displaced.patience_at,
            now + ms(1) + OVERFLOW_PATIENCE_MIN,
            "patience runs from the displacement"
        );
        assert_eq!(c.deadline(true), Some(now + HOLD_INFO));
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
        assert_eq!(old.retired_at, Some(now + ms(10)));
        let retired = pending_retired(&c);
        assert_eq!(
            retired,
            vec![(
                a,
                Retired::Superseded { by: posted.id },
                "Downloading aterm v0.91.0".to_string(),
                1
            )]
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
        assert_eq!(c.live_rows().count(), 1);
        assert_eq!(c.live(first.id).unwrap().repeats, 2);
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
        assert_eq!(c.log().get(first.id).unwrap().repeats, 2);
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
        assert_eq!(c.live(id).unwrap().revision, 1000);
        assert_eq!(
            c.live(id).unwrap().stale_at,
            Some(now + ms(999) + crate::STALE_TAILED),
            "each restate re-arms the cap"
        );
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

    /// AN IDENTICAL RESTATEMENT IS NOT A CHANGE OF THE GLASS (review
    /// 2026-09-24): atpkg's heartbeat re-feeds the same reading every 2 s,
    /// and each one bumped the revision, so every window re-laid, re-painted
    /// and presented the band. The same words and meter leave the revision
    /// and the fingerprint alone — and still re-arm the staleness cap.
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
                stats: "3 of 10 programs".into(),
                ..Meter::default()
            })),
            ..same
        };
        assert!(c.restate(id, moved, now + ms(4000)));
        assert!(c.revision() > rev, "a moved meter is a change");
        assert_ne!(c.fingerprint(120), fp);
    }

    /// …BUT AN ESTIMATE THAT MOVED IS: the same reading fed at a later
    /// instant moves the estimator (the secant flattens: a re-anchor, then
    /// an unlatch), and no motion deadline was armed for that, so a
    /// heartbeat that moves `eta(now)` bumps the revision — the host
    /// repaints the slot — and one that moves nothing does not.
    #[test]
    fn a_heartbeat_that_moves_the_estimate_is_a_change() {
        use crate::model::{Amount, Unit};
        let now = t0();
        let mut c = fresh(now);
        let meter = |done: u64| Meter {
            fill_permille: Some(u16::try_from(done * 1000 / 400).unwrap()),
            stats: "3 of 10 programs".into(),
            amount: Some(Amount {
                series: 1,
                done,
                total: 400,
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

    /// Invariant 9: `settle(now, false)` folds a Live row past `stale_at`
    /// and leaves an expired Default row; `deadline(false)` agrees (port of
    /// status_bars.rs:5625).
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
        let faded = now + STALE_HANDOFF + EchoKind::Vanish.span();
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
        assert_eq!(c.commit_rows(now + ms(2) + SHRINK_QUIET - ms(1), 3), None);
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
        // The host's clamp is inside the center too.
        let g = c.post(standing("g"), stamp(6), now + ms(700)).id;
        let h = c.post(standing("h"), stamp(7), now + ms(700)).id;
        assert_eq!(
            c.commit_rows(now + ms(700), 2),
            Some(2),
            "afford caps the grow"
        );
        assert_eq!(c.commit_rows(now + ms(700), 3), Some(3));
        assert_eq!(
            c.commit_rows(now + ms(800), 1),
            Some(1),
            "the WINDOW's shrink waits for no quiet: the terminal row comes back now"
        );
        assert_eq!(c.committed_rows(), 1);
        assert_eq!(
            c.commit_rows(now + ms(800), 3),
            Some(3),
            "…and the rows return at once"
        );
        c.resolve(a, Outcome::Ok, now + ms(900));
        assert_eq!(
            c.resolve_key_prefix("", Outcome::Ok, now + ms(900)),
            0,
            "a prefix resolve takes keyed rows only"
        );
        c.resolve(g, Outcome::Ok, now + ms(900));
        c.resolve(h, Outcome::Ok, now + ms(900));
        assert_eq!(c.wanted_rows(), 0);
        assert_eq!(
            c.commit_rows(now + ms(900), 3),
            None,
            "the host commits after every settle: the shrink clock starts here"
        );
        assert_eq!(c.commit_rows(now + ms(900) + SHRINK_QUIET, 3), Some(0));
        assert_eq!(c.fingerprint(80), 0, "FL-1: no rows, key 0");
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
                Message::new(tags::UPDATE, Severity::Warn, "Update failed").hold(Hold::Standing),
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
        c.resolve(r[1], Outcome::Ok, now + ms(10));
        assert_eq!(glass_ids(&c), vec![r3, late]);
        let p = c.presentation(100, &char_width, None, Links::Painted);
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0].kind, RowKind::Message(r3));
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
                    .hold(Hold::For(Duration::from_secs(300))),
                stamp(4),
                now,
            )
            .id;
        assert_eq!(
            c.live(w).unwrap().patience_at,
            now + Duration::from_secs(300)
        );
    }

    /// Invariant 16: 7 held rows at 3 rows → rows 0-1 messages, row 2
    /// `5 more messages`; at 1 row the Details capsule reads `+6 ›`.
    #[test]
    fn overflow_row_counts_what_is_hidden() {
        let now = t0();
        let mut c = fresh(now);
        for i in 0..7 {
            c.post(info(&format!("row {i}")), stamp(i), now + ms(i));
        }
        c.commit_rows(now + ms(10), 3);
        let p = c.presentation(120, &char_width, None, Links::Painted);
        assert_eq!(p.rows.len(), 3);
        assert!(
            matches!(p.rows[0].kind, RowKind::Message(_))
                && matches!(p.rows[1].kind, RowKind::Message(_))
        );
        assert_eq!(p.rows[2].kind, RowKind::Overflow { hidden: 5 });
        assert_eq!(p.rows[2].title.1, "5 more messages");
        assert_eq!(p.rows[2].glyph.1, '\u{2026}');
        assert_eq!(p.rows[2].capsules.len(), 1);
        assert_eq!(p.rows[2].capsules[0].text, "Messages \u{203a}");
        assert_eq!(p.rows[2].capsules[0].role, CapsuleRole::Details);
        assert_eq!(p.rows[0].capsules.last().unwrap().text, "Details \u{203a}");
        assert_eq!(p.hit(2, 5), crate::glass::Hit::Overflow);
        assert_ne!(c.fingerprint(120), 0);
        assert_ne!(c.fingerprint(120), c.fingerprint(80));
        // At one row: no overflow row; the single row's Details reads `+6 ›`.
        c.commit_rows(now + ms(20), 1);
        c.commit_rows(now + ms(20) + SHRINK_QUIET, 1);
        assert_eq!(c.committed_rows(), 1);
        let p = c.presentation(120, &char_width, None, Links::Painted);
        assert_eq!(p.rows.len(), 1);
        let details = p.rows[0].capsules.last().unwrap();
        assert_eq!(details.text, "+6 \u{203a}");
        assert_eq!(details.full_label, "Details \u{203a}");
        assert_eq!(c.queued(), 6);
        // Two rows: one message and `6 more messages`.
        c.commit_rows(now + ms(30), 2);
        let p = c.presentation(120, &char_width, None, Links::Painted);
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[1].kind, RowKind::Overflow { hidden: 6 });
    }

    /// Invariant 21: Ask row: `act(Open Settings)` → `Some(intent)`, row
    /// stays, `Acted` logged; `act(Not now)` → row gone, `Answered{label}`.
    #[test]
    fn act_returns_the_intent_and_closes_only_not_now() {
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
        assert_eq!(
            c.live(id).unwrap().acted,
            Some((ActionIndex(0), now + ms(1)))
        );
        assert!(
            matches!(c.log().pending_lines().last(), Some(LogLine::Acted { label, .. }) if label == "Open Settings")
        );
        assert_eq!(
            c.log()
                .get(id)
                .unwrap()
                .last_action
                .as_ref()
                .map(|(l, _)| l.as_str()),
            Some("Open Settings")
        );
        // The host restates to the route words; still the same row.
        assert!(c.restate(
            id,
            Restatement {
                title: Some("Grant access in System Settings".into()),
                hold: Some(Hold::For(crate::HOLD_ROUTE)),
                ..Restatement::default()
            },
            now + ms(2)
        ));
        assert_eq!(
            c.live(id).unwrap().fold_at,
            Some(now + ms(2) + crate::HOLD_ROUTE)
        );
        assert_eq!(c.act(id, ActionIndex(5), now), None, "no such action");
        let not_now = c.act(id, ActionIndex(1), now + ms(3));
        assert_eq!(
            not_now,
            Some(Intent::NotNow {
                decision: Decision::FileAccess
            })
        );
        assert!(c.live(id).is_none(), "Not now closes the row");
        assert_eq!(
            c.log().get(id).unwrap().state,
            LogState::Retired(Retired::Answered {
                label: "Not now".into()
            })
        );
        assert_eq!(c.act(id, ActionIndex(0), now), None);
        // Details marks seen: a held row folds now, an ask stays.
        let held = c.post(info("read me"), stamp(2), now).id;
        let asked = c.post(ask("decide"), stamp(3), now).id;
        c.commit_rows(now, 3);
        assert_eq!(
            c.act(held, ActionIndex::DETAILS, now + ms(4)),
            Some(Intent::Details)
        );
        assert!(c.live(held).is_none());
        assert_eq!(
            c.log().get(held).unwrap().state,
            LogState::Retired(Retired::Folded)
        );
        assert_eq!(
            c.act(asked, ActionIndex::DETAILS, now + ms(4)),
            Some(Intent::Details)
        );
        assert!(c.live(asked).unwrap().seen);
        assert!(c.dismiss(asked, now + ms(5)));
        assert_eq!(
            c.log().get(asked).unwrap().state,
            LogState::Retired(Retired::Dismissed)
        );
    }

    /// A busy flow row, as the update lane posts one (design ruling 56).
    fn busy(title: &str) -> Message {
        Message::new(tags::UPDATE, Severity::Info, title)
            .line("installs within a minute")
            .meter(Meter::busy(""))
            .hold(Hold::Live {
                stale_after: crate::STALE_UPDATE,
            })
            .key("update.progress")
    }

    /// A BUSY RESTATE IS A REPAINT, NEVER A RE-GRID OR A WAKE (design ruling
    /// 55): turning a row's meter busy or still is a restatement like any
    /// other — the revision moves and the glass changes, but the committed
    /// rows do not, and the deadline is exactly what it was: the engine owns
    /// no frame, so busy arms nothing.
    #[test]
    fn a_busy_restate_is_a_repaint_never_a_regrid_or_a_wake() {
        let now = t0();
        let mut c = fresh(now);
        let id = c.post(busy("Updating to aterm v1"), stamp(1), now).id;
        c.commit_rows(now, 3);
        let rows = c.committed_rows();
        let deadline = c.deadline(true);
        let fp = c.fingerprint(80);
        let revision = c.revision();
        assert!(c.restate(
            id,
            Restatement {
                meter: Some(Some(Meter::default())),
                ..Restatement::default()
            },
            now,
        ));
        assert_eq!(c.revision(), revision + 1);
        assert_ne!(c.fingerprint(80), fp, "the glass changed: busy is folded");
        assert_eq!(c.committed_rows(), rows, "no re-grid");
        assert_eq!(c.commit_rows(now, 3), None, "and none wanted");
        assert_eq!(c.deadline(true), deadline, "no wake of its own");
        assert!(!c.busy_on_glass());
        assert!(c.restate(
            id,
            Restatement {
                meter: Some(Some(Meter::busy(""))),
                ..Restatement::default()
            },
            now,
        ));
        assert!(c.busy_on_glass());
        assert_eq!(c.deadline(true), deadline);
    }

    /// `busy_on_glass` follows the COMMITTED rows: false with nothing
    /// committed (FL-1: an idle band asks for nothing), false for a busy row
    /// still queued behind the band, true once it is on glass.
    #[test]
    fn busy_on_glass_follows_the_committed_rows() {
        let now = t0();
        let mut c = fresh(now);
        assert!(!c.busy_on_glass(), "nothing posted");
        assert_eq!(c.fingerprint(80), 0);
        c.post(busy("Updating to aterm v1"), stamp(1), now);
        assert!(!c.busy_on_glass(), "posted, not committed");
        c.commit_rows(now, 3);
        assert!(c.busy_on_glass());
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

    /// THE CARRY KEEPS BUSY, AND COMMIT STILLS WHAT NOBODY RESTATED: a busy
    /// meter crosses the handoff (an older build's carry reads as still); at
    /// Commit a carried row no one here restated stops animating — work that
    /// is not this process's own is shown still — while a row restated
    /// before Commit (the update's own "finishing") keeps what it said.
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
        successor.seed_carried(&carry, now);
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
        older.seed_carried(&old, now);
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
            carry.live[0].id,
            queued.raw(),
            "the Warn outranks everything on glass"
        );
        assert!(carry.live[0].on_glass);
        assert!(
            carry.live.iter().filter(|m| m.on_glass).count() == 1,
            "one slot plus the overflow row at 2 rows"
        );
        assert!(!carry.is_empty());
        let carried_live = carry.live.iter().find(|m| m.id == live.raw()).unwrap();
        assert_eq!(carried_live.hold, "live:30");
        assert_eq!(carried_live.actions, vec!["open-settings:/packages"]);
        assert_eq!(carried_live.fill_permille, Some(420));

        let later = now + Duration::from_secs(5);
        let mut child = fresh(later);
        child.seed_carried(&carry, later);
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
            child.live(live).unwrap().msg.hold,
            Hold::Live {
                stale_after: crate::STALE_TAILED
            }
        );
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
        assert_eq!(
            child.live(live).unwrap().stale_at,
            Some(later + STALE_HANDOFF),
            "a promoted carried row keeps the handoff cap, not a fresh anchor"
        );
        // The round trip: the child's carry says the same rows.
        let again = child.carried();
        let mut a: Vec<_> = carry
            .live
            .iter()
            .map(|m| {
                (
                    m.id,
                    m.title.clone(),
                    m.hold.clone(),
                    m.actions.clone(),
                    m.key.clone(),
                    m.fill_permille,
                    m.stats.clone(),
                )
            })
            .collect();
        let mut b: Vec<_> = again
            .live
            .iter()
            .map(|m| {
                (
                    m.id,
                    m.title.clone(),
                    m.hold.clone(),
                    m.actions.clone(),
                    m.key.clone(),
                    m.fill_permille,
                    m.stats.clone(),
                )
            })
            .collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        assert_eq!(again.next_id, carry.next_id);
        // Junk in the carry is dropped, not guessed.
        let mut junk = carry.clone();
        junk.live[1].severity = "loud".into();
        junk.live[2].tag = "Not A Tag".into();
        junk.live[3].hold = "forever".into();
        junk.next_id = 900;
        let mut c = fresh(later);
        c.seed_carried(&junk, later);
        assert_eq!(c.live_rows().count(), 1);
        assert_eq!(c.log().next_id().raw(), 900);
        // Before commit the cap is the only exit. A restate of a carried
        // row's WORDS before the commit (the R1 config re-count, the R16
        // route words) makes it the successor's own at once: the handoff cap
        // comes off and its own hold anchors on glass there and then.
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
        // After it each row takes its own lifetime back: a held row ON GLASS
        // folds after its hold from the commit; a QUEUED one has no anchor
        // until it is painted (`fold_at: None until first glass`); the
        // restated row keeps the anchor it already took.
        assert_eq!(
            child.live(queued).unwrap().fold_at,
            Some(restated + HOLD_WARN)
        );
        assert!(child.live(held).unwrap().is_queued());
        assert_eq!(
            child.live(held).unwrap().fold_at,
            None,
            "queued in the successor: not painted, not anchored"
        );
        assert_eq!(child.live(held).unwrap().stale_at, None);
        assert_eq!(
            child.live(live).unwrap().stale_at,
            Some(commit + crate::STALE_TAILED)
        );
        assert_eq!(child.live(standing).unwrap().stale_at, None);
        assert_eq!(child.live(standing).unwrap().fold_at, None);
        // The Warn folds on its restate anchor; the freed slot promotes the
        // held row, which anchors THEN; the Live row's cap lands; then the
        // held row folds after its own 8 s.
        let settled = child.settle(restated + HOLD_WARN, true);
        assert_eq!(settled.retired, vec![(queued, Retired::Folded)]);
        assert_eq!(glass_ids(&child), vec![live, standing, held]);
        assert_eq!(
            child.live(held).unwrap().fold_at,
            Some(restated + HOLD_WARN + HOLD_SUCCESS)
        );
        let settled = child.settle(commit + crate::STALE_TAILED, true);
        assert_eq!(settled.retired, vec![(live, Retired::Stale)]);
        let settled = child.settle(restated + HOLD_WARN + HOLD_SUCCESS, true);
        assert_eq!(settled.retired, vec![(held, Retired::Folded)]);
        assert!(child.live(standing).is_some(), "a standing row stands");
        assert_eq!(
            fresh(now).carried(),
            Carry {
                next_id: MessageId::FIRST.raw(),
                live: vec![],
            },
            "an empty center carries only the first mintable id"
        );
    }

    /// A withdrawn row leaves the glass into the log with no outcome: the
    /// record stands (`Withdrawn`), the live set and the glass forget it,
    /// and a second withdraw of the same id is `false`.
    #[test]
    fn withdraw_keeps_the_record_without_an_outcome() {
        let now = t0();
        let mut c = fresh(now);
        let meter = c
            .post(
                Message::new(tags::TOOLCHAIN, Severity::Info, "Installing")
                    .hold(Hold::Live {
                        stale_after: crate::STALE_TAILED,
                    })
                    .key("toolchain.pass"),
                stamp(1),
                now,
            )
            .id;
        c.commit_rows(now, 3);
        assert_eq!(glass_ids(&c), vec![meter]);
        assert!(c.withdraw(meter, now + ms(1)));
        assert!(c.live(meter).is_none());
        assert!(glass_ids(&c).is_empty());
        assert_eq!(
            c.log().get(meter).unwrap().state,
            LogState::Retired(Retired::Withdrawn)
        );
        assert!(!c.withdraw(meter, now + ms(2)), "already gone");
        assert_eq!(Retired::Withdrawn.as_word(), "withdrawn");
    }

    /// `LogOnly` never enters the live set; a resolve by key prefix takes
    /// the whole family; the revision moves on every observable change.
    #[test]
    fn log_only_and_key_prefix_and_revision() {
        let now = t0();
        let mut c = fresh(now);
        let r0 = c.revision();
        let quiet = c.post(info("managed, current").hold(Hold::LogOnly), stamp(1), now);
        assert_eq!(quiet.outcome, PostOutcome::New);
        assert_eq!(c.live_rows().count(), 0);
        assert_eq!(
            c.log().get(quiet.id).unwrap().state,
            LogState::Retired(Retired::Recorded),
            "a record retires as recorded, never as a fold it never had"
        );
        assert!(c.revision() > r0);
        let a = c
            .post(
                Message::new(tags::CONFIG, Severity::Warn, "a").key("config.keybindings"),
                stamp(2),
                now,
            )
            .id;
        let b = c
            .post(
                Message::new(tags::CONFIG, Severity::Warn, "b").key("config.theme"),
                stamp(3),
                now,
            )
            .id;
        let other = c
            .post(
                Message::new(tags::PRIVACY, Severity::Info, "o").key("privacy.fda"),
                stamp(4),
                now,
            )
            .id;
        assert_eq!(c.resolve_key_prefix("config.", Outcome::Ok, now), 2);
        assert!(c.live(a).is_none() && c.live(b).is_none() && c.live(other).is_some());
        assert_eq!(
            c.log().get(a).unwrap().state,
            LogState::Retired(Retired::Resolved(Outcome::Ok))
        );
        assert!(!c.resolve(a, Outcome::Ok, now));
        assert!(!c.dismiss(a, now));
        assert!(!c.mark_seen(a, now));
        let r1 = c.revision();
        c.commit_rows(now, 3);
        assert!(c.revision() > r1, "a commit is observable");
        assert!(c.settle(now, true).retired.is_empty());
        assert!(
            c.restate(
                other,
                Restatement {
                    hold: Some(Hold::LogOnly),
                    ..Restatement::default()
                },
                now
            ),
            "a restate to LogOnly retires the row"
        );
        assert!(c.live(other).is_none());
    }

    /// REVIEW (2026-09-22): a row a Warn pushes off the band is released —
    /// `on_glass_since`/`fold_at` used to survive the demotion, so
    /// `is_queued()` said false for a row `on_glass()` did not contain,
    /// `settle` folded it `Folded` ("its hold elapsed on glass") while it sat
    /// in the queue, and a re-promotion did not re-anchor (the
    /// status_bars.rs:906-917 rule: anchor NOW "or a row queued behind a long
    /// hold would arrive already expired").
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

    /// REVIEW (2026-09-22): the same through the afford clamp — rows
    /// anchored on a 3-row band, the window shrinks to 1 row, the two rows
    /// the clamp took off glass must not fold `Folded` unpainted (the
    /// design's own "a clamped third row never burns its hold unpainted",
    /// §1.4).
    #[test]
    fn a_row_clamped_off_glass_by_afford_does_not_burn_its_hold() {
        let now = t0();
        let mut c = fresh(now);
        let r: Vec<MessageId> = (0..3)
            .map(|i| c.post(info(&format!("r{i}")), stamp(i), now).id)
            .collect();
        assert_eq!(c.commit_rows(now, 3), Some(3));
        assert_eq!(
            c.commit_rows(now + ms(1), 1),
            Some(1),
            "the window's own shrink lands at once"
        );
        assert_eq!(glass_ids(&c), vec![r[0]]);
        let settled = c.settle(now + HOLD_INFO, true);
        let folded_off_glass: Vec<_> = settled
            .retired
            .iter()
            .filter(|(id, how)| *id != r[0] && *how == Retired::Folded)
            .collect();
        assert!(
            folded_off_glass.is_empty(),
            "rows off glass since {SHRINK_QUIET:?} after the clamp folded as if read: {folded_off_glass:?}"
        );
    }

    /// REVIEW (2026-09-22): at one committed row the single row's `+N ›`
    /// capsule counts the queue; when a queued row folds Unseen the painted
    /// capsule changes, and `settle` says so (`glass_signature` folds the
    /// `+N` count as well as the overflow row's), so a host keyed on
    /// `glass_changed` repaints exactly when `fingerprint(cols)` moves.
    #[test]
    fn settle_reports_the_plus_n_capsule_change() {
        let now = t0();
        let mut c = fresh(now);
        let a = c.post(info("a").hold(Hold::Standing), stamp(1), now).id;
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

    /// REVIEW (2026-09-22): a carried held row the host restates before the
    /// handoff commit (re-wording is what R16 does) keeps its fold — the
    /// restate clears `carried_pending` and `after_handoff_commit` then
    /// skips it, so the restate itself must take the handoff cap off and
    /// anchor the row's own hold; it used to sit on glass with `fold_at ==
    /// None` until the cap retired it `Stale` at 125 s.
    #[test]
    fn a_carried_held_row_restated_before_commit_still_folds() {
        let now = t0();
        let mut parent = fresh(now);
        let held = parent
            .post(
                Message::new(tags::UPDATE, Severity::Success, "Updated"),
                stamp(1),
                now,
            )
            .id;
        parent.commit_rows(now, 3);
        let carry = parent.carried();
        let later = now + Duration::from_secs(5);
        let mut child = fresh(later);
        child.seed_carried(&carry, later);
        assert!(child.restate(
            held,
            Restatement {
                title: Some("Updated (v2)".into()),
                ..Restatement::default()
            },
            later + ms(1)
        ));
        let commit = later + Duration::from_secs(20);
        child.after_handoff_commit(commit);
        let row = child.live(held).unwrap();
        assert!(
            row.fold_at.is_some(),
            "a held row on glass with no fold anchor: fold_at {:?} stale_at {:?}",
            row.fold_at,
            row.stale_at
        );
        let settled = child.settle(later + STALE_HANDOFF, true);
        assert!(
            settled
                .retired
                .iter()
                .all(|(_, how)| *how != Retired::Stale),
            "a held row retired Stale: {:?}",
            settled.retired
        );
    }
}

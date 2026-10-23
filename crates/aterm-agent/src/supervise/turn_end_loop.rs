// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor turn-end event loop driving `turn_end` policy decisions against `Fx`.
use super::super::policy::approval::squashed;
use super::super::policy::exception::exception_for;
use super::super::policy::notice;
use super::super::policy::sleeps::InTurn;
use super::super::policy::trace::{Outcome as TraceOutcome, TraceStep};
use super::super::policy::turn_end::{
    ANSWERS_OFF, AccountAnswer, AccountAt, CREDITS_HOLD_MODEL_TOLD, CREDITS_HOLD_TOLD, Composer,
    ComposerRead, Conversation, CreditsCarry, LOGIN_SHARED_LOOK, MENU_DIVERTS, OWN_WORDS_STAND,
    ProbeGate, RULE_ACCOUNT, RULE_LAND, RULE_LEFT, RULE_LIMIT_RESUME, RULE_LOGIN, RULE_USAGE_PROBE,
    RecordTurn, Restart, TRY_ANSWER_SLACK, WAKE_KEPT, act_turn_work, budget_frees,
    conversation_mark, decide_turn_end, decide_turn_end_traced, done_row_work, reads_conversation,
};
use super::super::tap_clock::{TapClock, TapSays, Timed, UsageWindow, timed};
use super::wall_confirm::{Check, LeftRead};
use super::*;
use crate::harness::accounts::Sample;
use crate::harness::usage::{
    PanelRead, PanelWindow, USAGE_COMMAND, panel_sample, usage_panel_read,
};

/// THE USAGE TAP AT ONE WALL ([`Session::tap_reset`]; accounts v2 S10a): the
/// wall's key ([`reset_key`], its [`Session::wall_reset`]'s), the windows it
/// is timed by, what the tap read of the walled process since the loop's
/// first read of this answer of it (a turn ended on it again, or its answer
/// to an act of the loop's, starts afresh), the reset the wall is waited to,
/// and the disagreement between the notice and the tap that has had its
/// ledger row.
#[derive(Debug, Clone)]
pub(super) struct WallTap {
    key: String,
    windows: &'static [UsageWindow],
    clock: TapClock,
    /// The reset the wall is waited to (unix seconds), as last set
    /// ([`Session::tap_timing`]): the notice's and the tap's together
    /// ([`timed`]).
    at: Option<i64>,
    /// The `(notice, tap)` resets whose disagreement was written down.
    said: Option<(i64, i64)>,
    /// The tap said something of the reset ([`timed`]'s input was no
    /// `TapSays::Nothing`), or a loop before this one's tap did
    /// ([`Self::carried`]): [`Self::at`] is then the tap's timing, which a
    /// reading of the wall after its notice left hands the policy.
    by_tap: bool,
    /// THE RESET A LOOP BEFORE THIS ONE'S TAP TIMED THE WALL TO (its wall
    /// row's `tap_unix=`, [`Session::seed_tap`]): the reset waited to until
    /// this loop's own tap says something of it. Apart from the notice's
    /// placement ([`Session::wall_reset`], the row's `reset_unix=`), which
    /// the tap's next reading is set against, and gone with this clock at a
    /// new answer of the wall.
    carried: Option<i64>,
}

/// Whether `composer` holds the point `action` was decided at with nothing
/// due ([`Session::draft_held`]): aterm's own words, or a draft over a
/// completion menu, escalated for itself ([`OWN_WORDS_STAND`],
/// [`MENU_DIVERTS`]); a draft whose cells were not read, left alone until
/// they are.
fn draft_holds(composer: &Composer, action: &TurnEndAction) -> bool {
    let told = |why: &str| matches!(action, TurnEndAction::Escalate { reason } if reason == why);
    match composer {
        Composer::Own => told(OWN_WORDS_STAND),
        Composer::Diverted => told(MENU_DIVERTS),
        Composer::Unknown => matches!(action, TurnEndAction::Nothing),
        Composer::Absent | Composer::Empty | Composer::Placeholder(_) | Composer::Typed => false,
    }
}

/// The furthest a wall's reset may jump on a reprint of the same words before the jump is
/// doubted: the last rung of `TurnEndTiming::limit_backoff` (30 minutes).
const REPRINT_JUMP_MAX_S: i64 = 30 * 60;

/// How many reprints a doubted jump is kept back for ([`REPRINT_JUMP_MAX_S`]).
pub(super) const REPRINT_JUMPS_MAX: u32 = 4;

/// How long past its placement a reset is doubted at all: two hours, room for the four ladder
/// tries of [`REPRINT_JUMPS_MAX`] (10, 30, 30, 30 minutes).
const REPRINT_HOLD_WINDOW_S: i64 = 2 * 3600;

/// The longest text the cursor-row guard names whole, as `^❯ <text>`: it
/// sits on the caret row of any composer 43 columns wide or more.
const GUARD_TAIL_CHARS: usize = 40;

/// The most of the text's last word a longer text's guard names: the tail
/// of a word too long for the composer's width is what its last row holds.
const GUARD_WORD_CHARS: usize = 16;

/// `yield=<floor>` on every `turn` this policy types: the verb parks before
/// typing until the target's typing momentum has fallen to the floor, so a
/// person typing is not pasted over.
const TURN_YIELD: &str = "yield=0.2";

/// The standing rules ride in the continuation cut to this many characters (at a word, `…` after).
pub(super) const RULES_CAP: usize = 400;

/// Maximum duration the loop waits for the worker to react to a supervisor write.
pub(super) const REACTION_WAIT: Duration = Duration::from_secs(10);

/// How long the echo of a fenced write must hold still before its Enter, for
/// a program with a paste guard ([`ScreenReader::paste_guard`]: Codex and
/// Muse Code take an Enter within a burst of typed text as a newline): the
/// guarded `turn`'s own settle (`idle=600`).
const PASTE_GUARD_SETTLE_MS: &str = "600";

/// AN ACT'S WORDS AS ONE TURN.
pub(crate) fn one_turn(text: &str) -> String {
    if text.chars().any(limit::breaks_a_line) {
        limit::one_line(text)
    } else {
        text.to_string()
    }
}

/// Build the cursor-row input fence guard for `text` typed at `caret`.
pub(crate) fn composer_guard(caret: char, text: &str) -> String {
    composer_guard_at(caret, 0, text)
}

/// Build [`composer_guard`] when the caret is drawn `column` cells from the left margin.
pub(crate) fn composer_guard_at(caret: char, column: usize, text: &str) -> String {
    let lead = "\\s".repeat(column);
    if text.chars().count() <= GUARD_TAIL_CHARS {
        let guard = super::super::policy::row_guard(&format!("{caret} {text}"));
        let lit = super::super::policy::row_guard(&caret.to_string());
        let lit = lit
            .strip_prefix('^')
            .and_then(|l| l.strip_suffix(super::super::policy::guard::ROW_END))
            .unwrap_or(&lit);
        return guard.replacen(&format!("^{lit}\\x20"), &format!("^{lead}{lit}\\s"), 1);
    }
    let word = text.split_whitespace().last().unwrap_or(text);
    let n = word.chars().count();
    let tail: String = word
        .chars()
        .skip(n.saturating_sub(GUARD_WORD_CHARS))
        .collect();
    let anchored = super::super::policy::row_guard(&tail);
    format!(
        "^{lead}[{caret}\\s]\\s.*{}",
        anchored.trim_start_matches('^')
    )
}

/// Post-wake settle window before typing into an agent session.
pub(super) const WAKE_SETTLE: Duration = Duration::from_secs(30);
/// See [`WAKE_SETTLE`].
pub(super) const WAKE_SPREAD: Duration = Duration::from_secs(60);

/// How long the session repository's last answer stands for the same done words, no turn run since ([`Session::repo_read`]).
pub(super) const REPO_REPROBE: Duration = Duration::from_secs(20);

/// The reason of the due the wake's settle places.
pub(super) const WOKE: &str = "the Mac woke";

/// Why the point is decided again after the hour's typing budget held its act.
pub(super) const BUDGET_FREES: &str = "the hour's typing budget (continue_per_hour) frees";

/// How long after the worker's input queue was last seen unread the loop
/// holds an act on a screen that has not moved since
/// ([`Session::input_unread`]): a hang detector for a worker that read its
/// input and drew nothing, never the time a starved worker takes to draw.
pub(super) const UNDRAWN_INPUT_HANG: Duration = Duration::from_secs(60);

/// Whether the `tries`-th ask of a restart the host keeps deferring is journaled: the first four
/// (the pauses grow 10 s, 1, 5, 10 min) and then every sixth, an hour on the last pause, so a
/// fallback that waits a day on the agent's own jobs writes a line an hour.
fn restart_note_due(tries: u32) -> bool {
    tries < 4 || tries.is_multiple_of(6)
}

/// The composer's caret row as `reader` reads it (`None`: no composer).
fn caret_row(reader: &dyn ScreenReader, rows: &[String]) -> Option<usize> {
    reader.composer(rows).map(|(caret, _)| caret)
}

/// The text on the composer's caret row as `reader` reads it, the caret
/// stripped — typed, or the placeholder ([`typed_draft`] tells which).
fn caret_text(reader: &dyn ScreenReader, rows: &[String]) -> Option<String> {
    reader
        .composer(rows)
        .and_then(|(_, lines)| lines.into_iter().next())
}

/// A wall the host's measure of the API's reach answers: an API error that never reached the API,
/// a reply cut off, or a certificate or proxy refused ([`aterm_phase::ApiCause`]).
fn answered_by_the_network(kind: aterm_phase::WallKind) -> bool {
    matches!(
        kind,
        aterm_phase::WallKind::ApiError {
            cause: aterm_phase::ApiCause::Unreachable
                | aterm_phase::ApiCause::CutOff
                | aterm_phase::ApiCause::Config,
            ..
        }
    )
}

/// The walls a usage window's reset lifts.
fn resumes_at_reset(kind: aterm_phase::WallKind) -> bool {
    use aterm_phase::WallKind as K;
    matches!(
        kind,
        K::UsageSession | K::UsageWeekly | K::Spend | K::ModelBucket { .. }
    )
}

/// A session's stable share of `span`, from its id and the reset (unix seconds).
pub(super) fn resume_jitter(sid: &str, reset_unix: i64, span: Duration) -> Duration {
    let span_s = span.as_secs();
    if span_s == 0 {
        return Duration::ZERO;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in sid.bytes().chain(reset_unix.to_le_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    Duration::from_secs(h % span_s)
}

/// The standing rules of `policy` (`rules_file`), read now so an edit since launch counts, as one line of at most [`RULES_CAP`] characters.
#[must_use]
pub fn standing_rules(policy: &crate::supervise::SupervisorConfig) -> Option<String> {
    let path = policy.rules_file.as_ref()?;
    standing_rules_in(&std::fs::read_to_string(path).ok()?)
}

/// [`standing_rules`] of the rules file's text `text`, read by the caller: `None` when it is empty.
#[must_use]
pub fn standing_rules_in(text: &str) -> Option<String> {
    let line = capped_rules(&limit::one_line(text));
    (!line.is_empty()).then_some(line)
}

/// `rules` as one line of at most [`RULES_CAP`] characters.
pub(super) fn capped_rules(line: &str) -> String {
    if line.chars().count() <= RULES_CAP {
        return line.to_string();
    }
    let head: String = line.chars().take(RULES_CAP).collect();
    let cut = head.rfind(' ').filter(|&i| i > 0).unwrap_or(head.len());
    format!("{}…", head[..cut].trim_end())
}

/// The unix second nearest `ahead_ms` past the unix millisecond `now_ms`.
pub(super) fn nearest_s(now_ms: i64, ahead_ms: i64) -> i64 {
    let off = now_ms.rem_euclid(1000).saturating_add(ahead_ms);
    let whole = i64::try_from(off.unsigned_abs().saturating_add(500) / 1000).unwrap_or(i64::MAX);
    now_ms
        .div_euclid(1000)
        .saturating_add(off.signum().saturating_mul(whole))
}

impl<C: Ctl> Session<'_, C> {
    /// `t` on the loop's clock as unix seconds, the nearest, against the clock to the millisecond.
    pub(super) fn unix_of(&self, t: Instant) -> i64 {
        let (now_ms, ahead) = self.ms_ahead(t);
        nearest_s(now_ms, ahead)
    }

    /// `t` on the loop's clock as unix milliseconds ([`Self::unix_of`] to the millisecond).
    pub(super) fn unix_ms_of(&self, t: Instant) -> i64 {
        let (now_ms, ahead) = self.ms_ahead(t);
        now_ms.saturating_add(ahead)
    }

    /// The wall clock now (unix milliseconds) and how many milliseconds
    /// `t` lies ahead of now on the monotonic clock (negative: behind).
    fn ms_ahead(&self, t: Instant) -> (i64, i64) {
        let now = self.fx.mono();
        let ms = |d: Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
        let ahead = if t >= now { ms(t - now) } else { -ms(now - t) };
        (self.now_unix_ms(), ahead)
    }

    /// Stand `at` (on the loop's clock) as the policy's deadline at the point still showing ([`Self::turn_end_due`]), and why.
    pub(super) fn set_due(&mut self, at: Instant, why: impl Into<String>) {
        self.set_wait(at, why, WaitKind::Other);
    }

    /// [`Self::set_due`], `wait` the wait's kind (a [`TurnEndAction::WaitUntil`]'s `wait`).
    pub(super) fn set_wait(&mut self, at: Instant, why: impl Into<String>, wait: WaitKind) {
        self.turn_end_due = Some(Due {
            at,
            unix_ms: self.unix_ms_of(at),
            why: why.into(),
            wait,
        });
    }

    /// When the policy's deadline falls ([`Due`]) on the loop's clock now.
    pub(super) fn due_at(&self) -> Option<Instant> {
        self.turn_end_due.as_ref().map(|due| self.falls_at(due))
    }

    /// When `due` falls on the loop's clock now ([`Self::due_at`]'s rule).
    fn falls_at(&self, due: &Due) -> Instant {
        let wall = self.instant_of_ms(due.unix_ms);
        let ahead =
            i64::try_from(due.at.saturating_duration_since(wall).as_secs()).unwrap_or(i64::MAX);
        if super::slept_through(ahead, 0) {
            wall
        } else {
            due.at
        }
    }

    /// When the wake's settle falls ([`Self::settle_wake`]), while it is still ahead.
    pub(super) fn settling(&mut self) -> Option<Instant> {
        let at = self
            .wake_settle
            .as_ref()
            .map(|(settle, _)| self.falls_at(settle));
        let ahead = at.filter(|&at| self.fx.mono() < at);
        if ahead.is_none() {
            self.wake_settle = None;
        }
        ahead
    }

    /// `t` on the loop's clock as a UTC stamp, for the journal (a `t` already past stamps now).
    pub(super) fn stamp_in(&self, t: Instant) -> String {
        let secs = self.unix_of(t.max(self.fx.mono()));
        utc_stamp(u64::try_from(secs).unwrap_or(0))
    }

    /// `unix` (epoch seconds) on the loop's clock, placed to the millisecond ([`Self::now_unix_ms`]).
    pub(super) fn instant_of(&self, unix: i64) -> Instant {
        self.instant_of_ms(unix.saturating_mul(1000))
    }

    /// WHETHER THE CREDITS HOLD STANDS FOR A TURN THE HOST WOULD TYPE on `rows` (`Guard::Credits`).
    pub(super) fn credits_hold_here(&self, cfg: &SupervisorConfig, rows: &[String]) -> bool {
        let reader = self.reader(rows);
        let read = self.credits_carry(reader.credits(rows).as_ref());
        self.turn_end
            .credits_hold_stands(read.as_ref(), reader.program(), cfg, self.fx.mono())
    }

    /// Usage credits carry the session, a read's notice says (`drawn`), or the loop keeps one that stands without it ([`Self::note_credits`]).
    pub(super) fn credits_carry(
        &self,
        drawn: Option<&aterm_phase::credits::Credits>,
    ) -> Option<CreditsCarry> {
        let kept = self.credits_kept.as_ref();
        if drawn.is_none() && kept.is_none() {
            return None;
        }
        Some(CreditsCarry {
            reset_at: kept
                .and_then(|k| k.reset.as_ref())
                .and_then(|r| r.unix)
                .map(|unix| self.instant_of(unix)),
            since: kept.map(|k| k.since),
            model: drawn
                .and_then(|c| c.model.as_deref())
                .and_then(super::super::policy::turn_end::credits_model)
                .or_else(|| kept.and_then(|k| k.model.clone())),
            kept: drawn.is_none(),
        })
    }

    /// [`Self::instant_of`] for a unix millisecond ([`Self::unix_ms_of`]'s inverse).
    pub(super) fn instant_of_ms(&self, unix_ms: i64) -> Instant {
        let now = self.fx.mono();
        let delta = unix_ms.saturating_sub(self.now_unix_ms());
        let d = Duration::from_millis(delta.unsigned_abs());
        if delta >= 0 {
            now + d
        } else {
            now.checked_sub(d).unwrap_or(now)
        }
    }

    /// The model switch an earlier loop left open, its reset placed on this loop's clock at the second the earlier row named.
    pub(super) fn carried_model_switch(&self, open: approvals::OpenSwitch) -> ModelSwitch {
        ModelSwitch {
            from: open.from,
            to: open.to,
            back_at: open.back_at_unix.map(|unix| self.instant_of(unix)),
        }
    }

    /// THE MODEL SWITCH AN EARLIER LOOP MADE, taken up from the ledger at `path` ([`approvals::open_model_switch`], `drive_loop`'s seed).
    #[cfg(test)]
    pub(super) fn seed_model_switch(&mut self, path: &std::path::Path) {
        let body = self.fx.read_text(path).unwrap_or_default();
        self.seed_model_switch_in(&body, None);
    }

    /// [`Self::seed_model_switch`] over the ledger's text.
    pub(super) fn seed_model_switch_in(&mut self, body: &str, journal: Option<&str>) {
        let fallback = Restart::Model { to: String::new() };
        let journaled = journal.map_or_else(JournaledRelaunch::default, |j| {
            journaled_relaunch(j, self.setup.sid.as_deref(), fallback.word())
        });
        if let Some(made) =
            approvals::relaunch_waiting_in(body, self.setup.sid.as_deref()).or(journaled.waiting)
        {
            self.restart_run = Some((fallback.word(), made));
        }
        let Some(open) = approvals::open_model_switch_in(body, self.setup.sid.as_deref()) else {
            return;
        };
        let made_at = journaled
            .made
            .map_or(open.made_unix, |m| m.min(open.made_unix));
        let asked = open.asked;
        let switch = self.carried_model_switch(open);
        self.turn_end.seed_model(switch);
        if !asked {
            self.turn_end.seed_fallback_unasked(made_at);
        }
    }
    /// [`standing_rules`] of the loop's policy.
    fn rules_text(&self, opts: &SuperviseOpts) -> Option<String> {
        let path = opts.policy.rules_file.as_ref()?;
        standing_rules_in(&self.fx.read_text(path)?)
    }

    /// A wall's `reset=` text on the loop's clock, read once per text and kind (the same text is the same reset, [`Self::refresh_reset`]).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn wall_reset_at(
        &mut self,
        text: Option<&str>,
        kind: aterm_phase::WallKind,
        session_window: bool,
        live: bool,
        answered: bool,
        undated: bool,
        rows: &[String],
    ) -> Option<Instant> {
        let key = reset_key(kind, text);
        let reprint = (live || answered)
            && !session_window
            && places_resets_on_print(self.reader(rows).program())
            && matches!(
                text.and_then(limit::parse_reset),
                Some(limit::ResetSpec::At { date: None, .. })
            );
        let unix = match &self.wall_reset {
            Some((k, u)) if *k == key && !reprint => *u,
            _ => {
                let same = self
                    .wall_reset
                    .as_ref()
                    .filter(|(k, _)| *k == key)
                    .and_then(|(_, o)| *o);
                let mut u = self.reset_unix_for(text, session_window, live, answered, rows);
                if undated {
                    u = u.map(|at| self.within_a_session_window(text, at));
                }
                let now_unix = self.now_unix();
                match (reprint, same, u) {
                    (true, Some(old), Some(new))
                        if new > old.saturating_add(REPRINT_JUMP_MAX_S)
                            && now_unix.saturating_sub(old) <= REPRINT_HOLD_WINDOW_S
                            && self.reprint_jumps < REPRINT_JUMPS_MAX =>
                    {
                        self.reprint_jumps += 1;
                        u = Some(old);
                    }
                    (_, None, _) => self.reprint_jumps = 0,
                    _ => {}
                }
                self.wall_reset = Some((key, u));
                u
            }
        };
        unix.map(|unix| self.instant_of(unix))
    }

    /// THE USAGE TAP TIMES THE WALL (accounts v2 S10a, [`super::super::tap_clock`]):
    /// at a Claude Code wall a window times ([`UsageWindow::of_wall`]), a
    /// DECISION's read asks the host for the tap's newest reading of each
    /// ([`IdleHost::tap_window`]) and folds them into the wall's
    /// [`TapClock`], kept from the loop's first read of this wall — its kind
    /// and words ([`reset_key`]), at this answer of it (`turn_end_reading`
    /// drops the clock at a new one) — so a reading the tap wrote before that
    /// is none of it. The reset the wall is waited to, and its books, are then
    /// [`Self::tap_timing`]'s, returned (`Some(None)`: no reset at all). A
    /// read for the books after the decision ([`Check::AsDecided`]) asks
    /// nothing and keeps what the decision read. `None`: no tap times this
    /// wall — another program, a wall no window names, no host — and the
    /// notice's reset stands.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn tap_reset(
        &mut self,
        host: Option<&Arc<dyn IdleHost>>,
        kind: aterm_phase::WallKind,
        message: &str,
        text: Option<&str>,
        program: Program,
        check: Check,
        seq: u64,
    ) -> Option<Option<i64>> {
        // A recording build without this observer took neither its clock
        // reads nor its host queries. Leave both out of that replay.
        if self.fx.replay_lacks_host_door("host.tap_window") {
            self.wall_tap = None;
            return None;
        }
        let tapped = match program {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        let windows = UsageWindow::of_wall(kind, message);
        let Some(host) = host.filter(|_| tapped && !windows.is_empty()) else {
            self.wall_tap = None;
            return None;
        };
        let key = reset_key(kind, text);
        let now = self.now_unix();
        let mut tap = match self.wall_tap.take() {
            Some(tap) if tap.key == key => tap,
            _ => WallTap {
                key,
                windows,
                clock: TapClock::new(now),
                at: None,
                said: None,
                by_tap: false,
                carried: None,
            },
        };
        if check == Check::Decide {
            let readings: Vec<_> = windows.iter().map(|w| (*w, host.tap_window(*w))).collect();
            tap.clock.step(&readings, now);
        }
        self.wall_tap = Some(tap);
        self.tap_timing(seq).map(|(at, _)| at)
    }

    /// THE RESET THE WALL IS WAITED TO, AND ITS BOOKS — the one place the
    /// tap's timing of the wall changes, whichever read changed it (a read of
    /// the wall, [`Self::tap_reset`]; a step of its wait, the notice perhaps
    /// off the screen, [`Self::tap_edge`]): the notice's placed reset
    /// ([`Self::wall_reset_at`]) and what the wall's [`TapClock`] says,
    /// together ([`timed`]) — or, the clock saying nothing yet, the reset a
    /// loop before this one's tap timed ([`WallTap::carried`]) — kept for the
    /// loop's books ([`Self::wall_reset_unix`]). A disagreement over
    /// [`limit::CLOCK_SLACK_S`] not yet written is one ledger row
    /// ([`approvals::RULE_TAP_RESET`]), never an alert. The wall's own row
    /// ([`Self::persist_wall`]) then carries the tap's reset apart from the
    /// notice's, so a loop that takes the session over
    /// ([`Self::seed_wall_memory`], with no tap clock of its own) waits to it
    /// too and sets its own tap against the notice's: written by the read's own
    /// books at a read of the wall (after the answer of a try no loop read is
    /// dated, [`Self::date_unread_try`], whose row the wall's follows), and by
    /// [`Self::tap_edge`] at a step of its wait. The reset (`None`: none
    /// known) and whether it moved; `None` with no tap at this wall.
    fn tap_timing(&mut self, seq: u64) -> Option<(Option<i64>, bool)> {
        let notice = self.wall_reset.clone();
        let tap = self.wall_tap.as_mut()?;
        let notice = notice.filter(|(k, _)| *k == tap.key).and_then(|(_, u)| u);
        let says = tap.clock.says(tap.windows);
        let t = match (says, tap.carried) {
            (TapSays::Nothing, Some(at)) => Timed {
                at: Some(at),
                disagree: None,
            },
            _ => timed(notice, says),
        };
        let moved = t.at != tap.at;
        tap.at = t.at;
        tap.by_tap = says != TapSays::Nothing || tap.carried.is_some();
        let told = t.disagree.filter(|d| tap.said != Some(*d));
        if let Some(d) = told {
            tap.said = Some(d);
        }
        if let (Some((notice, tap_unix)), Some(at)) = (told, t.at) {
            let reason = approvals::tap_reset_reason(notice, tap_unix, at);
            self.ledger_row(
                approvals::RULE_TAP_RESET,
                approvals::Outcome::Waiting,
                "",
                &reason,
                seq,
            );
        }
        Some((t.at, moved))
    }

    /// THE RESET THE STANDING WALL IS WAITED TO (unix seconds): the usage
    /// tap's and the notice's together where the tap times it
    /// ([`Self::tap_reset`]), else the notice's as placed
    /// ([`Self::wall_reset_at`]) — what the session's jitter reads (the
    /// budget's stretch takes the reset read when the episode opened).
    pub(super) fn wall_reset_unix(&self) -> Option<i64> {
        let (key, notice) = self.wall_reset.as_ref()?;
        self.wall_tap
            .as_ref()
            .filter(|t| t.key == *key)
            .map_or(*notice, |t| t.at)
    }

    /// THE RESET THE USAGE TAP TIMED THE STANDING WALL TO (unix seconds),
    /// where it did ([`WallTap::by_tap`]): what the wall's ledger row keeps
    /// apart from the notice's placement ([`Self::persist_wall`],
    /// `tap_unix=`). `None` where no tap said anything of this wall.
    pub(super) fn tap_unix(&self) -> Option<i64> {
        let (key, _) = self.wall_reset.as_ref()?;
        self.wall_tap
            .as_ref()
            .filter(|t| t.key == *key && t.by_tap)
            .and_then(|t| t.at)
    }

    /// A LOOP BEFORE THIS ONE'S TAP TIMED THE WALL IT TAKES UP to `at` (the
    /// row's `tap_unix=`, [`Self::seed_wall_memory`]): the wall of `kind`
    /// and `message`, its key `key`, is waited to `at` until this loop's own
    /// tap says something of it ([`WallTap::carried`]); a new answer of the
    /// wall drops it with the clock (`turn_end_reading`). Its clock counts
    /// readings from `now`, the seed's.
    pub(super) fn seed_tap(
        &mut self,
        key: String,
        kind: aterm_phase::WallKind,
        message: &str,
        at: i64,
        now: i64,
    ) {
        self.wall_tap = Some(WallTap {
            key,
            windows: UsageWindow::of_wall(kind, message),
            clock: TapClock::new(now),
            at: Some(at),
            said: None,
            by_tap: true,
            carried: Some(at),
        });
    }

    /// THE USAGE TAP MOVED THE RESET WHILE THE LOOP WAITS ([`Self::tap_reset`]):
    /// asked at the top of each step of a wait ([`Self::wait_for_next`], the
    /// point's `seq`) at a wall the tap times, and an edge — the point
    /// decided again at once — where the reset the wall is waited to now
    /// differs from the one decided: the tap's run after the wall was read
    /// (it runs 0.4 s after the 429 and every 10 s), or its run at the reset
    /// showing the window rolled over. The readings are folded into the
    /// wall's clock here, the timing set by [`Self::tap_timing`] and the
    /// wall's row written ([`Self::persist_wall`]) — so a decision that does
    /// not read the wall again (its notice left the screen, and
    /// [`Self::tap_left_wall`] hands the policy the new reset) makes this an
    /// edge once, and the ledger holds the reset waited to all the same.
    /// Only while a wait of the policy's stands ([`Self::turn_end_due`],
    /// [`Self::reach_edge`]'s rule), and only at a wall the tap times: the
    /// loop's clock is read (a harness tape's record) only then, not at every
    /// step of every wait.
    pub(super) fn tap_edge(&mut self, opts: &SuperviseOpts, seq: u64) -> bool {
        if self.fx.replay_lacks_host_door("host.tap_window") {
            return false;
        }
        if self.turn_end_due.is_none() || self.wall_tap.is_none() {
            return false;
        }
        let Some(host) = opts.idle_host.as_ref() else {
            return false;
        };
        let now = self.now_unix();
        let Some(tap) = self.wall_tap.as_mut() else {
            return false;
        };
        let readings: Vec<_> = tap
            .windows
            .iter()
            .map(|w| (*w, host.tap_window(*w)))
            .collect();
        tap.clock.step(&readings, now);
        let moved = self.tap_timing(seq).is_some_and(|(_, moved)| moved);
        self.persist_wall(seq);
        moved
    }

    /// A WALL WHOSE NOTICE LEFT THE SCREEN, waited on to the reset it named
    /// ([`TurnEndState::waits_on_named_reset`]: Esc on the vendor's countdown
    /// leaves the cancel the last thing said): where the usage tap timed that
    /// wall ([`Self::tap_edge`] hears it while the loop waits), `reading` —
    /// one with no wall — names the tap's reset and the session's share of
    /// it, which the policy's fold takes as the limit's reset from then on
    /// (`TurnEndState::observe`).
    fn tap_left_wall(&self, reading: &mut TurnEndReading) {
        if reading.wall.is_some() || !self.turn_end.waits_on_named_reset() {
            return;
        }
        let Some(at) = self
            .wall_tap
            .as_ref()
            .filter(|t| t.by_tap)
            .and_then(|t| t.at)
        else {
            return;
        };
        reading.reset_at = Some(self.instant_of(at));
        reading.jitter = self.setup.sid.as_deref().map_or(Duration::ZERO, |sid| {
            resume_jitter(sid, at, self.turn_end.timing.reset_jitter)
        });
    }

    /// `at`, the placement of the reset `text` names, as an undated one is kept
    /// ([`Self::wall_reset_at`]): a bare clock placed more than [`limit::SESSION_AHEAD_MAX`] ahead
    /// is the day before's, passed.
    fn within_a_session_window(&self, text: Option<&str>, at: i64) -> i64 {
        text.and_then(limit::parse_reset).map_or(at, |spec| {
            limit::within_a_session_window(&spec, at, self.now_unix())
        })
    }

    /// How long ago a person last typed into the session ([`TurnEndReading::person`]).
    pub(super) fn person_ago(&self, now: Instant) -> Option<Duration> {
        self.person_at()
            .map(|typed| now.saturating_duration_since(typed))
    }

    /// When a person last typed into the session ([`Self::person_ago`]).
    pub(super) fn person_at(&self) -> Option<Instant> {
        let draft = self.draft_seen.as_ref().map(|(_, at)| *at);
        self.person.max(draft)
    }

    /// THE COUNT A SCREEN WAS READ WITH AND THE MARK OF ITS CONVERSATION, as
    /// a reading of it marks it ([`conversation_mark`],
    /// [`TurnEndReading::screen_mark`]): the baseline set once the harness's
    /// own keys were seen to land ([`TurnEndState::counted_after_harness`]),
    /// which the next reading's mark is set against.
    fn counted_conversation(&self, screen: &Screen) -> Option<(u64, u64)> {
        let reading = self
            .reader(&screen.rows)
            .read(&screen.rows, Some(screen.cursor_col));
        screen
            .submits
            .map(|n| (n, conversation_mark(&reading, &screen.rows)))
    }

    /// The turn-end reading of `screen`.
    pub(super) fn turn_end_reading(
        &mut self,
        screen: &Screen,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        self.turn_end_reading_as(screen, worked, opts, Check::Decide, true)
    }

    /// The reading of a point already decided, for the loop's books after its act.
    pub(super) fn turn_end_reread(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        self.turn_end_reading_as(screen, None, opts, Check::AsDecided, true)
    }

    /// [`Self::turn_end_reread`] of the point an account move's relaunch was made at
    /// ([`Restart::Account`]), telling nothing ([`Self::account_tells`]): the host judges the
    /// process its tab runs now, under the new login, so a lost login told from the old screen
    /// would strike the new account.
    fn turn_end_reread_relaunched(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        self.turn_end_reading_as(screen, None, opts, Check::AsDecided, false)
    }

    /// A POINT BEFORE ANY PROMPT SINCE A LAUNCH WITH NOTHING KEYED SINCE (P3
    /// of `docs/measured/forever-lab-2026-10-20.md`): this loop is the
    /// session's first ([`Self::first_books`]) or the agent's process was
    /// launched after the newest row of the books it found
    /// ([`Self::books_last_ms`]), it has judged no point, the
    /// screen `r` holds nothing of a conversation
    /// ([`TurnEndReading::holds_no_conversation`]: no message, no reply, no
    /// draft, no wall, no turn read running since), and the host saw the
    /// agent launched with no person's gesture since ([`IdleHost::launch`],
    /// asked only here, once the rest holds).
    fn unasked_launch(&self, r: &TurnEndReading, host: Option<&Arc<dyn IdleHost>>) -> bool {
        (self.first_books || self.books_last_ms.is_some())
            && self.turn_end.point_number().is_none()
            && r.holds_no_conversation()
            && if self.first_books {
                host.is_some_and(|h| h.launch().is_some())
            } else {
                self.launched_since_books(host)
            }
    }

    /// THE AGENT'S PROCESS WAS LAUNCHED SINCE THE NEWEST ROW OF THE BOOKS
    /// THIS LOOP FOUND ([`Self::books_last_ms`], [`IdleHost::launch`] past
    /// it): no loop before this one watched it.
    fn launched_since_books(&self, host: Option<&Arc<dyn IdleHost>>) -> bool {
        !self.first_books
            && self.books_last_ms.is_some_and(|last| {
                host.and_then(|h| h.launch())
                    .is_some_and(|launched| launched > last)
            })
    }

    fn turn_end_reading_as(
        &mut self,
        screen: &Screen,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
        check: Check,
        tell: bool,
    ) -> TurnEndReading {
        let reader = self.reader(&screen.rows);
        if check == Check::Decide {
            self.scope_limit_conversation(reader.program(), opts.idle_host.is_some(), screen.seq);
        }
        let mut reading = reader.read(&screen.rows, Some(screen.cursor_col));
        // Gemini CLI's usage-limit dialog answered `Stop` by another hand: the
        // error it leaves is that limit ([`Self::limit_stopped_adopt`]).
        self.limit_stopped_adopt(&reading, opts);
        self.limit_stopped_seat(&reading, &screen.rows);
        let stopped = self.limit_stopped(&reading, &screen.rows);
        if let Some(s) = &stopped {
            reading.phase = Phase::Limited {
                message: s.wall.message.clone(),
                reset: s.wall.reset.clone(),
            };
            reading.wall = Some(s.wall.clone());
        }
        if reading.phase == Phase::Busy && self.transcript_over_seq == Some(screen.seq) {
            reading.phase = Phase::Idle;
        }
        if check == Check::Decide
            && let Some(started) = opts.idle_host.as_ref().and_then(|h| h.agent_started())
        {
            let at = self.instant_of(started);
            self.turn_end.agent_started(at);
            self.turn_end.agent_started_unix(started);
        }
        let worked = worked.filter(|_| !reader.housekeeping(&screen.rows));
        self.read_left_answer(&mut reading, screen, check);
        self.confirm_wall(&mut reading, screen, opts, check);
        if check == Check::Decide
            && reading.wall.is_none()
            && match reading.program {
                Program::Claude => true,
                Program::Codex
                | Program::Agy
                | Program::Muse
                | Program::Gemini
                | Program::Generic => false,
            }
            && let Some(said) = aterm_phase::phase::last_said_notice(&screen.rows)
            && !said.hook_words
        {
            self.turn_end.compact_answered(&said.text);
        }
        let standing_remembered =
            self.read_standing_limit(&mut reading, screen, worked.is_some(), opts, check);
        let wall_was_hidden = reading.wall.is_none();
        self.wall_memory_step(reader, &screen.rows, &mut reading, screen.seq);
        let remembered_wall = (wall_was_hidden || standing_remembered) && reading.wall.is_some();
        let reset_at = match (&stopped, &reading.wall) {
            (Some(s), _) => {
                self.wall_reset = Some((
                    reset_key(s.wall.kind, s.wall.reset.as_deref()),
                    Some(s.reset_unix),
                ));
                Some(self.instant_of(s.reset_unix))
            }
            (None, Some(w)) => {
                let text = w.reset.clone();
                let session_window = w.kind == aterm_phase::WallKind::UsageSession
                    && limit::names_a_session_window(&w.message);
                let left = self.left_read.as_ref().filter(|l| l.seq == screen.seq);
                let undated = left.is_some_and(|l| !l.dated);
                let answer = left
                    .filter(|l| l.dated)
                    .map(|l| l.answer.since)
                    .or_else(|| self.turn_end.untaken_act())
                    .map(|at| (at, reset_key(w.kind, text.as_deref())))
                    .filter(|answer| self.wall_answered.as_ref() != Some(answer));
                let answered = answer.is_some();
                if answer.is_some() {
                    self.wall_answered = answer;
                }
                let notice = self.wall_reset_at(
                    text.as_deref(),
                    w.kind,
                    session_window,
                    worked.is_some(),
                    answered,
                    undated,
                    &screen.rows,
                );
                // A limit only the transcript shows is reset where its row
                // says (`quotaLimits.resetsAt`), not where its words would be
                // placed on a print the screen does not show: that is its
                // notice's reset, which the tap's timing, the jitter and the
                // wall's row go by too.
                let notice = match self.standing_until(screen.seq) {
                    Some(until) => {
                        self.wall_reset = Some((reset_key(w.kind, text.as_deref()), Some(until)));
                        Some(self.instant_of(until))
                    }
                    None => notice,
                };
                // …and timed by the usage tap where it reads the reset
                // (accounts v2 S10a): the same act, at the tap's reset. A NEW
                // ANSWER of the wall — a turn the loop saw run ended on it, or
                // it answered an act of the loop's — is read afresh, as its
                // reset is placed afresh above: what the tap read of the
                // answer before is none of this one's.
                if worked.is_some() || answered {
                    self.wall_tap = None;
                }
                let host = opts.idle_host.as_ref();
                let program = reading.program;
                match self.tap_reset(
                    host,
                    w.kind,
                    &w.message,
                    text.as_deref(),
                    program,
                    check,
                    screen.seq,
                ) {
                    Some(at) => at.map(|unix| self.instant_of(unix)),
                    None => notice,
                }
            }
            (None, None) => {
                // rule, [`TurnEndState::observe`]): the next is a new reset.
                if worked.is_some() {
                    self.wall_reset = None;
                    self.reprint_jumps = 0;
                }
                // The tap's clock is the wall's: kept while the policy waits
                // on the reset a wall whose notice left named, so its wait's
                // steps hear the tap ([`Self::tap_edge`]), else gone.
                if worked.is_some() || !self.turn_end.waits_on_named_reset() {
                    self.wall_tap = None;
                }
                None
            }
        };
        self.date_unread_try(&reading, &screen.rows, screen.seq);
        if reading.wall.is_some() {
            self.persist_wall(screen.seq);
        }
        self.reach_seen = reading
            .wall
            .as_ref()
            .filter(|w| answered_by_the_network(w.kind))
            .map(|_| {
                opts.idle_host
                    .as_ref()
                    .map_or(Reach::Unknown, |h| h.reach())
            });
        let composer = self.composer_read(&reading, screen);
        let r = TurnEndReading::of_slept(
            &reading,
            &screen.rows,
            composer,
            worked,
            self.turn_asleep(&screen.rows),
            reset_at,
            self.rules_text(opts),
        );
        let host = opts.idle_host.as_ref();
        let jitter = match (&reading.wall, self.setup.sid.as_deref()) {
            (Some(w), Some(sid)) if resumes_at_reset(w.kind) => resume_jitter(
                sid,
                self.wall_reset_unix().unwrap_or(0),
                self.turn_end.timing.reset_jitter,
            ),
            _ => Duration::ZERO,
        };
        self.memory_seen = host
            .and_then(|h| h.memory())
            .and_then(|m| super::super::memory::judge(opts.policy.memory_ceiling, &m))
            .filter(super::super::memory::MemoryOver::acts);
        let measured = self.memory_seen.filter(|_| {
            r.wall != Some(aterm_phase::WallKind::Memory)
                && matches!(r.phase, Phase::Idle | Phase::Limited { .. })
        });
        self.memory_wall = measured.is_some();
        self.memory_over_wall =
            self.memory_wall && (r.wall.is_some() || matches!(r.phase, Phase::Limited { .. }));
        let login_label = host
            .filter(|_| {
                opts.policy.logins
                    && match r.program {
                        Program::Claude => true,
                        Program::Codex
                        | Program::Agy
                        | Program::Muse
                        | Program::Gemini
                        | Program::Generic => false,
                    }
            })
            .and_then(|h| h.login_label());
        let login_track = self.turn_end.login_track().is_some();
        let lost = host.filter(|_| {
            login_label.is_some()
                && (r.wall == Some(aterm_phase::WallKind::Auth)
                    || (login_track && (r.login_screen.is_some() || r.login_interrupted)))
        });
        let stamp = lost.map(|h| h.account_stamp());
        let login_superseded = lost.is_some_and(|h| h.login_superseded());
        if check == Check::Decide {
            self.login_seen = stamp.filter(|_| !login_superseded);
        }
        let login_method = host
            .filter(|_| {
                super::super::policy::login::carried(r.program)
                    && (r.wall == Some(aterm_phase::WallKind::Auth)
                        || r.login_screen.is_some()
                        || r.login_interrupted)
            })
            .and_then(|h| h.login_method());
        let login_shared = host
            .filter(|_| {
                super::super::policy::login::carried(r.program)
                    && (r.wall == Some(aterm_phase::WallKind::Auth)
                        || (login_track && (r.login_screen.is_some() || r.login_interrupted)))
            })
            .and_then(|h| h.login_shared());
        let mut reading = TurnEndReading {
            conversation: self
                .wall_verdict
                .as_ref()
                .filter(|v| v.seq == screen.seq)
                .and_then(|v| v.conversation.clone())
                .or_else(|| {
                    self.wall_conversation
                        .as_ref()
                        .filter(|(seq, _)| *seq == screen.seq)
                        .map(|(_, c)| c.clone())
                })
                .unwrap_or_else(|| Conversation::unread(host.is_some(), r.program)),
            jitter,
            reach: self.reach_seen.unwrap_or_default(),
            person: self
                .person_ago(self.fx.mono())
                .into_iter()
                .chain(screen.human.ago())
                .min(),
            upgrading: self.limit.is_none() && host.is_some_and(|h| h.owns_turn_end()),
            restartable: host.is_some_and(|h| h.can_restart()),
            resume: host.and_then(|h| h.resume_command()),
            login_label,
            login_superseded,
            login_method,
            login_shared,
            taskless: (r.taskless
                && !self
                    .turn_end
                    .redrawn_fresh(screen.submits, screen.inputs_after))
                || host.is_some_and(|h| h.taskless())
                || self.unasked_launch(&r, host),
            finished_record: self
                .turn_end
                .reads_record(&r)
                .then(|| host.and_then(|h| h.finished()))
                .flatten(),
            autopilot: self.autopilot(opts),
            credits: self.credits_carry(reader.credits(&screen.rows).as_ref()),
            submits: screen.submits,
            inputs_before: screen.inputs_before,
            ..r
        };
        if reading.finished_record.is_some()
            && (self.turn_end.untaken_act().is_some() || self.turn_end.land_act_seeded())
        {
            let now_ms = self.now_unix_ms();
            reading.record_turn = host
                .and_then(|h| h.last_turn())
                .and_then(|t| RecordTurn::of(&t, now_ms));
        }
        let reader = reading.program.reader();
        let turn_cut = reading.said_tail.is_some() && !reader.turn_whole(&screen.rows);
        if check == Check::Decide
            && (reading.said_cut || turn_cut)
            && (super::super::policy::turn_end::says_done(reading.said_tail.as_deref())
                || self.turn_end.reads_turn_whole(&reading, worked.is_some()))
        {
            let turn = self.said_read_whole(screen, reader, true);
            let turn_read = turn.is_some();
            let read = turn.or_else(|| {
                reading
                    .said_cut
                    .then(|| self.said_read_whole(screen, reader, false))
                    .flatten()
            });
            if let Some(rows) = read {
                reading.said_cut = !reader.said_whole(&rows);
                if !reading.said_cut {
                    reading.said_tail = reader.said_tail(&rows);
                    reading.turn_said = reader.turn_said_before(&rows);
                    reading.opens_cut = !reader.bounds_messages() && !reader.turn_whole(&rows);
                    if let Some(opened) = reader.turn_opened_by(&rows) {
                        reading.opened_by = Some(super::super::approvals::words_lead(&opened));
                    }
                }
            }
            reading.said_cut |= turn_cut && !turn_read;
        }
        if check == Check::Decide {
            reading.repo = self.repo_read(&reading, host, worked.is_some());
        }
        if check == Check::Decide
            && !self.left_keys.is_empty()
            && self.turn_end.reads_record(&reading)
        {
            let walled = reading.wall.is_some();
            let turn = reading
                .finished_record
                .and(host.and_then(|h| h.last_turn()))
                .filter(|t| t.key().is_some())
                .or_else(|| Self::last_turn(opts, reading.said_tail.as_deref()));
            let at_wall = turn.as_ref().filter(|_| walled).and_then(|turn| {
                self.left_keys.iter().rev().find_map(|k| {
                    approvals::parse_left_at_wall(k)
                        .filter(|(key, _)| turn.names(key))
                        .map(|(_, work)| work)
                        .or_else(|| {
                            let (_, wall) = approvals::parse_left_retried(k)?;
                            let before = approvals::parse_left_at_wall(wall)?.1;
                            turn.names(k).then(|| {
                                Some(
                                    before
                                        .unwrap_or_default()
                                        .saturating_add(turn.ended.unwrap_or_default()),
                                )
                            })
                        })
                })
            });
            reading.left_record = turn
                .as_ref()
                .is_some_and(|t| self.left_keys.iter().any(|k| t.names(k)))
                || at_wall.is_some();
            reading.left_work = at_wall.flatten();
        }
        if check == Check::Decide
            && self.turn_end.has_limit_track()
            && (reading.wall.is_none() || remembered_wall)
            && reads_conversation(reading.program)
        {
            let read = self.limit_transcript(opts);
            reading.conversation = read.conversation;
            reading.notice_elsewhere = read.elsewhere;
            if reading.since_limit.is_none() && reading.worked.is_none() {
                reading.served_in_transcript = read.served;
                reading.chosen_in_transcript = read.chosen;
            }
        }
        if remembered_wall {
            reading = self.turn_end.read_remembered_wall(reading);
        }
        if let Some(over) = measured {
            reading.wall = Some(aterm_phase::WallKind::Memory);
            reading.wall_message = over.fact();
            reading.wall_remembered = false;
        }
        self.tap_left_wall(&mut reading);
        if tell {
            self.account_tells(&mut reading, screen.seq, opts);
        }
        if check == Check::Decide
            && match reading.program {
                Program::Claude => true,
                Program::Codex
                | Program::Agy
                | Program::Muse
                | Program::Gemini
                | Program::Generic => false,
            }
            && reading.phase == Phase::Idle
            && let Some(host) = host
        {
            host.usage_tap();
        }
        if check == Check::Decide
            && let Some(v) = self.wall_verdict.as_mut().filter(|v| v.seq == screen.seq)
        {
            v.conversation = Some(reading.conversation.clone());
        }
        reading
    }

    /// The limit a cancel left, read in `reading`'s place where it answered a turn
    /// ([`TurnEndState::left_answer`]) and the reader reads no wall: the reading takes that wall
    /// and the `limited` phase.
    fn read_left_answer(
        &mut self,
        reading: &mut aterm_phase::Reading,
        screen: &Screen,
        check: Check,
    ) {
        let decided = self
            .left_read
            .as_ref()
            .filter(|l| l.seq == screen.seq)
            .map(|l| l.answer.wall.clone());
        let left = match (check, decided) {
            (Check::AsDecided, Some(w)) => Some(w),
            _ => {
                let left = match reading.program {
                    Program::Claude => reading
                        .wall
                        .is_none()
                        .then(|| self.turn_end.left_answer(&screen.rows))
                        .flatten(),
                    Program::Codex
                    | Program::Agy
                    | Program::Muse
                    | Program::Gemini
                    | Program::Generic => None,
                };
                let wall = left.as_ref().map(|l| l.wall.clone());
                self.left_read = left.map(|answer| LeftRead {
                    seq: screen.seq,
                    answer,
                    dated: false,
                });
                wall
            }
        };
        if let Some(w) = left.filter(|_| reading.wall.is_none()) {
            reading.phase = Phase::Limited {
                message: w.message.clone(),
                reset: w.reset.clone(),
            };
            reading.wall = Some(w);
        }
    }

    /// A DECISION BEGINS ([`Self::turn_end_at_point`], [`Self::turn_end_now`]):
    /// what the host advised about
    /// the account for the decision before is forgotten, so this one asks
    /// again ([`Self::account_advise`]) — a wait that ran out is how a
    /// session with no account to run on learns that one is usable.
    fn account_round(&mut self) {
        self.account_seen = None;
        self.roster_seen = None;
        self.nothing_decided = false;
    }

    /// The session's account as every reading of a point carries it
    /// (`docs/DESIGN-claude-accounts-2026-10-01.md` §6):
    fn account_tells(&mut self, reading: &mut TurnEndReading, seq: u64, opts: &SuperviseOpts) {
        let Some(host) = opts.idle_host.as_ref() else {
            return;
        };
        if opts.policy.logins
            && let Some(days) = reading.login_notice
            && self.login_notice_told != Some(days)
        {
            host.login_notice(days);
            self.login_notice_told = Some(days);
        }
        if opts.policy.accounts == crate::harness::accounts::Mode::Off && !opts.policy.logins {
            return;
        }
        if let Some(answer) = self.turn_end.account_answer(reading) {
            let to = self
                .turn_end
                .account_move()
                .map(|m| m.to.clone())
                .unwrap_or_default();
            match answer {
                AccountAnswer::Served => host.account_served(),
                AccountAnswer::Bad(bad) => host.account_answered(bad),
            }
            self.turn_end.account_told(answer);
            self.account_seen = None;
            self.roster_seen = None;
            self.ledger_row(
                RULE_ACCOUNT,
                approvals::Outcome::Waiting,
                &format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND),
                &approvals::account_answer_reason(&to, answer),
                seq,
            );
        }
        if let Some((at, asked, advice)) = &self.account_seen
            && *at == seq
            && self.turn_end.account_point(reading, &opts.policy) == Some(*asked)
        {
            reading.account.clone_from(advice);
        }
        if let Some((at, word)) = &self.roster_seen
            && *at == seq
        {
            reading.roster.clone_from(word);
        }
    }

    /// The host's advice about the session's account ([`IdleHost::account`]).
    pub(super) fn account_advise(
        &mut self,
        reading: &mut TurnEndReading,
        seq: u64,
        opts: &SuperviseOpts,
    ) {
        let Some(host) = opts.idle_host.as_ref() else {
            return;
        };
        if opts.policy.accounts == crate::harness::accounts::Mode::Off && !opts.policy.logins {
            return;
        }
        self.account_news(host.as_ref());
        if self.turn_end.reads_roster(reading) {
            let word = host.roster_word();
            self.roster_seen = Some((seq, word.clone()));
            reading.roster = word;
        }
        if reading.roster.is_some() || reading.login_superseded {
            reading.relaunch_off = host.relaunch_off();
        }
        let Some(point) = self.turn_end.account_point(reading, &opts.policy) else {
            return;
        };
        let advice = host.account(point);
        self.account_seen = Some((seq, point, advice.clone()));
        reading.account = advice;
    }

    /// A TURN READ RUNNING ([`TurnEndState::took`]).
    pub(super) fn took(
        &mut self,
        seq: u64,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) {
        self.hold_live(seq, cfg, review);
        if self.turn_end.wake_may_keep_landing()
            && self.stall_host.as_ref().and_then(|h| h.finished()) == Some(true)
        {
            self.turn_end.took_quiet();
            if let Some(host) = &self.stall_host
                && self.turn_end.wake_start_owed()
            {
                let found = host.repo_work();
                self.turn_end.wake_began_over(found);
            }
        } else {
            self.turn_end.took();
        }
        self.tell_ran(seq);
    }

    /// A BOX READ UP ([`TurnEndState::box_up`]).
    pub(super) fn box_up(&mut self, seq: u64) {
        self.turn_end.box_up();
        self.tell_ran(seq);
    }

    /// The ledger's record that the worker worked since the point recorded
    /// last, written once ([`TurnEndState::take_ran`]).
    fn tell_ran(&mut self, seq: u64) {
        if self.turn_end.take_ran() {
            self.ledger_row(
                approvals::RULE_TURN_RAN,
                approvals::Outcome::Waiting,
                "",
                approvals::TURN_RAN,
                seq,
            );
        }
    }

    /// The point folded in, then the host asked ([`Self::observe_point`], [`Self::account_advise`]).
    pub(super) fn fold_point(
        &mut self,
        r: &mut TurnEndReading,
        now: Instant,
        seq: u64,
        opts: &SuperviseOpts,
    ) {
        // A screen-alone agent's relaunch under a limit: its start-up is no
        // work, and the limit holds it only on a seat vouched for — the
        // host's own relaunch, or the account its screen names
        // ([`TurnEndState::relaunch_startup`]).
        let host = opts.idle_host.as_ref();
        self.turn_end
            .relaunch_startup(r, now, || host.is_some_and(|h| h.relaunch_owed()));
        let awaited = self.turn_end.account_answer_awaited(r);
        self.point_said = r.said_tail.as_deref().filter(|_| !r.said_cut).map(|tail| {
            (
                r.program,
                crate::harness::result::screen_said(r.turn_said.as_deref(), tail),
            )
        });
        self.observe_point(r, now, seq);
        let returned = self.turn_end.take_login_returned();
        if let Some(host) = opts.idle_host.as_ref() {
            if returned {
                host.login_back();
            } else if self.turn_end.limit_ended() {
                host.limit_over();
            }
        }
        if self.turn_end.take_login_retaken()
            && let Some(host) = opts.idle_host.as_ref()
        {
            let _ = host.login_carry();
        }
        if self.turn_end.take_point_moved() {
            let reason =
                approvals::point_reason(self.carried(), self.turn_end.persons_esc_stands());
            self.ledger_row(
                approvals::RULE_POINT,
                approvals::Outcome::Waiting,
                "",
                &reason,
                seq,
            );
        }
        self.tell_face();
        if awaited {
            self.account_tells(r, seq, opts);
        }
        self.account_advise(r, seq, opts);
    }

    /// THE ROWS THAT HOLD THE LAST MESSAGE WHOLE, for a point whose message
    /// is taller than the rows read ([`TurnEndReading::said_cut`]): the whole
    /// grid (`text --json`), and where what bounds the message is above it
    /// too (`reader`'s [`ScreenReader::said_whole`]), the history above the
    /// grid up to the row `reader` stops at
    /// ([`ScreenReader::said_history_stop`], [`Self::rows_up_to`]).
    fn said_read_whole(
        &mut self,
        screen: &Screen,
        reader: &'static dyn ScreenReader,
        turn: bool,
    ) -> Option<Vec<String>> {
        let read = |rows: &[String]| {
            if turn {
                reader.turn_whole(rows)
            } else {
                reader.said_whole(rows)
            }
        };
        let (grid, bounded) = if screen.first == 0 {
            (self.rows_as_read(screen), screen.rows.clone())
        } else {
            let whole = self.grid().ok()?;
            let bounded = self.message_bounded_by_cells(whole.clone(), Some(reader));
            let same = |rows: &[String]| {
                rows.get(screen.first..)
                    .is_some_and(|rows| super::wall_confirm::same_rows(rows, &screen.rows))
            };
            if !same(&whole.rows) && !same(&bounded.rows) {
                return None;
            }
            (whole.rows, bounded.rows)
        };
        if read(&bounded) {
            return Some(bounded);
        }
        let stop = |r: &str| {
            if turn {
                reader.turn_history_stop(r)
            } else {
                reader.said_history_stop(r)
            }
        };
        let (mut joined, first) = self.rows_up_to_at(&grid, &stop).ok().flatten()?;
        let abs = first.and_then(|f| f.checked_sub(u64::try_from(joined.len()).ok()?));
        joined.extend(grid);
        Some(self.rows_bounded_by_cells(joined, reader, abs))
    }

    /// WHAT THE SESSION'S REPOSITORY HOLDS THAT NO REMOTE MAIN HAS, at a
    /// point `reading` reads (`TurnEndReading::repo`): asked of the host
    /// (`IdleHost::repo_work`) only where the worker's words claim it is done
    /// ([`super::super::policy::turn_end::says_done`]: a done report, a
    /// `Nothing left:` defense, a retired token) or the record says the task
    /// finished — the points whose act names what it holds (Cruise's defend
    /// prompt, Land's nudge) and the only ones a land act's clean defense can
    /// land the task at, which it does only over a clean repository (the
    /// landing's fourth condition); nothing elsewhere.
    fn repo_read(
        &mut self,
        reading: &TurnEndReading,
        host: Option<&Arc<dyn IdleHost>>,
        worked: bool,
    ) -> Option<crate::harness::repo_work::RepoWork> {
        let tail = reading.said_tail.as_deref();
        if !(super::super::policy::turn_end::says_done(tail)
            || reading.finished_record == Some(true))
        {
            return None;
        }
        if !worked && self.turn_end.landing_words_read(tail) {
            return None;
        }
        let host = host?;
        let mark = said_mark(tail);
        let now = self.fx.mono();
        if let Some((_, _, found)) = self.repo_seen.as_ref().filter(|(m, at, _)| {
            !worked && *m == mark && now.saturating_duration_since(*at) < REPO_REPROBE
        }) {
            return found.clone();
        }
        let found = host.repo_work();
        self.repo_seen = Some((mark, now, found.clone()));
        found
    }

    /// What the loop holds that its next row marks for a loop that follows
    /// ([`approvals::Carried`]): the short streak and the retry ladder of an
    /// API error's wall it waits at, its appearance
    /// on the wall clock ([`TurnEndState::retry_track`]).
    fn carried(&self) -> approvals::Carried {
        approvals::Carried {
            short: self.turn_end.short_streak(),
            retry: self
                .turn_end
                .retry_track()
                .map(|(kind, attempts, ups, since)| approvals::RetryMark {
                    kind,
                    attempts,
                    ups,
                    since_ms: self.unix_ms_of(since),
                }),
        }
    }

    /// THE TASK'S FACE, told to the window's host where it moved since the loop last told it ([`TurnEndState::face_moved`]).
    pub(super) fn tell_face(&mut self) {
        if let Some(face) = self.turn_end.face_moved()
            && let Some(host) = &self.stall_host
        {
            host.task_face(face);
        }
        self.tell_hold();
    }

    /// THE TASK'S FACE AT THE LOOP'S START ([`TurnEndState::face_start`]), told to the window's host whatever it is.
    pub(super) fn tell_start_face(&mut self) {
        let face = self.turn_end.face_start();
        if let Some(host) = &self.stall_host {
            match self.turn_end.face_start_noted() {
                Some(noted_ms) => host.task_face_noted(face, noted_ms),
                None => host.task_face(face),
            }
        }
        self.tell_hold();
    }

    /// WHY LAND STILL HOLDS.
    pub(super) fn tell_hold(&mut self) {
        let here = self.hold_is_ours();
        if let Some(held) = self.turn_end.held_moved(here)
            && let Some(host) = &self.stall_host
        {
            host.land_held(held);
        }
    }

    /// THE HOLD IS THIS LOOP'S TO TELL (§12.3.5).
    pub(super) fn hold_is_ours(&self) -> bool {
        !self.needs_not_ours()
            && self
                .autopilot
                .word()
                .is_none_or(|word| word == Autopilot::Land)
    }

    /// THE LOOP ENDS.
    pub(super) fn tell_hold_ended(&mut self) {
        if self.turn_end.held_ended()
            && let Some(host) = &self.stall_host
        {
            host.land_held(None);
        }
    }

    /// The accounts' news, read off the host's stamp ([`IdleHost::account_stamp`]).
    pub(super) fn account_news(&mut self, host: &dyn IdleHost) -> bool {
        let stamp = host.account_stamp();
        if self
            .account_stamp
            .replace(stamp)
            .is_none_or(|read| read == stamp)
        {
            return false;
        }
        self.turn_end.account_news();
        true
    }

    /// Whether the composer holds a PERSON'S draft, judged where it is DRAWN.
    pub(in crate::supervise) fn composer_draft(
        &mut self,
        composer: Option<&(usize, Vec<String>)>,
        placeholder: bool,
        hint_fg: Option<&str>,
        screen: &Screen,
    ) -> Option<bool> {
        let Some((caret, lines)) = composer else {
            return Some(false);
        };
        if lines.iter().skip(1).any(|l| !l.is_empty()) {
            return Some(true);
        }
        if lines.first().is_none_or(String::is_empty) {
            return Some(false);
        }
        if !placeholder || screen.cursor_col != 2 {
            return Some(true);
        }
        let row = (screen.first + caret).to_string();
        match self.call(&["cell", &row, "2"]) {
            Ok(r) if r.ok() => crate::supervise::screen::cell_is_placeholder(&r.stdout, hint_fg)
                .map(|placeholder| !placeholder),
            _ => None,
        }
    }

    /// What the loop reads of the composer's cells on `screen`, read by
    /// `reading`: whether its text was typed ([`Self::composer_draft`]) and,
    /// over Claude Code's completion menu, its selection's colours
    /// ([`Self::menu_colours`]).
    fn composer_read(&mut self, reading: &aterm_phase::Reading, screen: &Screen) -> ComposerRead {
        let typed = self.composer_draft(
            reading.composer.as_ref(),
            reading.program.reader().placeholder(),
            reading.program.reader().placeholder_fg(),
            screen,
        );
        let menu_fg = match reading.program {
            Program::Claude if typed == Some(true) => self.menu_colours(screen),
            Program::Claude
            | Program::Codex
            | Program::Agy
            | Program::Muse
            | Program::Gemini
            | Program::Generic => None,
        };
        ComposerRead { typed, menu_fg }
    }

    /// A DRAFT THAT HELD THE POINT ([`Self::draft_held`]), read again at a look at the point already handed over.
    pub(super) fn draft_look(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let Some(held) = self.draft_held.clone() else {
            return Ok(false);
        };
        let reading = self
            .reader(&seen.screen.rows)
            .read(&seen.screen.rows, Some(seen.screen.cursor_col));
        let read = self.composer_read(&reading, &seen.screen);
        let now = TurnEndReading::of(&reading, &seen.screen.rows, read, None, None, None).composer;
        if now == held || now == Composer::Unknown {
            return Ok(false);
        }
        self.draft_held = None;
        if held != Composer::Unknown {
            self.close_box(seen.screen.seq, review)?;
        }
        self.turn_end_now(seen, opts, allow, review)?;
        Ok(true)
    }

    /// THE COMPLETION MENU'S SELECTION, where Claude Code draws it in colour
    /// only ([`aterm_phase::phase::CompletionMenu::colour_only`]: 2.1.289 and
    /// before draw no pointer, and a person's arrow, or a selection the vendor
    /// kept as the draft changed, leaves the rows as they were): the colour of
    /// each entry's first glyph, read with `cell` at the rows `screen` read, in
    /// entry order — `None` for a cell not answered, malformed, or holding
    /// another glyph than the entry's (the screen moved under the read).
    fn menu_colours(&mut self, screen: &Screen) -> Option<Vec<Option<String>>> {
        let menu = aterm_phase::phase::completion_menu(&screen.rows).filter(|m| m.colour_only())?;
        let mut fg = Vec::with_capacity(menu.entries.len());
        for e in &menu.entries {
            let row = (screen.first + e.row).to_string();
            let col = e.col.to_string();
            fg.push(
                match (self.call(&["cell", &row, &col]), e.word.chars().next()) {
                    (Ok(r), Some(glyph)) if r.ok() => {
                        crate::supervise::screen::cell_glyph_fg(&r.stdout, glyph)
                    }
                    _ => None,
                },
            );
        }
        Some(fg)
    }

    /// The decision at a new point.
    pub(super) fn turn_end_at_point(
        &mut self,
        point: &Turn,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> Result<TurnEndAction, Fail> {
        if !self.held {
            self.turn_end_due = None;
        }
        let rows = &point.screen.rows;
        let limit_dialog = point.phase == Phase::Prompt
            && self
                .reader(rows)
                .prompt(rows)
                .is_some_and(|p| p.kind == aterm_phase::PromptKind::UsageLimit);
        let stopped = || {
            aterm_phase::gemini::wall(rows).is_some_and(|w| aterm_phase::gemini::stopped_error(&w))
        };
        let program = self.reader(rows).program();
        if !limit_dialog && !stopped() {
            self.limit_dialog_read = None;
            // …unless the turn-end policy still waits on that limit (the
            // error read as it, a relaunch since: [`Self::limit_ladder_kept`]),
            // or the agent left the tab while the dialog was held, and this
            // is no agent's point or its relaunch's first
            // ([`Self::limit_retry_away`]).
            if !self.limit_ladder_kept() && !self.limit_retry_away(program, rows, opts) {
                self.limit_retry = None;
            }
        }
        self.draft_held = None;
        if self.claim.watching_behind().is_some() {
            return Ok(TurnEndAction::Nothing);
        }
        self.account_round();
        if worked.is_none()
            && self.turn_end.point_number().is_none()
            && self.launched_since_books(opts.idle_host.as_ref())
        {
            self.turn_end.launched_since_books();
        }
        let worked = worked.or_else(|| self.turn_end.take_gap().then_some(Duration::ZERO));
        let scoped_limit = self.turn_end.has_limit_track()
            && (opts.idle_host.is_some()
                || self
                    .turn_end
                    .limit_conversation()
                    .is_some_and(|c| !matches!(c, Conversation::Hostless)));
        let worked = worked.or_else(|| {
            (!scoped_limit)
                .then(|| self.unseen_act_work(&point.screen.rows))
                .flatten()
        });
        if !self.autopilot.read()
            || worked.is_some() && (self.turn_end.landed() || self.turn_end.landed_seed_armed())
        {
            self.autopilot.begin();
            self.autopilot_read(opts)?;
        }
        let mut r = self.turn_end_reading(&point.screen, worked, opts);
        self.host_held = r.upgrading || host_withholds(opts);
        let now = self.policy_now();
        self.fold_point(&mut r, now, point.screen.seq, opts);
        self.limit_episode_folded(point);
        self.task_seed(opts)?;
        self.decide_to_act(r, opts, now, &point.screen)
    }

    /// THE LIMIT EPISODE AFTER `point`'s FOLD ([`Self::turn_end_at_point`],
    /// [`Self::turn_end_now`]): where the point is neither Gemini CLI's
    /// usage-limit dialog nor the error its `Stop` leaves, and the fold
    /// ended the policy's wait on the limit (work past it), the dialog's
    /// ladder ends with it — save while the agent is away. A relaunch's
    /// start-up the policy took as the limit is the track's from here
    /// ([`Self::limit_retry_back`]).
    fn limit_episode_folded(&mut self, point: &Turn) {
        let rows = &point.screen.rows;
        let limit_dialog = point.phase == Phase::Prompt
            && self
                .reader(rows)
                .prompt(rows)
                .is_some_and(|p| p.kind == aterm_phase::PromptKind::UsageLimit);
        let stopped =
            aterm_phase::gemini::wall(rows).is_some_and(|w| aterm_phase::gemini::stopped_error(&w));
        if !limit_dialog && !stopped && !self.limit_ladder_kept() && !self.limit_retry_is_away() {
            self.limit_retry = None;
        }
        self.limit_retry_back();
    }

    /// THE WORK OF THE TURN THE ACT AWAITED BEGAN, no read since it was typed
    /// having seen that turn run ([`TurnEndState::untaken_act`]): the done
    /// row Claude Code drew at this point, where the turn it closes ended
    /// after the act and began at it ([`act_turn_work`]).
    fn unseen_act_work(&mut self, rows: &[String]) -> Option<Duration> {
        let at = self.turn_end.untaken_act_typed()?;
        match self.reader(rows).program() {
            Program::Claude => {}
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                return None;
            }
        }
        let work = done_row_work(rows)?;
        let now = self.now_unix();
        let offset = self.local_offset_s();
        let ended = limit::done_row_ended(rows, now, offset)?;
        act_turn_work(
            self.unix_of(at),
            ended,
            work,
            self.turn_asleep(rows).most,
            self.turn_end.timing.min_work,
        )
    }

    /// THE SLEEP OF THE MACHINE INSIDE THE TURN Claude Code's done row on `rows` closes.
    pub(super) fn turn_asleep(&mut self, rows: &[String]) -> InTurn {
        if self.sleeps.is_empty() && self.sleep_unknown.is_none() {
            return InTurn::default();
        }
        let Some(work) = done_row_work(rows) else {
            return InTurn::default();
        };
        let now_ms = self.now_unix_ms();
        let now = now_ms.div_euclid(1000);
        let offset = self.local_offset_s();
        let (lo, hi) = limit::done_row_ended(rows, now, offset)
            .map_or((now_ms, now_ms), |minute| {
                (minute * 1000, (minute * 1000 + 59_999).min(now_ms))
            });
        let span = i64::try_from(work.as_millis()).unwrap_or(i64::MAX);
        if self
            .sleep_look
            .as_ref()
            .is_some_and(|look| lo.saturating_sub(span) < look.at_ms)
            && let Some(why) = self.sleep_unknown.take()
        {
            self.sleep_unsaid = Some(why);
        }
        self.sleeps.in_turn(work, lo, hi)
    }

    /// THE ANSWER OF A LIMIT'S TRY NO LOOP READ, DATED at this loop's first
    /// decision read ([`TurnEndState::date_unread_try`], "a
    /// loop that follows another"), AT THE LATEST its sign allows: where the
    /// session's transcript ends on a notice written within
    /// [`TRY_ANSWER_SLACK`] of the try ([`TurnEndState::notice_written`]:
    /// this read's), by that second and the slack; where Claude Code's done
    /// row on `rows` says a turn of no more than that ended in a minute that
    /// reaches the try (its start no more than the slack past it), by that
    /// minute's end; both, the earlier.
    fn date_unread_try(&mut self, reading: &aterm_phase::Reading, rows: &[String], seq: u64) {
        let Some(at) = self.turn_end.unread_try() else {
            return;
        };
        let secs = |d: Duration| i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
        let tried = self.unix_of(at);
        let now = self.now_unix();
        let slack = secs(TRY_ANSWER_SLACK);
        let by_notice = self
            .turn_end
            .notice_written()
            .filter(|t| (tried - slack..=tried + slack).contains(t))
            .map(|t| t.saturating_add(slack));
        let claude = match reading.program {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        let by_done = if claude && done_row_work(rows).is_some_and(|w| w <= TRY_ANSWER_SLACK) {
            limit::done_row_ended(rows, now, self.local_offset_s())
                .filter(|&minute| minute <= tried + slack && minute + 59 >= tried)
                .map(|minute| minute + 59)
        } else {
            None
        };
        let latest = match (by_notice, by_done) {
            (Some(n), Some(d)) => Some(n.min(d)),
            (n, d) => n.or(d),
        };
        let take = secs(self.turn_end.timing.take_within);
        let answered = latest
            .filter(|&t| now >= t.saturating_add(take))
            .map(|t| Duration::from_secs(u64::try_from(t - tried).unwrap_or(0)));
        let wall = reading.wall.as_ref().map(|w| w.kind);
        if let Some(read) = self.turn_end.date_unread_try(wall, answered) {
            let conversation = self
                .wall_memory
                .as_ref()
                .map(|m| m.conversation.clone())
                .or_else(|| {
                    self.wall_conversation
                        .as_ref()
                        .filter(|(at, _)| *at == seq)
                        .map(|(_, c)| c.clone())
                })
                .unwrap_or(Conversation::Unknown);
            let program = self.wall_memory.as_ref().and_then(|m| m.program);
            self.turn_end.seed_limit_conversation(conversation, program);
            let read_ms = self.unix_ms_of(read);
            self.ledger_row(
                approvals::RULE_TRY_ANSWERED,
                approvals::Outcome::Waiting,
                "",
                &approvals::try_answered_reason(read_ms),
                seq,
            );
        }
    }

    /// [`decide_turn_end`] on `r`; where it says to type, the session's program and a person's
    /// last keystroke are read fresh ([`Self::foreground_program`]) and the point decided again,
    /// so a `status` is paid only where there is something to type.
    fn decide_to_act(
        &mut self,
        r: TurnEndReading,
        opts: &SuperviseOpts,
        now: Instant,
        judged: &Screen,
    ) -> Result<TurnEndAction, Fail> {
        self.fx.note(super::super::tape::core::NOTE_TURN_END, || {
            format!("{:?}\n{r:?}", self.turn_end)
        });
        let composer = r.composer.clone();
        let mut steps = self.fx.tracing().then(Vec::new);
        let shadow = self.fx.shadow_policy();
        let mut point = shadow.as_ref().map(|_| ShadowPoint::default());
        let (action, limit_draft_holds) =
            self.decide_to_act_now(r, opts, now, &mut steps, point.as_mut())?;
        if let (Some(policy), Some(point)) = (shadow, point) {
            let candidate = point.decide(&self.turn_end, policy, now);
            self.fx
                .shadowed(candidate.rule_id(), |at| turn_end_outcome(&candidate, at));
        }
        let tells = |a: &TurnEndAction| {
            matches!(a, TurnEndAction::Escalate { reason }
                if reason == CREDITS_HOLD_TOLD || reason == CREDITS_HOLD_MODEL_TOLD)
        };
        if tells(&action)
            || matches!(&action, TurnEndAction::Restart { otherwise, .. } if tells(otherwise))
        {
            self.turn_end.credits_told();
        }
        // A draft that holds the point with nothing due: what moves it is
        // the person's send or clear ([`Self::draft_look`]). So too a draft
        // that holds a usage limit's switch or a renewed login's move,
        // whatever the decision waits on: its point's review key leaves the
        // composer out, and the wait is the limit's reset, days off — the
        // review of the login carry, r7h-3: cleared, the draft left the tab
        // waiting under a notice whose step was done.
        let told_draft = limit_draft_holds
            || self
                .exception_now
                .as_ref()
                .and_then(|e| e.notice.as_ref())
                .is_some_and(|n| notice::DRAFT_HOLDS.contains(&n.id));
        self.draft_held = (draft_holds(&composer, &action) || told_draft).then_some(composer);
        if let Some(steps) = steps {
            self.fx.traced(steps);
        }
        self.fx.decide(
            super::super::tape::doors::DECIDE_TURN_END,
            judged,
            action.rule_id(),
            |at| turn_end_outcome(&action, at),
            None,
        );
        Ok(action)
    }

    /// [`Self::decide_to_act`]'s decision, each pass of it traced into
    /// `trace` where it is `Some` (a replay that asked for the point's trace).
    fn decide_to_act_now(
        &mut self,
        mut r: TurnEndReading,
        opts: &SuperviseOpts,
        now: Instant,
        trace: &mut Option<Vec<TraceStep>>,
        mut shadow: Option<&mut ShadowPoint>,
    ) -> Result<(TurnEndAction, bool), Fail> {
        self.decided_withheld = host_withholds(opts);
        if self.decided_withheld {
            if let Some(point) = shadow {
                point.withheld = true;
            }
            if let Some(steps) = trace.as_mut() {
                steps.push(TraceStep {
                    check: "host-withholds",
                    rule: None,
                    outcome: TraceOutcome::Decided(format!("{:?}", TurnEndAction::Nothing)),
                });
            }
            return Ok((TurnEndAction::Nothing, false));
        }
        self.autopilot.begin();
        r.autopilot = Autopilot::Cruise;
        let mut action = self.turn_end_decision(&r, &opts.policy, now, "pass:cruise", trace);
        let mut cfg = std::borrow::Cow::Borrowed(&opts.policy);
        if r.asked().is_some()
            && session_word_can_decide(&action)
            && let Some(policy) = self.prose_policy(opts)?
        {
            if let Some(point) = shadow.as_deref_mut() {
                point.prose = Some(policy.question_priority);
            }
            cfg = std::borrow::Cow::Owned(policy);
            action = self.turn_end_decision(&r, &cfg, now, "pass:prose-word", trace);
        }
        r.autopilot = if matches!(
            action,
            TurnEndAction::Nothing | TurnEndAction::WaitUntil { .. }
        ) {
            self.autopilot(opts)
        } else {
            self.autopilot_read(opts)?
        };
        if r.autopilot != Autopilot::Cruise {
            action = self.turn_end_decision(&r, &cfg, now, "pass:autopilot", trace);
        }
        if action.rule_id().is_some() {
            self.program = self.foreground_program()?;
            r.person = self.person_ago(now);
            action = self.turn_end_decision(&r, &cfg, now, "pass:final", trace);
        }
        // The exceptions engine reads the same policy and the same reading
        // the loop's final decision was made on — a lost login's notice says
        // what that decision does ([`exception_for`]); with the autopilot off
        // it raises nothing.
        let limit_draft_holds = (r.autopilot != Autopilot::Off)
            && crate::supervise::policy::exception::draft_holds_limit_switch(
                &self.turn_end,
                &r,
                &cfg,
            );
        self.exception_now = (r.autopilot != Autopilot::Off)
            .then(|| exception_for(&self.turn_end, &r, &cfg, now))
            .flatten();
        if let Some(point) = shadow {
            point.reading = Some(r);
        }
        Ok((action, limit_draft_holds))
    }

    /// [`decide_turn_end`] on `r` under `cfg` at `now` — and, where a replay
    /// asks for the point's RULE TRACE (`trace` is `Some`, as `Fx::tracing`
    /// says: `aterm drive replay --why` at a turn end, strictly replaying the
    /// tape to it), the same decision made with its trace kept
    /// ([`decide_turn_end_traced`]), its steps added to `trace` after a step
    /// naming this `pass` of the point; [`Self::decide_to_act`] hands them to
    /// the replay with the point's mark.
    fn turn_end_decision(
        &self,
        r: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
        pass: &'static str,
        trace: &mut Option<Vec<TraceStep>>,
    ) -> TurnEndAction {
        let Some(steps) = trace.as_mut() else {
            return decide_turn_end(&self.turn_end, r, cfg, now);
        };
        steps.push(TraceStep {
            check: pass,
            rule: None,
            outcome: TraceOutcome::Passed,
        });
        decide_turn_end_traced(&self.turn_end, r, cfg, now, steps)
    }

    /// The policy a question asked in prose is decided under when the
    /// session's own `questions` word is a priority word
    /// ([`Self::question_policy`], the dialog's one resolver): the table's,
    /// answering, by that word's priority.
    fn prose_policy(&mut self, opts: &SuperviseOpts) -> Result<Option<SupervisorConfig>, Fail> {
        let policy = self.question_policy(opts)?;
        Ok(match policy.answer {
            Some(word) if policy.by_session => {
                let mut cfg = opts.policy.clone();
                cfg.answer_questions = true;
                cfg.question_priority = word;
                Some(cfg)
            }
            _ => None,
        })
    }

    /// The policy's `WaitUntil` ran out on the point still showing.
    pub(super) fn turn_end_now(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.turn_end_due = None;
        self.end_told_wait(opts);
        if self.claim.watching_behind().is_some() {
            return Ok(());
        }
        if std::mem::take(&mut self.held) {
            self.rejudge = true;
            return Ok(());
        }
        let screen = self.screen()?;
        let mut turn = self.point_of(screen);
        if self.review_key(&turn) != self.review_key(seen) {
            return Ok(());
        }
        self.account_round();
        let mut r = self.turn_end_reading(&turn.screen, None, opts);
        self.point_as_decided(&mut turn);
        self.host_held = r.upgrading || host_withholds(opts);
        let now = self.policy_now();
        self.fold_point(&mut r, now, turn.screen.seq, opts);
        // The limit episode this fold may have ended, and the badge it
        // carried off its screen with it (B1 of the review of walls-3).
        self.limit_episode_folded(&turn);
        if self.limit_badge_off_ended() {
            self.close_box(turn.screen.seq, review)?;
        }
        self.task_seed(opts)?;
        self.probe_shut(&turn, ProbeGate::NotNew, None, review)?;
        let action = self.decide_to_act(r, opts, now, &turn.screen)?;
        if let TurnEndAction::Escalate { reason } = &action
            && self.escalates_here(&turn.phase)
        {
            self.escalate_point(&turn, Some(reason), review)?;
            if self.memory_wall {
                self.mark_memory_badge();
            }
        }
        self.turn_end_execute(&turn, action, opts, allow, review)
    }

    /// THE TURN THE CONVERSATION ENDS ON, by every key that names it: the
    /// host's transcript ([`crate::harness::upgrade::LastTurn::of`]), else
    /// the screen's last words `said`, hashed.
    fn last_turn(
        opts: &SuperviseOpts,
        said: Option<&str>,
    ) -> Option<crate::harness::upgrade::LastTurn> {
        opts.idle_host
            .as_ref()
            .and_then(|h| h.transcript_tail())
            .map(|tail| crate::harness::upgrade::LastTurn::of(&tail))
            .filter(|t| t.key().is_some())
            .or_else(|| {
                said.map(|said| crate::harness::upgrade::LastTurn {
                    last: Some(Self::said_key(said)),
                    ..crate::harness::upgrade::LastTurn::default()
                })
            })
    }

    /// The key of a turn no transcript names: the screen's last words
    /// `said`, hashed ([`Self::last_turn`]).
    fn said_key(said: &str) -> String {
        format!("said:{:016x}", aterm_hash::fnv1a_64(said.as_bytes()))
    }

    /// A PERSON'S OWN EXCHANGE LEFT TO THEM at `point`, made durable (the
    /// review's RR3-TE-2): a `skipped` row under [`RULE_LEFT`] carrying the
    /// key of the turn it left ([`Self::last_turn`]), once per turn.
    fn ledger_left(&mut self, screen: &Screen, opts: &SuperviseOpts, seq: u64) {
        let said = self
            .reader(&screen.rows)
            .read(&screen.rows, Some(screen.cursor_col))
            .said_tail;
        let Some(key) = Self::last_turn(opts, said.as_deref()).and_then(|turn| turn.key()) else {
            return;
        };
        if self.left_keys.contains(&key) {
            return;
        }
        self.ledger_row(
            RULE_LEFT,
            approvals::Outcome::Skipped,
            &key,
            approvals::LEFT_REASON,
            seq,
        );
        self.left_keys.push(key);
    }

    /// A PERSON'S OWN EXCHANGE AT A WALL, made durable (the review's
    /// RR4-TE-2, [`TurnEndState::exchange_at_wall`]): a `skipped` row under
    /// [`RULE_LEFT`] keyed by the turn the wall answered — the person's
    /// prompt (`crate::harness::upgrade::last_turn_key` passes over the
    /// wall's own error row) — marked [`approvals::LEFT_AT_WALL`], once per
    /// standing row: the retry typed keeps it open as the retry's
    /// ([`approvals::left_after_typed`]), and the retry the wall answers
    /// again writes the next.
    fn ledger_left_at_wall(&mut self, screen: &Screen, opts: &SuperviseOpts, seq: u64) {
        if self.left_at_wall_seq.replace(seq) == Some(seq) {
            return;
        }
        let said = self
            .reader(&screen.rows)
            .read(&screen.rows, Some(screen.cursor_col))
            .said_tail;
        let Some(key) = Self::last_turn(opts, said.as_deref()).and_then(|turn| turn.key()) else {
            return;
        };
        let key = approvals::left_at_wall_key(&key, self.turn_end.exchange_work());
        if self.left_keys.contains(&key) {
            return;
        }
        self.ledger_row(
            RULE_LEFT,
            approvals::Outcome::Skipped,
            &key,
            approvals::LEFT_AT_WALL_REASON,
            seq,
        );
        self.left_keys.push(key);
    }

    /// Whether the policy's escalation at a point of `phase` is the point's own ([`Self::escalate_point`]).
    pub(super) fn escalates_here(&self, phase: &Phase) -> bool {
        match phase {
            Phase::Idle | Phase::Question => true,
            Phase::Limited { .. } => self.memory_wall,
            _ => false,
        }
    }

    /// THE TASK'S EDGE AT A POINT, said once a point.
    pub(super) fn task_edge(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.task_edge_with(screen, opts, review, true)
    }

    /// [`Self::task_edge`] at the point's fold, before the point's own line is journaled.
    pub(super) fn task_edge_at_fold(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.task_edge_with(screen, opts, review, false)
    }

    /// THE TASK'S END NOTE OWED ([`Self::task_note`]), written now, with the time the end it restates was first noted.
    pub(super) fn say_task_note(&mut self, review: &mut dyn Review) {
        if let Some((note, ended)) = self.task_note.take() {
            review.note_ended(&note, ended);
        }
    }

    /// A NEW POINT'S OWN LINE, journaled with the mark of the worker's last words there ([`said_mark`]; a box says none).
    pub(super) fn say_point(
        &mut self,
        point: &Turn,
        review: &mut dyn Review,
    ) -> Result<bool, String> {
        let mark = (point.phase != Phase::Prompt)
            .then(|| {
                let rows = &point.screen.rows;
                said_mark(self.reader(rows).said_tail(rows).as_deref())
            })
            .flatten();
        let go_on = review.review_marked(point, mark);
        self.say_task_note(review);
        go_on
    }

    /// A STEP BETWEEN A POINT'S FOLD AND ITS OWN LINE (`step`: the fold's own publish, a limit's episode closed, the point's badge raised).
    pub(super) fn said_before_fail<T>(
        &mut self,
        point: &Turn,
        step: Result<T, Fail>,
        review: &mut dyn Review,
    ) -> Result<T, Fail> {
        if step.is_err() && self.task_note.is_some() {
            let _ = self.say_point(point, review);
        }
        step
    }

    /// [`Self::task_edge`]; `say`: the note is written now (and any owed
    /// before it), else owed.
    fn task_edge_with(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
        say: bool,
    ) -> Result<(), Fail> {
        let seq = screen.seq;
        let done = self.turn_end.task_done();
        let restates_wake = self.turn_end.take_wake_restated() && done && self.task_done_said;
        if done && (!self.task_done_said || restates_wake) {
            let note = if self.turn_end.left_to_person() {
                let note = format!(
                    "LEFT seq={seq} rule={RULE_LEFT} a person's own exchange (their message, \
                     answered within 10 min): nothing is typed until the next turn ends"
                );
                self.ledger_left(screen, opts, seq);
                note
            } else if self.turn_end.landed_by_wake() {
                format!(
                    "DONE seq={seq} rule={RULE_LAND} {WAKE_KEPT} (autopilot land): nothing \
                     more is typed until a person or an orchestrator types"
                )
            } else {
                format!(
                    "DONE seq={seq} rule={RULE_LAND} the worker defended that everything is \
                     landed on the remote main branch (autopilot land): nothing more is typed \
                     until a person or an orchestrator types"
                )
            };
            let ended = if self.turn_end.left_to_person() {
                None
            } else if restates_wake {
                self.landed_noted_ms
                    .or_else(|| self.turn_end.restated_noted_ms())
            } else {
                self.turn_end.restated_noted_ms()
            };
            if !self.turn_end.left_to_person() {
                self.landed_noted_ms = Some(ended.unwrap_or_else(|| self.now_unix_ms()));
            }
            self.task_note = Some((note, ended));
            if !self.turn_end.left_to_person() && !restates_wake {
                self.ledger_row(
                    approvals::RULE_TASK_DONE,
                    approvals::Outcome::Waiting,
                    "",
                    approvals::TASK_DONE,
                    seq,
                );
            }
        } else if self.turn_end.left_judged() {
            // ([`TurnEndState::left_judged`]).
            self.ledger_left(screen, opts, seq);
        }
        if say {
            self.say_task_note(review);
        }
        self.sync_needs(screen, opts, review)?;
        let was_done = std::mem::replace(&mut self.task_done_said, done);
        self.task_at_point(seq, was_done, opts, review)
    }

    /// Clear the in-hand Autopilot Land episode after it ends or is superseded.
    pub(super) fn land_over(&mut self, had: bool, seq: u64) {
        if had && !self.turn_end.land_episode().0 {
            self.land_report = None;
            self.ledger_row(
                approvals::RULE_LAND_OVER,
                approvals::Outcome::Waiting,
                "",
                approvals::LAND_OVER,
                seq,
            );
        }
    }

    /// Carry out what the policy decided at `point` (module docs).
    pub(super) fn turn_end_execute(
        &mut self,
        point: &Turn,
        action: TurnEndAction,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        self.nothing_decided = matches!(action, TurnEndAction::Nothing);
        let withheld = std::mem::take(&mut self.decided_withheld);
        self.say_wall_refused(review);
        self.say_sleep_unknown(seq, review);
        self.sync_exception(seq, review);
        self.task_edge(&point.screen, opts, review)?;
        if self.turn_end.exchange_at_wall() {
            self.ledger_left_at_wall(&point.screen, opts, seq);
        }
        if !matches!(action, TurnEndAction::Restart { .. }) {
            self.restart_waits = 0;
            self.restart_run = None;
        }
        if action.rule_id().is_some()
            && let Some(at) = self.settling()
        {
            self.settle_said(seq, at, review);
            self.set_due(at, WOKE);
            return Ok(());
        }
        let hold = matches!(action, TurnEndAction::Nothing).then(|| {
            super::HoldSaid::Point(if withheld {
                "withheld"
            } else if !review.unattended() {
                "attended"
            } else {
                "policy"
            })
        });
        self.say_hold(seq, hold, review);
        let acts = !matches!(
            action,
            TurnEndAction::Nothing | TurnEndAction::WaitUntil { .. }
        );
        self.hold_point(&point.screen, acts, Some(&opts.policy), review);
        let (text, rule) = match &action {
            TurnEndAction::Nothing => return Ok(()),
            TurnEndAction::Escalate { .. } => {
                let now = self.fx.mono();
                if let Some(at) = budget_frees(&self.turn_end, &opts.policy, &action, now) {
                    self.set_due(at, BUDGET_FREES);
                }
                return Ok(());
            }
            TurnEndAction::Restart {
                why,
                rule_id,
                otherwise,
            } => {
                return self.restart_agent(
                    point,
                    why,
                    rule_id,
                    otherwise.as_ref().clone(),
                    opts,
                    allow,
                    review,
                );
            }
            TurnEndAction::WaitUntil { until, why, wait } => {
                let said = self.turn_end_due.as_ref().is_some_and(|d| d.at == *until);
                if !said {
                    review.note(&format!(
                        "WAITING seq={seq} until={} {why}",
                        self.stamp_in(*until)
                    ));
                }
                self.set_wait(*until, why.clone(), *wait);
                return Ok(());
            }
            TurnEndAction::Type { text, rule_id } | TurnEndAction::Accept { text, rule_id } => {
                (one_turn(text), *rule_id)
            }
            TurnEndAction::TypeCommand {
                command, rule_id, ..
            } => (command.clone(), *rule_id),
            TurnEndAction::Submit { rule_id } => (
                self.reader(&point.screen.rows)
                    .composer(&point.screen.rows)
                    .map(|(_, lines)| lines.join(" "))
                    .unwrap_or_default(),
                *rule_id,
            ),
            TurnEndAction::Confirm { rule_id, .. } => (String::new(), *rule_id),
        };
        if !self.speaks_to_its_agent(seq, rule, review) {
            return Ok(());
        }
        if self.stall_in_hand(review) {
            return Ok(());
        }
        // The same `status` says the worker has not read what was written to
        // it yet ([`Self::input_unread`]): nothing goes behind it.
        if self.input_unread(seq, rule, review) {
            return Ok(());
        }
        if let TurnEndAction::Confirm { rule_id, row } = &action {
            return self.confirm_dialog(point, &action, rule_id, row, opts, review);
        }
        if matches!(action, TurnEndAction::Submit { .. }) && self.probe_left_stands(&point.screen) {
            return self.probe_left_enter(point, review);
        }
        if rule == RULE_LOGIN
            && let Some(host) = opts.idle_host.as_ref()
            && let Err(by) = host.login_carry()
        {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the login's sign-in is {by}'s"
            ));
            let at = self.fx.mono() + LOGIN_SHARED_LOOK;
            self.set_due(at, format!("the login's sign-in is {by}'s"));
            return Ok(());
        }
        let typed = match &action {
            TurnEndAction::Accept { .. } => self.accept_suggestion(point, &text, rule, review),
            TurnEndAction::Submit { .. } => self.submit_draft(point, &text, rule, review),
            _ => self.type_text(point, &text, rule, review),
        };
        if rule == RULE_LOGIN
            && !matches!(&typed, Ok(true))
            && let Some(host) = opts.idle_host.as_ref()
        {
            host.login_abandon();
        }
        let typed = typed?;
        if !typed {
            return Ok(());
        }
        if rule == RULE_LIMIT_RESUME {
            self.limit_resume_typed(&point.screen.rows);
        }
        let r = self.turn_end_reread(&point.screen, opts);
        let land = self.turn_end.land_episode().0;
        self.turn_end.acted(&action, &r, self.fx.mono());
        if !land && self.turn_end.land_episode().0 {
            self.land_report = self.point_said.clone();
        }
        self.land_over(land, seq);
        let word = if matches!(action, TurnEndAction::TypeCommand { .. }) {
            "TYPED"
        } else {
            "CONTINUED"
        };
        review.say(&format!("{word} seq={seq} rule={rule} {}", clip(&text)))?;
        let reason = approvals::act_reason_after(self.carried(), self.turn_end.limit_try());
        self.ledger_row(rule, approvals::Outcome::Typed, &text, &reason, seq);
        Ok(())
    }

    /// THE POINT ONLY A PERSON CAN ANSWER, said once per change
    /// ([`exception_for`]): `EXCEPTION seq=<n> kind=<word> when=<later|now>
    /// [notice=<id>] <why> => <action>` to the journal and the host's
    /// [`IdleHost::exception`] when one appears or its words change, and, when
    /// the point no longer holds it, its end ([`Self::exception_ended`]).
    fn sync_exception(&mut self, seq: u64, review: &mut dyn Review) {
        self.stuck_over(seq, "the screen reached a point", review);
        let Some(now) = self.exception_now.clone() else {
            self.exception_ended(seq, None, review);
            return;
        };
        match &self.exception_open {
            Some((open, _)) if *open == now => return,
            Some((open, _)) if open.kind != now.kind => {
                self.exception_ended(seq, Some("another takes its place"), review);
            }
            _ => {}
        }
        let since = self
            .exception_open
            .as_ref()
            .map_or_else(|| self.fx.mono(), |(_, t)| *t);
        let notice = now
            .notice
            .as_ref()
            .map_or_else(String::new, |n| format!(" notice={}", n.id));
        review.note(&format!(
            "EXCEPTION seq={seq} kind={} when={}{notice} {} => {}",
            now.kind.word(),
            now.when.word(),
            now.why,
            now.action
        ));
        if let Some(host) = &self.stall_host {
            host.exception(Some(&now));
        }
        self.exception_open = Some((now, since));
    }

    /// The exception told and standing is over.
    pub(super) fn exception_ended(
        &mut self,
        seq: u64,
        cause: Option<&str>,
        review: &mut dyn Review,
    ) {
        self.exception_now = None;
        let Some((open, since)) = self.exception_open.take() else {
            return;
        };
        review.note(&format!(
            "CLEARED seq={seq} exception={} after={}s {}",
            open.kind.word(),
            self.fx.since(since).as_secs(),
            cause.unwrap_or(open.clears)
        ));
        if let Some(host) = &self.stall_host {
            host.exception(None);
        }
    }

    /// The restart the policy asked for, made by the host and journaled `HOST seq=<n> restart:<why> step=<word>`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn restart_agent(
        &mut self,
        point: &Turn,
        why: &Restart,
        rule: &'static str,
        otherwise: TurnEndAction,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        if self.stall_in_hand(review) {
            return Ok(());
        }
        let measured = self.memory_seen;
        let over_wall = self.memory_over_wall && matches!(why, Restart::Memory);
        let took = self.memory_wall && matches!(why, Restart::Memory);
        if let (Restart::Memory, Some(over)) = (why, measured) {
            self.memory_warn(&point.screen, over, opts, review);
        }
        let asked_unix = match self.restart_run {
            Some((kind, at)) if kind == why.word() => at,
            _ => {
                let at = self.now_unix();
                self.restart_waits = 0;
                self.restart_run = Some((why.word(), at));
                at
            }
        };
        let step = opts.idle_host.as_ref().and_then(|h| h.restart(why));
        if let Some(step) = &step
            && restart_note_due(self.restart_waits)
        {
            review.note(&format!(
                "HOST seq={seq} restart:{} step={step}",
                why.word()
            ));
        }
        let word = step
            .as_deref()
            .map(|s| s.split_once(':').map_or(s, |(head, _)| head));
        match (word, step.as_deref()) {
            (Some("adopted" | "relaunched" | "continued" | "done"), _) => {
                self.restart_waits = 0;
                self.restart_run = None;
                self.memory_refused = false;
                let r = if matches!(why, Restart::Account { .. }) {
                    self.turn_end_reread_relaunched(&point.screen, opts)
                } else {
                    self.turn_end_reread(&point.screen, opts)
                };
                let now = self.fx.mono();
                self.turn_end.restarted(why, true, &r, now);
                if word == Some("adopted") && point.screen.submits.is_some() {
                    let after = self.read_once().ok().map(|(screen, _)| screen);
                    let at = after.as_ref().and_then(|s| self.counted_conversation(s));
                    self.turn_end.counted_after_harness(at);
                }
                if matches!(why, Restart::Model { .. }) {
                    self.turn_end.fallback_made_at(asked_unix);
                    if let Some(kept) = &mut self.credits_kept {
                        kept.model = None;
                    }
                }
                let unasked = self.turn_end.fallback_unasked();
                if word == Some("continued") {
                    let kind = step
                        .as_deref()
                        .and_then(crate::harness::upgrade_drive::host_turn)
                        .unwrap_or(super::super::policy::turn_end::HostTurn::CarryOn);
                    self.turn_end.host_typed(now, kind);
                }
                let what = match why {
                    Restart::Memory => match measured {
                        Some(over) => format!("the agent, for its memory: {}", over.fact()),
                        None => "the agent, for its critical-memory banner".to_string(),
                    },
                    Restart::Model { to } => format!("relaunch --model {to}"),
                    Restart::ModelBack { to } => {
                        format!(
                            "relaunch --model {}",
                            to.as_deref().unwrap_or("(its launch's)")
                        )
                    }
                    Restart::Account { to, .. } => {
                        format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND)
                    }
                };
                if matches!(why, Restart::Account { .. }) {
                    self.forget_wall(seq);
                    self.wall_reset = None;
                    self.wall_tap = None;
                    self.reprint_jumps = 0;
                    self.account_seen = None;
                    self.roster_seen = None;
                    self.retry = None;
                    self.close_episode(
                        seq,
                        "relaunched under another account, off the limited one",
                        review,
                    )?;
                }
                let switch = self
                    .turn_end
                    .model_switch()
                    .filter(|_| matches!(why, Restart::Model { .. }))
                    .cloned();
                let back = match (&switch, why) {
                    (Some(m), _) => format!(
                        " (from {}, {}; session-only)",
                        m.from.as_deref().unwrap_or("the launch's model"),
                        m.back_at.map_or_else(
                            || "no return scheduled: the notice names no reset".to_string(),
                            |at| format!("back at {}", self.stamp_in(at))
                        )
                    ),
                    (
                        None,
                        Restart::Account {
                            at: AccountAt::Wall,
                            ..
                        },
                    ) => " (from the limit its turn ended on; the same conversation)".to_string(),
                    (
                        None,
                        Restart::Account {
                            at: AccountAt::Ahead,
                            ..
                        },
                    ) => " (at a turn end; the same conversation)".to_string(),
                    (None, _) => String::new(),
                };
                review.say(&format!("RESTARTED seq={seq} rule={rule} {what}{back}"))?;
                if let (Restart::Memory, Some(over)) = (why, measured) {
                    self.memory_say(&point.screen, over, true, opts, review);
                }
                if !matches!(why, Restart::Memory) {
                    let reason = match (&switch, why) {
                        (Some(m), _) => approvals::model_switch_reason(
                            m.from.as_deref(),
                            m.back_at.map(|at| self.unix_of(at)),
                            asked_unix,
                        ),
                        (None, Restart::Account { to, at }) => {
                            approvals::account_switch_reason(to, *at)
                        }
                        (None, _) => "the turn-end policy".to_string(),
                    };
                    self.ledger_row(rule, approvals::Outcome::Typed, &what, &reason, seq);
                }
                self.fallback_asked(unasked, seq);
                Ok(())
            }
            (Some("wait"), Some(step)) | (Some("busy"), Some(step))
                if matches!(
                    why,
                    Restart::ModelBack { .. }
                        | Restart::Account {
                            at: AccountAt::Ahead,
                            ..
                        }
                ) && (word == Some("wait") || step == "busy:another-sweep") =>
            {
                review.note(&format!(
                    "DEFERRED seq={seq} restart:{} waits for a quiet moment ({step}); the point goes on",
                    why.word()
                ));
                self.turn_end_execute(point, otherwise, opts, allow, review)
            }
            (Some("wait"), Some(step)) => {
                // ([`approvals::relaunch_waits_reason`]).
                if self.restart_waits == 0 && matches!(why, Restart::Model { .. }) {
                    self.ledger_row(
                        rule,
                        approvals::Outcome::Waiting,
                        "",
                        &approvals::relaunch_waits_reason(asked_unix),
                        seq,
                    );
                }
                let r = self.turn_end_reread(&point.screen, opts);
                let due = r
                    .reset_at
                    .map(|at| at + self.turn_end.timing.grace_for(r.program) + r.jitter);
                self.restart_later(seq, why, step, due, review)
            }
            (Some("busy"), Some(step)) if step == "busy:another-sweep" => {
                self.restart_later(seq, why, step, None, review)
            }
            _ => {
                self.restart_waits = 0;
                self.restart_run = None;
                let r = self.turn_end_reread(&point.screen, opts);
                self.memory_over_wall |= over_wall;
                self.turn_end
                    .restart_unmade(why, step.as_deref(), &r, self.fx.mono());
                if let Restart::Account { to, at } = why {
                    self.ledger_row(
                        rule,
                        approvals::Outcome::Skipped,
                        &format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND),
                        &approvals::account_unmade_reason(to, *at, step.as_deref()),
                        seq,
                    );
                }
                if matches!(why, Restart::Memory) {
                    self.memory_refused = true;
                    if let Some(over) = self.memory_measure(opts) {
                        self.memory_warn(&point.screen, over, opts, review);
                    }
                }
                match otherwise {
                    TurnEndAction::Escalate { reason } => {
                        let reason = match &step {
                            Some(step) => {
                                format!("the restart could not be made ({step}): {reason}")
                            }
                            None => reason,
                        };
                        self.escalate_point(point, Some(&reason), review)?;
                        if took {
                            self.mark_memory_badge();
                        }
                        Ok(())
                    }
                    other => self.turn_end_execute(point, other, opts, allow, review),
                }
            }
        }
    }

    /// The Mac woke, as the sleep watch measured it at `at` (monotonic) and `wall_now` (unix seconds) ([`Self::watch_sleep`]).
    pub(super) fn settle_wake(&mut self, wall_now: i64, at: Instant) {
        let share = self.setup.sid.as_deref().map_or(Duration::ZERO, |sid| {
            resume_jitter(sid, wall_now.div_euclid(60), WAKE_SPREAD)
        });
        let at = at + WAKE_SETTLE + share;
        let settle = Due {
            at,
            unix_ms: self.unix_ms_of(at),
            why: WOKE.to_string(),
            wait: WaitKind::Other,
        };
        self.wake_settle = Some((settle, false));
    }

    /// The wake the watch measured at `wall_now` (unix seconds), at a step of the wait on `seq` ([`Self::check_slept`]).
    pub(super) fn woke(&mut self, seq: u64, wall_now: i64, review: &mut dyn Review) {
        let carried = self
            .turn_end_due
            .as_ref()
            .is_some_and(|d| d.unix_ms <= wall_now.saturating_mul(1000));
        if !carried {
            return;
        }
        if let Some(at) = self.settling() {
            self.settle_said(seq, at, review);
            self.set_due(at, WOKE);
        }
    }

    /// `WAITING seq=<seq> until=<the settle> the Mac woke: deciding again`,
    /// once a settle ([`Self::settle_wake`]): said by whichever comes first,
    /// a due the sleep carried past or an act held inside it.
    fn settle_said(&mut self, seq: u64, at: Instant, review: &mut dyn Review) {
        if let Some((_, said)) = &mut self.wake_settle
            && !*said
        {
            *said = true;
            review.note(&format!(
                "WAITING seq={seq} until={} {WOKE}: deciding again",
                self.stamp_in(at)
            ));
        }
    }

    /// A restart that must wait: decided again after the next pause of
    /// [`TurnEndTiming::restart_backoff`], `WAITING … until=<t>
    /// restart:<why> waits: <step>`.
    fn restart_later(
        &mut self,
        seq: u64,
        why: &Restart,
        step: &str,
        cut_at: Option<Instant>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let pause = crate::supervise::ladder::Ladder(&self.turn_end.timing.restart_backoff)
            .step(usize::try_from(self.restart_waits).unwrap_or(usize::MAX));
        self.restart_waits = self.restart_waits.saturating_add(1);
        let now = self.fx.mono();
        let mut until = now + pause;
        if let Some(at) = cut_at.filter(|&at| at > now) {
            until = until.min(at);
        }
        let why = format!("restart:{} waits: {step}", why.word());
        if restart_note_due(self.restart_waits.saturating_sub(1)) {
            review.note(&format!(
                "WAITING seq={seq} until={} {why}",
                self.stamp_in(until)
            ));
        }
        self.set_due(until, why);
        Ok(())
    }

    /// Whether the session's foreground program, read fresh for this decision
    /// ([`Self::turn_end_at_point`], [`Self::turn_end_now`]), is an agent this
    /// policy types into: one whose reader is supervisable
    /// ([`aterm_phase::Program::supervisable`]: Claude Code, agy, Muse Code,
    /// Gemini CLI by its screen under `node`), never a shell, a Codex (whose
    /// screen aterm reads no more) or a program nothing reads.
    fn speaks_to_its_agent(&mut self, seq: u64, rule: &str, review: &mut dyn Review) -> bool {
        let rows = self
            .last
            .as_ref()
            .map(|s| s.rows.clone())
            .unwrap_or_default();
        if self.reader(&rows).program().supervisable() {
            return true;
        }
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} the session runs {}, no agent this policy types into",
            self.program.as_deref().unwrap_or("-")
        ));
        false
    }

    /// `text` typed and submitted under the guarded submit (module header):
    /// `true` when the server says it submitted it (`submitted=1`), or wrote
    /// the Enter unverified (`pressed=<n>`, journaled so).
    fn type_guarded(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        if self.caps.guarded_submit == Some(false) {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} this aterm is too old for a guarded submit"
            ));
            return Ok(false);
        }
        let reader = self.reader(&point.screen.rows);
        let Some(caret) = reader.caret_on(&point.screen.rows) else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} no composer this policy types into"
            ));
            return Ok(false);
        };
        let drawn = reader.folded_paste(text);
        let submit = format!(
            "submit=guarded:{}",
            composer_guard_at(
                caret,
                reader.caret_column(),
                drawn.as_deref().unwrap_or(text)
            )
        );
        let r = self.call(&["turn", &submit, TURN_IDLE, TURN_TIMEOUT, TURN_YIELD, text])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!(
                "turn (the {rule} text) failed: {}",
                r.stderr.trim()
            )));
        }
        let why = one_line(r.err_text());
        if r.is_err("halted") || r.is_err("busy") || r.is_err("rate") {
            self.write_refused(r.is_err("halted"), &why, seq, review)?;
            return Ok(false);
        }
        if r.is_err("yield") {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a person is typing: {why}"
            ));
            self.redecide_soon("a person was typing");
            return Ok(false);
        }
        if r.unknown_form() {
            self.caps.guarded_submit = Some(false);
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} this aterm is too old for a guarded submit: {why}"
            ));
            return Ok(false);
        }
        self.caps.guarded_submit = Some(true);
        if r.submitted() {
            return Ok(true);
        }
        let verdict = r.turn_verdict().unwrap_or("").to_string();
        if r.pressed_unverified() {
            review.note(&format!(
                "UNVERIFIED seq={seq} rule={rule} Enter written, the submit not seen: {verdict}"
            ));
            return Ok(true);
        }
        let verdict = if verdict.is_empty() { why } else { verdict };
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} not submitted: {verdict}"
        ));
        if r.skipped() || verdict.contains("reason=guard") {
            self.left_typed(
                point,
                text,
                &format!("the guard missed ({verdict})"),
                review,
            )?;
        }
        Ok(false)
    }

    /// A fenced write the server skipped on the unchanged screen (a bare `OK
    /// skipped`: the generation held and the guard matched no row): nothing
    /// written, and a retry would miss the same way, so it is journaled
    /// `SKIPPED … the guard matched no row …` and escalated.
    fn not_written(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        guard: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} the guard matched no row of the judged screen \
             ({guard}); nothing written"
        ));
        let reason = format!(
            "not typed: aterm could not find the prompt line: `{}`",
            clip(text)
        );
        let attention = escalate::attention_text(self.reader(&point.screen.rows), point, &reason);
        self.escalate(
            seq,
            &attention,
            None,
            super::escalate::Escalation::Ask,
            review,
        )?;
        self.turn_end_badge = true;
        Ok(())
    }

    /// The point decided again a fenced write's skip later
    /// ([`crate::harness::Timings::redecide_after_skip`]: the screen moved
    /// between the judged read and the write — a person's keystroke, a box)
    /// from now ([`Self::turn_end_now`], on the deadline of the loop's own
    /// wait).
    fn redecide_soon(&mut self, why: &str) {
        self.set_due(self.fx.mono() + self.setup.redecide_after_skip, why);
    }

    /// INPUT THE WORKER HAS NOT READ STANDS BEFORE ANY ACT: the `status` that
    /// named the program for this decision ([`Self::foreground_program`])
    /// says `input=pending|typeahead`, bytes queued to the program that its
    /// screen does not show yet. Nothing is written behind them: journaled
    /// `SKIPPED`, and the point decided again shortly
    /// ([`Self::redecide_soon`]) on the screen the worker draws once it has
    /// read them (the launch's own rule, [`LaunchCall::Again`]). A host that
    /// does not measure the queue (`input=-`, no field) says nothing here.
    ///
    /// READ IS NOT DRAWN: the queue reads `clear` the moment the worker's
    /// `read(2)` drains it, and the worker paints what it read later (seconds
    /// later, starved). The screen at the content seq the queue was last seen
    /// unread at is still the one the input has not reached, its composer
    /// empty, and the send's fences pass on it (`if-gen=` is that seq, `if=`
    /// matches the empty composer). So the hold lasts until the screen moves
    /// past that seq, and the point is decided on what the worker drew; or,
    /// a worker that read its input and drew nothing, until
    /// [`UNDRAWN_INPUT_HANG`] after the queue was last seen unread, a hang
    /// detector, after which the screen as it stands is decided on.
    ///
    /// Measured 2026-10-22 at a load of 175-377 (the worker at nice 20 beside
    /// 28 CPU hogs): a loop wrote its continuation at a wall's reset and was
    /// replaced (an update, a fault) before the starved worker read it; the
    /// next loop read the screen the write had not reached yet, its composer
    /// still empty, and typed the continuation again behind the first
    /// (`input=pending input_bytes=293` in the very `status` it read). The
    /// worker took both as one prompt, and that was submitted, or the
    /// read-back saw neither as written and the session was left with the
    /// text typed twice.
    ///
    /// [`LaunchCall::Again`]: super::approval_loop::LaunchCall::Again
    fn input_unread(&mut self, seq: u64, rule: &str, review: &mut dyn Review) -> bool {
        let now = self.fx.mono();
        let unread = self
            .launch_status
            .input
            .clone()
            .filter(|w| matches!(w.as_str(), "pending" | "typeahead"));
        let why = if let Some(word) = unread {
            self.input_unread_at = Some((seq, now));
            format!("the worker has not read the input written to it (input={word})")
        } else {
            match self.input_unread_at {
                Some((at, since))
                    if at == seq && now.saturating_duration_since(since) < UNDRAWN_INPUT_HANG =>
                {
                    "the worker has not drawn the input it read (the screen has not moved \
                     since it was unread)"
                        .to_string()
                }
                _ => {
                    self.input_unread_at = None;
                    return false;
                }
            }
        };
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} {why}; nothing written"
        ));
        self.redecide_soon("the worker had not drawn the input written to it");
        true
    }

    /// A write of the policy's act the server refused for now: a hold (`ERR halted`) is parked on,
    /// `ERR busy …` and `ERR rate` backed off ([`Self::refused`]), and then the point is decided
    /// again ([`Self::redecide_soon`]), since an unchanged idle point moves nothing the wait
    /// latches on and the act would be dropped.
    fn write_refused(
        &mut self,
        halted: bool,
        why: &str,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.refused(halted, why, seq, review)?;
        self.redecide_soon(if halted {
            "the hold that refused the write lifted"
        } else {
            "the server turned the write away"
        });
        Ok(())
    }

    /// Text this policy wrote and did not submit: escalated (the worker's `attention`, one ask),
    /// the badge cleared when the worker works.
    fn left_typed(
        &mut self,
        point: &Turn,
        text: &str,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let reason = format!("typed, not submitted ({why}): `{}`", clip(text));
        let attention = escalate::attention_text(self.reader(&point.screen.rows), point, &reason);
        self.escalate(
            point.screen.seq,
            &attention,
            None,
            super::escalate::Escalation::Ask,
            review,
        )?;
        self.turn_end_badge = true;
        Ok(())
    }

    /// `text` typed and submitted: [`Self::type_fenced`] where the host fences
    /// `send`, else [`Self::type_guarded`] (module docs), either one
    /// [`Self::under_hand`].
    fn type_text(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        self.under_hand(|s| match s.type_fenced(point, text, rule, review)? {
            Some(typed) => Ok(typed),
            None => s.type_guarded(point, text, rule, review),
        })
    }

    /// THE OCCUPANCY DOOR'S LINE TYPED AT A BREAK (`docs/DESIGN-harness-occupancy-2026-10-20.md`
    /// §2.1, §5 row 1), under the owner's `prompt` alone (`_grant`: only
    /// [`crate::supervise::config::Occupancy::grant`] makes one): [`Self::type_fenced`] under
    /// the hand, ONLY. `None`: the read cannot be fenced (no generation or caret, or a host
    /// without the `if-gen=` fences), and nothing is written — a break's write is never the
    /// unfenced guarded submit, which no person count holds ([`Self::type_text`]'s fallback).
    /// `Some(false)`: a fence or a person stopped it, as `type_fenced` says.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OccupancyDoor",
            action = "Type",
            project = "conformance_occupancy::project"
        )
    )]
    pub(super) fn type_fact(
        &mut self,
        _grant: &crate::supervise::config::PromptGrant,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<Option<bool>, Fail> {
        use crate::supervise::occupancy::fault::{Fault, armed};
        self.under_hand(|s| match s.type_fenced(point, text, rule, review)? {
            Some(typed) => Ok(Some(typed)),
            None if armed(Fault::DoorGuardedFallback) => {
                s.type_guarded(point, text, rule, review).map(Some)
            }
            None => Ok(None),
        })
    }

    /// ONE TYPED ACT UNDER THIS PROCESS'S HAND, where the host asked for it
    /// ([`Session::type_under_hand`]): the hand is taken for the act
    /// ([`crate::harness::upgrade_drive::hand_take_here_request`]) and given
    /// back after it, whatever the act came to
    /// ([`crate::harness::upgrade_drive::hand_give_back_here_request`]).
    fn under_hand<T>(&mut self, act: impl FnOnce(&mut Self) -> Result<T, Fail>) -> Result<T, Fail> {
        let held = self.take_hand()?;
        let out = act(self);
        if held {
            let back =
                crate::harness::upgrade_drive::hand_give_back_here_request_as(self.setup.holder());
            let words: Vec<&str> = back.split(' ').collect();
            let _ = self.call(&words);
        }
        out
    }

    /// [`Self::under_hand`]'s take: whether the hand is now held.
    fn take_hand(&mut self) -> Result<bool, Fail> {
        if !self.setup.types_under_hand || self.stopped() || self.claim_refuses(&["send"]).is_some()
        {
            return Ok(false);
        }
        let take = crate::harness::upgrade_drive::hand_take_here_request_as(self.setup.holder());
        let words: Vec<&str> = take.split(' ').collect();
        let r = self.call(&words)?;
        Ok(r.ok() && r.stdout.starts_with("OK lease acquired"))
    }

    /// Whether the host's `send` takes the `if-gen=` fence.
    pub(super) fn send_fence_known(&mut self) -> Result<bool, Fail> {
        if let Some(known) = self.caps.send_gen {
            return Ok(known);
        }
        let r = self.call(&["help", "send"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("help send failed: {}", r.stderr.trim())));
        }
        let known = r.ok() && super::super::policy::server_fences_send(&r.stdout);
        self.caps.send_gen = Some(known);
        Ok(known)
    }

    /// The fenced write (module docs): `None` when the host or the read cannot
    /// fence it (no generation on the read, no composer, `key`/`send` without
    /// `if-gen=`), and the caller falls back to [`Self::type_guarded`]; else
    /// whether the text was submitted.
    fn type_fenced(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<Option<bool>, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
            return Ok(None);
        };
        if !self.gen_fence_known()? || !self.send_fence_known()? {
            return Ok(None);
        }
        let Some(human) = point.screen.human_seq else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the judged read carries no person count; \
                 nothing written"
            ));
            return Ok(Some(false));
        };
        let person = format!("if-human={human}");
        let fence = format!("if-gen={generation}");
        let guard = format!("if={}", super::super::policy::row_guard(&rows[caret]));
        let r = self.call(&["send", &person, &fence, &guard, "--", text])?;
        match self.refused_press(&r, &format!("{person} {fence} {guard} (send)"))? {
            None => {}
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.write_refused(true, &why, seq, review)?;
                return Ok(Some(false));
            }
            Some(Pressing::Refused { why }) => {
                self.write_refused(false, &why, seq, review)?;
                return Ok(Some(false));
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                return Ok(Some(false));
            }
            Some(_) => return Ok(Some(false)),
        }
        if r.skipped_person() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a person keyed before the write; nothing written"
            ));
            self.redecide_soon("a person keyed before the write");
            return Ok(Some(false));
        }
        if r.skipped_changed() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before the write; nothing \
                 written"
            ));
            self.redecide_soon("the screen moved before the write");
            return Ok(Some(false));
        }
        if r.skipped() {
            self.not_written(point, text, rule, &format!("{fence} {guard}"), review)?;
            return Ok(Some(false));
        }
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        let settle = if self.reader(rows).paste_guard() {
            PASTE_GUARD_SETTLE_MS
        } else {
            SETTLE_MS
        };
        self.wait(&["idle", settle], SETTLE_CAP)?;
        let now = self.screen()?;
        if now.human_seq != Some(human) {
            let turn = self.turn_of(now);
            self.left_typed(
                &turn,
                text,
                "the original person count changed or is unavailable",
                review,
            )?;
            return Ok(Some(false));
        }
        let reader = self.reader(&now.rows);
        let held = reader
            .composer(&now.rows)
            .is_some_and(|(_, lines)| squashed(&lines.concat()) == squashed(text))
            && typed_draft(reader, &now)
            && reader.prompt(&now.rows).is_none();
        let (true, Some(generation), Some(caret)) =
            (held, now.generation.clone(), caret_row(reader, &now.rows))
        else {
            let turn = self.turn_of(now);
            self.left_typed(
                &turn,
                text,
                "the composer does not show it as written",
                review,
            )?;
            return Ok(Some(false));
        };
        let guard = super::super::policy::row_guard(&now.rows[caret]);
        let (r, args) = self.fenced_key(&guard, "enter", &generation, Some(&person))?;
        let turn = self.turn_of(now);
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => Ok(Some(true)),
            None if r.skipped_person() => {
                self.left_typed(&turn, text, "a person keyed before Enter", review)?;
                Ok(Some(false))
            }
            None => {
                self.left_typed(&turn, text, "the screen moved before Enter", review)?;
                Ok(Some(false))
            }
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                self.left_typed(&turn, text, "the session is halted", review)?;
                Ok(Some(false))
            }
            Some(Pressing::Refused { why }) | Some(Pressing::Unconfirmed { why, .. }) => {
                self.left_typed(&turn, text, &why, review)?;
                Ok(Some(false))
            }
            Some(_) => Ok(Some(false)),
        }
    }

    /// The draft standing in the composer submitted as it is (module docs):
    /// Enter, fenced on the judged read's generation and guarded on its caret
    /// row.
    fn submit_draft(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let fenced = match (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) {
            (Some(generation), Some(caret)) if self.gen_fence_known()? => Some((generation, caret)),
            _ => None,
        };
        let Some((generation, caret)) = fenced else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a draft stands in the composer and this host \
                 has no fenced Enter"
            ));
            let reason = format!(
                "a draft stands in the composer, not submitted: `{}`",
                clip(text)
            );
            let attention = escalate::attention_text(self.reader(rows), point, &reason);
            self.escalate(
                seq,
                &attention,
                None,
                super::escalate::Escalation::Ask,
                review,
            )?;
            self.turn_end_badge = true;
            return Ok(false);
        };
        let Some((r, args)) =
            self.press_fenced(&rows[caret], "enter", &generation, (seq, rule), review)?
        else {
            return Ok(false);
        };
        if r.skipped_changed() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before Enter; the draft stays"
            ));
            self.redecide_soon("the screen moved before the draft's Enter");
            return Ok(false);
        }
        if r.skipped() {
            self.not_written(point, text, rule, &args, review)?;
            return Ok(false);
        }
        Ok(true)
    }

    /// `key if-gen=<generation> if=<row's guard> <key>`, the fenced press [`Self::submit_draft`]
    /// and [`Self::accept_suggestion`] make.
    fn press_fenced(
        &mut self,
        row: &str,
        key: &str,
        generation: &str,
        (seq, rule): (u64, &str),
        review: &mut dyn Review,
    ) -> Result<Option<(CtlReply, String)>, Fail> {
        let guard = super::super::policy::row_guard(row);
        let args = super::super::policy::key_args(&guard, key, Some(generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        match self.refused_press(&r, &args)? {
            None => return Ok(Some((r, args))),
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => self.write_refused(true, &why, seq, review)?,
            Some(Pressing::Refused { why }) => self.write_refused(false, &why, seq, review)?,
            Some(Pressing::Unconfirmed { why, .. }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
            }
            Some(_) => {}
        }
        Ok(None)
    }

    /// The worker's own suggestion accepted (module docs): `right`, fenced on the judged read's
    /// generation and guarded on the suggestion's row; the settled screen read; then, where the
    /// composer holds exactly the suggestion as typed text, Enter, fenced and guarded on that
    /// read.
    fn accept_suggestion(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        if self.setup.types_under_hand {
            return self.type_text(point, text, rule, review);
        }
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
            return self.type_text(point, text, rule, review);
        };
        if !self.gen_fence_known()? {
            return self.type_text(point, text, rule, review);
        }
        let Some((r, args)) =
            self.press_fenced(&rows[caret], "right", &generation, (seq, rule), review)?
        else {
            return Ok(false);
        };
        if r.skipped_changed() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before the accept key"
            ));
            self.redecide_soon("the screen moved before the accept key");
            return Ok(false);
        }
        if r.skipped() {
            self.not_written(point, text, rule, &args, review)?;
            return Ok(false);
        }
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
        let mut now = self.screen()?;
        if caret_row(self.reader(&now.rows), &now.rows).is_none() {
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
        }
        let reader = self.reader(&now.rows);
        let shows = caret_text(reader, &now.rows).as_deref() == Some(text);
        let filled = shows && typed_draft(reader, &now) && reader.prompt(&now.rows).is_none();
        let (generation, caret) = match (now.generation.clone(), caret_row(reader, &now.rows)) {
            (Some(generation), Some(caret)) => (generation, caret),
            (_, None) => {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} no composer after the accept key on two \
                     settled reads; nothing more is pressed"
                ));
                return Ok(false);
            }
            (None, Some(_)) => {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the read after the accept key carries no \
                     generation to fence Enter on; nothing more is pressed"
                ));
                return Ok(false);
            }
        };
        if !filled {
            // suggestion gone): its words, typed — once, guarded.
            if !(shows && !typed_draft(reader, &now)) {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the composer no longer shows the suggestion"
                ));
                return Ok(false);
            }
            let turn = self.turn_of(now);
            return self.type_text(&turn, text, rule, review);
        }
        let Some((r, _)) =
            self.press_fenced(&now.rows[caret], "enter", &generation, (seq, rule), review)?
        else {
            return Ok(false);
        };
        if r.skipped() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the composer moved before Enter; the suggestion \
                 stays filled"
            ));
            return Ok(false);
        }
        Ok(true)
    }
}

/// What the host is told, once, when the panel's Escape has failed
/// [`PROBE_STUCK_AT`] times ([`IdleHost::inform`]: a record, no badge): the
/// state on its first line, the one action that ends it on the second.
const PANEL_STUCK: &str = "The /usage panel did not close\nPress Esc in this tab.";

/// What the host is told, once, when the probe's `/usage` was written and its Enter was not
/// ([`IdleHost::inform`]): a record, never a badge or an ask, since the loop's own words left
/// typed need no person now.
const USAGE_LEFT: &str = "/usage was typed and not sent\nThe supervisor's own check of the \
                          account: it is sent with the session's next act, or stands until you \
                          send or clear it; its panel is closed either way.";

/// How many reads in a row must say the same thing of the open panel before the probe acts on it:
/// one read alone is a frame that may be half drawn.
const PROBE_READS: u32 = 2;

/// How long the screen must hold still before the probe's next read of it
/// ([`Session::probe_pace`]), in the server's `await idle` milliseconds.
const PROBE_STILL_MS: &str = "50";

/// The host told what a claimed probe read ([`IdleHost::account_probed`]) at most once however
/// the step ends: the first word counts, and [`Session::usage_probe`] says [`ProbeMiss::Lost`]
/// for a step that ended on an error with nothing told.
struct Telling<'h> {
    host: &'h dyn IdleHost,
    told: bool,
}

impl Telling<'_> {
    fn tell(&mut self, read: Result<&Sample, ProbeMiss>) {
        if !std::mem::replace(&mut self.told, true) {
            self.host.account_probed(read);
        }
    }
}

/// What the probe's fenced write did ([`Session::probe_type`]).
enum ProbeTyped {
    /// `/usage` is in the composer and its Enter was written: `at` is the server's stamp on the
    /// Enter, `human` the person count it was fenced on (a read that carries another saw a person
    /// key since).
    Entered { at: u64, human: u64 },
    /// It was not submitted: what the host is told, why (the journal's words), and what the look
    /// does next.
    Not {
        miss: ProbeMiss,
        why: String,
        then: Probed,
        left: bool,
    },
}

/// A percentage as the journal's `PROBED` line carries it: `7%`, or `?%`
/// for a figure nobody read.
fn pct_word(pct: Option<u32>) -> String {
    pct.map_or_else(|| "?%".to_string(), |p| format!("{p}%"))
}

impl<C: Ctl> Session<'_, C> {
    /// One `/usage` probe as a step of the loop.
    pub(super) fn usage_probe(
        &mut self,
        point: &Turn,
        decided: &TurnEndAction,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Probed, Fail> {
        let Some(host) = opts.idle_host.clone() else {
            return Ok(Probed::No);
        };
        if !self.probe_advised(&point.screen) {
            return Ok(Probed::No);
        }
        let shut = match self.probe_gates(point, decided, host.as_ref(), opts)? {
            Some(gate) => Some((gate, None)),
            None => host
                .account_probe()
                .err()
                .map(|why| (ProbeGate::Claim, Some(why.word()))),
        };
        if let Some((gate, why)) = shut {
            self.probe_shut(point, gate, why, review)?;
            return Ok(Probed::No);
        }
        self.probe_said = Some(self.review_key(point));
        let mut tell = Telling {
            host: host.as_ref(),
            told: false,
        };
        let done = self.probe_step(point, &mut tell, opts, review);
        if !tell.told {
            tell.tell(Err(ProbeMiss::Lost));
            review.note(&format!(
                "PROBED seq={} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} no sample ({})",
                point.screen.seq,
                ProbeMiss::Lost.word()
            ));
        }
        done
    }

    /// Whether the advice this point's decision was given ([`Self::account_advise`]) is the
    /// probe: the spec's fourth gate, asked first, so with no roster nothing else is looked at,
    /// requested or said.
    fn probe_advised(&self, screen: &Screen) -> bool {
        matches!(
            &self.account_seen,
            Some((at, AccountPoint::TurnEnd, Some(AccountAdvice::Probe { .. })))
                if *at == screen.seq
        )
    }

    /// A due probe was not run at `point`.
    pub(super) fn probe_shut(
        &mut self,
        point: &Turn,
        gate: ProbeGate,
        host: Option<&str>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if !self.probe_advised(&point.screen) {
            return Ok(());
        }
        let key = self.review_key(point);
        if self.probe_said.as_deref() == Some(key.as_str()) {
            return Ok(());
        }
        self.probe_said = Some(key);
        let host = host.map_or_else(String::new, |word| format!(" host={word}"));
        review.say(&format!(
            "SKIPPED seq={} rule={RULE_USAGE_PROBE} gate={}{host}",
            point.screen.seq,
            gate.word()
        ))?;
        Ok(())
    }

    /// The probe's gates, in the spec's order (§3 G) but for the roster's advice, asked first
    /// ([`Self::probe_advised`]): `None` with every one open, else the first that shut.
    fn probe_gates(
        &mut self,
        point: &Turn,
        decided: &TurnEndAction,
        host: &dyn IdleHost,
        opts: &SuperviseOpts,
    ) -> Result<Option<ProbeGate>, Fail> {
        //    probes.
        if self.claim.watching_behind().is_some() {
            return Ok(Some(ProbeGate::Watching));
        }
        if self.setup.ledger_path.is_none() {
            return Ok(Some(ProbeGate::NoLedger));
        }
        if self.probe_off {
            return Ok(Some(ProbeGate::ProbesOff));
        }
        if self.probe != UsageProbe::Idle {
            return Ok(Some(ProbeGate::PanelOwed));
        }
        if !self.turn_live {
            return Ok(Some(ProbeGate::NotLive));
        }
        if matches!(decided, TurnEndAction::Escalate { .. }) {
            return Ok(Some(ProbeGate::Escalated));
        }
        let screen = &point.screen;
        if self.probe_fold == Some((screen.cols, screen.pane_rows)) {
            return Ok(Some(ProbeGate::FoldLatch));
        }
        if host.wants() {
            return Ok(Some(ProbeGate::HostWants));
        }
        if self.stalled.is_some() || self.stall_suspect {
            return Ok(Some(ProbeGate::Stalled));
        }
        if self.settling().is_some() {
            return Ok(Some(ProbeGate::WakeSettle));
        }
        if screen.generation.is_none()
            || screen.human_seq.is_none()
            || !self.gen_fence_known()?
            || !self.send_fence_known()?
        {
            return Ok(Some(ProbeGate::NoFence));
        }
        self.program = self.foreground_program()?;
        if self.stall_suspect {
            return Ok(Some(ProbeGate::Stalled));
        }
        match self.reader(&screen.rows).program() {
            Program::Claude => {}
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                return Ok(Some(ProbeGate::NotClaude));
            }
        }
        let reading = self.turn_end_reread(screen, opts);
        if let Some(gate) = self
            .turn_end
            .probe_gate(&reading, &opts.policy, self.fx.mono())
        {
            return Ok(Some(gate));
        }
        Ok((!self.alt_screen()?).then_some(ProbeGate::NotFullscreen))
    }

    /// Whether the session's program draws on the alternate screen (`modes` prints
    /// `alt_screen=true`): Claude Code's fullscreen renderer, the only one the probe types into,
    /// since under the inline renderer a grown panel scrolls its header off the grid, where no
    /// header guard can fence an Escape.
    fn alt_screen(&mut self) -> Result<bool, Fail> {
        let r = self.call(&["modes"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("modes failed: {}", r.stderr.trim())));
        }
        Ok(r.ok() && r.stdout.lines().any(|l| l.trim() == "alt_screen=true"))
    }

    /// Whether the composer on `screen` holds exactly the probe's command as
    /// typed text: the loop's own words, not yet submitted.
    fn holds_probe_draft(&self, screen: &Screen) -> bool {
        let reader = self.reader(&screen.rows);
        reader
            .composer(&screen.rows)
            .is_some_and(|(_, lines)| squashed(&lines.concat()) == squashed(USAGE_COMMAND))
            && typed_draft(reader, screen)
    }

    /// A panel of the harness's may come: `/usage` was written into the composer (or its write's
    /// answer never came).
    fn probe_owes(&mut self, seq: u64) {
        if self.probe != UsageProbe::Idle {
            return;
        }
        self.probe = UsageProbe::Owed {
            own: true,
            since: self.fx.mono(),
        };
        self.ledger_row(
            RULE_USAGE_PROBE,
            approvals::Outcome::Typed,
            USAGE_COMMAND,
            approvals::PROBE_TYPED_REASON,
            seq,
        );
    }

    /// The session was seen with no panel of the harness's left on it: the ledger's `waiting` row,
    /// `CLEARED` where the journal said the panel was stuck, and the point decided again shortly
    /// (its act may never have been carried out).
    fn probe_closed(&mut self, seq: u64, review: &mut dyn Review) {
        let was = std::mem::replace(&mut self.probe, UsageProbe::Idle);
        self.probe_hold = None;
        if was == UsageProbe::Idle {
            return;
        }
        self.ledger_row(
            RULE_USAGE_PROBE,
            approvals::Outcome::Waiting,
            USAGE_COMMAND,
            approvals::PROBE_CLOSED_REASON,
            seq,
        );
        if matches!(was, UsageProbe::Closing { .. }) {
            review.note(&format!("CLEARED seq={seq} usage panel"));
        }
        self.redecide_soon("the usage panel closed");
    }

    /// Whether a composer is drawn on `screen`: if so no panel stands in its place, and an owed
    /// panel is closed ([`Self::probe_closed`]) unless the composer still holds the probe's own
    /// unsent command ([`Self::holds_probe_draft`]), which may yet be sent.
    fn composer_settles(&mut self, screen: &Screen, review: &mut dyn Review) -> bool {
        if self.reader(&screen.rows).composer(&screen.rows).is_none() {
            return false;
        }
        if !self.holds_probe_draft(screen) {
            self.probe_closed(screen.seq, review);
        }
        true
    }

    /// The pause between two of the probe's reads: the screen holding still [`PROBE_STILL_MS`],
    /// or [`ProbeWaits::step`] at most, never past `until`.
    fn probe_pace(&mut self, until: Instant) -> Result<(), Fail> {
        let left = until.saturating_duration_since(self.fx.mono());
        self.wait(
            &["idle", PROBE_STILL_MS],
            left.min(self.setup.probe_waits.step),
        )?;
        Ok(())
    }

    /// The step past the claim ([`Self::usage_probe`]): type, await the
    /// panel, read it settled, tell the host, close.
    fn probe_step(
        &mut self,
        point: &Turn,
        tell: &mut Telling<'_>,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Probed, Fail> {
        let seq = point.screen.seq;
        let (entered, human) = match self.under_hand(|s| s.probe_type(point, review))? {
            ProbeTyped::Entered { at, human } => (at, human),
            ProbeTyped::Not {
                miss,
                why,
                then,
                left,
            } => {
                tell.tell(Err(miss));
                review.note(&format!("SKIPPED seq={seq} rule={RULE_USAGE_PROBE} {why}"));
                if left {
                    tell.host.inform(USAGE_LEFT);
                }
                if then == Probed::Again && self.turn_end_due.is_none() {
                    self.redecide_soon("the accounts probe was not written");
                }
                return Ok(then);
            }
        };
        let no_sample = |miss: ProbeMiss, tell: &mut Telling<'_>, review: &mut dyn Review| {
            tell.tell(Err(miss));
            review.note(&format!(
                "PROBED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} no sample ({})",
                miss.word()
            ));
        };
        let waits = self.setup.probe_waits;
        let busy = |s: &Self, grid: &Screen| s.reader(&grid.rows).phase(&grid.rows) == Phase::Busy;
        let open_by = self.fx.mono() + waits.open;
        self.wait(&["seq", &entered.to_string()], waits.step.min(waits.open))?;
        let mut grid = loop {
            let grid = self.grid()?;
            if aterm_phase::settings_panel(&grid.rows).is_some() {
                break grid;
            }
            if busy(self, &grid) || self.fx.mono() >= open_by {
                no_sample(ProbeMiss::NotOpened, tell, review);
                return Ok(Probed::Done);
            }
            self.probe_pace(open_by)?;
        };
        let settle_by = self.fx.mono() + waits.settle;
        let mut prior: Option<Vec<PanelWindow>> = None;
        let (mut folds, mut strange, mut tail_seen) = (0u32, 0u32, false);
        let (read, panel) = loop {
            let Some(panel) = aterm_phase::settings_panel(&grid.rows) else {
                no_sample(ProbeMiss::Closed, tell, review);
                return Ok(Probed::Done);
            };
            if grid.human_seq != Some(human) {
                no_sample(ProbeMiss::Person, tell, review);
                return Ok(Probed::Open);
            }
            if busy(self, &grid) {
                no_sample(ProbeMiss::Unsettled, tell, review);
                return Ok(Probed::Open);
            }
            let said = if panel.renderer == aterm_phase::PanelRenderer::Fullscreen {
                usage_panel_read(&grid.rows, panel.header)
            } else {
                PanelRead::NotUsage
            };
            folds = if said == PanelRead::Fold {
                folds + 1
            } else {
                0
            };
            strange = if said == PanelRead::NotUsage {
                strange + 1
            } else {
                0
            };
            tail_seen |= matches!(said, PanelRead::Settled(_) | PanelRead::Unsettled);
            match said {
                PanelRead::Settled(windows) => {
                    if prior.as_ref() == Some(&windows) {
                        let now = self.now_unix();
                        let offset = self.local_offset_s();
                        let zone = self.zone_lookup();
                        break (
                            panel_sample(&windows, now, offset, &zone).ok_or(ProbeMiss::Unsettled),
                            panel,
                        );
                    }
                    prior = Some(windows);
                }
                PanelRead::Unsettled => prior = None,
                PanelRead::Fold => {
                    prior = None;
                    if folds >= PROBE_READS {
                        // read of this probe was the fold.
                        if !tail_seen {
                            self.probe_fold = Some((point.screen.cols, point.screen.pane_rows));
                        }
                        break (Err(ProbeMiss::Fold), panel);
                    }
                }
                PanelRead::NotUsage => {
                    prior = None;
                    if strange >= PROBE_READS {
                        self.probe_off = true;
                        break (Err(ProbeMiss::Renderer), panel);
                    }
                }
            }
            if self.fx.mono() >= settle_by {
                break (Err(ProbeMiss::Unsettled), panel);
            }
            self.probe_pace(settle_by)?;
            grid = self.grid()?;
        };
        match &read {
            Ok(sample) => {
                tell.tell(Ok(sample));
                review.say(&format!(
                    "PROBED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} session={} week={}",
                    pct_word(sample.session_pct),
                    pct_word(sample.week_pct)
                ))?;
            }
            Err(miss) => no_sample(*miss, tell, review),
        }
        self.panel_escape(&grid, panel, false, opts, review)?;
        Ok(if self.probe == UsageProbe::Idle {
            Probed::Done
        } else {
            Probed::Open
        })
    }

    /// The probe's fenced write: [`Self::type_fenced`] with the person fence added and no
    /// fallback: `send if-human=<n> if-gen=<the judged read> if=<its caret row> -- /usage`, the
    /// composer required to hold exactly the command with no box up, then `key if-human=<n'>
    /// if-gen=<that read> if=<its caret row> enter`. Never the guarded `turn`, which pastes before
    /// its guard is checked.
    fn probe_type(&mut self, point: &Turn, review: &mut dyn Review) -> Result<ProbeTyped, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let not = |miss: ProbeMiss, why: &str, then: Probed| {
            Ok(ProbeTyped::Not {
                miss,
                why: why.to_string(),
                then,
                left: false,
            })
        };
        let left = |miss: ProbeMiss, why: &str| {
            Ok(ProbeTyped::Not {
                miss,
                why: format!("typed, not submitted: {why}"),
                then: Probed::Again,
                left: true,
            })
        };
        let (Some(generation), Some(human), Some(caret)) = (
            point.screen.generation.clone(),
            point.screen.human_seq,
            caret_row(self.reader(rows), rows),
        ) else {
            return not(
                ProbeMiss::NotWritten,
                "the judged read carries no fence; nothing written",
                Probed::No,
            );
        };
        let person = format!("if-human={human}");
        let fence = format!("if-gen={generation}");
        let guard = format!("if={}", super::super::policy::row_guard(&rows[caret]));
        let r = self.call(&["send", &person, &fence, &guard, "--", USAGE_COMMAND])?;
        match self.refused_press(&r, &format!("{person} {fence} {guard} (send)"))? {
            None => {}
            Some(Pressing::Lost { why, .. }) => {
                // Cut mid-exchange, the text may be in the composer.
                if !r.untouched() {
                    self.probe_owes(seq);
                }
                return Err(Fail::Lost(why));
            }
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                return not(
                    ProbeMiss::NotWritten,
                    "the session is halted; nothing written",
                    Probed::Again,
                );
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                return not(
                    ProbeMiss::NotWritten,
                    "the server turned the write away; nothing written",
                    Probed::Again,
                );
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                self.caps.send_gen = Some(false);
                return not(ProbeMiss::NotWritten, &why, Probed::No);
            }
            Some(_) => {
                return not(ProbeMiss::NotWritten, "nothing written", Probed::No);
            }
        }
        if r.skipped_person() {
            return not(
                ProbeMiss::Person,
                "a person keyed before the write; nothing written",
                Probed::Again,
            );
        }
        if r.skipped_changed() {
            return not(
                ProbeMiss::NotWritten,
                "the screen moved before the write; nothing written",
                Probed::Again,
            );
        }
        if r.skipped() {
            return not(
                ProbeMiss::NotWritten,
                "the guard matched no row of the judged screen; nothing written",
                Probed::No,
            );
        }
        self.probe_owes(seq);
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
        let now = self.screen()?;
        let reader = self.reader(&now.rows);
        let held = self.holds_probe_draft(&now) && reader.prompt(&now.rows).is_none();
        let (true, Some(generation), Some(human), Some(caret)) = (
            held,
            now.generation.clone(),
            now.human_seq,
            caret_row(reader, &now.rows),
        ) else {
            return left(
                ProbeMiss::NotWritten,
                "the composer does not show it as written",
            );
        };
        let guard = super::super::policy::row_guard(&now.rows[caret]);
        let person = format!("if-human={human}");
        let (r, args) = self.fenced_key(&guard, "enter", &generation, Some(&person))?;
        let stamp = r.seq().unwrap_or(now.seq);
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => Ok(ProbeTyped::Entered { at: stamp, human }),
            None if r.skipped_person() => left(ProbeMiss::Person, "a person keyed before Enter"),
            None => left(ProbeMiss::NotWritten, "the screen moved before Enter"),
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                left(ProbeMiss::NotWritten, "the session is halted")
            }
            Some(Pressing::Refused { why }) | Some(Pressing::Unconfirmed { why, .. }) => {
                left(ProbeMiss::NotWritten, &one_line(&why))
            }
            Some(_) => left(ProbeMiss::NotWritten, "its Enter was not written"),
        }
    }

    /// Whether the probe's own `/usage` stands typed and unsent on `screen`: a panel is owed (by
    /// this loop or, by the ledger, one before it) and the composer holds exactly the command.
    fn probe_left_stands(&self, screen: &Screen) -> bool {
        self.probe != UsageProbe::Idle && self.holds_probe_draft(screen)
    }

    /// The probe's own `/usage`, left typed ([`Self::probe_type`]), sent where the turn-end policy
    /// would send the draft in an act's place ([`TurnEndAction::Submit`]).
    fn probe_left_enter(&mut self, point: &Turn, review: &mut dyn Review) -> Result<(), Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let skipped = |why: &str, review: &mut dyn Review| {
            review.note(&format!(
                "SKIPPED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} stays typed: {why}"
            ));
        };
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
            skipped("the judged read carries no fence for its Enter", review);
            return Ok(());
        };
        let guard = super::super::policy::row_guard(&rows[caret]);
        let person = point.screen.human_seq.map(|n| format!("if-human={n}"));
        let (r, args) = self.fenced_key(&guard, "enter", &generation, person.as_deref())?;
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => {
                review.note(&format!(
                    "TYPED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} left typed, its \
                     Enter written now: no act of the policy's, and the panel it opens is closed \
                     unsampled"
                ));
                let due = self.fx.mono() + self.turn_end.timing.take_within;
                self.set_due(due, "the accounts probe's text was sent");
            }
            None if r.skipped_person() || r.skipped_changed() => {
                skipped("a person keyed or the screen moved before Enter", review);
                self.redecide_soon("the accounts probe's text was not sent");
            }
            None => skipped("its caret row matched no row of the judged screen", review),
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => self.write_refused(true, &why, seq, review)?,
            Some(Pressing::Refused { why }) => self.write_refused(false, &why, seq, review)?,
            Some(Pressing::Unconfirmed { why, .. }) => skipped(&one_line(&why), review),
            Some(_) => {}
        }
        Ok(())
    }

    /// THE ONE ENTER A DIALOG'S STATE ASKS FOR ([`TurnEndAction::Confirm`]:
    /// Claude Code's `Login successful. Press Enter to continue…`), fenced as
    /// the probe's Enter is — `key if-human=<n> if-gen=<the judged read>
    /// if=<its row> enter` — journaled `TYPED … rule=<rule> enter`, booked in
    /// the ledger as typed, and told to the policy
    /// ([`TurnEndState::acted`], which spends nothing of the hour's and awaits
    /// nothing: the dialog's leaving is the answer).
    fn confirm_dialog(
        &mut self,
        point: &Turn,
        action: &TurnEndAction,
        rule: &'static str,
        row: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        let skipped = |why: &str, review: &mut dyn Review| {
            review.note(&format!("SKIPPED seq={seq} rule={rule} enter: {why}"));
        };
        let Some(generation) = point.screen.generation.clone() else {
            skipped("the judged read carries no fence for its Enter", review);
            return Ok(());
        };
        let Some(human_seq) = point.screen.human_seq else {
            skipped(
                "the judged read carries no person count for its Enter",
                review,
            );
            return Ok(());
        };
        let guard = super::super::policy::row_guard(row);
        let person = format!("if-human={human_seq}");
        let (r, args) = self.fenced_key(&guard, "enter", &generation, Some(&person))?;
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => {
                let reread = self.turn_end_reread(&point.screen, opts);
                self.turn_end.acted(action, &reread, self.fx.mono());
                review.say(&format!(
                    "TYPED seq={seq} rule={rule} enter at {}",
                    clip(row)
                ))?;
                self.ledger_row(rule, approvals::Outcome::Typed, "enter", row.trim(), seq);
            }
            None if r.skipped_person() || r.skipped_changed() => {
                skipped("a person keyed or the screen moved before Enter", review);
                self.redecide_soon("the dialog's Enter was not pressed");
            }
            None => skipped("its row matched no row of the judged screen", review),
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => self.write_refused(true, &why, seq, review)?,
            Some(Pressing::Refused { why }) => self.write_refused(false, &why, seq, review)?,
            Some(Pressing::Unconfirmed { why, .. }) => skipped(&one_line(&why), review),
            Some(_) => {}
        }
        Ok(())
    }

    /// One guarded Escape at the panel on `grid`.
    pub(super) fn panel_escape(
        &mut self,
        grid: &Screen,
        panel: aterm_phase::SettingsPanel,
        off: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = grid.seq;
        let guard = panel.header_guard(&grid.rows);
        if !guard.is_empty() && grid.human_seq.is_some() {
            match self.guarded_key(seq, &guard, "esc", grid.human_seq)? {
                Dismiss::Pressed { .. } => {
                    self.wait_row_gone(&guard, seq, self.setup.probe_waits.close)?;
                }
                Dismiss::Skipped => {}
                Dismiss::Yielded => return Ok(()),
                Dismiss::Refused { halted, why } if off => {
                    if why.contains("ERR busy input-unread") {
                        self.stall_suspect = true;
                    }
                    review.note(&format!(
                        "{} seq={seq} {}: the {USAGE_COMMAND} panel is owed until the word turns \
                         (autopilot off: no wait, no second Escape)",
                        if halted { "HALTED" } else { "REFUSED" },
                        clip(&one_line(&why))
                    ));
                    return Ok(());
                }
                Dismiss::Refused { halted, why } => {
                    return self.refused(halted, &why, seq, review);
                }
            }
            let after = self.grid()?;
            if aterm_phase::settings_panel(&after.rows).is_none() {
                self.composer_settles(&after, review);
                return Ok(());
            }
        }
        let (own, since, failed, told) = match self.probe {
            UsageProbe::Idle => return Ok(()),
            UsageProbe::Owed { own, since } => (own, since, 0, false),
            UsageProbe::Closing {
                own,
                since,
                tries,
                told,
                ..
            } => (own, since, tries, told),
        };
        let tries = failed.saturating_add(1);
        let pause = crate::supervise::ladder::Ladder(&self.setup.survey_retry)
            .step(usize::try_from(tries - 1).unwrap_or(usize::MAX));
        let stuck = !off && tries >= PROBE_STUCK_AT && !told;
        self.probe = UsageProbe::Closing {
            own,
            since,
            tries,
            at: self.fx.mono() + pause,
            told: told || stuck,
        };
        let next = if off {
            "owed until the word turns (autopilot off: no Escape again)".to_string()
        } else {
            format!("Escape again in {} s", pause.as_secs())
        };
        review.note(&format!(
            "WAITING seq={seq} the {USAGE_COMMAND} panel is open ({tries} tries): {next}"
        ));
        if stuck && let Some(host) = opts.idle_host.as_ref() {
            host.inform(PANEL_STUCK);
        }
        Ok(())
    }

    /// The panel owed its close, at the top of every look ([`Session::look`]; the spec's §3.1).
    pub(super) fn usage_panel_look(
        &mut self,
        turn: &Turn,
        state: &mut Looking,
        deadline: Instant,
        off: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let (own, since) = match self.probe {
            UsageProbe::Idle => return Ok(false),
            UsageProbe::Owed { own, since } | UsageProbe::Closing { own, since, .. } => {
                (own, since)
            }
        };
        let seq = turn.screen.seq;
        if self.composer_settles(&turn.screen, review) {
            return Ok(false);
        }
        let grid = self.grid()?;
        let Some(panel) = aterm_phase::settings_panel(&grid.rows) else {
            self.probe_closed(seq, review);
            return Ok(!off && turn.phase != Phase::Prompt);
        };
        let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        if off
            && (self.stalled.is_some()
                || self.stall_suspect
                || matches!(self.probe, UsageProbe::Closing { .. })
                || self.reader(&grid.rows).phase(&grid.rows) == Phase::Busy
                || self.claim.watching_behind().is_some()
                || self.probe_hold.is_some_and(|until| until > self.fx.mono()))
        {
            return Ok(false);
        }
        let guard = panel.header_guard(&grid.rows);
        let fx = self.fx.clone();
        let step = |until: Option<Instant>| {
            let now = fx.mono();
            until
                .map_or(WAIT_STEP, |t| t.saturating_duration_since(now))
                .min(WAIT_STEP)
                .min(deadline.saturating_duration_since(now))
        };
        if self.stall_look(seq, state, review)?
            || self.reader(&grid.rows).phase(&grid.rows) == Phase::Busy
            || self.claim.watching_behind().is_some()
        {
            self.wait_row_gone(&guard, grid.seq, step(None))?;
            return Ok(true);
        }
        if !own && self.fx.since(since) < grace {
            self.wait_row_gone(&guard, grid.seq, step(since.checked_add(grace)))?;
            return Ok(true);
        }
        if let Some(until) = self.probe_hold.filter(|until| *until > self.fx.mono()) {
            self.wait_row_gone(&guard, grid.seq, step(Some(until)))?;
            return Ok(true);
        }
        self.program = self.foreground_program()?;
        let hold = self.hands_off_until(opts);
        if off && (hold.is_some() || self.stall_suspect) {
            return Ok(false);
        }
        self.probe_hold = hold;
        if let Some(until) = self.probe_hold {
            review.note(&self.held_line(seq, until, "the usage panel's Escape"));
            self.wait_row_gone(&guard, grid.seq, step(Some(until)))?;
            return Ok(true);
        }
        if let UsageProbe::Closing { at, .. } = self.probe
            && self.fx.mono() < at
        {
            self.wait_row_gone(&guard, grid.seq, step(Some(at)))?;
            return Ok(true);
        }
        self.panel_escape(&grid, panel, off, opts, review)?;
        Ok(!off)
    }

    /// The point decided again after a probe ([`Probed::Done`]; the spec's P21).
    pub(super) fn after_probe(
        &mut self,
        point: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let screen = self.screen()?;
        let turn = self.point_of(screen);
        if !self.composer_settles(&turn.screen, review) && self.probe != UsageProbe::Idle {
            self.redecide_soon("the usage panel is closing");
            return Ok(());
        }
        if self.review_key(&turn) != self.review_key(point) {
            self.probe_off = true;
            review.note(&format!(
                "SKIPPED seq={} rule={RULE_USAGE_PROBE} the point changed across the probe: \
                 nothing is carried out on it, and this loop probes no more",
                turn.screen.seq
            ));
            return Ok(());
        }
        self.turn_end_now(point, opts, allow, review)
    }
}

/// What a SHADOW replay (`tape::shadow`) decides a turn end again on, under its candidate policy.
#[derive(Debug, Default)]
pub(super) struct ShadowPoint {
    reading: Option<TurnEndReading>,
    prose: Option<super::super::policy::approval::QuestionPriority>,
    withheld: bool,
}

impl ShadowPoint {
    /// The candidate's decision, under `policy`, on the fold `state` at `now`.
    fn decide(
        self,
        state: &super::super::policy::turn_end::TurnEndState,
        mut policy: SupervisorConfig,
        now: Instant,
    ) -> TurnEndAction {
        let Some(reading) = self.reading.filter(|_| !self.withheld) else {
            return TurnEndAction::Nothing;
        };
        if let Some(word) = self.prose {
            policy.answer_questions = true;
            policy.question_priority = word;
        }
        decide_turn_end(state, &reading, &policy, now)
    }
}

/// A turn-end decision as its mark on a harness tape writes it ([`Session::decide_to_act`]).
pub(super) fn turn_end_outcome(action: &TurnEndAction, at: &dyn Fn(Instant) -> String) -> String {
    match action {
        TurnEndAction::WaitUntil { until, why, wait } => format!(
            "WaitUntil {{ until: {}, why: {why:?}, wait: {wait:?} }}",
            at(*until)
        ),
        TurnEndAction::Restart {
            why,
            rule_id,
            otherwise,
        } => format!(
            "Restart {{ why: {why:?}, rule_id: {rule_id:?}, otherwise: {} }}",
            turn_end_outcome(otherwise, at)
        ),
        TurnEndAction::Nothing
        | TurnEndAction::Type { .. }
        | TurnEndAction::Accept { .. }
        | TurnEndAction::TypeCommand { .. }
        | TurnEndAction::Submit { .. }
        | TurnEndAction::Confirm { .. }
        | TurnEndAction::Escalate { .. } => format!("{action:?}"),
    }
}

/// Whether the table's `action` at a point that asks a question in prose is one the session's
/// priority word can change ([`Session::decide_to_act`]): an answer, or the escalation
/// `answer_questions = false` makes ([`ANSWERS_OFF`]).
fn session_word_can_decide(action: &TurnEndAction) -> bool {
    match action {
        TurnEndAction::Escalate { reason } => reason.ends_with(ANSWERS_OFF),
        action => action
            .rule_id()
            .is_some_and(super::super::policy::turn_end::is_answer_rule),
    }
}

/// The words a wall's reset is placed and remembered under ([`Session::wall_reset_at`]): its kind
/// and its `reset=` text.
fn reset_key(kind: aterm_phase::WallKind, text: Option<&str>) -> String {
    format!("{}|{}", kind.name(), text.unwrap_or("-"))
}

/// Whether the loop's host withholds the session's continuations now
/// ([`IdleHost::withholds_turn_end`]).
fn host_withholds(opts: &SuperviseOpts) -> bool {
    opts.idle_host
        .as_ref()
        .is_some_and(|h| h.withholds_turn_end())
}

/// THE FALLBACK'S RELAUNCH AS A LOOP JOURNALED IT, in unix seconds.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct JournaledRelaunch {
    /// When the last relaunch made was first asked: its run's first host
    /// step, or its own line where none was journaled.
    pub(super) made: Option<i64>,
    /// When the run of a relaunch asked and not made yet was first asked.
    pub(super) waiting: Option<i64>,
}

/// THE FALLBACK'S RELAUNCH, read from the journal beside `sid`'s ledger (`journal`).
pub(super) fn journaled_relaunch(
    journal: &str,
    sid: Option<&str>,
    word: &str,
) -> JournaledRelaunch {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let step = format!(" restart:{word} step=");
    let mut run: Option<i64> = None;
    let mut made = None;
    for rec in journal
        .lines()
        .filter_map(|text| crate::supervise::journal::JournalRecord::from_json(text).ok())
        .filter(|rec| rec.sid.as_deref() == sid)
    {
        match rec.kind.as_str() {
            "restarted" if rec.phase == crate::supervise::policy::turn_end::RULE_MODEL_FALLBACK => {
                made = Some(run.take().unwrap_or(rec.t));
            }
            "continued" | "typed" => run = None,
            _ if rec.summary.starts_with("HOST ") && rec.summary.contains(&step) => {
                run.get_or_insert(rec.t);
            }
            _ => {}
        }
    }
    JournaledRelaunch {
        made: made.map(|ms: i64| ms.div_euclid(1000)),
        waiting: run.map(|ms| ms.div_euclid(1000)),
    }
}

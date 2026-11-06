// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor turn-end event loop driving `turn_end` policy decisions against `Fx`.
use super::super::policy::approval::squashed;
use super::super::policy::exception::{Exception, exception_for};
use super::super::policy::needs;
use super::super::policy::notice;
use super::super::policy::sleeps::InTurn;
use super::super::policy::trace::{Outcome as TraceOutcome, TraceStep};
use super::super::policy::turn_end::{
    ANSWERS_OFF, AccountAnswer, AccountAt, CREDITS_HOLD_MODEL_TOLD, CREDITS_HOLD_TOLD, Composer,
    ComposerRead, Conversation, CreditsCarry, GoAheadVerdict, LOGIN_SHARED_LOOK, ProbeGate,
    RULE_ACCOUNT, RULE_INPUT_LOST, RULE_LAND, RULE_LEFT, RULE_LOGIN, RULE_USAGE_PROBE, RecordTurn,
    Restart, RestartAnswer, RestartAsk, RestartClaim, TRY_ANSWER_SLACK, WAKE_KEPT, account_hold,
    act_turn_work, budget_frees, conversation_mark, decide_turn_end, decide_turn_end_traced,
    done_row_work, is_wait_resume, outage_of, reads_conversation,
};
use super::super::tap_clock::{TapClock, TapSays, Timed, UsageWindow, timed};
use super::wall_confirm::{Check, LeftRead};
use super::*;
use crate::harness::accounts::Sample;
use crate::harness::acts::TaskFace;
use crate::harness::profile::AccountKey;
use crate::harness::upgrade_drive::HAND_RENEW;
use crate::harness::usage::{
    PanelRead, PanelWindow, USAGE_COMMAND, panel_sample_local, usage_panel_read,
};
use crate::supervise::custody::{self, CustodyGate, LeftStands};
#[cfg(test)]
pub(super) use crate::supervise::custody::{CUSTODY_CAP, CUSTODY_STEP, custody_step};
pub(super) use crate::supervise::custody::{ECHO_HANG, Settles};
use aterm_types::control_verbs::{ScreenGen, TURN_SUBMIT_BUSY_REPLY};

/// Usage-tap state and timing at a single wall ([`Session::tap_reset`]).
#[derive(Debug, Clone)]
pub(super) struct WallTap {
    key: String,
    windows: &'static [UsageWindow],
    clock: TapClock,
    /// The reset unix timestamp the wall is waited to ([`Session::tap_timing`]).
    at: Option<i64>,
    /// Whether the tap (or a prior loop's tap) timed the reset.
    by_tap: bool,
    /// Reset unix timestamp carried from a prior loop's tap (`tap_unix=`, [`Session::seed_tap`]).
    carried: Option<i64>,
}

/// THE ACCOUNT A STANDING LIMIT WAS READ UNDER ([`Session::login_moved_at`]):
/// kept from the first decision at the limit (or a prior loop's `login=`)
/// until its track ends, as the usage tap's clock is ([`WallTap`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WallLogin {
    /// The account the session's profile named then.
    pub(super) account: AccountKey,
    /// Another account the profile names since, held until the policy takes
    /// the move up ([`Session::login_taken`]).
    pub(super) moved_to: Option<AccountKey>,
    /// The account the limit was read under before the move the policy
    /// took, while the resume that move owes is untyped: what the limit's
    /// ledger row names, so a successor loop owes the resume too.
    pub(super) owed_from: Option<AccountKey>,
}

/// How often the profile is read for a moved login while a wait stands ([`Session::login_edge`]).
pub(super) const LOGIN_LOOK_EVERY: Duration = Duration::from_secs(5);

/// Whether a wall of `kind` is the account's own, lifted by a login on
/// another account: a usage limit or a model bucket ([`TurnEndReading::login_moved`]).
const fn moves_with_login(kind: aterm_phase::WallKind) -> bool {
    matches!(
        kind,
        aterm_phase::WallKind::UsageSession
            | aterm_phase::WallKind::UsageWeekly
            | aterm_phase::WallKind::ModelBucket { .. }
    )
}

/// Whether `composer` holds the point `action` was decided at with nothing due
/// ([`Session::draft_held`]).
fn draft_holds(composer: &Composer, action: &TurnEndAction) -> bool {
    match (composer.asks(), action) {
        (Some(why), TurnEndAction::Escalate { reason }) => reason == why,
        // A person's command stands as their hold (v6 §14.17.5.8), as an unread composer does.
        (None, TurnEndAction::Nothing) => {
            matches!(composer, Composer::Unknown | Composer::Command(..))
        }
        _ => false,
    }
}

/// Whether executing `action` drives the agent: what an observed agent's turn end withholds
/// ([`Session::observing`]). A restart drives it too: the host ends the process, types its
/// relaunch line into the tab and its continuation into the new process, through its own
/// connection rather than [`Session::call`]'s gate, so an observed agent is never restarted.
const fn types_input(action: &TurnEndAction) -> bool {
    match action {
        TurnEndAction::Type { .. }
        | TurnEndAction::TypeCommand { .. }
        | TurnEndAction::Submit { .. }
        | TurnEndAction::Confirm { .. }
        | TurnEndAction::Cancel { .. }
        | TurnEndAction::Restart { .. } => true,
        TurnEndAction::Nothing
        | TurnEndAction::WaitUntil { .. }
        | TurnEndAction::Escalate { .. } => false,
    }
}

/// The furthest a wall's reset may jump on a reprint of the same words before the jump is doubted.
const REPRINT_JUMP_MAX_S: i64 = 30 * 60;

/// How many reprints a doubted jump is kept back for ([`REPRINT_JUMP_MAX_S`]).
pub(super) const REPRINT_JUMPS_MAX: u32 = 4;

/// How long past its placement a reset is doubted at all ([`REPRINT_JUMPS_MAX`]).
const REPRINT_HOLD_WINDOW_S: i64 = 2 * 3600;

/// The longest text the cursor-row guard names whole, as `^❯ <text>`.
const GUARD_TAIL_CHARS: usize = 40;

/// The most of the text's last word a longer text's guard names.
const GUARD_WORD_CHARS: usize = 16;

/// `yield=<floor>` on every `turn` this policy types.
const TURN_YIELD: &str = "yield=0.2";

/// The standing rules ride in the continuation cut to this many characters.
pub(super) const RULES_CAP: usize = 400;

/// Maximum duration the loop waits for the worker to react to a supervisor write.
pub(super) const REACTION_WAIT: Duration = Duration::from_secs(10);

/// How long the echo of a fenced write must hold still before its Enter
/// ([`aterm_phase::ScreenReader::paste_guard`], [`Session::await_echo`]).
pub(super) const PASTE_GUARD_HOLD: Duration = Duration::from_millis(600);

/// How many [`crate::harness::Timings::redecide_after_skip`] a write stands off once a line left
/// went to the worker ([`LeftLine::went`]): a minute as shipped.
const LEFT_WENT_WAITS: u32 = 30;

/// Collapse multi-line text into a single line for a turn write.
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
pub(crate) const WAKE_SETTLE: Duration = Duration::from_secs(30);
/// Each session's own share of the wake's settle, added past [`WAKE_SETTLE`], is below this.
pub(super) const WAKE_SPREAD: Duration = Duration::from_secs(60);

/// How long the session repository's last answer stands for the same done words, no turn seen and
/// no act typed since ([`Session::repo_read`]).
pub(super) const REPO_REPROBE: Duration = Duration::from_secs(20);

/// The reason of the due the wake's settle places.
pub(super) const WOKE: &str = "the Mac woke";

/// The reason of the due a wait's resume takes in a dark wake ([`Session::turn_end_execute`]).
pub(super) const DARK_WAKE: &str =
    "a dark wake between sleeps: the wait's resume is typed once the Mac stays up";

/// How long a wait's resume decided in a dark wake waits before it is decided again, the first
/// time ([`dark_wake_step`]).
pub(crate) const DARK_WAKE_STEP: Duration = Duration::from_secs(20);

/// The longest a wait's resume met in a dark wake waits before it is decided again
/// ([`dark_wake_step`]): a dark wake that turns full with no sleep between (the lid opened during
/// it) is not told to the loop, so this bounds how late the resume comes then.
pub(crate) const DARK_WAKE_CAP: Duration = Duration::from_secs(320);

/// How much of the Mac's time a [`SleepWindow`] fills before it starts a new span: past the 121 s
/// a Mac really sleeping stayed up at the most (2026-10-25), so a wake after a long sleep reads as
/// sleep until it has stayed up this long (v6 §14.17.5.5).
pub(crate) const DARK_WAKE_WINDOW: Duration = Duration::from_secs(300);

/// How long the dark-wake gate holds a wait's resume before its next decision lets it through, the
/// Mac's sleep counted (v6 §14.17.5.5): the outage ladder's own cap. Nothing has measured a Mac
/// kept running while it is idle, the state a resume falls due in; it may sleep more than it runs
/// and still run the turn, so the gate's hold ends here, at the cost of one cut turn an hour on a
/// Mac really sleeping. The decision comes on the awake clock, up to one [`DARK_WAKE_CAP`] of it
/// later. The wake's settle comes before this gate: a Mac that re-arms it at every wait never
/// reaches the gate, and waits for a full wake.
pub(crate) const DARK_WAKE_LONGEST: Duration = Duration::from_secs(3600);

/// HOW THE MAC HAS SPENT THE LAST FEW MINUTES (v6 §14.17.5.5), from the loop's own clocks: the wall
/// clock runs on through a sleep and the awake clock does not, so what the first gains on the
/// second is the sleep the kernel counted ([`Session::watch_sleep`]). It takes no read of its own.
/// A forward step of the wall clock reads as sleep, and holds a resume at most until the window
/// has filled again. It holds the span filling and the last full one. A span closes past one
/// [`DARK_WAKE_WINDOW`], and one long sleep can close it at any length, which is read whole until
/// the next span closes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SleepWindow {
    /// The span filling, awake and asleep.
    filling: (Duration, Duration),
    /// The last full span, awake and asleep.
    full: (Duration, Duration),
}

impl SleepWindow {
    /// `awake` and `asleep` more of the Mac's time.
    pub(crate) fn add(&mut self, awake: Duration, asleep: Duration) {
        self.filling.0 = self.filling.0.saturating_add(awake);
        self.filling.1 = self.filling.1.saturating_add(asleep);
        if self.filling.0.saturating_add(self.filling.1) >= DARK_WAKE_WINDOW {
            self.full = std::mem::take(&mut self.filling);
        }
    }

    /// Whether the Mac slept more of the window than it ran. An empty window has no sleep to show,
    /// so a loop just started counts as awake, as a host that cannot tell does.
    pub(crate) fn mostly_asleep(&self) -> bool {
        self.filling.1.saturating_add(self.full.1) > self.filling.0.saturating_add(self.full.0)
    }
}

/// WHETHER THE DARK-WAKE GATE HOLDS AN ACT OF `rule` (v6 §14.17.5, slice 12a's G6, and §14.17.5.5):
/// a wait's resume ([`is_wait_resume`]) where the host reads macOS's dark wake (`full_wake` says
/// `Some(false)`) and the Mac is `sleeping` between such wakes, not running through them. A host
/// that cannot tell (`None`) counts as awake. The wake is read only for a wait's resume, so no
/// other act's books carry the read.
pub(crate) fn dark_wake_holds(
    rule: Option<&str>,
    full_wake: impl FnOnce() -> Option<bool>,
    sleeping: impl FnOnce() -> bool,
) -> bool {
    rule.is_some_and(is_wait_resume) && full_wake() == Some(false) && sleeping()
}

/// THE STEP OF THE `gates`-TH DARK-WAKE GATE IN A ROW (v6 §14.17.5, slice 12a's G6):
/// [`DARK_WAKE_STEP`] doubling at each gate to [`DARK_WAKE_CAP`], so a Mac that goes on sleeping
/// between its dark wakes wakes the loop a few times an hour, not three times a minute.
#[must_use]
pub(crate) fn dark_wake_step(gates: u32) -> Duration {
    DARK_WAKE_STEP
        .checked_mul(1u32.checked_shl(gates).unwrap_or(u32::MAX))
        .map_or(DARK_WAKE_CAP, |d| d.min(DARK_WAKE_CAP))
}

/// The reason of the due an act at a point waits on while a switch of the session's word settles
/// ([`Session::word_moved`]): the word read when it has stood decides the act.
pub(super) const WORD_SETTLES: &str = "the session's Autopilot word settles";

/// Why the point is decided again after the hour's typing budget held its act.
pub(super) const BUDGET_FREES: &str = "the hour's typing budget (continue_per_hour) frees";

/// Hang detector timeout after the worker's input queue was last seen unread on a screen
/// ([`Session::input_unread`]): input no write of this loop left, which no [`Custody`] holds.
pub(super) const UNDRAWN_INPUT_HANG: Duration = Duration::from_secs(60);

/// A LINE THIS LOOP WROTE AND GAVE UP ON UNDRAWN, HELD IN CUSTODY (ATERM-KI-1101-3): formed where a
/// fenced write's echo hung or its screen never moved after the write, on a read that shows none of
/// it ([`Session::shows_none_of`]; [`Session::type_fenced`], [`Session::probe_type`]), and kept as
/// one record per session until the line settles. The screen cannot tell a line the worker
/// dropped from one it still holds, and writing again on one it holds doubles it, so while
/// custody stands no loop writer writes ([`Session::custody_gate`], the occupancy guards' draft,
/// the accounts probe's `PendingInput`), the host types nothing into the agent but its upgrade's
/// step once the loop judged the line lost (`IdlePoint::line_held`, at an idle point and at a
/// break), and no point's act is booked: it stays owed. A held line raises nothing: it is a
/// journal record, never a badge or an ask.
///
/// What the custody decides is the pure core's ([`custody::Line`]), which this holds with who
/// wrote the line and the rule it ran under; the session's custody methods gather the reads,
/// `status` words, the host's answers and the clock for it, and do what it decides. It settles
/// only by evidence ([`Session::custody_look`]): its draw (whole, in part, or with other words
/// joined to it), handed to the [`LeftLine`] and [`OwnLine`] it was written as; a person's keys
/// drawn after it (in order, their text came after the line); a turn read running after it
/// ([`custody::Line::ran`]), once `status` reads the input clear on a read that shows none of it
/// ([`Session::custody_turned`]; a read that cannot say what the composer holds keeps the turn's
/// evidence for the next that can); or its program's end, a restart's or any other: the host names
/// another process than the one it was written to ([`custody::Line::ended`]). A move of the
/// screen, a new point, an epoch, a stall's thaw, an edge, an Off park, or a busy spell with no
/// turn read running in it (a break's reads, a frame being drawn) settles nothing. A line the
/// worker read ([`custody::Held::Read`]) and drew none of [`ECHO_HANG`] on, at rest, gets a
/// restart in place of the process it was written to ([`Restart::InputLost`]): the PTY holds no
/// copy of it, and the process that might ends, so the point resumed is decided and written once.
/// The host's word that it made the restart is made only where the restart's own report, else the
/// host, then names another process, or none at either end ([`custody::Restarts::answered`]);
/// with the line's own process named, nothing shows it ended, and it is asked again on its
/// back-off ([`Session::restart_agent`]); a restart that stands asked so, or deferred, is spent
/// once the line settles ([`custody::Restarts::settled`]). Where no restart can
/// be made, where the host refused one, and after one already was, it is a doubling wait that
/// writes nothing ([`custody::custody_step`]), and the host's upgrade may move the process
/// ([`LineHeld::Lost`]). A break of the agent's own background work is no point: a person's
/// keys, its program's end and a turn after it settle a line there too, but its draw is kept as
/// evidence ([`custody::Line::drawn`]) for the point the spell ends on to hand back, or to settle
/// though its composer has emptied since, and no restart is made at one
/// ([`Session::custody_at_break`]).
#[derive(Debug, Clone)]
pub(super) struct Custody {
    /// The line and what the custody knows of it.
    line: custody::Line,
    /// Who wrote it: what its draw hands back as its [`OwnLine`], though the worker's turn has
    /// taken the loop's own line since ([`Session::left_own`] names it).
    by: OwnBy,
    /// The rule the write ran under, for its records.
    rule: String,
    /// The ask of this line's restart in place, the same at its every ask
    /// ([`Restart::InputLost`]'s `line`).
    ask: custody::LineAsk,
}

impl Custody {
    /// The line `text` held since `now`, for a test.
    #[cfg(test)]
    pub(super) fn of(text: &str, now: Instant) -> Self {
        Self {
            line: custody::Line::formed(text, 0, None, None, None, now),
            by: OwnBy::Line,
            rule: String::new(),
            ask: custody::LineAsk { at_ms: 0, nth: 0 },
        }
    }

    /// Whether `shown` (a draft a read saw) is the line held.
    pub(super) fn is(&self, shown: &str) -> bool {
        self.line.is(shown)
    }

    /// The line as its writer's own ([`OwnLine`]), for its draw to hand back.
    fn own(&self) -> OwnLine {
        OwnLine {
            text: self.line.text().to_string(),
            by: self.by.clone(),
            human: Some(self.line.human()),
            at: None,
        }
    }
}

/// The reason of the due a line held in [`Custody`] stands on ([`Session::custody_rearm`]).
pub(super) const CUSTODY_DUE: &str = "the line held is looked at again";

/// The reason of the due a write stands on where the line held was drawn on the read it was
/// decided on ([`Session::custody_gate`]).
const CUSTODY_DRAWN: &str = "the line held was drawn";

/// The reason of the due a point judged without its task record stands on, its read lost
/// ([`Session::record_unread`]).
pub(super) const RECORD_UNREAD: &str = "the task record's read was lost";

/// The reason of the due a point decided to do nothing stands on where the host's record read it
/// as nobody's under a conversation on screen ([`Session::record_contradicted`]).
pub(super) const RECORD_TRAILS: &str = "the conversation's record is read again";

/// Why no restart in place is made for a line held in [`Custody`] where the loop runs with no
/// host to make one, as its `WAITING` records say.
pub(super) const NO_HOST: &str = "no restart in place (no host)";

/// Why no restart in place is made for a line held in [`Custody`] under `[harness] relaunch =
/// false`, as its `WAITING` records say.
pub(super) const RELAUNCH_OFF: &str = "no restart in place (relaunch is off)";

/// Whether the `tries`-th deferred restart attempt is journaled.
const fn restart_note_due(tries: u32) -> bool {
    tries < 4 || tries.is_multiple_of(6)
}

/// The composer's caret row as `reader` reads it (`None`: no composer).
fn caret_row(reader: &dyn ScreenReader, rows: &[String]) -> Option<usize> {
    reader.composer(rows).map(|(caret, _)| caret)
}

/// What a read after a fenced write shows of its text ([`echo_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Echo {
    /// The composer holds the text whole, however it wrapped.
    Whole,
    /// The echo is on its way.
    Arriving,
    /// Anything else: another text, no composer, a prompt.
    Other,
}

/// What [`Session::await_echo`] came to: the read the Enter is fenced on, or the read the text was
/// left on and why.
#[derive(Debug)]
pub(super) enum Echoed {
    Whole(Screen),
    Not(Screen, &'static str),
}

/// What deciding a point again came to ([`Session::turn_end_again`]).
#[derive(Debug)]
enum Again {
    /// The point still showing, and its decision.
    Decided(Box<Turn>, TurnEndAction),
    /// The screen read for it is another point: its key moved, or the model a limit wall binds to
    /// ([`Session::point_model_moved`]). Nothing is decided on that read.
    Moved,
    /// Not decided here: the session is watched from behind, a hold sends it to the next look, or
    /// a line left went or holds the point.
    Not,
}

/// What a fenced write came to ([`Session::type_fenced`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Written {
    /// Written and submitted.
    Submitted,
    /// Nothing written.
    Not,
    /// Written, not submitted ([`Session::left_typed`]).
    Left,
}

/// A person keyed since the write.
const ECHO_PERSON: &str = "the original person count changed or is unavailable";
/// The composer shows something the text never begins as.
const ECHO_OTHER: &str = "the composer does not show it as written";
/// The echo did not finish within [`ECHO_HANG`].
pub(super) const ECHO_HUNG: &str =
    "the composer had not shown it whole a minute after it was written";

/// What `screen` shows of `text` written into the composer `reader` reads.
pub(super) fn echo_of(reader: &dyn ScreenReader, screen: &Screen, text: &str) -> Echo {
    if reader.prompt(&screen.rows).is_some() {
        return Echo::Other;
    }
    let Some((_, lines)) = reader.composer(&screen.rows) else {
        return if reader.frame_torn(&screen.rows) {
            Echo::Arriving
        } else {
            Echo::Other
        };
    };
    if !typed_draft(reader, screen) {
        return Echo::Arriving;
    }
    let shown = squashed(&lines.concat());
    let want = squashed(text);
    if shown == want {
        Echo::Whole
    } else if want.starts_with(&shown) {
        Echo::Arriving
    } else {
        Echo::Other
    }
}

/// WHAT A GLYPH'S CELL HAS TOLD OF A DRAFT THE LOOP'S READS FOUND ([`Session::draft_seen`],
/// [`Session::cell_saw`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DraftCell {
    /// No read has asked it: the rows found this text, and a cell's first word may yet agree.
    Unasked,
    /// A read asked and it could not say, and no read has said typed since.
    Unsaid,
    /// A read said typed.
    Typed,
}

/// WHAT A PROMPT-HISTORY SEARCH HAS SHOWN IN THE COMPOSER ([`Session::searched`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Searched {
    /// The search stands (opened where the reader reads its own row, `ScreenReader::history_search`,
    /// the cursor under the composer, `screen::cursor_under_composer`, which keeps it open): what
    /// the composer held at the last read before it opened, where a read said it typed, by its
    /// glyph's cell or by the rows and the cursor (`None` where none did: nothing, a grey
    /// suggestion read with the cursor home, or an old prompt a search left) and the text its row
    /// last showed.
    Open {
        before: Option<String>,
        shown: String,
    },
    /// The search went, its row's last text left in the composer, standing unedited: other text
    /// than the composer held before it, so the old prompt it found (`Diversion::Recall`).
    Left(String),
}

impl Searched {
    /// What a read of a drawn composer showing `shown` makes of what the search showed before
    /// (`was`, `None` where nothing stood), the search standing at this read or not (`stands`: the
    /// cursor under the composer, `screen::cursor_under_composer`, with the record already open or
    /// the reader reading the search's own row, `ScreenReader::history_search`); `last` is what the
    /// composer held at the read before where a read said it typed (`Session::draft_seen`, unless
    /// its cell could not say), passed where the search opens.
    pub(super) fn seen(
        was: Option<Self>,
        shown: &str,
        stands: bool,
        last: Option<String>,
    ) -> Option<Self> {
        match (was, stands) {
            (Some(Self::Open { before, .. }), true) => Some(Self::Open {
                before,
                shown: shown.to_string(),
            }),
            (was, true) => Some(Self::Open {
                before: last.filter(|text| was != Some(Self::Left(text.clone()))),
                shown: shown.to_string(),
            }),
            (
                Some(Self::Open {
                    before,
                    shown: found,
                }),
                false,
            ) => (found == shown && before.as_deref() != Some(shown)).then_some(Self::Left(found)),
            (Some(Self::Left(found)), false) => (found == shown).then_some(Self::Left(found)),
            (None, false) => None,
        }
    }
}

/// A line this loop wrote and did not submit ([`Session::left_line_look`]).
#[derive(Debug, Clone)]
pub(super) struct LeftLine {
    text: String,
    /// Review key of the point the line was written at.
    point_key: String,
    /// What its composer last showed of it.
    stands: LeftStands,
    /// The person's key count as it was left.
    human: Option<u64>,
}

/// A LINE THIS LOOP WROTE THAT STANDS UNSENT, KEPT AS ITS OWN ([`Session::left_line_look`]): the
/// text and who wrote it ([`OwnBy`]), until the loop writes again or a turn runs (the accounts
/// probe's, until the probe ends). Drawn late, it is no person's draft ([`Session::person_at`]), so
/// no person's grace waits on it; and the Enter a later decision gives it is its writer's: an act's
/// booked as typed, so an instruction its program drew past the echo's hang counts as given once
/// and is never typed a second time, and the probe's `/usage` its own.
#[derive(Debug, Clone)]
pub(super) struct OwnLine {
    text: String,
    by: OwnBy,
    /// The window's gesture count the write that left it was fenced on (`if-human=`): while the
    /// window's count still says it, no person's gesture came since, and a command equal to the
    /// line is the loop's to send ([`Self::unkeyed`], v6 §14.17.5.8), where the window's typing
    /// stamp says nothing.
    human: Option<u64>,
    /// When the loop found the line left (its clock): a person's typing the window stamps after
    /// it ([`Session::typed_since`]) makes a command equal to the line theirs, and no other
    /// gesture does (a scroll or a click is no typing). Typing before it that left the line as
    /// written changed nothing of it.
    at: Option<Instant>,
}

/// Who wrote a line this loop left ([`OwnLine`]), and so whose its Enter is.
#[derive(Debug, Clone)]
pub(super) enum OwnBy {
    /// A point's act: the Enter a later decision gives the line is that act's.
    Act(TurnEndAction),
    /// The accounts probe's `/usage` ([`Session::probe_type`]): its Enter is the probe's own
    /// ([`Session::probe_left_enter`]), no act of the policy's, and the line is the loop's only
    /// while the probe that wrote it lasts ([`Session::probe_closed`]).
    Probe,
    /// Any other write's line (Cruise's carry at a break, a door's fact): its Enter is the
    /// policy's draft step's, as before, and a door's line is settled by the facts' look.
    Line,
}

impl OwnLine {
    /// The line `text` that `by` left by a write fenced on the gesture count `human`, found left
    /// at `at`, for a test.
    #[cfg(test)]
    pub(super) fn of(text: &str, by: OwnBy, human: Option<u64>) -> Self {
        Self {
            text: text.to_string(),
            by,
            human,
            at: None,
        }
    }

    /// [`Self::of`] found left at `at`, for a test.
    #[cfg(test)]
    pub(super) fn at(mut self, at: Instant) -> Self {
        self.at = Some(at);
        self
    }

    /// Whether no person has typed since the write that left this line: what the window's typing
    /// stamp says of the span since [`Self::at`] (`typed`: [`Session::typed_since`]) where it
    /// says, else the window's gesture count `now` is the one the write was fenced on (no gesture
    /// at all since). A host that says neither proves nothing.
    pub(super) fn unkeyed(&self, now: Option<u64>, typed: Option<bool>) -> bool {
        match typed {
            Some(typed) => !typed,
            None => self.human.is_some() && self.human == now,
        }
    }

    /// Whether `shown` (a composer's text, or a draft a read saw) is this line, whitespace aside.
    pub(super) fn is(&self, shown: &str) -> bool {
        custody::same_line(&self.text, shown)
    }

    /// Whether `shown` holds this line whole with other words joined to it (a person's keys landed
    /// before its Enter), whitespace aside: neither the line nor a draft of the person's own.
    pub(super) fn joined_in(&self, shown: &str) -> bool {
        custody::joined_line(&self.text, shown)
    }
}

impl LeftLine {
    /// WHETHER THE LINE WENT TO THE WORKER, by an Enter the loop could not confirm having landed or
    /// another writer's, read where its composer shows no text: text that stood is gone with no key
    /// of a person's since, as only a person or the program clears a composer. The host counts no
    /// Enter it could not deliver whole, and the point's key does not move until a turn is drawn. A
    /// person's key says nothing either way, and any decision after one waits out their grace, by
    /// which a turn shows.
    fn went(&self, screen: &Screen) -> bool {
        self.stands != LeftStands::Nothing && self.human.is_some() && self.human == screen.human_seq
    }
}

/// Whether `screen` has not moved since a write at content seq `at` on the grid of epoch `epoch`.
pub(super) fn unmoved_since(screen: &Screen, at: u64, epoch: Option<u64>) -> bool {
    screen
        .generation
        .as_deref()
        .and_then(ScreenGen::parse)
        .is_some_and(|now| Some(now.epoch) == epoch && now.seq <= at)
}

/// A wall the host's measure of the API's reach answers ([`aterm_phase::ApiCause`]).
const fn answered_by_the_network(kind: aterm_phase::WallKind) -> bool {
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

/// THE TURN AT `r` ENDED ON A REPLY (v6 §14.17.5.4, slice 12c): idle, no wall, no interrupt, its
/// reply drawn, after work the loop saw (a gap's zero is none). Only the shape:
/// [`Session::answered_at`] asks the positive evidence that the API answered before the host
/// books it as a witness (`IdleHost::answered`).
fn answered_turn(r: &TurnEndReading) -> bool {
    r.phase == Phase::Idle
        && r.wall.is_none()
        && !r.interrupted
        && r.said_tail.is_some()
        && r.worked.is_some_and(|w| !w.is_zero())
}

/// A dated reply's age without converting a wall timestamp through a possibly younger boot
/// clock. Missing, future or unrepresentable dates supply no positive witness.
pub(super) fn answered_age(now_ms: i64, at: Option<i64>) -> Option<Duration> {
    let at_ms = at?.checked_mul(1000)?;
    let age_ms = u64::try_from(now_ms.checked_sub(at_ms)?).ok()?;
    Some(Duration::from_millis(age_ms))
}

/// The walls a usage window's reset lifts.
const fn resumes_at_reset(kind: aterm_phase::WallKind) -> bool {
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
    let mut h = aterm_hash::Fnv1a64::new();
    h.write(sid.as_bytes());
    h.write(&reset_unix.to_le_bytes());
    Duration::from_secs(h.finish() % span_s)
}

/// The standing rules of `policy` ([`RULES_CAP`]).
#[must_use]
pub fn standing_rules(policy: &crate::supervise::SupervisorConfig) -> Option<String> {
    let path = policy.rules_file.as_ref()?;
    standing_rules_in(&crate::harness::ai_pool::read_bounded_text(
        path,
        1024 * 1024,
    )?)
}

/// [`standing_rules`] of the rules file's text `text`, read by the caller.
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
    /// `t` on the loop's clock as unix seconds.
    pub(super) fn unix_of(&self, t: Instant) -> i64 {
        let (now_ms, ahead) = self.ms_ahead(t);
        nearest_s(now_ms, ahead)
    }

    /// `t` on the loop's clock as unix milliseconds ([`Self::unix_of`] to the millisecond).
    pub(super) fn unix_ms_of(&self, t: Instant) -> i64 {
        let (now_ms, ahead) = self.ms_ahead(t);
        now_ms.saturating_add(ahead)
    }

    /// The wall clock now (unix milliseconds) and how many milliseconds `t` lies ahead of it.
    fn ms_ahead(&self, t: Instant) -> (i64, i64) {
        let now = self.fx.mono();
        let ms = |d: Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
        let ahead = if t >= now { ms(t - now) } else { -ms(now - t) };
        (self.now_unix_ms(), ahead)
    }

    /// Stand `at` (on the loop's clock) as the policy's deadline at the point still showing
    /// ([`Self::turn_end_due`]).
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
            put_off: None,
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
        let ahead = self.settle_ahead(self.fx.mono());
        if ahead.is_none() {
            self.wake_settle = None;
        }
        ahead
    }

    /// When the wake's settle falls, while it is still ahead of `now`: a settle run out holds
    /// nothing, wherever the loop reads it, though only [`Self::settling`] drops it.
    pub(super) fn settle_ahead(&self, now: Instant) -> Option<Instant> {
        self.wake_settle
            .as_ref()
            .map(|(settle, _)| self.falls_at(settle))
            .filter(|&at| now < at)
    }

    /// `t` on the loop's clock as a UTC stamp, for the journal (a `t` already past stamps now).
    pub(super) fn stamp_in(&self, t: Instant) -> String {
        self.stamp_at(t.max(self.fx.mono()))
    }

    /// A historical event's own UTC stamp, without moving a past event to the current read.
    pub(super) fn stamp_at(&self, t: Instant) -> String {
        let secs = self.unix_of(t);
        utc_stamp(u64::try_from(secs).unwrap_or(0))
    }

    /// `unix` (epoch seconds) on the loop's clock ([`Self::now_unix_ms`]).
    pub(super) fn instant_of(&self, unix: i64) -> Instant {
        self.instant_of_ms(unix.saturating_mul(1000))
    }

    /// WHETHER THE CREDITS HOLD STANDS FOR A TURN THE HOST WOULD TYPE on `rows`.
    pub(super) fn credits_hold_here(&self, cfg: &SupervisorConfig, rows: &[String]) -> bool {
        let reader = self.reader(rows);
        let read = self.credits_carry(reader.credits(rows).as_ref());
        self.turn_end
            .credits_hold_stands(read.as_ref(), reader.program(), cfg, self.fx.mono())
    }

    /// When the credits hold that stands for a turn the host would type on `rows` lifts, in unix
    /// seconds ([`Self::credits_hold_here`]): `Some(None)` where the loop knows no such time,
    /// `None` where no hold stands.
    pub(super) fn credits_hold_lifts_here(
        &self,
        cfg: &SupervisorConfig,
        rows: &[String],
    ) -> Option<Option<i64>> {
        let reader = self.reader(rows);
        let read = self.credits_carry(reader.credits(rows).as_ref());
        self.turn_end
            .credits_hold_lifts(read.as_ref(), reader.program(), cfg, self.fx.mono())
            .map(|lifts| lifts.map(|at| self.unix_of(at)))
    }

    /// Usage credits carry the session ([`Self::note_credits`]).
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

    /// The model switch an earlier loop left open.
    pub(super) fn carried_model_switch(&self, open: approvals::OpenSwitch) -> ModelSwitch {
        ModelSwitch {
            from: open.from,
            to: open.to,
            back_at: open.back_at_unix.map(|unix| self.instant_of(unix)),
        }
    }

    /// THE MODEL SWITCH AN EARLIER LOOP MADE ([`approvals::open_model_switch`]).
    #[cfg(test)]
    pub(super) fn seed_model_switch(&mut self, path: &std::path::Path) {
        let body = self.fx.read_text(path).unwrap_or_default();
        self.seed_model_switch_in(&body, None);
    }

    /// `Self::seed_model_switch` over the ledger's text: the switch the ledger shows made and not
    /// yet taken back, dated by its row's `made=`, or, where the loop reads the journal beside a
    /// ledger an older build left with no point rows (`journal`), by the earlier of that and the
    /// second the journal's restart notes show the claim that made it taken
    /// ([`journaled_fallback_made`]). A move still waiting is restored from the journal alone, by
    /// its claim ([`Self::seed_open_move_in`]).
    pub(super) fn seed_model_switch_in(&mut self, body: &str, journal: Option<&str>) {
        let Some(open) = approvals::open_model_switch_in(body, self.setup.sid.as_deref()) else {
            return;
        };
        let made_at = journal
            .and_then(|j| journaled_fallback_made(j, self.setup.sid.as_deref()))
            .map_or(open.made_unix, |m| m.min(open.made_unix));
        let asked = open.asked;
        let switch = self.carried_model_switch(open);
        self.turn_end.seed_model(switch);
        if !asked {
            self.turn_end.seed_fallback_unasked(made_at);
        }
    }

    /// THE RUNS OF THE MOVES AN EARLIER LOOP LEFT OPEN, restored from the journal beside the
    /// ledger (`journal`, [`journaled_open_moves`]): the run in the slot and the one set aside
    /// beside an account move's (`Session::restart_aside`), each a move's run that outlives the
    /// loop's acts ([`RestartRun::move_outlives_acts`]): a model's move onto its fallback or back,
    /// or an account move, in a run that held a live claim of the conversation's record
    /// ([`RestartRun::claimed`]), or an account move ahead put off to a quiet moment, whatever it
    /// held. Each comes back holding what its notes name, so it outlives this loop's acts as it
    /// outlived the earlier loop's (`Self::run_outlives_act`), and the move asked here is asked as
    /// the earlier loop asked it: a claimed move meets the record its ask claimed and landed a
    /// relaunch for (`relaunch::its_own`), carried while in flight, made once carried on in the
    /// asker (`relaunch::already_made`), never restarted over, and is dated by that claim; an
    /// account move's record is never its own, so the move ahead keeps its ask with no other
    /// effect. Only their back-off starts again. A move whose last note held no claim and is no
    /// account move ahead leaves no run: its next ask here is a run of this loop's, as it would have
    /// been in the earlier loop after any act. The floor moves to their seconds, so no new run is
    /// minted at them.
    pub(super) fn seed_open_move_in(&mut self, journal: &str) {
        let (run, aside) = journaled_open_moves(journal, self.setup.sid.as_deref());
        for open in run.iter().chain(aside.iter()) {
            self.restart_floor = self.restart_floor.max(open.at);
        }
        if run.is_some() {
            self.restart_run = run;
        }
        if aside.is_some() {
            self.restart_aside = aside.map(|set| (set, 0));
        }
    }

    /// [`standing_rules`] of the loop's policy.
    fn rules_text(&self, opts: &SuperviseOpts) -> Option<String> {
        let path = opts.policy.rules_file.as_ref()?;
        standing_rules_in(&self.fx.read_text(path)?)
    }

    /// A wall's `reset=` text on the loop's clock ([`Self::refresh_reset`]). `shortly`: the wall
    /// draws Claude Code's own continue at hand ([`limit::continues_shortly`]), so a reset placed
    /// later than [`limit::SHORTLY`] from now is placed there: the bare clock time beside it may be
    /// a day old (a `12am` read late on the day it named, with only a done row of the day before
    /// to place it by, is placed at the coming midnight).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn wall_reset_at(
        &mut self,
        text: Option<&str>,
        kind: aterm_phase::WallKind,
        session_window: bool,
        live: bool,
        answered: bool,
        undated: bool,
        shortly: bool,
        rows: &[String],
    ) -> Option<Instant> {
        let key = remember_reset_key(
            self.wall_reset.as_ref().map(|(key, _)| key.as_str()),
            reset_key(kind, text),
        );
        let text = key
            .split_once('|')
            .map(|(_, text)| text)
            .filter(|text| *text != "-");
        let reprint = (live || answered)
            && !session_window
            && places_resets_on_print(self.reader(rows).program())
            && matches!(
                text.and_then(limit::parse_reset),
                Some(limit::ResetSpec::At { date: None, .. })
            );
        let kept = self
            .wall_reset
            .as_ref()
            .filter(|(k, _)| same_reset(k, &key))
            .map(|(_, u)| *u);
        let unix = match kept {
            Some(u) if !reprint => {
                self.wall_reset = Some((key, u));
                u
            }
            _ => {
                let same = kept.flatten();
                let mut u = self.reset_unix_for(text, session_window, live, answered, rows);
                if undated {
                    u = u.map(|at| self.within_a_session_window(text, at));
                }
                u = u.map(|at| self.on_reset_clock(at));
                let now_unix = self.now_unix();
                if shortly {
                    let soon = i64::try_from(limit::SHORTLY.as_secs())
                        .map_or(now_unix, |s| now_unix.saturating_add(s));
                    u = u.map(|at| at.min(soon));
                }
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

    /// Time the standing wall via the usage tap (`IdleHost::tap_window`) when supported.
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
        if self.fx.replay_lacks_host_door("host.tap_window") {
            self.wall_tap = None;
            return None;
        }
        let tapped = match program {
            Program::Claude => true,
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => false,
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
            Some(tap) if same_reset(&tap.key, &key) => WallTap {
                key: remember_reset_key(Some(&tap.key), key),
                windows,
                ..tap
            },
            _ => WallTap {
                key,
                windows,
                clock: TapClock::new(now),
                at: None,
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

    /// Reconcile the notice's reset and the usage tap's clock ([`timed`]).
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
        let said = self
            .tap_said
            .as_ref()
            .filter(|s| s.key.as_deref().is_none_or(|k| same_reset(k, &tap.key)))
            .map(|s| s.pair);
        let told = t.disagree.filter(|d| said != Some(*d));
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

    /// The reset unix timestamp the standing wall is waited to.
    pub(super) fn wall_reset_unix(&self) -> Option<i64> {
        let (key, notice) = self.wall_reset.as_ref()?;
        self.wall_tap
            .as_ref()
            .filter(|t| t.key == *key)
            .map_or(*notice, |t| t.at)
    }

    /// The reset unix timestamp timed by the usage tap (`tap_unix=`), if any.
    pub(super) fn tap_unix(&self) -> Option<i64> {
        let (key, _) = self.wall_reset.as_ref()?;
        self.wall_tap
            .as_ref()
            .filter(|t| t.key == *key && t.by_tap)
            .and_then(|t| t.at)
    }

    /// Seed `wall_tap` from a prior loop's persisted `tap_unix=` ([`Self::seed_wall_memory`]).
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
            by_tap: true,
            carried: Some(at),
        });
    }

    /// Whether the policy still waits on a limit whose notice left the screen, for the usage tap
    /// to time: one whose reset was named, or a Claude Code session or weekly limit a cut notice
    /// named none of, which the tap may first time after its notice left, unless a login dropped
    /// its reset since ([`TurnEndState::tracks_tapped_limit`]).
    pub(super) fn tap_tracked(&self) -> bool {
        self.turn_end.waits_on_named_reset() || self.turn_end.tracks_tapped_limit()
    }

    /// Poll the usage tap during a standing wait and return `true` if the wall's reset moved.
    pub(super) fn tap_edge(&mut self, opts: &SuperviseOpts, seq: u64) -> bool {
        if self.turn_end_due.is_none() {
            return false;
        }
        opts.idle_host
            .as_ref()
            .is_some_and(|host| self.tap_step(host, seq))
    }

    /// Fold the host's reading of each window the standing wall's tap clock watches into it, and
    /// write the wall's row ([`Self::persist_wall`]): `true` if the wall's reset moved. A replay of
    /// a tape whose recording build had no tap door, or no clock kept, reads nothing.
    fn tap_step(&mut self, host: &Arc<dyn IdleHost>, seq: u64) -> bool {
        if self.fx.replay_lacks_host_door("host.tap_window") {
            return false;
        }
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

    /// Carry the usage tap's reset onto `reading` when the wall's notice has left the screen and
    /// the policy still waits on its limit ([`Self::tap_tracked`]).
    fn tap_left_wall(&self, reading: &mut TurnEndReading) {
        if reading.wall.is_some() || !self.tap_tracked() {
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

    /// Whether the session's login moved off the account its standing limit
    /// was read under ([`TurnEndReading::login_moved`]): at a decision at a
    /// Claude Code usage limit or model bucket (`wall`, the one the reading
    /// shows or the one the policy still waits on with its notice gone), the
    /// account the host reads off the session's profile
    /// ([`IdleHost::login_account`]), the first kept as the limit's
    /// ([`WallLogin`]) and another held as moved until the policy takes it up
    /// ([`Self::login_taken`]). No host, no account read, another program or
    /// a replay that lacks the door is no move.
    pub(super) fn login_moved_at(
        &mut self,
        host: Option<&Arc<dyn IdleHost>>,
        wall: Option<aterm_phase::WallKind>,
        program: Program,
        check: Check,
    ) -> bool {
        let signs_in = match program {
            Program::Claude => true,
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => false,
        };
        if !signs_in || !wall.is_some_and(moves_with_login) {
            return false;
        }
        if !self.turn_end.login_resume_owed()
            && let Some(kept) = self.wall_login.as_mut()
        {
            kept.owed_from = None;
        }
        if check == Check::Decide
            && !self.fx.replay_lacks_host_door("host.login_account")
            && let Some(now) = host.and_then(|h| h.login_account())
        {
            match self.wall_login.as_mut() {
                Some(kept) => kept.moved_to = (now != kept.account).then_some(now),
                None => {
                    self.wall_login = Some(WallLogin {
                        account: now,
                        moved_to: None,
                        owed_from: None,
                    });
                }
            }
        }
        self.wall_login
            .as_ref()
            .is_some_and(|kept| kept.moved_to.is_some())
    }

    /// The policy took the moved login up ([`TurnEndState::login_resume_owed`]):
    /// the moved-to account is the limit's from now on, and the ledger says so.
    pub(super) fn login_taken(&mut self, seq: u64) {
        let Some(kept) = self.wall_login.as_mut() else {
            return;
        };
        let Some(to) = kept.moved_to.take() else {
            return;
        };
        let reason = approvals::login_moved_reason(kept.account, to);
        kept.owed_from.get_or_insert(kept.account);
        kept.account = to;
        self.ledger_row(
            approvals::RULE_LOGIN_MOVED,
            approvals::Outcome::Waiting,
            "",
            &reason,
            seq,
        );
    }

    /// Read the session's profile for a moved login while a limit stands,
    /// waited on or held with nothing decided, every [`LOGIN_LOOK_EVERY`]:
    /// `true` where it names an account other than the limit's and other
    /// than one already held as moved, so the point is decided again at once
    /// and once for each account it names.
    pub(super) fn login_edge(&mut self, opts: &SuperviseOpts) -> bool {
        let stands =
            self.turn_end_due.is_some() || (self.nothing_decided && self.wall_login.is_some());
        if !stands
            || self.fx.replay_lacks_host_door("host.login_account")
            || self
                .login_looked_at
                .is_some_and(|at| self.fx.since(at) < LOGIN_LOOK_EVERY)
        {
            return false;
        }
        let (Some(kept), Some(host)) = (self.wall_login.as_ref(), opts.idle_host.as_ref()) else {
            return false;
        };
        let (account, moved_to) = (kept.account, kept.moved_to);
        self.login_looked_at = Some(self.fx.mono());
        let Some(now) = host.login_account().filter(|now| *now != account) else {
            return false;
        };
        if moved_to == Some(now) {
            return false;
        }
        if let Some(kept) = self.wall_login.as_mut() {
            kept.moved_to = Some(now);
        }
        true
    }

    /// The account the standing limit was read under, for its ledger row
    /// ([`approvals::wall_wait_reason_of`]): the one before a move whose
    /// resume is still owed ([`WallLogin::owed_from`]).
    pub(super) fn wall_login_account(&self) -> Option<AccountKey> {
        self.wall_login.as_ref().map(|kept| match kept.owed_from {
            Some(from) if self.turn_end.login_resume_owed() => from,
            _ => kept.account,
        })
    }

    /// Seed the limit's account from a prior loop's `login=` ([`Self::seed_wall_memory`]).
    pub(super) fn seed_login(&mut self, account: Option<AccountKey>) {
        self.wall_login = account.map(|account| WallLogin {
            account,
            moved_to: None,
            owed_from: None,
        });
    }

    /// `at`, the placement of the reset `text` names, as an undated one is kept
    /// ([`Self::wall_reset_at`], [`limit::SESSION_AHEAD_MAX`]).
    fn within_a_session_window(&mut self, text: Option<&str>, at: i64) -> i64 {
        let now = self.now_unix();
        let local = self.local_offset_s(now);
        text.and_then(limit::parse_reset).map_or(at, |spec| {
            limit::within_a_session_window_local(
                &spec,
                at,
                now,
                &self.local_at(local),
                &self.zone_lookup(),
            )
        })
    }

    /// How long ago a person last typed into the session ([`TurnEndReading::person`]).
    pub(super) fn person_ago(&self, now: Instant) -> Option<Duration> {
        self.person_at()
            .map(|typed| now.saturating_duration_since(typed))
    }

    /// How long before `now` the person's last measured key came ([`TurnEndReading::key`]): the
    /// last `status`'s `human_typed_ms` (`Session::key`), the last read that saw another driver's
    /// hand, none another harness process's own (`Session::hand_key`, kept once the hand lifts),
    /// and the screen's `human_typed_ms` ([`Screen::typed`]): their typing, never a scroll or a
    /// click, and never a draft the loop saw ([`Self::person_at`]'s), which may be the harness's
    /// own words.
    pub(super) fn key_ago(&self, now: Instant, screen: &Screen) -> Option<Duration> {
        self.key
            .into_iter()
            .chain(self.hand_key)
            .map(|typed| now.saturating_duration_since(typed))
            .chain(screen.typed.ago())
            .min()
    }

    /// WHETHER A PERSON TYPED SINCE `at` (THE LOOP'S CLOCK), by the typing [`Self::key_ago`] reads
    /// on `screen`: their key, text or paste, never a scroll or a click (v6 §14.17.5.8). `None`
    /// where no stamp says: a window that reports no typing, or one that served the session for
    /// less than the span with none.
    pub(super) fn typed_since(&self, at: Instant, screen: &Screen) -> Option<bool> {
        let now = self.fx.mono();
        let since = now.saturating_duration_since(at);
        match self.key_ago(now, screen) {
            Some(ago) => Some(ago <= since),
            None => match screen.typed {
                HumanInput::Never => Some(false),
                HumanInput::NeverFor(served) => {
                    (Duration::from_millis(served) >= since).then_some(false)
                }
                HumanInput::Ago(_) | HumanInput::Unknown => None,
            },
        }
    }

    /// Whether no person has typed since `own` was left ([`OwnLine::unkeyed`]), as `screen` says.
    pub(super) fn own_unkeyed(&self, own: &OwnLine, screen: &Screen) -> bool {
        let typed = own.at.and_then(|at| self.typed_since(at, screen));
        own.unkeyed(screen.human_seq, typed)
    }

    /// BOOKS THE PERSON'S HOUR where it runs from the end of the turn their key came in
    /// ([`TurnEndState::take_go_ahead_hour`]), at the point or break `seq` that read it: a loop
    /// after a restart holds it for as long ([`approvals::go_ahead_hour_in`]). Its start is on
    /// the clock the books' rows are stamped by, which that loop's seed reads them by.
    pub(super) fn book_go_ahead_hour(&mut self, seq: u64) {
        let Some(from) = self.turn_end.take_go_ahead_hour() else {
            return;
        };
        let back = self.fx.mono().saturating_duration_since(from);
        let from_ms = self
            .fx
            .stamp_ms()
            .saturating_sub(i64::try_from(back.as_millis()).unwrap_or(i64::MAX));
        self.ledger_row(
            super::super::policy::turn_end::RULE_GO_AHEAD_HOUR,
            approvals::Outcome::Waiting,
            &format!("from={from_ms}"),
            super::super::policy::turn_end::GO_AHEAD_HOUR_BOOKED,
            seq,
        );
    }

    /// When a person last typed into the session ([`Self::person_ago`]): their last key, the draft a
    /// read saw, unless that draft is this loop's own words ([`Self::own_words`]), which are no
    /// person's however late its program draws them, or the read that found a held draft gone
    /// ([`Self::draft_left`]).
    pub(super) fn person_at(&self) -> Option<Instant> {
        let draft = self
            .draft_seen
            .as_ref()
            .filter(|(draft, ..)| !self.own_words(draft))
            .map(|(_, at, _)| *at);
        self.person.max(draft).max(self.draft_left)
    }

    /// WHETHER `shown` (A DRAFT A READ SAW) IS THIS LOOP'S OWN WORDS: the line it wrote and left
    /// ([`OwnLine`]) or holds in [`Custody`], whitespace aside, or a line headed by aterm's own
    /// upgrade words (`upgrade::own_words`). No person's grace waits on them while they stand; the
    /// policy's draft step finishes the loop's own act or asks about it. Text that is only
    /// a part of such a line is not among them: a person's draft can be (a `k`, a `keep going` under
    /// a longer line left, a `[a` before an upgrade's head), and a line still drawing is the echo's
    /// to wait out (`Echo::Arriving`). A person's key still holds what stands (`Session::person`),
    /// and words a person joined to such a line make it none of these ([`OwnLine::joined_in`]).
    fn own_words(&self, shown: &str) -> bool {
        crate::harness::upgrade::own_words(shown) || self.own_line_shown(shown)
    }

    /// Whether `shown` is the line this loop wrote and left or holds in [`Custody`], whitespace aside.
    pub(super) fn own_line_shown(&self, shown: &str) -> bool {
        self.own_line.as_ref().is_some_and(|own| own.is(shown))
            || self.custody.as_ref().is_some_and(|c| c.is(shown))
    }

    /// Return the submit count and conversation mark for `screen` ([`conversation_mark`]).
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

    /// Reread the point after an account relaunch without reporting old screen state to the host.
    fn turn_end_reread_relaunched(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        self.turn_end_reading_as(screen, None, opts, Check::AsDecided, false)
    }

    /// Whether `r` is the initial idle point after an unasked launch with no conversation yet.
    /// The launch is the host's ([`IdleHost::launch`]), or this loop's own: it read the agent
    /// before the agent's first frame ([`super::FirstFrame::saw_start`]).
    fn unasked_launch(&self, r: &TurnEndReading, host: Option<&Arc<dyn IdleHost>>) -> bool {
        (self.first_books || self.books_last_ms.is_some())
            && self.turn_end.point_number().is_none()
            && r.holds_no_conversation()
            && (self.first_frame.saw_start()
                || if self.first_books {
                    host.is_some_and(|h| h.launch().is_some())
                } else {
                    self.launched_since_books(host)
                })
    }

    /// WHETHER THE HOST'S RECORD OF THE CONVERSATION CAN TRAIL THE SESSION'S TASK HERE, so its word
    /// that nobody asked the conversation anything ([`IdleHost::taskless`]) is not asked: at this
    /// loop's first point ([`Session::point_first`]), where the session's task record stands
    /// ([`TurnEndState::managed`], read ahead of the point's reading). A worker's task is its
    /// launch's own prompt, typed by aterm for its manager, and its first reply can end the turn on
    /// screen before Claude Code writes the task's row: 2.1.318 had written its transcript's mode
    /// rows and file-history snapshots, and not the task's row, when the lab stub's instant reply
    /// ended a worker's first turn. At a later point the record's word stands, as for any session:
    /// one that holds no prompt there is a conversation nobody asked (one `/clear` began, a fresh
    /// relaunch).
    pub(super) fn record_trails_task(&self) -> bool {
        self.point_first && self.turn_end.managed()
    }

    /// WHETHER NOBODY HAS ASKED THE CONVERSATION AT `screen` ANYTHING, `r` its reading: the screen
    /// reads it fresh (unless a redraw made it so), the host's record of it says so
    /// ([`IdleHost::taskless`], not asked where it can trail the session's task:
    /// [`Self::record_trails_task`]), the launch was nobody's ([`Self::unasked_launch`]), or the
    /// agent never drew ([`Session::launch_unmet`]). A decision's reading ([`Check::Decide`])
    /// notes whether the host's record says so of a conversation the screen shows and does not
    /// read fresh ([`Session::record_contradicted`]).
    fn taskless_at(
        &mut self,
        r: &TurnEndReading,
        screen: &Screen,
        host: Option<&Arc<dyn IdleHost>>,
        check: Check,
    ) -> bool {
        let fresh = r.taskless
            && !self
                .turn_end
                .redrawn_fresh(screen.submits, screen.inputs_after);
        let record = !fresh && !self.record_trails_task() && host.is_some_and(|h| h.taskless());
        if check == Check::Decide {
            self.record_contradicted = record && !r.holds_no_conversation();
        }
        fresh
            || record
            || self.unasked_launch(r, host)
            || self.launch_unmet(&screen.rows, host.is_some())
    }

    /// Whether the agent process launched after the newest ledger row this loop found.
    fn launched_since_books(&self, host: Option<&Arc<dyn IdleHost>>) -> bool {
        !self.first_books
            && self.books_last_ms.is_some_and(|last| {
                host.and_then(|h| h.launch())
                    .is_some_and(|launched| launched > last)
            })
    }

    /// Seed the usage or model limit drawn above a person's own `/login` dialog
    /// ([`aterm_phase::login_over_limit`]) when neither the reading nor an open ledger row yet
    /// tracks a named limit ([`TurnEndState::waited_limit`]), so a loop that attaches with no
    /// ledger or past [`wall_memory::MEMORY_TTL`] still waits for the reset and closes the dialog
    /// once due.
    fn seed_login_over_limit(
        &mut self,
        rows: &[String],
        program: Program,
        host: Option<&Arc<dyn IdleHost>>,
        seq: u64,
    ) {
        if self.turn_end.waited_limit().is_some()
            || !super::super::policy::login::carried(program)
            || !aterm_phase::login_screen(rows).is_some_and(|screen| {
                super::super::policy::login::over_limit_screen(&self.turn_end, &screen)
            })
        {
            return;
        }
        let Some(over) = aterm_phase::login_over_limit(rows) else {
            return;
        };
        let session_window = over.kind == aterm_phase::WallKind::UsageSession
            && limit::names_a_session_window(&over.message);
        let notice = self.wall_reset_at(
            over.reset.as_deref(),
            over.kind,
            session_window,
            false,
            false,
            false,
            limit::continues_shortly(&over.message),
            rows,
        );
        let reset_at = match self.tap_reset(
            host,
            over.kind,
            &over.message,
            over.reset.as_deref(),
            program,
            Check::Decide,
            seq,
        ) {
            Some(at) => at.map(|unix| self.instant_of(unix)),
            None => notice,
        };
        if let Some(at) = reset_at {
            let unix = self
                .tap_unix()
                .or_else(|| self.wall_reset_unix())
                .unwrap_or(0);
            let jitter = self.setup.sid.as_deref().map_or(Duration::ZERO, |sid| {
                resume_jitter(sid, unix, self.turn_end.timing.reset_jitter)
            });
            self.turn_end.seed_named_reset(
                over.kind,
                &over.message,
                at,
                jitter,
                limit::resumes_by_itself(&over.message),
                limit::ResetClock::of(over.reset.as_deref()),
                self.fx.mono(),
            );
            let conv = self
                .wall_conversation
                .as_ref()
                .filter(|(s, _)| *s == seq)
                .map_or_else(
                    || Conversation::unread(host.is_some(), program),
                    |(_, c)| c.clone(),
                );
            self.turn_end.seed_limit_conversation(conv, Some(program));
            self.remember_wall(&over, program, seq);
            self.scope_limit_conversation(program, host.is_some(), seq);
            self.persist_wall(seq);
        } else {
            self.turn_end
                .seed_unnamed_wall(over.kind, &over.message, self.fx.mono());
        }
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
            self.adopt_stale_wall_on_login_dialog(
                reader.program(),
                &screen.rows,
                opts.idle_host.is_some(),
            );
            self.scope_limit_conversation(reader.program(), opts.idle_host.is_some(), screen.seq);
        }
        let mut reading = reader.read(&screen.rows, Some(screen.cursor_col));
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
        if check == Check::Decide && self.turn_end.reads_opening() {
            self.turn_end.opening_read(reader.opening(&screen.rows));
        }
        let worked = worked.filter(|_| !reader.housekeeping(&screen.rows));
        self.read_left_answer(&mut reading, screen, check);
        self.confirm_wall(&mut reading, screen, opts, check);
        if check == Check::Decide
            && reading.wall.is_none()
            && match reading.program {
                Program::Claude => true,
                Program::Codex
                | Program::Grok
                | Program::Kimi
                | Program::Agy
                | Program::Muse
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
        let reset_at = match &reading.wall {
            Some(w) => {
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
                    .filter(|(at, key)| {
                        !self
                            .wall_answered
                            .as_ref()
                            .is_some_and(|(was, said)| was == at && same_reset(said, key))
                    });
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
                    limit::continues_shortly(&w.message),
                    &screen.rows,
                );
                let notice = match self.standing_until(screen.seq) {
                    Some(until) => {
                        let key = remember_reset_key(
                            self.wall_reset.as_ref().map(|(key, _)| key.as_str()),
                            reset_key(w.kind, text.as_deref()),
                        );
                        self.wall_reset = Some((key, Some(until)));
                        Some(self.instant_of(until))
                    }
                    None => notice,
                };
                if worked.is_some() || answered {
                    self.wall_tap = None;
                    self.wall_login = None;
                    self.agy_pool_blocked = false;
                }
                let host = opts.idle_host.as_ref();
                let program = reading.program;
                let agy_limit = match program {
                    Program::Agy => true,
                    Program::Claude
                    | Program::Codex
                    | Program::Grok
                    | Program::Kimi
                    | Program::Muse
                    | Program::Generic => false,
                };
                let now_unix = self.now_unix();
                let notice = if check == Check::Decide
                    && agy_limit
                    && w.kind.reads_limited()
                    && !self.fx.replay_lacks_host_door("host.limit_until")
                    && let Some(until) = host
                        .and_then(|h| h.limit_until())
                        .and_then(|u| i64::try_from(u).ok())
                {
                    if until > now_unix {
                        self.agy_pool_blocked = true;
                    }
                    if (until > now_unix || self.agy_pool_blocked)
                        && (notice.is_none() || self.wall_reset_unix().is_none_or(|u| until < u))
                    {
                        let key = remember_reset_key(
                            self.wall_reset.as_ref().map(|(key, _)| key.as_str()),
                            reset_key(w.kind, text.as_deref()),
                        );
                        self.wall_reset = Some((key, Some(until)));
                        Some(self.instant_of(until))
                    } else {
                        notice
                    }
                } else {
                    notice
                };
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
            None => {
                if worked.is_some() {
                    self.wall_reset = None;
                    self.reprint_jumps = 0;
                } else if check == Check::Decide {
                    self.seed_login_over_limit(
                        &screen.rows,
                        reading.program,
                        opts.idle_host.as_ref(),
                        screen.seq,
                    );
                }
                // The limit the policy still tracks with its notice gone keeps its tap clock and
                // the account it was met under, so a move of the login before the tap times it is
                // still a move; a decision's read polls the tap as the wait does
                // ([`Self::tap_edge`]), so a reading that came before the wait's next poll times
                // the limit here.
                if worked.is_some() || !(self.tap_tracked() || self.turn_end.login_resume_owed()) {
                    self.wall_tap = None;
                    self.wall_login = None;
                    self.agy_pool_blocked = false;
                }
                if check == Check::Decide
                    && let Some(host) = opts.idle_host.as_ref()
                {
                    self.tap_step(host, screen.seq);
                }
                None
            }
        };
        let login_moved = self.login_moved_at(
            opts.idle_host.as_ref(),
            reading
                .wall
                .as_ref()
                .map(|w| w.kind)
                .or_else(|| self.turn_end.waited_limit().map(|w| w.kind())),
            reading.program,
            check,
        );
        self.date_unread_try(&reading, &screen.rows, screen.seq);
        if reading.wall.is_some() {
            self.persist_wall(screen.seq);
        }
        let measured_down = self.turn_end.retry_track().is_some_and(|(.., down)| down);
        let wall_kind = reading.wall.as_ref().map(|w| w.kind).or_else(|| {
            (worked.is_none() && opts.policy.retry_api_errors)
                .then(|| self.turn_end.standing_retry_wall())
                .flatten()
        });
        let net_wall = wall_kind.is_some_and(|kind| {
            answered_by_the_network(kind)
                || (measured_down
                    && outage_of(kind, true, reading.program)
                        == Some(super::super::policy::turn_end::Outage::Connection))
        });
        // An outage wall asks the host for the witness on its route (v6 §14.17.5.4: another
        // session's turn there finished since the wall's last try clears it), and its wait asks
        // again at each step (`Session::witness_edge`); no other point asks, so it costs nothing
        // while no tab waits. A replay whose recording build had no `host.witness` door asks
        // nothing.
        //
        // A point already decided keeps the witness its decision read (`Check::AsDecided`): the
        // act's journal and its track's latch are the decision's, never a witness that landed
        // while its line was typed.
        if check == Check::Decide {
            self.witness_seen = (wall_kind
                .is_some_and(|kind| outage_of(kind, measured_down, reading.program).is_some())
                && !self.fx.replay_lacks_host_door("host.witness"))
            .then(|| opts.idle_host.as_ref().and_then(|h| h.witness()));
        }
        // A network wall asks the reach (its lease keeps the probe measuring), and so does a wall
        // its track read while the route was measured down, where that makes it the connection's
        // outage (`turn_end::outage_of`: a code-less server error), so its edge up is measured.
        // Every other point peeks, which takes no lease, and asks only on a Down: a measured
        // outage then holds the act the policy would type there (`turn_end::outage_hold`), and
        // its edge (`Session::reach_edge`) decides the point again. A point no measure says is
        // down asks nothing, so no probe runs for it.
        let asked = opts.idle_host.as_ref();
        self.reach_seen = match asked {
            _ if net_wall => Some(asked.map_or(Reach::Unknown, |h| h.reach())),
            Some(h) if self.peeks_down(h.as_ref()) => Some(h.reach()),
            _ => None,
        };
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
        let logins = host.filter(|_| {
            opts.policy.logins
                && match r.program {
                    Program::Claude => true,
                    Program::Codex
                    | Program::Grok
                    | Program::Kimi
                    | Program::Agy
                    | Program::Muse
                    | Program::Generic => false,
                }
        });
        let mut login_label = logins.and_then(|h| h.login_label());
        let login_track = self.turn_end.login_track().is_some();
        // A lost login no roster row names yet, before any `/login` is typed into it: the host's
        // discovery runs once (the upkeep's sync, which adopts the default login's address as a
        // row at once), so a login whose address aterm can read takes the roster row's wait, and
        // its sign-in ladder's, in place of a `/login` only a person could finish.
        let unnamed = r.wall == Some(aterm_phase::WallKind::Auth)
            && self
                .turn_end
                .wall_attempts(aterm_phase::WallKind::Auth)
                .unwrap_or(0)
                == 0;
        if !unnamed && !login_track {
            self.login_discovered = false;
        }
        if check == Check::Decide
            && unnamed
            && login_label.is_none()
            && !self.login_discovered
            && !self.fx.replay_lacks_host_door("host.login_discover")
            && let Some(h) = logins
        {
            self.login_discovered = true;
            h.login_discover();
            login_label = h.login_label();
        }
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
        // A lost login aterm has not typed `/login` into asks what the session's settings make of
        // it (the post-landing review, 2026-11-02): a pinned sign-in method opens the browser at
        // once, so `/login` is then the person's. Asked only there, so no other point reads a
        // file: on screen with no attempt, or tracked with none (a notice that left the screen
        // before its point is decided off its track, `turn_end::still_shown`).
        let login_pin = host
            .filter(|_| {
                super::super::policy::login::carried(r.program)
                    && (unnamed || self.turn_end.login_track() == Some(0))
            })
            .map_or_else(Default::default, |h| h.login_pin());
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
            witness: self.witness_seen.flatten(),
            person: self
                .person_ago(self.fx.mono())
                .into_iter()
                .chain(screen.human.ago())
                .min(),
            key: self.key_ago(self.fx.mono(), screen),
            upgrading: self.limit.is_none() && host.is_some_and(|h| h.owns_turn_end()),
            restartable: host.is_some_and(|h| h.can_restart()),
            resume: host.and_then(|h| h.resume_command()),
            login_label,
            login_superseded,
            login_method,
            login_shared,
            login_pin,
            login_moved,
            taskless: self.taskless_at(&r, screen, host, check),
            finished_record: self
                .turn_end
                .reads_record(&r)
                .then(|| host.and_then(|h| h.finished()))
                .flatten(),
            autopilot: self.autopilot(opts),
            autopilot_by_person: self.autopilot.by_person(),
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
        // An answer's turn that opens above the screen ran no tool only where its whole turn says so.
        let tools_cut = check == Check::Decide
            && self.turn_end.weighs_tools()
            && reader.reads_tool_rows()
            && !reading.ran_tools
            && reader.turn_opens_above(&screen.rows);
        let mut whole_turn = None;
        if check == Check::Decide
            && (reading.said_cut || turn_cut)
            && (super::super::policy::turn_end::says_done(reading.said_tail.as_deref())
                || self.turn_end.reads_turn_whole(&reading, worked.is_some()))
        {
            let read = self.turn_read_whole(screen, reader, true);
            whole_turn = Some(read.clone());
            let turn = read.map(|(rows, _)| rows);
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
                    (reading.turn_said, reading.turn_breaks.said) =
                        needs::joined(reader.turn_messages_before(&rows));
                    reading.said_lines = reader.said_lines(&rows, needs::apart);
                    (reading.turn_lines, reading.turn_breaks.lines) =
                        needs::joined(reader.turn_message_lines_before(&rows, needs::apart));
                    reading.opens_cut = !reader.bounds_messages() && !reader.turn_whole(&rows);
                    if let Some(opened) = reader.turn_opened_by(&rows) {
                        reading.opened_by = Some(super::super::approvals::words_lead(&opened));
                    }
                }
            }
            reading.said_cut |= turn_cut && !turn_read;
        }
        if tools_cut {
            let read = match whole_turn {
                Some(read) => read,
                None => self.turn_read_whole(screen, reader, true),
            };
            // Rows read up to the history's start are all the turn the terminal holds.
            let whole = read.filter(|(rows, start)| *start || !reader.turn_opens_above(rows));
            reading.tools_unread = whole.is_none();
            reading.ran_tools = whole.is_none_or(|(rows, _)| reader.turn_ran_tools(&rows));
        }
        if check == Check::Decide {
            reading.repo = self.repo_read(&reading, host, worked.is_some());
            reading.leftover = self.left_read(&reading, host, worked.is_some());
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
                | Program::Grok
                | Program::Kimi
                | Program::Agy
                | Program::Muse
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

    /// The limit a cancel left ([`TurnEndState::left_answer`]).
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
                    | Program::Grok
                    | Program::Kimi
                    | Program::Agy
                    | Program::Muse
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

    /// Clear cached account advice before starting a new decision round.
    fn account_round(&mut self) {
        self.account_seen = None;
        self.roster_seen = None;
        self.nothing_decided = false;
    }

    /// Report account answers and login notices to the host and apply cached advice to `reading`.
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
            self.tell_account_answer(host, answer, seq);
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
        // A live turn is a look at any line left in the composer (the read is the loop's last).
        if self.facts.state.left_lines().next().is_some()
            && let Some(screen) = self.last.clone().filter(|s| s.seq == seq)
        {
            let at = super::occupancy_loop::LookAt::Live;
            self.left_look_at(&screen, at, cfg, Some(&mut *review));
        }
        self.cruise_carry
            .live(self.turn_end.typed_untaken(), || self.fx.mono());
        // A turn ran after a line held in custody (`custody::Line::took`).
        if let Some(c) = self.custody.as_mut() {
            c.line.took();
        }
        // The agent left its last point, by whatever hand: that point's reach read is spent, and
        // no break of this turn is held on it (`Guard::Reach`).
        self.reach_seen = None;
        self.witness_seen = None;
        self.hold_live(seq, cfg, review);
        if self.turn_end.wake_may_keep_landing()
            && self.stall_host.as_ref().and_then(|h| h.finished()) == Some(true)
        {
            self.turn_end.took_quiet();
            if let Some(host) = self.stall_host.clone()
                && self.turn_end.wake_start_owed()
                && let Some(found) = self.repo_asked(host.as_ref())
            {
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

    /// The ledger's record that the worker worked since the point recorded last
    /// ([`TurnEndState::take_ran`]).
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

    /// Tell the host what answered the move ([`TurnEndState::account_told`]), and book it.
    fn tell_account_answer(&mut self, host: &Arc<dyn IdleHost>, answer: AccountAnswer, seq: u64) {
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

    /// The point folded in, then the host asked ([`Self::observe_point`],
    /// [`Self::account_advise`]).
    pub(super) fn fold_point(
        &mut self,
        r: &mut TurnEndReading,
        now: Instant,
        seq: u64,
        opts: &SuperviseOpts,
    ) {
        let host = opts.idle_host.as_ref();
        self.turn_end
            .relaunch_startup(r, now, || host.is_some_and(|h| h.relaunch_owed()));
        if r.interrupted
            && !self.fx.replay_lacks_host_door("host.carry_owed")
            && host.is_some_and(|h| h.carry_owed())
        {
            self.turn_end.restored_cut();
        }
        if r.phase != Phase::Busy
            && let Some(host) = opts.idle_host.as_ref()
        {
            let current = host.agent_resumed();
            self.turn_end.host_agent_resumed(current);
            // The host may read the process only now (it forgets an exited agent's snapshot): its
            // start is the agent's before the point is observed, so a program begun in the turn
            // is known at its first point. The clock is read only for a start not told before.
            if let Some(started) = current.and_then(|cur| cur.started)
                && self.turn_end.agent_start_unix() != Some(started)
            {
                let at = self.instant_of(started);
                self.turn_end.agent_started(at);
                self.turn_end.agent_started_unix(started);
            }
        }
        let awaited = self.turn_end.account_answer_awaited(r);
        // A TURN UNDER THE MOVE, ANSWERED BEFORE ANY READ SAW IT RUN (the act asked under it, or
        // with no act awaited a prompt someone typed): told before the fold takes the act and
        // the point's prompt, at the point's own instant ([`TurnEndState::account_answer_unseen`]).
        if self.turn_end.account_answer_open(r)
            && let Some(host) = opts.idle_host.as_ref()
            && (opts.policy.accounts != crate::harness::accounts::Mode::Off || opts.policy.logins)
            && let Some(answer) = self.turn_end.account_answer_unseen(r, now)
        {
            self.tell_account_answer(host, answer, seq);
        }
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
        self.tell_face_over(Some(r));
        if awaited {
            self.account_tells(r, seq, opts);
        }
        self.account_advise(r, seq, opts);
    }

    /// [`Self::turn_read_whole`]'s rows.
    fn said_read_whole(
        &mut self,
        screen: &Screen,
        reader: &'static dyn ScreenReader,
        turn: bool,
    ) -> Option<Vec<String>> {
        self.turn_read_whole(screen, reader, turn)
            .map(|(rows, _)| rows)
    }

    /// Read the full grid and scrollback history needed to bound a cut message or turn, and whether the rows run from the history's start (nothing above them).
    pub(super) fn turn_read_whole(
        &mut self,
        screen: &Screen,
        reader: &'static dyn ScreenReader,
        turn: bool,
    ) -> Option<(Vec<String>, bool)> {
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
            return Some((bounded, false));
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
        Some((
            self.rows_bounded_by_cells(joined, reader, abs),
            abs == Some(0),
        ))
    }

    /// Query the host for unpushed repository work when the worker claims to be done or finished.
    fn repo_read(
        &mut self,
        reading: &TurnEndReading,
        host: Option<&Arc<dyn IdleHost>>,
        worked: bool,
    ) -> crate::harness::repo_work::RepoRead {
        use crate::harness::repo_work::RepoRead;
        let tail = reading.said_tail.as_deref();
        if !self.repo_reads(reading) {
            return RepoRead::Clean;
        }
        if !worked && self.turn_end.landing_words_read(tail) {
            return RepoRead::Clean;
        }
        let Some(host) = host else {
            return RepoRead::Clean;
        };
        let mark = said_mark(tail);
        let now = self.fx.mono();
        if let Some((_, _, found)) = self.repo_seen.as_ref().filter(|(m, at, _)| {
            !worked && *m == mark && now.saturating_duration_since(*at) < REPO_REPROBE
        }) {
            return found.clone();
        }
        let Some(found) = self.repo_asked(host.as_ref()) else {
            // Stopped while the probe read: nothing it found is known or journaled.
            return RepoRead::Unread;
        };
        self.repo_unread_unsaid |= found.is_unread();
        self.repo_seen = Some((mark, now, found.clone()));
        found
    }

    /// THE AGENT'S LEFT ROWS AT THIS YIELD ([`IdleHost::leftover`]), read where the repository is
    /// ([`Self::repo_reads`]: a done claim, a finished record, an awaited land check, a Land
    /// go-ahead's park), and kept for the same done words as the repository's answer is, so a turn
    /// that ran since, or the answer to an act typed since, reads them again; not asked at a
    /// landing's own words read again, nor where a tape recorded without the door replays.
    fn left_read(
        &mut self,
        reading: &TurnEndReading,
        host: Option<&Arc<dyn IdleHost>>,
        worked: bool,
    ) -> crate::harness::leftover::LeftRead {
        use crate::harness::leftover::LeftRead;
        let tail = reading.said_tail.as_deref();
        if !self.repo_reads(reading)
            || (!worked && self.turn_end.landing_words_read(tail))
            || self.fx.replay_lacks_host_door("host.leftover")
        {
            return LeftRead::NotAsked;
        }
        let Some(host) = host else {
            return LeftRead::NotAsked;
        };
        let mark = said_mark(tail);
        let now = self.fx.mono();
        if let Some((_, _, read)) = self.left_seen.as_ref().filter(|(m, at, _)| {
            !worked && *m == mark && now.saturating_duration_since(*at) < REPO_REPROBE
        }) {
            return read.clone();
        }
        let read = loop {
            if let Some(read) = host.leftover() {
                break read;
            }
            if self.stopped() {
                // Stopped while the host read: nothing it found is known.
                return LeftRead::Unchecked;
            }
            if self.metadata_context == claim::MetadataContext::Normal {
                self.renew_claim_if_due();
            }
        };
        self.left_seen = Some((mark, now, read.clone()));
        read
    }

    /// WHERE THE HOST'S PROBES ARE READ ([`Self::repo_read`], [`Self::left_read`]): a done claim, a
    /// finished record, an awaited land check, or a reply that parks on Land's go-ahead.
    fn repo_reads(&self, reading: &TurnEndReading) -> bool {
        // The land check's answer is read over the repository at its own yield: a `LANDED`
        // lands nothing a probe finds. Land's go-ahead rides the Repo leg
        // ([`TurnEndState::go_ahead_owed`]): a reply that parks on one is probed as a done claim is.
        let parks = reading.autopilot == Autopilot::Land && reading.go_ahead().is_some();
        super::super::policy::turn_end::says_done(reading.said_tail.as_deref())
            || reading.finished_record == Some(true)
            || self.turn_end.land_check_awaited()
            || parks
    }

    /// THE PROBE ASKED UNTIL IT ANSWERS ([`IdleHost::repo_work`]): while its
    /// reads go on the host answers `None`, and between asks the loop renews its
    /// claim when due, so a probe however long lets no lease lapse; `None` once
    /// the loop is stopped, what the repository holds unknown.
    ///
    /// A disk-free worker's repository reads clean by its task record, unasked:
    /// its trusted folder is the worker folder
    /// ([`crate::harness::disk::fence::is_worker_folder`]), and nothing it does
    /// is landed anywhere, so a `cd` into a checkout holding another session's
    /// work neither holds it nor has Land nudge it to land that work
    /// (`docs/DESIGN-disk-free-ask-2026-10-24.md` §8.4, "Land for a worker"). A credit-claim
    /// worker's reads clean so, by its claim folder
    /// ([`crate::harness::credit_offers::is_claim_folder`]): it lands nothing either.
    fn repo_asked(&mut self, host: &dyn IdleHost) -> Option<crate::harness::repo_work::RepoRead> {
        if self.trusted_cwd.as_deref().is_some_and(|folder| {
            crate::harness::disk::fence::is_worker_folder(folder)
                || crate::harness::credit_offers::is_claim_folder(folder)
        }) {
            return Some(crate::harness::repo_work::RepoRead::Clean);
        }
        loop {
            if let Some(found) = host.repo_work() {
                return Some(found);
            }
            if self.stopped() {
                return None;
            }
            // As before a request ([`Self::call`]): a metadata attempt renews nothing.
            if self.metadata_context == claim::MetadataContext::Normal {
                self.renew_claim_if_due();
            }
        }
    }

    /// Short-turn streak and API-error retry state persisted for subsequent loops
    /// ([`approvals::Carried`]).
    pub(super) fn carried(&self) -> approvals::Carried {
        approvals::Carried {
            short: self.turn_end.short_streak(),
            retry: self
                .turn_end
                .retry_track()
                .map(|(kind, attempts, ups, since, down)| approvals::RetryMark {
                    kind,
                    attempts,
                    ups,
                    since_ms: self.unix_ms_of(since),
                    down,
                    witnessed: self.turn_end.retry_witnessed(),
                }),
            said: self.turn_end.replies(),
            said_reset_ms: self.turn_end.replies_reset().map(|t| self.unix_ms_of(t)),
            checks: self.turn_end.checks(),
            land: self.turn_end.land_books(),
            pass: self.turn_end.wall_pass(),
        }
    }

    /// THE TASK'S FACE ([`TurnEndState::face_moved`]).
    pub(super) fn tell_face(&mut self) {
        self.tell_face_over(None);
    }

    /// [`Self::tell_face`] at a point's fold, `point` its reading: a fresh landing's receipt is
    /// made of it and told before the face ([`Self::receipt_moved`]).
    fn tell_face_over(&mut self, point: Option<&TurnEndReading>) {
        let restated = self.turn_end.restated_noted_ms();
        if let Some(face) = self.turn_end.face_moved() {
            self.receipt_moved(face, restated, point);
            if let Some(host) = &self.stall_host {
                host.task_face(face);
            }
        }
        self.tell_hold();
    }

    /// WHAT ATERM CHECKED AT THE LANDING, as the face moves to `face` (§14.9, slice 9b). A
    /// fresh landing's receipt is made of `point`, the reading the landing was decided on (the
    /// probe's at the claim, the agent's last words; where the land check's `LANDED` landed it,
    /// the defense the check audited, and the receipt says the check was answered), at the
    /// landing's time: the wall read its
    /// `DONE` note used to make (`landing_read_ms`, read here, before the face, and taken by
    /// [`Self::task_edge_with`]), so a landing reads the clock no more often than before. A
    /// landing a loop before this one noted (`restated`) keeps the receipt read with it from the
    /// journal, or none: it is never made of this point's reading, which the probe may have left
    /// unread for the seed's own words (`repo_read`). (Such a landing's face is told at the
    /// loop's start, [`Self::tell_start_face`], so it does not move here; this is the guard.) One
    /// the task's own wake kept tells nothing new. A move to open or theirs drops it, and tells
    /// `None` where one was told. Each is told before the face.
    fn receipt_moved(
        &mut self,
        face: TaskFace,
        restated: Option<i64>,
        point: Option<&TurnEndReading>,
    ) {
        let tell = match face {
            TaskFace::Landed if self.turn_end.landed_by_wake() && restated.is_none() => return,
            TaskFace::Landed => {
                if restated.is_none() {
                    // THE LAND CHECK'S `LANDED` (slice 13a): the receipt says the check was
                    // answered, and quotes the defense that check audited, never the word.
                    let check = self.turn_end.landed_by_check();
                    let built = point.and_then(|r| {
                        let said = if check {
                            self.turn_end.land_audited()
                        } else {
                            r.said_tail.as_deref()
                        };
                        crate::harness::acts::LandReceipt::of(&r.repo, said, 0)
                            .map(|receipt| crate::harness::acts::LandReceipt { check, ..receipt })
                    });
                    self.land_receipt = built.map(|mut receipt| {
                        let ms = self.now_unix_ms();
                        self.landing_read_ms = Some(ms);
                        receipt.at_ms = ms;
                        receipt
                    });
                }
                match &self.land_receipt {
                    Some(receipt) => Some(receipt.clone()),
                    None if self.receipt_told => None,
                    None => return,
                }
            }
            TaskFace::Open | TaskFace::Theirs | TaskFace::Failed => {
                self.land_receipt = None;
                if !self.receipt_told {
                    return;
                }
                None
            }
        };
        self.receipt_told = tell.is_some();
        if let Some(host) = &self.stall_host {
            host.land_receipt(tell.as_ref());
        }
    }

    /// THE TASK'S FACE AT THE LOOP'S START ([`TurnEndState::face_start`]).
    pub(super) fn tell_start_face(&mut self) {
        let face = self.turn_end.face_start();
        match face {
            TaskFace::Landed => {
                if let Some(receipt) = &self.land_receipt {
                    self.receipt_told = true;
                    if let Some(host) = &self.stall_host {
                        host.land_receipt(Some(receipt));
                    }
                }
            }
            TaskFace::Open | TaskFace::Theirs | TaskFace::Failed => {
                self.receipt_moved(face, None, None);
            }
        }
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
        let said = self.turn_end.said_moved(here);
        let held = self.turn_end.held_moved(here);
        let Some(host) = &self.stall_host else {
            return;
        };
        // The words first, then the hold they belong to: where only the words moved (an item
        // the agent reworded within one word family), the hold is told again, equal, so the
        // window reads the new words with it (§14.17 item 1, slice 4b).
        if let Some(said) = &said {
            host.land_said(said.as_ref());
        }
        match (held, said) {
            (Some(held), _) => host.land_held(held),
            (None, Some(_)) => host.land_held(self.turn_end.land_held().filter(|_| here)),
            (None, None) => {}
        }
        self.tell_switch();
    }

    /// THE SWITCH'S ONE ASK, told on a change ([`TurnEndState::switch_moved`]): standing from its
    /// act until its answer is read, whatever word the session holds since.
    fn tell_switch(&mut self) {
        if let Some(asked) = self.turn_end.switch_moved()
            && let Some(host) = &self.stall_host
        {
            host.switch_asked(asked);
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
        let said = self.turn_end.said_ended();
        let held = self.turn_end.held_ended();
        if let Some(host) = &self.stall_host {
            if said {
                host.land_said(None);
            }
            if held {
                host.land_held(None);
            }
        }
        if self.turn_end.switch_ended()
            && let Some(host) = &self.stall_host
        {
            host.switch_asked(false);
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

    /// Whether the composer holds a PERSON'S draft, judged where it is DRAWN: the cell of its
    /// first glyph, or of the glyph after it where the program draws its caret over the first
    /// ([`crate::supervise::screen::composer_glyph_cell`]). Where the program draws a grey
    /// suggestion (`placeholder`), that cell is read WHEREVER THE CURSOR STANDS, however many rows
    /// the text has. A plain glyph is typed text. A grey one is a suggestion only with the cursor
    /// home on it ([`cursor_home`]); with the cursor anywhere else it says neither, so this answers
    /// `None`: that is a frame read before its program placed the cursor (Muse Code's bottom pane,
    /// Claude Code's launch frame, `fabric_world`'s `174640090a`), or a person's own text drawn grey
    /// (Claude Code 2.1.268 draws a voice transcript still forming `dimColor`, the cursor after it,
    /// read from its source). No read tells those apart, and the two wrong acts are an Enter on a
    /// suggestion and a line typed over a person's words, so nothing is done until the frame says
    /// more: [`Composer::Unknown`] holds the point, the wait's own steps look again
    /// (`Session::wait_for_next`), and the host's guards hold ([`Self::cell_saw`]). A `None` is
    /// likewise an unread cell, or a reply inside an unfinished frame. A key the window measured
    /// holds the point whatever this reads (the policy's person).
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
        // THE CURSOR UNDER THE COMPOSER, where only the prompt-history search leaves it: its row
        // shows the search's match, nothing, or a suggestion, none of them a draft Enter would send
        // or an empty composer to type into, so no read says what stands until the search goes (the
        // policy reads a search whose label it reads as `Composer::Diverted`).
        if screen.cursor_under(*caret) {
            return None;
        }
        if lines.first().is_none_or(String::is_empty) {
            return Some(lines.iter().skip(1).any(|l| !l.is_empty()));
        }
        if !placeholder {
            return Some(true);
        }
        let row = (screen.first + caret).to_string();
        let cell = crate::supervise::screen::composer_glyph_cell(|col| {
            self.call(&["cell", &row, &col.to_string()])
                .ok()
                .filter(|r| r.ok())
                .map(|r| r.stdout)
        })?;
        match crate::supervise::screen::cell_is_placeholder(&cell, hint_fg)? {
            false => Some(true),
            true if screen_cursor_home(*caret, lines, screen) => Some(false),
            true => None,
        }
    }

    /// [`Self::composer_draft`] of `reading`'s composer, and what its glyph's cell told is what the
    /// loop saw ([`Self::cell_saw`]): the read a decision makes ([`Self::composer_read`]) and a break
    /// of the agent's own background work (`Session::host_steps_in_background`), where what the
    /// host's guards hold ([`Self::occupancy_guards`]) is no decision's to set. With the cursor under
    /// the composer (the prompt-history search's, `screen::cursor_under_composer`) the composer's
    /// row is the search's, no text of the composer's, so this read records nothing of it: what the
    /// screen's own read made of its rows stands
    /// (`Session::see_draft`, which lets a draft seen go where the row shows other text, marking its
    /// leaving where a hold stood on it). Else the search's match, or the empty row under a query
    /// matching nothing, would be a draft seen at this read, holding a grace nobody's key started
    /// and, empty, holding the host's guards after the search goes until text next shows.
    pub(super) fn composer_typed(
        &mut self,
        reading: &aterm_phase::Reading,
        screen: &Screen,
    ) -> Option<bool> {
        let reader = reading.program.reader();
        let typed = self.composer_draft(
            reading.composer.as_ref(),
            reader.placeholder(),
            reader.placeholder_fg(),
            screen,
        );
        if !reading
            .composer
            .as_ref()
            .is_some_and(|(caret, _)| screen.cursor_under(*caret))
        {
            self.cell_saw(reading.composer.as_ref(), typed);
        }
        typed
    }

    /// What the loop reads of the composer's cells on `screen` ([`Self::composer_typed`],
    /// [`Self::menu_colours`]).
    pub(super) fn composer_read(
        &mut self,
        reading: &aterm_phase::Reading,
        screen: &Screen,
    ) -> ComposerRead {
        let typed = self.composer_typed(reading, screen);
        let menu_fg = match reading.program {
            Program::Claude if typed == Some(true) => self.menu_colours(screen),
            Program::Claude
            | Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => None,
        };
        ComposerRead { typed, menu_fg }
    }

    /// WHAT THE GLYPH'S CELL SAW ([`Self::composer_typed`]) of the composer's text, `typed` its verdict.
    /// Typed text is a draft seen (`Session::draft_seen`) from this read on, the same text keeping
    /// its first sight: else the policy, which reads the same cell, would judge a draft no grace
    /// holds, and its draft step would send it. The exception is text the first read to ask could not
    /// say of ([`DraftCell::Unsaid`]): a read saying typed is the first sign of a person's text there,
    /// as a transcript's last word is (`Self::composer_draft`), and its grace starts at that read.
    /// Text the cell reads as a grey suggestion or a hint, the cursor home on it, is no draft, whatever
    /// an earlier read took it for (a read with the cursor past it, or over a row its redraw had not
    /// yet cleared): the draft seen goes, so neither the policy's grace nor the host's guards hold it.
    /// A read that could not say keeps a draft seen and the first sight of one a read has said typed,
    /// and with none seen its text stands as one: the host's guards hold as the door does, which reads
    /// no verdict as a draft (`Session::door_draft`). Whether a draft seen is a person's is
    /// [`Self::person_at`]'s to say.
    fn cell_saw(&mut self, composer: Option<&(usize, Vec<String>)>, typed: Option<bool>) {
        let Some((_, lines)) = composer else {
            return;
        };
        let text = lines.join("\n");
        self.draft_seen = match (typed, self.draft_seen.take()) {
            (Some(true), Some((seen, at, DraftCell::Unasked | DraftCell::Typed)))
                if seen == text =>
            {
                Some((seen, at, DraftCell::Typed))
            }
            (Some(true), _) => Some((text, self.fx.mono(), DraftCell::Typed)),
            (Some(false), _) if !text.is_empty() => None,
            (None, Some((seen, at, DraftCell::Unasked))) if seen == text => {
                Some((seen, at, DraftCell::Unsaid))
            }
            (None, None) => Some((text, self.fx.mono(), DraftCell::Unsaid)),
            (_, seen) => seen,
        };
    }

    /// THE COMPOSER AS THE HOST'S GUARDS ARE TO HOLD IT ([`Self::occupancy_guards`], `draft_seen`) at
    /// a step no decision reads it: [`Self::composer_typed`] of `reading`'s composer on `screen`, once
    /// for a text and a cursor its cell has said of ([`Self::guards_told`]), so a screen ticking over
    /// the same composer asks no cell at each move. A read that could not say is asked again, and so
    /// is a composer whose draft a read has since let go: the text may have left and come back. Grey
    /// text turning plain under the same words and cursor goes unasked until one of them moves, and
    /// these guards hold nothing over it till then: no write rests on them alone, as the door
    /// (`Session::door_draft`) and the upgrade's gate each read the cell at the write.
    pub(super) fn composer_for_guards(&mut self, reading: &aterm_phase::Reading, screen: &Screen) {
        let told = |s: &Self| {
            reading.composer.as_ref().map(|(_, lines)| {
                (
                    lines.join("\n"),
                    screen.cursor_row,
                    screen.cursor_col,
                    s.draft_seen.is_some(),
                )
            })
        };
        if self.guards_told != told(self) && self.composer_typed(reading, screen).is_some() {
            self.guards_told = told(self);
        }
    }

    /// THE LINE THIS LOOP LEFT AT `point`, WRITTEN BY `by` ([`LeftLine`], [`OwnLine`]): looked at
    /// again as its program draws it ([`Self::left_line_look`]), and the loop's own, no person's
    /// draft. A read that cannot say is no text seen yet: the line's draw is still a move. A line
    /// of `text` held in [`Custody`] takes `by` as its writer, so its draw hands the line back as
    /// `by` left it.
    fn left_own(&mut self, point: &Turn, text: &str, by: OwnBy) {
        let last = self.last.clone();
        let human = last.as_ref().and_then(|screen| screen.human_seq);
        let stands = last.and_then(|screen| self.left_stands(&screen, text));
        self.left_line = Some(LeftLine {
            text: text.to_string(),
            point_key: self.review_key(point),
            stands: stands.unwrap_or(LeftStands::Nothing),
            human,
        });
        if let Some(c) = self.custody.as_mut().filter(|c| c.is(text)) {
            c.by = by.clone();
        }
        self.own_line = Some(OwnLine {
            text: text.to_string(),
            by,
            human: point.screen.human_seq,
            at: Some(self.fx.mono()),
        });
    }

    /// A LINE THIS LOOP LEFT, LOOKED AT AGAIN ([`LeftStands`]): under a program whose composer is no
    /// part of the point's key, a line drawn late or cleared by a person is the same point, and only
    /// this look decides it again. It decides when the line stands whole (past the paste guard's hold
    /// where the reader has one) or when typed text that stood is gone. Nothing else decides: a grey
    /// suggestion changing is no composer's text, a line still queued must not be written twice, and
    /// a line that went ([`LeftLine::went`]) is no line cleared; other text coming decides nothing here.
    /// What a decision does is the policy's, the person's
    /// grace first and then its draft step, for whatever the composer holds by then.
    pub(super) fn left_line_look(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let Some(line) = self.left_line.clone() else {
            return Ok(false);
        };
        let Some(now) = self.left_stands(&seen.screen, &line.text) else {
            return Ok(false);
        };
        let guarded = self.reader(&seen.screen.rows).paste_guard();
        match (line.stands, now) {
            (was, LeftStands::Whole) if was != now => {}
            (LeftStands::Whole | LeftStands::Part, LeftStands::Nothing)
                if line.went(&seen.screen) =>
            {
                self.left_line_went(&seen.screen, review);
                return Ok(false);
            }
            (LeftStands::Whole | LeftStands::Part, LeftStands::Nothing) => {}
            _ => {
                self.left_line = Some(LeftLine {
                    stands: now,
                    ..line
                });
                return Ok(false);
            }
        }
        self.left_line = None;
        if guarded && now == LeftStands::Whole {
            self.set_due(self.fx.mono() + PASTE_GUARD_HOLD, "the line left has drawn");
            return Ok(false);
        }
        self.turn_end_now(seen, opts, allow, review)?;
        Ok(true)
    }

    /// THE LINE LEFT WENT TO THE WORKER ([`LeftLine::went`]): no write for now, and no look on from
    /// here. A turn that has shown nothing a good while on is no turn, so the point is decided again
    /// then; how long a worker takes to draw one is unmeasured, so the wait is long against a redraw
    /// and short against a parked session.
    fn left_line_went(&mut self, screen: &Screen, review: &mut dyn Review) {
        let at = self.fx.mono() + self.setup.redecide_after_skip * LEFT_WENT_WAITS;
        if let Some(line) = self.left_line.take() {
            review.note(&format!(
                "SKIPPED seq={} the line left went to the worker: `{}`",
                screen.seq, line.text
            ));
            self.left_went = Some((line.point_key, screen.human_seq, at));
        }
        self.set_due(at, "the line left went to the worker");
    }

    /// [`Self::left_line_went`] for a read that decides on its own, a due's: no wake need have brought
    /// the look first. Whether the line went on `screen`, and is taken if so.
    fn left_line_went_on(&mut self, screen: &Screen, review: &mut dyn Review) -> bool {
        let Some(line) = self.left_line.clone() else {
            return false;
        };
        let went =
            line.went(screen) && self.left_stands(screen, &line.text) == Some(LeftStands::Nothing);
        if went {
            self.left_line_went(screen, review);
        }
        went
    }

    /// Whether a [`Self::left_line_went`] wait still holds `point`: neither elapsed, broken by a
    /// person's key, nor superseded by a new point key. Re-arms its due when a stall hold or edge
    /// cleared [`Self::turn_end_due`].
    fn left_went_holds(&mut self, point: &Turn) -> bool {
        let Some((key, human, until)) = self.left_went.clone() else {
            return false;
        };
        if self.fx.mono() >= until
            || point.screen.human_seq != human
            || self.review_key(point) != key
        {
            self.left_went = None;
            return false;
        }
        self.set_due(until, "the line left went to the worker");
        true
    }

    /// [`LeftStands`] on `screen`, or `None` where this read cannot say: no composer drawn, a dialog
    /// over it, a glyph's cell unread, or grey with the cursor off it ([`Self::composer_draft`]). What
    /// stood stands until a read says: a read with the cursor away from a suggestion is no text gone.
    fn left_stands(&mut self, screen: &Screen, text: &str) -> Option<LeftStands> {
        let reading = self
            .reader(&screen.rows)
            .read(&screen.rows, Some(screen.cursor_col));
        let (_, lines) = reading
            .composer
            .as_ref()
            .filter(|_| reading.prompt.is_none())?;
        let whole = squashed(&lines.concat()) == squashed(text);
        let reader = reading.program.reader();
        let (grey, hint_fg) = (reader.placeholder(), reader.placeholder_fg());
        Some(
            match self.composer_draft(reading.composer.as_ref(), grey, hint_fg, screen)? {
                false => LeftStands::Nothing,
                true if whole => LeftStands::Whole,
                true => LeftStands::Part,
            },
        )
    }

    /// A COMPOSER THAT HELD THE POINT ([`Self::draft_held`]), read again. While it still holds, what
    /// stands is what the hold is on ([`Self::held_text`]). Anything else decides the point again; a
    /// read that found that text gone has left its mark (`Session::see_draft`), so nothing is typed
    /// there for a whole grace, and a draft that comes back in it is read afresh.
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
        let mut r = TurnEndReading::of(&reading, &seen.screen.rows, read, None, None, None);
        self.left_composer(&mut r, &opts.policy, &seen.screen);
        let now = r.composer;
        if now == held || now == Composer::Unknown {
            self.hold_on(&now, true);
            return Ok(false);
        }
        self.draft_held = None;
        if held != Composer::Unknown {
            self.close_box(seen.screen.seq, review)?;
        }
        self.turn_end_now(seen, opts, allow, review)?;
        Ok(true)
    }

    /// The text a read of `composer` holds the point on ([`Self::held_text`]), where it `holds`: the
    /// draft seen, unless it is the loop's own line. A line headed by aterm's upgrade words
    /// ([`Self::own_words`]) is held on like any text: no read tells it from a person's quote of one,
    /// so its leaving marks a grace, a wait of 120 s at most against words typed over a quote. A frame
    /// with no composer drawn says nothing of the text, so what was held stays held until a composer
    /// is read (`Session::see_draft`).
    fn hold_on(&mut self, composer: &Composer, holds: bool) {
        if *composer == Composer::Absent {
            return;
        }
        self.held_text = self
            .draft_seen
            .as_ref()
            .filter(|(text, ..)| holds && !self.own_line_shown(text))
            .map(|(text, _, cell)| (text.clone(), *cell));
    }

    /// AN UNAUTHORITATIVE READ THAT HELD THE POINT ([`Self::unauthoritative_held`]): once the
    /// screen's phase becomes authoritative on the same point (agy's `/` menu closed or its
    /// exit-confirm cleared, say), the point is offered to the host and decided again.
    pub(super) fn unauthoritative_look(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if !self.unauthoritative_held {
            return Ok(false);
        }
        let reader = self.reader(&seen.screen.rows);
        if !reader.phase_authoritative(&seen.screen.rows, &seen.phase) {
            return Ok(false);
        }
        self.unauthoritative_held = false;
        self.decision_owed = true;
        if self.host_steps_here(seen, opts, &mut false, review)? {
            self.probe_shut(seen, ProbeGate::HostWants, None, review)?;
            return Ok(true);
        }
        self.turn_end_now(seen, opts, allow, review)?;
        Ok(true)
    }

    /// Read the foreground colour of each entry's first glyph in a colour-only completion menu.
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

    /// THE DECISION AT A NEW POINT, ITS READ A LOOK FIRST: any line left in the composer settled
    /// on the read the point is judged on ([`Self::left_look_at`]) before the policy decides, so it
    /// decides on the account that read leaves (a line the person cleared released, and free to
    /// ride this point's act); then [`Self::turn_end_at_point`].
    pub(super) fn turn_end_looked(
        &mut self,
        point: &Turn,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<TurnEndAction, Fail> {
        self.probe_seed_claims(&point.screen, review);
        self.custody_look(point, review)?;
        let at = super::occupancy_loop::LookAt::Point;
        self.left_look_at(&point.screen, at, Some(&opts.policy), Some(review));
        self.turn_end_at_point(point, worked, opts)
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
            self.edge_moved = false;
        }
        self.record_contradicted = false;
        self.record_looked_again = false;
        if worked.is_some() {
            self.draft_left = None;
        }
        // A `/login` DIALOG'S ERROR STATE NAMES NO PROGRAM (`aterm_phase::identify` knows Claude
        // Code's dialog by its own words, which the failure lacks): where no `status` named the
        // agent yet, as at a loop's first point, it is read first, so a person's own `/login` left
        // failed over a limit is read as the agent's dialog (`policy::login::over_limit_act`), not
        // as no program's screen held for good.
        if self.program.is_none()
            && matches!(
                aterm_phase::login_screen(&point.screen.rows),
                Some(aterm_phase::LoginScreen::Failed)
            )
        {
            self.program = self.foreground_program()?;
        }
        self.draft_held = None;
        self.unauthoritative_held = false;
        self.stall_withheld = false;
        let point_key = self.review_key(point);
        if let Some(line) = self.left_line.as_mut()
            && line.point_key != point_key
        {
            if line.stands == LeftStands::Nothing {
                line.point_key.clone_from(&point_key);
            } else {
                self.left_line = None;
            }
        }
        if self
            .left_went
            .as_ref()
            .is_some_and(|(k, ..)| k != &point_key)
        {
            self.left_went = None;
        }
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
        // The point is folded under the word it is decided under: a switch a wait noted
        // ([`Self::word_moved`]) is read once it has stood its settle, as the decision reads it
        // ([`Self::decide_to_act_now`]), so Land never lands a turn Cruise then acts on.
        if !self.autopilot.read()
            || self.word_at.is_some_and(|at| self.fx.mono() >= at)
            || worked.is_some() && (self.turn_end.landed() || self.turn_end.landed_seed_armed())
        {
            self.autopilot.begin();
            self.autopilot_read(opts)?;
        }
        // The record ahead of the reading: whether a manager waits on the session is part of
        // what the first point reads (its `taskless`, `Self::record_trails_task`).
        self.point_first = self.turn_end.point_number().is_none();
        let record = self.task_ahead(opts)?;
        let mut r = self.turn_end_reading(&point.screen, worked, opts);
        if !self.fx.replay_lacks_host_door("host.answered")
            && let Some(ago) = self.answered_ago(&r, point.screen.seq, opts)
            && let Some(host) = opts.idle_host.as_ref()
        {
            host.answered(u64::try_from(ago.as_millis()).unwrap_or(u64::MAX));
        }
        self.host_held = r.upgrading || host_withholds(opts);
        let now = self.policy_now();
        self.fold_point(&mut r, now, point.screen.seq, opts);
        self.task_seed(record, opts);
        self.decide_to_act(r, opts, (now, true), &point.screen)
    }

    /// HOW LONG AGO THE API ANSWERED THE TURN AT `r` (point `seq`), BY POSITIVE EVIDENCE (v6
    /// §14.17.5.4, slice 12c): the turn's shape ([`answered_turn`]), no wall the transcript
    /// refused at this point (its words may still be the outage's, [`Self::wall_refused_at`]), and
    /// the program's own word that the model replied. Claude Code's transcript ends on the model's
    /// reply, an assistant entry that is no API error's notice, dated by that entry's own
    /// readable time; a missing, future or unrepresentable date books nothing;
    /// agy's or Muse Code's reply reads as no error ([`aterm_phase::answered::reads_as_error`]),
    /// dated by this read (zero), as their screens date nothing. `None` books no witness: no
    /// transcript, one ending on an error's notice, a prompt or a tool's result, a reply that
    /// reads as an error, or a program whose route is not read.
    fn answered_ago(&self, r: &TurnEndReading, seq: u64, opts: &SuperviseOpts) -> Option<Duration> {
        if !answered_turn(r) || self.wall_refused_at(seq) {
            return None;
        }
        match r.program {
            Program::Claude => {
                let tail = opts.idle_host.as_ref()?.transcript_tail()?;
                let end = super::wall_confirm::transcript_ends_on(&tail);
                (end.last == super::wall_confirm::Entry::Reply)
                    .then(|| answered_age(self.now_unix_ms(), end.at))
                    .flatten()
            }
            Program::Agy | Program::Muse => r
                .said_tail
                .as_deref()
                .filter(|said| !aterm_phase::answered::reads_as_error(said))
                .map(|_| Duration::ZERO),
            Program::Codex | Program::Grok | Program::Kimi | Program::Generic => None,
        }
    }

    /// Infer turn work from a Claude Code done row when no intermediate busy read was seen
    /// ([`act_turn_work`]).
    fn unseen_act_work(&mut self, rows: &[String]) -> Option<Duration> {
        let at = self
            .turn_end
            .untaken_act_typed()
            .or_else(|| self.turn_end.seeded_act_typed())?;
        match self.reader(rows).program() {
            Program::Claude => {}
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => {
                return None;
            }
        }
        let work = done_row_work(rows)?;
        let now = self.now_unix();
        let ended = self.done_row_ended(rows, now)?;
        act_turn_work(
            self.unix_of(at),
            ended,
            work,
            self.turn_asleep(rows).most,
            self.turn_end.timing.min_work,
        )
    }

    /// The sleep of the machine inside the turn Claude Code's done row on `rows` closes.
    pub(super) fn turn_asleep(&mut self, rows: &[String]) -> InTurn {
        if self.sleeps.is_empty() && self.sleep_unknown.is_none() {
            return InTurn::default();
        }
        let Some(work) = done_row_work(rows) else {
            return InTurn::default();
        };
        let now_ms = self.now_unix_ms();
        let now = now_ms.div_euclid(1000);
        let (lo, hi) = self
            .done_row_ended(rows, now)
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

    /// Date an unread limit-try answer from the transcript notice or Claude Code's done row
    /// ([`TurnEndState::date_unread_try`]).
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
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => false,
        };
        let by_done = if claude && done_row_work(rows).is_some_and(|w| w <= TRY_ANSWER_SLACK) {
            self.done_row_ended(rows, now)
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

    /// [`decide_turn_end`] on `r` ([`Self::foreground_program`]): a new point's decision where
    /// `fresh`, which a declared wake's fact may ride (`Self::wake_ride`), and
    /// [`Self::turn_end_again`]'s re-decision of the same point otherwise, which it never rides
    /// (the slice-2 plan's D3). The composer is judged as [`Self::left_composer`] reads it: a
    /// draft the owner's `prompt` left is aterm's own once `prompt` is no longer in force.
    fn decide_to_act(
        &mut self,
        r: TurnEndReading,
        opts: &SuperviseOpts,
        (now, fresh): (Instant, bool),
        judged: &Screen,
    ) -> Result<TurnEndAction, Fail> {
        let mut r = r;
        self.left_composer(&mut r, &opts.policy, judged);
        self.fx.note(super::super::tape::core::NOTE_TURN_END, || {
            format!("{:?}\n{r:?}", self.turn_end)
        });
        let composer = r.composer.clone();
        let authoritative = r.authoritative;
        let mut steps = self.fx.tracing().then(Vec::new);
        let shadow = self.fx.shadow_policy();
        let mut point = shadow.as_ref().map(|_| ShadowPoint::default());
        let (action, limit_draft_holds) =
            self.decide_to_act_now(r, opts, (now, fresh, judged), &mut steps, point.as_mut())?;
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
        let told_draft = limit_draft_holds
            || self
                .exception_now
                .as_ref()
                .and_then(|e| e.notice.as_ref())
                .is_some_and(|n| notice::DRAFT_HOLDS.contains(&n.id));
        let holds = draft_holds(&composer, &action) || told_draft;
        let grace = matches!(&action, TurnEndAction::WaitUntil { why, .. } if why == PERSON_TYPING);
        self.hold_on(&composer, holds || grace);
        self.draft_held = holds.then_some(composer);
        self.unauthoritative_held =
            !authoritative && !self.decided_withheld && matches!(action, TurnEndAction::Nothing);
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

    /// Evaluate the turn-end decision passes and record trace steps when `trace` is `Some`.
    fn decide_to_act_now(
        &mut self,
        mut r: TurnEndReading,
        opts: &SuperviseOpts,
        (now, fresh, judged): (Instant, bool, &Screen),
        trace: &mut Option<Vec<TraceStep>>,
        mut shadow: Option<&mut ShadowPoint>,
    ) -> Result<(TurnEndAction, bool), Fail> {
        self.decided_withheld = host_withholds(opts);
        self.command_why = false;
        self.replies_stopped = self.turn_end.replies_stop(&r, now);
        self.fact_offer(None, None, &TurnEndAction::Nothing);
        if self.decided_withheld {
            // Withheld, no go-ahead goes: its lines are on the menu bar (§15.6).
            self.turn_end.go_ahead_judged(GoAheadVerdict::Other);
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
        let folded = r.autopilot;
        r.autopilot = Autopilot::Cruise;
        let mut action = self.turn_end_decision(&r, &opts.policy, now, "pass:cruise", trace);
        let mut cfg = std::borrow::Cow::Borrowed(&opts.policy);
        if (r.asked().is_some() || r.go_ahead().is_some())
            && session_word_can_decide(&action)
            && let Some(policy) = self.prose_policy(opts)?
        {
            if let Some(point) = shadow.as_deref_mut() {
                point.prose = Some(policy.question_priority);
            }
            cfg = std::borrow::Cow::Owned(policy);
            action = self.turn_end_decision(&r, &cfg, now, "pass:prose-word", trace);
        }
        // A wait Cruise decides on the word last read stands only where no switch is noted: every
        // wait carries that word, at rest and inside the turn ([`Session::busy_wait`]), so a
        // switch made since wakes one ([`Session::word_moved`]). A point with one noted decides on
        // the word read now, never on a stale Land that would turn Cruise's wait into its
        // instruction; one inside the switch's settle waits for it (`WORD_SETTLES`, v6 §14.10).
        let noted = self.word_at;
        r.autopilot = if noted.is_none()
            && matches!(
                action,
                TurnEndAction::Nothing | TurnEndAction::WaitUntil { .. }
            ) {
            self.autopilot(opts)
        } else {
            self.autopilot_read(opts)?
        };
        r.autopilot_by_person = self.autopilot.by_person();
        if noted.is_some_and(|at| self.fx.mono() >= at) {
            self.word_at = None;
        }
        if r.autopilot != Autopilot::Cruise {
            action = self.turn_end_decision(&r, &cfg, now, "pass:autopilot", trace);
        }
        // A SWITCH TO CRUISE NO WAIT NOTED, read first here: made after the turn's last wait, it
        // left the point folded under the word before it, whose books stand landed, and Cruise
        // unlands only books it folds ([`TurnEndState::unlanded`]). Cruise types nothing into
        // them: the switch is noted as a wait notes one ([`Session::word_moved`]), and the word
        // edge folds the point again under Cruise, which opens the record before Cruise types
        // ([`Session::word_edge`]).
        if noted.is_none()
            && folded != Autopilot::Cruise
            && r.autopilot == Autopilot::Cruise
            && self.turn_end.landed()
        {
            self.autopilot.reported(Some(Autopilot::Cruise));
            self.word_at = Some(self.fx.mono() + self.word_settle);
            if let Some(steps) = trace.as_mut() {
                steps.push(TraceStep {
                    check: "word-unseen-by-fold",
                    rule: action.rule_id(),
                    outcome: TraceOutcome::Decided(format!("{:?}", TurnEndAction::Nothing)),
                });
            }
            action = TurnEndAction::Nothing;
        }
        if action.rule_id().is_some() {
            self.program = self.foreground_program()?;
            r.person = self.person_ago(now);
            r.key = self.key_ago(now, judged);
            r.fact = self.fact_for(&action, opts);
            r.wake = if r.fact.is_none() && fresh {
                self.wake_ride(&action, opts, now, judged)
            } else {
                None
            };
            if r.fact.is_none() && r.wake.is_none() {
                r.fact = self.overlap_for(&action, opts);
            }
            action = self.turn_end_decision(&r, &cfg, now, "pass:final", trace);
        }
        action = self.guard_account_execution(action, opts, now);
        // An observed agent's turn end is decided as ever, so an escalation still raises; an act
        // that would type into it is withheld (`observe.rs`), and a withheld act concludes no
        // managed worker's wall.
        let withheld = types_input(&action) && self.observing();
        if withheld {
            if let Some(steps) = trace.as_mut() {
                steps.push(TraceStep {
                    check: "agent-observed",
                    rule: action.rule_id(),
                    outcome: TraceOutcome::Decided(format!("{:?}", TurnEndAction::Nothing)),
                });
            }
            action = TurnEndAction::Nothing;
        }
        // WHETHER THE PERSON'S COMMAND IS WHY NOTHING IS DONE (v6 §14.17.5.8): the same decision on
        // the draft read as a prompt sends it. Asked before the books close the point.
        self.command_why = matches!(action, TurnEndAction::Nothing)
            && matches!(r.composer, Composer::Command(..))
            && matches!(
                decide_turn_end(
                    &self.turn_end,
                    &TurnEndReading {
                        composer: Composer::Typed,
                        ..r.clone()
                    },
                    &cfg,
                    now
                ),
                TurnEndAction::Submit { .. }
            );
        self.fact_offer(r.fact.clone(), r.wake.clone(), &action);
        if matches!(action, TurnEndAction::Nothing) {
            self.turn_end.conclude_repeated(&r, now);
        }
        let limit_draft_holds = (r.autopilot != Autopilot::Off)
            && crate::supervise::policy::exception::draft_holds_limit_switch(
                &self.turn_end,
                &r,
                &cfg,
            );
        self.exception_now = (r.autopilot != Autopilot::Off)
            .then(|| exception_for(&self.turn_end, &r, &cfg, now))
            .flatten();
        // The go-ahead's lines are off the menu bar from the decision that types it, or waits out
        // its back-off, and back with any other (DESIGN-harness-autopilot §15.6).
        let refused = self.exception_now.as_ref().is_some_and(Exception::refuses);
        let verdict = self
            .turn_end
            .go_ahead_verdict(&action, &r, &cfg, refused, now);
        self.turn_end.go_ahead_judged(verdict);
        self.tell_hold();
        if let Some(point) = shadow {
            point.reading = Some(r);
        }
        Ok((action, limit_draft_holds))
    }

    /// Run [`decide_turn_end`] on `r` under `cfg` at `now`.
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

    /// Build a prose-question policy override when the session's `questions` word sets a priority
    /// ([`Self::question_policy`]).
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

    /// The policy's `WaitUntil` ran out on the point still showing, or an edge decides it again.
    pub(super) fn turn_end_now(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.stall_withheld = false;
        self.edge_moved = false;
        let standing = self.turn_end_due.take();
        let decided = self.turn_end_again(seen, opts, review);
        if matches!(&decided, Ok(Again::Moved))
            && standing.as_ref().is_none_or(|due| due.at > self.fx.mono())
        {
            self.edge_moved = true;
        }
        let same = match (&decided, &standing) {
            (Ok(Again::Decided(_, TurnEndAction::WaitUntil { until, wait, .. })), Some(due)) => {
                due.at == *until && due.wait == *wait
            }
            // THE POINT'S SCREEN MOVED AS IT WAS DECIDED (a frame torn mid-redraw, a row or a
            // model drawn for one read): the due stands, so the next look decides the new point
            // there (which clears it, `Self::turn_end_at_point`), or this one again once its
            // screen is back (`Self::edge_moved` when no due had yet run out), which no look of
            // the same point would.
            (Ok(Again::Moved), Some(_)) => true,
            _ => false,
        };
        if same {
            self.turn_end_due = standing;
        } else {
            self.end_told_wait(opts);
        }
        let Again::Decided(turn, action) = decided? else {
            return Ok(());
        };
        let turn = *turn;
        if matches!(turn.phase, Phase::Limited { .. })
            && self.host_steps_here(&turn, opts, &mut false, review)?
        {
            self.probe_shut(&turn, ProbeGate::HostWants, None, review)?;
            return Ok(());
        }
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

    /// [`Self::turn_end_now`]'s decision of the point `seen`, the due already taken.
    fn turn_end_again(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Again, Fail> {
        if self.claim.watching_behind().is_some() {
            return Ok(Again::Not);
        }
        if std::mem::take(&mut self.held) {
            self.rejudge = true;
            return Ok(Again::Not);
        }
        let screen = self.screen()?;
        let mut turn = self.point_of(screen);
        if self.review_key(&turn) != self.review_key(seen) || self.point_model_moved(&turn) {
            return Ok(Again::Moved);
        }
        self.custody_look(&turn, review)?;
        if self.left_line_went_on(&turn.screen, review) || self.left_went_holds(&turn) {
            return Ok(Again::Not);
        }
        self.account_round();
        self.probe_seed_claims(&turn.screen, review);
        let record = self.task_ahead(opts)?;
        let mut r = self.turn_end_reading(&turn.screen, None, opts);
        self.point_as_decided(&mut turn);
        self.host_held = r.upgrading || host_withholds(opts);
        let now = self.policy_now();
        self.fold_point(&mut r, now, turn.screen.seq, opts);
        self.task_seed(record, opts);
        self.probe_shut(&turn, ProbeGate::NotNew, None, review)?;
        // The point read again is a look, as its first read was (`Self::turn_end_looked`).
        let at = super::occupancy_loop::LookAt::Point;
        self.left_look_at(&turn.screen, at, Some(&opts.policy), Some(&mut *review));
        let action = self.decide_to_act(r, opts, (now, false), &turn.screen)?;
        Ok(Again::Decided(Box::new(turn), action))
    }

    /// Identify the conversation's last turn from the host transcript or hashed screen tail.
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

    /// Hash the screen's last words `said` as a fallback turn key ([`Self::last_turn`]).
    fn said_key(said: &str) -> String {
        format!("said:{:016x}", aterm_hash::fnv1a_64(said.as_bytes()))
    }

    /// Record a `skipped` ledger row under [`RULE_LEFT`] for a person's own exchange.
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
        let reason = approvals::left_reason(approvals::LEFT_REASON, self.turn_end.exchange_opens());
        self.ledger_row(RULE_LEFT, approvals::Outcome::Skipped, &key, &reason, seq);
        self.left_keys.push(key);
    }

    /// Record a `skipped` ledger row under [`RULE_LEFT`] for a person's own exchange answered by a
    /// wall.
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
        let reason = approvals::left_reason(
            approvals::LEFT_AT_WALL_REASON,
            self.turn_end.exchange_opens(),
        );
        self.ledger_row(RULE_LEFT, approvals::Outcome::Skipped, &key, &reason, seq);
        self.left_keys.push(key);
    }

    /// Whether the policy's escalation at a point of `phase` is the point's own
    /// ([`Self::escalate_point`]).
    pub(super) const fn escalates_here(&self, phase: &Phase) -> bool {
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

    /// THE TASK'S END NOTE OWED ([`Self::task_note`]).
    pub(super) fn say_task_note(&mut self, review: &mut dyn Review) {
        if let Some((note, ended)) = self.task_note.take() {
            review.note_ended(&note, ended);
            // THE LANDING'S RECEIPT, right after its DONE note (§14.9, slice 9b).
            if let Some((receipt, at_ms)) = self.receipt_note.take() {
                review.note_ended(&receipt, Some(at_ms));
            }
        }
    }

    /// A NEW POINT'S OWN LINE ([`said_mark`]).
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

    /// A STEP BETWEEN A POINT'S FOLD AND ITS OWN LINE.
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

    /// [`Self::task_edge`].
    fn task_edge_with(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
        say: bool,
    ) -> Result<(), Fail> {
        let seq = screen.seq;
        let landing_read = self.landing_read_ms.take();
        let done = self.turn_end.task_done();
        let restates_wake = self.turn_end.take_wake_restated() && done && self.task_done_said;
        if done && (!self.task_done_said || restates_wake) {
            let note = if let Some(ending) = self.turn_end.gave_up() {
                format!(
                    "FAILED seq={seq} rule={RULE_LAND} {} (autopilot land): nothing more is typed \
                     until a person or an orchestrator types",
                    ending.words()
                )
            } else if self.turn_end.left_to_person() {
                let note = format!(
                    "LEFT seq={seq} rule={RULE_LEFT} a person's own exchange (their message, \
                     answered within 10 min): nothing is typed until the next turn ends"
                );
                self.ledger_left(screen, opts, seq);
                self.receipt_note = None;
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
            let ended = if self.turn_end.left_to_person() || self.turn_end.gave_up().is_some() {
                None
            } else if restates_wake {
                self.landed_noted_ms
                    .or_else(|| self.turn_end.restated_noted_ms())
            } else {
                self.turn_end.restated_noted_ms()
            };
            let landing = !self.turn_end.left_to_person() && self.turn_end.gave_up().is_none();
            if landing {
                self.landed_noted_ms =
                    Some(ended.or(landing_read).unwrap_or_else(|| self.now_unix_ms()));
                self.receipt_note = self.land_receipt.as_ref().map(|r| (r.line(seq), r.at_ms));
            }
            self.task_note = Some((note, ended));
            if landing && !restates_wake {
                self.ledger_row(
                    approvals::RULE_TASK_DONE,
                    approvals::Outcome::Waiting,
                    "",
                    approvals::TASK_DONE,
                    seq,
                );
            }
        } else if self.turn_end.left_judged() {
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

    /// Keep execution authority independent of optional account automation.
    fn guard_account_execution(
        &self,
        action: TurnEndAction,
        opts: &SuperviseOpts,
        now: Instant,
    ) -> TurnEndAction {
        let hold = AccountAdvice::Hold { until: now };
        if super::super::policy::turn_end::account_hold(Some(&hold), now, &action).is_none() {
            return action;
        }
        let until = opts
            .idle_host
            .as_ref()
            .and_then(|host| host.account_execution_hold());
        match until {
            Some(until) => super::super::policy::turn_end::account_hold(
                Some(&AccountAdvice::Hold { until }),
                now,
                &action,
            )
            .unwrap_or(action),
            None => action,
        }
    }

    /// Carry out what the policy decided at `point`, rechecking authority for delayed fallbacks.
    pub(super) fn turn_end_execute(
        &mut self,
        point: &Turn,
        action: TurnEndAction,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        // Explicit policy origins take precedence over a generic visible wall.
        let scope = duty::action_scope(
            &action,
            self.reader(&point.screen.rows)
                .wall(&point.screen.rows)
                .is_some(),
            point.phase == Phase::Question,
        );
        let _duty = self.duty_guard(scope);
        let typing = action.rule_id().is_some();
        let action = self.guard_account_execution(action, opts, self.fx.mono());
        let seq = point.screen.seq;
        // The point's read was looked at by the decision that chose `action`
        // (`Self::turn_end_looked`, `Self::turn_end_again`), and nothing was written since: a
        // second look at that read would settle nothing new.
        self.decision_owed = false;
        self.nothing_decided = matches!(action, TurnEndAction::Nothing);
        let withheld = std::mem::take(&mut self.decided_withheld);
        self.say_wall_refused(review);
        self.say_sleep_unknown(seq, review);
        self.say_repo_unread(seq, review);
        let backing_out = matches!(
            action,
            TurnEndAction::Cancel { rule_id } if rule_id == super::super::policy::login::RULE_LOGIN_BACK_OUT
        );
        self.sync_exception(seq, backing_out, review);
        self.task_edge(&point.screen, opts, review)?;
        if self.turn_end.exchange_at_wall() {
            self.ledger_left_at_wall(&point.screen, opts, seq);
        }
        // An act that is no restart ends the restart run and its back-off (`RestartAsk`), save
        // where the run outlives it (`Self::run_outlives_act`): a held line's restart's, a move's
        // while it holds a live claim of the conversation's record, and an account move ahead's;
        // the run set aside goes by what it is and holds alike (`Session::restart_aside`), so a
        // move's set aside holding a live claim stands past an account move's run that ends here.
        if !matches!(action, TurnEndAction::Restart { .. }) {
            if !self.run_outlives_act() {
                let aside = self.restart_aside;
                self.end_restart_run();
                self.restart_aside = aside;
            }
            self.aside_past_act();
        }
        // Any decision but the resume the gate holds ends the hold, and a later wait's hold starts
        // its own hour. The resume itself does not, so one a deferral or unread input puts off
        // past the hour is not held another.
        if self.dark_gates > 0 && !action.rule_id().is_some_and(is_wait_resume) {
            self.dark_gates = 0;
            self.dark_held = Duration::ZERO;
        }
        if action.rule_id().is_some()
            && let Some(at) = self.settling()
        {
            self.settle_said(seq, at, review);
            self.set_due(at, WOKE);
            return Ok(());
        }
        // A WAIT'S RESUME WAITS OUT A DARK WAKE BETWEEN SLEEPS (v6 §14.17.5, slice 12a's G6): there
        // the network comes and goes for seconds and the Mac sleeps again before the turn it
        // starts can run. A Mac that runs through its dark wakes runs the turn (§14.17.5.5): kept
        // running with its lid closed it never reads a full wake, and would wait for ever.
        // A host that cannot tell (`None`) counts as awake.
        // Decided again on a doubling step to a cap ([`dark_wake_step`]).
        if dark_wake_holds(
            action.rule_id(),
            || self.fx.full_wake(),
            || self.sleep_window.mostly_asleep() && self.dark_held < DARK_WAKE_LONGEST,
        ) {
            let at = self.fx.mono() + dark_wake_step(self.dark_gates);
            self.dark_gates = self.dark_gates.saturating_add(1);
            review.note(&format!(
                "WAITING seq={seq} until={} {DARK_WAKE}",
                self.stamp_in(at)
            ));
            self.set_due(at, DARK_WAKE);
            return Ok(());
        }
        if action.rule_id().is_some()
            && let Some(at) = self.word_at.filter(|&at| self.fx.mono() < at)
        {
            self.set_due(at, WORD_SETTLES);
            return Ok(());
        }
        // An act that was not submitted puts the lines its decision answered (the go-ahead's, or
        // Land's instruction first over its park) back on the menu bar
        // (DESIGN-harness-autopilot §15.6).
        let wrote = self.turn_end_write(point, action, withheld, opts, allow, review);
        if typing && self.turn_end.go_ahead_unwritten() {
            wrote?;
            self.tell_hold();
            return self.sync_needs(&point.screen, opts, review);
        }
        wrote
    }

    /// WHETHER THE LOOP'S RESTART RUN (`Self::restart_run`) OUTLIVES AN ACT IT DECIDES THAT IS NO
    /// RESTART ([`Self::turn_end_execute`]), which ends every other run and its back-off: the run
    /// of a lost line's restart ([`Restart::InputLost`]) while a line stands held in [`Custody`],
    /// as a write the custody holds may turn into that restart (`Self::custody_lost`), its
    /// back-off going on; the run of a move ([`Restart::moves`]: a model's move onto its fallback
    /// or back, an account move) while it holds a live claim of the conversation's record
    /// ([`RestartRun::claimed`], [`RestartClaim`]); and the run of an account move ahead
    /// ([`Restart::ACCOUNT_AHEAD`], put off to a quiet moment), whatever it holds
    /// ([`Self::outlives_act`]). A move's run so kept stands until the move is made or refused or
    /// another restart's run takes its place ([`Self::restart_agent`]), save the one an account
    /// move's point goes on with, which acts on its own run while the move's is held
    /// ([`Self::set_aside_run`]), or, a claim's, until an answer that read the record finds the
    /// claim no longer live (its relaunch stopped or failed unmade). The answer that took the claim
    /// is typically a wait that came once the move's ask had claimed the record and landed a
    /// relaunch (`wait:resume`), and while that relaunch is owed its carry-on the policy asks no
    /// move at the turn ends that come (`upgrading`), so the move's next ask must be the same to
    /// meet that record (`relaunch::its_own`): carried while in flight, made once carried on in the
    /// asker (`relaunch::already_made`), never a second relaunch, as a later loop's is
    /// ([`Self::seed_open_move_in`]). A model's move whose run holds no claim ends its run here, so
    /// a later ask that claims the record is another run's. Either way the move is dated by the
    /// second its claim was taken ([`approvals::model_switch_reason`]), so a run kept past the act
    /// dates no move by an ask that claimed nothing. No account move's record is its own, so no
    /// account move's answer holds or takes a claim, and the move ahead's kept run changes no
    /// answer: it keeps the move's back-off, so the move is asked again on its own step (a run
    /// minted afresh at every point would hold the point's wait, and the move's asks, at the
    /// ladder's first step). Any other restart's run ends at the act, a line held or not, its claim
    /// or not: a memory restart asked again after the loop's other acts is a need the process now
    /// running raised, which a record an earlier ask landed there must not answer.
    fn run_outlives_act(&self) -> bool {
        self.restart_run.is_some_and(|run| self.outlives_act(run))
    }

    /// Whether the restart run `run` outlives an act that is no restart
    /// ([`Self::run_outlives_act`]): a held line's restart's while the line stands in custody, a
    /// move's while it holds a live claim, an account move ahead's whatever it holds.
    fn outlives_act(&self, run: RestartRun) -> bool {
        (run.kind == Restart::INPUT_LOST && self.custody.is_some()) || run.move_outlives_acts()
    }

    /// Ends the restart run and its back-off, and the run set aside with it
    /// (`Session::restart_aside`).
    fn end_restart_run(&mut self) {
        self.restart_waits = 0;
        self.restart_run = None;
        self.restart_aside = None;
    }

    /// Drops the run set aside (`Session::restart_aside`) at an act that is no restart, unless it
    /// outlives that act by what it is and holds ([`Self::outlives_act`]).
    fn aside_past_act(&mut self) {
        self.restart_aside = self
            .restart_aside
            .filter(|&(run, _)| self.outlives_act(run));
    }

    /// The run set aside (`Session::restart_aside`) once a run of `word` takes the slot: for a
    /// move onto another account ([`Restart::moves_account`]), the run of another kind standing
    /// there (the restart its point may go on with), with its back-off step and the claim it
    /// holds, else the one already set aside; for any other restart, none, as its run takes the
    /// place of both.
    fn aside_for(&self, word: &str) -> Option<(RestartRun, u32)> {
        if !Restart::moves_account(word) {
            return None;
        }
        self.restart_run
            .filter(|run| run.kind != word)
            .map(|run| (run, self.restart_waits))
            .or(self.restart_aside)
    }

    /// Puts the run set aside (`Session::restart_aside`, left empty) back in the slot for the
    /// restart a put-off account move's point goes on with: the move's run and back-off step,
    /// which [`Self::put_back_run`] restores.
    fn set_aside_run(&mut self) -> (Option<RestartRun>, u32) {
        let aside = self.restart_aside.take();
        let moved = (self.restart_run, self.restart_waits);
        (self.restart_run, self.restart_waits) =
            aside.map_or((None, 0), |(run, waits)| (Some(run), waits));
        moved
    }

    /// Restores the move's run once the restart its point goes on with has acted, that restart's
    /// own run set aside with the back-off step and the claim its answer left it
    /// ([`Self::set_aside_run`]).
    fn put_back_run(&mut self, (run, waits): (Option<RestartRun>, u32)) {
        self.restart_aside = self.restart_run.map(|run| (run, self.restart_waits));
        (self.restart_run, self.restart_waits) = (run, waits);
    }

    /// The write [`Self::turn_end_execute`] carries out for `action` at `point`, past its
    /// deferrals: `withheld`, the host withheld the decision.
    fn turn_end_write(
        &mut self,
        point: &Turn,
        action: TurnEndAction,
        withheld: bool,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        let hold = matches!(action, TurnEndAction::Nothing).then(|| {
            if withheld {
                super::HoldSaid::Point("withheld")
            } else if !review.unattended() {
                super::HoldSaid::Point("attended")
            } else if self.command_why
                && let Some(Composer::Command(form, _)) = self.draft_held
            {
                super::HoldSaid::Command(form)
            } else if self.draft_held == Some(Composer::Unknown) {
                super::HoldSaid::Point("composer-unknown")
            } else {
                super::HoldSaid::Point("policy")
            }
        });
        self.say_hold(seq, hold, review);
        let acts = !matches!(
            action,
            TurnEndAction::Nothing | TurnEndAction::WaitUntil { .. }
        );
        self.cruise_carry.point();
        self.hold_point(&point.screen, acts, Some(&opts.policy), review);
        let (text, rule) = match &action {
            TurnEndAction::Nothing => {
                // A POINT JUDGED WITHOUT ITS TASK RECORD, its read lost ([`Self::task_ahead`]),
                // rests as a session with no record does, and is decided again soon, reading the
                // record again: whether a manager waits on the session can change what the point
                // is (`Self::record_trails_task`), and a screen at rest brings no next point.
                if self.record_unread {
                    self.redecide_soon(RECORD_UNREAD);
                } else if self.record_contradicted && !self.record_looked_again {
                    // A POINT THE HOST'S RECORD READ AS NOBODY'S WHILE ITS SCREEN SHOWS A
                    // CONVERSATION ([`Self::record_contradicted`]) is decided again once, soon:
                    // Claude Code can write a prompt's row after a reply that came at once has
                    // ended the turn on screen, and a screen at rest brings no next point to read
                    // the record again.
                    self.record_looked_again = true;
                    self.redecide_soon(RECORD_TRAILS);
                }
                // LAND RESTS ON A NEED ([`TurnEndState::land_resting`]): said once, quietly; the
                // need is on the menu bar already, and nothing is typed until someone else's turn.
                if self.turn_end.rest_untold() {
                    let need = self
                        .turn_end
                        .standing_needs()
                        .first()
                        .cloned()
                        .unwrap_or_else(|| super::super::policy::needs::UNREAD_NEED.to_string());
                    review.note(&format!(
                        "WAITING seq={seq} rule={} {}: {need}",
                        super::super::policy::land_check::RULE_LAND_CHECK,
                        super::super::policy::land_check::WAITS_ON_YOU
                    ));
                }
                return Ok(());
            }
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
            TurnEndAction::Type { text, rule_id } => (one_turn(text), *rule_id),
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
            TurnEndAction::Confirm { rule_id, .. } | TurnEndAction::Cancel { rule_id } => {
                (String::new(), *rule_id)
            }
        };
        if matches!(
            action,
            TurnEndAction::Type { .. }
                | TurnEndAction::TypeCommand { .. }
                | TurnEndAction::Submit { .. }
        ) && self.left_went_holds(point)
        {
            return Ok(());
        }
        if !self.speaks_to_its_agent(seq, rule, review) {
            return Ok(());
        }
        if self.stall_in_hand(review) {
            self.stall_withheld = true;
            return Ok(());
        }
        // A LINE HELD IN CUSTODY HOLDS EVERY WRITE (ATERM-KI-1101-3), or is lost and restarted.
        match self.custody_gate(point, &action, (rule, opts), review)? {
            CustodyGate::Free => {}
            CustodyGate::Hold => return Ok(()),
            CustodyGate::Lost => return self.custody_lost(point, rule, opts, allow, review),
        }
        if self.input_unread(seq, rule, review) {
            return Ok(());
        }
        if let TurnEndAction::Confirm { rule_id, row } = &action {
            return self.confirm_dialog(point, &action, rule_id, (row, "enter"), opts, review);
        }
        if let TurnEndAction::Cancel { rule_id } = &action {
            let Some(row) = super::super::policy::login::cancel_row(&point.screen.rows).cloned()
            else {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule_id} esc: the judged read draws no dialog row \
                     to cancel at"
                ));
                return Ok(());
            };
            return self.confirm_dialog(point, &action, rule_id, (&row, "esc"), opts, review);
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
        let fact = self.fact_intends(&action, seq);
        let would = self.point_would_owed(&action, &opts.policy, fact.is_some(), review);
        let (action, ride) = self.point_claims(action, &opts.policy, seq, review);
        let text = match &action {
            TurnEndAction::Type { text, .. } if ride.is_none() => one_turn(text),
            _ => text,
        };
        // Left: the text, any fact with it, stands in the composer unsent.
        let mut left = false;
        self.left_line = None;
        // The draft a Submit sends is the loop's own line where it is the text the loop wrote and
        // left ([`OwnLine`]): its Enter is that act's.
        let own = if matches!(action, TurnEndAction::Submit { .. }) {
            self.own_line.take_if(|own| own.is(&text))
        } else {
            None
        };
        let typed = match &action {
            TurnEndAction::Submit { .. } => {
                let submitted = self.submit_draft(point, &text, rule, review);
                if matches!(submitted, Ok(false)) {
                    // Whatever missed, the draft stood: gone with no person's key, it went.
                    self.left_line = Some(LeftLine {
                        text: text.clone(),
                        point_key: self.review_key(point),
                        stands: LeftStands::Whole,
                        human: point.screen.human_seq,
                    });
                    self.own_line.clone_from(&own);
                }
                submitted
            }
            _ => self
                .type_text_written(point, &text, rule, review)
                .map(|written| {
                    left = written == Written::Left;
                    written == Written::Submitted
                }),
        };
        if rule == RULE_LOGIN
            && !matches!(&typed, Ok(true))
            && let Some(host) = opts.idle_host.as_ref()
        {
            host.login_abandon();
        }
        if typed.is_err() {
            // A press that failed on the way leaves the line where it stood, still the loop's own.
            self.own_line.clone_from(&own);
        }
        let typed = typed?;
        if !typed && left {
            // The act and its fact stand in the composer: settled at the next look.
            if let Some(fact) = &fact {
                self.fact_left(fact, seq, review);
            }
            if let Some(ride) = ride {
                self.point_left(ride, seq, review);
            }
            self.left_own(point, &text, OwnBy::Act(action.clone()));
            return Ok(());
        }
        if !typed {
            if let Some(fact) = &fact {
                self.fact_rode(fact, Some("not-written"), seq, review);
            }
            if let Some(ride) = ride {
                self.point_told(ride, Some("not-written"), seq, review);
            }
            return Ok(());
        }
        if matches!(action, TurnEndAction::Submit { .. }) {
            // The draft standing went: a line a door left in it is told.
            let look = super::super::facts::LeftLook::Submitted;
            self.left_look(look, None, seq, Some(&opts.policy), Some(&mut *review));
        }
        // The loop's own line, drawn late and sent now, is booked as the act that wrote it: Land's
        // instruction given, never owed again at the next point.
        let (action, rule) = match own {
            Some(OwnLine {
                by: OwnBy::Act(act),
                ..
            }) => {
                let rule = act.rule_id().unwrap_or(rule);
                (act, rule)
            }
            _ => {
                self.own_line = None;
                (action, rule)
            }
        };
        let r = self.turn_end_reread(&point.screen, opts);
        let land = self.turn_end.land_episode().0;
        if let Some(at) = self.turn_end.witness_resumed(&action, &r) {
            review.note(&crate::harness::witness::witness_note(
                seq,
                rule,
                &self.stamp_at(at),
            ));
        }
        self.turn_end.acted(&action, &r, self.fx.mono());
        // An act submitted: a line lost from here may be restarted for again.
        self.input_lost.submitted();
        // The point's reach read is spent by the act it let go (at the outage's cap, or by an arm
        // that types at any reach): the next decision reads its own (`turn_end_reading_as`), and
        // until then no break of the turn this act opens is held on it (`Guard::Reach`), whose
        // write reads the reach afresh (`Session::write_guards`).
        self.reach_seen = None;
        self.witness_seen = None;
        // So are the host's repository and left reads: the act's answer is a new yield, read
        // again whatever its words, though no turn of it is seen (`Session::repo_read`,
        // `Session::left_read`), so what the agent ended or pushed in that turn holds nothing.
        self.repo_seen = None;
        self.left_seen = None;
        if let Some(fact) = &fact {
            self.fact_rode(fact, None, seq, review);
        }
        if let Some(ride) = ride {
            self.point_told(ride, None, seq, review);
        }
        if let Some(would) = would {
            self.point_would(would, seq, review);
        }
        if !land && self.turn_end.land_episode().0 {
            self.land_report = self.point_said.clone();
        }
        if rule == crate::supervise::policy::land_check::RULE_LAND_CHECK {
            self.land_defense_said = self.point_said.clone();
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

    /// Publish any change in the standing exception ([`exception_for`]). `backing_out`: the
    /// action the turn end carries out at this point is its Esc out of the `/login` it typed
    /// ([`TurnEndAction::Cancel`], `login-back-out@v1`, at the browser hand-off or the dialog's
    /// error), so a login exception that ends here ends by that back-out.
    pub(super) fn sync_exception(&mut self, seq: u64, backing_out: bool, review: &mut dyn Review) {
        use super::super::policy::exception::{LOGIN_BACKED_OUT, LOGIN_BACKING_OUT};
        self.stuck_over(seq, "the screen reached a point", review);
        if self.exception_now.is_none() {
            let cause = self.exception_open.as_ref().and_then(|(open, since)| {
                if open.kind != super::super::policy::outlook::ExceptionKind::Login {
                    None
                } else if self.turn_end.login_backed_out_since(*since) {
                    Some(LOGIN_BACKED_OUT)
                } else {
                    backing_out.then_some(LOGIN_BACKING_OUT)
                }
            });
            self.exception_ended(seq, cause, review);
            return;
        }
        self.raise_exception(seq, review);
    }

    /// Publish the standing exception [`Self::exception_now`] names, once a change: journaled
    /// `EXCEPTION seq=<n> kind=… when=…` and put on the host, ending one of another kind first. A
    /// point's [`Self::sync_exception`] runs it, and so does a break that tells the person now
    /// (a spent go-ahead, [`Self::cruise_carry`]).
    pub(super) fn raise_exception(&mut self, seq: u64, review: &mut dyn Review) {
        let Some(now) = self.exception_now.clone() else {
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
        let held = now
            .held
            .map_or_else(String::new, |arm| format!(" held={}", arm.word()));
        review.note(&format!(
            "EXCEPTION seq={seq} kind={} when={}{held}{notice} {} => {}",
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

    /// The restart the policy asked for, made by the host and journaled `HOST seq=<n>
    /// restart:<why> ask=<ask> claimed=<second|no> step=<word>` (the host's answer,
    /// [`RestartAnswer`]: its word, and what it read of the conversation's record, which sets the
    /// claim the run holds, [`RestartRun::claimed`], by which a model's move is dated). The host's
    /// success word says it made the
    /// restart, and the process the restart's own report names after it
    /// (`IdleHost::restart_left`), else the one the host names now, is the evidence
    /// ([`custody::Line::restarted`]): a line held in [`Custody`] settles with its
    /// program where that is another process than the one it was written to, or where none is
    /// named at either end (the word all there is), and a restart for that line
    /// ([`Restart::InputLost`]) is made only there ([`custody::Restarts::answered`]); with the
    /// line's own process named, nothing shows it ended, the restart stands asked, and it is asked
    /// again on its back-off as a deferral is ([`Self::restart_later`], its step
    /// [`super::super::journal::LINE_RESTART_STANDS`]). Any other restart is booked on the word,
    /// the line held standing where its process is named: such a restart names no process, and
    /// asked again it would end the process a restart it made started.
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
            self.stall_withheld = true;
            return Ok(());
        }
        let measured = self.memory_seen;
        let over_wall = self.memory_over_wall && matches!(why, Restart::Memory);
        let took = self.memory_wall && matches!(why, Restart::Memory);
        if let (Restart::Memory, Some(over)) = (why, measured) {
            self.memory_warn(&point.screen, over, opts, review);
        }
        let asked_unix = match (self.restart_run, self.restart_aside) {
            (Some(run), _) if run.kind == why.word() => run.at,
            (_, Some((run, waits))) if run.kind == why.word() => {
                // The run set aside (`Session::restart_aside`), asked on its own: it takes the
                // slot on its own back-off step, holding the claim it held, in place of the one
                // standing.
                self.restart_aside = None;
                self.restart_run = Some(run);
                self.restart_waits = waits;
                run.at
            }
            _ => {
                // A new run's second: now, or one past the last run's where that is this second
                // or later, so two runs asked in one second are two asks (`RestartAsk::of`).
                // An account move's run sets aside the one standing (`Self::aside_for`); any
                // other restart's run takes the place of it and of the one set aside.
                let at = self.now_unix().max(self.restart_floor.saturating_add(1));
                self.restart_floor = at;
                let aside = self.aside_for(why.word());
                self.end_restart_run();
                self.restart_aside = aside;
                self.restart_run = Some(RestartRun {
                    kind: why.word(),
                    at,
                    claimed: None,
                });
                at
            }
        };
        let ask = RestartAsk::of(why, asked_unix);
        let answer: Option<RestartAnswer> =
            opts.idle_host.as_ref().and_then(|h| h.restart(why, &ask));
        // The claim the run holds, which keeps a move's ask past the loop's other acts
        // (`Self::run_outlives_act`) and dates the move: taken at this answer's second, held as
        // it was (at the run's second where this loop never saw it taken), lapsed where the
        // answer read the record and found no live claim of the run's, and kept as it was where
        // the answer read no record (another sweep's lock, no session record of the asker).
        let was_claimed = self.restart_run.and_then(|run| run.claimed);
        let claimed = match answer.as_ref().map_or(RestartClaim::Unread, |a| a.claim) {
            RestartClaim::Unread => was_claimed,
            RestartClaim::Unclaimed => None,
            RestartClaim::Held => was_claimed.or(Some(asked_unix)),
            RestartClaim::Taken => Some(self.now_unix()),
        };
        if let Some(run) = &mut self.restart_run {
            run.claimed = claimed;
        }
        let step = answer.map(|a| a.step);
        // The second a move made by this answer is dated by (`fallback_made_at`,
        // `approvals::model_switch_reason`): its claim's, else the run's.
        let made_unix = claimed.unwrap_or(asked_unix);
        // The note a later loop reads the run back from (`journaled_open_moves`): at the run's
        // first asks, where its claim changed, and at every answer that ends it (`-`: none), so
        // the journal's last note of a run says what the loop last knew of it.
        if restart_note_due(self.restart_waits)
            || claimed != was_claimed
            || !step.as_deref().is_some_and(defers)
        {
            review.note(&format!(
                "HOST seq={seq} restart:{} ask={} claimed={} step={}",
                why.word(),
                ask.word(),
                claim_word(claimed),
                step.as_deref().unwrap_or("-")
            ));
        }
        let word = step
            .as_deref()
            .map(|s| s.split_once(':').map_or(s, |(head, _)| head));
        match (word, step.as_deref()) {
            (Some("adopted" | "relaunched" | "continued" | "done"), _) => {
                // The word is the host's claim; for a line held in custody the evidence is the
                // process the restart's own report names after it, else the one the host names
                // now (`custody::Line::restarted`): only another one is its program ended, and
                // only that is the line's restart made (`custody::Restarts::answered`).
                let (left, named) = if self.custody.is_some() {
                    let left = opts.idle_host.as_ref().and_then(|h| h.restart_left());
                    (left, self.host_process())
                } else {
                    (None, None)
                };
                let line = self.custody.as_ref().map(|c| &c.line);
                let restarted = if matches!(why, Restart::InputLost { .. }) {
                    self.input_lost.answered(line, left, named)
                } else {
                    line.map_or(custody::Restarted::Ended, |l| l.restarted(left, named))
                };
                let stands = restarted == custody::Restarted::Stands;
                if stands && matches!(why, Restart::InputLost { .. }) {
                    // Nothing shows the process the line was written to ended: the line's restart
                    // stands asked, and is asked again on its back-off, as a deferral's is.
                    let step = format!(
                        "{} ({})",
                        step.as_deref().unwrap_or("-"),
                        super::super::journal::LINE_RESTART_STANDS
                    );
                    return self.restart_later(seq, why, &step, None, review);
                }
                self.end_restart_run();
                self.memory_refused = false;
                if !stands {
                    // The process that might hold a line held in custody ended with this restart.
                    self.custody_ended(seq, Settles::Ended, review);
                }
                let r = if matches!(why, Restart::Account { .. }) {
                    self.turn_end_reread_relaunched(&point.screen, opts)
                } else {
                    self.turn_end_reread(&point.screen, opts)
                };
                // The point's reach read is spent by the restart, as by a typed act
                // (`Self::turn_end_execute`).
                self.reach_seen = None;
                self.witness_seen = None;
                let now = self.fx.mono();
                self.turn_end.restarted(why, true, &r, now);
                if step.as_deref() == Some("done:fresh") {
                    self.turn_end.relaunched_afresh();
                }
                if word == Some("adopted") && point.screen.submits.is_some() {
                    let after = self.read_once().ok().map(|(screen, _)| screen);
                    let at = after.as_ref().and_then(|s| self.counted_conversation(s));
                    self.turn_end.counted_after_harness(at);
                }
                if matches!(why, Restart::Model { .. }) {
                    self.turn_end.fallback_made_at(made_unix);
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
                    Restart::InputLost { .. } => {
                        "the agent, for the line it read and drew none of".to_string()
                    }
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
                    self.wall_login = None;
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
                            made_unix,
                        ),
                        (None, Restart::Account { to, at }) => {
                            approvals::account_switch_reason(to, *at)
                        }
                        (None, Restart::InputLost { .. }) => {
                            super::super::journal::LINE_READ_UNDRAWN.to_string()
                        }
                        (None, _) => "the turn-end policy".to_string(),
                    };
                    self.ledger_row(rule, approvals::Outcome::Typed, &what, &reason, seq);
                }
                self.fallback_asked(unasked, seq);
                Ok(())
            }
            (Some("wait" | "busy"), Some(step)) if Restart::put_off(why.word()) && defers(step) => {
                review.note(&format!(
                    "DEFERRED seq={seq} restart:{} waits for a quiet moment ({step}); the point goes on",
                    why.word()
                ));
                // While the run holds a live claim of the conversation's record, the act the
                // point goes on with, and every act after it, keeps the move's run
                // (`Self::run_outlives_act`), so the move asked again at a later turn end is the
                // same ask (`RestartAsk`). The way back so meets the record its ask claimed
                // (`relaunch::its_own`): carried, or made where its relaunch was carried on in
                // the asker (`relaunch::already_made`), never restarted over. Deferred with no
                // claim held, the act ends the way back's run, and the next ask is another run's.
                // No account move's record is its own, so no account move's answer holds or
                // takes a claim: the move ahead's record is carried in flight whatever its ask
                // (`relaunch::carried_word`), and restarted over once carried on. Its run outlives
                // the acts by its kind, and a restart the account move's point goes on with acts
                // on its own run (`Self::set_aside_run`), the move's held meanwhile, so each backs
                // off on its own step.
                let otherwise = self.ahead_deferred(seq, why, otherwise, opts);
                if !Restart::moves_account(why.word()) {
                    return self.turn_end_execute(point, otherwise, opts, allow, review);
                }
                let moved = self.set_aside_run();
                let acted = self.turn_end_execute(point, otherwise, opts, allow, review);
                self.put_back_run(moved);
                acted?;
                self.put_off_wait(why.word());
                Ok(())
            }
            (Some("wait"), Some(step)) => {
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
                // The run set aside stays for the restart the point goes on with, which takes it
                // back (`Session::restart_aside`); an act that is no restart drops it by its kind.
                self.restart_waits = 0;
                self.restart_run = None;
                if matches!(why, Restart::InputLost { .. }) {
                    // Refused or not answered: the loss waits from here, never asked again
                    // (`Self::custody_lost`).
                    self.input_lost.unmade(step.as_deref());
                }
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
                match self.ahead_deferred(seq, why, otherwise, opts) {
                    TurnEndAction::Escalate { reason } => {
                        let reason = match &step {
                            Some(step) => {
                                format!("the restart could not be made ({step}): {reason}")
                            }
                            None => reason,
                        };
                        self.aside_past_act();
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

    /// The Mac woke ([`Self::watch_sleep`]).
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
            put_off: None,
        };
        self.wake_settle = Some((settle, false));
    }

    /// The wake the watch measured at `wall_now` ([`Self::check_slept`]).
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

    /// `WAITING seq=<seq> until=<the settle> the Mac woke: deciding again` ([`Self::settle_wake`]).
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

    /// The act a move ahead the host deferred or refused falls back to (`otherwise`), the host
    /// asked again: another session here may have taken the row first, and where the session's
    /// own account is another Mac's to hold (`docs/DESIGN-fluid-swarm-2026-10-28.md` F19) the host
    /// now holds it ([`AccountAdvice::Hold`]), or advises again the move the session yields by
    /// (`yields`: the relaunch put it off for a live job, a held tab or a missing socket, or
    /// refused it), so the act waits as a held turn end's does ([`account_hold`]) and nothing is
    /// typed onto that account. Any other advice leaves the act as it stands.
    fn ahead_deferred(
        &mut self,
        seq: u64,
        why: &Restart,
        otherwise: TurnEndAction,
        opts: &SuperviseOpts,
    ) -> TurnEndAction {
        let (
            Restart::Account {
                at: AccountAt::Ahead,
                ..
            },
            Some(host),
        ) = (why, opts.idle_host.as_ref())
        else {
            return otherwise;
        };
        let point = AccountPoint::TurnEnd;
        let advice = host.account(point);
        let advised = advice.as_ref().and_then(AccountAdvice::past_probe);
        let held = account_hold(advised, self.fx.mono(), &otherwise);
        self.account_seen = Some((seq, point, advice));
        held.unwrap_or(otherwise)
    }

    /// The wait a deferred move onto another account's point goes on with (a usage-credits hold,
    /// `held_by_credits`), where it stood one: the move's ([`Due::put_off`]), ending at the move's
    /// back-off step at the latest ([`TurnEndTiming::restart_backoff`]), as
    /// [`Self::restart_later`]'s does. The step is the move's own: a restart the point went on
    /// with acted on its own run ([`Self::set_aside_run`]). A wait the host answers past its rotation
    /// (`wait:conversation-in-other-tab`, `busy:another-sweep`) strikes the row the ask took,
    /// and that strike's news would end the wait and ask the move again at once, R6's loop for a
    /// move ahead. A point that goes on by typing stands no wait, and its next turn end asks.
    fn put_off_wait(&mut self, word: &'static str) {
        let Some(falls) = self.turn_end_due.as_ref().map(|due| self.falls_at(due)) else {
            return;
        };
        let pause = crate::supervise::ladder::Ladder(&self.turn_end.timing.restart_backoff)
            .step(usize::try_from(self.restart_waits).unwrap_or(usize::MAX));
        self.restart_waits = self.restart_waits.saturating_add(1);
        let until = self.fx.mono() + pause;
        if until < falls
            && let Some((why, wait)) = self
                .turn_end_due
                .as_ref()
                .map(|due| (due.why.clone(), due.wait))
        {
            self.set_wait(until, why, wait);
        }
        if let Some(due) = self.turn_end_due.as_mut() {
            due.put_off = Some(word);
        }
    }

    /// A restart that must wait ([`TurnEndTiming::restart_backoff`]); the restart of a line held
    /// ([`Restart::InputLost`]) stands asked meanwhile ([`custody::Restarts::deferred`]).
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
        if matches!(why, Restart::InputLost { .. }) {
            // The line's restart stands asked: spent should the line settle before it is made
            // (`custody::Restarts::settled`), as the host may have made it unseen.
            self.input_lost.deferred();
        }
        let word = why.word();
        let why = format!("restart:{word} waits: {step}");
        if restart_note_due(self.restart_waits.saturating_sub(1)) {
            review.note(&format!(
                "WAITING seq={seq} until={} {why}",
                self.stamp_in(until)
            ));
        }
        self.set_due(until, why);
        if let Some(due) = self.turn_end_due.as_mut() {
            due.put_off = Some(word);
        }
        Ok(())
    }

    /// Whether the session's foreground program is a supervisable agent
    /// ([`aterm_phase::Program::supervisable`]).
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

    /// Type and submit `text` using the server's guarded `turn` verb: how far it went
    /// ([`Written::Left`] where the server typed it and its Enter did not go: the guard skipped
    /// it, a hold landed before it, or the sink took none of the first one:
    /// [`TURN_SUBMIT_BUSY_REPLY`]).
    fn type_guarded(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<Written, Fail> {
        let seq = point.screen.seq;
        if self.caps.guarded_submit == Some(false) {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} this aterm is too old for a guarded submit"
            ));
            return Ok(Written::Not);
        }
        let reader = self.reader(&point.screen.rows);
        let Some(caret) = reader.caret_on(&point.screen.rows) else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} no composer this policy types into"
            ));
            return Ok(Written::Not);
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
        if (!r.ok() && r.err_text().starts_with(TURN_SUBMIT_BUSY_REPLY)) || r.is_err("busy sink") {
            // `turn` answers this only from its first guarded Enter, after the paste and a guard
            // that matched it: the text stands in the composer unsent, as a fenced write's
            // refused Enter leaves it. An aterm before it had this reply answers `ERR busy sink`
            // from any guarded Enter, a re-press included, so there an earlier Enter may have gone.
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} not submitted: {why}"
            ));
            self.left_typed(point, text, &why, review)?;
            return Ok(Written::Left);
        }
        if r.is_err("halted") || r.is_err("busy") || r.is_err("rate") {
            self.write_refused(r.is_err("halted"), &why, seq, review)?;
            return Ok(Written::Not);
        }
        if r.is_err("yield") {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a person is typing: {why}"
            ));
            self.redecide_soon("a person was typing");
            return Ok(Written::Not);
        }
        if r.unknown_form() {
            self.caps.guarded_submit = Some(false);
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} this aterm is too old for a guarded submit: {why}"
            ));
            return Ok(Written::Not);
        }
        self.caps.guarded_submit = Some(true);
        if r.submitted() {
            return Ok(Written::Submitted);
        }
        let verdict = r.turn_verdict().unwrap_or("").to_string();
        if r.pressed_unverified() {
            review.note(&format!(
                "UNVERIFIED seq={seq} rule={rule} Enter written, the submit not seen: {verdict}"
            ));
            return Ok(Written::Submitted);
        }
        let verdict = if verdict.is_empty() { why } else { verdict };
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} not submitted: {verdict}"
        ));
        if verdict.split_whitespace().any(|w| w == "reason=halted") {
            // The hold came after the server pasted the text and before its Enter: the text
            // stands in the composer unsent, as the fenced write's halted Enter leaves it.
            self.park_on_hold(&verdict, seq, review)?;
            self.left_typed(point, text, "the session is halted", review)?;
            return Ok(Written::Left);
        }
        if r.skipped() || verdict.contains("reason=guard") {
            self.left_typed(
                point,
                text,
                &format!("the guard missed ({verdict})"),
                review,
            )?;
            return Ok(Written::Left);
        }
        Ok(Written::Not)
    }

    /// Journal and escalate a fenced write skipped because the guard matched no row on the
    /// unchanged screen.
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

    /// Schedule a re-decision after [`crate::harness::Timings::redecide_after_skip`].
    fn redecide_soon(&mut self, why: &str) {
        self.set_due(self.fx.mono() + self.setup.redecide_after_skip, why);
    }

    /// INPUT THE WORKER HAS NOT READ STANDS BEFORE ANY ACT ([`Self::foreground_program`],
    /// [`Self::redecide_soon`], [`LaunchCall::Again`]).
    ///
    /// Input `status` read unread holds acts on the screen it was last read on until the screen
    /// moves or [`UNDRAWN_INPUT_HANG`] passes, and the point is decided again soon. That input is
    /// no line of this loop's: a line it wrote and gave up on undrawn is held in [`Custody`], which
    /// no move of the screen and no clock lifts ([`Self::custody_gate`]).
    ///
    /// [`LaunchCall::Again`]: super::approval_loop::LaunchCall::Again
    pub(super) fn input_unread(&mut self, seq: u64, rule: &str, review: &mut dyn Review) -> bool {
        let now = self.fx.mono();
        let unread = self
            .launch_status
            .input
            .clone()
            .filter(|w| matches!(w.as_str(), "pending" | "typeahead"));
        let why = if let Some(word) = unread {
            self.input_unread_at = Some((seq, now));
            format!("the worker has not read the input written to it (input={word})")
        } else if self.input_undrawn(seq, now) {
            "the worker has not drawn the input it read (the screen has not moved since it was \
             seen undrawn)"
                .to_string()
        } else {
            self.input_unread_at = None;
            return false;
        };
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} {why}; nothing written"
        ));
        self.redecide_soon("the worker had not drawn the input written to it");
        true
    }

    /// Whether the existing unread-input hold still covers this screen ([`UNDRAWN_INPUT_HANG`]).
    fn input_undrawn(&self, seq: u64, now: Instant) -> bool {
        self.input_unread_at.is_some_and(|(at, since)| {
            at == seq && now.saturating_duration_since(since) < UNDRAWN_INPUT_HANG
        })
    }

    /// A LINE THIS LOOP WROTE, GIVEN UP ON UNDRAWN AT THE READ `point` (`why`), TAKEN INTO
    /// [`Custody`]: `human` the key count its write was fenced on, `at` the content seq the write
    /// left. One observing `status` read ([`Self::call_observed`], so a stall's word it finds
    /// starts no stall look) says what the worker has read of it ([`custody::Held`]); a clear
    /// read by the write's held-text note ([`Self::note_text_held`]) dates its read that early
    /// ([`custody::Line::formed`]). One `SKIPPED` record ([`super::super::journal::LINE_HELD`]),
    /// no badge and no ask, and a due stands for the next look ([`Self::custody_rearm`]). Its
    /// writer is named by [`Self::left_own`]; until then it is the loop's own line
    /// ([`OwnBy::Line`]).
    #[allow(clippy::too_many_arguments)]
    fn custody_formed(
        &mut self,
        point: &Turn,
        text: &str,
        (rule, why): (&str, &str),
        (human, at): (u64, u64),
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let now = self.fx.mono();
        let r = self.call_observed(&["status"])?;
        let word = r
            .ok()
            .then(|| crate::supervise::screen::status_field(&r.stdout, "input"))
            .flatten()
            .map(str::to_string);
        let noted = self
            .held_note
            .take()
            .filter(|(seq, _, said)| *seq == at && said == "clear")
            .map(|(_, read_at, _)| read_at);
        let process = self
            .stall_host
            .as_ref()
            .map(|h| (h.agent_pid(), h.agent_started()));
        let line = custody::Line::formed(text, human, word.as_deref(), noted, process, now);
        let ask = custody::LineAsk {
            at_ms: self.now_unix_ms(),
            nth: self.lines_held,
        };
        self.lines_held = self.lines_held.saturating_add(1);
        let word = word.unwrap_or_else(|| "-".to_string());
        review.note(&format!(
            "SKIPPED seq={} rule={rule} typed, not submitted ({why}): `{}`; {} (input={word})",
            point.screen.seq,
            clip(text),
            super::super::journal::LINE_HELD
        ));
        self.custody = Some(Custody {
            line,
            by: OwnBy::Line,
            rule: rule.to_string(),
            ask,
        });
        self.custody_rearm();
        Ok(())
    }

    /// Whether the read `screen` a write of `text` was given up on shows none of it: no typed text
    /// in its composer (or no composer this read can say of) and no auto-mode setup offer risen
    /// over the keys. Only such a line is taken into [`Custody`]; one drawn in part is a line left
    /// ([`LeftLine`]), and its give-up asks as before.
    fn shows_none_of(&mut self, screen: &Screen, text: &str) -> bool {
        !self.offer_rose(&screen.rows)
            && self
                .left_stands(screen, text)
                .is_none_or(|stands| stands == LeftStands::Nothing)
    }

    /// THE DUE A LINE HELD IN [`Custody`] STANDS ON ([`custody::Line::rearm`]): a read line's
    /// minute from its first clear read ([`ECHO_HANG`], the hang detector), else the next of a
    /// doubling wait ([`custody::custody_step`]). Re-armed at every hold
    /// ([`Self::custody_gate`]), as a stall hold or an edge may have cleared the due since: every
    /// hold leaves one standing.
    fn custody_rearm(&mut self) {
        let now = self.fx.mono();
        let Some(c) = self.custody.as_mut() else {
            return;
        };
        let at = c.line.rearm(now);
        self.set_due(at, CUSTODY_DUE);
    }

    /// WHAT THE READ `screen` SHOWS OF A LINE HELD IN [`Custody`], for the core's look
    /// ([`custody::Shows`]): what its composer holds of it ([`Self::left_stands`], which may read
    /// a glyph's cell), the composer's text, and the person's key count; `None` where no line is
    /// held.
    fn custody_shows(&mut self, screen: &Screen) -> Option<custody::Shows> {
        let text = self.custody.as_ref()?.line.text().to_string();
        let stands = self.left_stands(screen, &text);
        let composer = self
            .reader(&screen.rows)
            .composer(&screen.rows)
            .map(|(_, lines)| lines.join(" "))
            .unwrap_or_default();
        Some(custody::Shows {
            stands,
            composer,
            human: screen.human_seq,
        })
    }

    /// A LOOK AT A LINE HELD IN [`Custody`] AT THE POINT `seen`: how it settled, if it did
    /// (`None`: it stands held), as the core's look at a point says ([`custody::Line::look`]):
    /// its draw on this read, or as a break of the spell saw it drawn ([`custody::Line::drawn`])
    /// where this read shows none of it, or a person's keys drawn after it. Its draw hands it to
    /// the [`LeftLine`] and [`OwnLine`] it was written as, so the look that decides a line left
    /// decides it and its Enter is its writer's ([`Self::left_line_look`]): one drawn at a break
    /// and gone since is a line left that went, or that someone cleared, as the break's read
    /// showed it, never a line lost. Where the look settles nothing and a turn read running since
    /// the write waits on a read that shows none of the line ([`custody::Line::weighs`]: a read
    /// that cannot say what the composer holds, a dialog over it, kept the turn's evidence), one
    /// fresh `status` read decides it ([`Self::custody_turned`]). Every look at a point is this
    /// one (a new point's, a point decided again, a handed point's moved read, the point a busy
    /// spell ends on, the gate's), so a turn kept under a dialog is weighed on the first point's
    /// read after it that can say, before the host is offered that point and whatever the policy
    /// then decides there.
    pub(super) fn custody_look(
        &mut self,
        seen: &Turn,
        review: &mut dyn Review,
    ) -> Result<Option<Settles>, Fail> {
        let Some(shows) = self.custody_shows(&seen.screen) else {
            return Ok(None);
        };
        self.custody_look_on(seen, &shows, review)
    }

    /// [`Self::custody_look`] on what the read `seen` shows of the line held (`shows`, from
    /// [`Self::custody_shows`]).
    fn custody_look_on(
        &mut self,
        seen: &Turn,
        shows: &custody::Shows,
        review: &mut dyn Review,
    ) -> Result<Option<Settles>, Fail> {
        let Some(c) = self.custody.as_mut() else {
            return Ok(None);
        };
        let custody::Look::Settled(settles, stands) = c.line.look(shows, custody::At::Point) else {
            let weighs = c.line.weighs(shows);
            if weighs && self.custody_turned(seen.screen.seq, shows, review)? {
                return Ok(Some(Settles::Turn));
            }
            return Ok(None);
        };
        if settles == Settles::Drawn {
            let own = c.own();
            if self.left_line.is_none() {
                self.left_line = Some(LeftLine {
                    text: own.text.clone(),
                    point_key: self.review_key(seen),
                    stands,
                    human: seen.screen.human_seq,
                });
            }
            if self.own_line.is_none() {
                self.own_line = Some(own);
            }
        }
        self.custody_ended(seen.screen.seq, settles, review);
        Ok(Some(settles))
    }

    /// The line held in [`Custody`] settled at content seq `seq`, as `settles` says: one `NOTE`
    /// ([`super::super::journal::LINE_SETTLED`]), and no write holds on it from here. Where its
    /// program ended, no draw of it comes: the line left and the loop's own line of its text go
    /// with it. A restart that stood asked for it is spent ([`custody::Restarts::settled`]).
    pub(super) fn custody_ended(&mut self, seq: u64, settles: Settles, review: &mut dyn Review) {
        let Some(c) = self.custody.take() else {
            return;
        };
        self.input_lost.settled();
        if settles == Settles::Ended {
            if self.left_line.as_ref().is_some_and(|l| c.is(&l.text)) {
                self.left_line = None;
            }
            if self.own_line.as_ref().is_some_and(|o| c.is(&o.text)) {
                self.own_line = None;
            }
        }
        review.note(&format!(
            "NOTE seq={seq} rule={} {} {}: `{}`",
            c.rule,
            super::super::journal::LINE_SETTLED,
            settles.words(),
            clip(c.line.text())
        ));
    }

    /// A TURN WAS READ RUNNING AFTER A LINE HELD IN [`Custody`] ([`custody::Line::ran`]), and the
    /// look at the read at content seq `seq`, which shows `shows` of it, settled nothing (at a
    /// point, [`Self::custody_look`], or at a break, [`Self::custody_at_break`]; each looks first,
    /// as a line drawn went with no turn): one fresh `status` read, whose input word decides it as
    /// the core says ([`custody::Line::turned`]): on a read that shows none of it, the input clear
    /// settles it (`went with a turn`), and a line still unread, or one no word can say of,
    /// outlasts the turn ([`custody::Line::outlasted`]) and waits for another turn read running;
    /// on a read that cannot say what the composer holds (a dialog over it, no composer drawn, a
    /// glyph's cell unread), under which the line may stand drawn, nothing moves, the turn's
    /// evidence kept for the next read that can say. Whether it settled.
    pub(super) fn custody_turned(
        &mut self,
        seq: u64,
        shows: &custody::Shows,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if self.custody.is_none() {
            return Ok(false);
        }
        let r = self.call(&["status"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
        }
        let word = r
            .ok()
            .then(|| crate::supervise::screen::status_field(&r.stdout, "input"))
            .flatten();
        let now = self.fx.mono();
        let Some(c) = self.custody.as_mut() else {
            return Ok(false);
        };
        Ok(match c.line.turned(word, shows) {
            custody::Turned::Settles => {
                self.custody_ended(seq, Settles::Turn, review);
                true
            }
            custody::Turned::Outlasts => {
                c.line.outlasted(word, now);
                false
            }
            custody::Turned::Stands => false,
        })
    }

    /// A LOOK AT A LINE HELD IN [`Custody`] AT A BREAK of the agent's own background work, on the
    /// break's read `screen` (`Session::host_steps_in_background`), before any line the break
    /// types. A break is no point: no restart in place is made there (it would end the background
    /// work the break waits on), and no Enter is given there, so a line drawn at one stands held,
    /// what the break showed of it kept as the evidence of its draw ([`custody::Look::Kept`]), for
    /// the point's look to settle and hand back ([`Self::custody_after_work`]), whether that point
    /// still shows it or someone cleared it since: handed back here, it would be dropped with the
    /// lines the loop left once the spell ends, and its Enter would no longer be its writer's. The
    /// rest of what settles a line at a point settles it here: a person's keys
    /// ([`custody::Line::look`]), its program's end ([`Self::custody_process_ended`]), and a turn
    /// read running since its write ([`custody::Line::ran`]) once `status` reads the input clear
    /// on a read that shows none of it ([`Self::custody_turned`]); a break whose read cannot say
    /// what the composer holds (a glyph's cell unread) reads no `status` for it and keeps the
    /// turn's evidence for the next read that can ([`custody::Line::weighs`]). A line that stands
    /// held holds every break's line behind it as
    /// behind a draft ([`Session::write_guards`]), and the host's step there
    /// (`IdlePoint::line_held`), through the spell, until one of these settles it or the point
    /// the spell ends on looks at it, where the gate decides what the look leaves
    /// ([`Self::custody_gate`]).
    pub(super) fn custody_at_break(
        &mut self,
        screen: &Screen,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let Some(shows) = self.custody_shows(screen) else {
            return Ok(());
        };
        let look = self.custody.as_mut().map_or(custody::Look::Stands, |c| {
            c.line.look(&shows, custody::At::Break)
        });
        match look {
            custody::Look::Kept => return Ok(()),
            custody::Look::Settled(settles, _) => {
                self.custody_ended(screen.seq, settles, review);
                return Ok(());
            }
            custody::Look::Stands => {}
        }
        if self.custody_process_ended() {
            self.custody_ended(screen.seq, Settles::Ended, review);
            return Ok(());
        }
        if self.custody.as_ref().is_some_and(|c| c.line.weighs(&shows)) {
            self.custody_turned(screen.seq, &shows, review)?;
        }
        Ok(())
    }

    /// A LOOK AT A LINE HELD IN [`Custody`] AT THE POINT `seen` A BUSY SPELL ENDED ON (the loop's
    /// worked arm, once the lines the loop left are dropped), before anything else of the point
    /// ([`Self::custody_look`]): drawn there, in the spell's turn or at a break of it (shown there
    /// still, or cleared since), it is handed back to the [`LeftLine`] and [`OwnLine`] it was
    /// written as, so its Enter is its writer's; under a person's keys it is theirs. A line no
    /// read of the spell showed goes with a turn read running since its write
    /// ([`custody::Line::ran`]) once `status` reads the input clear, where this read shows none
    /// of it ([`Self::custody_turned`]); where it cannot say what the composer holds (a dialog
    /// over it, no composer drawn, a glyph's cell unread), the line may stand drawn under it, and
    /// the turn's evidence is kept for the next read that can say: the next point's look, or the
    /// next spell's end or break. A spell with no such turn (a break's reads alone, a frame being
    /// drawn) settles nothing, and the point's gate decides it ([`Self::custody_gate`]), its
    /// restart in place included.
    pub(super) fn custody_after_work(
        &mut self,
        seen: &Turn,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.custody_look(seen, review).map(drop)
    }

    /// WHAT A LINE HELD IN [`Custody`] LETS THE HOST DO at the idle point at content seq `seq`
    /// (`IdlePoint::line_held`), which the host is offered before the point's gate reads the
    /// line: a line whose program ended (a restart the host made, an upgrade's relaunch) is
    /// settled first ([`Self::custody_process_ended`]), so the carry-on owed the new process is
    /// never held by a line the old one held; then as [`Self::custody_line_held`] says.
    pub(super) fn custody_holds_host(
        &mut self,
        seq: u64,
        review: &mut dyn Review,
    ) -> Option<LineHeld> {
        if self.custody_process_ended() {
            self.custody_ended(seq, Settles::Ended, review);
        }
        self.custody_line_held()
    }

    /// What a line held in [`Custody`] lets the host do ([`custody::line_held`]):
    /// [`LineHeld::Lost`] once the loop judged it lost and makes no restart in place for it, else
    /// [`LineHeld::Stands`]; `None` where no line is held.
    pub(super) fn custody_line_held(&self) -> Option<LineHeld> {
        custody::line_held(self.custody.as_ref().map(|c| &c.line))
    }

    /// Whether the program a line held in [`Custody`] was written to ended
    /// ([`custody::Line::ended`]): the host names another agent process than the one at the write
    /// (`IdleHost::agent_pid`). The host is asked whether a line is held or not.
    fn custody_process_ended(&self) -> bool {
        let process = self.host_process();
        self.custody.as_ref().is_some_and(|c| c.line.ended(process))
    }

    /// The agent's process as the host names it now (`IdleHost::agent_pid`,
    /// `IdleHost::agent_started`; `None`: no host).
    fn host_process(&self) -> Option<custody::Process> {
        self.stall_host
            .as_ref()
            .map(|h| (h.agent_pid(), h.agent_started()))
    }

    /// WHETHER A LINE HELD IN [`Custody`] HOLDS `action`'s WRITE AT `point` (`rule`, the act's),
    /// and the restart in place it may take instead: the one gate every write a point decides goes
    /// through, beside [`Self::left_went_holds`]. The line looked at on the judged read first
    /// ([`Self::custody_look`], a turn kept under a dialog weighed there where the read can say):
    /// drawn there, a write the policy decided on a read that took it for no line of the loop's is
    /// held, and the point decided again soon; its Enter goes. Then one ordinary `status` read
    /// ([`Self::foreground_program`], so a stall's word reaches the stall look) moves what is
    /// known of it ([`custody::Line::gate_read`]), and another process in the tab is its program
    /// ended. A key that answers a box over the composer (a dialog's Enter or Esc) goes past the
    /// line only once `status` proves the worker read it, and the line stands held for the read
    /// after the box ([`custody::answer_goes`]); without that proof the key may be queued behind
    /// the line and is held too. A line the worker read
    /// [`ECHO_HANG`] ago and still reads clear, where the policy wants to write a line now, is
    /// read again in order, `status` then the screen (a draw that came before the clear read
    /// shows on the screen after it): still undrawn and at rest ([`Self::custody_at_rest`],
    /// [`custody::Rest::gate`]), it is [`CustodyGate::Lost`]. Every other hold journals one
    /// `SKIPPED` record per change and re-arms its due ([`Self::custody_rearm`]).
    fn custody_gate(
        &mut self,
        point: &Turn,
        action: &TurnEndAction,
        (rule, opts): (&str, &SuperviseOpts),
        review: &mut dyn Review,
    ) -> Result<CustodyGate, Fail> {
        let Some(shows) = self.custody_shows(&point.screen) else {
            return Ok(CustodyGate::Free);
        };
        let seq = point.screen.seq;
        if let Some(settles) = self.custody_look_on(point, &shows, review)? {
            if settles == Settles::Drawn && !matches!(action, TurnEndAction::Submit { .. }) {
                self.redecide_soon(CUSTODY_DRAWN);
                return Ok(CustodyGate::Hold);
            }
            return Ok(CustodyGate::Free);
        }
        self.program = self.foreground_program()?;
        let now = self.fx.mono();
        let word = self.launch_status.input.clone();
        if self.custody_process_ended() {
            self.custody_ended(seq, Settles::Ended, review);
            return Ok(CustodyGate::Free);
        }
        let Some(c) = self.custody.as_mut() else {
            return Ok(CustodyGate::Free);
        };
        let writes_a_line = matches!(
            action,
            TurnEndAction::Type { .. }
                | TurnEndAction::TypeCommand { .. }
                | TurnEndAction::Submit { .. }
        );
        let read = c.line.gate_read(word.as_deref(), writes_a_line, now);
        if !writes_a_line && custody::answer_goes(c.line.held(), &shows) {
            return Ok(CustodyGate::Free);
        }
        let word = word.unwrap_or_else(|| "-".to_string());
        if read == custody::GateRead::Holds {
            let said = custody::hold_said(c.line.held(), &word);
            self.custody_said(seq, rule, said, &word, review);
            self.custody_rearm();
            return Ok(CustodyGate::Hold);
        }
        let screen = self.screen()?;
        let fresh = self.turn_of(screen);
        if self.custody_look(&fresh, review)?.is_some() {
            self.redecide_soon(CUSTODY_DRAWN);
            return Ok(CustodyGate::Hold);
        }
        let gate = self.custody_at_rest(&fresh, opts).gate();
        if gate == CustodyGate::Hold {
            self.custody_said(seq, rule, custody::NOT_AT_REST.to_string(), &word, review);
            self.custody_rearm();
        }
        Ok(gate)
    }

    /// One `SKIPPED` record of a write a line held in [`Custody`] holds, where what holds it
    /// changed (`said`: [`custody::hold_said`]'s words, or [`custody::NOT_AT_REST`]) since the
    /// last ([`custody::Line::says`]).
    fn custody_said(
        &mut self,
        seq: u64,
        rule: &str,
        said: String,
        word: &str,
        review: &mut dyn Review,
    ) {
        let now = self.fx.mono();
        let Some(c) = self.custody.as_mut() else {
            return;
        };
        let why = if said == custody::NOT_AT_REST {
            "; the session is not at rest, so no restart in place yet"
        } else {
            ""
        };
        if !c.line.says(said) {
            return;
        }
        let evidence = match c.line.read_ago(now) {
            Some(ago) => format!("input={word}, read {} s ago", ago.as_secs()),
            None => format!("input={word}"),
        };
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} {}: `{}` ({evidence}){why}; nothing written",
            super::super::journal::LINE_HELD,
            clip(c.line.text())
        ));
    }

    /// WHAT THE READ `fresh` OF A LINE HELD IN [`Custody`] SAYS OF REST, for the core's judgment
    /// ([`custody::Rest`]): a turn's end its reader stands by, of every phase the policy writes a
    /// line at (idle, a question, a limit's notice, as a point is decided at:
    /// [`super::super::policy::turn_end::turn_ended`]), so a line lost at a question or after a
    /// limit's reset is judged as one lost at an idle point; none of the agent's own work running
    /// under it as the read names it (`ScreenReader::work_running`: for Claude Code the work it
    /// runs in its own process, `aterm_phase::phase::in_process_work`, which no process row names,
    /// a shell or a monitor the turn left running, and any other work the footer's pill counts,
    /// never a workflow's progress row, which outlives its workflow), since a restart in place
    /// would end it: a question asked over a monitor, or a limit's notice drawn over a background
    /// agent, reads as its phase and is no rest while the work runs, and the relaunch reads the
    /// same screen again before its signal
    /// (`relaunch::restart_as`); a composer drawn with no typed text, no dialog and no frame torn
    /// mid-redraw; no hold on the session, the session's word Cruise or Land, the person's grace
    /// past, and no background call of the agent's standing that the host reads
    /// (`IdleHost::background_jobs`, which names one making no progress). Every part the screen
    /// says is read off the one read `fresh`. The composer's text is read only where the turn's
    /// end is drawn, and the host is asked of its background calls only where everything else is
    /// at rest ([`custody::Rest::asks_jobs`]).
    fn custody_at_rest(&mut self, fresh: &Turn, opts: &SuperviseOpts) -> custody::Rest {
        let rows = &fresh.screen.rows;
        let reader = self.reader(rows);
        let turn_ended = super::super::policy::turn_end::turn_ended(&fresh.phase)
            && reader.phase_authoritative(rows, &fresh.phase)
            && reader.composer(rows).is_some()
            && reader.prompt(rows).is_none()
            && !reader.frame_torn(rows);
        let working = reader.work_running(rows).is_some();
        let Some(text) = self.custody.as_ref().map(|c| c.line.text().to_string()) else {
            return custody::Rest::default();
        };
        let now = self.fx.mono();
        let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        let person = self
            .person_ago(now)
            .into_iter()
            .chain(fresh.screen.human.ago())
            .min();
        let clear =
            turn_ended && self.left_stands(&fresh.screen, &text) == Some(LeftStands::Nothing);
        let mut rest = custody::Rest {
            turn_ended,
            working,
            clear,
            halted: self.halted,
            autopilot: self.autopilot(opts) != Autopilot::Off,
            person,
            grace,
            stuck: false,
        };
        if rest.asks_jobs() {
            rest.stuck = opts
                .idle_host
                .as_ref()
                .is_some_and(|h| h.background_jobs().no_progress().is_some());
        }
        rest
    }

    /// THE LINE HELD IN [`Custody`] IS LOST ([`CustodyGate::Lost`]): the worker read it and drew
    /// none of it a minute on, at rest. The core decides what it comes to
    /// ([`custody::Line::lose`]): the first time since the last act submitted, a restart in place
    /// of the process the line was written to ([`Restart::InputLost`], as a memory restart's:
    /// [`Self::restart_agent`]), its `NOTE` written once; where none can be made (relaunch off,
    /// no host, a host that makes none for this program, asked only where nothing else already
    /// says none: each its own `WAITING` words) a doubling wait that writes nothing, its step the
    /// next of the line's waits in a row. A host's deferral (`wait:…`), and its success word while
    /// the line's own process is named after it, are asked again as the restart's back-off says,
    /// their records the restart's own, thinned ([`restart_note_due`]); a refusal or no answer is
    /// asked once ([`custody::LostRestart::Unmade`]), and a loss after a restart made
    /// ([`custody::LostRestart::Made`]), or after a line settled while its restart stood asked
    /// ([`custody::LostRestart::Unseen`]), is never restarted again: each waits, as a repeated
    /// failure calls for a different approach. A line that waits so is lost for good to the loop
    /// ([`custody::Line::is_lost`]), which tells the host so ([`LineHeld::Lost`]): its upgrade,
    /// whose move ends the process, may go.
    fn custody_lost(
        &mut self,
        point: &Turn,
        rule: &str,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        let now = self.fx.mono();
        let Some(read_s) = self
            .custody
            .as_ref()
            .map(|c| c.line.read_ago(now).map_or(0, |ago| ago.as_secs()))
        else {
            return Ok(());
        };
        let mut lane = custody::Lane {
            relaunch: opts.policy.relaunch,
            host: opts.idle_host.is_some(),
            can_restart: false,
        };
        if lane.asks_host(&self.input_lost) {
            lane.can_restart = opts.idle_host.as_ref().is_some_and(|h| h.can_restart());
        }
        let Some(c) = self.custody.as_mut() else {
            return Ok(());
        };
        let loss = c.line.lose(&self.input_lost, &lane, now);
        // The restart in place for this line, of the process it was written to, asked for this
        // line at its every ask.
        let asked = Restart::InputLost {
            of: c.line.process(),
            line: c.ask,
        };
        let undrawn = super::super::journal::LINE_READ_UNDRAWN;
        let unmade = super::super::journal::LINE_RESTART_UNMADE;
        let (until, no_restart) = match &loss {
            custody::Loss::Restart { until, .. } => (*until, None),
            custody::Loss::Wait { why, until, .. } => (
                *until,
                Some(match why {
                    custody::NoRestart::Again => super::super::journal::LINE_LOST_AGAIN.to_string(),
                    custody::NoRestart::Unmade(step) => format!("{undrawn}; {unmade} ({step})"),
                    custody::NoRestart::RelaunchOff => format!("{undrawn}; {RELAUNCH_OFF}"),
                    custody::NoRestart::NoHost => format!("{undrawn}; {NO_HOST}"),
                    custody::NoRestart::NoLane => format!(
                        "{undrawn}; no restart in place (the host makes none for {})",
                        self.program.as_deref().unwrap_or("this program")
                    ),
                }),
            ),
        };
        let why = format!(
            "{}: nothing is written until it draws or a person's keys or a turn follow it",
            no_restart.unwrap_or_else(|| format!("{undrawn}; {unmade}"))
        );
        match loss {
            custody::Loss::Wait { waits, .. } => {
                if restart_note_due(waits) {
                    review.note(&format!(
                        "WAITING seq={seq} until={} {why}",
                        self.stamp_in(until)
                    ));
                }
                self.set_due(until, CUSTODY_DUE);
                Ok(())
            }
            custody::Loss::Restart { first, .. } => {
                // One record of the restart asked for the line, however often a host's deferral
                // has it asked again and whatever holds' records come between the asks: each
                // ask's own records are the restart's (`HOST`, `WAITING`), thinned.
                if first {
                    review.note(&format!(
                        "NOTE seq={seq} rule={rule} {undrawn} {read_s} s on (input=clear, at \
                         rest): restarting it in place"
                    ));
                }
                let otherwise = TurnEndAction::WaitUntil {
                    until,
                    why,
                    wait: WaitKind::Other,
                };
                self.restart_agent(
                    point,
                    &asked,
                    RULE_INPUT_LOST,
                    otherwise,
                    opts,
                    allow,
                    review,
                )
            }
        }
    }

    /// Park or back off when the server refuses a write.
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

    /// Escalate when text was typed into the composer but not submitted, unless Claude Code's
    /// auto-mode setup offer rose over it: that box is the harness's to answer at every level,
    /// and the turn's end is decided again once it is gone.
    fn left_typed(
        &mut self,
        point: &Turn,
        text: &str,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if self.offer_rose(&point.screen.rows) {
            review.note(&format!(
                "SKIPPED seq={} typed, not submitted ({why}): `{}`; the auto-mode setup offer \
                 rose over it: it is answered, then the turn's end is decided again",
                point.screen.seq,
                clip(text)
            ));
            return Ok(());
        }
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

    /// Whether Claude Code's auto-mode setup offer is up on `rows`: it rises at a turn's end over
    /// keys just written and takes them, and the approval loop answers it before the turn's end
    /// is decided again.
    fn offer_rose(&self, rows: &[String]) -> bool {
        self.reader(rows)
            .prompt(rows)
            .is_some_and(|p| p.kind == aterm_phase::PromptKind::AutoModeSetup)
    }

    /// Type and submit `text` via [`Self::type_fenced`] when supported ([`Self::type_guarded`]),
    /// saying how far the text went, the guarded write's as well.
    pub(super) fn type_text_written(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<Written, Fail> {
        let written = self.under_hand(|s| match s.type_fenced(point, text, rule, review)? {
            Some(written) => Ok(written),
            None => s.type_guarded(point, text, rule, review),
        })?;
        self.wrote_own(text, written, point.screen.human_seq);
        Ok(written)
    }

    /// THE LINE A WRITE LEFT IS THE LOOP'S OWN ([`OwnLine`], [`OwnBy::Line`]): written and not
    /// submitted, `text` is no person's draft until the loop's next write, as a write submitted
    /// is. A point's act claims its own line after this ([`Self::left_own`]). `human` is the key
    /// count the write was fenced on ([`OwnLine::unkeyed`]).
    fn wrote_own(&mut self, text: &str, written: Written, human: Option<u64>) {
        match written {
            Written::Left => {
                self.own_line = Some(OwnLine {
                    text: text.to_string(),
                    by: OwnBy::Line,
                    human,
                    at: Some(self.fx.mono()),
                });
            }
            Written::Submitted => self.own_line = None,
            Written::Not => {}
        }
    }

    /// Type a fact line at a break (the door's under `prompt`, or the stuck fact's under `stuck_break_note`: `grant` names whose) using [`Self::type_fenced`] under
    /// [`Self::under_hand`].
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
        _grant: crate::supervise::config::BreakGrant<'_>,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<Option<Written>, Fail> {
        let _duty = self.duty_guard(Scope::TurnEnd);
        use crate::supervise::occupancy::fault::{Fault, armed};
        let written = self.under_hand(|s| match s.type_fenced(point, text, rule, review)? {
            Some(written) => Ok(Some(written)),
            None if armed(Fault::DoorGuardedFallback) => {
                s.type_guarded(point, text, rule, review).map(Some)
            }
            None => Ok(None),
        })?;
        if let Some(written) = written {
            self.wrote_own(text, written, point.screen.human_seq);
        }
        Ok(written)
    }

    /// Run `act` while holding the session's hand lease when `types_under_hand` is enabled.
    fn under_hand<T>(&mut self, act: impl FnOnce(&mut Self) -> Result<T, Fail>) -> Result<T, Fail> {
        let held = self.take_hand()?;
        self.hand_taken = held.then(|| self.fx.mono());
        let out = act(self);
        self.hand_taken = None;
        if held {
            let back =
                crate::harness::upgrade_drive::hand_give_back_here_request_as(self.setup.holder());
            let words: Vec<&str> = back.split(' ').collect();
            let _ = self.call(&words);
        }
        out
    }

    /// [`Self::under_hand`]'s take.
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

    /// THE HAND OUTLASTS THE ACT ([`Self::under_hand`], [`HAND_RENEW`], [`ECHO_HANG`]).
    fn keep_hand(&mut self) -> Result<(), Fail> {
        let Some(taken) = self.hand_taken else {
            return Ok(());
        };
        let now = self.fx.mono();
        if now.saturating_duration_since(taken) < HAND_RENEW {
            return Ok(());
        }
        let take = crate::harness::upgrade_drive::hand_take_here_request_as(self.setup.holder());
        let words: Vec<&str> = take.split(' ').collect();
        let r = self.call(&words)?;
        self.hand_taken = (r.ok() && r.stdout.starts_with("OK lease acquired")).then_some(now);
        Ok(())
    }

    /// How long one wait inside a typed act may last ([`HAND_RENEW`], [`Self::keep_hand`]).
    fn hand_step(&self, left: Duration) -> Duration {
        if self.hand_taken.is_some() {
            left.min(HAND_RENEW)
        } else {
            left
        }
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

    /// THE ECHO OF A FENCED WRITE ([`SETTLE_CAP`], [`echo_of`], [`PASTE_GUARD_HOLD`],
    /// [`unmoved_since`], [`ECHO_HANG`], [`Self::keep_hand`]). A screen that has not moved
    /// [`REACTION_WAIT`] after the write is noted once ([`Self::note_text_held`], under `rule`).
    fn await_echo(
        &mut self,
        at: u64,
        epoch: Option<u64>,
        text: &str,
        (human, guarded): (u64, bool),
        (rule, review): (&str, &mut dyn Review),
    ) -> Result<Echoed, Fail> {
        let began = self.fx.mono();
        let hold = PASTE_GUARD_HOLD.as_millis().to_string();
        let settle = if guarded { hold.as_str() } else { SETTLE_MS };
        let mut whole_since: Option<Instant> = None;
        if matches!(
            self.wait(&["seq", &at.to_string()], REACTION_WAIT)?,
            Wait::TimedOut
        ) {
            self.note_text_held(at, rule, review)?;
        }
        loop {
            self.keep_hand()?;
            let (settled, latched) = self.wait_at(&["idle", settle], SETTLE_CAP)?;
            let now = self.screen()?;
            if now.human_seq != Some(human) {
                return Ok(Echoed::Not(now, ECHO_PERSON));
            }
            let read_at = self.fx.mono();
            let grid = now.generation.as_deref().and_then(ScreenGen::parse);
            let still = matches!(settled, Wait::Latched)
                && grid.is_some_and(|g| Some(g.epoch) == epoch && latched == Some(g.seq));
            let echo = match echo_of(self.reader(&now.rows), &now, text) {
                Echo::Other if unmoved_since(&now, at, epoch) => Echo::Arriving,
                echo => echo,
            };
            match echo {
                Echo::Whole => {
                    let first = *whole_since.get_or_insert(read_at);
                    if !guarded
                        || still
                        || read_at.saturating_duration_since(first) >= PASTE_GUARD_HOLD
                    {
                        return Ok(Echoed::Whole(now));
                    }
                }
                Echo::Arriving => {
                    whole_since = None;
                    let spent = read_at.saturating_duration_since(began);
                    if spent >= ECHO_HANG {
                        return Ok(Echoed::Not(now, ECHO_HUNG));
                    }
                    let step = self.hand_step(ECHO_HANG - spent);
                    let moved = self.wait(&["seq", &now.seq.to_string()], step)?;
                    if matches!(moved, Wait::Unsupported) {
                        return Ok(Echoed::Not(now, ECHO_OTHER));
                    }
                }
                Echo::Other => return Ok(Echoed::Not(now, ECHO_OTHER)),
            }
        }
    }

    /// THE PROGRAM HOLDS THE TEXT ([`super::super::journal::TEXT_HELD`]): a write whose screen
    /// has not moved [`REACTION_WAIT`] on reads `status` once and journals one `NOTE` with what it
    /// says of the session's input (`input=pending input_bytes=<n> input_wait_ms=<ms>` where the
    /// program has not read it), so the journal names the program, not aterm, as what the Enter
    /// waits on. One line per write, never per look, and nothing is typed. The read only observes
    /// ([`Self::call_observed`]): an `input=stalled` it finds starts no stall look, which the
    /// echo's hang reaches as it would without the note. What it read of the input is kept for the
    /// custody the echo's give-up may take the line into: a clear read dates the worker's read of
    /// it ([`Self::custody_formed`]).
    fn note_text_held(&mut self, at: u64, rule: &str, review: &mut dyn Review) -> Result<(), Fail> {
        let r = self.call_observed(&["status"])?;
        let field = |f: &str| {
            r.ok()
                .then(|| crate::supervise::screen::status_field(&r.stdout, f))
                .flatten()
                .unwrap_or("-")
        };
        self.held_note = Some((at, self.fx.mono(), field("input").to_string()));
        review.note(&format!(
            "NOTE seq={at} rule={rule} {} {} s after the write: input={} input_bytes={} \
             input_wait_ms={}",
            super::super::journal::TEXT_HELD,
            REACTION_WAIT.as_secs(),
            field("input"),
            field("input_bytes"),
            field("input_wait_ms")
        ));
        Ok(())
    }

    /// Type and submit `text` using `if-gen=` and `if-human=` fenced `send` and `key` calls.
    fn type_fenced(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<Option<Written>, Fail> {
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
            return Ok(Some(Written::Not));
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
                return Ok(Some(Written::Not));
            }
            Some(Pressing::Refused { why }) => {
                self.write_refused(false, &why, seq, review)?;
                return Ok(Some(Written::Not));
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                return Ok(Some(Written::Not));
            }
            Some(_) => return Ok(Some(Written::Not)),
        }
        if r.skipped_person() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a person keyed before the write; nothing written"
            ));
            self.redecide_soon("a person keyed before the write");
            return Ok(Some(Written::Not));
        }
        if r.skipped_changed() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before the write; nothing \
                 written"
            ));
            self.redecide_soon("the screen moved before the write");
            return Ok(Some(Written::Not));
        }
        if r.skipped() {
            self.not_written(point, text, rule, &format!("{fence} {guard}"), review)?;
            return Ok(Some(Written::Not));
        }
        let at = r.seq().unwrap_or(seq);
        let epoch = ScreenGen::parse(&generation).map(|g| g.epoch);
        let guarded = self.reader(rows).paste_guard();
        self.held_note = None;
        let now = match self.await_echo(at, epoch, text, (human, guarded), (rule, review))? {
            Echoed::Whole(now) => now,
            Echoed::Not(now, why) => {
                let undrawn = (why == ECHO_HUNG || unmoved_since(&now, at, epoch))
                    && self.shows_none_of(&now, text);
                let turn = self.turn_of(now);
                if undrawn {
                    // Undrawn: held in custody, never written again while it may still come, and
                    // no ask.
                    self.custody_formed(&turn, text, (rule, why), (human, at), review)?;
                } else {
                    self.left_typed(&turn, text, why, review)?;
                }
                if why == ECHO_PERSON {
                    self.redecide_soon("a person keyed before the line drew");
                }
                return Ok(Some(Written::Left));
            }
        };
        let reader = self.reader(&now.rows);
        let (Some(generation), Some(caret)) =
            (now.generation.clone(), caret_row(reader, &now.rows))
        else {
            let turn = self.turn_of(now);
            self.left_typed(
                &turn,
                text,
                "the composer does not show it as written",
                review,
            )?;
            return Ok(Some(Written::Left));
        };
        let guard = super::super::policy::row_guard(&now.rows[caret]);
        let (r, args) = self.fenced_key(&guard, "enter", &generation, Some(&person))?;
        let turn = self.turn_of(now);
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => Ok(Some(Written::Submitted)),
            None if r.skipped_person() => {
                self.left_typed(&turn, text, "a person keyed before Enter", review)?;
                // Decided again as the sibling arms are: the person's grace first, then the line
                // still whole gets its Enter alone (the policy's draft step), never a second write;
                // no look moves on a line that stands whole and unchanged. A command line gets it
                // only with no typing of theirs since it was found left, and only where the menu
                // over it runs it as written (v6 §14.17.5.8). Their words joined to it are sent as
                // neither's: it stands badged as aterm's own (`Self::left_composer`).
                self.redecide_soon("a person keyed before the line's Enter");
                Ok(Some(Written::Left))
            }
            None => {
                self.left_typed(&turn, text, "the screen moved before Enter", review)?;
                self.redecide_soon("the screen moved before the line's Enter");
                Ok(Some(Written::Left))
            }
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                self.left_typed(&turn, text, "the session is halted", review)?;
                self.redecide_soon("the hold that refused the line's Enter lifted");
                Ok(Some(Written::Left))
            }
            Some(Pressing::Refused { why }) | Some(Pressing::Unconfirmed { why, .. }) => {
                self.left_typed(&turn, text, &why, review)?;
                self.redecide_soon("the server turned the line's Enter away");
                Ok(Some(Written::Left))
            }
            Some(_) => Ok(Some(Written::Left)),
        }
    }

    /// Submit the draft standing in the composer via a fenced Enter press.
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
            self.draft_stands(point, &reason, review)?;
            return Ok(false);
        };
        let Some((r, args)) = self.press_fenced(point, &rows[caret], text, &generation, review)?
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

    /// Badge a draft standing unsubmitted, which may be a person's.
    fn draft_stands(
        &mut self,
        point: &Turn,
        reason: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let rows = &point.screen.rows;
        let attention = escalate::attention_text(self.reader(rows), point, reason);
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

    /// Press Enter on the draft `text`, fenced on `generation` and guarded on `row`. An Enter the
    /// server could not confirm badges the draft once and backs off like a refusal, so a permanent
    /// error neither parks the point nor repeats unseen.
    fn press_fenced(
        &mut self,
        point: &Turn,
        row: &str,
        text: &str,
        generation: &str,
        review: &mut dyn Review,
    ) -> Result<Option<(CtlReply, String)>, Fail> {
        let seq = point.screen.seq;
        let guard = super::super::policy::row_guard(row);
        let args = super::super::policy::key_args(&guard, "enter", Some(generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        match self.refused_press(&r, &args)? {
            None => return Ok(Some((r, args))),
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => self.write_refused(true, &why, seq, review)?,
            Some(Pressing::Refused { why }) => self.write_refused(false, &why, seq, review)?,
            Some(Pressing::Unconfirmed { why, .. }) => {
                if !self.turn_end_badge {
                    let reason = format!(
                        "a draft stands in the composer, not submitted ({why}): `{}`",
                        clip(text)
                    );
                    self.draft_stands(point, &reason, review)?;
                }
                self.write_refused(false, &why, seq, review)?;
            }
            Some(_) => {}
        }
        Ok(None)
    }
}

/// Host inform message when the `/usage` panel fails to close after [`PROBE_STUCK_AT`] tries.
const PANEL_STUCK: &str = "The /usage panel did not close\nPress Esc in this tab.";

/// What the seeded probe's `NOTE` says of a `/usage` it finds standing that its ledger row proves
/// the previous loop's write ([`Session::probe_seed_claims`], v6 §14.17.5.8).
pub(super) const PROBE_SEED_PROVEN: &str =
    "is the previous loop's write, standing with no person's typing since";

/// What the seeded probe's `SKIPPED` says of a `/usage` it finds standing that its ledger row
/// does not prove the previous loop's write: the person's command, which ends the probe.
pub(super) const PROBE_SEED_UNPROVEN: &str = "stands with no proof it is the previous loop's \
     write (a person's typing since, or no stamp or count that says): theirs, and the seeded \
     probe ends";

/// How long before the probe's row its write may have gone ([`typed_since_ms`]): typing that
/// close before the row is read as after the write, which no write fenced on a later read makes.
const PROBE_ROW_SLACK_MS: i64 = 1_000;

/// WHETHER A PERSON TYPED IN THE WINDOW SINCE A ROW STAMPED `from_ms` (the ledger's clock), by its
/// typing stamp `typed` read at `now_ms`: their key, text or paste, never a scroll or a click,
/// typing within [`PROBE_ROW_SLACK_MS`] before the row counted as after it. `None` where the
/// stamp does not say (a window that reports none, or one that served the session for less than
/// the span with none: a relaunch since the row) and where the clock stands before the row (set
/// back since), which no span is read from.
fn typed_since_ms(typed: HumanInput, now_ms: i64, from_ms: i64) -> Option<bool> {
    let since = now_ms.checked_sub(from_ms).filter(|since| *since >= 0)?;
    match typed {
        HumanInput::Ago(ms) => {
            Some(i64::try_from(ms).is_ok_and(|ms| ms <= since.saturating_add(PROBE_ROW_SLACK_MS)))
        }
        HumanInput::Never => Some(false),
        HumanInput::NeverFor(served) => {
            (i64::try_from(served).is_ok_and(|served| served >= since)).then_some(false)
        }
        HumanInput::Unknown => None,
    }
}

/// Host inform message when `/usage` was typed into the composer without its Enter being sent.
const USAGE_LEFT: &str = "/usage was typed and not sent\nThe supervisor's own check of the \
                          account: it is sent with the session's next act, or stands until you \
                          send or clear it; its panel is closed either way.";

/// Consecutive identical reads of the open `/usage` panel required before acting on it.
const PROBE_READS: u32 = 2;

/// Idle settle duration (ms) between consecutive `/usage` panel reads ([`Session::probe_pace`]).
const PROBE_STILL_MS: &str = "50";

/// Guard ensuring [`IdleHost::account_probed`] is called at most once for a claimed probe.
struct Telling<'h> {
    host: &'h dyn IdleHost,
    told: bool,
    use_token: Option<crate::harness::provider_read::Use>,
}

impl Telling<'_> {
    fn tell(&mut self, read: Result<&Sample, ProbeMiss>) {
        if !std::mem::replace(&mut self.told, true) {
            self.host.account_probed(read);
        }
    }
}

/// Outcome of the probe's fenced `/usage` write ([`Session::probe_type`]).
enum ProbeTyped {
    /// `/usage` is in the composer and its Enter was written at seq `at` under `human`.
    Entered { at: u64, human: u64 },
    /// `/usage` was not submitted.
    Not {
        miss: ProbeMiss,
        why: String,
        then: Probed,
        left: bool,
    },
}

/// A percentage as the journal's `PROBED` line carries it; a row the panel lacked leads with `–`
/// and says so, never a bare `?%` (as the ledger's panel row, `accounts::note_sample`).
pub(super) fn pct_word(pct: Option<u32>) -> String {
    pct.map_or_else(
        || "\u{2013} (not on the panel)".to_string(),
        |p| format!("{p}%"),
    )
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
        if let Some(gate) = self.probe_gates(point, decided, host.as_ref(), opts)? {
            self.probe_shut(point, gate, None, review)?;
            return Ok(Probed::No);
        }
        let use_token = match host.account_probe() {
            Ok(token) => token,
            Err(why) => {
                self.probe_shut(point, ProbeGate::Claim, Some(why.word()), review)?;
                return Ok(Probed::No);
            }
        };
        self.probe_said = Some(self.review_key(point));
        let mut tell = Telling {
            host: host.as_ref(),
            told: false,
            use_token: Some(use_token),
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

    /// Whether the host advised a `/usage` probe at `screen.seq` ([`Self::account_advise`]).
    const fn probe_advised(&self, screen: &Screen) -> bool {
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

    /// Evaluate the probe gates in order and return the first gate that shuts.
    fn probe_gates(
        &mut self,
        point: &Turn,
        decided: &TurnEndAction,
        host: &dyn IdleHost,
        opts: &SuperviseOpts,
    ) -> Result<Option<ProbeGate>, Fail> {
        if self.claim.watching_behind().is_some() {
            return Ok(Some(ProbeGate::Watching));
        }
        if self.setup.ledger_path.is_none() {
            return Ok(Some(ProbeGate::NoLedger));
        }
        if self.probe_off {
            return Ok(Some(ProbeGate::ProbesOff));
        }
        if self.custody.is_some() || self.input_undrawn(point.screen.seq, self.fx.mono()) {
            return Ok(Some(ProbeGate::PendingInput));
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
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => {
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

    /// Whether the session's program draws on the alternate screen (`alt_screen=true`).
    fn alt_screen(&mut self) -> Result<bool, Fail> {
        let r = self.call(&["modes"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("modes failed: {}", r.stderr.trim())));
        }
        Ok(r.ok() && r.stdout.lines().any(|l| l.trim() == "alt_screen=true"))
    }

    /// Whether the composer on `screen` holds the probe's unsent `/usage` command as typed text.
    fn holds_probe_draft(&self, screen: &Screen) -> bool {
        let reader = self.reader(&screen.rows);
        reader
            .composer(&screen.rows)
            .is_some_and(|(_, lines)| squashed(&lines.concat()) == squashed(USAGE_COMMAND))
            && typed_draft(reader, screen)
    }

    /// Record that a `/usage` panel is owed after typing `/usage` into the composer, its row
    /// carrying the person's key count `human` the write was fenced on: a restart's proof that a
    /// `/usage` it finds standing is still this write's ([`Self::probe_seed_claims`]).
    fn probe_owes(&mut self, seq: u64, human: Option<u64>) {
        if self.probe != UsageProbe::Idle {
            return;
        }
        self.probe = UsageProbe::Owed {
            own: true,
            since: self.fx.mono(),
        };
        self.ledger_row_fenced(
            RULE_USAGE_PROBE,
            approvals::Outcome::Typed,
            USAGE_COMMAND,
            approvals::PROBE_TYPED_REASON,
            (seq, human),
        );
    }

    /// Clear `self.probe` once the `/usage` panel is gone, and with it the probe's claim on the
    /// line it left ([`OwnBy::Probe`]).
    pub(super) fn probe_closed(&mut self, seq: u64, review: &mut dyn Review) {
        let was = std::mem::replace(&mut self.probe, UsageProbe::Idle);
        self.probe_hold = None;
        self.probe_seed = None;
        if self
            .own_line
            .as_ref()
            .is_some_and(|own| matches!(own.by, OwnBy::Probe))
        {
            self.own_line = None;
        }
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

    /// THE `/usage` A RESTART FINDS STANDING (v6 §14.17.5.8): a probe the ledger seeded
    /// (`UsageProbe::Owed` with `own: false`) finds a `/usage` no write of this process left. It is
    /// the previous process's write, and the loop's own to send and close, only where the ledger's
    /// row of that write proves it ([`approvals::ProbeWrote`]): the window's key count is the one
    /// the write was fenced on, so no person has keyed since, and the screen's sequence has not
    /// gone back past the row's, so it is the same window's count. Then the line is the probe's
    /// own ([`OwnBy::Probe`]): its Enter the probe's ([`Self::probe_left_enter`]) and the panel it
    /// opens closed unsampled. Anything else is a person's `/usage`, a command that stands as their
    /// hold: the seeded probe ends ([`Self::probe_closed`]), so it closes no panel they open, and
    /// the tab waits only on them. Weighed once, at the first decision on a point whose composer
    /// shows it ([`Self::turn_end_looked`], [`Self::turn_end_again`]): after the look's books of
    /// a busy spell, which end any line a loop left, so a claim made there lasts to the act.
    fn probe_seed_claims(&mut self, screen: &Screen, review: &mut dyn Review) {
        let Some(wrote) = self.probe_seed else {
            return;
        };
        if !matches!(self.probe, UsageProbe::Owed { own: false, .. }) {
            self.probe_seed = None;
            return;
        }
        if !self.holds_probe_draft(screen) {
            return;
        }
        self.probe_seed = None;
        let seq = screen.seq;
        let typed = wrote
            .ts
            .and_then(|ts| typed_since_ms(screen.typed, self.fx.stamp_ms(), ts));
        let unkeyed = match typed {
            Some(typed) => !typed,
            None => wrote.human.is_some() && wrote.human == screen.human_seq,
        };
        if unkeyed && seq >= wrote.seq {
            self.own_line = Some(OwnLine {
                text: USAGE_COMMAND.to_string(),
                by: OwnBy::Probe,
                human: screen.human_seq,
                at: Some(self.fx.mono()),
            });
            review.note(&format!(
                "NOTE seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} {PROBE_SEED_PROVEN} (seq {} \
                 human {}): its Enter is the probe's",
                wrote.seq,
                wrote.human.unwrap_or_default()
            ));
        } else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} {PROBE_SEED_UNPROVEN}"
            ));
            self.probe_closed(seq, review);
        }
    }

    /// Return `true` when a composer is drawn on `screen`.
    fn composer_settles(&mut self, screen: &Screen, review: &mut dyn Review) -> bool {
        if self.reader(&screen.rows).composer(&screen.rows).is_none() {
            return false;
        }
        // A composer that shows nothing of the probe's `/usage` while its program may still draw
        // it is no text cleared: the probe lasts while a line given up on undrawn is held
        // ([`Custody`]), and while the line it left has not been seen whole with no person's key
        // since, however the screen moved meanwhile ([`Self::probe_line_unseen`]).
        if !self.holds_probe_draft(screen)
            && self.custody.is_none()
            && !self.probe_line_unseen(screen)
        {
            self.probe_closed(screen.seq, review);
        }
        true
    }

    /// Whether the probe's own `/usage` left typed ([`OwnBy::Probe`], [`LeftLine`]) is yet to be
    /// seen whole, with no person's key on `screen` since it was left: a program that holds its
    /// input may draw it after any repaint, so the probe and its claim last. The loop's next write
    /// (which takes the line), a turn and a person's key each end that.
    fn probe_line_unseen(&self, screen: &Screen) -> bool {
        self.own_line
            .as_ref()
            .zip(self.left_line.as_ref())
            .is_some_and(|(own, line)| {
                matches!(own.by, OwnBy::Probe)
                    && own.is(&line.text)
                    && line.stands != LeftStands::Whole
                    && line.human == screen.human_seq
            })
    }

    /// The pause between two of the probe's reads ([`PROBE_STILL_MS`], [`ProbeWaits::step`]).
    fn probe_pace(&mut self, until: Instant) -> Result<(), Fail> {
        let left = until.saturating_duration_since(self.fx.mono());
        self.wait(
            &["idle", PROBE_STILL_MS],
            left.min(self.setup.probe_waits.step),
        )?;
        Ok(())
    }

    /// The step past the claim ([`Self::usage_probe`]).
    fn probe_step(
        &mut self,
        point: &Turn,
        tell: &mut Telling<'_>,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Probed, Fail> {
        let seq = point.screen.seq;
        let (entered, human) = match self.under_hand(|s| s.probe_type(point, review))? {
            ProbeTyped::Entered { at, human } => {
                if let Some(use_token) = tell.use_token.take() {
                    tell.host.account_probe_typed(use_token);
                }
                (at, human)
            }
            ProbeTyped::Not {
                miss,
                why,
                then,
                left,
            } => {
                tell.tell(Err(miss));
                // A `/usage` held in custody has its record, and raises nothing.
                let held = self.custody.as_ref().is_some_and(|c| c.is(USAGE_COMMAND));
                if !held {
                    review.note(&format!("SKIPPED seq={seq} rule={RULE_USAGE_PROBE} {why}"));
                }
                if left {
                    // The `/usage` left typed is this loop's own line, drawn now or late: looked
                    // at again as it draws, no person's draft while the probe lasts, and its
                    // Enter the probe's.
                    self.left_own(point, USAGE_COMMAND, OwnBy::Probe);
                    if !held {
                        tell.host.inform(USAGE_LEFT);
                    }
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
                        let offset = self.local_offset_s(now);
                        let local = self.local_at(offset);
                        let zone = self.zone_lookup();
                        break (
                            panel_sample_local(&windows, now, &local, &zone)
                                .ok_or(ProbeMiss::Unsettled),
                            panel,
                        );
                    }
                    prior = Some(windows);
                }
                PanelRead::Unsettled => prior = None,
                PanelRead::Fold => {
                    prior = None;
                    if folds >= PROBE_READS {
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

    /// Type and submit `/usage` using `if-human=` and `if-gen=` fenced `send` and `key` calls.
    /// The write's echo is judged by [`Self::await_echo`], the continuation's rules: waited for
    /// as a hang detector while each read shows it on its way, its Enter fenced on the read
    /// that shows it whole. A text given up on undrawn (an echo that hung, or a screen that never
    /// moved after the write) is held in [`Custody`]: every act ([`Self::custody_gate`]) and the
    /// next probe ([`ProbeGate::PendingInput`]) wait on it until it settles.
    fn probe_type(&mut self, point: &Turn, review: &mut dyn Review) -> Result<ProbeTyped, Fail> {
        let _duty = self.duty_guard(Scope::Walls);
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
                if !r.untouched() {
                    self.probe_owes(seq, Some(human));
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
        self.probe_owes(seq, Some(human));
        let at = r.seq().unwrap_or(seq);
        let epoch = ScreenGen::parse(&generation).map(|g| g.epoch);
        let guarded = self.reader(rows).paste_guard();
        self.held_note = None;
        let now = match self.await_echo(
            at,
            epoch,
            USAGE_COMMAND,
            (human, guarded),
            (RULE_USAGE_PROBE, review),
        )? {
            Echoed::Whole(now) => now,
            Echoed::Not(now, why) => {
                if (why == ECHO_HUNG || unmoved_since(&now, at, epoch))
                    && self.shows_none_of(&now, USAGE_COMMAND)
                {
                    // Input the worker has not drawn: held, so no act nor probe goes behind it
                    // while it may still come ([`Self::custody_gate`]).
                    let turn = self.turn_of(now.clone());
                    self.custody_formed(
                        &turn,
                        USAGE_COMMAND,
                        (RULE_USAGE_PROBE, why),
                        (human, at),
                        review,
                    )?;
                }
                let stands = self
                    .reader(&now.rows)
                    .composer(&now.rows)
                    .is_some_and(|(_, lines)| squashed(&lines.concat()) == squashed(USAGE_COMMAND));
                if self.offer_rose(&now.rows) && !stands {
                    return not(
                        ProbeMiss::NotWritten,
                        "the auto-mode setup offer rose over the write; it is answered, then the \
                         probe is tried again",
                        Probed::Again,
                    );
                }
                let miss = if why == ECHO_PERSON {
                    ProbeMiss::Person
                } else {
                    ProbeMiss::NotWritten
                };
                return left(miss, why);
            }
        };
        let reader = self.reader(&now.rows);
        let (Some(generation), Some(human), Some(caret)) = (
            now.generation.clone(),
            now.human_seq,
            caret_row(reader, &now.rows),
        ) else {
            return left(ProbeMiss::NotWritten, ECHO_OTHER);
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

    /// [`Self::probe_type`] for a test: `None` where `/usage` was entered, else
    /// why it was not and whether it was left typed.
    #[cfg(test)]
    pub(super) fn probe_type_seen(
        &mut self,
        point: &Turn,
        review: &mut dyn Review,
    ) -> Result<Option<(String, bool)>, Fail> {
        Ok(match self.probe_type(point, review)? {
            ProbeTyped::Entered { .. } => None,
            ProbeTyped::Not { why, left, .. } => Some((why, left)),
        })
    }

    /// The probe's `/usage` written at `point` and left typed ([`Self::probe_step`]'s path where
    /// its echo or its Enter missed), for a test.
    #[cfg(test)]
    pub(super) fn probe_left_seen(&mut self, point: &Turn) {
        self.probe_owes(point.screen.seq, point.screen.human_seq);
        self.left_own(point, USAGE_COMMAND, OwnBy::Probe);
    }

    /// [`Self::probe_gates`] for a test, at a point whose policy decided nothing.
    #[cfg(test)]
    pub(super) fn probe_gate_seen(
        &mut self,
        point: &Turn,
        host: &dyn IdleHost,
        opts: &SuperviseOpts,
    ) -> Result<Option<ProbeGate>, Fail> {
        self.probe_gates(point, &TurnEndAction::Nothing, host, opts)
    }

    /// Whether the probe's own `/usage` stands typed and unsent on `screen`: the line this
    /// process's probe left ([`OwnBy::Probe`]), or the previous process's that the ledger's row
    /// proves ([`Self::probe_seed_claims`]). Any other `/usage` (a person's, or one a seeded probe
    /// cannot prove) is no line the probe's Enter sends.
    fn probe_left_stands(&self, screen: &Screen) -> bool {
        self.probe != UsageProbe::Idle
            && self.holds_probe_draft(screen)
            && self
                .own_line
                .as_ref()
                .is_some_and(|own| matches!(own.by, OwnBy::Probe) && own.is(USAGE_COMMAND))
    }

    /// The probe's own `/usage` ([`Self::probe_type`], [`TurnEndAction::Submit`]).
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
                // The line goes: no look at it again, so no draw of the panel reads it as gone.
                if self
                    .left_line
                    .as_ref()
                    .is_some_and(|line| squashed(&line.text) == squashed(USAGE_COMMAND))
                {
                    self.left_line = None;
                }
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

    /// Press a fenced `key` at `row` of a dialog: Enter to dismiss a confirmation
    /// ([`TurnEndAction::Confirm`]), Esc to back out of one the loop opened
    /// ([`TurnEndAction::Cancel`]), fenced on the judged read and on no person's keys since it.
    fn confirm_dialog(
        &mut self,
        point: &Turn,
        action: &TurnEndAction,
        rule: &'static str,
        (row, key): (&str, &'static str),
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        let skipped = |why: &str, review: &mut dyn Review| {
            review.note(&format!("SKIPPED seq={seq} rule={rule} {key}: {why}"));
        };
        let Some(generation) = point.screen.generation.clone() else {
            skipped("the judged read carries no fence for its key", review);
            return Ok(());
        };
        let Some(human_seq) = point.screen.human_seq else {
            skipped(
                "the judged read carries no person count for its key",
                review,
            );
            return Ok(());
        };
        let guard = super::super::policy::row_guard(row);
        let person = format!("if-human={human_seq}");
        let (r, args) = self.fenced_key(&guard, key, &generation, Some(&person))?;
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => {
                let reread = self.turn_end_reread(&point.screen, opts);
                // The point's reach read is spent by the key, as by a typed act
                // (`Self::turn_end_execute`).
                self.reach_seen = None;
                self.witness_seen = None;
                self.turn_end.acted(action, &reread, self.fx.mono());
                review.say(&format!(
                    "TYPED seq={seq} rule={rule} {key} at {}",
                    clip(row)
                ))?;
                self.ledger_row(rule, approvals::Outcome::Typed, key, row.trim(), seq);
                // A PERSON'S OWN `/login` ATERM CLOSED OVER A USAGE LIMIT is decided again once its
                // Esc's pause is over (`login-over-limit@v1`'s pacing): a dialog the Esc left
                // drawn is the point already handed, which no look decides again, so without this
                // due the tab would stand on it with no second Esc and no one told.
                if rule == super::super::policy::login::RULE_LOGIN_OVER_LIMIT
                    && let Some(until) = self.turn_end.login_over_limit_closing(self.fx.mono())
                {
                    self.set_due(until, "the /login dialog aterm closed to close");
                }
            }
            None if r.skipped_person() || r.skipped_changed() => {
                skipped("a person keyed or the screen moved before the key", review);
                self.redecide_soon("the dialog's key was not pressed");
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
            // A read that drew no composer (a torn frame) says nothing of the probe's line: the
            // fresh grid's composer settles it where one is drawn, and the probe ends only where
            // neither shows.
            if !self.composer_settles(&grid, review) {
                self.probe_closed(seq, review);
            }
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
        if self.stall_look(turn, state, review)?
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
        | TurnEndAction::TypeCommand { .. }
        | TurnEndAction::Submit { .. }
        | TurnEndAction::Confirm { .. }
        | TurnEndAction::Cancel { .. }
        | TurnEndAction::Escalate { .. } => format!("{action:?}"),
    }
}

/// Whether the table's `action` at a point that asks a question in prose is one the session's
/// priority word can change ([`Session::decide_to_act`], [`ANSWERS_OFF`]).
fn session_word_can_decide(action: &TurnEndAction) -> bool {
    match action {
        TurnEndAction::Escalate { reason } => reason.ends_with(ANSWERS_OFF),
        action => action
            .rule_id()
            .is_some_and(super::super::policy::turn_end::is_answer_rule),
    }
}

/// The words a wall's reset is placed and remembered under ([`Session::wall_reset_at`]).
fn reset_key(kind: aterm_phase::WallKind, text: Option<&str>) -> String {
    approvals::wall_reset_key(kind, text)
}

/// Keep a known zone when another rendering of this reset omits it. Otherwise a bare
/// footer between two different named zones could make both appear to be the same reset.
fn remember_reset_key(previous: Option<&str>, key: String) -> String {
    let names_zone = |key: &str| {
        matches!(
            key.split_once('|')
                .and_then(|(_, text)| limit::parse_reset(text)),
            Some(limit::ResetSpec::At { zone: Some(_), .. })
        )
    };
    match previous {
        Some(previous)
            if same_reset(previous, &key) && names_zone(previous) && !names_zone(&key) =>
        {
            previous.to_owned()
        }
        _ => key,
    }
}

/// Whether reset keys `a` and `b` ([`reset_key`]) name one reset: the same key, or one wall
/// kind's two texts that [`limit::parse_reset`] reads as one clock time on one day (or both on
/// none), with one zone, or a zone named on one side only, as Claude Code's footer (`10:58pm`)
/// and its turn's notice (`10:58pm (America/Los_Angeles)`, or `10:58pm PDT`) print one limit's.
/// Two named zones that differ, a parenthesized word that names no zone (`(3pm)`, `(in 2
/// hours)`: [`zone_word`]), or a relative reset (`in 2h`) in other words name two.
pub(super) fn same_reset(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (Some((kind_a, text_a)), Some((kind_b, text_b))) = (a.split_once('|'), b.split_once('|'))
    else {
        return false;
    };
    let clock = |text: &str| match limit::parse_reset(text)? {
        limit::ResetSpec::At {
            date,
            hour,
            minute,
            zone,
        } => match zone {
            Some(z) if !zone_word(&z) => None,
            z => Some(((date, hour, minute), z.map(|z| z.to_ascii_lowercase()))),
        },
        limit::ResetSpec::In(_) => None,
    };
    kind_a == kind_b
        && match (clock(text_a), clock(text_b)) {
            (Some((at_a, zone_a)), Some((at_b, zone_b))) => {
                at_a == at_b && (zone_a.is_none() || zone_b.is_none() || zone_a == zone_b)
            }
            _ => false,
        }
}

/// Whether `zone` is shaped as a zone's name ([`same_reset`]): a letter, then letters, digits and
/// `/`, `_`, `+` or `-` only (`America/Los_Angeles`, `UTC`, `Etc/GMT+5`), no clock and no space.
fn zone_word(zone: &str) -> bool {
    zone.starts_with(|c: char| c.is_ascii_alphabetic())
        && zone
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '+' | '-'))
}

/// Whether the loop's host withholds the session's continuations now
/// ([`IdleHost::withholds_turn_end`]).
fn host_withholds(opts: &SuperviseOpts) -> bool {
    opts.idle_host
        .as_ref()
        .is_some_and(|h| h.withholds_turn_end())
}

/// THE LOOP'S RESTART RUN (`Session::restart_run`): the restart's kind ([`Restart::word`]) and its
/// run's Unix second, which make its ask ([`RestartAsk::of`]), and the live claim of the
/// conversation's record it holds ([`RestartClaim`]), which keeps a move's run past the loop's
/// other acts (`Session::run_outlives_act`) and dates the move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RestartRun {
    /// The restart's kind ([`Restart::word`]).
    pub(super) kind: &'static str,
    /// The run's Unix second.
    pub(super) at: i64,
    /// The Unix second the live claim the run holds was taken, by the loop's clock at the answer
    /// that took it (`None`: it holds none).
    pub(super) claimed: Option<i64>,
}

impl RestartRun {
    /// Whether the run, a move's ([`Restart::moves`]), outlives the loop's acts by what it is and
    /// holds (`Session::outlives_act`, [`journaled_open_moves`]): a move's while it holds a live
    /// claim of the conversation's record, an account move ahead's ([`Restart::ACCOUNT_AHEAD`],
    /// put off to a quiet moment) whatever it holds.
    fn move_outlives_acts(self) -> bool {
        (self.claimed.is_some() && Restart::moves(self.kind)) || self.kind == Restart::ACCOUNT_AHEAD
    }
}

/// The word a restart note names a run's claim by (`claimed=<word>`, [`restart_note`]): the
/// second it was taken, or `no`.
fn claim_word(claimed: Option<i64>) -> String {
    claimed.map_or_else(|| "no".to_string(), |at| at.to_string())
}

/// A `HOST … restart:<kind> ask=<ask> claimed=<second|no> step=<step>` note as [`restart_note`]
/// reads it.
struct RestartNote<'a> {
    /// The restart's kind ([`Restart::word`]).
    kind: &'a str,
    /// The Unix second of the run it asked.
    at: i64,
    /// The Unix second the live claim the run held was taken ([`RestartRun::claimed`]).
    claimed: Option<i64>,
    /// The host's step (`-`: it answered none).
    step: &'a str,
}

/// The restart note of `rec` (`Session::restart_agent` writes one at a run's first asks, where its
/// claim changed and at every answer that ends it): the run's second is the one its ask carries
/// (`<kind>@<second>`, [`RestartAsk`]), which a run minted past the wall clock
/// (`Session::restart_floor`) holds ahead of the note's own, else the note's own second, as for a
/// lost line's ask (`line:…`), which is its line's and carries none. A note with no `claimed=`
/// second, which a build before the host said its claim wrote, is read as holding no claim: no
/// answer of that build said one was taken, and its run is restored for none.
fn restart_note(rec: &crate::supervise::journal::JournalRecord) -> Option<RestartNote<'_>> {
    let (_, rest) = rec.summary.strip_prefix("HOST ")?.split_once(" restart:")?;
    let (kind, rest) = rest.split_once(' ')?;
    let (asked, step) = rest.split_once("step=")?;
    let mut words = asked.split_whitespace();
    let second = words
        .next()
        .and_then(|ask| ask.strip_prefix("ask="))
        .and_then(|ask| ask.rsplit_once('@'))
        .and_then(|(_, at)| at.parse().ok());
    let claimed = words
        .next()
        .and_then(|w| w.strip_prefix("claimed="))
        .and_then(|at| at.parse().ok());
    Some(RestartNote {
        kind,
        at: second.unwrap_or_else(|| rec.t.div_euclid(1000)),
        claimed,
        step: step.trim_end(),
    })
}

/// Whether the host's `step` defers the restart it answers, to its back-off or, for a move put off
/// to a quiet moment ([`Restart::put_off`]), to a later turn end (`DEFERRED`,
/// `Session::restart_agent`): a wait (`wait:…`), or another sweep holding the lock
/// (`busy:another-sweep`).
pub(super) fn defers(step: &str) -> bool {
    step.split_once(':').map_or(step, |(head, _)| head) == "wait" || step == "busy:another-sweep"
}

/// The records of `sid` in the journal beside its ledger (`journal`), in order.
fn session_records<'a>(
    journal: &'a str,
    sid: Option<&'a str>,
) -> impl Iterator<Item = crate::supervise::journal::JournalRecord> + 'a {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    journal
        .lines()
        .filter_map(|text| crate::supervise::journal::JournalRecord::from_json(text).ok())
        .filter(move |rec| rec.sid.as_deref() == sid)
}

/// THE NEWEST UNIX SECOND A RESTART RUN WAS ASKED AT, read from the journal beside `sid`'s ledger
/// (`journal`): the newest run's second its restart notes name ([`restart_note`]). `None` where
/// it notes no restart.
pub(super) fn journaled_restart_floor(journal: &str, sid: Option<&str>) -> Option<i64> {
    session_records(journal, sid)
        .filter_map(|rec| restart_note(&rec).map(|note| note.at))
        .max()
}

/// THE RUNS OF THE MOVES AN EARLIER LOOP LEFT OPEN, read from the journal beside `sid`'s ledger
/// (`journal`): the run in the loop's slot and the one set aside beside an account move's
/// (`Session::restart_aside`), each a move's ([`Restart::MOVES`]: a model's move onto its fallback
/// or back, an account move) whose last note was a deferral ([`defers`]) in a run that outlives
/// the loop's acts ([`RestartRun::move_outlives_acts`]: one that held a live claim of the
/// conversation's record, `claimed=<second>`, or an account move ahead's, whatever it held). The
/// session's restart notes ([`restart_note`]) are read in order as the loop moved its runs
/// (`Session::restart_agent`): an account move's open deferral takes the slot and sets aside the
/// open run standing there (`Session::aside_for`); the account move made ends both runs, and
/// refused or deferred unopen it leaves the one set aside; any other restart's note takes the
/// place of both, its run open only where its note is. That note may be the restart the account
/// move's point went on with, which acted on its own run beside the move's
/// (`Session::set_aside_run`); after it the account move is not restored, and only its back-off
/// starts again. As in that loop (`Session::run_outlives_act`), an open run outlived every act
/// after it, and an act ended every other, so only open runs are read back. A later loop restores
/// them so that its ask of a claimed move is the one that claimed the record
/// (`relaunch::its_own`), and an account move ahead is asked on its own run again.
pub(super) fn journaled_open_moves(
    journal: &str,
    sid: Option<&str>,
) -> (Option<RestartRun>, Option<RestartRun>) {
    let mut slot: Option<RestartRun> = None;
    let mut aside: Option<RestartRun> = None;
    for rec in session_records(journal, sid) {
        let Some(note) = restart_note(&rec) else {
            continue;
        };
        let kind = Restart::MOVES.into_iter().find(|&kind| kind == note.kind);
        let open = kind
            .map(|kind| RestartRun {
                kind,
                at: note.at,
                claimed: note.claimed,
            })
            .filter(|run| defers(note.step) && run.move_outlives_acts());
        match open {
            Some(run) if Restart::moves_account(run.kind) => {
                aside = slot.filter(|standing| standing.kind != run.kind).or(aside);
                slot = Some(run);
            }
            _ if kind.is_some_and(Restart::moves_account) => {
                slot = None;
                if made_word(note.step) {
                    aside = None;
                }
            }
            open => {
                slot = open;
                aside = None;
            }
        }
    }
    (slot, aside)
}

/// Whether the host's `step` says the restart it answers made (`adopted`, `relaunched`,
/// `continued`, `done`), as `Session::restart_agent` reads it.
fn made_word(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "adopted" | "relaunched" | "continued" | "done"
    )
}

/// WHEN THE CLAIM THAT MADE THE FALLBACK'S LAST RELAUNCH WAS TAKEN, read from the journal beside
/// `sid`'s ledger (`journal`): the claim's second the model move's last restart note before its
/// `RESTARTED` record names ([`restart_note`]: the note of the answer that made it, as every answer
/// that ends a run is noted), else that note's run's second, else that record's own second. `None`
/// where no relaunch onto the fallback was made.
pub(super) fn journaled_fallback_made(journal: &str, sid: Option<&str>) -> Option<i64> {
    let mut asked: Option<i64> = None;
    let mut made = None;
    for rec in session_records(journal, sid) {
        if rec.kind == "restarted"
            && rec.phase == crate::supervise::policy::turn_end::RULE_MODEL_FALLBACK
        {
            made = Some(asked.take().unwrap_or_else(|| rec.t.div_euclid(1000)));
        } else if let Some(note) = restart_note(&rec)
            && note.kind == Restart::MODEL
        {
            asked = Some(note.claimed.unwrap_or(note.at));
        }
    }
    made
}

#[cfg(test)]
mod left_line_tests {
    use super::*;

    /// WENT TAKES A COUNT THAT STOOD STILL: a host with no key count at all says nothing of who
    /// cleared the composer, though "no count" equals "no count".
    #[test]
    fn a_line_goes_only_on_a_count_that_held() {
        let line = |stands, human| LeftLine {
            text: String::new(),
            point_key: String::new(),
            stands,
            human,
        };
        let on = |human_seq| Screen {
            human_seq,
            ..Screen::default()
        };
        assert!(line(LeftStands::Whole, Some(3)).went(&on(Some(3))));
        assert!(line(LeftStands::Part, Some(3)).went(&on(Some(3))));
        assert!(!line(LeftStands::Whole, Some(3)).went(&on(Some(4))));
        assert!(!line(LeftStands::Nothing, Some(3)).went(&on(Some(3))));
        assert!(!line(LeftStands::Whole, None).went(&on(None)));
    }
}

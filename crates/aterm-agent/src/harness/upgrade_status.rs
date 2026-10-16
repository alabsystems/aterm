// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The owner's view of the live upgrade, and the owner's word on it.
//!
//! Everything here is read from or written to the per-conversation state
//! files the upgrade already keeps, never a second record:
//!
//! * [`rows`]: one [`Row`] per upgrade record (relaunch records are not
//!   upgrades): phase, target, how long the session has been behind, the wait
//!   and since when, the owner's word, and a finished restart's outcome.
//!   `aterm harness upgrade --status` prints them; the window's host hands
//!   its own tabs' rows to the window when they change ([`View`]), shown as
//!   the `upgrade=` column, a band record and, only when [`Row::stall`] says
//!   the upgrade will not move on its own, a row.
//! * The tab's mark: the host raises `meta set attention owner=upgrade` on a
//!   stalled tab and lowers it when that ends ([`View`]); never for pending,
//!   the healthy state.
//! * [`ask`]: the owner's word ([`Request`]), written under the same sweep
//!   lock every step holds so no step's read-modify-write loses it, plus a
//!   push to the window's host ([`word_marker`]), whose worker takes it at
//!   the session's next idle point.
//!
//! A state file outlives what it describes (a current session's file is not
//! touched; an ended conversation leaves its file), so what the owner is shown
//! or counted is vetted against live processes ([`Row::standing`],
//! [`Holder`]): a conversation still held, in its tab, on a build older than
//! its target.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use aterm_json::{Map, Value};

use super::{
    LiveTab, Opts, Phase, Report, Request, STALE_S, St, Version, connect, conversation_live,
    ledger, load, now_s, process_group, roster_aside, save, session_files, state_dir, sweep_lock,
    tab_is_live, unique_tab_for_group, upgrade,
};

/// How long a session may be behind before its upgrade reads stalled on its
/// own ([`Row::stall`], `overdue`): the ladder's Land rung
/// ([`upgrade::RUNG_LAND_S`]) plus [`LAND_GRACE_S`]. Until then the upgrade is
/// working (each rung passes more; only its floors hold it) and is a record,
/// never a warning. Past it, a floor has stood for hours and the owner is
/// shown what holds it and what moves it.
pub const STALLED_AFTER_S: u64 = upgrade::RUNG_LAND_S + LAND_GRACE_S;

/// The grace past the ladder's Land rung before an unmoved upgrade reads
/// `overdue` ([`STALLED_AFTER_S`]).
pub const LAND_GRACE_S: u64 = 4 * 3_600;

const _: () = assert!(STALLED_AFTER_S == 6 * 3_600);

/// Waits the ladder cannot pass at any rung and no person's pause ends
/// ([`Row::stall`], `blocked:<wait>`): `no-shell-integration` (a daemon-mode
/// Codex's `/exit` needs the tab's shell marks to name the thread it resumes,
/// where the kernel cannot name it instead) and `screen-unreadable`. Each is
/// a Warn row, with what moves it, once it has blocked the move for
/// [`BLOCKED_AFTER_S`], timed from the first look that met it
/// (`St::blocked_since`) until a look finds it gone.
pub const BLOCKERS: [&str; 2] = ["no-shell-integration", "screen-unreadable"];

/// How long one of the [`BLOCKERS`] must have blocked the move before it is
/// shown as `blocked` ([`Row::stall`]): a re-ask's interval, so one look that
/// caught a tab mid-handoff is never a warning.
pub const BLOCKED_AFTER_S: u64 = upgrade::REASK_S;

/// How long a finished restart stays in the host's summary: long enough for
/// the window to record it once, and across a handoff.
const DONE_SHOWN_S: u64 = 10 * 60;

/// How long a move that failed after its exit (a Codex `/exit`, a Claude Code
/// SIGTERM) stays in the host's summary (band row, `upgrade=` column, tab
/// mark) once no process is left to vet it by ([`Row::exited_at`]). The
/// ledger keeps it after that.
pub const EXITED_FAILURE_SHOWN_S: u64 = 24 * 3_600;

/// The keyed attention owner the host raises on a stalled tab.
pub const ATTENTION_OWNER: &str = "upgrade";

/// How long the owner's `--now` keeps an overdue upgrade from reading stalled
/// ([`Row::stall`]): the notice's re-ask interval. A `--now` that has not
/// moved the session by then reads stalled again, so an ineffective word
/// cannot hide a stall.
pub const NOW_QUIETS_S: u64 = upgrade::REASK_S;

/// How long aterm's own failure to type a due notice (the notice's fence
/// refused it at every look since, [`Row::refused_notice`]) may stand before
/// the owner is told: three re-ask intervals, enough for a repaint or a
/// person's typing to end.
pub const TELL_S: u64 = 90 * 60;

/// How long past a usage limit's named reset its notice may still be the
/// recorded wait before the owner is told ([`Row::limit_passed`]). A limit is
/// a record until its reset (it ends by itself), but a notice left on screen
/// holds every point the loop would offer. An hour is past any clock skew and
/// a first look after the reset. The same hour ends the hold: the loop offers
/// the point (`supervise::run`'s `limit_spent`), and the quiet upgrade moves
/// the tab when its transcript row names that reset (`upgrade_drive`'s
/// `queue_facts`) or, for Codex, its wall does. Where no reset is named, and
/// in the ask flow, the notice still holds and the owner is told.
pub const LIMIT_GRACE_S: u64 = 3_600;

/// The quiet move's chance past [`LIMIT_GRACE_S`]: under the quiet flow a tab
/// whose limit row named its reset moves once the grace ends (its loop offers
/// the point within a step, or the host looks within [`super::LATER`]), so
/// the tell ([`Row::limit_told_at`]) waits this much longer, marking only a
/// move that had its chance and did not go.
pub const QUIET_MOVE_S: u64 = 10 * 60;

const _: () = assert!(QUIET_MOVE_S >= super::LATER[super::LATER.len() - 1].as_secs());

/// The longest a usage limit lasts (the weekly limits). A `limited` wait
/// whose reset no look read (`St::limit_until == 0`: a Codex wall naming
/// none, or a Claude transcript tail naming no reset) has ended this long
/// after the wait began ([`Row::limit_ends_at`]). A limit is never told
/// before it can have ended.
pub const LONGEST_LIMIT_S: u64 = 7 * 24 * 3_600;

/// THE MOVE CLOCK (design record 2026-09-28, §3.2 C2 and C6): how long a
/// session may be behind before an upgrade that asks again on its own
/// ([`Row::asks_on_its_own`]) is no longer only a record, whatever it waits
/// on — a usage limit alone excepted, which ends by itself at its named reset
/// (ruling 307). The asking is bounded per round (four notices, a give-up, a
/// rest, a new round), but nothing bounded the ROUNDS: an agent whose own
/// work never ends (a widowed `tail -f`, a poll loop, a server it started)
/// is asked, given up on and asked again for as long as it runs, the tab on
/// its old build for days with nothing on the glass. A day is past a whole
/// ask, give-up and stretched rest cycle, so the upgrade working is never
/// news, and a day is when a session is two builds behind. ONE CLOCK for what
/// the day means to each reader: the words stop calling an upgrade that asks
/// again on its own "pending on its next round" ([`Row::asks_on_its_own`]),
/// and the classifier's digest line for the agent's own work and its
/// no-progress backstop wait that long ([`DIGEST_AFTER_S`], [`BACKSTOP_S`]).
///
/// Also the watch's move clock (`upgrade_drive::watch_at`): each round step
/// counts as progress (`St::progress_at`), and only `St::behind_since`, kept
/// across retargets and rounds, sees the session never move. One constant so
/// the row and the watch agree.
pub const MOVE_BUDGET_S: u64 = 24 * 3_600;

const _: () = assert!(MOVE_BUDGET_S > STALLED_AFTER_S);

// ---------------------------------------------------------------- unwatched

/// How long no look may have read a record before it reads `unwatched`: two
/// [`super::WATCH_GAP`]s. A healthy host watches a stuck record once per gap
/// ([`super::watch_at`]), so two unread gaps mean the watch missed at least
/// once. The tab's mark states this bound, never a running age
/// ([`Row::badge`]).
pub const UNWATCHED_AFTER_S: u64 = 2 * super::WATCH_GAP;

/// No record reads `unwatched` within this long of the machine's last wake or
/// boot ([`awake_since`]). Deadlines pass during sleep, and the host's timer
/// fires only after the wake, so without the grace every tab would flag
/// aterm's own fault each morning. Past the host's post-wake catch-up (it
/// checks the wall clock every [`WAKE_CATCH_UP_S`]), yet short enough that a
/// host that really died is reported early.
pub const WAKE_GRACE_S: u64 = 10 * 60;

/// The longest the window's host sleeps on its timer before checking the wall
/// clock (`aterm-gui`'s `harness_host`, `bell_wait`). Its timers use the
/// monotonic clock, which on macOS stops during sleep (`Instant` is
/// `CLOCK_UPTIME_RAW`), so a watch slept through would otherwise fire hours
/// late. One wake per slice while a watch is owed; no look unless one is due.
pub const WAKE_CATCH_UP_S: u64 = WAKE_GRACE_S / 4;

const _: () = assert!(UNWATCHED_AFTER_S == 3_600 && WAKE_CATCH_UP_S < WAKE_GRACE_S);

/// When a record nothing has checked was last checked, read at `now` from its
/// own fields: its last look (`looked_at`) or, never looked at, the watch it
/// was due (`watch_at`); `None` otherwise. Flags a record owed a watch
/// (`watch_at != 0`; [`super::watch_at`] is `None` for finished and
/// `--skip`-held records) whose watch is more than a [`super::WATCH_GAP`] late
/// and unread for [`UNWATCHED_AFTER_S`], never within [`WAKE_GRACE_S`] of
/// `awake_since` (`0`, unknown, never flags).
///
/// The lateness bound keeps a record whose deadline lies hours past its last
/// look from flagging the second that deadline passes; for a record watched
/// at its gap the two bounds coincide.
///
/// A healthy host watches a record at `watch_at` and stamps its look
/// (`upgrade_drive::watch`), moving `watch_at` a gap on, so a record reading
/// this was looked at by nothing: the host thread died, no host runs for its
/// tab, or every watch failed to write. Computed at read time (the CLI's
/// `--status` and `ls`'s `upgrade=`), so a dead host is still visible.
#[must_use]
pub fn unwatched_since(watch_at: u64, looked_at: u64, now: u64, awake_since: u64) -> Option<u64> {
    if watch_at == 0 || watch_at > now || awake_since == 0 {
        return None;
    }
    let late = now - watch_at > super::WATCH_GAP;
    let unread = now.saturating_sub(looked_at) > UNWATCHED_AFTER_S;
    let awake = now >= awake_since.saturating_add(WAKE_GRACE_S);
    (late && unread && awake).then_some(if looked_at != 0 { looked_at } else { watch_at })
}

/// The first unix second at which [`unwatched_since`] reads `Some` for these
/// fields if nothing looks meanwhile, for [`next_change`]. `None` for a
/// record owed no watch, or an unknown awake clock.
fn unwatched_turns_at(watch_at: u64, looked_at: u64, awake_since: u64) -> Option<u64> {
    (watch_at != 0 && awake_since != 0).then(|| {
        watch_at
            .saturating_add(super::WATCH_GAP + 1)
            .max(looked_at.saturating_add(UNWATCHED_AFTER_S + 1))
            .max(awake_since.saturating_add(WAKE_GRACE_S))
    })
}

/// Since when the machine has been awake, unix seconds: the later of its last
/// wake and boot (macOS `kern.waketime`, `kern.boottime`); elsewhere boot
/// alone (Linux `/proc/stat` `btime`, which misses resumes). `0` when unknown,
/// and then nothing reads `unwatched`. Read at every look
/// ([`Row::awake_since`]).
///
/// Test builds read [`pin_awake_since`] (`0` unless a test set it);
/// [`system_awake_since`] is the real reading.
#[must_use]
pub fn awake_since() -> u64 {
    #[cfg(test)]
    {
        AWAKE_PIN.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        system_awake_since()
    }
}

#[cfg(test)]
thread_local! {
    /// The awake clock a test of this crate reads ([`awake_since`]), per
    /// thread: `0`, unknown, until [`pin_awake_since`] sets it.
    static AWAKE_PIN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Pin this thread's [`awake_since`] for a test (`0`: unknown again).
#[cfg(test)]
pub(crate) fn pin_awake_since(at: u64) {
    AWAKE_PIN.with(|pin| pin.set(at));
}

/// The machine's awake clock ([`awake_since`]), read now.
#[must_use]
pub fn system_awake_since() -> u64 {
    #[cfg(target_os = "macos")]
    {
        sysctl_secs(c"kern.waketime").max(sysctl_secs(c"kern.boottime"))
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|stat| {
                stat.lines()
                    .find_map(|l| l.strip_prefix("btime "))
                    .and_then(|s| s.trim().parse::<u64>().ok())
            })
            .unwrap_or(0)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

/// One `struct timeval` sysctl by name (`kern.waketime`, `kern.boottime`), its
/// whole seconds; `0` if the kernel refuses it, answers with another width, or
/// answers a time before 1970.
#[cfg(target_os = "macos")]
fn sysctl_secs(name: &std::ffi::CStr) -> u64 {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len: libc::size_t = std::mem::size_of::<libc::timeval>();
    // SAFETY: `name` is NUL-terminated and outlives the call; `tv` and `len`
    // are live locals the kernel writes through only for the call's duration,
    // and `len` is initialised to the buffer's real size so the copy is
    // bounded; the new-value pair is null/0, so this reads the sysctl and never
    // sets it. No pointer escapes.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut tv).cast::<libc::c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != std::mem::size_of::<libc::timeval>() {
        return 0;
    }
    u64::try_from(tv.tv_sec).unwrap_or(0)
}

/// Whether a recorded wait is one only a person can end (a box, a draft,
/// aterm's hold, a login, Claude's `waiting` on a question's answer, and the
/// release gate `release:<gate>` of each). The upgrade never types over or
/// ends one, so while one stands its re-ask never comes and nothing of the
/// upgrade's own bounds the wait.
fn person_holds(wait: &str) -> bool {
    let wait = wait.strip_prefix("release:").unwrap_or(wait);
    // Aterm's own words left in the prompt, however they stand: neither lane
    // takes them back and the supervisor never sends them, so only a person
    // moves them.
    // Unless aterm cannot read the tab at all: its words may be gone, and
    // only a readable screen settles that.
    if wait.starts_with("left-typed:") {
        return wait != "left-typed:screen-unreadable";
    }
    matches!(
        wait,
        "draft"
            | "box"
            | "held"
            | "login"
            | "not-idle:waiting"
            | NOT_FOREGROUND
            | CLAIMED
            | AUTOPILOT_OFF
            | AGENT_OFF
    )
}

impl Row {
    /// The instant an announced round's person hold ([`person_holds`]) has
    /// kept the re-ask from happening: the same word stood a whole
    /// [`upgrade::REASK_S`] ([`Self::wait_since`]) and the last ask is as old.
    /// `None` otherwise. A hold that passes sooner was the notice's gate
    /// working, and the round still asks on its own ([`Self::asks_on_its_own`]).
    fn person_hold_stands_at(&self) -> Option<u64> {
        let Phase::Announced { at_s, .. } = self.phase else {
            return None;
        };
        person_holds(&self.wait).then(|| self.wait_since.max(at_s).saturating_add(upgrade::REASK_S))
    }
}

/// One process under the agent that held the move at the last waiting look,
/// as shown to the owner ([`Row::held_by`]): pid, name and start time (age is
/// derived from `since`, so the row is stable). Never its command line, which
/// is quoted only to the agent ([`upgrade::running_clause`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldBy {
    /// Its pid.
    pub pid: u32,
    /// Its executable's basename (`zsh`, `caffeinate`).
    pub name: String,
    /// When it started (unix seconds).
    pub since: u64,
}

/// One conversation's upgrade as the owner sees it, from its state file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// The conversation (Claude's session id).
    pub session: String,
    /// The aterm tab (`s-<hex>`), empty until a step has visited it.
    pub tab: String,
    /// The build it runs, and the one it is being moved to.
    pub from: String,
    /// The target version.
    pub to: String,
    /// `managed` or `native`.
    pub source: String,
    /// Where the upgrade stands.
    pub phase: Phase,
    /// Since when the session has been behind (unix seconds).
    pub behind_since: u64,
    /// The last wait a step recorded ([`upgrade::wait_word`]), empty after an
    /// act — or, on a Claude Code row with none recorded, the word its live
    /// holder's status reads (`not-idle:<status>`, [`Row::unreached_status`]).
    pub wait: String,
    /// When that wait began (for a status read off the holder, when that
    /// status began, never before the session was behind).
    pub wait_since: u64,
    /// One of the [`BLOCKERS`] blocking the move (`St::blocked`) and since
    /// when, kept through other waits between looks that meet it; empty and
    /// `0` while none does.
    pub blocked: String,
    pub blocked_since: u64,
    /// For a person at the tab (`attended`), when a look last recorded that
    /// wait; `0` for any other wait or an older build's state. The person is
    /// named only while a look has seen them within [`ATTENDED_SEEN_S`], since
    /// nothing looks while the session works.
    pub wait_seen: u64,
    /// Whether the row's words name a person at the tab ([`Self::person_seen`])
    /// as of the look that made it. Carried like [`Self::stalled`] so that
    /// the moment they stop is a changed row, re-sent at the instant
    /// [`View::refresh`] names.
    pub person_named: bool,
    /// THE AGENT AGREED TO THE MOVE (Claude Code's lane, `St::ready_since`):
    /// its READY answer to this announced round stands, and the restart
    /// waits only on its gates — what the tab's footer says while that is
    /// the person at the tab ([`Self::pause_note`]). `false` for a Codex row
    /// and for any phase but an announced round.
    pub ready: bool,
    /// The owner's word.
    pub request: Request,
    /// When the owner's word was written (unix seconds; `0`: none recorded).
    pub request_at: u64,
    /// A finished restart's outcome line ([`upgrade::restart_outcome`]).
    pub outcome: String,
    /// When the restart finished (`0`: not done).
    pub done_at: u64,
    /// WHEN THE MOVE HAPPENED: its relaunch line typed (`St::relaunched_at`),
    /// which a finished move's record names when the move finished well after
    /// it (`0`: an older record, or no relaunch yet).
    pub moved_at: u64,
    /// Until when the model named by the carry-on's answer is awaited
    /// ([`St::confirming`], `relaunch::MODEL_WAIT`; `0`: nothing owed). Until
    /// then a done row with no `done_at` reads
    /// `restarting/<to>/continued/<age>`, and after it `done`, aged from then
    /// ([`Self::restarting`], [`Self::finished_at`]).
    pub confirm_by: u64,
    /// [`Row::stall`] at the moment the row was read — carried so a summary
    /// that crosses [`STALLED_AFTER_S`] differs from the last one and is sent.
    pub stalled: Option<String>,
    /// [`Row::attention`] at the moment the row was read ([`Level`]) — carried
    /// for the same reason: a wait that turns a day old is a digest line from
    /// that instant, and the window is told then.
    pub level: Level,
    /// THE ENTRY'S SHAPE AND AGE AT THE MOMENT THE ROW WAS READ, carried as
    /// [`Self::stalled`] is ([`Self::read_at`]): whether it asks again on its
    /// own ([`Self::asks_on_its_own`], a record rather than a row), and — for
    /// an `overdue` stall, a row or a record — how long it has been behind
    /// in the words its entry says ([`Self::age_said`]; no other stall's
    /// words carry its age). Each changes by time alone (a person's hold that has
    /// stood, the move clock, the age's next word — minute, hour or day, by
    /// the unit [`behind_words`] counts in: [`next_change`] names the
    /// instant), and a summary equal to the last one is never sent.
    pub on_its_own: bool,
    pub behind_said: Option<String>,
    /// The model the announcement asked for by the model rule
    /// ([`crate::harness::upgrade_models`]), empty when it asked for none or
    /// has not been typed yet — what [`Row::move_words`] names.
    pub model: String,
    /// Which agent the upgrade moves: Claude Code, or Codex
    /// ([`super::codex`], filed per tab as `codex-<tab>`).
    pub agent: upgrade::Agent,
    /// When the agent the move ended was seen gone (the Codex TUI `/exit` was
    /// typed into, the Claude Code sent SIGTERM; unix seconds, `0` if alive or
    /// not ended). A move that failed after that has no process to be vetted
    /// against ([`Self::standing`]) and is still shown.
    pub exited_at: u64,
    /// What ran under the agent at the last look whose step waited
    /// ([`HeldBy`], [`St::held_by`]): empty when nothing did, and once a
    /// step acted.
    pub held_by: Vec<HeldBy>,
    /// A ROUND THAT GAVE UP ([`upgrade::GAVE_UP`]) WHOSE AGENT'S OWN WORK
    /// WAS SEEN since its latest notice (`St::work_seen`: a break of that
    /// work, or processes under the agent): what tells a quiet give-up held
    /// by that work from one a status that only lagged held
    /// ([`Self::rests_on_own_work`]). `false` for any other round.
    pub work_seen: bool,
    /// When a STOPPED round starts the next one (unix seconds): the stop's
    /// stamp plus [`upgrade::RETRY_S`] (`St::failed_at`). `0` for a round
    /// that has not stopped. A stop an older build recorded carries no stamp
    /// and is due at once. What `--status` says as `next_round=` instead of a
    /// permanent `gave-up`.
    pub retry_at: u64,
    /// Why the previous round stopped, while the re-armed round has typed no
    /// notice yet (`St::last_stop`). A refusal there (not a shell job,
    /// uncarriable flags, an untypeable line) keeps its stall through the new
    /// round's first look, so the band row does not resolve and repost at
    /// every re-arm.
    pub last_stop: String,
    /// How many rounds in a row stopped for [`Self::streak_why`]
    /// (`St::stop_streak`). A first stop is the upgrade working (a record); a
    /// repeating one is the person's to see ([`Self::repeating_stop`]).
    pub stop_streak: u32,
    pub streak_why: String,
    /// The notices the process holding the conversation has been asked
    /// while behind, in every round (`St::notices`): what "Asked N times"
    /// says ([`Self::notices_asked`]).
    pub notices: u32,
    /// A given-up round holding a late READY its last look heard (Claude
    /// Code's lane, `St::ready_since`): it acts on the answer and starts no
    /// new round meanwhile ([`upgrade::retry_due`]), so no next round is due
    /// ([`Self::next_round_in`]).
    pub late_ready: bool,
    /// The release still owed (`St::release`): why the upgrade abandoned a
    /// notice without restarting the agent (`gave-up`, `void`, `skipped`,
    /// `deferred`, `retargeted`, a stop's reason) while the carry-on line is
    /// not typed yet; empty when nothing is owed and for Codex rows. It means
    /// the agent asked to wind down is stopped now. `--status` says
    /// `release=`, `--json` `release`, and a given-up round's words say the
    /// line is owed.
    pub release: String,
    /// When the move paused a Codex goal (the tab's goal record,
    /// `super::super::goal_hold`) while the upgrade still owes its resume; `0`
    /// otherwise. The waiting record then says nothing is to be done.
    pub goal_held_since: u64,
    /// Since when that goal counts as left paused
    /// (`upgrade_codex::goal_left_since`): its resume failed; a row with the
    /// one hand step, `/goal resume` ([`Row::stall`] `goal-paused`). `0`
    /// otherwise.
    pub goal_left_since: u64,
    /// Until when a hold that ended without its move keeps the next pause off
    /// (`upgrade_codex::goal_rest_until`); `0` otherwise.
    pub goal_rest_until: u64,
    /// When an announced round's latest notice was typed (unix seconds; `0`:
    /// not announced or an older record). The owner's "last asked" time; not
    /// the phase's `at_s`, which limits and releases move without asking and
    /// which the re-ask is due from.
    pub noticed_at: u64,
    /// When this record was MINTED (`St::salt`; a retarget mints a new one;
    /// `0` for a state an older build wrote with none). What "no look has
    /// recorded what it waits on" is measured from ([`Row::attention`]).
    pub minted_at: u64,
    /// The last look that saw the agent's own work hold the move
    /// (`St::own_work_at`; `0`: none). Acts do not clear it, so the class of a
    /// round does not dip at every notice ([`Row::works`]).
    pub own_work_at: u64,
    /// A `/goal` was in force at the last look that read the screen
    /// (`St::goal`): Claude Code re-prompts itself at every turn end under one,
    /// so its status stays busy and the restart's gate does not open until
    /// the goal is met or cleared. Said in the own-work line's words
    /// ([`Row::attention`]).
    pub goal: bool,
    /// The launch's relaunch line as the record planned it (`St::line`; empty
    /// before a restart was planned): the flags of the manual resume command
    /// ([`Row::resume_said`]) come from it.
    pub line: String,
    /// The restart started its agent AFRESH (`St::cause`,
    /// `relaunch::starts_afresh`: a conversation nobody had asked anything,
    /// or one with no transcript): nothing of the conversation is there to
    /// resume, so an agent it could not bring back is started again by hand
    /// with a plain `claude` ([`Row::agent_down`]), never `claude --resume`,
    /// which Claude refuses for a conversation with no message.
    pub fresh: bool,
    /// When the record last MOVED (`St::progress_at`, unix seconds): stamped
    /// by its one writer when its progress key changes (design record
    /// 2026-09-28, §3.2 C1).
    pub progress_at: u64,
    /// When a look off a point last read the record, and the aterm build
    /// that looked (`St::looked_at`, `St::looked_by`; `0`/empty: never).
    pub looked_at: u64,
    pub looked_by: String,
    /// The last point the session's loop offered its host, and the loop
    /// guard that withheld the latest one it did not (`St::point_at`,
    /// `St::guard`; `0`/empty: none recorded).
    pub point_at: u64,
    pub guard: String,
    /// When the host watches the record (`upgrade_drive::watch_at`: the step
    /// deadline or the move clock, whichever is first, never within
    /// `WATCH_GAP` of the last watch), unix seconds; `0` for a record
    /// nothing more is owed on. `--status` says it as `watch_at=`.
    pub watch_at: u64,
    /// Looks in a row, since the upgrade's last act, whose due notice the
    /// notice's own fence refused (`St::refused`), first at
    /// [`Self::refused_at`] (`0`: none). Past [`TELL_S`] the move reads
    /// overdue whatever its age ([`Self::refused_notice`]) and the owner is
    /// told aterm could not type it.
    pub refused: u32,
    pub refused_at: u64,
    /// THE WAIT LOG (`St::waits`): the last changes of wait and acts, each
    /// with the facts that decided it, newest last. `--status --json` prints
    /// it; nothing gates on it.
    pub waits: Vec<super::WaitNote>,
    /// The first time each distinct wait word was recorded, for the record's
    /// life (`St::firsts`): what the last few rows of [`Row::waits`] no longer
    /// hold of a stall days old.
    pub firsts: Vec<(String, u64)>,
    /// THE USAGE LIMIT'S NAMED RESET, as the conversation's transcript (a
    /// Codex row: its wall) said it at the last look that read it
    /// (`St::limit_until`, unix seconds; `0`: none named, or a record an older
    /// build wrote). A `limited` wait [`LIMIT_GRACE_S`] past it is no longer
    /// a record ([`Self::limit_passed`]); `--json` says it raw.
    pub limit_until: u64,
    /// The limit is money (`St::limit_spend`): a spend cap, shared budget or
    /// no credits, not a usage window. Naming no reset, it is the account
    /// holder's to lift ([`Self::spend_held`]).
    pub limit_spend: bool,
    /// The machine's awake clock as the row was read ([`awake_since`]; `0`
    /// unknown or hand-built). No row reads `unwatched` within
    /// [`WAKE_GRACE_S`] of it ([`Self::unwatched`]). Carried like
    /// [`Self::stalled`]; a later reader keeps the later clock, and the
    /// window's host folds in its own watch's start ([`View::note_look`]).
    pub awake_since: u64,
    /// The conversation runs where no aterm host watches it: a `--status` row
    /// standing only by a live holder proven in no tab ([`Self::standing`]'s
    /// `unproven_tab`), e.g. resumed in another terminal app. No look will
    /// stamp it, so it never reads `unwatched` ([`Self::watch_owed`]). Always
    /// `false` on the window host's rows.
    pub outside: bool,
    /// Why no host worker runs for its tab by someone else's hand, though its
    /// holder is proven there: [`NOT_FOREGROUND`] (every live holder is
    /// suspended or backgrounded, [`Holder::aside`]), [`CLAIMED`] (another
    /// supervisor holds the tab, [`View::set_claimed`]), [`AGENT_OFF`] (the
    /// tab's agent switched off by its own table, [`View::set_agent_off`]) or
    /// [`AUTOPILOT_OFF`] (the owner turned the tab's Autopilot off,
    /// [`View::set_off`]); empty otherwise. No look will stamp the record
    /// meanwhile, so it never reads `unwatched` ([`Self::watch_owed`]). A move
    /// still owed reads the word as its wait ([`Self::go_aside`]). Both the
    /// window's view and `--status` apply this by one rule
    /// ([`Self::set_aside`]; `--status` reads the claim, the agent and the
    /// Autopilot from the instance, [`status_rows`]).
    pub aside: String,
    /// The flow the row is read under: the quiet upgrade (`[harness]
    /// upgrade_ask = false`, the default: nothing asked, the move made at an
    /// idle point) or, `false`, the ask flow and hand-built rows. Taken from
    /// the reader's current setting ([`Self::in_flow`], [`read_rows`]), never
    /// the record. Under it an announced round is judged as pending
    /// ([`Self::asks_on_its_own`]), and a limit past its named reset is the
    /// quiet move's before it is told ([`Self::limit_told_at`]).
    pub quiet: bool,
}

/// THE NOTICES A ROUND RECORD PROVES BY ITSELF: whole rounds in a row that
/// gave up on their notices ([`upgrade::GAVE_UP`], [`upgrade::MAX_ASKS`] each)
/// and this round's asks so far. Fewer than the process was asked wherever a
/// retarget or another stop started its round over, so it is only what a
/// record an older build wrote is read with (`St::notices`) and the least a
/// row says ([`Row::notices_asked`]).
pub(super) fn round_notices(phase: &Phase, streak_why: &str, stop_streak: u32) -> u32 {
    let asks = match phase {
        Phase::Announced { asks, .. } => *asks,
        _ => 0,
    };
    let rounds = if streak_why == upgrade::GAVE_UP {
        stop_streak
    } else {
        0
    };
    rounds
        .saturating_mul(upgrade::MAX_ASKS)
        .saturating_add(asks)
}

/// SINCE WHEN THE SESSION HAS BEEN BEHIND, as the view reads a record before
/// any visit of this build has vetted its age against the process that holds
/// the conversation (`St::for_target`). A record an older build wrote — it
/// names no process, and keeps no wait log — whose age predates its own
/// minting (`pending_since < salt`) carries an age a retarget lent it: read
/// from the minting until a visit settles it, the safe
/// direction (a later digest, never an earlier one). Every record this build
/// writes names its process or has a wait log, and is read as it stands.
fn read_age(st: &St) -> u64 {
    if st.proc_start.is_empty()
        && st.waits.is_empty()
        && st.pending_since != 0
        && st.pending_since < st.salt
    {
        st.salt
    } else {
        st.behind_since()
    }
}

impl Row {
    fn of(session: &str, st: &St, now: u64) -> Row {
        let mut row = Row {
            session: session.to_string(),
            tab: st.tab.clone(),
            from: st.from.clone(),
            to: st.to.clone(),
            source: st.source.clone(),
            phase: st.phase.clone(),
            behind_since: read_age(st),
            wait: st.wait.clone(),
            wait_since: st.wait_since,
            blocked: st.blocked.clone(),
            blocked_since: st.blocked_since,
            wait_seen: st.wait_seen,
            request: st.request_for(&st.tab),
            request_at: st.request_at,
            outcome: st.outcome.clone(),
            done_at: st.done_at,
            moved_at: st.relaunched_at,
            confirm_by: if st.confirming() { st.confirm_by } else { 0 },
            stalled: None,
            level: Level::Quiet,
            on_its_own: false,
            behind_said: None,
            person_named: false,
            ready: reads_ready_clock(st.agent)
                && matches!(st.phase, Phase::Announced { .. })
                && st.ready_since != 0,
            model: st.model_list.clone(),
            agent: st.agent,
            exited_at: st.exited_at,
            held_by: st.held_by.clone(),
            work_seen: st.work_seen
                && matches!(&st.phase, Phase::Failed(why) if why == upgrade::GAVE_UP),
            retry_at: if matches!(st.phase, Phase::Failed(_)) {
                st.failed_at.saturating_add(upgrade::RETRY_S)
            } else {
                0
            },
            last_stop: st.last_stop.clone(),
            stop_streak: st.stop_streak,
            streak_why: st.streak_why.clone(),
            notices: st.notices,
            // A Codex round that gave up hears no late READY (its lane's rule),
            // whatever READY clock its announced round left behind.
            late_ready: reads_ready_clock(st.agent)
                && matches!(&st.phase, Phase::Failed(why) if why == upgrade::GAVE_UP)
                && st.ready_since != 0,
            release: if types_release(st.agent) {
                st.release.clone()
            } else {
                String::new()
            },
            goal_held_since: 0,
            goal_left_since: 0,
            goal_rest_until: 0,
            noticed_at: if matches!(st.phase, Phase::Announced { .. }) {
                st.noticed_at
            } else {
                0
            },
            minted_at: st.salt,
            own_work_at: st.own_work_at,
            goal: st.goal,
            line: st.line.clone(),
            fresh: crate::harness::relaunch::starts_afresh(&st.cause),
            progress_at: st.progress_at,
            looked_at: st.looked_at,
            looked_by: st.looked_by.clone(),
            point_at: st.point_at,
            guard: st.guard.clone(),
            watch_at: super::watch_at(st).unwrap_or(0),
            refused: st.refused,
            refused_at: st.refused_at,
            waits: st.waits.clone(),
            firsts: st.firsts.clone(),
            limit_until: st.limit_until,
            limit_spend: st.limit_spend,
            awake_since: awake_since(),
            outside: false,
            aside: String::new(),
            quiet: false,
        };
        row.read_at(now);
        row.person_named = row.person_seen(now);
        row
    }

    /// This row read under the flow `opts` names ([`Self::quiet`]: `[harness]
    /// upgrade_ask` as the reader has it), its stall read again at `now`.
    fn in_flow(mut self, opts: &Opts, now: u64) -> Row {
        self.quiet = !opts.ask;
        self.read_at(now);
        self
    }

    /// Judges this row again at `now` when served later than it was read (the
    /// window's `ls` column, a pressed word's answer): its awake clock becomes
    /// the later of the machine's now and the one it was read with (which
    /// carries the host's watch start, [`View::note_look`]), so a row carried
    /// over a sleep is inside the wake grace, as at the host's next look. The
    /// one way to serve a row late.
    pub fn reread(&mut self, now: u64) {
        self.awake_since = awake_since().max(self.awake_since);
        self.read_at(now);
    }

    /// This row with the tab's Codex goal record `hold` read in
    /// ([`Self::goal_held_since`], [`Self::goal_left_since`],
    /// [`Self::goal_rest_until`]) at `now`, its stall read again. `switch`: a
    /// save-then-wait switch is open on the tab.
    ///
    /// A goal is never said left paused while aterm still holds it as it
    /// should: while a switch is open (it resumes the goal at its reset), or
    /// while the look's wait is the hold working (`goal-held`,
    /// `goal-resuming`, `switch`, `goal:switch`).
    #[must_use]
    pub fn with_goal(
        mut self,
        hold: Option<&super::super::goal_hold::Hold>,
        switch: bool,
        now: u64,
    ) -> Row {
        use super::super::goal_hold::Owner;
        use super::super::upgrade_codex as cx;
        if let Some(h) = hold.filter(|_| holds_goals(self.agent)) {
            self.goal_held_since = if cx::goal_owed(Some(h)) {
                h.at.max(1)
            } else {
                0
            };
            let holding = switch
                || matches!(
                    self.wait.as_str(),
                    "goal-held" | "goal-resuming" | "switch" | "goal:switch"
                );
            self.goal_left_since = if holding {
                0
            } else if self.wait == cx::SANDBOXED && h.owner == Owner::Upgrade && h.owes() {
                // Left paused on purpose (its thread fell into a sandbox and
                // only a person's relaunch recovers it): a row at once.
                h.last_at().max(1)
            } else {
                cx::goal_left_since(h, now).unwrap_or(0)
            };
            self.goal_rest_until = cx::goal_rest_until(h, now).unwrap_or(0);
        }
        self.read_at(now);
        self
    }

    /// What the row says at `now` that time alone moves, as read
    /// ([`Self::stalled`], [`Self::on_its_own`], [`Self::behind_said`],
    /// [`Self::level`]).
    pub(super) fn read_at(&mut self, now: u64) {
        self.stalled = self.stall(now);
        self.on_its_own = self.asks_on_its_own(now);
        self.behind_said = self.age_said(now);
        self.level = self.attention(now).level();
    }

    /// This row read by a host whose watch started or resumed after a sleep at
    /// `since` ([`View::note_look`]): its awake clock becomes the later of the
    /// machine's and that, so the wake grace covers it, and its stall is read
    /// again at `now`. An unknown awake clock stays unknown.
    fn host_watching_since(&mut self, since: u64, now: u64) {
        if self.awake_since != 0 && since > self.awake_since {
            self.awake_since = since;
            self.read_at(now);
        }
    }

    /// What held the move at the last waiting look ([`Self::held_by`]), in the
    /// owner's words at `now` (`pid 63492 (zsh, 5d4h); …`), at most
    /// [`upgrade::HELD_NAMED`] named and the rest counted
    /// ([`upgrade::held_list`]), never a command. `None` when nothing did.
    #[must_use]
    pub fn held_words(&self, now: u64) -> Option<String> {
        let held: Vec<upgrade::Held> = self
            .held_by
            .iter()
            .map(|h| upgrade::Held {
                pid: h.pid,
                name: h.name.clone(),
                age_s: now.saturating_sub(h.since),
                command: String::new(),
            })
            .collect();
        let words = upgrade::held_list(&held);
        (!words.is_empty()).then_some(words)
    }

    /// [`Self::held_words`] as one `--status` value: `63492(zsh:5d4h),…`,
    /// `,+<n>` for the rest past [`upgrade::HELD_NAMED`], `-` for none —
    /// oldest first, as [`upgrade::held_list`] names them, so `--status` and
    /// the row name the same processes.
    fn held_token(&self, now: u64) -> String {
        let mut held: Vec<&HeldBy> = self.held_by.iter().collect();
        held.sort_by_key(|h| h.since);
        let mut out: Vec<String> = held
            .into_iter()
            .take(upgrade::HELD_NAMED)
            .map(|h| {
                format!(
                    "{}({}:{})",
                    h.pid,
                    word(&h.name),
                    upgrade::span(now.saturating_sub(h.since))
                )
            })
            .collect();
        if self.held_by.len() > upgrade::HELD_NAMED {
            out.push(format!("+{}", self.held_by.len() - upgrade::HELD_NAMED));
        }
        if out.is_empty() {
            "-".to_string()
        } else {
            out.join(",")
        }
    }

    /// Seconds until this stopped round's next one ([`Self::retry_at`]):
    /// `Some(0)` when due; `None` if not stopped, held by the owner's word (a
    /// skip is never re-armed; a deferral is once it runs out), or holding a
    /// late READY ([`Self::late_ready`]).
    #[must_use]
    pub fn next_round_in(&self, now: u64) -> Option<u64> {
        if !matches!(self.phase, Phase::Failed(_)) || self.owner_holds(now) || self.late_ready {
            return None;
        }
        Some(self.retry_at.saturating_sub(now))
    }

    /// When the upgrade reaches the ladder's Land rung (unix seconds), from
    /// which it moves at the first pause (nothing typed within
    /// [`upgrade::KEYS_GAP_S`], no draft, dialog or running turn). Shown to the
    /// owner. `None` where the start is unknown.
    #[must_use]
    pub fn lands_by(&self) -> Option<u64> {
        (self.behind_since > 0).then(|| self.behind_since.saturating_add(upgrade::RUNG_LAND_S))
    }

    /// The ladder's rung at `now` ([`upgrade::rung`] of how long the session
    /// has been behind; the first where the start is not known): what
    /// `--status` says as `rung=`.
    #[must_use]
    pub fn rung(&self, now: u64) -> upgrade::Rung {
        match self.behind_since {
            0 => upgrade::Rung::Prefer,
            since => upgrade::rung(now.saturating_sub(since)),
        }
    }

    /// The one of the [`BLOCKERS`] that has blocked the move for
    /// [`BLOCKED_AFTER_S`] at `now` ([`Self::blocked`]), if any.
    fn blocker(&self, now: u64) -> Option<&str> {
        (BLOCKERS.contains(&self.blocked.as_str())
            && self.blocked_since > 0
            && now.saturating_sub(self.blocked_since) >= BLOCKED_AFTER_S)
            .then_some(self.blocked.as_str())
    }

    /// A restart whose carry-on is typed, with the model its answer names
    /// still waited for at `now` ([`Self::confirm_by`]): `restarting` to the
    /// owner.
    fn restarting(&self, now: u64) -> bool {
        self.phase == Phase::Done && self.done_at == 0 && now <= self.confirm_by
    }

    /// When the move finished for the owner: [`Self::done_at`], or — its
    /// carry-on's model not read by then — the end of the wait for it.
    fn finished_at(&self) -> u64 {
        if self.done_at == 0 {
            self.confirm_by
        } else {
            self.done_at
        }
    }

    /// Whether the owner's word is in force at `now`: it holds the upgrade
    /// ([`Self::owner_holds`]), or a `--now` still quiets its stall
    /// ([`NOW_QUIETS_S`]). A stall ending under the word is not a finished
    /// move.
    #[must_use]
    pub fn word_in_force(&self, now: u64) -> bool {
        self.owner_holds(now) || self.hurried(now)
    }

    /// Whether the owner's word holds this upgrade still: a skip of this
    /// target, or a deferral not yet run out.
    fn owner_holds(&self, now: u64) -> bool {
        match &self.request {
            Request::Skip(v) => upgrade::Version::parse(v) == upgrade::Version::parse(&self.to),
            Request::DeferUntil(t) => now < *t,
            Request::None | Request::Now => false,
        }
    }

    /// Whether the upgrade will move this session to the target at a turn end
    /// with nobody's help: under way or waiting, not held by the owner, and
    /// stalled by nothing but its age. A stopped round resting until
    /// [`Self::retry_at`] is not moving: it needs the rest, a new notice and a
    /// READY first.
    #[must_use]
    pub fn moving(&self, now: u64) -> bool {
        matches!(
            self.phase,
            Phase::Pending
                | Phase::Announced { .. }
                | Phase::Exiting { .. }
                | Phase::Relaunched { .. }
        ) && !self.owner_holds(now)
            && self.stall(now).is_none_or(|s| s == "overdue")
    }

    /// Why this upgrade will not move on its own, as one word; `None` while
    /// healthy or held by the owner:
    ///
    /// * `refused:<what>`: a launch that cannot be resumed, or not its shell's
    ///   job (while stopped and through the re-armed round's first look,
    ///   [`Self::last_stop`]);
    /// * `failed:<why>`: a restart that stopped;
    /// * `held-back:<owner>`: the agent runs under a multiplexer or other pty
    ///   the tab's typing does not reach;
    /// * `blocked:<wait>`: one of the [`BLOCKERS`] for [`BLOCKED_AFTER_S`];
    /// * `stuck:<what>`: a restart under way unmoved for [`STALE_S`]
    ///   ([`Self::stuck`]);
    /// * `unwatched`: no look for [`UNWATCHED_AFTER_S`] ([`Self::unwatched`];
    ///   aterm's own fault, said over every kind below);
    /// * `overdue`: behind for [`STALLED_AFTER_S`] or more, whatever it waits
    ///   on, unless a `--now` was given within [`NOW_QUIETS_S`].
    ///
    /// Pending alone is not stalled, and neither is a round that gave up: it
    /// rests until its next round ([`Self::retry_at`]), though it still reads
    /// `overdue` once that far behind.
    #[must_use]
    pub fn stall(&self, now: u64) -> Option<String> {
        // A Codex goal the move paused and could not resume: a row whatever
        // the move's state or the owner's word; `/goal resume` moves it.
        if self.goal_left_since != 0 {
            // Left paused on purpose, its thread fallen into a sandbox: a
            // row of its own, whose hand step is a relaunch.
            return Some(if self.wait == super::super::upgrade_codex::SANDBOXED {
                "goal-sandboxed".to_string()
            } else {
                "goal-paused".to_string()
            });
        }
        if self.owner_holds(now) {
            return None;
        }
        // Nothing has checked it: aterm's own fault, said over whatever the
        // last look said.
        if self.unwatched(now).is_some() {
            return Some("unwatched".to_string());
        }
        // Overdue: behind STALLED_AFTER_S or more, whatever it waits on
        // (resting rounds included, so the word does not flap across rounds);
        // or a notice aterm could not type for TELL_S (`refused_notice`); or a
        // usage limit whose named reset passed LIMIT_GRACE_S ago with its
        // notice still the wait (`limit_passed`).
        let overdue = ((now.saturating_sub(self.behind_since) >= STALLED_AFTER_S
            && !self.hurried(now))
            || self.refused_notice(now)
            || self.limit_passed(now))
        .then(|| "overdue".to_string());
        match &self.phase {
            Phase::Failed(why) if why == upgrade::GAVE_UP => overdue,
            Phase::Failed(why) if refusal(why) => Some(format!("refused:{}", word(why))),
            Phase::Failed(why) => Some(format!("failed:{}", word(why))),
            Phase::Pending if refusal(&self.last_stop) => {
                Some(format!("refused:{}", word(&self.last_stop)))
            }
            Phase::Pending | Phase::Announced { .. } => {
                if let Some(owner) = self.wait.strip_prefix("terminal:") {
                    Some(format!("held-back:{}", word(owner)))
                } else if let Some(blocker) = self.blocker(now) {
                    // A wait no rung passes and no pause ends.
                    Some(format!("blocked:{}", word(blocker)))
                } else if self.repeating_stop() {
                    // A repeating stop stays the stall through the re-armed
                    // round until it moves or stops for another reason, so the
                    // band keeps one entry for it.
                    Some(format!("failed:{}", word(&self.streak_why)))
                } else {
                    overdue
                }
            }
            Phase::Exiting { .. } | Phase::Relaunched { .. } => {
                self.stuck(now).map(|what| format!("stuck:{what}"))
            }
            Phase::Done => None,
        }
    }

    /// A restart under way that has not moved ([`stuck_on`]), from the row's
    /// phase and when its agent was seen gone; the sweep's ledger note
    /// (`St::stuck`) reads the same predicate.
    fn stuck(&self, now: u64) -> Option<&'static str> {
        stuck_on(&self.phase, self.exited_at, now)
    }

    /// How long this session has been behind at `now`, in a person's words
    /// (`40 min`, `26 h`, `3 days`), for the window's row of an agent's
    /// stalled tabs. `None` where the start is unknown.
    #[must_use]
    pub fn behind_for(&self, now: u64) -> Option<String> {
        behind_words(self.behind_since, now)
    }

    /// The age in the words this entry says at `now` ([`Self::behind_for`]):
    /// only an `overdue` stall (row or record) says how long it has been
    /// behind, and an `unwatched` one how long nothing has checked it
    /// ([`Self::age_from`]); every other kind says [`stall_reason`] with no
    /// age, so `None`. [`Self::behind_said`] carries it and the window compares
    /// entries by it. An `overdue` age counts in hours, then days: at most one
    /// wake an hour.
    #[must_use]
    pub fn age_said(&self, now: u64) -> Option<String> {
        self.age_from(now)
            .and_then(|since| behind_words(since, now))
    }

    /// The instant [`Self::age_said`] counts from: behind-since for `overdue`,
    /// the last check for `unwatched` ([`Self::unwatched`]); `None` for other
    /// stalls.
    fn age_from(&self, now: u64) -> Option<u64> {
        match self.stall(now).as_deref() {
            Some("overdue") => Some(self.behind_since),
            Some("unwatched") => self.unwatched(now),
            _ => None,
        }
    }

    /// Aterm's own failure to type a due notice, standing past [`TELL_S`]: the
    /// last look's wait is a fence-refused notice (`announce-refused:<why>`)
    /// and every look since [`Self::refused_at`] failed too. Told whatever the
    /// move's age or the owner's `--now`.
    #[must_use]
    pub fn refused_notice(&self, now: u64) -> bool {
        self.wait.starts_with("announce-refused:")
            && self.refused > 0
            && self.refused_at != 0
            && now.saturating_sub(self.refused_at) >= TELL_S
    }

    /// Whether the notice was refused for a state that is NOT aterm's: a
    /// person's draft or box, or another driver's lease (`held`). The fence
    /// told the truth there, and the holder's to end it — never "aterm's to
    /// fix".
    fn refusal_stands(&self) -> bool {
        matches!(
            self.wait.as_str(),
            "announce-refused:draft" | "announce-refused:held" | "announce-refused:box"
        )
    }

    /// A USAGE LIMIT WHOSE NAMED RESET HAS PASSED, its notice still the
    /// recorded wait (the assessment of 2026-09-29, A1; design record
    /// 2026-09-28, §3.2 C6, `Wall(until)`): the last look waited `limited`,
    /// and the reset the transcript named ([`Self::limit_until`]) is
    /// [`LIMIT_GRACE_S`] or more behind `now`. Until its reset a limit is a
    /// record whatever the move's age (ruling 307), and it stays one where no
    /// look read a reset (`0`). Past it the session can take a turn again,
    /// and a notice still on its screen held every point the loop would
    /// offer until the ruling of 2026-09-29 (C9, [`LIMIT_GRACE_S`]): the quiet
    /// upgrade now moves such a tab at its next look, and where it cannot (no
    /// reset named, the ask flow) nothing moves until a person types in the
    /// tab. Told at once, whatever the move's age and the owner's `--now`
    /// (which never waives a limit) — [`Self::stall`], [`Self::badge`] — as
    /// aterm's own class, at the instant [`next_change`] names.
    ///
    /// `HarnessUpgradeProgress`'s open gap `LimitRecordForEver`, closed: a
    /// tab left at its limit notice after the reset was never marked and
    /// never moved (`asks_on_its_own` and `past_move_budget` read `limited`
    /// alone, and no record kept the reset).
    ///
    /// A RESET NO LOOK READ is bounded ([`Self::limit_ends_at`]): the limit
    /// has ended by [`LONGEST_LIMIT_S`] after its wait began, so it is told
    /// [`LIMIT_GRACE_S`] past that — never a record for ever.
    #[must_use]
    pub fn limit_passed(&self, now: u64) -> bool {
        self.limit_told_at().is_some_and(|at| now >= at)
    }

    /// The instant a recorded `limited` wait is told ([`Self::limit_passed`],
    /// for [`next_change`]): [`LIMIT_GRACE_S`] past its end
    /// ([`Self::limit_ends_at`]), plus [`QUIET_MOVE_S`] under the quiet flow
    /// for a named reset ([`Self::quiet`], [`Self::limit_until`]), since the
    /// grace's end is when the quiet move may first go.
    fn limit_told_at(&self) -> Option<u64> {
        let end = self.limit_ends_at()?;
        let moves = if self.quiet && self.limit_until != 0 {
            QUIET_MOVE_S
        } else {
            0
        };
        Some(end.saturating_add(LIMIT_GRACE_S).saturating_add(moves))
    }

    /// When a recorded `limited` wait is known to have ended: the named reset
    /// ([`Self::limit_until`]), else [`LONGEST_LIMIT_S`] after the wait began.
    /// `None` for other waits, an unknown start, and a spend or credit wall
    /// naming no reset ([`Self::spend_held`]), which only the owner lifts.
    #[must_use]
    pub fn limit_ends_at(&self) -> Option<u64> {
        if self.wait != "limited" {
            return None;
        }
        if self.limit_until != 0 {
            return Some(self.limit_until);
        }
        if self.limit_spend {
            return None;
        }
        (self.wait_since != 0).then(|| self.wait_since.saturating_add(LONGEST_LIMIT_S))
    }

    /// A spend or credit wall holds the move with no reset read
    /// ([`Self::limit_spend`]): a Codex spend cap or workspace credits, a
    /// Claude Code spend limit or extra usage. Nothing ends it by itself, so it
    /// is the owner's hold: a person's class ([`Self::overdue_class`]), a row
    /// once overdue ([`Self::asks_on_its_own`]), never bounded by the week
    /// ([`Self::limit_ends_at`]), worded as what lifts it (credits added, cap
    /// raised).
    #[must_use]
    pub fn spend_held(&self) -> bool {
        self.wait == "limited" && self.limit_spend && self.limit_until == 0
    }

    /// Since when no look has read a record the move is still owed on
    /// ([`unwatched_since`] of [`Self::watch_at`], [`Self::looked_at`],
    /// [`Self::awake_since`]). `None` while looked at, within the wake grace,
    /// and for records no watch is for: a restart under way (bounded by
    /// `stuck`, [`STALE_S`]), a finished move, or one that stopped after its
    /// agent was gone ([`Self::failed_after_exit`]).
    ///
    /// Aterm's own fault, always told ([`Self::stall`] `unwatched`, ahead of
    /// every kind but a paused goal and the owner's hold; never a record,
    /// [`Self::asks_on_its_own`]). It shows the owner when the model's
    /// `WF(Watch)` assumption (the host's timer fires) fails.
    #[must_use]
    pub fn unwatched(&self, now: u64) -> Option<u64> {
        if !self.watch_owed() {
            return None;
        }
        unwatched_since(self.watch_at, self.looked_at, now, self.awake_since)
    }

    /// Whether a watch is for this record at all ([`Self::unwatched`]): the
    /// move still owed — waiting, or a stopped round resting — with its
    /// agent not seen gone, its conversation where a host watches it
    /// ([`Self::outside`]), and its tab one a host runs a worker for
    /// ([`Self::aside`]).
    fn watch_owed(&self) -> bool {
        matches!(
            self.phase,
            Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
        ) && !self.failed_after_exit()
            && !self.outside
            && self.aside.is_empty()
    }

    /// Whether the due notice is one aterm's own fence refuses
    /// (`announce-refused:<why>`) while the owner's `--now` is in force; then
    /// `Upgrade now` is no remedy ([`Self::remedy`]).
    #[must_use]
    pub fn fence_fails_under_now(&self) -> bool {
        self.wait.starts_with("announce-refused:") && self.request == Request::Now
    }

    /// Whether the owner's `--now` still quiets an overdue stall at `now`:
    /// in force, and given within [`NOW_QUIETS_S`] ([`Self::stall`]).
    fn hurried(&self, now: u64) -> bool {
        self.request == Request::Now && now < self.request_at.saturating_add(NOW_QUIETS_S)
    }

    /// What the stall means in a person's words ([`Self::stall`]); `None` when
    /// not stalled. An `overdue` stall says how long and what it waits on,
    /// which change every step, so these words are for a record, never the
    /// tab's mark ([`Self::badge`]). Never the wait's raw word; `--status` and
    /// the `upgrade=` column carry that.
    #[must_use]
    pub fn stall_words(&self, now: u64) -> Option<String> {
        let stall = self.stall(now)?;
        // How long, in a person's words (`behind for 8 h`), only where the
        // start is known (a start of 0 would count from 1970).
        let behind = behind_words(self.behind_since, now).map(|b| format!("behind for {b}"));
        let with = |what: &str| match &behind {
            Some(b) => format!("{b}: {what}"),
            None => what.to_string(),
        };
        Some(match stall.as_str() {
            // A limit whose named reset has passed ([`Self::limit_passed`]):
            // said as such, never "it is at a usage limit".
            "overdue" if self.limit_passed(now) => {
                if self.limit_until != 0 {
                    let ago = behind_words(self.limit_until, now)
                        .map_or_else(String::new, |span| format!(" {span} ago"));
                    with(&format!(
                        "its usage limit's reset passed{ago}, and the last look still read the \
                         limit"
                    ))
                } else {
                    // No look read the reset: say what is known, never a reset
                    // time (stall words stay within 64 characters).
                    with("its limit has outlasted any usage limit")
                }
            }
            // A given-up round that heard a late READY acts on it
            // ([`Self::late_ready`]): no rest and no re-ask meanwhile.
            "overdue" if self.late_ready => {
                let late = with(
                    "it agreed to the move after the upgrade stopped asking, and the upgrade acts \
                     on that answer",
                );
                match self.wait_words(now) {
                    Some(what) => format!("{late} ({what})"),
                    None => late,
                }
            }
            // A round that gave up and rests (ruling 283): what happened and
            // what comes next, never the step's `failed` word — and, while
            // the agent it asked to wind down has not been told to carry on
            // ([`Self::release`], L5 of the upgrade's leftovers), that it is
            // owed that line: "it has not agreed" alone read as an agent at
            // work, of one that stopped for the restart.
            // Under the QUIET flow a round gives up only on its agent's own
            // work, and the round after the rest moves it at a quiet moment,
            // asking nothing.
            "overdue"
                if self.quiet
                    && matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) =>
            {
                with(if self.release.is_empty() {
                    "its own work kept the move from coming, so the upgrade rests, then moves it \
                     at a quiet moment"
                } else {
                    "its own work kept the move from coming and it is owed the line that tells \
                     it to carry on; the upgrade rests, then moves it at a quiet moment"
                })
            }
            "overdue" if matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) => {
                with(if self.release.is_empty() {
                    "it has not agreed to the move yet, so the upgrade rests, then asks again"
                } else {
                    "it has not agreed to the move yet and is owed the line that tells it to \
                     carry on; the upgrade rests, then asks again"
                })
            }
            // Aterm could not type its notice: said as aterm's own, with how
            // often and for how long.
            "overdue" if self.refused > 0 && self.wait.starts_with("announce-refused:") => {
                let what = self
                    .wait_words(now)
                    .unwrap_or("aterm could not type its notice");
                let times = if self.refused == 1 {
                    "once".to_string()
                } else {
                    format!("{} times", self.refused)
                };
                let over = behind_words(self.refused_at, now)
                    .map_or_else(String::new, |span| format!(" over {span}"));
                with(&format!("{what} ({times}{over})"))
            }
            "overdue" => match self.wait_words(now) {
                Some(what) => with(what),
                None => behind.unwrap_or_else(|| "it has not moved yet".to_string()),
            },
            // Nothing has checked it: for how long and whose fault, never the
            // last look's words (stall words stay within 64 characters).
            "unwatched" => {
                let age = self
                    .unwatched(now)
                    .and_then(|since| behind_words(since, now))
                    .unwrap_or_else(|| "a while".to_string());
                format!("not checked for {age} \u{2014} aterm's own fault")
            }
            kind => stall_reason(self.agent, kind),
        })
    }

    /// Whether the upgrade is still asking on its own, so the window records
    /// it and marks nothing ([`Self::badge`]):
    ///
    /// * an overdue announced move ([`Phase::Announced`]) re-asked every half
    ///   hour, waiting on what `--now` does not waive ([`Remedy::Waits`]);
    /// * a stopped round that will ask again: a give-up, or the first stop for
    ///   any reason other than a refusal or a move whose agent is gone
    ///   ([`Self::failed_after_exit`]); each rests until [`Self::retry_at`].
    ///   A repeating stop ([`Self::repeating_stop`]) is a row;
    /// * a round re-armed after a give-up that still waits on the agent's own
    ///   work, or has not yet been told apart from its rest
    ///   ([`Self::rest_carries_on`]); the move comes when that work ends
    ///   (`upgrade::Facts::work_ended`).
    ///
    /// Not when:
    ///
    /// NOT WHILE A PERSON HOLDS IT: an announced round whose
    /// notice waits on a draft, a box, aterm's hold, a login or a question
    /// ([`person_holds`]) is never asked again — the re-ask is typed only
    /// under the gate that holds it — so it never gives up either, and
    /// nothing but the person ends the wait. Read as the upgrade working, the
    /// column and `--status` said "asks again" of a round that never would.
    /// (What the OWNER is told of it is [`Self::attention`]'s: the person who
    /// holds it is at the tab, so a record, a log line once a day has passed
    /// with no progress, a digest line for a draft nobody sent — never a row
    /// with a tab mark.)
    ///
    /// NOT PAST THE MOVE CLOCK ([`MOVE_BUDGET_S`]): a session
    /// a day behind is no longer "asking on its own", whatever its rounds do —
    /// ask, give up, rest and ask again for as long as the agent's own work
    /// runs — except at a usage limit, which ends by itself (ruling 307). The
    /// day is news once, as one digest line ([`Self::own_work`]), not a mark.
    ///
    /// A HOLD THAT HAS STOOD, never a passing word ([`Self::person_hold_stands_at`],
    /// review two): the notice's gate records `draft`, `held`, a box at
    /// whatever look meets one — a person typing their next prompt at an
    /// idle look — and the next look's `busy` ends it. Read as a person's
    /// hold at once, each such look flipped the record's words (the column,
    /// `--status`) and back.
    ///
    /// NOT A LIMIT WHOSE NAMED RESET HAS PASSED ([`Self::limit_passed`]):
    /// ruling 307 keeps a limit a record until its reset, whatever the round
    /// does; past it (and its grace), the notice on the screen is what holds
    /// every point, and no round asks past it ([`Self::will_ask_again`]).
    ///
    /// NOR A RECORD NOTHING HAS CHECKED ([`Self::unwatched`], design record
    /// C10): asking on its own is the host's work, and no look of the host's
    /// has come — a limit before its reset included, whose end no look would
    /// see either.
    ///
    /// NOR AN ANNOUNCED ROUND UNDER THE QUIET FLOW ([`Self::quiet`]): nothing
    /// asks it again there — the quiet step answers every announced round an
    /// older aterm (or `upgrade_ask`) left, and never re-asks it (it gives up
    /// only once the agent's own work has held its restart a whole re-ask
    /// window, and releases the agent) — so it is judged as a pending round
    /// is, and a busy agent's stall is marked at six hours behind, not only at
    /// the move clock's day.
    #[must_use]
    pub fn asks_on_its_own(&self, now: u64) -> bool {
        !self.past_move_budget(now) && self.will_ask_again(now)
    }

    /// WHETHER THE UPGRADE WILL ASK AGAIN BY ITSELF, at any age: the FACT
    /// [`Self::asks_on_its_own`] is the words' reading of (the `upgrade=`
    /// column, `--status`) until the move clock runs out. A round resting
    /// after a give-up, or asking, goes on to its next notice whether it is
    /// six hours or six days behind - the day changes what the owner is told
    /// ([`Self::attention`]), not what the upgrade does - so a record's title
    /// and its remedy (`Claude upgrade retries later`, `Upgrade now cannot
    /// move it past its own work`) read THIS, never the age.
    #[must_use]
    pub fn will_ask_again(&self, now: u64) -> bool {
        if self.limit_passed(now) || self.unwatched(now).is_some() {
            return false;
        }
        // An announced round nothing asks again reads as the pending one it
        // waits like.
        let pending = self.phase == Phase::Pending
            || (self.quiet && matches!(self.phase, Phase::Announced { .. }));
        match &self.phase {
            Phase::Announced { .. } if !self.quiet => {
                self.remedy(now) == Some(Remedy::Waits)
                    && self.person_hold_stands_at().is_none_or(|at| now < at)
            }
            _ if pending
                && self.last_stop == upgrade::GAVE_UP
                && !self.refused_notice(now)
                && (self.overdue_cause(now) == OWN_WORK || self.rest_carries_on(now)) =>
            {
                true
            }
            // A usage limit ends by itself, before a notice as after one: a
            // record whatever the phase until its named reset. A spend or
            // credit wall ([`Self::spend_held`]) is the owner's hold, a row
            // once overdue.
            _ if pending => {
                self.wait == "limited"
                    && !self.spend_held()
                    && self.remedy(now) == Some(Remedy::Waits)
            }
            Phase::Failed(why) => {
                !self.owner_holds(now)
                    && (why == upgrade::GAVE_UP
                        || (!refusal(why) && !self.failed_after_exit() && !self.repeating_stop()))
            }
            _ => false,
        }
    }

    /// Whether a round re-armed after a give-up is still the rest before it.
    /// The re-arm clears the recorded wait and what ran under the agent, so
    /// until a look records them again nothing tells the round apart from the
    /// rest, which asks on its own: a wait not recorded yet, a person at the
    /// tab (`attended`), or a person's hold that has not stood a whole
    /// [`upgrade::REASK_S`] ([`Self::person_hold_stands_at`]). A standing hold,
    /// a wait `Upgrade now` moves, a usage limit and a refused notice are
    /// judged as themselves.
    ///
    /// So is a round whose look found nothing holding it
    /// ([`upgrade::NEXT_POINT`]): its first step is due at the next point. The
    /// move clock still applies ([`Self::past_move_budget`]).
    fn rest_carries_on(&self, now: u64) -> bool {
        match self.wait.as_str() {
            "" | "attended" | upgrade::NEXT_POINT => true,
            _ => self.rearmed_hold_stands_at().is_some_and(|at| now < at),
        }
    }

    /// The instant a re-armed round's person hold ([`person_holds`]) has
    /// stood a whole [`upgrade::REASK_S`] and stops being the rest before it
    /// ([`Self::rest_carries_on`]); `None` for any other row or word.
    fn rearmed_hold_stands_at(&self) -> Option<u64> {
        (self.phase == Phase::Pending
            && self.last_stop == upgrade::GAVE_UP
            && person_holds(&self.wait))
        .then(|| {
            self.wait_since
                .max(self.behind_since)
                .saturating_add(upgrade::REASK_S)
        })
    }

    /// Whether the session has been behind [`MOVE_BUDGET_S`] or more at `now`
    /// and waits on anything but a usage limit; a start of 0 is no age.
    fn past_move_budget(&self, now: u64) -> bool {
        self.behind_since > 0
            && now.saturating_sub(self.behind_since) >= MOVE_BUDGET_S
            && (self.wait != "limited" || self.spend_held())
    }

    /// Whether the latest stop repeated (same reason two or more rounds in a
    /// row, not a refusal or give-up): the rest stretches
    /// ([`upgrade::rest_extension`]) and the person is shown it.
    #[must_use]
    pub fn repeating_stop(&self) -> bool {
        self.stop_streak >= 2
            && !self.streak_why.is_empty()
            && self.streak_why != upgrade::GAVE_UP
            && !refusal(&self.streak_why)
    }

    /// What a Codex wait `--now` cannot move stands for, in a person's words:
    /// what its daemon waits on (`daemon-first:<why>`, the daemon's step
    /// carried into the client's wait), a background terminal left running,
    /// the lane's own text in the composer. `None` for other waits, whose
    /// word says it.
    ///
    /// A person at a Claude Code tab (`attended`) is named only while a look
    /// has seen them within [`ATTENDED_SEEN_S`]; past that the word is the
    /// last idle point's (no look is taken while the session works), and the
    /// words say the move waits for the next point.
    fn wait_words(&self, now: u64) -> Option<&'static str> {
        if self.person_gone(now) {
            return Some("it waits for its next turn end");
        }
        self.recorded_wait_words()
    }

    /// Whether this row's wait is a Claude Code tab's `attended` person no look
    /// has seen within [`ATTENDED_SEEN_S`]. Read by both the row's words
    /// ([`Self::wait_words`]) and the tab's mark ([`Self::overdue_cause`]) so
    /// they agree.
    fn person_gone(&self, now: u64) -> bool {
        attended_is_person(self.agent) && self.wait == "attended" && self.attended_unseen(now)
    }

    /// [`Self::wait_words`] as the look that recorded the wait said it, with
    /// no clock: what [`Self::waiting_line`] names, which reads a person at
    /// the tab as the ladder's comfort before it gets here.
    fn recorded_wait_words(&self) -> Option<&'static str> {
        if self.wait.is_empty() {
            return None;
        }
        if self.spend_held() {
            return Some(SPEND_WORDS);
        }
        match self.agent {
            upgrade::Agent::Claude => {
                // A release gate's words say a line is owed only while one is
                // ([`Self::release`]); a recorded gate word can outlive its
                // release.
                if self.wait.starts_with("release:") && self.release.is_empty() {
                    return None;
                }
                return claude_wait_words(&self.wait);
            }
            upgrade::Agent::Codex => {}
        }
        Some(match self.wait.as_str() {
            // Its own conversation's turn (root's or a subagent's) running in
            // its daemon, possibly off screen; the kernel names the
            // conversation (its daemon's threads share that root and this
            // Codex is the only client).
            "daemon-turn" => {
                "a turn is running in this tab's Codex conversation (its own, or a subagent's it \
                 started)"
            }
            // The same, a GOAL being pursued (its footer's `Pursuing goal`):
            // the wait aterm may lift
            // under `upgrade_ask = true`, by pausing the goal briefly at the
            // ladder's Land rung.
            "goal" => "a goal is running in this tab's Codex",
            // The goal pause (`upgrade_codex::goal_step`): aterm paused the
            // goal (its last turn still running) or is resuming it.
            "goal-held" | "goal-pausing" => {
                "aterm paused its goal for a moment to install the new Codex"
            }
            "goal-resume" | "goal-resuming" => {
                "aterm is resuming the goal it paused to install the new Codex"
            }
            "goal-left-paused" => {
                "aterm paused its goal to install the new Codex and has not been able to resume it"
            }
            // Left paused on purpose (`upgrade_codex::SANDBOXED`).
            "goal-sandboxed" => {
                "aterm paused its goal to install the new Codex and leaves it paused: its thread \
                 fell into a sandbox"
            }
            w if w.starts_with("goal:") => "a goal is running in this tab's Codex",
            // A turn the lane cannot place (own conversation not named): never
            // said as this tab's, never a goal to pause here, and never
            // presumed another session's (it is often this conversation's
            // subagent).
            "daemon-busy" => {
                "a turn is running on its Codex daemon that may be this tab's own (a subagent of \
                 this conversation) or another Codex session's"
            }
            "no-daemon" => "its Codex daemon is restarting",
            "no-shell-integration" => {
                "its tab's shell integration is not reaching aterm (as after an aterm update), \
                 so once Codex quits aterm cannot tell which conversation to resume"
            }
            "screen-unreadable" => "aterm cannot read its screen",
            "settling" => "waiting for a quiet moment between its turns",
            // A click or scroll stamps the tab as a keystroke does, in Codex
            // and Claude Code tabs alike.
            "attended" => "someone is using its tab",
            "busy" | "not-idle:busy" | "not-idle" => "its turn is still running",
            "daemon-first:busy-thread" => {
                "its Codex daemon runs a turn in some thread, one with no tab (run in the \
                 background) too; `codex agents` lists them"
            }
            "daemon-first:background-terminal" => {
                "a background terminal still runs in its Codex daemon (`/ps` in the Codex that \
                 started it lists it, `/stop` closes it)"
            }
            "daemon-first:owner-held" => {
                "the owner's word on another Codex tab keeps their shared daemon where it is"
            }
            "daemon-first:unseen-client" | "daemon-first:clients-unreadable" => {
                "a Codex this window does not see (another window, a pane) is attached to their \
                 shared daemon"
            }
            "daemon-first:attended" => "a person is at a Codex tab on the same daemon",
            "daemon-first:held" => "a Codex tab on the same daemon is held",
            "daemon-first:update-failed" => {
                "moving its Codex daemon failed; it is tried again in half an hour"
            }
            "daemon-first:vendor-ahead" => {
                "its Codex daemon is on a newer build than the managed one"
            }
            "background-terminal" => {
                "a background terminal still runs under it (`/ps` lists it, `/stop` closes it)"
            }
            // Its step was taken at a break of its own background work: the
            // notice may go there, nothing else.
            "background" => {
                "its own background work still runs (`/ps` lists a background terminal, `/stop` \
                 closes it); the move waits for it to finish"
            }
            "left-typed-backoff" => {
                "the last text it typed there did not go (the composer's row moved); it is tried \
                 again in half an hour, or at once on `--now`"
            }
            // The lane's own line with a person's keys among it: theirs.
            "left-typed:mixed" => {
                "the harness's own text is still in its composer, beside a person's typing"
            }
            "left-typed:screen-unreadable" => "aterm cannot read its screen",
            // Alone, or hidden by a box: never taken back, never sent by the
            // supervisor.
            w if w.starts_with("left-typed") => {
                "the harness's own text is still in its composer, unsent"
            }
            // The waits every agent's step says alike (a draft, a box, a
            // hold, a limit, the login wall).
            w => return claude_wait_words(w),
        })
    }

    /// Whether only the ladder's own comfort holds this wait (the settle, a
    /// person near the tab, the agent's own turn at a look, a status its idle
    /// screen does not bear out, nothing recorded yet, nothing holding it at
    /// all, [`upgrade::NEXT_POINT`]) and no floor: [`Self::waiting_line`]
    /// words it as the ladder's promise. The waiting record keeps a floor's
    /// sentence over it while the upgrade waits, so it does not flip between
    /// the two.
    ///
    /// So does a Claude Code release's gate word a look recorded after its
    /// release was dropped (`release:<gate>` with no release owed,
    /// [`Self::recorded_wait_words`]): nothing owes that line, the gate held
    /// only it, and the move is back on the ladder's ordinary course.
    ///
    /// NOT A STATUS THAT LAGS UNDER THE QUIET FLOW ([`Self::quiet`], round
    /// eight, #5): the ask flow's notice goes through the lag, and its turn
    /// rewrites Claude's status — comfort, there. The quiet flow types
    /// nothing, so nothing rewrites it: the restart waits on Claude's own
    /// `idle` or the owner's word (`upgrade::quiet_step`), and the record
    /// that said "nothing to do: it starts on its own" of it was false.
    #[must_use]
    pub fn waits_for_comfort(&self) -> bool {
        matches!(
            self.wait.as_str(),
            "" | "settling"
                | "attended"
                | "busy"
                | "not-idle"
                | "not-idle:busy"
                | "in-flight"
                | upgrade::NOTICE_SHOWN
                | upgrade::NEXT_POINT
        ) || (self.wait.starts_with("status-stale") && !self.quiet)
            || (types_release(self.agent)
                && self.wait.starts_with("release:")
                && self.release.is_empty())
    }

    /// Whether the owner's `--now` moves this wait once it is overdue
    /// ([`Self::remedy`]: [`Remedy::Now`]): its own text left typed backing
    /// off, a notice behind a full queue, aterm's own refused notice
    /// ([`now_moves_past`]) — and, under the quiet flow ([`Self::quiet`]), a
    /// status of Claude's own that only lags its idle screen, which the quiet
    /// restart reads past on that word alone (`upgrade::quiet_step`).
    fn now_moves(&self) -> bool {
        now_moves_past(&self.wait) || (self.quiet && self.wait.starts_with("status-stale"))
    }

    /// THE WAITING RECORD'S SENTENCE (ruling 380, as amended by the review of
    /// 2026-09-28): worded by WHAT HOLDS the move, and never promising the
    /// first pause in the person's typing where another floor stands (it said
    /// that of a goal-mode Codex whose daemon ran its turns). `from` is the
    /// ladder's Land rung on the person's clock ([`Self::lands_by`]). Where
    /// only the ladder's own comfort holds it — the settle, a person near
    /// the tab, the agent's own turn at a look, nothing recorded yet — it is
    /// the ladder's promise: a quiet moment, and from `from` the first moment
    /// the agent is idle and nobody typed for 20 s. Where a floor stands
    /// (a draft, a dialog, a goal or turn in this tab's Codex daemon, a turn
    /// in another session there, the shell integration, work under it, a
    /// hold, a limit, the login), it names it, and when it moves.
    #[must_use]
    pub fn waiting_line(&self, from: &str) -> String {
        // THE GOAL PAUSE, under
        // `upgrade_ask = true` only (the ask flow: not [`Self::quiet`]): a
        // goal holds the move until the Land rung, where aterm pauses it for
        // a moment and resumes it after the move — nothing for the owner to
        // do. Under the default quiet flow no new pause is made (a pause
        // already made is still resumed), so a goal holds the move for as
        // long as it runs. Aterm's own words left in its composer are no
        // goal's to wait out: only a person moves them, so they are said as
        // they are, below, whatever the goal's hold.
        if holds_goals(self.agent) && !self.wait.starts_with("left-typed:") {
            if self.goal_held_since != 0 {
                return "aterm paused its goal for a moment to install the new Codex, and \
                        resumes it right after: nothing to do"
                    .to_string();
            }
            if self.goal_rest_until != 0 {
                let next = if !self.quiet {
                    "and tries again later (nothing to do)"
                } else {
                    "and it moves once the goal stops: `/goal pause` in this tab stops it"
                };
                return format!(
                    "the goal aterm paused to install the new Codex is running again: the move \
                     did not come in time, so aterm resumed it, {next}"
                );
            }
            if self.wait == "goal" || self.wait.starts_with("goal:") {
                return if !self.quiet {
                    format!(
                        "a goal is running in this tab's Codex; from {from} aterm pauses the goal \
                         for a moment to install the new Codex, and resumes it right after: \
                         nothing to do"
                    )
                } else {
                    "a goal is running in this tab's Codex; it moves once the goal stops: \
                     `/goal pause` in this tab stops it"
                        .to_string()
                };
            }
        }
        if self.waits_for_comfort() {
            return format!(
                "nothing to do: it starts on its own at a quiet moment in the tab, and from \
                 {from} as soon as it is idle and nobody has typed there for {} seconds",
                upgrade::KEYS_GAP_S
            );
        }
        let when = match self.wait.as_str() {
            "daemon-turn" => "it moves as soon as that turn is over",
            "daemon-busy" => {
                "it moves once that turn is over, or once aterm can tell it runs in another tab"
            }
            "no-shell-integration" => {
                "to move it now, quit Codex in the tab and resume it with the `codex resume` line \
                 it prints"
            }
            "screen-unreadable" => "it moves once aterm can read the tab again",
            "draft" => "it moves once that draft is sent or cleared",
            "box" => "it moves once that choice is made",
            "held" => "it moves once the tab is let go",
            NOT_FOREGROUND => "it moves once it is back in front in its tab (`fg` there)",
            CLAIMED => "it moves once that supervisor lets the tab go",
            AUTOPILOT_OFF => "it moves once its autopilot is cruise or land again",
            AGENT_OFF => "it moves once its agent is switched on again (Settings ▸ Harness)",
            "limited" if self.spend_held() => "it moves once you add credits or raise the cap",
            "limited" => "it moves once the limit resets",
            "login" => "it moves once it is logged in again",
            // The quiet flow's restart never reads a lagging status past on
            // its own: Claude's own `idle`, or the owner's
            // word.
            w if w.starts_with("status-stale") && self.quiet => {
                "it moves once Claude Code says it is idle again, or at once on the owner's \
                 word to upgrade now"
            }
            // Aterm's notice stopped PART-WAY (a person's key between its
            // keys, a screen switch, its deadline — or typed whole, its
            // guarded Enter unpressed): the part stays in the prompt — aterm
            // never takes it back (a person's key may be on its way) and the
            // supervisor never sends it — never read as a person's draft, and
            // never "nothing to do": a person sends or clears it.
            w if w.starts_with("announce-refused:cut-") => {
                "the part it left in its prompt waits to be sent or cleared there"
            }
            // Aterm's own words left in the prompt, never taken back or sent
            // by the supervisor: a person sends or clears them (theirs, if
            // their keys are among them).
            "left-typed:mixed" => "it moves once that draft is sent or cleared",
            "left-typed:screen-unreadable" => "it moves once aterm can read the tab again",
            w if w.starts_with("left-typed:") => match self.agent {
                // Codex's: a person CLEARS it — a sent `/exit` ends Codex with
                // no relaunch (its phase was put back), a sent `/goal pause`
                // pauses a goal nothing then resumes.
                upgrade::Agent::Codex => "it moves once that text is cleared in its tab",
                upgrade::Agent::Claude => "it moves once they are sent or cleared in its tab",
            },
            // Aterm's own notice, which its fence would not type (the
            // no-stall branch's words say why): tried again, nothing to do.
            w if w.starts_with("announce-refused:") => {
                "it tries again at the next quiet moment: nothing to do"
            }
            _ => "it moves on its own once that is over",
        };
        match self.recorded_wait_words() {
            Some(what) => format!("{what}; {when}"),
            None => unworded_waiting_line(&self.wait).to_string(),
        }
    }

    /// Whether no look has seen the wait lately ([`ATTENDED_SEEN_S`]): the last
    /// look that recorded it ([`Self::wait_seen`]), else its start.
    fn attended_unseen(&self, now: u64) -> bool {
        now.saturating_sub(self.wait_seen.max(self.wait_since)) > ATTENDED_SEEN_S
    }

    /// Whether the row's words name a person at the tab at `now`
    /// ([`Self::wait_words`]): a Claude Code `attended` wait seen lately.
    #[must_use]
    pub fn person_seen(&self, now: u64) -> bool {
        attended_is_person(self.agent) && self.wait == "attended" && !self.attended_unseen(now)
    }

    /// The unix second this row's words stop naming a person at the tab
    /// ([`Self::person_seen`]), if they name one.
    fn person_unseen_at(&self) -> Option<u64> {
        (attended_is_person(self.agent) && self.wait == "attended").then(|| {
            self.wait_seen
                .max(self.wait_since)
                .saturating_add(ATTENDED_SEEN_S + 1)
        })
    }

    /// THE MOVE WAITS ONLY ON THE PERSON AT ITS TAB, in the words its tab's
    /// own footer shows under the composer they use (a keystroke within
    /// [`upgrade::KEYS_GAP_S`] holds even the Land rung, so the screen must
    /// say why a READY agent has not moved). Said while the move —
    /// still owed, and moving ([`Self::moving`]: no word of the owner's holds
    /// it, nothing but its age stalls it) — waits on the person (Claude
    /// Code's lane alone): `attended` with the person NAMED as of the row's
    /// look ([`Self::person_named`]), or a `draft` in the composer. What it
    /// says is what HOLDS the move, never when it goes: `attended` is any
    /// click, scroll, paste or keystroke (`human_ms`) within `[harness]
    /// human_grace_s` below [`upgrade::Rung::KeysOnly`] and within
    /// [`upgrade::KEYS_GAP_S`] from it, and after the person the gate still
    /// asks for no draft, no box and the agent idle — so `Claude Code 2.1.286
    /// is ready · waits while you use this tab` once the agent agreed
    /// ([`Self::ready`]), else `Claude Code 2.1.286 waits while you use this
    /// tab`, at every rung; and, for the composer the window reads holding a
    /// draft, `… waits until your draft is sent or cleared`
    /// ([`footer::UpgradeNote`]: the gate asks for the person first and the
    /// draft after, so the note follows a person who types a prompt through
    /// the pause that leaves it unsent). A row the draft held has no words
    /// for an empty composer: the draft was sent — the turn it started holds
    /// the move — or cleared, and the next look says what holds it. `None`
    /// otherwise — gone with the move's next step, with any other wait, and
    /// once no look has seen the person lately, the instant the host hands
    /// the row over again ([`next_change`]). Read from the row's words alone,
    /// never its clocks ([`Self::wait_seen`] moves at every look), so the
    /// footer changes only when the note does.
    ///
    /// [`footer::UpgradeNote`]: crate::harness::footer::UpgradeNote
    #[must_use]
    pub fn pause_note(&self, now: u64) -> Option<crate::harness::footer::UpgradeNote> {
        use crate::harness::footer::{UpgradeNote, UpgradeWords};
        let owed = matches!(self.phase, Phase::Pending | Phase::Announced { .. });
        let footed = match self.agent {
            upgrade::Agent::Claude => true,
            upgrade::Agent::Codex => false,
        };
        if !(owed && footed && self.moving(now)) {
            return None;
        }
        let using = match self.wait.as_str() {
            "attended" if self.person_named => true,
            "draft" => false,
            _ => return None,
        };
        let product = self.agent.product();
        // A same-build restart moves the model, never the build
        // ([`Self::move_words`]): no version to name.
        let build = (self.from.is_empty() || self.from != self.to).then(|| word(&self.to));
        let name = match (&build, self.model.is_empty()) {
            (Some(to), _) => format!("{product} {to}"),
            (None, false) => format!("{product} with {}", word(&self.model)),
            (None, true) => product.to_string(),
        };
        let joint = if self.ready { " is ready \u{b7} " } else { " " };
        let words = |holds: &str| UpgradeWords {
            whole: format!("{name}{joint}{holds}"),
            short: build
                .as_ref()
                .map_or_else(|| holds.to_string(), |to| format!("{to} {holds}")),
        };
        Some(UpgradeNote {
            using: using.then(|| words("waits while you use this tab")),
            draft: words("waits until your draft is sent or cleared"),
        })
    }

    /// WHAT THE OWNER CAN DO about this stall, by its kind ([`Remedy`]);
    /// `None` while it is not stalled. `--now` moves only two kinds: a
    /// held-back agent is
    /// never reached (the terminal check returns before the word is read) and
    /// a refused or failed one stays stopped. And an `overdue` one is moved by
    /// `--now` only while it waits on what `--now` waives or on the turn end
    /// it moves at ([`now_moves_past`]); one waiting on the agent's READY
    /// answer, a draft, a box, a hold or work under it is told what it waits
    /// on instead ([`Remedy::Waits`]).
    ///
    /// A given-up round is no stall, but `--now` re-arms it at once rather
    /// than at [`Self::retry_at`], so its remedy is [`Remedy::AskAgain`] and
    /// the window keeps `Upgrade now` for it.
    #[must_use]
    ///
    /// Before its rest ends it reads `overdue` once that far behind, as every
    /// round does, while its step waits `failed` — which `--now` does move:
    /// until ruling 283 that read [`Remedy::Waits`], and the row said
    /// `Upgrade now` could not move it and dropped the capsule that does.
    ///
    /// SAVE A QUIET ROUND ITS AGENT'S OWN WORK GAVE UP ON
    /// ([`Self::rests_on_own_work`]): `--now`
    /// re-arms it, and the round it re-arms waits on that very work — the
    /// quiet restart's gate waits `background` while anything runs under the
    /// agent or at a break of its work, whatever the owner said — so it is
    /// [`Remedy::Waits`], its row naming the work, never `Upgrade now`, which
    /// moved nothing and then read "does not move it past that wait".
    pub fn remedy(&self, now: u64) -> Option<Remedy> {
        let stall = self.stall(now);
        // Nothing has checked it: aterm's own failure, which no owner word
        // mends. The words still hold it.
        if stall.as_deref() == Some("unwatched") {
            return Some(Remedy::Waits);
        }
        if matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) {
            if self.owner_holds(now) {
                return None;
            }
            return Some(if self.rests_on_own_work() {
                Remedy::Waits
            } else {
                Remedy::AskAgain
            });
        }
        let stall = stall?;
        Some(match stall.as_str() {
            // The goal left paused: `/goal resume` in the tab, or, its thread
            // fallen into a sandbox, a relaunch.
            "goal-paused" | "goal-sandboxed" => Remedy::ByHand,
            // Aterm's own fence fails under the owner's `--now`: pressing it
            // again only arms another refused round. The owner can hold the
            // move; aterm keeps trying at the next quiet moment.
            "overdue" if self.fence_fails_under_now() => Remedy::Waits,
            "overdue" if self.now_moves() => Remedy::Now,
            "overdue" => Remedy::Waits,
            s if s.starts_with("held-back:") => Remedy::InItsPane,
            // A restart under way: nothing the owner says moves it
            // ([`ask`] refuses a word then), and nothing is forced.
            s if s.starts_with("stuck:") => Remedy::Waits,
            _ if self.failed_after_exit() => Remedy::ResumeInTab,
            _ => Remedy::ByHand,
        })
    }

    /// A ROUND THAT GAVE UP UNDER THE QUIET FLOW ON ITS AGENT'S OWN WORK
    /// [`Self::quiet`], the round
    /// [`upgrade::GAVE_UP`], and that work seen since its notice
    /// ([`Self::work_seen`]) or running under the agent at the last look
    /// ([`Self::held_by`]). The quiet flow gives a round up only once the
    /// agent's own work — what runs under it, a break of it, or a status its
    /// idle screen does not bear out — has held its restart a whole re-ask
    /// window (`upgrade::quiet_step`), and the owner's `--now` reads only the
    /// last of them past: the round it re-arms waits on the others as before.
    /// So this round moves once that work ends (its end re-arms the round at
    /// the next idle point), never at the owner's word ([`Self::remedy`]). A
    /// give-up a lagging status alone held is not one: `--now` moves it.
    ///
    /// WHAT HOLDS IT NOW, AND CLAUDE CODE'S LANE ALONE. `work_seen` says the
    /// work was seen once since the notice, never that it still runs: once it
    /// has ended, the end's re-arm asks the restart's gate, which never reads
    /// a status that only lags past on its own, so a round whose status still
    /// says `shell` after its shells ended records `status-stale` and does not
    /// move — while the owner's `--now` reads that lag past
    /// (`upgrade::quiet_step`). A recorded wait `--now` moves past
    /// ([`Self::now_moves`]) is therefore no wait on the work; what runs under
    /// the agent at the last look ([`Self::held_by`]) always is. And the Codex
    /// lane has no end-of-work re-arm at all (its reducer reads no
    /// `work_ended`, and a resting round's visit waits `failed` until
    /// [`upgrade::RETRY_S`]): its give-up waits out the rest whatever the
    /// TUI's work does, and `--now` re-arms it to move at the first idle point
    /// with nothing under it — [`Remedy::AskAgain`].
    #[must_use]
    pub fn rests_on_own_work(&self) -> bool {
        let rearms_at_work_end = match self.agent {
            upgrade::Agent::Claude => true,
            upgrade::Agent::Codex => false,
        };
        self.quiet
            && rearms_at_work_end
            && matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP)
            && (!self.held_by.is_empty() || (self.work_seen && !self.now_moves()))
    }

    /// Whether this row's phase is one a live process must still hold for it
    /// to mean anything ([`Self::standing`]). A finished move and a restart
    /// in flight are not — a record, and a move bounded by
    /// [`super::STALE_S`] whose old process is meant to be gone — and nor is
    /// a move that failed after its agent was seen gone
    /// ([`Self::failed_after_exit`]): the agent a holder would be is gone by
    /// construction.
    fn held_by_a_process(&self) -> bool {
        matches!(
            self.phase,
            Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
        ) && !self.failed_after_exit()
    }

    /// A move that failed after its agent was seen gone (Codex `/exit`, Claude
    /// Code SIGTERM; [`Self::exited_at`]): the tab's record, shown for
    /// [`EXITED_FAILURE_SHOWN_S`] by the host.
    #[must_use]
    pub fn failed_after_exit(&self) -> bool {
        self.exited_at != 0 && matches!(self.phase, Phase::Failed(_))
    }

    /// A Claude Code move that failed after its exit, held again: once a live
    /// process holds its conversation (resumed by hand, anywhere, any build) it
    /// is an ordinary stopped upgrade, shown only for a behind holder in its
    /// tab ([`Self::standing`]) with the remedy by hand. (A Codex move's
    /// holder is the tab's Codex TUI, whose lane retires the record once
    /// current.)
    fn held_again(&mut self, holders: &[Holder]) {
        if held_again_after_exit(self.agent)
            && self.failed_after_exit()
            && holders.iter().any(|h| h.session == self.session)
        {
            self.exited_at = 0;
        }
    }

    /// Whether the owner sees or counts this upgrade now: a finished move or a
    /// restart in flight always; any other only while a live process holds
    /// its conversation in its tab on a build older than its target
    /// ([`Holder`]). Ended, hand-moved and elsewhere-resumed conversations read
    /// `false`; their files stay. `unproven_tab`: whether a holder in no proven
    /// tab still counts (the CLI's view, which has no roster). A row with no
    /// recorded tab (older builds recorded it only at the announcement) stands
    /// in whichever tab its holder is proven in ([`Self::proven_tab`]).
    #[must_use]
    pub(super) fn standing(&self, holders: &[Holder], unproven_tab: bool) -> bool {
        if !self.held_by_a_process() {
            return true;
        }
        self.behind_holders(holders)
            .any(|h| self.held_here(h, unproven_tab))
    }

    /// Whether live holder `h` holds this row's conversation in its tab:
    /// proven there (anywhere, for a row with no recorded tab), or with
    /// `unproven_tab` in no proven tab ([`Self::standing`]).
    fn held_here(&self, h: &Holder, unproven_tab: bool) -> bool {
        h.tab
            .as_deref()
            .map_or(unproven_tab, |t| self.tab.is_empty() || t == self.tab)
    }

    /// Whether at least one, and every, live holder of this conversation in
    /// its tab is proven not in front of its terminal ([`Holder::aside`]); a
    /// holder in front is the tab's agent.
    fn held_aside(&self, holders: &[Holder], unproven_tab: bool) -> bool {
        let mut here = self
            .behind_holders(holders)
            .filter(|h| self.held_here(h, unproven_tab))
            .peekable();
        here.peek().is_some() && here.all(|h| h.aside)
    }

    /// This row set aside by `why` ([`Self::aside`]: [`NOT_FOREGROUND`],
    /// [`CLAIMED`], [`AGENT_OFF`] or [`AUTOPILOT_OFF`]), read again at `now`. A move still
    /// owed reads `why` as its wait, as a first look would record it (earlier
    /// words cannot have been renewed). Since when is unknown (`0`, shown
    /// `-`) unless a visit recorded that word.
    fn go_aside(&mut self, why: &str, now: u64) {
        if !self.held_by_a_process() {
            return;
        }
        why.clone_into(&mut self.aside);
        if matches!(self.phase, Phase::Pending | Phase::Announced { .. }) && self.wait != why {
            why.clone_into(&mut self.wait);
            self.wait_since = 0;
        }
        self.read_at(now);
        self.person_named = self.person_seen(now);
    }

    /// Sets this row aside if any hand but a host's holds its tab; the one
    /// rule both the window's view and `--status` apply: every live holder in
    /// its tab not in front ([`NOT_FOREGROUND`]; `unproven_tab` as in
    /// [`Self::standing`]), else a tab another supervisor claims
    /// ([`CLAIMED`]), else a tab whose agent its own switch holds off
    /// ([`AGENT_OFF`]: the switch wins over the session's word), else a tab
    /// whose Autopilot is Off ([`AUTOPILOT_OFF`]), each as `aside` names them.
    /// Never a conversation running outside aterm ([`Self::outside`]).
    fn set_aside(&mut self, held: &[Holder], unproven_tab: bool, aside: &Aside, now: u64) {
        if self.held_aside(held, unproven_tab) {
            self.go_aside(NOT_FOREGROUND, now);
        } else if !self.outside && aside.claimed.contains(&self.tab) {
            self.go_aside(CLAIMED, now);
        } else if !self.outside && aside.agent_off.contains(&self.tab) {
            self.go_aside(AGENT_OFF, now);
        } else if !self.outside && aside.off.contains(&self.tab) {
            self.go_aside(AUTOPILOT_OFF, now);
        }
    }

    /// Keeps the aside of the row `shown` on this one, read at `now` (a
    /// pressed word's answer laid over its tab's row): the answer is read from
    /// the record alone, and the tab's agent is as far from the host as its
    /// last look found. Same conversation only.
    pub fn keep_aside(&mut self, shown: &Row, now: u64) {
        if !shown.aside.is_empty() && shown.session == self.session {
            self.go_aside(&shown.aside, now);
        }
    }

    /// Lets a row no look has reached read Claude's live status. Only a step
    /// records a wait, and the window steps only at an idle screen or a
    /// break, so a session minted behind at attach (`note_behind`), or whose
    /// last step acted, keeps an empty wait through a long question box or
    /// turn and would be offered `Upgrade now`, which cannot move a non-idle
    /// session. So a Claude Code row, pending or announced, with an empty
    /// recorded wait and a live holder in its tab (`holders`, `unproven_tab`
    /// as [`Self::standing`]) whose status is not `idle` reads the word its
    /// first look would record (`not-idle:<status>`, [`upgrade::wait_word`])
    /// since that status began (never before the session was behind):
    /// `not-idle:waiting` ([`Remedy::Waits`]), `not-idle:shell`,
    /// `not-idle:busy` (which `--now` moves at its next turn end). Never over
    /// a recorded word. The owner's view only; no gate reads it.
    ///
    /// Returns whether the row follows that status (a live holder in its tab
    /// and no recorded word): the host then looks again whenever that tab's
    /// screen moves ([`View::follows`]), since no instant names the change.
    fn unreached_status(&mut self, holders: &[Holder], unproven_tab: bool, now: u64) -> bool {
        // A Codex holder has no status ([`Holder`]).
        let has_status = match self.agent {
            upgrade::Agent::Claude => true,
            upgrade::Agent::Codex => false,
        };
        if !has_status
            || !matches!(self.phase, Phase::Pending | Phase::Announced { .. })
            || !self.wait.is_empty()
        {
            return false;
        }
        let Some((status, since)) = self
            .behind_holders(holders)
            .find(|h| self.held_here(h, unproven_tab))
            .map(|h| (h.status.clone(), h.status_since))
        else {
            return false;
        };
        if !status.is_empty() && status != "idle" {
            self.wait = upgrade::wait_word("not-idle", &status);
            self.wait_since = since.max(self.behind_since);
            self.read_at(now);
        }
        true
    }

    /// The tab a behind live holder of this conversation is proven to run in:
    /// where a row with no recorded tab is shown, and what the owner's word
    /// may name.
    pub(super) fn proven_tab(&self, holders: &[Holder]) -> Option<String> {
        self.behind_holders(holders).find_map(|h| h.tab.clone())
    }

    /// The live holders of this conversation on a build older than its
    /// target, or for a same-build restart (`from == to`, a model move due,
    /// [`crate::harness::upgrade_models`]) on that build, since a `--model`
    /// relaunch moves the conversation without moving its build.
    fn behind_holders<'a>(&'a self, holders: &'a [Holder]) -> impl Iterator<Item = &'a Holder> {
        let to = Version::parse(&self.to);
        let same_build = to.is_some() && Version::parse(&self.from) == to;
        holders.iter().filter(move |h| {
            h.session == self.session
                && to.as_ref().is_some_and(|to| {
                    Version::parse(&h.version).is_some_and(|v| v < *to || (same_build && v == *to))
                })
        })
    }

    /// The one word a roster carries for this upgrade, `<state>/<to>/<why>/<age>`
    /// (`/` never appears in a version or a wait word): `state` is `pending`,
    /// `announced`, `restarting`, `deferred`, `skipped`, `stalled` or `done`;
    /// `why` the wait, the stall or `-`; `age` how long the session has been
    /// behind (for `done`, how long ago it finished) as [`upgrade::span`]
    /// spells it. `-` for nothing (`pending/2.1.282/settling/3m`).
    #[must_use]
    pub fn column(&self, now: u64) -> String {
        let to = word(&self.to);
        let behind = upgrade::span(now.saturating_sub(self.behind_since));
        let (state, why, age) = match &self.phase {
            Phase::Done if self.restarting(now) => ("restarting", "continued".to_string(), behind),
            Phase::Done => (
                "done",
                "-".to_string(),
                upgrade::span(now.saturating_sub(self.finished_at())),
            ),
            // A restart under way that has not moved reads stalled, checked
            // before it reads `restarting`.
            Phase::Exiting { .. } | Phase::Relaunched { .. } => match self.stall(now) {
                Some(stall) => ("stalled", stall, behind),
                None => ("restarting", self.phase.word(), behind),
            },
            _ if self.owner_holds(now) => (
                if matches!(self.request, Request::Skip(_)) {
                    "skipped"
                } else {
                    "deferred"
                },
                self.request.shown(now),
                behind,
            ),
            // A stopped round that asks again on its own (resting after a
            // give-up or a first stop) is pending on its next round, however
            // far behind. `--status` keeps the stall word (`stalled=`) and the
            // stop (`phase=`).
            _ if matches!(self.phase, Phase::Failed(_)) && self.asks_on_its_own(now) => {
                ("pending", self.next_round_word(now), behind)
            }
            // An overdue stall names what it waits on.
            _ => match self.stall(now) {
                Some(stall) if stall == "overdue" && !self.wait.is_empty() => {
                    ("stalled", format!("overdue:{}", self.wait), behind)
                }
                // Nothing has checked it: for how long, read now
                // (`stalled/<to>/unwatched:1h40m/<age>`).
                Some(stall) if stall == "unwatched" => (
                    "stalled",
                    format!(
                        "unwatched:{}",
                        upgrade::span(now.saturating_sub(self.unwatched(now).unwrap_or(now)))
                    ),
                    behind,
                ),
                Some(stall) => ("stalled", stall, behind),
                // A round that gave up rests until its next: pending on that.
                None if matches!(self.phase, Phase::Failed(_)) => {
                    ("pending", self.next_round_word(now), behind)
                }
                None => (
                    if matches!(self.phase, Phase::Announced { .. }) {
                        "announced"
                    } else {
                        "pending"
                    },
                    if self.wait.is_empty() {
                        "-".to_string()
                    } else {
                        self.wait.clone()
                    },
                    behind,
                ),
            },
        };
        format!("{state}/{to}/{}/{age}", word(&why))
    }

    /// `next-round:<span>` until a stopped round's next one, `next-round:due`
    /// once it is due (the next look re-arms it), `ready` for a round that
    /// gave up and acts on a late READY instead ([`Self::late_ready`]); `-`
    /// for any other row.
    fn next_round_word(&self, now: u64) -> String {
        if self.late_ready {
            return "ready".to_string();
        }
        match self.next_round_in(now) {
            Some(0) => "next-round:due".to_string(),
            Some(secs) => format!("next-round:{}", upgrade::span(secs)),
            None => "-".to_string(),
        }
    }

    /// The ladder's rung as `--status`'s `rung=`: [`upgrade::Rung::word`]
    /// while the move is owed (`land` at once under `--now`), `-` for a
    /// restart under way, a finished or a stopped one.
    fn rung_word(&self, now: u64) -> String {
        if !matches!(self.phase, Phase::Pending | Phase::Announced { .. }) {
            return "-".to_string();
        }
        if self.request == Request::Now {
            return upgrade::Rung::Land.word().to_string();
        }
        self.rung(now).word().to_string()
    }

    /// One `--status` line.
    #[must_use]
    pub fn line(&self, now: u64) -> String {
        let dash = |s: &str| {
            if s.is_empty() {
                "-".to_string()
            } else {
                word(s)
            }
        };
        // A wait whose start no look knows (`0`: an agent set aside,
        // [`Self::go_aside`]) says none, never the time since 1970.
        let wait_for = if self.wait.is_empty() || self.wait_since == 0 {
            "-".to_string()
        } else {
            upgrade::span(now.saturating_sub(self.wait_since))
        };
        let next_round = match self.next_round_in(now) {
            Some(0) => "due".to_string(),
            Some(secs) => upgrade::span(secs),
            None => "-".to_string(),
        };
        // The watch's fields: how long ago a look off a point read the record
        // (`looked=`) and which build (`by=`), how long ago the loop last
        // offered a point (`point=`) and the guard withholding the latest
        // (`guard=`), and the next watch (`watch_at=`: `due` once past, `-`
        // when nothing is owed).
        let ago = |at: u64| {
            if at == 0 {
                "-".to_string()
            } else {
                upgrade::span(now.saturating_sub(at))
            }
        };
        let watch_at = match self.watch_at {
            0 => "-".to_string(),
            at if at <= now => "due".to_string(),
            at => upgrade::span(at - now),
        };
        // How often in a row, and for how long, the notice due could not be
        // typed (`St::refused`): ` refused=3/1h40m`, said only while it
        // could not.
        let refused = if self.refused == 0 {
            String::new()
        } else {
            format!(
                " refused={}/{}",
                self.refused,
                upgrade::span(now.saturating_sub(self.refused_at))
            )
        };
        // New fields go last so a reader splitting the line keeps positions;
        // the one field said only sometimes (`refused=`) is the tail.
        format!(
            "upgrade tab={} session={} from={} to={}({}) phase={} pending_for={} wait={} \
             wait_for={wait_for} request={} next_round={next_round} stalled={} held_by={} \
             release={} rung={} looked={} by={} point={} guard={} watch_at={watch_at} cause={} \
             told={}{refused}",
            dash(&self.tab),
            dash(&self.session),
            dash(&self.from),
            dash(&self.to),
            dash(&self.source),
            word(&self.phase.word()),
            upgrade::span(now.saturating_sub(self.behind_since)),
            dash(&self.wait),
            self.request.shown(now),
            self.stall(now)
                .map_or_else(|| "-".to_string(), |s| word(&s)),
            self.held_token(now),
            dash(&self.release),
            self.rung_word(now),
            ago(self.looked_at),
            dash(&self.looked_by),
            ago(self.point_at),
            dash(&self.guard),
            self.cause_word(now),
            word(&self.told_word(now)),
        )
    }

    /// The JSON form of [`Self::line`], the seconds raw.
    #[must_use]
    pub fn to_json(&self, now: u64) -> Value {
        let mut o = Map::new();
        o.insert("schema".into(), Value::from(1u64));
        o.insert("kind".into(), Value::from("upgrade-status"));
        for (k, v) in [
            ("tab", self.tab.clone()),
            ("session", self.session.clone()),
            ("from", self.from.clone()),
            ("to", self.to.clone()),
            ("source", self.source.clone()),
            ("phase", self.phase.word()),
            ("wait", self.wait.clone()),
            ("request", self.request.word()),
            ("outcome", self.outcome.clone()),
            ("stalled", self.stall(now).unwrap_or_default()),
            ("model", self.model.clone()),
            ("agent", self.agent.product().to_string()),
            // The release still owed ([`Self::release`]); empty: none.
            ("release", self.release.clone()),
            ("rung", self.rung_word(now)),
            ("looked_by", self.looked_by.clone()),
            ("guard", self.guard.clone()),
            // What holds the move, by class, and what the tab's mark tells
            // of it (`-`: nothing told).
            ("cause", self.cause_word(now).to_string()),
            ("told", self.told_word(now)),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        for (k, v) in [
            ("pending_for_s", now.saturating_sub(self.behind_since)),
            (
                "wait_for_s",
                if self.wait.is_empty() || self.wait_since == 0 {
                    0
                } else {
                    now.saturating_sub(self.wait_since)
                },
            ),
            ("done_at", self.done_at),
            // When a stopped round's next one starts (0: not stopped), and
            // the seconds until then (0: due, or none coming — not stopped,
            // held by the owner's word, or acting on a late READY; the line's
            // `next_round=` tells them apart).
            ("retry_at", self.retry_at),
            ("next_round_s", self.next_round_in(now).unwrap_or(0)),
            // The watch's stamps, raw (0: none), and when the record is
            // watched next (0: nothing owed).
            ("progress_at", self.progress_at),
            ("looked_at", self.looked_at),
            ("point_at", self.point_at),
            ("watch_at", self.watch_at),
            // The notice due that could not be typed, in a row, and since
            // when (0: none).
            ("refused", u64::from(self.refused)),
            ("refused_at", self.refused_at),
            // The usage limit's named reset the transcript said at the last
            // look (0: none).
            ("limit_until", self.limit_until),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        // What held the move at the last look that waited, each by pid, name
        // and age — never its command.
        let held = self
            .held_by
            .iter()
            .map(|h| {
                let mut p = Map::new();
                p.insert("pid".into(), Value::from(u64::from(h.pid)));
                p.insert("name".into(), Value::from(h.name.as_str()));
                p.insert("age_s".into(), Value::from(now.saturating_sub(h.since)));
                Value::Object(p)
            })
            .collect();
        o.insert("held_by".into(), Value::Array(held));
        // The wait log, newest last: when it began to wait on what, and the
        // facts that decided it.
        let waits = self
            .waits
            .iter()
            .map(|w| {
                let mut p = Map::new();
                p.insert("at".into(), Value::from(w.at));
                p.insert("word".into(), Value::from(w.word.as_str()));
                p.insert("note".into(), Value::from(w.note.as_str()));
                Value::Object(p)
            })
            .collect();
        o.insert("waits".into(), Value::Array(waits));
        // THE FIRST TIME EACH DISTINCT WAIT WAS RECORDED, for the record's
        // life: the log above keeps the last few, and a multi-day stall's
        // first cause scrolls off it.
        let firsts = self
            .firsts
            .iter()
            .map(|(word, at)| {
                let mut p = Map::new();
                p.insert("at".into(), Value::from(*at));
                p.insert("word".into(), Value::from(word.as_str()));
                Value::Object(p)
            })
            .collect();
        o.insert("firsts".into(), Value::Array(firsts));
        // HOW LOUD THE OWNER IS TOLD, and why (`--status` is the introspection
        // of the screen: the class every surface follows is printed here).
        let (class, kind) = match self.attention(now) {
            Attention::Quiet => ("quiet", ""),
            Attention::Fault(n) => ("fault", n.kind),
            Attention::Digest(n) => ("digest", n.kind),
            Attention::Interrupt(n) => ("interrupt", n.kind),
        };
        o.insert("attention".into(), Value::from(class));
        o.insert("attention_kind".into(), Value::from(kind));
        // The class as a sentence ([`Self::overdue_cause`]); `cause` is the
        // class's one word.
        o.insert("cause_words".into(), Value::from(self.overdue_cause(now)));
        Value::Object(o)
    }

    /// What the upgrade moves the session onto, in the owner's words: `Claude
    /// Code <from> → <to>` (`Codex …` for Codex), `… with <model>` once the
    /// announcement named a model, and for a same-build restart (`from ==
    /// to`) `Claude Code <to> → <model>`, or `→ another model` before one is
    /// named.
    #[must_use]
    pub fn move_words(&self) -> String {
        let model = (!self.model.is_empty()).then(|| word(&self.model));
        if !self.from.is_empty() && self.from == self.to {
            return format!(
                "{} {} → {}",
                self.agent.product(),
                word(&self.to),
                model.unwrap_or_else(|| "another model".to_string())
            );
        }
        let with = model.map_or_else(String::new, |m| format!(" with {m}"));
        format!(
            "{} {} → {}{with}",
            self.agent.product(),
            word(&self.from),
            word(&self.to)
        )
    }

    /// `--status`'s sentence under [`Self::line`]: the move, and what holds
    /// it — `done`; for a restart, `its move is under way` unless it is stuck
    /// ([`stuck_words`]) or otherwise stalled; the owner's word while it
    /// holds; else [`Self::overdue_cause`].
    #[must_use]
    pub fn summary(&self, now: u64) -> String {
        let restarting = matches!(self.phase, Phase::Exiting { .. } | Phase::Relaunched { .. });
        let why = match &self.request {
            _ if self.phase == Phase::Done => "done".to_string(),
            _ if restarting => match (self.stall(now), self.stuck(now)) {
                (None, _) => "its move is under way".to_string(),
                (Some(_), Some(what)) => {
                    format!("its restart is stuck: {}", stuck_words(self.agent, what))
                }
                (Some(stall), None) => format!("its restart has stalled ({stall})"),
            },
            _ if self.cause_word(now) == "-" => "its move is under way".to_string(),
            Request::Skip(_) if self.owner_holds(now) => "skipped by the owner's word".to_string(),
            Request::DeferUntil(t) if self.owner_holds(now) => format!(
                "deferred by the owner's word for {}",
                upgrade::span(t.saturating_sub(now))
            ),
            _ => self.overdue_cause(now),
        };
        format!("{}: {why}", self.move_words())
    }

    /// The tab's attention text (at most 200 bytes, one line: the keyed
    /// attention cap), for an INTERRUPT only: the owner is needed now and the
    /// action is theirs alone ([`Attention::Interrupt`]). The mark, the
    /// menu-bar row, the notification and the band row all say the same
    /// `Need`: why the owner, and the one thing to do.
    ///
    /// STABLE FOR AS LONG AS THE KIND IS: a text that
    /// carried a running age (`behind for 7h1m`) re-sent `meta set attention`
    /// every minute — a meta-change event and a wake a minute, and, the newest
    /// stamp being the one shown, the upgrade took the tab's attention back
    /// from any owner that raised it later. No age is in a `Need`'s words
    /// that reach here, and no wait word either.
    ///
    /// A stall is not marked for its age alone (`overdue`): a timer is not an
    /// inability.
    fn badge(&self, now: u64) -> Option<String> {
        // THE TAB IS MARKED FOR AN INTERRUPT ONLY: a fact the owner can act
        // on now. Everything else is the column, `--status` and the log.
        let Attention::Interrupt(need) = self.attention(now) else {
            return None;
        };
        // The band row, the mark, the menu-bar row and the notification say
        // ONE thing: why the owner, and the one action (a `Need`, which moves
        // only when the kind does — no age is in it).
        //
        // THE ACTION IS NEVER CUT (a byte cut in the resume command's id
        // gives a command that finds no conversation): the
        // head — the move and why — gives way, with an ellipsis, before the
        // `then` loses a byte.
        const CAP: usize = MARK_MAX;
        let tail = match &need.command {
            Some(command) => format!(" - {}: {command}", need.then),
            None => format!(" - {}", need.then),
        };
        let room = CAP.saturating_sub(tail.len());
        let head = format!("{}: {}", self.move_words(), need.why);
        let head = if head.len() <= room {
            head
        } else {
            let mut kept = super::super::one_line(&head, room.saturating_sub('…'.len_utf8()));
            kept.push('…');
            kept
        };
        Some(super::super::one_line(&format!("{head}{tail}"), CAP))
    }

    /// WHAT HOLDS AN OVERDUE MOVE, as a class in the owner's words — the
    /// `cause` of `--status --json`, and the classes the digest reads
    /// ([`Self::cause`]). Read from the recorded wait (a
    /// release's own gate, `release:<gate>`, by the gate it waits on) and
    /// what ran under the agent at that look ([`Self::held_by`]):
    ///
    /// * the agent's own work: a turn, a background shell or task, processes
    ///   under it, a status not yet quiet, READY not given, a given-up round
    ///   resting;
    /// * a person: a box, a draft, a login, a person at the tab, a hold;
    /// * a usage limit (ends at its reset), or a Codex save-then-wait switch
    ///   holding the session until then;
    /// * a Codex goal aterm paused and could not resume (its row names the
    ///   hand step);
    /// * its Codex daemon: shared, restarting, or version not yet read;
    /// * aterm itself: a notice it could not type, notices queued unread, a
    ///   tab or conversation it cannot reach, an unreadable screen or shell
    ///   integration, a move already under way, or a usage limit whose named
    ///   reset has passed with its notice still up ([`Self::limit_passed`]);
    /// * a wait this build does not classify, named, and a record with no wait
    ///   recorded, said as such.
    ///
    /// Processes running under the agent outrank passing wait words
    /// (`background`, `announce-refused:changed`, `attended`), so a long stall
    /// on background work keeps one class and the mark is not re-sent. A
    /// standing box, login or limit still names its own class.
    ///
    /// A person no look has seen lately ([`Self::person_gone`]) counts as the
    /// agent's own work, matching the row's words ([`Self::wait_words`]); the
    /// row and mark change once at [`Self::person_unseen_at`].
    fn overdue_cause(&self, now: u64) -> String {
        // A LIMIT WHOSE NAMED RESET HAS PASSED ([`Self::limit_passed`]): aterm's
        // own class, said as what the record read.
        if self.limit_passed(now) {
            return LIMIT_PASSED.to_string();
        }
        if self.spend_held() {
            return SPEND_HELD.to_string();
        }
        // A person's hold of a class of its own, said as whose it is.
        match self.wait.as_str() {
            NOT_FOREGROUND => return "its agent is not in front in its tab".to_string(),
            CLAIMED => return "another supervisor holds its tab".to_string(),
            AUTOPILOT_OFF => return "its autopilot is off".to_string(),
            AGENT_OFF => return "its agent is switched off".to_string(),
            _ => {}
        }
        if self.person_gone(now) {
            return OWN_WORK.to_string();
        }
        match self.cause() {
            Cause::OwnWork => OWN_WORK.to_string(),
            Cause::Person => {
                "waiting on a person (a question, a draft, a login or a hold)".to_string()
            }
            Cause::Limit => "paused by a usage limit until its reset".to_string(),
            Cause::GoalLeftPaused => "its paused goal waits for a person to resume it".to_string(),
            Cause::SharedDaemon => {
                "waiting on its Codex daemon, shared with other tabs".to_string()
            }
            Cause::Daemon => "waiting on its Codex daemon".to_string(),
            Cause::TabUnread => "aterm cannot read its tab well enough to move it".to_string(),
            Cause::UnderWay => "its move is already under way".to_string(),
            Cause::CouldNotType => "aterm could not type its notice".to_string(),
            Cause::Queued => "its notices wait unread behind its queue".to_string(),
            Cause::Unreached => "aterm cannot reach its tab to ask".to_string(),
            Cause::BuildFails => "its new build fails to start".to_string(),
            Cause::NextPoint => NEXT_POINT_CAUSE.to_string(),
            Cause::NoLook => "no look has recorded what it waits on".to_string(),
            Cause::Unclassified(wait) => {
                format!("a wait aterm does not classify ({})", word(&wait))
            }
        }
    }

    /// WHAT HOLDS THE MOVE, by class ([`Cause`]): the recorded wait (a
    /// release's own gate by the gate it waits on) against what ran under the
    /// agent at that look, the processes outranking a PASSING word (see
    /// [`Self::overdue_cause`]).
    fn cause(&self) -> Cause {
        Self::cause_of(&self.wait, !self.held_by.is_empty())
    }

    /// [`Self::cause`] for a recorded `wait`, with processes under the agent
    /// (`held`) or none.
    ///
    /// A PERSON'S DRAFT, A HOLD AND A REFUSED FENCE THAT NAMES ONE ARE NEVER
    /// PASSING: text a person typed and did not send, or a halt or another
    /// driver's lease, stands until they end it, whatever runs under the
    /// agent. What
    /// passes with the look — no word after an act, a person's keystroke, a
    /// refused fence, a resting round — reads as the work under the agent
    /// while any runs, and as the round's remembered class beyond that
    /// ([`Row::works`]).
    fn cause_of(wait: &str, held: bool) -> Cause {
        let wait = wait.strip_prefix("release:").unwrap_or(wait);
        let head = wait.split_once(':').map_or(wait, |(head, _)| head);
        // A person's or a peer's state that outlasts a look.
        let stands = matches!(
            wait,
            "announce-refused:draft" | "announce-refused:held" | "announce-refused:box"
        );
        // The notice path's passing gates, and a resting round's quiet word.
        let passing = !stands && matches!(head, "" | "attended" | "announce-refused" | "failed");
        if passing && held {
            return Cause::OwnWork;
        }
        if stands {
            return Cause::Person;
        }
        match (head, wait_class(wait)) {
            // The owner's own setup: a suspended agent (`^Z`), a multiplexer
            // pane.
            ("not-foreground" | "terminal", _) => Cause::Person,
            (_, WaitClass::OwnWork) => Cause::OwnWork,
            (_, WaitClass::Person) => Cause::Person,
            (_, WaitClass::Limit) => Cause::Limit,
            (_, WaitClass::GoalLeftPaused) => Cause::GoalLeftPaused,
            (_, WaitClass::SharedDaemon) => Cause::SharedDaemon,
            (_, WaitClass::Daemon) => Cause::Daemon,
            (_, WaitClass::TabUnread) => Cause::TabUnread,
            (_, WaitClass::UnderWay) => Cause::UnderWay,
            (_, WaitClass::Notice) => Cause::CouldNotType,
            (_, WaitClass::Queued) => Cause::Queued,
            (_, WaitClass::Unreached) => Cause::Unreached,
            (_, WaitClass::BuildFails) => Cause::BuildFails,
            (_, WaitClass::NextPoint) => Cause::NextPoint,
            (_, WaitClass::Unrecorded) => Cause::NoLook,
            (_, WaitClass::Unclassified) => Cause::Unclassified(wait.to_string()),
        }
    }

    /// The class [`Self::overdue_cause`] words: [`wait_class`] of the recorded
    /// wait, except that processes under the agent outrank a passing word and
    /// a person no look has seen lately counts as the agent's own work. Shared
    /// by the mark, `--status`'s `cause=` ([`Self::cause_word`]) and the
    /// model's projection ([`progress_projection`]) so they agree.
    fn overdue_class(&self, now: u64) -> WaitClass {
        let wait = self.wait.strip_prefix("release:").unwrap_or(&self.wait);
        let head = wait.split_once(':').map_or(wait, |(head, _)| head);
        // The notice path's passing gates, and a resting round's quiet word.
        let passing = matches!(
            head,
            "" | "attended" | "draft" | "held" | "announce-refused" | "failed"
        );
        if (passing && !self.held_by.is_empty()) || self.person_gone(now) {
            WaitClass::OwnWork
        } else if self.spend_held() {
            // Money no reset ends: the owner's hold ([`Self::spend_held`]).
            WaitClass::Person
        } else {
            wait_class(&self.wait)
        }
    }

    /// What holds the move, as one word (`--status` `cause=`, `--json`
    /// `cause`): the class the mark would name ([`Self::overdue_cause`],
    /// [`WaitClass::word`]) for a move still owed, told or not; `-` for a move
    /// under way or done.
    fn cause_word(&self, now: u64) -> &'static str {
        if !matches!(
            self.phase,
            Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
        ) {
            return "-";
        }
        if self.limit_passed(now) {
            return "limit-passed";
        }
        self.overdue_class(now).word()
    }

    /// WHAT THE OWNER IS TOLD, AS ONE WORD (`--status`'s `told=`, `--json`'s
    /// `told`; design record 2026-09-28, §3.2 C8): `-` for a record, however
    /// long it has waited — a wait is a record, never a row or a mark
    /// ([`Self::attention`]) — else the KIND of what the row is: the
    /// interrupt the tab is marked for (`agent-down`, `hung-exit`,
    /// `goal-paused`, `relaunch-stuck`), the digest line (`own-work`,
    /// `no-terminal`, …) or the fault line in the log (`harness-stuck`,
    /// `no-progress`, `limit-passed`, …). What `cause=` beside it cannot say
    /// alone: whether the owner has been told, and where (`attention` in
    /// `--json` names the surface).
    fn told_word(&self, now: u64) -> String {
        match self.attention(now) {
            Attention::Quiet => "-".to_string(),
            Attention::Fault(n) | Attention::Digest(n) | Attention::Interrupt(n) => {
                n.kind.to_string()
            }
        }
    }
}

/// What holds a wait, by class: one table for the overdue mark's cause
/// ([`Row::overdue_cause`]) and the waiting record's sentence for a wordless
/// wait ([`unworded_waiting_line`]), so the two never disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitClass {
    /// The agent's own work.
    OwnWork,
    /// A person: a box, a draft, a login, a person at the tab, a hold.
    Person,
    /// A usage limit, or a Codex save-then-wait switch, until its reset.
    Limit,
    /// A Codex goal aterm paused and could not resume.
    GoalLeftPaused,
    /// Its Codex daemon, shared with other tabs.
    SharedDaemon,
    /// Its Codex daemon: restarting, its version or pin not yet read.
    Daemon,
    /// A tab, screen, shell integration or process aterm cannot read.
    TabUnread,
    /// A move already under way.
    UnderWay,
    /// A notice aterm could not type.
    Notice,
    /// Notices queued unread.
    Queued,
    /// A tab or conversation aterm cannot reach to ask in.
    Unreached,
    /// The build it would move to failed to start, and is not proved to
    /// start since (`upgrade_drive`'s `build_start`).
    BuildFails,
    /// Nothing holds it now ([`upgrade::NEXT_POINT`]): the round's next step
    /// is due at the session's next point. A record, never a row of its own;
    /// the move clock still applies ([`Row::stall`], [`Row::asks_on_its_own`]).
    NextPoint,
    /// No look has recorded a wait.
    Unrecorded,
    /// A wait this build does not classify.
    Unclassified,
}

impl WaitClass {
    /// The class as `--status` says it (`cause=`, [`Row::cause_word`]).
    fn word(self) -> &'static str {
        match self {
            WaitClass::OwnWork => "own-work",
            WaitClass::Person => "person",
            WaitClass::Limit => "limit",
            WaitClass::GoalLeftPaused => "goal-paused",
            WaitClass::SharedDaemon => "shared-daemon",
            WaitClass::Daemon => "daemon",
            WaitClass::TabUnread => "tab-unread",
            WaitClass::UnderWay => "under-way",
            WaitClass::Notice => "notice",
            WaitClass::Queued => "queued",
            WaitClass::Unreached => "unreached",
            WaitClass::BuildFails => "build-fails",
            WaitClass::NextPoint => "next-point",
            WaitClass::Unrecorded => "unrecorded",
            WaitClass::Unclassified => "unclassified",
        }
    }
}

/// The class of `wait` ([`WaitClass`]); a release gate (`release:<gate>`) by
/// the gate it waits on. From the word alone; row state that outranks it is
/// [`Row::overdue_cause`]'s.
fn wait_class(wait: &str) -> WaitClass {
    let wait = wait.strip_prefix("release:").unwrap_or(wait);
    let head = wait.split_once(':').map_or(wait, |(head, _)| head);
    match (head, wait) {
        ("limited", _) => WaitClass::Limit,
        (_, "not-idle:waiting")
        | (
            "box" | "draft" | "login" | "attended" | "held" | "terminal" | NOT_FOREGROUND | CLAIMED
            | AUTOPILOT_OFF | AGENT_OFF,
            _,
        ) => WaitClass::Person,
        (_, "daemon-first:attended" | "daemon-first:held") => WaitClass::Person,
        (
            _,
            "not-idle:busy" | "not-idle:shell" | "status-stale" | "status-stale:busy"
            | "status-stale:shell",
        )
        | (
            "busy"
            | "background"
            | "background-terminal"
            | "not-ready"
            | "awaiting-ready"
            | "ready-unrecorded"
            | "superseded"
            | "settling"
            | "notice-shown"
            | "busy-thread"
            | "failed",
            _,
        ) => WaitClass::OwnWork,
        // The Codex lane's ladder and goal pause: a save-then-wait switch
        // holds the session until the limit's reset; a pursued goal, the
        // pause's own steps and its conversation's daemon turn are own work;
        // a goal left paused waits on the hand step its row names.
        (_, "goal:switch") | ("switch", _) => WaitClass::Limit,
        (
            "goal" | "goal-held" | "goal-pausing" | "goal-pause-refused" | "goal-resume"
            | "goal-resuming" | "daemon-turn" | "changed",
            _,
        ) => WaitClass::OwnWork,
        ("goal-left-paused" | "goal-sandboxed", _) => WaitClass::GoalLeftPaused,
        ("daemon-first" | "daemon-busy", _) => WaitClass::SharedDaemon,
        ("no-daemon" | "daemon-version" | "daemon-env" | "pin", _) => WaitClass::Daemon,
        // What aterm must read of the tab, the agent's process or its
        // conversation before it may type there.
        (
            "no-shell-integration"
            | "screen-unreadable"
            | "threads-ambiguous"
            | "files-unreadable"
            | "session-files-unreadable"
            | "shell-dialect"
            | "ids"
            | "no-process"
            | "argv-unreadable"
            | "exe-unreadable"
            | "version-unreadable"
            | "home-unreadable",
            _,
        ) => WaitClass::TabUnread,
        ("in-flight", _) => WaitClass::UnderWay,
        // Aterm cannot read the tab, so cannot tell whether its words still
        // stand: the tab's unread, as a plain `screen-unreadable` is.
        (_, "left-typed:screen-unreadable") => WaitClass::TabUnread,
        // Aterm's own words left in the prompt, however they stand (alone,
        // beside a person's keys, behind a box, unread): a person's, since
        // only a person moves them — never taken back by either lane, never
        // sent by the supervisor.
        ("left-typed", _) => WaitClass::Person,
        ("announce-refused", _) => WaitClass::Notice,
        ("queued", _) => WaitClass::Queued,
        (
            "tab-not-live"
            | "no-socket"
            | "conversation-in-other-tab"
            | "notice-owned-by-other-process"
            | "tab-ownership-changed"
            | "thread-locked",
            _,
        ) => WaitClass::Unreached,
        (crate::harness::upgrade_drive::BUILD_FAILS, _) => WaitClass::BuildFails,
        (upgrade::NEXT_POINT, _) => WaitClass::NextPoint,
        ("", "") => WaitClass::Unrecorded,
        _ => WaitClass::Unclassified,
    }
}

/// The waiting record's sentence for a wait with no words of its own
/// ([`Row::waiting_line`]), worded by its class ([`wait_class`]), never the
/// raw word. A word no class knows is said as aterm's own step; `--status`
/// names it.
fn unworded_waiting_line(wait: &str) -> &'static str {
    match wait {
        // The composer moved between the typing fence's two reads, or the
        // last look before the SIGTERM found the tab changed.
        "changed" | "changed-before-signal" => {
            "the tab changed just as aterm was about to type there; it tries again at the next \
             quiet moment: nothing to do"
        }
        _ => match wait_class(wait) {
            WaitClass::OwnWork => {
                "its own work is still running; it moves on its own once that is over"
            }
            WaitClass::Person => {
                "it waits on a person (a question, a draft, a login or a hold); it moves once \
                 they are done"
            }
            WaitClass::Limit => {
                "a usage limit holds it until its reset; it moves once the limit resets"
            }
            WaitClass::GoalLeftPaused => {
                "its paused goal waits for a person to resume it; it moves once the goal runs again"
            }
            WaitClass::SharedDaemon => {
                "its Codex daemon, shared with other tabs, is not ready to move yet; it moves on \
                 its own once it is"
            }
            WaitClass::Daemon => {
                "aterm is waiting on its Codex daemon; it moves on its own once the daemon is ready"
            }
            WaitClass::TabUnread => {
                "aterm cannot yet read its tab well enough to move it; it moves on its own once it \
                 can"
            }
            WaitClass::UnderWay => "its move is already under way: nothing to do",
            WaitClass::Notice => {
                "aterm could not type its notice yet; it tries again at the next quiet moment: \
                 nothing to do"
            }
            WaitClass::Queued => {
                "its notices wait unread behind its queue; it moves once it reads them"
            }
            WaitClass::Unreached => {
                "aterm cannot reach its tab to ask yet; it moves on its own once it can"
            }
            WaitClass::BuildFails => {
                "its new build fails to start, so aterm moves no agent onto it; it moves once a \
                 build that starts is installed"
            }
            // Never reached today ([`Row::waiting_line`] words the next point
            // as the ladder's comfort first); said as the mark says it.
            WaitClass::NextPoint => NEXT_POINT_CAUSE,
            WaitClass::Unrecorded | WaitClass::Unclassified => {
                "aterm is waiting on a step of its own; it moves on its own once that is over \
                 (`aterm harness upgrade --status` names the step)"
            }
        },
    }
}

// ---------------------------------------------------------------- what the owner is told

/// How long a session must have been behind THE PROCESS THAT RUNS (never a
/// timer on the build: [`Row::behind_since`] is that process's) before the
/// agent's own work, or a draft nobody sent, becomes ONE line in the
/// exceptions digest. Claude Code ships about daily; a session a full day
/// behind that has answered a whole round of notices with "not yet" is news,
/// once, and nothing sooner is.
pub const DIGEST_AFTER_S: u64 = MOVE_BUDGET_S;

const _: () = assert!(
    BACKSTOP_S == MOVE_BUDGET_S,
    "one day: the digest's, the backstop's and the move clock"
);

/// How long an agent suspended (`^Z`) or put in the background may stand
/// before the digest says so: the person did it, and knows.
pub const SUSPENDED_AFTER_S: u64 = 3_600;

/// How long a wait that is aterm's own fault — its notice could not be typed,
/// or a word this build does not classify — may stand before the digest says
/// so: the design record's `TELL_S` (2026-09-28, §3.2 C6). It is the owner's
/// only as the developer of aterm; the line names where the wait log is. One
/// constant with the notice a fence keeps refusing ([`TELL_S`]).
pub const HARNESS_TELL_S: u64 = TELL_S;

/// How long a SIGTERMed agent may still be alive before the owner is
/// interrupted: three times [`STALE_S`], the bound a shutdown hook or an MCP
/// teardown is given three times over. Nothing harder is ever sent.
pub const HUNG_EXIT_AFTER_S: u64 = 3 * STALE_S;

/// How long the agent's own work is REMEMBERED as what holds a move, after
/// the last look that saw it ([`Row::own_work_at`]): one hour, two re-asks
/// and far more than the longest pause between looks (the ten-minute rung).
/// The class of a round is a property of the ROUND: read from the last look
/// alone it dipped to quiet at every act (a notice, a give-up, a re-arm clear
/// the wait and the list of holders) and at every box or draft a look met in
/// between — a dozen flips in one round, each a new log line and a dismissed
/// entry back.
pub const OWN_WORK_HOLD_S: u64 = 2 * upgrade::REASK_S;

/// THE LONGEST A RECORD MAY GO WITHOUT PROGRESS, whatever it waits on (a
/// notice typed, a round begun, a restart): a day. Past it a record that no
/// class above claims is a fault line — one line in the log, quoting the last
/// waits — never silence, for a session nobody asked for days or one that
/// waits on a box for ever.
pub const BACKSTOP_S: u64 = DIGEST_AFTER_S;

/// The longest a usage limit stands: a weekly limit, plus a day. Past it the
/// wait is a banner that outlived its reset (`upgrade::limited` has no clock
/// until the resume stream's `place_reset` lands), which is a fault, not a
/// limit.
pub const LIMIT_MAX_S: u64 = 8 * 86_400;

/// The longest notices may wait unread behind a full queue: the queue's rest
/// grows to a day ([`upgrade::QUEUE_REST_MAX_S`]), three days is never a rest.
pub const QUEUED_MAX_S: u64 = 3 * 86_400;

/// How long a relaunched agent may go without reaching an idle point before
/// the log says so: three times [`STALE_S`], as a hung shutdown is (the
/// supervisor answers a box within moments; a start-up screen that is
/// neither a box nor idle reaches nobody).
pub const RELAUNCH_STUCK_S: u64 = 3 * STALE_S;

/// How long a relaunched agent may go without reaching an idle point before
/// the OWNER is interrupted: twice [`RELAUNCH_STUCK_S`]. The upgrade ended the
/// agent to move it to a new build, it came back, and nothing has answered
/// whatever it shows — the supervisor answers a box within moments, so a
/// screen that is still neither a box nor idle after half an hour is one only
/// a person can look at.
pub const RELAUNCH_INTERRUPT_S: u64 = 2 * RELAUNCH_STUCK_S;

/// WHETHER THE AGENT'S OWN WORK holds a move for a look that recorded `wait`
/// with (`held`) or without processes under the agent: the one predicate
/// the record's memory of it ([`St::own_work_at`]) and the class
/// ([`Row::cause`]) share.
pub(super) fn holds_by_own_work(wait: &str, held: bool) -> bool {
    Row::cause_of(wait, held) == Cause::OwnWork
}

/// HOW LOUD ONE UPGRADE IS: an alert persists only with a clear reason the
/// owner is needed and one thing to do. Decided by
/// whether the OWNER can do anything — never by how long the session has been
/// behind (a timer is not an inability), never by which phase it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attention {
    /// Nothing for the glass: the upgrade is working, or waiting on something
    /// that is not the owner's — the agent's own work, a person at the tab, a
    /// usage limit, a refused fence, the supervisor's box — and asks again on
    /// its own. The `upgrade=` column, `--status`, the ledger and the wait
    /// log carry it.
    Quiet,
    /// ATERM'S OWN FAULT, which the owner cannot act on: a wait this build
    /// does not classify, a notice its screen keeps refusing, a daemon move
    /// that keeps failing. A RECORD — ONE Warn line in the log, once, for the
    /// owner as the developer to look at later (the Problems filter of the
    /// messages page); never a row, a mark or a digest line, for the owner's
    /// rule is that an alert persists only where they can act. The wait log
    /// and `--status` carry the facts.
    Fault(Need),
    /// The owner should know, only the owner can end it, and nothing is lost
    /// by waiting a day: ONE line in the exceptions digest, which the owner
    /// dismisses into the log.
    Digest(Need),
    /// The owner is needed NOW and the action is theirs alone — the upgrade
    /// ended their agent and it is not back: a row, the tab's mark.
    Interrupt(Need),
}

impl Attention {
    /// This attention as a row carries it ([`Level`]).
    #[must_use]
    pub fn level(&self) -> Level {
        match self {
            Attention::Quiet => Level::Quiet,
            Attention::Fault(n) => Level::Fault(n.kind),
            Attention::Digest(n) => Level::Digest(n.kind),
            Attention::Interrupt(n) => Level::Interrupt(n.kind),
        }
    }
}

/// [`Attention`] AS A ROW CARRIES IT: the class and the kind, never the words
/// (which hold spans that move with the clock). Carried as [`Row::stalled`]
/// is, so a row that crosses a horizon — a wait turning a day old, a shutdown
/// turning hung — is a CHANGED row, handed to the window again at the instant
/// [`next_change`] names. Without it a digest line for a wait that has just
/// turned a day old would wait for some other change to be sent at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    /// [`Attention::Quiet`].
    #[default]
    Quiet,
    /// [`Attention::Fault`] of this kind.
    Fault(&'static str),
    /// [`Attention::Digest`] of this kind.
    Digest(&'static str),
    /// [`Attention::Interrupt`] of this kind.
    Interrupt(&'static str),
}

/// How much a [`Need`] weighs, for the surface that shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeedSeverity {
    /// Worth a line in the log.
    Info,
    /// Something the owner set up stops the upgrade, or it keeps stopping.
    Warn,
    /// The owner's agent is not running because of the upgrade.
    Error,
}

/// WHY THE OWNER IS NEEDED AND WHAT THE ONE ACTION IS, as data the exceptions
/// surface renders and files. `kind` and the tab are the once-only key — never
/// the build, so tomorrow's release does not raise a dismissed item again —
/// and `why`/`then` are the sentences (`why` at most [`NEED_WHY_MAX`]
/// characters, `then` at most [`NEED_THEN_MAX`]); neither says "restart".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Need {
    /// The kind, a stable slug (`agent-down`, `no-terminal`, `own-work`…).
    pub kind: &'static str,
    /// THE ONCE-ONLY KEY: the kind and the tab, never the build, an age or
    /// the words (which move with the clock): an item the owner dismissed is
    /// not raised again for tomorrow's release, only for another kind or
    /// another tab.
    pub fingerprint: String,
    /// How much it weighs.
    pub severity: NeedSeverity,
    /// A glass title without its place (the host adds `in tab 1`): at most six
    /// words and 48 characters with the place, no clause seam, no trailing
    /// period — the band's title law (`aterm-messages`' `glass_title_fault`).
    pub title: &'static str,
    /// Why the OWNER is needed — what aterm cannot do or decide.
    pub why: String,
    /// The one thing to do, in the imperative.
    pub then: String,
    /// The exact command the owner runs for it, when it is one (`claude
    /// --resume <conversation>`), for a surface that offers to copy it; the
    /// sentence [`Self::then`] never carries a conversation id.
    pub command: Option<String>,
    /// When the condition began (unix seconds).
    pub since: u64,
    /// The one line the log keeps when the item is dismissed or resolved.
    pub log_line: String,
}

/// The longest [`Need::why`], in characters: the exceptions engine's own cap
/// on a reason (`Need::WHY_CAP`), so a sentence that fits here fits there.
pub const NEED_WHY_MAX: usize = 90;
/// The longest [`Need::then`], in characters (the engine's `Need::THEN_CAP`).
pub const NEED_THEN_MAX: usize = 90;

/// The waits whose truth, past a horizon, is news to the owner — each with
/// the one action only the owner has ([`Row::attention`]). Every other wait is
/// read by its CAUSE ([`Cause`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Special {
    /// Text a person typed into the composer and did not send.
    Draft,
    /// The agent is suspended or in the background of its shell.
    Foreground,
    /// A Codex outside this window attached to the shared daemon.
    OtherClient,
    /// A turn or a background terminal in the shared Codex daemon, which the
    /// daemon's move waits for and no notice can ask about.
    DaemonBusy,
    /// A Codex whose exit cannot be told from a prompt without the shell
    /// integration's marks.
    NoIntegration,
}

/// THE RESUME LINE A PERSON IS TOLD for `session`, whose relaunch the record
/// planned as `line` (`. '<rc>'; rehash; '<exe>' '<flag>'... '--resume'
/// '<id>'`), every flag carried: the ONE line every other place tells for
/// that launch ([`crate::harness::upgrade_drive::resume_command_kept`], each
/// word quoted exactly as [`crate::harness::resume::command`] quotes it), or
/// `None` when the line is not there, is no Claude Code's resume, or holds a
/// word a blind read cannot take exactly ([`launch_of`]). What the owner is
/// told of it within the mark is [`Row::resume_said`]'s. The harness's own
/// line keeps `--dangerously-skip-permissions`; a bare `claude --resume
/// <id>` would bring the orchestrator back in another permission mode. One
/// reader with the upgrade row (`resume_command_kept`), so a quoted model
/// such as `'claude-opus-5-5[1m]'` or a directory with a space keeps its
/// flags and every surface tells the same line.
fn resume_command(line: &str, session: &str) -> Option<String> {
    let launch = launch_of(line)?;
    crate::harness::upgrade_drive::resume_command_kept(None, &launch, session)
}

/// THE LAUNCH a relaunch line planned, as its argv: the agent's own
/// executable and the words after it before `--resume` — past what the line
/// puts in front of the program ([`program_at`]). `None` when there is no
/// `--resume` to carry it over, the program is no Claude Code, or a word
/// holds a backslash: fish writes one (and a quote) otherwise inside its
/// quotes, which [`last_command`] reads in the POSIX form, so such a fish
/// line reads garbled from that word on — nothing is ever guessed into a
/// command the owner types.
fn launch_of(line: &str) -> Option<Vec<String>> {
    let last = last_command(line);
    let (exe, rest) = last.get(program_at(&last)..)?.split_first()?;
    if exe.rsplit('/').next() != Some("claude") {
        return None;
    }
    let mut launch = vec![exe.clone()];
    for word in rest {
        if matches!(word.as_str(), "--resume" | "-r") {
            return Some(launch);
        }
        if word.contains('\\') {
            return None;
        }
        launch.push(word.clone());
    }
    None
}

/// Whether `word` is one of the relaunch line's `NAME=VALUE` exports, and
/// then its name and value (the quotes already taken off by
/// [`last_command`]).
fn export_of(word: &str) -> Option<(&str, &str)> {
    word.split_once('=').filter(|(name, _)| {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    })
}

/// Where the program stands among `last` ([`last_command`]): past the
/// subshell's `( cd -- <dir> &&`, the `NAME=VALUE` exports, `exec` and
/// fish's `env` ([`upgrade::relaunch_line_env`]).
fn program_at(last: &[String]) -> usize {
    let mut at = 0;
    while let Some(word) = last.get(at).map(String::as_str) {
        match word {
            "(" | "&&" | "exec" | "env" => at += 1,
            "cd" if last.get(at + 1).is_some_and(|w| w.as_str() == "--") => at += 3,
            _ if export_of(word).is_some() => at += 1,
            _ => break,
        }
    }
    at
}

/// THE LOGIN THE RECORDED RELAUNCH LINE CARRIES: the value of its
/// `CLAUDE_SECURESTORAGE_CONFIG_DIR` export
/// ([`crate::harness::accounts::LOGIN_ENV`]; `Some("")` for the default login named),
/// or `None` when the line carries none — a line planned for a process that
/// had no such variable, every line from before accounts existed. Read among
/// the words in front of the program only, so a flag's value that looks like
/// an export is not one.
fn resume_login(line: &str) -> Option<String> {
    let last = last_command(line);
    last[..program_at(&last)]
        .iter()
        .filter_map(|word| export_of(word))
        .find(|(name, _)| *name == crate::harness::accounts::LOGIN_ENV)
        .map(|(_, value)| value.to_string())
}

/// The shell words of the LAST command of a relaunch line — or of a resume
/// line ([`crate::harness::resume::shorter`]): single quotes joined, `;`
/// ends a command. A quote INSIDE a word is read as the POSIX
/// shells write it ([`upgrade::quote`]: `'\''`, a backslash outside the
/// quotes), so a login directory with an apostrophe in it does not turn the
/// rest of a zsh or bash line inside out. NOT fish's form (`\'` inside the
/// quotes: read blind of the dialect, it would break a POSIX word that ends
/// in a backslash): such a fish line reads as garbled as it always did, and
/// the command falls back to the bare one.
pub(in crate::harness) fn last_command(line: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if !quoted => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
                any = true;
            }
            '\'' => {
                quoted = !quoted;
                any = true;
            }
            ' ' | '\t' if !quoted => {
                if any {
                    words.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            ';' if !quoted => {
                if any {
                    words.push(std::mem::take(&mut cur));
                    any = false;
                }
                words.push(";".to_string());
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(cur);
    }
    words
        .rsplit(|w| w == ";")
        .find(|command| !command.is_empty())
        .map(<[String]>::to_vec)
        .unwrap_or_default()
}

/// The most bytes of launch flags a manual resume command carries whole
/// ([`Row::resume_said`]): the tab's mark ([`MARK_MAX`]) never cuts the
/// action, and flags past this leave the move and why no room beside it.
const RESUME_FLAGS_MAX: usize = 80;

/// The tab's mark, in bytes: the keyed attention cap ([`Row::badge`]).
const MARK_MAX: usize = 200;

/// [`Special`] for a recorded wait word, if it is one.
fn special(wait: &str) -> Option<Special> {
    match wait {
        "draft" | "announce-refused:draft" => Some(Special::Draft),
        "not-foreground" => Some(Special::Foreground),
        "daemon-first:unseen-client" | "daemon-first:clients-unreadable" => {
            Some(Special::OtherClient)
        }
        "daemon-first:busy-thread" | "daemon-first:background-terminal" => {
            Some(Special::DaemonBusy)
        }
        "no-shell-integration" => Some(Special::NoIntegration),
        _ => None,
    }
}

/// WHAT HOLDS A MOVE, by class ([`Row::cause`]): the one classifier the
/// overdue words ([`Row::overdue_cause`]), the digest's `own-work` line and
/// owner's tell (design record 2026-09-28, §3.2 C6) all read.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Cause {
    /// The agent's own work: a turn, a shell, a workflow, a status not yet
    /// quiet, its READY not given, a round resting after four unanswered
    /// notices — or processes under it while a gate passes.
    OwnWork,
    /// A person: a box, a draft, a login, someone at the tab, a hold.
    Person,
    /// A usage limit, which ends by itself at its reset.
    Limit,
    /// aterm's own notice could not be typed.
    CouldNotType,
    /// Notices wait unread behind the conversation's queue.
    Queued,
    /// Waiting on the Codex daemon shared with other tabs.
    SharedDaemon,
    /// Waiting on its Codex daemon: restarting, its version not yet read.
    Daemon,
    /// A Codex goal aterm paused and could not resume (ruling 381).
    GoalLeftPaused,
    /// A tab, screen, shell integration or process aterm cannot read.
    TabUnread,
    /// A move already under way.
    UnderWay,
    /// A tab or conversation aterm cannot reach to ask in.
    Unreached,
    /// The build it would move to fails to start.
    BuildFails,
    /// Nothing holds it now: the last look found no gate, and its next step
    /// goes at the next point ([`upgrade::NEXT_POINT`]) - the upgrade working
    /// as the rest before it did, never a row for being about to take it.
    NextPoint,
    /// No look has recorded what it waits on.
    NoLook,
    /// A wait this build does not classify — NAMED, so it fails loud.
    Unclassified(String),
}

impl Row {
    /// HOW LOUD THIS UPGRADE IS ([`Attention`]): the one rule the row, the
    /// tab's mark, the menu bar and the notification all follow.
    ///
    /// * INTERRUPT — the upgrade ended the owner's agent and it is not
    ///   running: a move that stopped after the agent was seen gone
    ///   (`agent-down`), or a SIGTERMed agent still alive [`HUNG_EXIT_AFTER_S`]
    ///   on (`hung-exit`; nothing harder is ever sent).
    /// * DIGEST — only the owner can end it, and it can wait: an agent that
    ///   runs where typing cannot reach it (`no-terminal`), a launch a resume
    ///   cannot repeat (`refused-launch`), a stop that repeats
    ///   (`repeating-stop`), an agent suspended for an hour (`suspended`), a
    ///   draft unsent for a day (`typed-draft`), and the agent's own work
    ///   that has held the move for a day and either answered a whole round
    ///   of notices with "not yet" or been too busy to be asked at all
    ///   (`own-work`).
    /// * QUIET — everything else, FOR A REASON THAT ENDS: a wait on the
    ///   agent's turn, a person at the tab, a limit, the login wall, a box, a
    ///   hold, a refused fence, the settle; a first stop; a round that gave
    ///   up and rests. The upgrade asks again on its own, and a timer is not
    ///   a reason to interrupt. NO WAIT IS QUIET WITHOUT AN END: every quiet
    ///   state has a horizon that names who acts ([`Self::backstop`],
    ///   [`Self::transient_bound`]) — a record with no progress in a day, a
    ///   settle that never settles, a relaunched agent that never idles, a
    ///   limit past its longest — and it becomes a FAULT line there.
    /// * FAULT — QUIET, plus one Warn line in the log for the developer: the
    ///   owner has nothing to do about it now, and it should not last
    ///   (`harness-stuck`, `no-progress`, `relaunch-stuck`).
    ///
    /// The owner's word (`--skip`, `--defer`) and a finished move are quiet.
    #[must_use]
    pub fn attention(&self, now: u64) -> Attention {
        // A CODEX GOAL THE MOVE PAUSED AND COULD NOT RESUME (ruling 381: aterm
        // never leaves the goal paused): the upgrade's own damage, one hand step in the tab, and
        // the owner's word on the move does not reach it ([`Self::stall`]).
        if self.goal_left_since != 0 {
            return Attention::Interrupt(self.goal_paused());
        }
        if self.owner_holds(now) {
            return Attention::Quiet;
        }
        // NOTHING HAS CHECKED THE RECORD (design record C10, the owner's
        // ruling of 2026-09-29 "with a grace after wake"): the host's timer
        // did not fire. Aterm's own fault, and the owner has nothing to do
        // about it: ONE line in the log, never a row, a mark or a digest line
        // (`--status` still says `stalled=unwatched`).
        if let Some(since) = self.unwatched(now) {
            return Attention::Fault(self.unwatched_need(since));
        }
        // A USAGE LIMIT THAT HAS ENDED, its notice still the recorded wait
        // ([`Self::limit_passed`], the assessment of 2026-09-29, A1): aterm's
        // own business — the resume that continues a session at its reset
        // has not cleared the notice, and every point the upgrade would
        // offer stays withheld — so ONE line in the log, never a row, a mark
        // or a digest line. Until the reset (and its grace) a limit is a
        // record whatever its age (ruling 307).
        if matches!(
            self.phase,
            Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
        ) && !self.failed_after_exit()
            && self.limit_passed(now)
        {
            return Attention::Fault(self.limit_passed_need(now));
        }
        match &self.phase {
            Phase::Done => Attention::Quiet,
            // THE AGENT IS GONE and the line that resumes it is not typed past
            // the bound: nothing runs in the tab, and no lane visits a tab
            // with no agent (`stuck:exited`, the same fact as a move that
            // stopped after its exit).
            Phase::Exiting { at_s } if self.exited_at != 0 => {
                if now.saturating_sub((*at_s).max(self.exited_at)) > STALE_S {
                    Attention::Interrupt(self.agent_down("exited"))
                } else {
                    Attention::Quiet
                }
            }
            // ASKED TO END and still alive: nothing harder is ever sent.
            Phase::Exiting { at_s } => {
                if now.saturating_sub(*at_s) > HUNG_EXIT_AFTER_S {
                    Attention::Interrupt(self.hung_exit(*at_s))
                } else {
                    Attention::Quiet
                }
            }
            // THE RELAUNCHED AGENT that reaches no idle point: the supervisor
            // answers a box within moments, but a start-up screen that is
            // neither a box nor idle reaches nobody, and the lane holds the
            // conversation for as long as it stands. The log says so.
            Phase::Relaunched { at_s } => {
                let waited = now.saturating_sub(*at_s);
                if waited >= RELAUNCH_INTERRUPT_S {
                    // THE UPGRADE'S OWN DAMAGE, and only the owner can act:
                    // it ended their agent for the new build, the agent came
                    // back and has reached no prompt — a start-up screen
                    // (an update, a what's-new, a consent flow) the
                    // supervisor cannot answer. The same fact as an agent
                    // that did not come back ([`Self::agent_down`]).
                    Attention::Interrupt(self.relaunch_stuck_interrupt(*at_s))
                } else if waited >= RELAUNCH_STUCK_S {
                    Attention::Fault(self.relaunch_stuck(waited, *at_s))
                } else {
                    Attention::Quiet
                }
            }
            Phase::Failed(why) => {
                if self.failed_after_exit() {
                    Attention::Interrupt(self.agent_down(why))
                } else if why == crate::harness::upgrade_drive::BUILD_FAILS {
                    // A round the build's hold stopped (its agent released,
                    // `upgrade::build_hold`): the build is the fact, told as
                    // the round after it, waiting on it, tells it.
                    Attention::Fault(self.build_fails())
                } else if why == upgrade::GAVE_UP {
                    self.or_backstop(self.own_work(now), now)
                } else if refusal(why) {
                    Attention::Digest(self.refused_launch(why))
                } else if self.repeating_stop() {
                    Attention::Digest(self.repeating_stop_need())
                } else {
                    self.backstop(now)
                }
            }
            Phase::Pending | Phase::Announced { .. } => {
                // A BUILD THAT FAILS TO START holds the move
                // (`upgrade_drive`'s `build_start`): nothing the owner does
                // here moves it, and the build, not this tab, is the fact —
                // ONE record per build, however many agents wait on it.
                if self.wait == crate::harness::upgrade_drive::BUILD_FAILS {
                    return Attention::Fault(self.build_fails());
                }
                if let Some(owner) = self.wait.strip_prefix("terminal:") {
                    return Attention::Digest(self.no_terminal(owner));
                }
                if self.repeating_stop() {
                    return Attention::Digest(self.repeating_stop_need());
                }
                if self.phase == Phase::Pending && refusal(&self.last_stop) {
                    return Attention::Digest(self.refused_launch(&self.last_stop));
                }
                // NO LOOK HAS RECORDED WHAT IT WAITS ON, for as long as a fault
                // takes to tell: nothing visits this record, or every visit
                // returns before it records (a read that fails, a host that
                // stopped). Quiet by age, it must still be told once (the design record's "unknown
                // means look again, loudly"): aterm's own fault, a log line.
                if self.phase == Phase::Pending
                    && self.wait.is_empty()
                    && self.minted_at != 0
                    && now.saturating_sub(self.minted_at) >= HARNESS_TELL_S
                {
                    return Attention::Fault(self.no_look(now.saturating_sub(self.minted_at)));
                }
                // A NOTICE ITS OWN FENCE REFUSED at every look since the upgrade's
                // last act, for [`TELL_S`]: counted by the driver
                // (`St::refused`, from the first refusal of the streak, which a
                // change of the refusal's reason does not restart).
                if self.refused_notice(now) && !self.refusal_stands() {
                    return Attention::Fault(
                        self.harness_stuck(now.saturating_sub(self.refused_at)),
                    );
                }
                let waited = now.saturating_sub(self.wait_since);
                // ATERM'S OWN BUSINESS stands out: an unclassified wait, a
                // notice the fence keeps refusing, a settle that never
                // settles, a tab it cannot reach — none is the agent's or a
                // person's (the design record's "unknown means look again,
                // loudly"), and the owner has nothing to do about it: ONE
                // Warn line in the log, for them to read later as the
                // developer.
                if self.wait_since != 0 && special(&self.wait).is_none() {
                    // (A refused notice the driver counts is judged above, from
                    // its first refusal: the wait word's own clock restarts
                    // with each change of the reason.)
                    let counted =
                        self.wait.starts_with("announce-refused:") && self.refused_at != 0;
                    if !counted && self.transient_bound().is_some_and(|bound| waited >= bound) {
                        return Attention::Fault(self.harness_stuck(waited));
                    }
                    if waited >= DIGEST_AFTER_S && self.wait == "daemon-first:update-failed" {
                        return Attention::Fault(self.daemon_update_failing(waited));
                    }
                }
                match special(&self.wait) {
                    Some(Special::Foreground)
                        if self.wait_since != 0 && waited >= SUSPENDED_AFTER_S =>
                    {
                        Attention::Digest(self.suspended())
                    }
                    Some(Special::Draft) if self.wait_since != 0 && waited >= DIGEST_AFTER_S => {
                        Attention::Digest(self.typed_draft(waited))
                    }
                    Some(Special::OtherClient)
                        if self.wait_since != 0 && waited >= DIGEST_AFTER_S =>
                    {
                        Attention::Digest(self.other_client())
                    }
                    Some(Special::DaemonBusy)
                        if self.wait_since != 0 && waited >= DIGEST_AFTER_S =>
                    {
                        Attention::Digest(self.daemon_busy(waited))
                    }
                    Some(Special::NoIntegration) => Attention::Digest(self.no_integration()),
                    _ => self.or_backstop(self.own_work(now), now),
                }
            }
        }
    }

    /// THE INTERRUPT THIS ROW IS, if it is one: the owner is needed NOW and
    /// the action is theirs alone. The one class that takes a band row, the
    /// tab's mark, the menu-bar row and the notification — the hosts ask this,
    /// never the age.
    #[must_use]
    pub fn interrupt(&self, now: u64) -> Option<Need> {
        match self.attention(now) {
            Attention::Interrupt(need) => Some(need),
            _ => None,
        }
    }

    /// `first`, unless it is quiet: then [`Self::backstop`].
    fn or_backstop(&self, first: Attention, now: u64) -> Attention {
        if first == Attention::Quiet {
            self.backstop(now)
        } else {
            first
        }
    }

    /// WHEN A WAIT NO CLASS CLAIMS HAS STOOD TOO LONG: a record that has made
    /// no progress (no notice typed, no round begun) in [`BACKSTOP_S`] — a
    /// usage limit in [`LIMIT_MAX_S`], notices unread behind a full queue in
    /// [`QUEUED_MAX_S`] — is a FAULT line, one, quoting the last waits: quiet
    /// to the glass and never silent, whatever the record waits on.
    fn backstop(&self, now: u64) -> Attention {
        // A row that names no record (one read off Claude's live status, a
        // state an older build wrote with no salt) has no start to count from.
        let Some(since) = self.last_progress() else {
            return Attention::Quiet;
        };
        let stood = now.saturating_sub(since);
        let bound = match self.cause() {
            Cause::Limit => LIMIT_MAX_S,
            Cause::Queued => QUEUED_MAX_S,
            _ => BACKSTOP_S,
        };
        if stood >= bound {
            Attention::Fault(self.no_progress(stood, since))
        } else {
            Attention::Quiet
        }
    }

    /// The last instant this record made progress ([`Row::progress_at`]: its
    /// minting, latest notice, round start or latest stop), or — past a
    /// round's own asks — the record's minting. `None` for a row that names no
    /// record.
    fn last_progress(&self) -> Option<u64> {
        // AN ASK PAST A ROUND'S OWN [`upgrade::MAX_ASKS`] IS NO PROGRESS: the
        // notice at the end of the agent's
        // work is asked again whatever the count, and each one moved the
        // stamp — a session asked six times in a day, and declining every
        // time, read as moving. The row's stamp ([`Row::progress_at`]) moves
        // with any ask; past a round's asks only its minting counts.
        let past = matches!(self.phase, Phase::Announced { asks, .. } if asks > upgrade::MAX_ASKS);
        // A row built without the stamp (one read off Claude's live status
        // names no record and has none) counts from what it does carry.
        let announced = match self.phase {
            Phase::Announced { at_s, .. } => at_s,
            _ => 0,
        };
        let at = if past {
            self.minted_at
        } else {
            self.progress_at
                .max(self.minted_at)
                .max(self.noticed_at)
                .max(announced)
        };
        (at != 0).then_some(at)
    }

    /// How long a wait that is ATERM'S OWN BUSINESS may stand before it is a
    /// fault: a notice its screen keeps refusing, a word it does not
    /// classify, a settle that never settles, an answer awaited past a whole
    /// round, a tab it cannot reach because it has gone. [`HARNESS_TELL_S`].
    /// `None` for a wait that is someone else's — including the agent's
    /// status that lags its work (`status-stale`, which is work inside its own
    /// process) and a conversation held by another process (the owner's own
    /// setup), which the day's backstop covers.
    fn transient_bound(&self) -> Option<u64> {
        let wait = self.wait.strip_prefix("release:").unwrap_or(&self.wait);
        let own = matches!(
            self.cause(),
            Cause::CouldNotType | Cause::TabUnread | Cause::Unclassified(_)
        ) || matches!(
            wait,
            "settling"
                | "changed"
                | "changed-before-signal"
                // The quiet upgrade's own first look, and a READY on the
                // screen not yet in its transcript: each is decided at the
                // next look, bounded as the settle is.
                | "notice-shown"
                | "ready-unrecorded"
                | "awaiting-ready"
                | "not-ready"
                | "tab-not-live"
                | "no-socket"
                | "tab-ownership-changed"
        );
        own.then_some(HARNESS_TELL_S)
    }

    /// Whether the agent's own work is what holds the move NOW: this look's
    /// class, or — for a round that was held by it at a look within
    /// [`OWN_WORK_HOLD_S`] — the round's remembered one. A look that finds a
    /// box, a person, or nothing recorded (an act clears the wait and the
    /// holders) between two looks at the work does not end the work; a limit
    /// and the Codex daemon are their own classes.
    fn works(&self, now: u64) -> bool {
        match self.cause() {
            Cause::OwnWork => true,
            Cause::Limit | Cause::Daemon | Cause::SharedDaemon | Cause::GoalLeftPaused => false,
            _ => self.own_work_at != 0 && now.saturating_sub(self.own_work_at) < OWN_WORK_HOLD_S,
        }
    }

    /// A record never asked: still pending, and no round has stopped, no
    /// notice has been typed for it.
    fn never_asked(&self) -> bool {
        self.phase == Phase::Pending
            && self.notices_asked() == 0
            && self.stop_streak == 0
            && self.streak_why.is_empty()
            && self.last_stop.is_empty()
    }

    /// The unix second after `now` at which [`Self::attention`] next reads
    /// otherwise BY TIME ALONE — nothing else moves it but a step, a word or a
    /// process — for the host's next look ([`next_change`]): a day behind (the
    /// agent's own work, the backstop), an hour suspended, a draft a day old, a
    /// shutdown or a vanished agent past its bound, the remembered work
    /// running out.
    ///
    /// READ OFF [`Self::attention`] ITSELF, never a second copy of its
    /// horizons (a copy drifts: the class turns at one instant and the host
    /// is told at another): every span this row's classifier compares an age against,
    /// added to every instant it compares it from, is a candidate, and the
    /// first candidate at which the class differs is the answer. The sweep
    /// `next_attention_at_names_every_change_by_time` pins the candidate set
    /// complete.
    #[must_use]
    pub fn next_attention_at(&self, now: u64) -> Option<u64> {
        let here = self.attention(now).level();
        let at_s = match self.phase {
            Phase::Exiting { at_s }
            | Phase::Relaunched { at_s }
            | Phase::Announced { at_s, .. } => at_s,
            _ => 0,
        };
        let bases = [
            self.behind_since,
            self.minted_at,
            self.noticed_at,
            self.wait_since,
            self.own_work_at,
            self.exited_at,
            self.refused_at,
            self.limit_until,
            at_s,
        ];
        let spans = [
            SUSPENDED_AFTER_S,
            HARNESS_TELL_S,
            OWN_WORK_HOLD_S,
            DIGEST_AFTER_S,
            QUEUED_MAX_S,
            LIMIT_MAX_S,
            LIMIT_GRACE_S,
            LONGEST_LIMIT_S + LIMIT_GRACE_S,
            STALE_S + 1,
            HUNG_EXIT_AFTER_S + 1,
            RELAUNCH_STUCK_S,
            RELAUNCH_INTERRUPT_S,
        ];
        let mut candidates: Vec<u64> = bases
            .iter()
            .filter(|base| **base != 0)
            .flat_map(|base| spans.iter().map(move |span| base.saturating_add(*span)))
            .filter(|at| *at > now)
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        candidates
            .into_iter()
            .find(|at| self.attention(*at).level() != here)
    }

    /// The whole rounds of notices in a row that ended in a give-up.
    fn unanswered_rounds(&self) -> u32 {
        if self.streak_why == upgrade::GAVE_UP {
            self.stop_streak
        } else {
            0
        }
    }

    /// The agent's own work holds the move: a digest line once the session has
    /// been behind for [`DIGEST_AFTER_S`] and either has answered a whole round
    /// of notices with "not yet" or was never asked, its work having left no
    /// point to ask at. Sooner, or with the work gone, it is quiet. THE WORK IS
    /// THE ROUND'S ([`Self::works`]): an act, a box or a draft between two
    /// looks at it does not end it.
    fn own_work(&self, now: u64) -> Attention {
        let behind = now.saturating_sub(self.behind_since);
        let (rounds, never_asked) = (self.unanswered_rounds(), self.never_asked());
        let notices = self.notices_asked();
        // A WHOLE ROUND UNANSWERED is judged by the ASKS, not by the give-up
        // that ends it: a session asked again at the end of every run of its
        // work never reaches the give-up, and one whose round a new build
        // started over never reaches it in one round.
        let unanswered = notices >= upgrade::MAX_ASKS;
        if behind < DIGEST_AFTER_S || (!unanswered && !never_asked) || !self.works(now) {
            return Attention::Quiet;
        }
        let held = self.held_words(now);
        let holders = self.held_by.len();
        // WHY THE OWNER, AND NOT THE MACHINE (the exceptions design's words):
        // aterm never ends running work, so the one who can end it is the
        // person (or the agent in the tab). How many notices went unanswered,
        // and for how long, are the details ([`Self::more_lines`]), not the
        // reason.
        // WHAT THE WORK IS, as far as the record knows it: programs under the
        // agent, a `/goal` in force (the footer's `◎ /goal active`: Claude
        // Code re-prompts itself at every turn end, so its status stays busy
        // and the restart's gate never opens until the goal is met or
        // cleared), or a break of its own background work (a workflow it
        // waits on).
        let breaking = matches!(
            self.wait.strip_prefix("release:").unwrap_or(&self.wait),
            "background" | "background-terminal"
        );
        // THE REASON NAMES NO COUNT: what runs under the agent comes and goes
        // at every look, and a count here changed the entry's words with it
        // (2026-10-08: one restatement after another, in a day). How many and
        // which are the details ([`Self::more_lines`]).
        let why = match (holders > 0, self.goal) {
            (true, true) => {
                "aterm never ends running work: its /goal is active and work it started still runs"
            }
            (true, false) => "aterm never ends running work, and work it started is still running",
            (false, true) => "aterm never ends running work, and its /goal is still active",
            // No word the door refuses (`aterm_messages::need::Reason::fault`:
            // a reason that says something is pending, "waiting" among them):
            // refused, the entry was never shown.
            (false, false) if breaking => {
                "aterm never ends running work, and its background work is still running"
            }
            (false, false) => "aterm never ends running work, and it is busy with work of its own",
        };
        // THE ACTION IS THE ONE THAT ENDS THE WAIT: the end of the agent's
        // own work is the upgrade's point (`upgrade::Facts::work_ended`: the
        // round asks again at once, and a round that gave up is re-armed), and
        // a pending round asks at its next point, so "stop that work" is
        // followed by the ask without a second step. ONE action, as the door
        // reads it (`aterm_messages::need::Step::fault`: no " or "): under a
        // `/goal` ending the goal is that action, the work beside it named
        // as its object.
        let then = if self.goal && holders > 0 {
            "End its /goal and that work in the tab when it suits you"
        } else if self.goal {
            "End its /goal in the tab when it suits you"
        } else if holders > 0 || breaking {
            "Stop that work in the tab when it suits you"
        } else {
            "Ask it to wrap up in the tab when it suits you"
        };
        Attention::Digest(Need {
            kind: "own-work",
            fingerprint: self.fp("own-work"),
            title: "Upgrade waits on work",
            severity: NeedSeverity::Info,
            why: cut(why, NEED_WHY_MAX),
            then: cut(then, NEED_THEN_MAX),
            command: None,
            since: self.behind_since,
            log_line: self.log_line(
                "own-work",
                &format!(
                    "{notices} notices unanswered in {} ({rounds} rounds gave up), held by {}",
                    upgrade::span(behind),
                    held.unwrap_or_else(|| "its own work".to_string())
                ),
            ),
        })
    }

    /// The notices this row's session has been asked and left unanswered so
    /// far, in every round (the record's own count, `notices`) — and never
    /// fewer than its round proves by itself ([`round_notices`]: a row built
    /// by hand carries no count).
    fn notices_asked(&self) -> u32 {
        self.notices.max(round_notices(
            &self.phase,
            &self.streak_why,
            self.stop_streak,
        ))
    }

    /// THE DETAILS A SURFACE SHOWS BEHIND A `Need` — Settings, never the digest
    /// line itself (which must not restate every minute as pids come and go):
    /// at most three lines of at most 200 characters, the volatile facts and the
    /// way to read the rest. For the agent's own work: how many notices went
    /// unanswered in how long (`failed:unanswered` is the round's word for
    /// it), what runs under it (counted, no command), and that the upgrade
    /// goes on by itself once that stops.
    #[must_use]
    pub fn more_lines(&self, need: &Need, now: u64) -> Vec<String> {
        let mut lines = Vec::new();
        if need.kind == "own-work" {
            let behind = upgrade::span(now.saturating_sub(self.behind_since));
            let notices = self.notices_asked();
            lines.push(if notices == 0 {
                format!("Not asked in {behind}: the work left no point to ask at")
            } else {
                format!(
                    "Asked {notices} time{} in {behind}, unanswered",
                    if notices == 1 { "" } else { "s" }
                )
            });
            if let Some(held) = self.held_words(now) {
                lines.push(format!("Held by {held}"));
            }
            lines.push("The upgrade goes on by itself once that work stops".to_string());
        }
        lines.push(format!(
            "aterm harness upgrade {} --status shows the wait log",
            word(&self.tab)
        ));
        lines.truncate(3);
        lines.into_iter().map(|line| cut(&line, 200)).collect()
    }

    /// The upgrade ended the agent and nothing runs in the tab. The action
    /// follows from WHY it could not be brought back: a tab whose shell is
    /// gone has no prompt to type at, and a Codex conversation whose rollout
    /// is gone has nothing to resume. The command is the agent's own resume
    /// verb WITH the launch's flags ([`Self::resume_said`]) — the harness's
    /// own relaunch line keeps `--dangerously-skip-permissions`, and a bare
    /// `claude --resume` would bring the orchestrator back in another
    /// permission mode — and it is carried in [`Need::command`], never cut
    /// into the sentence.
    fn agent_down(&self, why: &str) -> Need {
        let (then, command) = match (self.agent, why) {
            (upgrade::Agent::Codex, "no-rollout") => {
                (cut("Start Codex again in that tab", NEED_THEN_MAX), None)
            }
            // Started afresh: there is no conversation of it to resume.
            (upgrade::Agent::Claude, why) if self.fresh => {
                let then = if why == "shell-gone" {
                    "Open a new tab and start Claude Code there with this command"
                } else {
                    "Start Claude Code again in that tab with this command"
                };
                (
                    cut(&self.resume_then(then), NEED_THEN_MAX),
                    Some("claude".to_string()),
                )
            }
            (upgrade::Agent::Claude | upgrade::Agent::Codex, "shell-gone") => {
                let (then, command) =
                    self.resume_said("Open a new tab and resume it there with this command");
                (then, Some(command))
            }
            (upgrade::Agent::Claude | upgrade::Agent::Codex, _) => {
                let (then, command) = self.resume_said("Resume it in that tab with this command");
                (then, Some(command))
            }
        };
        Need {
            kind: "agent-down",
            fingerprint: self.fp("agent-down"),
            title: "Agent is not running",
            severity: NeedSeverity::Error,
            why: cut(
                "The upgrade ended it and aterm could not start it again",
                NEED_WHY_MAX,
            ),
            then,
            command,
            since: self.exited_at,
            log_line: self.log_line("agent-down", &format!("stopped after its exit: {why}")),
        }
    }

    /// A relaunched agent that has reached no idle point for
    /// [`RELAUNCH_INTERRUPT_S`]: the owner is needed, and the one action is to
    /// look at the tab.
    fn relaunch_stuck_interrupt(&self, since: u64) -> Need {
        Need {
            kind: "relaunch-stuck",
            fingerprint: self.fp("relaunch-stuck"),
            title: "Agent is not ready",
            severity: NeedSeverity::Warn,
            why: cut(
                "It came back after the upgrade and has not reached a prompt",
                NEED_WHY_MAX,
            ),
            then: cut("Look at that tab and answer what it shows", NEED_THEN_MAX),
            command: None,
            since,
            log_line: self.log_line(
                "relaunch-stuck",
                "relaunched, never idle: the owner is needed",
            ),
        }
    }

    /// A Codex goal the move paused and could not resume: the one hand step
    /// is `/goal resume` in the tab (a thread that fell into a sandbox needs
    /// its relaunch first).
    fn goal_paused(&self) -> Need {
        let sandboxed = self.wait == super::super::upgrade_codex::SANDBOXED;
        Need {
            kind: "goal-paused",
            fingerprint: self.fp("goal-paused"),
            title: if sandboxed {
                "Goal left paused"
            } else {
                "Goal still paused"
            },
            severity: NeedSeverity::Warn,
            why: cut(
                if sandboxed {
                    "aterm paused its Codex goal for the upgrade and its thread fell into a sandbox"
                } else {
                    "aterm paused its Codex goal for the upgrade and could not resume it"
                },
                NEED_WHY_MAX,
            ),
            then: cut(
                if sandboxed {
                    "Quit Codex in that tab, resume it with its launch flags, then type /goal resume"
                } else {
                    "Type /goal resume in that tab"
                },
                NEED_THEN_MAX,
            ),
            command: None,
            since: self.goal_left_since,
            log_line: self.log_line("goal-paused", "a paused Codex goal was not resumed"),
        }
    }

    /// A SIGTERMed agent that has not exited.
    fn hung_exit(&self, since: u64) -> Need {
        Need {
            kind: "hung-exit",
            fingerprint: self.fp("hung-exit"),
            title: "Agent did not exit",
            severity: NeedSeverity::Warn,
            // No minute count: a running age in the words would re-send the
            // tab's mark and restate the row every minute (`since` carries it).
            why: cut(
                "It was asked to end for the upgrade and has not exited",
                NEED_WHY_MAX,
            ),
            then: cut("Press Ctrl-C in that tab", NEED_THEN_MAX),
            command: None,
            since,
            log_line: self.log_line("hung-exit", "the agent outlived its SIGTERM"),
        }
    }

    /// The agent runs where the upgrade's typing cannot reach it.
    fn no_terminal(&self, owner: &str) -> Need {
        let who = self.agent.product();
        let pane = runs_under(owner);
        let (then, command) = self.resume_said("Quit it there and resume it in an aterm tab");
        Need {
            kind: "no-terminal",
            fingerprint: self.fp("no-terminal"),
            title: "Agent out of reach",
            severity: NeedSeverity::Info,
            why: cut(
                &format!("{who} runs where the upgrade cannot type to it ({pane})"),
                NEED_WHY_MAX,
            ),
            then,
            command: Some(command),
            since: self.wait_since,
            log_line: self.log_line("no-terminal", &format!("terminal owner: {owner}")),
        }
    }

    /// A BUILD THAT FAILED TO START, NOT PROVED TO START SINCE
    /// (`upgrade_drive`'s `build_start`): a fault, a record, keyed by the
    /// BUILD — its version and install, never the tab — so the owner is told
    /// once per build however many agents it holds back, and never by a stop
    /// per agent moved onto it (handoff §4, round eighteen).
    fn build_fails(&self) -> Need {
        let who = self.agent.product();
        Need {
            kind: "build-fails",
            fingerprint: format!("build-fails/{}/{}", word(&self.source), word(&self.to)),
            title: "New build fails to start",
            severity: NeedSeverity::Warn,
            why: cut(
                &format!(
                    "{who} {} failed to start, so aterm moves no agent onto it",
                    word(&self.to)
                ),
                NEED_WHY_MAX,
            ),
            then: cut(
                "Nothing to do: agents stay on the build they run until one that starts",
                NEED_THEN_MAX,
            ),
            command: None,
            since: self.wait_since,
            log_line: self.log_line(
                "build-fails",
                "the build failed to start and is not proved to start since",
            ),
        }
    }

    /// A launch a resume cannot repeat.
    fn refused_launch(&self, why: &str) -> Need {
        let who = self.agent.product();
        let how = if why == "not-a-shell-job" {
            "was started through a wrapper, not directly by its shell"
        } else if why.starts_with("line:") {
            "has a launch line a resume cannot repeat"
        } else {
            "was started with a flag a resume cannot carry"
        };
        let (then, command) = self.resume_said("Start it directly at a shell prompt in that tab");
        Need {
            kind: "refused-launch",
            fingerprint: self.fp("refused-launch"),
            title: "Agent cannot be resumed",
            severity: NeedSeverity::Info,
            why: cut(&format!("{who} {how}"), NEED_WHY_MAX),
            then,
            command: Some(command),
            since: self.wait_since.max(self.behind_since),
            log_line: self.log_line("refused-launch", &format!("refused: {}", word(why))),
        }
    }

    /// A stop that repeats.
    fn repeating_stop_need(&self) -> Need {
        let who = self.agent.product();
        let (then, command) = self.resume_said("Quit it and resume it in that tab");
        Need {
            kind: "repeating-stop",
            fingerprint: self.fp("repeating-stop"),
            title: "Upgrade keeps stopping",
            severity: NeedSeverity::Warn,
            why: cut(
                &format!(
                    "Upgrading {who} has stopped the same way {} times in a row",
                    self.stop_streak
                ),
                NEED_WHY_MAX,
            ),
            then,
            command: Some(command),
            since: self.behind_since,
            log_line: self.log_line(
                "repeating-stop",
                &format!(
                    "{} x{}: {}",
                    word(&self.streak_why),
                    self.stop_streak,
                    stop_words(self.agent, &self.streak_why)
                ),
            ),
        }
    }

    /// The agent is suspended or in the background of its shell.
    fn suspended(&self) -> Need {
        let who = self.agent.product();
        Need {
            kind: "suspended",
            fingerprint: self.fp("suspended"),
            title: "Agent is suspended",
            severity: NeedSeverity::Info,
            why: cut(
                &format!("{who} is suspended or in the background, so it cannot be upgraded"),
                NEED_WHY_MAX,
            ),
            then: cut("Run fg in that tab", NEED_THEN_MAX),
            command: Some("fg".to_string()),
            since: self.wait_since,
            log_line: self.log_line("suspended", "not its shell's foreground job"),
        }
    }

    /// A draft nobody sent.
    fn typed_draft(&self, waited: u64) -> Need {
        Need {
            kind: "typed-draft",
            fingerprint: self.fp("typed-draft"),
            title: "Draft waits unsent",
            severity: NeedSeverity::Info,
            why: cut(
                &format!(
                    "A draft has waited in this tab's prompt for {}",
                    upgrade::span(waited)
                ),
                NEED_WHY_MAX,
            ),
            then: cut("Send the draft in that tab", NEED_THEN_MAX),
            command: None,
            since: self.wait_since,
            log_line: self.log_line("typed-draft", "text in the composer"),
        }
    }

    /// A usage limit whose named reset has passed, its notice still the
    /// recorded wait (a fault, a record: [`Attention::Fault`]): what the
    /// record read, how long ago the limit ended, and where the wait log is.
    fn limit_passed_need(&self, now: u64) -> Need {
        let ended = self.limit_ends_at().unwrap_or(0);
        let ago = behind_words(ended, now).map_or_else(String::new, |span| format!(" {span} ago"));
        let what = if self.limit_until != 0 {
            format!("Its usage limit's reset passed{ago}, and the last look still read the limit")
        } else {
            "Its usage limit has outlasted any usage limit".to_string()
        };
        Need {
            kind: "limit-passed",
            fingerprint: self.fp("limit-passed"),
            title: "Usage limit notice outlived",
            severity: NeedSeverity::Warn,
            why: cut(&what, NEED_WHY_MAX),
            then: cut("Report it with the wait log from --status", NEED_THEN_MAX),
            command: Some(format!(
                "aterm harness upgrade {} --status --json",
                word(&self.tab)
            )),
            since: ended,
            log_line: self.log_line(
                "limit-passed",
                &format!("limit_until {} wait {}", self.limit_until, word(&self.wait)),
            ),
        }
    }

    /// A wait that is aterm's own fault (`Cause::CouldNotType`, an
    /// unclassified word): the line the log keeps once ([`Attention::Fault`]).
    fn harness_stuck(&self, waited: u64) -> Need {
        let what = match self.cause() {
            Cause::Unclassified(word) => {
                format!("an unclassified wait ({})", word_short(&word))
            }
            Cause::CouldNotType if self.refused > 0 => format!(
                "typing its notice, which the screen refused {} time{}",
                self.refused,
                if self.refused == 1 { "" } else { "s" }
            ),
            Cause::CouldNotType => "typing its notice, which the screen keeps refusing".to_string(),
            _ => format!(
                "the wait `{}`, which should not last",
                word_short(&self.wait)
            ),
        };
        Need {
            kind: "harness-stuck",
            fingerprint: self.fp("harness-stuck"),
            title: "Upgrade stuck on aterm",
            severity: NeedSeverity::Warn,
            why: cut(
                &format!("Waited {} on {what}: aterm's to fix", upgrade::span(waited)),
                NEED_WHY_MAX,
            ),
            then: cut("Report it with the wait log from --status", NEED_THEN_MAX),
            command: Some(format!(
                "aterm harness upgrade {} --status --json",
                word(&self.tab)
            )),
            since: self.wait_since,
            log_line: self.log_line("harness-stuck", &format!("wait {}", word(&self.wait))),
        }
    }

    /// A turn or a background terminal in the shared Codex daemon holds its
    /// move, a day on: no notice can ask a thread anything.
    fn daemon_busy(&self, waited: u64) -> Need {
        let terminal = self.wait.ends_with("background-terminal");
        Need {
            kind: "daemon-busy",
            fingerprint: self.fp("daemon-busy"),
            title: "Codex thread holds daemon",
            severity: NeedSeverity::Info,
            why: cut(
                &if terminal {
                    format!(
                        "A background terminal has run in its Codex daemon for {}, which keeps it",
                        upgrade::span(waited)
                    )
                } else {
                    format!(
                        "A thread in its Codex daemon has run for {}, which keeps it where it is",
                        upgrade::span(waited)
                    )
                },
                NEED_WHY_MAX,
            ),
            then: cut(
                if terminal {
                    "Close it with /stop in the Codex that started it"
                } else {
                    "Stop that thread (codex agents lists them)"
                },
                NEED_THEN_MAX,
            ),
            command: (!terminal).then(|| "codex agents".to_string()),
            since: self.wait_since,
            log_line: self.log_line("daemon-busy", &format!("wait {}", word(&self.wait))),
        }
    }

    /// A record no look has read for two watch gaps, the Mac awake past its
    /// wake grace (a fault, a record: [`Attention::Fault`]; [`Self::unwatched`]):
    /// the host's timer did not fire, which is aterm's to fix. The bound
    /// ([`UNWATCHED_AFTER_S`]) is the line's number, never the running age, so
    /// the line reads the same at every look while it stands.
    fn unwatched_need(&self, since: u64) -> Need {
        Need {
            kind: "unwatched",
            fingerprint: self.fp("unwatched"),
            title: "Upgrade not checked",
            severity: NeedSeverity::Warn,
            why: cut(
                &format!(
                    "No look has read its record for more than {}: aterm's own watch did not \
                     run",
                    upgrade::span(UNWATCHED_AFTER_S)
                ),
                NEED_WHY_MAX,
            ),
            then: cut("Report it with the wait log from --status", NEED_THEN_MAX),
            command: Some(format!(
                "aterm harness upgrade {} --status --json",
                word(&self.tab)
            )),
            since,
            log_line: self.log_line("unwatched", "no look read the record"),
        }
    }

    /// A Pending record no look has recorded a wait for (a fault, a record:
    /// [`Attention::Fault`]).
    fn no_look(&self, waited: u64) -> Need {
        Need {
            kind: "harness-stuck",
            fingerprint: self.fp("harness-stuck"),
            title: "Upgrade stuck on aterm",
            severity: NeedSeverity::Warn,
            why: cut(
                &format!(
                    "No look recorded what it waits on for {}: aterm's to fix",
                    upgrade::span(waited)
                ),
                NEED_WHY_MAX,
            ),
            then: cut("Report it with the wait log from --status", NEED_THEN_MAX),
            command: Some(format!(
                "aterm harness upgrade {} --status --json",
                word(&self.tab)
            )),
            since: self.minted_at,
            log_line: self.log_line("harness-stuck", "no look recorded a wait"),
        }
    }

    /// A record that has made no progress in a day, whatever it waits on
    /// (a fault, a record: [`Attention::Fault`]): the last waits, quoted, so
    /// the log line answers "why has this tab not been upgraded" without a
    /// transcript, a journal and a process table.
    fn no_progress(&self, stood: u64, since: u64) -> Need {
        let last: Vec<String> = self
            .waits
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(|w| word_short(&w.word))
            .collect();
        let what = if last.is_empty() {
            format!(
                "No progress in {} and no wait recorded",
                upgrade::span(stood)
            )
        } else {
            format!(
                "No progress in {}; its last waits: {}",
                upgrade::span(stood),
                last.join(", ")
            )
        };
        Need {
            kind: "no-progress",
            fingerprint: self.fp("no-progress"),
            title: "Upgrade has not moved",
            severity: NeedSeverity::Warn,
            why: cut(&what, NEED_WHY_MAX),
            then: cut("Report it with the wait log from --status", NEED_THEN_MAX),
            command: Some(format!(
                "aterm harness upgrade {} --status --json",
                word(&self.tab)
            )),
            since,
            log_line: self.log_line(
                "no-progress",
                &format!(
                    "wait {} cause {}",
                    word(&self.wait),
                    self.overdue_cause(since.saturating_add(stood))
                ),
            ),
        }
    }

    /// A relaunched agent that has reached no idle point (a fault, a
    /// record: [`Attention::Fault`]).
    fn relaunch_stuck(&self, waited: u64, since: u64) -> Need {
        Need {
            kind: "relaunch-stuck",
            fingerprint: self.fp("relaunch-stuck"),
            title: "Relaunch has not settled",
            severity: NeedSeverity::Warn,
            why: cut(
                &format!(
                    "The relaunched agent has reached no idle point in {}",
                    upgrade::span(waited)
                ),
                NEED_WHY_MAX,
            ),
            then: cut("Report it with the wait log from --status", NEED_THEN_MAX),
            command: Some(format!(
                "aterm harness upgrade {} --status --json",
                word(&self.tab)
            )),
            since,
            log_line: self.log_line("relaunch-stuck", "relaunched, never idle"),
        }
    }

    /// Moving the shared Codex daemon has failed for a day (a fault, a
    /// record: [`Attention::Fault`]).
    fn daemon_update_failing(&self, waited: u64) -> Need {
        Need {
            kind: "daemon-update-failing",
            fingerprint: self.fp("daemon-update-failing"),
            title: "Codex daemon stuck",
            severity: NeedSeverity::Warn,
            why: cut(
                &format!(
                    "Moving its Codex daemon has failed for {}: that is aterm's to fix",
                    upgrade::span(waited)
                ),
                NEED_WHY_MAX,
            ),
            then: cut("Report it with the wait log from --status", NEED_THEN_MAX),
            command: Some(format!(
                "aterm harness upgrade {} --status --json",
                word(&self.tab)
            )),
            since: self.wait_since,
            log_line: self.log_line(
                "daemon-update-failing",
                &format!("wait {}", word(&self.wait)),
            ),
        }
    }

    /// A Codex outside this window holds the shared daemon where it is.
    fn other_client(&self) -> Need {
        Need {
            kind: "other-client",
            fingerprint: self.fp("other-client"),
            title: "Another Codex holds it",
            severity: NeedSeverity::Info,
            why: cut(
                "A Codex outside this window is attached to the shared Codex daemon, which keeps it",
                NEED_WHY_MAX,
            ),
            then: cut("Quit that other Codex", NEED_THEN_MAX),
            command: None,
            since: self.wait_since,
            log_line: self.log_line("other-client", "a client this window does not see"),
        }
    }

    /// A Codex tab whose shell has no integration marks.
    fn no_integration(&self) -> Need {
        Need {
            kind: "no-integration",
            fingerprint: self.fp("no-integration"),
            title: "No shell integration",
            severity: NeedSeverity::Info,
            why: cut(
                "Codex in this tab cannot be exited safely: its shell sends no prompt marks",
                NEED_WHY_MAX,
            ),
            then: cut("Resume Codex in a new tab", NEED_THEN_MAX),
            command: Some("codex resume".to_string()),
            since: self.wait_since,
            log_line: self.log_line("no-integration", "no shell integration marks"),
        }
    }

    /// `then` — the sentence of a need whose command is the resume line
    /// ([`Self::resume_said`]) — saying WHICH LOGIN that command runs under
    /// where the record's own relaunch line named one
    /// ([`resume_login`]; [`crate::harness::resume::SHELL_LOGIN_CLAUSE`] says why the
    /// command itself carries none): the harness relaunched this session
    /// under a login of its choosing, and the command typed by hand comes
    /// back under the shell's. Every sentence it is used on still fits
    /// [`NEED_THEN_MAX`] with the clause. A record with no login on its line
    /// — every record of a machine with no roster — gets `then` as it is.
    fn resume_then(&self, then: &str) -> String {
        let named = match self.agent {
            upgrade::Agent::Claude => resume_login(&self.line).is_some(),
            upgrade::Agent::Codex => false,
        };
        format!(
            "{then}{}",
            if named {
                crate::harness::resume::SHELL_LOGIN_CLAUSE
            } else {
                ""
            }
        )
    }

    /// THE SENTENCE AND THE RESUME COMMAND a need hands the owner, `then`
    /// naming the action: the command as the owner types it, carrying the
    /// launch's flags ([`resume_command`]) and NO environment (see
    /// [`Self::resume_then`]), the two chosen TOGETHER against the tab's mark
    /// ([`MARK_MAX`]): ` - <then>: <command>` whole beside the ellipsis the
    /// head gives way to ([`Self::badge`]), and the band row, the menu-bar
    /// row and the notification say the same pair. What gives way, in order:
    /// the move and why (the head), the flags past [`RESUME_FLAGS_MAX`] and
    /// every flag that sets no permission mode
    /// ([`crate::harness::resume::mode_of`]), the login clause, and last the
    /// permission mode itself, so the conversation id is never cut and
    /// `--dangerously-skip-permissions` is the last flag to go.
    fn resume_said(&self, then: &str) -> (String, String) {
        let said = cut(&self.resume_then(then), NEED_THEN_MAX);
        match self.agent {
            upgrade::Agent::Codex => return (said, "codex resume".to_string()),
            upgrade::Agent::Claude => {}
        }
        if self.session.is_empty() {
            return (said, "claude --resume".to_string());
        }
        let Some(bare) = crate::harness::resume::bare(&self.session) else {
            return (said, format!("claude --resume {}", word(&self.session)));
        };
        let told = resume_command(&self.line, &self.session);
        let mode = told.as_deref().and_then(crate::harness::resume::mode_of);
        let full = told.filter(|told| told.len().saturating_sub(bare.len()) <= RESUME_FLAGS_MAX);
        let plain = cut(then, NEED_THEN_MAX);
        let fits = |then: &str, command: &str| {
            format!(" - {then}: {command}").len() + '…'.len_utf8() <= MARK_MAX
        };
        let chosen = [
            (&said, full.as_ref()),
            (&said, mode.as_ref()),
            (&plain, mode.as_ref()),
            (&said, Some(&bare)),
        ]
        .into_iter()
        .find_map(|(then, command)| {
            command
                .filter(|command| fits(then, command))
                .map(|command| (then.clone(), command.clone()))
        });
        chosen.unwrap_or((plain, bare))
    }

    /// The once-only key of a need of `kind` in this row's tab ([`Need::fingerprint`]).
    fn fp(&self, kind: &str) -> String {
        format!("{kind}/{}", word(&self.tab))
    }

    /// The one line the log keeps for a need of `kind` (`detail`: the fact).
    fn log_line(&self, kind: &str, detail: &str) -> String {
        format!(
            "upgrade need={kind} tab={} session={} move={}->{} phase={} behind_since={} {detail}",
            word(&self.tab),
            word(&self.session),
            word(&self.from),
            word(&self.to),
            word(&self.phase.word()),
            self.behind_since,
        )
    }
}

/// A wait word cut to what a sentence can hold.
fn word_short(w: &str) -> String {
    cut(&word(w), 32)
}

impl Need {
    /// THE UPGRADE CANNOT RUN AT ALL (its state folder cannot hold the lock:
    /// `busy:state-unwritable`, `busy:lock-unopenable`): no record exists to
    /// show a row from, so the HOST posts this — once, the way it warns in
    /// `aterm.log` once. Only the owner can mend a folder's permissions.
    #[must_use]
    pub fn upgrade_cannot_run(step: &str) -> Need {
        Need {
            kind: "upgrade-cannot-run",
            fingerprint: "upgrade-cannot-run/-".to_string(),
            title: "Agent upgrades cannot run",
            severity: NeedSeverity::Warn,
            why: cut(
                "aterm cannot write the folder agent upgrades keep their state in",
                NEED_WHY_MAX,
            ),
            then: cut("Fix that folder's permissions", NEED_THEN_MAX),
            command: None,
            since: 0,
            log_line: format!("upgrade need=upgrade-cannot-run step={}", word_short(step)),
        }
    }
}

/// `text` cut to at most `max` characters, the cut marked with `…`.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
/// The class [`Row::overdue_cause`] names a move its agent's own work holds
/// by — what [`Row::asks_on_its_own`] keeps a record.
const OWN_WORK: &str = "held by its own work";

/// The mark's cause for a round whose last look found nothing holding its
/// next step ([`upgrade::NEXT_POINT`]). The row's words (within the band's 64
/// characters) are its second half, [`NEXT_POINT_WORDS`].
const NEXT_POINT_CAUSE: &str = "nothing holds it now; its next step goes at its next pause";

/// The row's words for the same round ([`claude_wait_words`]): the half of
/// [`NEXT_POINT_CAUSE`] that fits beside the age, for a grouped band too.
const NEXT_POINT_WORDS: &str = "its next step goes at its next pause";

/// The mark's cause for a usage limit whose named reset has passed with its
/// notice still the recorded wait ([`Row::limit_passed`]): aterm's own class.
/// No clock time, so the mark stays stable ([`Row::badge`]); it says what the
/// record read, not what the screen shows now.
/// A spend or credit wall naming no reset ([`Row::spend_held`]): the owner's
/// hold, and what lifts it ([`Row::overdue_cause`]).
const SPEND_HELD: &str = "held by its account's credits or spend cap, which only you can lift";

/// The same wall in a stall's own words ([`Row::stall_words`]; within 64
/// characters).
pub const SPEND_WORDS: &str = "it is out of credits or at its spend cap";

const LIMIT_PASSED: &str =
    "its usage limit's reset has passed and the last look still read the limit";

/// The wait of a move whose agent is not in front in its tab ([`Row::aside`]):
/// recorded by a visit where the agent's shell holds it suspended or in the
/// background (`upgrade_drive::foreground_shell`), and read by the owner's
/// view while no look can come.
pub const NOT_FOREGROUND: &str = "not-foreground";

/// The wait of a move whose tab another supervisor's claim holds
/// ([`Row::aside`], [`View::set_claimed`]): the host parks its worker, and no
/// step or look comes until the claim is released.
pub const CLAIMED: &str = "claimed";

/// The wait of a move whose tab's Autopilot is Off ([`Row::aside`],
/// [`View::set_off`]; the review of a295792e4): the owner took the harness
/// off the session, so the host stops its worker and fires no watch there,
/// and no step or look comes until its Autopilot is Cruise or Land again.
pub const AUTOPILOT_OFF: &str = "autopilot-off";

/// The wait of a move whose tab's agent its own switch holds off
/// (`[harness.<agent>] enabled = false`; [`Row::aside`],
/// [`View::set_agent_off`]): the host admits no worker of that agent and
/// fires no watch there, and no step or look comes until it is switched on
/// again.
pub const AGENT_OFF: &str = "agent-off";

/// THE TABS NO HOST'S WORKER RUNS IN BY A HAND THE HOST DOES NOT HOLD
/// ([`Row::set_aside`]), each read as held by that hand, never as a record
/// its host stopped checking: another supervisor's claim ([`CLAIMED`]), the
/// agent's own switch off ([`AGENT_OFF`]) and the owner's Autopilot off
/// ([`AUTOPILOT_OFF`]). The window's host says them before each look
/// ([`View::set_claimed`], [`View::set_agent_off`], [`View::set_off`]);
/// `--status` reads all three off the instance's roster, the agent's switch
/// by its `program=` and the policy's off list ([`status_rows`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Aside {
    /// Tabs another supervisor's claim holds.
    pub claimed: BTreeSet<String>,
    /// Tabs whose Autopilot is Off.
    pub off: BTreeSet<String>,
    /// Tabs whose agent its own switch holds off.
    pub agent_off: BTreeSet<String>,
}

/// What the owner can do about a stalled upgrade ([`Row::remedy`]) — the
/// words differ because what moves it differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    /// `overdue` waiting on what `--now` still moves ([`now_moves_past`]): a
    /// Codex act that left its text typed (retried at once) or a notice behind
    /// a full queue (`queued`, typed once more). `--skip` keeps it.
    Now,
    /// `overdue` waiting on what `--now` does not waive (the agent's READY, a
    /// draft, a box, a hold, work under it, a background shell, a question for
    /// a person): [`Row::wait`] is named; `--skip` keeps it. Also `unwatched`
    /// ([`Row::unwatched`]), aterm's own failure, which no owner word mends.
    Waits,
    /// A given-up round ([`upgrade::GAVE_UP`]) resting before it asks again:
    /// `--now` asks at once. The one stopped kind it re-arms.
    AskAgain,
    /// `held-back:<owner>`: typing into the tab cannot reach it, so no word
    /// moves it; quit it in its pane and resume it there, or `--skip`.
    InItsPane,
    /// `refused:*`, `failed:*`: asked again only after a rest
    /// ([`Row::retry_at`]); sooner means quitting and resuming by hand, or
    /// `--skip`. `blocked:*` ([`BLOCKERS`]): no rung passes it; quit and
    /// resume by hand in its tab, or `--skip`.
    ByHand,
    /// A move that stopped after its agent was seen gone
    /// ([`Row::failed_after_exit`]): nothing to quit; the conversation is kept
    /// (`codex resume` or `claude --resume <conversation>` in the tab).
    ResumeInTab,
}

/// How long a person at the tab (`attended`) stays the owner's word for a
/// wait with no look to confirm it ([`Row::wait_words`]): three of the host's
/// attended looks, one [`upgrade::QUIET_S`] apart
/// (`upgrade_drive::after_attended`). Past it the wait is the last point's,
/// and the words say the move waits for the next turn end.
pub const ATTENDED_SEEN_S: u64 = 3 * upgrade::QUIET_S;

/// Whether `--now` moves an overdue upgrade past the wait a step recorded
/// ([`upgrade::wait_word`]). An overdue upgrade has stood at the Land rung
/// for hours, and `--now` is that same rung, so the settle, a person near the
/// tab and a running turn are nothing `--now` moves. What stands whatever the
/// owner says: READY, a draft, a box, a hold, work under the agent or its
/// daemon, a process proof, Claude's status off `idle`, a keystroke, and a
/// release line held by its gate (`release:<gate>`).
///
/// `--now` does move: a Codex act's back-off after leaving its text typed
/// (`left-typed-backoff`); a Claude notice behind a full queue (`queued`,
/// [`upgrade::Facts::queued`]), typed once more unless a limit stands; and a
/// notice aterm's own fence refused (`announce-refused:<why>`), asked again
/// through the same fence. Once that word is in force the remedy is the wait
/// ([`Row::fence_fails_under_now`]).
fn now_moves_past(wait: &str) -> bool {
    matches!(wait, "left-typed-backoff" | "queued") || wait.starts_with("announce-refused:")
}

/// A recorded wait in a person's words, for `agent` — the band's own words
/// ([`claude_wait_words`]) for a Claude Code record; `None` for a word that
/// has none. A Codex record's words are only its relaunch line's
/// ([`relaunch_line_words`]): its row reads its other waits with its own
/// state. The relaunch's stale bound names the wait that held it with this
/// (`relaunch::stale_exit_line`).
pub(in crate::harness) fn wait_words_of(agent: upgrade::Agent, wait: &str) -> Option<&'static str> {
    match agent {
        upgrade::Agent::Claude => claude_wait_words(wait),
        upgrade::Agent::Codex => relaunch_line_words(wait),
    }
}

/// Whether a row of `agent`'s reads its record's READY clock
/// (`St::ready_since`: [`Row::ready`], [`Row::late_ready`]): Claude Code's
/// lane keeps it. A Codex row says no READY stands, and a Codex round that
/// gave up hears no late READY (its lane's rule).
fn reads_ready_clock(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex => false,
    }
}

/// Whether `agent`'s lane types a release line (`upgrade_drive::release`,
/// [`Row::release`]): only Claude Code's, so only its rows owe one or wait
/// on a `release:` gate.
fn types_release(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex => false,
    }
}

/// Whether `agent`'s tab can run a goal the move pauses and resumes: Codex's
/// `/goal`, with its record in [`crate::harness::goal_hold`]
/// ([`Row::with_goal`], [`Row::waiting_line`]).
fn holds_goals(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => false,
        upgrade::Agent::Codex => true,
    }
}

/// Whether `agent`'s `attended` wait names the person at its tab on the
/// look's clock ([`ATTENDED_SEEN_S`], [`Row::person_seen`]): Claude Code's.
/// A Codex row says its `attended` as recorded.
fn attended_is_person(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex => false,
    }
}

/// Whether a move of `agent`'s that failed after its exit is an ordinary
/// stopped upgrade again once a live process holds its conversation
/// (resumed by hand, anywhere, any build): Claude Code's. A Codex move's
/// holder is the tab's Codex TUI, whose lane retires the record once current.
fn held_again_after_exit(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex => false,
    }
}

/// What a Claude Code wait stands for, in a person's words
/// ([`Row::wait_words`]): the agent's own work under it (a background-work
/// break, or Claude's `shell` status at an idle point), which `--now` cannot
/// move, and a notice waiting behind a full queue (`queued`), which it can
/// ([`now_moves_past`]). Every recorded wait has words.
fn claude_wait_words(wait: &str) -> Option<&'static str> {
    Some(match wait {
        "background" | "not-idle:shell" => "its own work runs",
        "busy" | "not-idle:busy" => "its turn is still running",
        "not-idle:waiting" => "it asked a question and waits",
        "settling" => "waiting for the tab to settle",
        // The quiet move's first look: this record is the notice, and the
        // move comes at a later look.
        upgrade::NOTICE_SHOWN => "it moves over once it is idle",
        // The round's next step is due at a point nothing holds
        // (`upgrade::NEXT_POINT`).
        upgrade::NEXT_POINT => NEXT_POINT_WORDS,
        // A click or a scroll stamps the tab as a keystroke does.
        "attended" => "someone is using its tab",
        "awaiting-ready" | "not-ready" => "waiting for it to agree to the move",
        // READY is on screen but not yet in the transcript: the next look
        // reads it.
        "ready-unrecorded" => "it agreed; its answer is being read",
        // It agreed, then got more work before the restart (a continuation,
        // a person's prompt): it is asked again once that turn is over,
        // never waited on for the re-ask's window.
        "superseded" => "it agreed, then got more work: it is asked again",
        "draft" => "a draft waits in its prompt",
        "box" => "a box on its screen waits for a choice",
        "held" => "its tab is held",
        // Its agent suspended (`^Z`) or in the background, its tab under
        // another supervisor's claim, its Autopilot off, or its agent
        // switched off by its own table ([`Row::aside`]): no worker of
        // aterm's runs there, and the words say whose hand holds it.
        NOT_FOREGROUND => "it is not in front in its tab",
        CLAIMED => "another supervisor holds its tab",
        AUTOPILOT_OFF => "its autopilot is off",
        AGENT_OFF => "its agent is switched off",
        "limited" => "it is at a usage limit",
        // Never `notice`: the upgrade's own word for the text it
        // types, which read as one of the band's rows.
        "queued" => "it has not read the upgrade's question yet",
        "login" => "it is not logged in",
        "in-flight" => "a step is under way",
        // A build that failed to start, not proved to start since
        // (`upgrade_drive`'s `build_start`): no agent is moved onto it.
        crate::harness::upgrade_drive::BUILD_FAILS => "its new build fails to start",
        // Aterm's own notice, refused by its fence: what could not be done,
        // and by whom.
        "announce-refused:changed" => {
            "aterm could not type its notice: the screen moved as it typed"
        }
        "announce-refused:yield" => {
            "aterm could not type its notice: someone kept typing in its tab"
        }
        "announce-refused:survey" => {
            "aterm left Claude Code's survey to the person and types its notice once it is gone"
        }
        // Stopped part-way, pressing no Enter.
        w if w.starts_with("announce-refused:cut-") => "aterm stopped its notice part-way, unsent",
        // Aterm's own words left in the prompt, never taken back
        // (`upgrade_drive::left_standing` only reads): aterm's, not a draft,
        // unless a person's keys are among them.
        "left-typed:mixed" => "aterm's words are in its prompt, with a person's keys",
        "left-typed:screen-unreadable" => "aterm cannot read its screen",
        w if w.starts_with("left-typed:") => "aterm's own words are still in its prompt, unsent",
        w if w.starts_with("announce-refused:") => "aterm could not type its notice",
        // The release gate holds the line an abandoned notice owes
        // (`wait:release:<gate>`, `upgrade_drive::owed_word`): the agent asked
        // to wind down has not been told to carry on. Never the gate's word
        // or "waits" (the agent may be working again). Said only while a
        // release is owed ([`Row::wait_words`]).
        w if w.starts_with("release:") => "it is owed the line telling it to carry on",
        w if w.starts_with("status-stale") => "its status lags",
        w if w.starts_with("not-idle") => "it has not gone idle",
        w => return relaunch_line_words(w),
    })
}

/// THE RELAUNCH LINE'S OWN WAITS, once the agent the move ended is gone
/// (`relaunch::type_relaunch_line`), alike
/// for Claude Code and Codex. Each says the state, never when it moves: the
/// live row adds that ([`Row::waiting_line`]).
fn relaunch_line_words(wait: &str) -> Option<&'static str> {
    Some(match wait {
        "exiting" => "it has not exited yet",
        "typing" => "the prompt it returned to does not read as empty",
        "mark-elsewhere" => "its shell and its screen disagree on where its prompt is",
        "unmarked" => "aterm sees no prompt where its shell returned",
        "prompt-moved" => "its prompt changed as aterm read it",
        "yield" | "cut" => "someone typed in its tab as the line that resumes it went in",
        "shell-prompt" => "its shell has not taken the terminal back",
        "screen-unreadable" => "aterm cannot read its screen",
        "tab-ownership-changed" => "another program holds its tab now",
        "conversation-owner-ambiguous" => "another process still holds its conversation",
        _ => return None,
    })
}

/// How long a session has been behind, in a person's words (`40 min`, `8 h`,
/// `3 days`), never the roster's `8h28m`. `None` for an unknown start (0), a
/// start not in the past, or one more than a year back.
fn behind_words(since: u64, now: u64) -> Option<String> {
    const YEAR_S: u64 = 365 * 86_400;
    let secs = now.checked_sub(since).filter(|_| since > 0)?;
    if secs == 0 || secs > YEAR_S {
        return None;
    }
    Some(elapsed_words(secs))
}

/// A span of `secs` in a person's words (`40 min`, `8 h`, `3 days`, at least
/// `1 min`), never the roster's `8h28m`. Used by [`behind_words`] and beside
/// it (`last asked 10 min ago` where no local clock is known).
#[must_use]
pub fn elapsed_words(secs: u64) -> String {
    if secs < 3_600 {
        format!("{} min", (secs / 60).max(1))
    } else if secs < 48 * 3_600 {
        format!("{} h", secs / 3_600)
    } else {
        format!("{} days", secs / 86_400)
    }
}

/// The first unix second after `now` at which [`behind_words`] of `since`
/// reads otherwise (the next minute, hour or day, by the unit it counts in);
/// `None` where it says nothing.
fn behind_words_change_at(since: u64, now: u64) -> Option<u64> {
    behind_words(since, now)?;
    let secs = now - since;
    let unit = if secs < 3_600 {
        60
    } else if secs < 48 * 3_600 {
        3_600
    } else {
        86_400
    };
    Some(since.saturating_add((secs / unit + 1) * unit))
}

/// Why a stall of `kind` ([`Row::stall`], any kind but `overdue`) will not
/// move on its own, by kind alone so the tab's mark is sent once per stall.
/// A Codex move stopped after `/exit` says where its conversation is
/// ([`codex_failure`]); a Claude Code move stopped after SIGTERM says so in
/// [`stop_words`]; a stuck restart says what it waits on ([`stuck_words`]).
fn stall_reason(agent: upgrade::Agent, kind: &str) -> String {
    let after_exit = match agent {
        upgrade::Agent::Claude => None,
        upgrade::Agent::Codex => kind.strip_prefix("failed:").and_then(codex_failure),
    };
    if let Some(words) = after_exit {
        return words.to_string();
    }
    if let Some(what) = kind.strip_prefix("stuck:") {
        return stuck_words(agent, what).to_string();
    }
    match kind {
        "goal-paused" => {
            "aterm paused its Codex goal for a moment to install the new Codex, and has not been \
             able to resume it yet"
                .to_string()
        }
        "goal-sandboxed" => {
            "aterm paused its Codex goal to install the new Codex, and leaves it paused: its \
             thread fell into a sandbox, where it can neither commit nor push"
                .to_string()
        }
        "refused:not-a-shell-job" => {
            "it is not its shell's foreground job, so nothing brings it back on the new build"
                .to_string()
        }
        // A `line:` refusal is the relaunch line's (`upgrade::relaunch_line`),
        // not the flags'.
        s if s.starts_with("refused:line:fish") => {
            "its shell is fish in a directory other than the agent's, and the relaunch line \
             cannot resume it from there"
                .to_string()
        }
        s if s.starts_with("refused:line:") => {
            "the line that would resume it cannot be typed at its shell (too long for the \
             terminal, or a control character in it)"
                .to_string()
        }
        s if s.starts_with("refused:") => {
            "its launch flags cannot be carried into a resume".to_string()
        }
        s if s.starts_with("failed:") => stop_words(agent, &s[7..]),
        // Said once, plainly; the remedy names the pane.
        s if s.starts_with("held-back:") => format!(
            "it runs inside {}, where the upgrade cannot type to it",
            runs_under(&s[10..])
        ),
        "blocked:no-shell-integration" => {
            "its tab's shell integration is not reaching aterm (as after an aterm update), so \
             once Codex quits aterm cannot tell which conversation to resume — and it does not \
             guess one"
                .to_string()
        }
        "blocked:screen-unreadable" => {
            "aterm cannot read its screen, so it cannot tell when it may move".to_string()
        }
        other => other.to_string(),
    }
}

/// Why a move stopped, in a person's words: never the raw word (that stays on
/// `--status` and the `upgrade=` column) and never "restart"; says what
/// happened to the agent.
fn stop_words(agent: upgrade::Agent, why: &str) -> String {
    let who = match agent {
        upgrade::Agent::Claude => "Claude",
        upgrade::Agent::Codex => "Codex",
    };
    match why {
        "signal-refused" => format!("the system refused to stop the old {who}"),
        "no-resume" => format!("the new {who} never picked the conversation back up"),
        "stale-exit" => format!("{who} ended and was never started again on the new build"),
        "shell-gone" => {
            "the shell it ran in is gone, so there is no prompt to start it at".to_string()
        }
        "exited-before-continuing" => format!("the new {who} quit before it carried on"),
        "relaunch-refused" => {
            "its prompt refused the line that starts it on the new build".to_string()
        }
        "resumed-elsewhere" => {
            "the conversation was resumed by hand, outside this upgrade".to_string()
        }
        "build-fails" => {
            format!("its new build fails to start, so the move was called off and {who} runs on")
        }
        _ => "the move stopped before it finished".to_string(),
    }
}

/// Why a Codex move stopped, for the kinds that stop it after its `/exit`:
/// what became of the conversation and what takes it back. `None` for the
/// rest, whose generic words stand.
fn codex_failure(why: &str) -> Option<&'static str> {
    Some(match why {
        "no-resume-hint" => {
            "its `/exit` ended it without naming its thread; the conversation runs on in its \
             Codex daemon, and `codex resume` in the tab takes it back"
        }
        "hint-mismatch" => {
            "its `/exit` named another thread than the one it held, so nothing was resumed; \
             `codex resume` in the tab lists the conversations"
        }
        "no-resume" => "its resume line was typed at the prompt and no Codex came up in the tab",
        "stale-exit" => "its `/exit` was typed minutes ago and it was never resumed",
        "shell-gone" => "the shell it ran in is gone: there is no prompt to resume it at",
        "relaunch-refused" => "the line that resumes it was refused at its prompt",
        "no-rollout" => "its conversation's rollout is gone: there is nothing to resume",
        "resumed-elsewhere" => "another Codex, not the one this upgrade resumed, runs in the tab",
        _ => return None,
    })
}

/// What a restart under way that has not moved waits on, once its `phase`
/// has stood longer than [`STALE_S`]: `exiting` (asked to end and has not),
/// `exited` (ended at `exited_at`, resume line not typed: a person at the
/// prompt, a hold) or `relaunched` (line typed, the new agent has not held
/// the conversation or reached idle; usually a box). Nothing is forced: the
/// stall only names the wait. `None` for other phases and within the bound.
/// Read by both [`Row::stuck`] and the sweep's once-only ledger note
/// (`St::stuck`).
pub(super) fn stuck_on(phase: &Phase, exited_at: u64, now: u64) -> Option<&'static str> {
    let (at_s, what) = match *phase {
        Phase::Exiting { at_s } if exited_at == 0 => (at_s, "exiting"),
        Phase::Exiting { at_s } => (at_s, "exited"),
        Phase::Relaunched { at_s } => (at_s, "relaunched"),
        _ => return None,
    };
    (now.saturating_sub(at_s) > STALE_S).then_some(what)
}

/// What a stuck restart waits on ([`Row::stuck`]), in words that ask for
/// nothing (nothing is forced, and no owner word moves a move under way).
/// Stable per kind, for the tab's mark and the sweep's ledger note
/// (`note_stuck`).
pub(super) fn stuck_words(agent: upgrade::Agent, what: &str) -> &'static str {
    match (agent, what) {
        (upgrade::Agent::Codex, "exiting") => {
            "the /exit typed into it for the upgrade has not ended it yet; nothing is forced"
        }
        (upgrade::Agent::Claude, "exiting") => {
            "it was asked to end for the upgrade and has not exited, its own shutdown still \
             running; nothing is forced"
        }
        (upgrade::Agent::Claude | upgrade::Agent::Codex, "exited") => {
            "it ended for the upgrade, and the line that resumes it waits until a person or a \
             hold lets go of its prompt; nothing is forced"
        }
        (upgrade::Agent::Claude | upgrade::Agent::Codex, _) => {
            "the line that resumes it was typed, and the agent it started has not reached an \
             idle point, perhaps behind a box that waits on a person; nothing is forced"
        }
    }
}

/// What a held-back agent runs under, for a line: the terminal owner's name
/// (`tmux`), or, where there is none to name (`none`, and `?` or `-` for a
/// name with no printable character), a terminal that is not the tab's.
pub(in crate::harness) fn runs_under(owner: &str) -> &str {
    match owner {
        "" | "none" | "?" | "-" => "a terminal that is not the tab's",
        name => name,
    }
}

/// Whether a stop's reason is a refusal (the upgrade would not type or signal
/// at all): not the shell's job, uncarriable launch flags (`argv:`), an
/// untypeable relaunch line (`line:`). A re-armed round meets it again, so it
/// stays a stall shown to the owner ([`Row::stall`]).
fn refusal(why: &str) -> bool {
    why == "not-a-shell-job" || why.starts_with("argv:") || why.starts_with("line:")
}

/// A third party's or a derived word, cut to what a roster token may carry.
fn word(s: &str) -> String {
    let keep =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '(' | ')');
    // A word that is a token already (every kept character is ASCII, so at
    // most 64 bytes is at most 64 characters) is itself: copied whole rather
    // than rebuilt a character at a time. Every row's need builds several.
    if !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| keep(char::from(b))) {
        return s.to_owned();
    }
    let w: String = s.chars().filter(|c| keep(*c)).take(64).collect();
    if w.is_empty() { "-".to_string() } else { w }
}

/// Every upgrade record as a [`Row`], sorted by conversation, limited to the
/// tab `opts` names if any. Files only, no socket or lock (each file is
/// replaced whole by rename). Unreadable records are left out ([`read_rows`]
/// says whether any were).
#[must_use]
pub fn rows(opts: &Opts) -> Vec<Row> {
    rows_at(opts, now_s())
}

fn rows_at(opts: &Opts, now: u64) -> Vec<Row> {
    read_rows(opts, now).0
}

/// [`rows_at`], plus whether the records were read whole: `false` for an
/// unlistable directory, a listing that broke off, or an unreadable record
/// (reading those as no records would make the window withdraw stall rows
/// that still stand). No directory at all is whole. A record removed between
/// listing and read, a directory, and a file that is no upgrade record
/// (`models.json`, a relaunch record) are not records.
fn read_rows(opts: &Opts, now: u64) -> (Vec<Row>, bool) {
    let dir = match std::fs::read_dir(state_dir(opts)) {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), true),
        Err(_) => return (Vec::new(), false),
    };
    let mut whole = true;
    let mut out: Vec<Row> = Vec::new();
    for entry in dir {
        let Ok(entry) = entry else {
            whole = false;
            continue;
        };
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::IsADirectory
                ) =>
            {
                continue;
            }
            Err(_) => {
                whole = false;
                continue;
            }
        };
        // Relaunch records sit beside the upgrade's (`St::cause`): an agent
        // relaunched after an exit, memory or model restart is no upgrade; the
        // upgrade's own restart of a taskless conversation is.
        let Some(st) = St::from_json(&text).filter(St::is_upgrade) else {
            continue;
        };
        if opts.only_sid.as_ref().is_none_or(|s| *s == st.tab) {
            // A Codex tab's goal record (`goal_hold`), beside its loop's
            // ledger under the aterm state root.
            let root = opts
                .aterm_state
                .as_deref()
                .filter(|_| holds_goals(st.agent) && !st.tab.is_empty());
            let hold = root.and_then(|root| {
                super::super::goal_hold::read(&super::super::goal_hold::path(root, &st.tab))
            });
            // A save-then-wait switch open on the tab: its loop's ledger
            // holds a wind-down no row closed (read only for a goal held).
            let switch = root.is_some_and(|root| {
                hold.as_ref()
                    .is_some_and(super::super::goal_hold::Hold::owes)
                    && crate::supervise::approvals::open_wind_down(
                        &crate::supervise::approvals::ledger_under(root, Some(&st.tab)),
                        Some(&st.tab),
                    )
                    .is_some()
            });
            out.push(Row::of(&session, &st, now).in_flow(opts, now).with_goal(
                hold.as_ref(),
                switch,
                now,
            ));
        }
    }
    out.sort_by(|a, b| a.session.cmp(&b.session));
    (out, whole)
}

/// [`rows`] as `aterm harness upgrade --status` shows them: each
/// [`Row::standing`] where Claude's session files read whole (`true`), else
/// every recorded row (`false`: the caller says they are unchecked). With no
/// roster, a tab is proven by the agent's environment alone, and an
/// unreadable environment disproves nothing. A row with no recorded tab is
/// shown under its holder's proven tab, then matched against `opts`'s tab.
/// Where `claims` (the window's host runs: `[harness] enabled` and `upgrade`
/// both on), the instance is asked once which tabs another supervisor holds
/// and which tabs' Autopilot is Off ([`roster_aside`]) and those rows read so
/// ([`Row::set_aside`]); no answer means none.
#[must_use]
pub fn status_rows(opts: &Opts, claims: bool) -> (Vec<Row>, bool) {
    status_rows_with(
        opts,
        now_s(),
        &|sessions| holders(opts, sessions, None),
        &|| aside_tabs(opts, claims),
    )
}

/// The tabs another supervisor's claim holds and those whose Autopilot is
/// Off, as the instance at `opts` names them ([`roster_aside`]) — asked only
/// where `claims` (the window's host running at all: with a switch off, the
/// switch is why no host watches, [`status_rows`]); none where the instance
/// does not answer.
///
/// EVERY INSTANCE ON THE MACHINE: a conversation's tab may live
/// in any running aterm, and `--status` reads every record, so the roster of
/// every instance `aterm ctl ls` lists ([`aterm_ctl::local_instances`]) is
/// read, each bounded by [`roster_aside`]'s own wait; tab ids are unique
/// across instances. An explicit socket (`--sock`), or a directory that
/// cannot be listed, asks the one instance it names.
fn aside_tabs(opts: &Opts, claims: bool) -> Aside {
    if !claims {
        return Aside::default();
    }
    let one = || {
        connect(opts, opts.only_sid.as_deref().unwrap_or(""))
            .ok()
            .and_then(|mut c| roster_aside(&mut c, &opts.agents_off))
            .unwrap_or_default()
    };
    if opts.sock.is_some() {
        return one();
    }
    match aterm_ctl::local_instances() {
        Ok(instances) if !instances.is_empty() => super::aside_of(
            instances
                .iter()
                .filter_map(|(_, sock)| super::Client::connect_local_beside(sock).ok()),
            &opts.agents_off,
        ),
        _ => one(),
    }
}

/// [`status_rows`] at `now`, over `holders` and the tabs `aside` names:
/// asked once, and only when some row is vetted by its holders — no record
/// to vet, no dial.
fn status_rows_with(
    opts: &Opts,
    now: u64,
    holders: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>,
    aside: &dyn Fn() -> Aside,
) -> (Vec<Row>, bool) {
    let all = rows_at(
        &Opts {
            only_sid: None,
            ..opts.clone()
        },
        now,
    );
    let vet = sessions_to_vet(&all);
    let (mut shown, vetted) = if vet.is_empty() {
        (all, true)
    } else {
        match holders(&vet).map(|held| (held, aside())) {
            Some((held, aside)) => (
                all.into_iter()
                    .map(|mut r| {
                        r.held_again(&held);
                        r
                    })
                    .filter(|r| r.standing(&held, true))
                    .map(|mut r| {
                        // Standing only by a holder proven in no tab: no
                        // host watches it ([`Row::outside`]).
                        if r.held_by_a_process() && !r.standing(&held, false) {
                            r.outside = true;
                            r.read_at(now);
                        }
                        // An unreached row reads its holder's live status
                        // before its tab is filled in from the proof.
                        r.unreached_status(&held, true, now);
                        if r.tab.is_empty() {
                            r.tab = r.proven_tab(&held).unwrap_or_default();
                        }
                        // Held only by an agent not in front in its tab,
                        // another supervisor's claim on it, or its Autopilot
                        // off: no host runs a worker there ([`Row::aside`]) —
                        // read once its tab is known, so a record written
                        // before records named their tab is read by the
                        // claim on the tab its holder is proven in.
                        r.set_aside(&held, true, &aside, now);
                        r
                    })
                    .collect(),
                true,
            ),
            None => (all, false),
        }
    };
    shown.retain(|r| opts.only_sid.as_ref().is_none_or(|s| *s == r.tab));
    (shown, vetted)
}

/// The conversations whose rows [`Row::standing`] must vet — a Claude Code
/// move that failed after its exit among them, for a process that holds it
/// again ([`Row::held_again`]).
fn sessions_to_vet(rows: &[Row]) -> BTreeSet<String> {
    rows.iter()
        .filter(|r| {
            r.held_by_a_process() || (held_again_after_exit(r.agent) && r.failed_after_exit())
        })
        .map(|r| r.session.clone())
        .collect()
}

// ---------------------------------------------------------------- who holds it now

/// One live Claude Code process a recorded upgrade is vetted against
/// ([`Row::standing`]): the conversation it holds, its build (its session
/// file's `version`), the tab it is proven to run in (`None` without proof),
/// and Claude's status in that file (`busy`, `shell`, `idle`, `waiting`) and
/// since when, which an unreached row reads as its wait
/// ([`Row::unreached_status`]). A Codex holder has no status (empty, `0`).
///
/// `aside`: proven not in front of its terminal ([`not_in_front`]: suspended
/// or backgrounded) where its tab was proven by environment alone. The host
/// runs a worker, and watches, only where the foreground is an agent, so a
/// record it holds is not stamped until it is back in front ([`Row::aside`]).
/// `false` without such proof and for Codex holders.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Holder {
    pub(super) session: String,
    pub(super) version: String,
    pub(super) tab: Option<String>,
    pub(super) status: String,
    pub(super) status_since: u64,
    pub(super) aside: bool,
}

/// Whether `pid` is proven not the foreground of its terminal, from the kernel
/// now (its group vs. its controlling terminal's foreground group): a job
/// suspended or backgrounded by its shell. No controlling terminal or
/// unreadable numbers prove nothing (`false`). An agent leading its own
/// multiplexer pane is not aside.
fn not_in_front(pid: u32) -> bool {
    super::ids(pid).is_some_and(|(_, group, front)| group > 0 && front > 0 && group != front)
}

/// The live holders of `sessions`, from Claude's session files — the user's
/// own `~/.claude` and every other Claude home the records of `sessions` name
/// ([`St::home`]: where the visit found each agent; read in `~/.claude`
/// alone, an agent under another home held nothing, and its upgrade was never
/// shown or counted) — a file whose
/// pid is alive and started when the file says ([`conversation_live`]; a
/// recycled pid holds nothing). Its tab is proven by its process group being
/// one tab's foreground group in `tabs` (the host's `who` roster), or by its
/// environment's `ATERM_PARENT_SESSION_ID`; two disagreeing proofs prove
/// nothing. One proven by environment alone is checked against its
/// terminal's foreground ([`Holder::aside`]). `None` when the session files
/// of any of those homes cannot be read whole; no sessions directory at all
/// means nothing holds anything there. Only the files of `sessions` cost a
/// read (a `ps` and a `KERN_PROCARGS2` each).
fn holders(
    opts: &Opts,
    sessions: &BTreeSet<String>,
    tabs: Option<&[LiveTab]>,
) -> Option<Vec<Holder>> {
    let home = opts.home.as_path();
    // A Codex upgrade (`codex-<tab>`) is held by the Codex TUI that leads its
    // tab now ([`super::codex::holders`]); Claude's files say nothing of it.
    let codex = super::codex::holders(home, sessions, tabs);
    if sessions.iter().all(|s| s.starts_with("codex-")) {
        return Some(codex);
    }
    let first = match session_files(home) {
        Some(files) => files,
        None if matches!(
            std::fs::symlink_metadata(home.join(".claude/sessions")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound
        ) =>
        {
            Vec::new()
        }
        None => return None,
    };
    let files = super::Homes::of_records(opts, sessions).roster(Some(first))?;
    Some(
        files
            .into_iter()
            .filter(|sf| sessions.contains(&sf.session_id))
            .filter(|sf| conversation_live(std::slice::from_ref(sf), &sf.session_id))
            .map(|sf| {
                let by_group = tabs.and_then(|tabs| {
                    unique_tab_for_group(tabs, process_group(sf.pid)?).map(str::to_owned)
                });
                let by_env = atpkg::caller_shell::process_args(sf.pid)
                    .and_then(|a| a.env_var("ATERM_PARENT_SESSION_ID").map(str::to_owned));
                // Proven by its group, it is its tab's foreground; by its
                // environment alone, the kernel says whether it is its
                // terminal's.
                let aside = by_group.is_none() && by_env.is_some() && not_in_front(sf.pid);
                let tab = match (by_group, by_env) {
                    (Some(group), Some(env)) if group != env => None,
                    (group, env) => group.or(env),
                };
                Holder {
                    session: sf.session_id,
                    version: sf.version,
                    tab,
                    status: sf.status,
                    status_since: sf.status_updated_at_ms / 1_000,
                    aside,
                }
            })
            .chain(codex)
            .collect(),
    )
}

// ---------------------------------------------------------------- the owner's word

/// What the owner asks of one tab's upgrade (`aterm harness upgrade <sid>
/// --now|--defer <dur>|--skip`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Restart at the next turn end ([`Request::Now`]), waiving the settling
    /// window and the attended-tab guard. A given-up upgrade is re-armed at
    /// once (the notice typed again, then the restart after READY) rather than
    /// at its next round ([`upgrade::RETRY_S`]). A refused or failed one is
    /// not ([`ask`] refuses); it starts its next round after its rest.
    Now,
    /// Not for this many seconds ([`Request::DeferUntil`]).
    Defer(u64),
    /// Stay on the running build until a newer one than the target comes
    /// ([`Request::Skip`]).
    Skip,
}

/// How long [`ask`] waits for a step that holds the lock. A step that is
/// restarting a session can hold it for minutes (its waits are bounded at
/// 30 + 15 + 90 + 120 s); past this the ask says so and changes nothing.
#[cfg(not(test))]
const ASK_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The same bound in this crate's tests, short: the CLI's busy exit is what
/// they check, and [`ask_within`]'s own test proves the wait is waited.
#[cfg(test)]
const ASK_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(300);

/// Prefix of [`ask`]'s refusal of `--now` for a stopped round the owner's
/// word may not re-arm: `stopped:<next round, unix s; 0 at next
/// look>:<sentence>`, read by the window ([`refused_stopped`]), never printed
/// by the CLI ([`refusal_sentence`]).
pub const REFUSED_STOPPED: &str = "stopped:";

/// A `--now` refused because its round stopped ([`REFUSED_STOPPED`]): when
/// its next round starts (unix seconds, `0` at its next look) and the CLI's
/// sentence.
#[must_use]
pub fn refused_stopped(e: &str) -> Option<(u64, &str)> {
    let (at, sentence) = e.strip_prefix(REFUSED_STOPPED)?.split_once(':')?;
    Some((at.parse().ok()?, sentence))
}

/// What a person at a shell is told for [`ask`]'s refusal: the sentence,
/// without the machine prefix a window reads.
#[must_use]
pub fn refusal_sentence(e: &str) -> &str {
    refused_stopped(e).map_or(e, |(_, sentence)| sentence)
}

/// Writes the owner's word for the upgrade in tab `sid`, under the sweep
/// lock, with one ledger line (`requested:<word>`). The word applies to the
/// conversation in that tab, not the same conversation resumed elsewhere
/// (`St::request_for`). The target is the tab's newest upgrade not yet
/// restarting (pending, announced or stopped), preferring one a live process
/// holds in that tab ([`Row::standing`]).
///
/// THE ROW IT ANSWERS IS READ AS `--status` READS IT: set aside by the one
/// rule ([`Row::set_aside`]) — its holder not in front in its tab, or another
/// supervisor's claim on the tab or its Autopilot off where `claims` (as
/// [`status_rows`]'s) — so a `--now` on a tab no host runs a worker for never
/// prints `stalled=unwatched`, "aterm's own fault", where `--status` names
/// what holds it. Claims are read off this instance alone.
///
/// # Errors
///
/// As above; the state directory is left as it was.
pub fn ask(opts: &Opts, sid: &str, what: Ask, claims: bool) -> Result<Row, String> {
    ask_aside(opts, sid, what, &|| aside_tabs(opts, claims))
}

/// [`ask`] over the tabs `aside` names, asked only for a row a live process
/// may hold.
fn ask_aside(opts: &Opts, sid: &str, what: Ask, aside: &dyn Fn() -> Aside) -> Result<Row, String> {
    let (mut row, held) = ask_within(opts, sid, what, None, ASK_LOCK_WAIT)?;
    if row.held_by_a_process() {
        row.set_aside(&held, true, &aside(), now_s());
    }
    Ok(row)
}

/// [`ask`] for the build the owner was shown: written only if the tab's
/// upgrade still moves to `target` (equal as versions, else as text), so a
/// press on a stale band row or menu never skips or hurries a newer build.
/// Checked under the same lock as the write.
///
/// # Errors
///
/// [`ask`]'s, and `stale:<the target now>` when the upgrade moved on;
/// nothing is written.
pub fn ask_for(opts: &Opts, sid: &str, what: Ask, target: &str) -> Result<Row, String> {
    // The window lays the aside its own last look read over the answer
    // (`Row::keep_aside`).
    ask_within(opts, sid, what, Some(target), ASK_LOCK_WAIT).map(|(row, _)| row)
}

/// The word written as [`ask`] writes it, waiting at most `wait` for the
/// lock; with `target`, [`ask_for`]. The row, and the live holders it was
/// chosen by.
fn ask_within(
    opts: &Opts,
    sid: &str,
    what: Ask,
    target: Option<&str>,
    wait: std::time::Duration,
) -> Result<(Row, Vec<Holder>), String> {
    let started = std::time::Instant::now();
    let _held = loop {
        match sweep_lock(&Opts {
            dry_run: false,
            ..opts.clone()
        }) {
            Ok(held) => break held,
            Err("another-sweep") if started.elapsed() < wait => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(why) => return Err(format!("busy:{why}")),
        }
    };
    let now = now_s();
    let mut candidates: Vec<(String, St)> = std::fs::read_dir(state_dir(opts))
        .map(|dir| {
            dir.flatten()
                .filter_map(|e| {
                    let path = e.path();
                    if path.extension().is_none_or(|x| x != "json") {
                        return None;
                    }
                    let session = path.file_stem()?.to_string_lossy().into_owned();
                    // An upgrade's record, never a relaunch's (`St::cause`).
                    let st = load(opts, &session).filter(|st| st.cause.is_empty())?;
                    // A state with no recorded tab (an older build's pending
                    // one) is a candidate only once a live holder is proven in
                    // `sid`, below.
                    (st.tab == sid || st.tab.is_empty()).then_some((session, st))
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some((_, st)) = candidates
        .iter()
        .find(|(_, st)| st.tab == sid && st.in_flight())
    {
        let begun = match st.agent {
            upgrade::Agent::Claude => "once the agent is signalled",
            // No Codex is ever signalled: its move begins with a typed `/exit`.
            upgrade::Agent::Codex => "once its `/exit` is typed",
        };
        return Err(format!(
            "the restart in tab {sid} is already under way: it is not held or hurried {begun}"
        ));
    }
    candidates.retain(|(_, st)| {
        matches!(
            st.phase,
            Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
        )
    });
    // One a live process still holds in this tab first (a file an ended
    // conversation left must not take the word meant for the live one), then
    // the newest upgrade that has not stopped, else the newest stopped one.
    let held = holders(
        opts,
        &candidates.iter().map(|(s, _)| s.clone()).collect(),
        None,
    )
    .unwrap_or_default();
    candidates.retain(|(session, st)| {
        !st.tab.is_empty() || Row::of(session, st, now).proven_tab(&held).as_deref() == Some(sid)
    });
    candidates.sort_by_key(|(session, st)| {
        let mut row = Row::of(session, st, now);
        row.held_again(&held);
        (
            row.standing(&held, true),
            !matches!(st.phase, Phase::Failed(_)),
            st.salt,
        )
    });
    let Some((session, mut st)) = candidates.pop() else {
        return Err(format!(
            "no upgrade is recorded for tab {sid}; `aterm harness upgrade {sid} --dry-run` \
             shows the plan"
        ));
    };
    // A word given for another build than the one this upgrade moves to now
    // was given about a row that has moved on: refused whole ([`ask_for`]).
    if let Some(shown) = target
        && !same_target(shown, &st.to)
    {
        return Err(format!("stale:{}", word(&st.to)));
    }
    match what {
        Ask::Now => {
            match &st.phase {
                // Gave up (no actionable READY after the last notice,
                // `Step::GiveUp`): re-armed, every gate asked again from the
                // notice. Nothing is killed to pass them.
                Phase::Failed(why) if why == upgrade::GAVE_UP => {
                    st.phase = Phase::Pending;
                    st.forget_markers();
                    st.noted.clear();
                }
                // Refused or failed: what stopped it may still hold (an
                // unresumable launch, no shell to relaunch at, a conversation
                // resumed by hand), and re-arming would type a notice and later
                // signal an agent the harness had stopped acting on. A move
                // stopped after its exit has nothing left to quit, unless a
                // process holds its conversation again
                // ([`held_again_after_exit`]).
                Phase::Failed(why)
                    if st.exited_at != 0
                        && !(held_again_after_exit(st.agent)
                            && held.iter().any(|h| h.session == session)) =>
                {
                    return Err(match st.agent {
                        // Its conversation is `codex resume`'s.
                        upgrade::Agent::Codex => format!(
                            "the upgrade in tab {sid} stopped after its `/exit` ({}): `--now` \
                             re-arms only one that gave up — `codex resume` in the tab takes \
                             its conversation back, and `--skip` quiets this record",
                            word(why)
                        ),
                        upgrade::Agent::Claude => format!(
                            "the upgrade in tab {sid} stopped after its SIGTERM ended the agent \
                             ({}): `--now` re-arms only one that gave up — `claude --resume {}` \
                             in the tab takes its conversation back, and `--skip` quiets this \
                             record",
                            word(why),
                            word(&session)
                        ),
                    });
                }
                Phase::Failed(why) => {
                    let row = Row::of(&session, &st, now);
                    let (next, at) = match row.next_round_in(now) {
                        Some(0) | None => ("at its next look".to_string(), 0),
                        Some(secs) => (
                            format!("in {}", upgrade::span(secs)),
                            now.saturating_add(secs),
                        ),
                    };
                    // Typed for the window: the band words it
                    // ([`refused_stopped`]); the CLI prints the sentence
                    // ([`refusal_sentence`]).
                    return Err(format!(
                        "{REFUSED_STOPPED}{at}:the upgrade in tab {sid} stopped ({}): `--now` \
                         re-arms only one that gave up (no READY answer it could act on after its \
                         last notice) — this one starts a new round on its own {next}; `--skip` \
                         keeps it on {}, or quit it and resume it by hand",
                        word(why),
                        word(&st.from)
                    ));
                }
                _ => {}
            }
            st.request = Request::Now;
            // A Codex act whose text was left typed waits before it is tried
            // again (`St::left_at`); the owner's `--now` is that try.
            st.left_at = 0;
        }
        Ask::Defer(secs) => st.request = Request::DeferUntil(now.saturating_add(secs)),
        Ask::Skip => st.request = Request::Skip(st.to.clone()),
    }
    st.request_at = now;
    // Every word arms a new round (`St::new_round`): a fresh salt, so no
    // READY given before the word matches a notice typed after it.
    st.new_round(now);
    // A hold ends an announcement. A held upgrade owns none of the session's
    // turn ends, so the supervisor continues the worker past the requested
    // wind-down; keeping the notice and its READY would read as consent when
    // the hold ends and restart an agent at work with no fresh notice. The
    // upgrade is pending again with its marker gone: a new notice and a new
    // READY are needed.
    //
    // The agent, asked to wind down for a restart now held, is owed one line
    // telling it to go on ([`St::owe_release`], typed by the next step that
    // may type).
    if matches!(st.phase, Phase::Announced { .. }) && st.request.holds(&st.phase, &st.to, now) {
        st.owe_release(if matches!(st.request, Request::Skip(_)) {
            "skipped"
        } else {
            "deferred"
        });
        st.phase = Phase::Pending;
        st.forget_markers();
        st.noted.clear();
    }
    // A given-up upgrade still honours a READY to its notices
    // (`upgrade::next_step`); a word holds that too, so no answer given
    // before the word acts after it.
    if matches!(&st.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) && what != Ask::Now {
        st.forget_markers();
    }
    // The tab the word is on; for a state with none recorded, the tab its
    // holder was just proven in.
    if st.tab.is_empty() {
        st.tab = sid.to_string();
    }
    st.request_tab = sid.to_string();
    save(
        &Opts {
            dry_run: false,
            ..opts.clone()
        },
        &session,
        &st,
    );
    let report = Report {
        pid: st.pid,
        tab: sid.to_string(),
        session: session.clone(),
        from: st.from.clone(),
        to: format!("{}({})", st.to, st.source),
        step: format!("requested:{}", st.request.word()),
    };
    ledger(
        &Opts {
            dry_run: false,
            ..opts.clone()
        },
        &report,
        "the owner's word (aterm harness upgrade --now|--defer|--skip)",
    );
    announce_word(opts, now);
    Ok((Row::of(&session, &st, now).in_flow(opts, now), held))
}

/// Whether the build a word was given for is the one an upgrade moves to:
/// equal as versions (`2.1` is `2.1.0`), or, where either does not parse, as
/// text. An empty `shown` names no build and matches none.
fn same_target(shown: &str, to: &str) -> bool {
    match (upgrade::Version::parse(shown), upgrade::Version::parse(to)) {
        (Some(a), Some(b)) => a == b,
        _ => !shown.is_empty() && shown == to,
    }
}

// ---------------------------------------------------------------- the window's host

/// The window's view of its tabs' upgrades, kept by its host thread
/// (`aterm-gui`'s `harness_host`). Refreshed after workers act, on an
/// activation notice or the owner's word ([`word_marker`]), on roster
/// changes, at the instant [`Self::refresh`] names, and when a followed tab's
/// screen moves ([`Self::follows`]); never on a timer of its own. Holds the
/// rows last handed to the window (re-sent only when changed; a [`Row`]
/// carries instants, not ages) and the stalled tabs it marked.
#[derive(Debug, Default)]
pub struct View {
    sent: Option<Vec<Row>>,
    badged: BTreeMap<String, String>,
    sock: Option<String>,
    /// Looks in a row whose roster could not be read (socket not bound yet, a
    /// busy window): the next look comes on a growing pause ([`UNREAD_RETRY`]).
    unread: u32,
    /// The tabs whose row the last whole look read off its live holder's
    /// status ([`Self::follows`]).
    follows: Vec<String>,
    /// Tabs with a record the last whole look found past its watch
    /// ([`Self::watch_due`]).
    watch: Vec<String>,
    /// Since when this view's host has been watching, unix seconds
    /// ([`Self::note_look`]): its first look since the view began or stood
    /// down, or after the host slept. Within [`WAKE_GRACE_S`] of it no row
    /// reads `unwatched` ([`Row::host_watching_since`]).
    watching_since: u64,
    /// The last look's clocks, monotonic and wall (`None`: no look since the
    /// view began or stood down).
    looked: Option<(std::time::Instant, u64)>,
    /// Tabs whose worker the host parks behind another supervisor's claim
    /// ([`Self::set_claimed`]), and those whose Autopilot is Off
    /// ([`Self::set_off`]).
    aside: Aside,
    /// The live tabs whose `owner=upgrade` attention this view has SETTLED
    /// since it began or stood down ([`Self::reconcile`]): lowered where it
    /// wanted none.
    settled: BTreeSet<String>,
}

/// How far the wall clock may run ahead of the monotonic one between two looks
/// before the host is taken to have slept ([`View::note_look`]): past any
/// clock slew, well short of a watch gap.
const SLEPT_S: u64 = 60;

/// Pauses before looking again at an unreadable roster, from two seconds to
/// ten minutes. Only a failed read schedules one.
const UNREAD_RETRY: [std::time::Duration; 4] = [
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(16),
    std::time::Duration::from_secs(120),
    std::time::Duration::from_secs(600),
];

impl View {
    /// Looks at the upgrades of the instance at `opts.sock`: hands the rows of
    /// its live tabs (a finished restart for [`DONE_SHOWN_S`]) to `summary`
    /// when they changed, raises each stalled tab's attention and lowers each
    /// recovered one's. An unreadable roster changes nothing (an unseen tab
    /// has not recovered) and asks for another look on a growing pause
    /// ([`UNREAD_RETRY`]). Returns the unix second at which a row next changes
    /// by time alone ([`next_change`]), or `None`; the host looks again then.
    pub fn refresh(&mut self, opts: &Opts, summary: &mut dyn FnMut(&[Row])) -> Option<u64> {
        self.note_look(std::time::Instant::now(), now_s());
        let Some(tabs) = connect(opts, "")
            .ok()
            .and_then(|mut c| super::host_roster(&mut c))
        else {
            let pause = crate::supervise::ladder::Ladder(&UNREAD_RETRY)
                .step(usize::try_from(self.unread).unwrap_or(usize::MAX));
            self.unread = self.unread.saturating_add(1);
            // A roster that cannot be read names no tab due: nothing is
            // watched off a guess.
            self.watch.clear();
            return Some(now_s().saturating_add(pause.as_secs()));
        };
        self.unread = 0;
        self.refresh_with(
            opts,
            &tabs,
            now_s(),
            &|sessions| holders(opts, sessions, Some(&tabs)),
            summary,
        )
    }

    /// [`Self::refresh`] at `now` over the roster `tabs`, with `holders`
    /// reading the live holders of a set of conversations ([`holders`] in the
    /// host, a script in tests). Only [`Row::standing`] rows are handed over
    /// or marked. Unreadable session files or upgrade records ([`read_rows`])
    /// change nothing, since what is handed over is taken as a whole look.
    fn refresh_with(
        &mut self,
        opts: &Opts,
        tabs: &[LiveTab],
        now: u64,
        holders: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>,
        summary: &mut dyn FnMut(&[Row]),
    ) -> Option<u64> {
        let (mut all, whole) = read_rows(opts, now);
        // What this host has not yet had the chance to watch is not missed
        // ([`Self::note_look`]).
        for r in &mut all {
            r.host_watching_since(self.watching_since, now);
        }
        // A record with no tab (older builds recorded it only at the
        // announcement): its tab is the one its live holder is proven in, as
        // `--status` shows it ([`status_rows`]). Read for the watch alone; its
        // row is shown as before.
        let (untabbed, all): (Vec<Row>, Vec<Row>) = all
            .into_iter()
            .partition(|r| r.tab.is_empty() && r.watch_at != 0 && r.held_by_a_process());
        let mut mine: Vec<Row> = all
            .into_iter()
            .filter(|r| tab_is_live(tabs, &r.tab))
            .filter(|r| {
                r.phase != Phase::Done
                    || r.restarting(now)
                    || now.saturating_sub(r.finished_at()) <= DONE_SHOWN_S
                    || r.goal_held_since != 0
                    || r.goal_left_since != 0
            })
            .collect();
        // The watch: every live tab with a record past its watch, whether or
        // not its row stands, its holders read, or other records read whole.
        // Each record decides its own watch, and the host hands each tab to
        // the watch, which reads it off any point. The next instant is named
        // the same way even when a look stops short, or the host would keep
        // no timer for a session offering no point.
        let mut watch: Vec<String> = mine
            .iter()
            .filter(|r| r.watch_at != 0 && r.watch_at <= now)
            .map(|r| r.tab.clone())
            .collect();
        watch.sort();
        watch.dedup();
        self.watch = watch;
        let watch_next = mine.iter().map(|r| r.watch_at).filter(|at| *at > now).min();
        if !whole {
            return watch_next;
        }
        let mut vet = sessions_to_vet(&mine);
        vet.extend(untabbed.iter().map(|r| r.session.clone()));
        let held = if vet.is_empty() {
            None
        } else {
            let Some(held) = holders(&vet) else {
                return watch_next;
            };
            Some(held)
        };
        let mut untabbed_next: Option<u64> = None;
        if let Some(held) = &held {
            for r in &untabbed {
                let Some(tab) = r.proven_tab(held).filter(|t| tab_is_live(tabs, t)) else {
                    continue;
                };
                if r.watch_at <= now {
                    if !self.watch.contains(&tab) {
                        self.watch.push(tab);
                        self.watch.sort();
                    }
                } else {
                    untabbed_next = Some(untabbed_next.map_or(r.watch_at, |n| n.min(r.watch_at)));
                }
            }
        }
        // A Claude Code move that failed after its exit and is held again is
        // an ordinary stopped upgrade. A tab the host runs no worker for by
        // someone else's hand (agent not in front, another supervisor's
        // claim, or the owner's Autopilot off) is out of this host's watch
        // ([`Row::aside`]), read so before its `unwatched` instant is named.
        if let Some(held) = &held {
            for r in &mut mine {
                r.held_again(held);
                r.set_aside(held, false, &self.aside, now);
            }
        }
        mine.retain(|r| {
            !r.failed_after_exit() || now.saturating_sub(r.exited_at) <= EXITED_FAILURE_SHOWN_S
        });
        let next = [next_change(&mine, now), untabbed_next]
            .into_iter()
            .flatten()
            .min();
        let mut follows: Vec<String> = Vec::new();
        if let Some(held) = &held {
            mine.retain(|r| r.standing(held, false));
            // A row no look has reached reads its holder's live status: a
            // changed status is a changed row, sent at the look taken when
            // that tab's screen moves ([`Self::follows`]).
            for r in &mut mine {
                if r.unreached_status(held, false, now) {
                    follows.push(r.tab.clone());
                }
            }
        }
        follows.sort();
        follows.dedup();
        self.follows = follows;
        // The rows are final (held again, followed): their level is read once
        // more, so what is handed over is what a surface would judge.
        for r in &mut mine {
            r.level = r.attention(now).level();
        }
        if self.sent.as_ref() != Some(&mine) {
            summary(&mine);
            self.sent = Some(mine.clone());
        }
        let want: BTreeMap<String, String> = mine
            .iter()
            .filter_map(|r| Some((r.tab.clone(), r.badge(now)?)))
            .collect();
        self.sock.clone_from(&opts.sock);
        self.reconcile(opts, tabs, &want);
        self.mark(opts, &want);
        next
    }

    /// THE MARKS THIS VIEW DID NOT RAISE (round eight, #20): [`Self::mark`]
    /// lowers only what this view raised ([`Self::badged`], empty when it
    /// begins), and an `owner=upgrade` entry can stand on a tab with no view
    /// of this process behind it — a seamless update carries every keyed
    /// owner's attention but the server's into the successor
    /// (`aterm-gui`'s `handoff_attention_owners`), and a host that ended
    /// without standing down leaves its marks. Such a mark ("upgrade stalled:
    /// …") outlived the stall it named — the move finished, or the
    /// successor's newer rules read the round as on its way — and was carried
    /// on at every later update, lowered only by a view that raised and
    /// lowered its own on that tab, or by the tab closing. So at its first
    /// whole look (and the first after it stood down), and at a tab's first
    /// look in its roster, this view lowers the `owner=upgrade` entry of
    /// every live tab it does not want marked, once — the server answers a
    /// tab with none the same — and only by that key: another owner's entry
    /// is never touched. A tab it wants marked is written by [`Self::mark`],
    /// over any carried text. A write that fails is tried again at the next
    /// look.
    fn reconcile(&mut self, opts: &Opts, tabs: &[LiveTab], want: &BTreeMap<String, String>) {
        self.settled
            .retain(|tab| tabs.iter().any(|t| t.sid == *tab));
        // A tab this view wants marked, or has marked, is [`Self::mark`]'s:
        // left unsettled, it is settled here once it is neither.
        let due: Vec<String> = tabs
            .iter()
            .map(|t| t.sid.clone())
            .filter(|tab| {
                !self.settled.contains(tab)
                    && !want.contains_key(tab)
                    && !self.badged.contains_key(tab)
            })
            .collect();
        if due.is_empty() {
            return;
        }
        let Ok(mut c) = connect(opts, "") else {
            return;
        };
        let owner = format!("owner={ATTENTION_OWNER}");
        for tab in due {
            if c.request_line(&format!("@{tab} meta unset attention {owner}"))
                .is_ok_and(|l| crate::is_ok_reply(&l))
            {
                self.settled.insert(tab);
            }
        }
    }

    /// THE TABS WHOSE WORKER THE WINDOW'S HOST PARKS BEHIND ANOTHER
    /// SUPERVISOR'S CLAIM (`aterm-gui`'s `harness_host`: a loop that found
    /// the session's claim held ends, and no worker starts for that tab until
    /// a claim is released): said before each look, which reads a move owed
    /// there as held by that claim ([`CLAIMED`], [`Row::set_aside`]) — never
    /// as a record its host stopped checking. `--status` reads the claim
    /// itself off the instance ([`roster_aside`]).
    pub fn set_claimed(&mut self, tabs: &[String]) {
        self.aside.claimed = tabs.iter().cloned().collect();
    }

    /// THE TABS WHOSE AUTOPILOT IS OFF (`aterm-gui`'s `harness_host`, the
    /// review of a295792e4): the owner took the harness off the session, so
    /// the host stops its worker and fires no watch there — said before each
    /// look, which reads a move owed there as held by the Off
    /// ([`AUTOPILOT_OFF`], [`Row::set_aside`]), never as a record its host
    /// stopped checking. `--status` reads the Autopilot itself off the
    /// instance ([`roster_aside`]).
    pub fn set_off(&mut self, tabs: &[String]) {
        self.aside.off = tabs.iter().cloned().collect();
    }

    /// THE TABS WHOSE AGENT ITS OWN SWITCH HOLDS OFF (`aterm-gui`'s
    /// `harness_host`: `[harness.<agent>] enabled = false` admits no worker
    /// there): said before each look, which reads a move owed there as held
    /// by that switch ([`AGENT_OFF`], [`Row::set_aside`]), never as a record
    /// its host stopped checking.
    pub fn set_agent_off(&mut self, tabs: &[String]) {
        self.aside.agent_off = tabs.iter().cloned().collect();
    }

    /// Tabs whose row follows its agent's live status, per the last whole look
    /// ([`Row::unreached_status`]). No instant names a status change, so the
    /// host looks again whenever one of these tabs' screens moves, never on a
    /// timer. A look that could not read whole leaves them as they were.
    #[must_use]
    pub fn follows(&self) -> &[String] {
        &self.follows
    }

    /// Tabs due a watch per the last look: a live tab with a record past its
    /// [`Row::watch_at`] (step deadline or move clock), which the host hands
    /// to `upgrade_drive::watch` whatever point the session offers.
    /// [`next_change`] names the instant; this says whose it was. Each record
    /// decides its own: a partial look still names the tabs of records it
    /// read (an untabbed record only through its proven holder); an unreadable
    /// roster names none.
    #[must_use]
    pub fn watch_due(&self) -> &[String] {
        &self.watch
    }

    /// A look of this view's host at monotonic `mono` and unix second `now`.
    /// The view marks a record `unwatched` in the same pass that first finds
    /// it due, before the watches that stamp it run, so a host start, a
    /// re-enabled `[harness]`, or a Linux resume (whose awake clock is boot
    /// alone, [`system_awake_since`]) would flash "not checked" on every owed
    /// record. So the first look since the view began or stood down
    /// ([`Self::stand_down`]), and the first after the host slept (wall clock
    /// more than [`SLEPT_S`] ahead of the monotonic one, which stops in sleep
    /// or suspend), start this view's watch, and rows read within
    /// [`WAKE_GRACE_S`] of it do not read `unwatched`. A host that watches
    /// past the grace and never stamps a record is still reported.
    pub fn note_look(&mut self, mono: std::time::Instant, now: u64) {
        let slept = self.looked.is_none_or(|(at, wall)| {
            let awake = mono.saturating_duration_since(at).as_secs();
            now.saturating_sub(wall) > awake.saturating_add(SLEPT_S)
        });
        if slept {
            self.watching_since = now;
        }
        self.looked = Some((mono, now));
    }

    /// The host stood down (the switch is off, or it is shutting down): an
    /// empty summary once, and every mark it raised lowered.
    pub fn stand_down(&mut self, summary: &mut dyn FnMut(&[Row])) {
        if self.sent.as_ref().is_some_and(|rows| !rows.is_empty()) {
            summary(&[]);
        }
        self.sent = None;
        self.follows.clear();
        self.watch.clear();
        // Stood down, nothing is watched: the first look back is the watch's
        // start again ([`Self::note_look`]).
        self.looked = None;
        self.watching_since = 0;
        self.aside = Aside::default();
        // Stood down, the next look back settles every tab again.
        self.settled.clear();
        if !self.badged.is_empty()
            && let Some(sock) = self.sock.clone()
        {
            let opts = Opts {
                home: std::path::PathBuf::new(),
                state: std::path::PathBuf::new(),
                sock: Some(sock),
                only_sid: None,
                dry_run: false,
                human_grace_s: 0,
                hand_back: true,
                background: false,
                aterm_state: None,
                ask: false,
                several_logins: false,
                usage_tap: None,
                instances: crate::harness::upgrade_drive::Instances::Machine,
                agents_off: Vec::new(),
            };
            self.mark(&opts, &BTreeMap::new());
        }
    }

    /// Brings the tabs' `owner=upgrade` attention to `want`: sets changed or
    /// new text, unsets tabs no longer in it. A failed write is retried at the
    /// next refresh.
    fn mark(&mut self, opts: &Opts, want: &BTreeMap<String, String>) {
        if *want == self.badged {
            return;
        }
        let Ok(mut c) = connect(opts, "") else {
            return;
        };
        let owner = format!("owner={ATTENTION_OWNER}");
        let gone: Vec<String> = self
            .badged
            .keys()
            .filter(|tab| !want.contains_key(*tab))
            .cloned()
            .collect();
        for tab in gone {
            if c.request_line(&format!("@{tab} meta unset attention {owner}"))
                .is_ok_and(|l| crate::is_ok_reply(&l))
                || c.request_line(&format!("@{tab} status"))
                    .is_ok_and(|l| !crate::is_ok_reply(&l))
            {
                self.badged.remove(&tab);
            }
        }
        for (tab, text) in want {
            if self.badged.get(tab) == Some(text) {
                continue;
            }
            if c.request_line(&format!("@{tab} meta set attention {owner} {text}"))
                .is_ok_and(|l| crate::is_ok_reply(&l))
            {
                self.badged.insert(tab.clone(), text.clone());
            }
        }
    }
}

/// The unix second after `now` at which one of `rows` next reads otherwise
/// by time alone — nothing else moves a row but a step, a word or a process:
/// a pending or announced upgrade turning `overdue` ([`STALLED_AFTER_S`]),
/// or `blocked` on one of the [`BLOCKERS`] ([`BLOCKED_AFTER_S`] after the
/// wait began), an upgrade still asking on its own no longer doing so
/// ([`MOVE_BUDGET_S`]), the owner's `--now` no longer quieting it
/// ([`NOW_QUIETS_S`]), a `--defer`
/// running out, a restart's carry-on no longer waited on for its model
/// ([`Row::confirm_by`]: `restarting` turns `done`), a finished move leaving
/// the summary ([`DONE_SHOWN_S`]),
/// a move that failed after its exit leaving it
/// ([`EXITED_FAILURE_SHOWN_S`]), a restart under way turning `stuck`
/// ([`STALE_S`] from its phase's start: S2 of the in-flight review), a
/// person's hold that has stood turning a record into a row, an overdue
/// stall's entry reaching the next word of its age ([`Row::behind_said`]), and —
/// for every record still owed a step — the instant the host WATCHES it
/// ([`Row::watch_at`], design record 2026-09-28, §3.2 C3): the step's
/// deadline or the move clock, reached whether or not the session offers a
/// point. One wake per deadline, never a poll: past it the row is listed
/// due ([`View::watch_due`]) and the watch stamps it, which moves the
/// instant a whole [`super::WATCH_GAP`] on. And the two tells of aterm's own
/// class that no other instant brings (the assessment of 2026-09-29): a
/// notice its own fence refused turning a row [`TELL_S`] after the first
/// refusal ([`Row::refused_notice`] — unnamed until then, its fault line came
/// at the next watch, up to a [`super::WATCH_GAP`] late: the classifier bind in
/// `upgrade_progress_tests.rs` found it), and a usage limit's named reset passing, its notice still
/// the recorded wait ([`Row::limit_passed`], [`LIMIT_GRACE_S`] after it).
/// And a record nothing has checked turning `unwatched` ([`Row::unwatched`]):
/// without it a host alive but unable to watch (every watch of the tab
/// failing to write) woke for nothing that would mark the tab.
fn next_change(rows: &[Row], now: u64) -> Option<u64> {
    rows.iter()
        .flat_map(|r| {
            let waiting = matches!(r.phase, Phase::Pending | Phase::Announced { .. });
            // A move still owed a step: waiting, or a stopped round resting.
            let owed = waiting || matches!(r.phase, Phase::Failed(_));
            let under_way = match r.phase {
                Phase::Exiting { at_s } | Phase::Relaunched { at_s } => Some(at_s),
                _ => None,
            };
            // The move clock changes what the row says ([`MOVE_BUDGET_S`]):
            // a round resting after a stop included — named only for a row
            // it changes, one still asking on its own.
            let behind = r.behind_since > 0 && r.asks_on_its_own(now);
            [
                under_way.map(|at_s| at_s.saturating_add(STALE_S + 1)),
                waiting.then(|| r.behind_since.saturating_add(STALLED_AFTER_S)),
                // A wait no rung passes turning `blocked`.
                (waiting && BLOCKERS.contains(&r.blocked.as_str()) && r.blocked_since > 0)
                    .then(|| r.blocked_since.saturating_add(BLOCKED_AFTER_S)),
                behind.then(|| r.behind_since.saturating_add(MOVE_BUDGET_S)),
                (waiting && r.request == Request::Now)
                    .then(|| r.request_at.saturating_add(NOW_QUIETS_S)),
                match r.request {
                    Request::DeferUntil(t) if waiting => Some(t),
                    _ => None,
                },
                r.restarting(now).then(|| r.confirm_by.saturating_add(1)),
                (r.phase == Phase::Done).then(|| r.finished_at().saturating_add(DONE_SHOWN_S + 1)),
                r.failed_after_exit()
                    .then(|| r.exited_at.saturating_add(EXITED_FAILURE_SHOWN_S + 1)),
                // A goal the move paused turning left paused, and a goal's
                // rest running out.
                (r.goal_held_since != 0 && r.goal_left_since == 0).then(|| {
                    r.goal_held_since
                        .saturating_add(super::super::upgrade_codex::GOAL_LEFT_AFTER_S)
                }),
                (r.goal_rest_until != 0).then_some(r.goal_rest_until),
                // A person no look has seen lately stops being named
                // ([`Row::person_named`]).
                r.person_unseen_at().filter(|_| waiting),
                // A digest line's horizon, an interrupt's bound.
                r.next_attention_at(now),
                // THE WATCH (§3.2 C3): `0` for a record owed nothing more.
                (r.watch_at != 0).then_some(r.watch_at),
                // A person's hold that has stood turns a record into a row
                // ([`Row::person_hold_stands_at`]), a re-armed round's too
                // ([`Row::rearmed_hold_stands_at`]).
                r.person_hold_stands_at(),
                // Aterm's own refused notice, told TELL_S after the first
                // refusal ([`Row::refused_notice`]).
                (owed
                    && r.refused > 0
                    && r.refused_at != 0
                    && r.wait.starts_with("announce-refused:"))
                .then(|| r.refused_at.saturating_add(TELL_S)),
                // A limit's named reset passing with its notice still the
                // wait ([`Row::limit_passed`]), after the quiet move's chance
                // ([`Row::limit_told_at`]); an unread reset is bounded
                // ([`Row::limit_ends_at`]).
                r.limit_told_at().filter(|_| owed),
                r.rearmed_hold_stands_at(),
                // An `overdue` stall's age reaching its next word
                // ([`Row::behind_said`], [`Row::age_said`]), and an
                // `unwatched` one's; no other stall's words carry an age.
                r.behind_said
                    .is_some()
                    .then(|| r.age_from(now))
                    .flatten()
                    .and_then(|since| behind_words_change_at(since, now)),
                // A record nothing has checked turning `unwatched`
                // ([`Row::unwatched`]): the instant a host whose watch cannot
                // write marks it, if no look comes first.
                r.watch_owed()
                    .then(|| unwatched_turns_at(r.watch_at, r.looked_at, r.awake_since))
                    .flatten(),
            ]
        })
        .flatten()
        .filter(|t| *t > now)
        .min()
}

/// THE PROJECTION of `HarnessUpgradeProgress` (design record 2026-09-28, §4;
/// `aterm_spec::derive::harness_upgrade_progress_model`, the `#[refines]`
/// anchors on `upgrade::next_step`, `upgrade::break_step` and the watch's
/// `watch_with`): the model's variables one upgrade record, read at `now`,
/// stands for — `phase` (0 pending, 1 announced, 2 stopped and resting, 3
/// the move under way or done), `rearmed` (a pending round re-armed after a
/// give-up, `last_stop`), `cause` (the class of the recorded wait as
/// `Row::overdue_cause` reads it: 0 its own work — the processes under it
/// outranking a passing word, a person no look has seen lately — 1 a
/// person, 2 a limit, 3 aterm's own and every other class), `clock` (the
/// round's clock run: `upgrade::REASK_S` since the pending round's progress
/// or the latest notice, the stopped round's rest over) and `reset` (a
/// recorded `limited` wait past its named reset and its grace,
/// `Row::limit_passed`). The session's posture, the work under it and its
/// READY are the look's facts, not the record's. The model says what the
/// record SAYS; what the owner is then told is `Row::attention`'s, bound over
/// the same states by `upgrade_progress_tests.rs`. Compiled only for proof
/// and test builds.
#[cfg(any(test, feature = "spec-anchors"))]
#[doc(hidden)]
#[must_use]
pub fn progress_projection(row: &Row, now: u64) -> BTreeMap<&'static str, i64> {
    let flag = |b: bool| i64::from(b);
    let phase = match &row.phase {
        Phase::Pending => 0,
        Phase::Announced { .. } => 1,
        Phase::Failed(_) => 2,
        Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => 3,
    };
    let clock = match &row.phase {
        Phase::Pending => now >= row.progress_at.saturating_add(upgrade::REASK_S),
        Phase::Announced { at_s, .. } => now >= at_s.saturating_add(upgrade::REASK_S),
        Phase::Failed(_) => now >= row.retry_at,
        Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => false,
    };
    // [`Row::overdue_cause`]'s reading, by class.
    let cause = match row.overdue_class(now) {
        WaitClass::OwnWork => 0,
        WaitClass::Person | WaitClass::GoalLeftPaused => 1,
        WaitClass::Limit => 2,
        WaitClass::SharedDaemon
        | WaitClass::Daemon
        | WaitClass::TabUnread
        | WaitClass::UnderWay
        | WaitClass::Notice
        | WaitClass::Queued
        | WaitClass::Unreached
        | WaitClass::BuildFails
        | WaitClass::NextPoint
        | WaitClass::Unrecorded
        | WaitClass::Unclassified => 3,
    };
    BTreeMap::from([
        ("phase", phase),
        (
            "rearmed",
            flag(row.phase == Phase::Pending && row.last_stop == upgrade::GAVE_UP),
        ),
        ("cause", cause),
        ("clock", flag(clock)),
        ("reset", flag(row.limit_passed(now))),
    ])
}

/// Where the owner's word is announced to the window's host: a file under
/// `<state>/upgrade/owner/` that [`ask`] replaces after every word, watched
/// by the host's activation wake (`super::upgrade_wake`), so the tab's
/// worker takes the word at the next idle point without a timer.
#[must_use]
pub fn word_marker(state: &Path) -> std::path::PathBuf {
    state.join("upgrade").join("owner").join("word")
}

/// Replaces the [`word_marker`] (new file renamed over the old, so its
/// identity changes). Best effort: an unannounced word is still written and
/// taken at the next activation notice.
fn announce_word(opts: &Opts, now: u64) {
    let path = word_marker(&opts.state);
    let Some(dir) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, format!("{now}\n")).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

// The upgrade tests drive real processes through Unix APIs (process groups, modes, inodes).
#[cfg(all(test, unix))]
#[path = "upgrade_status_tests.rs"]
mod tests;

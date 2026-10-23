// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Live upgrade status rows and operator requests backed by per-conversation state files.
//!
//! * [`rows`] and [`View`]: project one [`Row`] per upgrade record (vetted by [`Row::standing`]
//!   and [`Holder`]), raising `attention owner=upgrade` when [`Row::stall`] holds.
//! * [`ask`]: persists the operator [`Request`] under the sweep lock and wakes the window host
//!   via [`word_marker`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use aterm_json::{Map, Value};

use super::{
    LiveTab, Opts, Phase, Report, Request, STALE_S, St, Version, connect, conversation_live,
    ledger, load, now_s, process_group, roster_aside, save, session_files, started_s, state_dir,
    sweep_lock, tab_is_live, unique_tab_for_group, upgrade,
};

/// How long a session may be behind before its upgrade reads stalled on its own.
pub const STALLED_AFTER_S: u64 = upgrade::RUNG_LAND_S + LAND_GRACE_S;

/// The grace past the ladder's Land rung before an unmoved upgrade reads `overdue`.
pub const LAND_GRACE_S: u64 = 4 * 3_600;

/// The words of a row of an agent with no upgrade lane.
const NO_LANE_WORDS: &str = "no lane of aterm's moves this agent, so nothing is asked or typed";

/// The words of a row of Muse Code's or agy's where a ladder's words are owed.
const QUIET_LANE_WORDS: &str = "aterm restarts this agent onto a newer build at a genuinely \
                                idle point and types nothing into it";

const _: () = assert!(STALLED_AFTER_S == 6 * 3_600);

/// How long a finished restart stays in the host's summary.
const DONE_SHOWN_S: u64 = 10 * 60;

/// How long a move that failed after its exit (a Codex `/exit`, a Claude Code SIGTERM) stays.
pub const EXITED_FAILURE_SHOWN_S: u64 = 24 * 3_600;

/// The keyed attention owner the host raises on a stalled tab.
pub const ATTENTION_OWNER: &str = "upgrade";

/// How long the owner's `--now` keeps an overdue upgrade from reading stalled ([`Row::stall`]).
pub const NOW_QUIETS_S: u64 = upgrade::REASK_S;

/// How long aterm's own failure to type a due notice.
pub const TELL_S: u64 = 90 * 60;

/// How long past a usage limit's named reset its notice may still be the recorded wait.
pub const LIMIT_GRACE_S: u64 = 3_600;

/// The quiet move's chance past [`LIMIT_GRACE_S`].
pub const QUIET_MOVE_S: u64 = 10 * 60;

const _: () = assert!(QUIET_MOVE_S >= super::LATER[super::LATER.len() - 1].as_secs());

/// The longest a usage limit lasts (the weekly limits).
pub const LONGEST_LIMIT_S: u64 = 7 * 24 * 3_600;

/// The move clock: how long a session may be behind before an upgrade that asks again on its own.
pub const MOVE_BUDGET_S: u64 = 24 * 3_600;

const _: () = assert!(MOVE_BUDGET_S > STALLED_AFTER_S);

/// How long no look may have read a record before it reads `unwatched`.
pub const UNWATCHED_AFTER_S: u64 = 2 * super::WATCH_GAP;

/// No record reads `unwatched` within this long of the machine's last wake or boot.
pub const WAKE_GRACE_S: u64 = 10 * 60;

/// The longest the window's host sleeps on its timer before checking the wall clock.
pub const WAKE_CATCH_UP_S: u64 = WAKE_GRACE_S / 4;

const _: () = assert!(UNWATCHED_AFTER_S == 3_600 && WAKE_CATCH_UP_S < WAKE_GRACE_S);

/// When a record nothing has checked was last checked.
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

/// The first unix second at which [`unwatched_since`] reads `Some` for these fields if nothing.
fn unwatched_turns_at(watch_at: u64, looked_at: u64, awake_since: u64) -> Option<u64> {
    (watch_at != 0 && awake_since != 0).then(|| {
        watch_at
            .saturating_add(super::WATCH_GAP + 1)
            .max(looked_at.saturating_add(UNWATCHED_AFTER_S + 1))
            .max(awake_since.saturating_add(WAKE_GRACE_S))
    })
}

/// Since when the machine has been awake, unix seconds.
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
    /// The awake clock a test of this crate reads ([`awake_since`]), per thread.
    static AWAKE_PIN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Pin this thread's [`awake_since`] for a test (`0`: unknown again).
#[cfg(all(test, unix))]
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

/// One `struct timeval` sysctl by name (`kern.waketime`, `kern.boottime`), its whole seconds.
#[cfg(target_os = "macos")]
fn sysctl_secs(name: &std::ffi::CStr) -> u64 {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len: libc::size_t = std::mem::size_of::<libc::timeval>();
    // SAFETY: `name` is NUL-terminated and outlives the call.
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

/// Whether a recorded wait is one only a person can end.
fn person_holds(wait: &str) -> bool {
    let wait = wait.strip_prefix("release:").unwrap_or(wait);
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
    /// The instant an announced round's person hold ([`person_holds`]) has kept the re-ask.
    fn person_hold_stands_at(&self) -> Option<u64> {
        let Phase::Announced { at_s, .. } = self.phase else {
            return None;
        };
        person_holds(&self.wait).then(|| self.wait_since.max(at_s).saturating_add(upgrade::REASK_S))
    }
}

/// One process under the agent that held the move at the last waiting look, as shown to the owner.
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
    /// The build it runs.
    pub from: String,
    /// The target version.
    pub to: String,
    /// `managed` or `native`.
    pub source: String,
    /// Where the upgrade stands.
    pub phase: Phase,
    /// Since when the session has been behind (unix seconds).
    pub behind_since: u64,
    /// The last wait a step recorded ([`upgrade::wait_word`]), empty after an act.
    pub wait: String,
    /// When that wait began.
    pub wait_since: u64,
    /// For a person at the tab (`attended`), when a look last recorded that wait.
    pub wait_seen: u64,
    /// Whether the row's words name a person at the tab ([`Self::person_seen`]) as of the look.
    pub person_named: bool,
    /// The agent agreed to the move (Claude Code's lane, `St::ready_since`).
    pub ready: bool,
    /// The owner's word.
    pub request: Request,
    /// When the owner's word was written (unix seconds; `0`: none recorded).
    pub request_at: u64,
    /// A finished restart's outcome line ([`upgrade::restart_outcome`]).
    pub outcome: String,
    /// When the restart finished (`0`: not done).
    pub done_at: u64,
    /// When the move happened.
    pub moved_at: u64,
    /// Until when the model named by the carry-on's answer is awaited.
    pub confirm_by: u64,
    /// [`Row::stall`] at the moment the row was read.
    pub stalled: Option<String>,
    /// [`Row::attention`] at the moment the row was read ([`Level`]).
    pub level: Level,
    /// The entry's shape and age as read, carried as [`Self::stalled`].
    pub on_its_own: bool,
    pub behind_said: Option<String>,
    /// The model the announcement asked for by the model rule.
    pub model: String,
    /// Which agent the upgrade moves.
    pub agent: upgrade::Agent,
    /// When the agent the move ended was seen gone.
    pub exited_at: u64,
    /// The agent the record names (`St::pid`; `0`: none recorded).
    pub pid: u32,
    /// The last look that found a hand the relaunch line waits on standing on the tab.
    pub hand_seen: u64,
    /// What ran under the agent at the last look whose step waited ([`HeldBy`], [`St::held_by`]).
    pub held_by: Vec<HeldBy>,
    /// A round that gave up ([`upgrade::GAVE_UP`]) whose agent's own work was seen.
    pub work_seen: bool,
    /// When a stopped round starts the next one (unix seconds).
    pub retry_at: u64,
    /// Why the previous round stopped, while the re-armed round has typed no notice.
    pub last_stop: String,
    /// How many rounds in a row stopped for [`Self::streak_why`] (`St::stop_streak`).
    pub stop_streak: u32,
    pub streak_why: String,
    /// The notices the process holding the conversation has been asked.
    pub notices: u32,
    /// A given-up round holding a late ready its last look heard.
    pub late_ready: bool,
    /// The release still owed (`St::release`).
    pub release: String,
    /// When an announced round's latest notice was typed.
    pub noticed_at: u64,
    /// When this record was minted.
    pub minted_at: u64,
    /// The last look that saw the agent's own work hold the move (`St::own_work_at`; `0`: none).
    pub own_work_at: u64,
    /// A `/goal` was in force at the last look that read the screen (`St::goal`).
    pub goal: bool,
    /// The launch's relaunch line as the record planned it.
    pub line: String,
    /// The restart started its agent afresh.
    pub fresh: bool,
    /// When the record last moved (`St::progress_at`, unix seconds).
    pub progress_at: u64,
    /// When a look off a point last read the record, and the aterm build that looked.
    pub looked_at: u64,
    pub looked_by: String,
    /// The last point the session's loop offered its host.
    pub point_at: u64,
    pub guard: String,
    /// When the host watches the record.
    pub watch_at: u64,
    /// Looks in a row, since the upgrade's last act.
    pub refused: u32,
    pub refused_at: u64,
    /// The wait log (`St::waits`).
    pub waits: Vec<super::WaitNote>,
    /// The first time each distinct wait word was recorded, for the record's life (`St::firsts`).
    pub firsts: Vec<(String, u64)>,
    /// The usage limit's named reset as the transcript (a Codex row: its wall) said it at the last.
    pub limit_until: u64,
    /// The limit is money (`St::limit_spend`).
    pub limit_spend: bool,
    /// The machine's awake clock as the row was read ([`awake_since`]; `0` unknown or hand-built).
    pub awake_since: u64,
    /// The conversation runs where no aterm host watches it.
    pub outside: bool,
    /// Why no host worker runs for its tab by someone else's hand.
    pub aside: String,
    /// The flow the row is read.
    pub quiet: bool,
    /// A restart in flight whose agent was read gone, held again ([`Self::held_again`]).
    pub held_back: bool,
    /// The restart's agent no look stamped gone ([`Self::exited_at`] still `0` on its record).
    pub ended_unstamped: bool,
}

/// The notices a round record proves by itself.
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

/// When the row reads its agent gone ([`Row::exited_at`]).
fn exited_seen(st: &St, now: u64, gone: bool) -> u64 {
    match st.phase {
        Phase::Exiting { at_s } if gone && now.saturating_sub(at_s) > HUNG_EXIT_AFTER_S => {
            at_s.max(1)
        }
        _ => st.exited_at,
    }
}

/// Whether the process table says a restart's agent no look stamped is gone.
fn ended_unstamped(st: &St, now: u64) -> bool {
    matches!(st.phase, Phase::Exiting { at_s }
        if st.exited_at == 0
            && st.pid != 0
            && now.saturating_sub(at_s) > STALE_S
            && super::super::upgrade_wake::wait_exit(st.pid, std::time::Duration::ZERO))
}

/// Since when the session.
const fn read_age(st: &St) -> u64 {
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
        let ended_unstamped = ended_unstamped(st, now);
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
            exited_at: exited_seen(st, now, ended_unstamped),
            ended_unstamped,
            pid: st.pid,
            hand_seen: st.hand_seen_s,
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
            late_ready: reads_ready_clock(st.agent)
                && matches!(&st.phase, Phase::Failed(why) if why == upgrade::GAVE_UP)
                && st.ready_since != 0,
            release: if types_release(st.agent) {
                st.release.clone()
            } else {
                String::new()
            },
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
            held_back: false,
        };
        row.read_at(now);
        row.person_named = row.person_seen(now);
        row
    }

    /// This row read under the flow `opts` names.
    fn in_flow(mut self, opts: &Opts, now: u64) -> Row {
        self.quiet = !opts.ask;
        self.read_at(now);
        self
    }

    /// Judges this row again at `now` when served later than it was read.
    pub fn reread(&mut self, now: u64) {
        self.awake_since = awake_since().max(self.awake_since);
        self.read_at(now);
    }

    /// What the row says at `now` that time alone moves, as read.
    pub(super) fn read_at(&mut self, now: u64) {
        self.stalled = self.stall(now);
        self.on_its_own = self.asks_on_its_own(now);
        self.behind_said = self.age_said(now);
        self.level = self.attention(now).level();
    }

    /// This row read by a host whose watch started or resumed after a sleep at `since`.
    fn host_watching_since(&mut self, since: u64, now: u64) {
        if self.awake_since != 0 && since > self.awake_since {
            self.awake_since = since;
            self.read_at(now);
        }
    }

    /// What held the move at the last waiting look ([`Self::held_by`]).
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

    /// [`Self::held_words`] as one `--status` value.
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

    /// Seconds until this stopped round's next one ([`Self::retry_at`]).
    #[must_use]
    pub fn next_round_in(&self, now: u64) -> Option<u64> {
        if !matches!(self.phase, Phase::Failed(_)) || self.owner_holds(now) || self.late_ready {
            return None;
        }
        Some(self.retry_at.saturating_sub(now))
    }

    /// When the upgrade reaches the ladder's Land rung (unix seconds).
    #[must_use]
    pub fn lands_by(&self) -> Option<u64> {
        (self.behind_since > 0).then(|| self.behind_since.saturating_add(upgrade::RUNG_LAND_S))
    }

    /// The ladder's rung at `now`.
    #[must_use]
    pub fn rung(&self, now: u64) -> upgrade::Rung {
        match self.behind_since {
            0 => upgrade::Rung::Prefer,
            since => upgrade::rung(now.saturating_sub(since)),
        }
    }

    /// A restart whose carry-on is typed.
    fn restarting(&self, now: u64) -> bool {
        self.phase == Phase::Done && self.done_at == 0 && now <= self.confirm_by
    }

    /// When the move finished for the owner.
    const fn finished_at(&self) -> u64 {
        if self.done_at == 0 {
            self.confirm_by
        } else {
            self.done_at
        }
    }

    /// Whether the owner's word is in force at `now`.
    #[must_use]
    pub fn word_in_force(&self, now: u64) -> bool {
        self.owner_holds(now) || self.hurried(now)
    }

    /// Whether the owner's word holds this upgrade.
    fn owner_holds(&self, now: u64) -> bool {
        match &self.request {
            Request::Skip(v) => upgrade::Version::parse(v) == upgrade::Version::parse(&self.to),
            Request::DeferUntil(t) => now < *t,
            Request::None | Request::Now => false,
        }
    }

    /// Whether the upgrade will move this session to the target at a turn end with nobody's help.
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

    /// Why this upgrade will not move on its own, as one word.
    #[must_use]
    pub fn stall(&self, now: u64) -> Option<String> {
        if self.owner_holds(now) {
            return None;
        }
        if self.unwatched(now).is_some() {
            return Some("unwatched".to_string());
        }
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
                } else if self.repeating_stop() {
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

    /// A restart under way that has not moved ([`stuck_on`]).
    fn stuck(&self, now: u64) -> Option<&'static str> {
        let seen = if self.ended_unstamped {
            self.exited_at.max(1)
        } else {
            self.exited_at
        };
        stuck_on(&self.phase, seen, now)
    }

    /// How long this session has been behind at `now`.
    #[must_use]
    pub fn behind_for(&self, now: u64) -> Option<String> {
        behind_words(self.behind_since, now)
    }

    /// The age this entry says at `now`.
    #[must_use]
    pub fn age_said(&self, now: u64) -> Option<String> {
        self.age_from(now)
            .and_then(|since| behind_words(since, now))
    }

    /// The instant [`Self::age_said`] counts.
    fn age_from(&self, now: u64) -> Option<u64> {
        match self.stall(now).as_deref() {
            Some("overdue") => Some(self.behind_since),
            Some("unwatched") => self.unwatched(now),
            _ => None,
        }
    }

    /// Aterm's own failure to type a due notice, standing past [`TELL_S`].
    #[must_use]
    pub fn refused_notice(&self, now: u64) -> bool {
        self.wait.starts_with("announce-refused:")
            && self.refused > 0
            && self.refused_at != 0
            && now.saturating_sub(self.refused_at) >= TELL_S
    }

    /// Whether the notice was refused for a state.
    fn refusal_stands(&self) -> bool {
        matches!(
            self.wait.as_str(),
            "announce-refused:draft" | "announce-refused:held" | "announce-refused:box"
        )
    }

    /// A usage limit past its end, its notice still the recorded wait.
    #[must_use]
    pub fn limit_passed(&self, now: u64) -> bool {
        self.limit_told_at().is_some_and(|at| now >= at)
    }

    /// The instant a recorded `limited` wait is told ([`Self::limit_passed`], for [`next_change`]).
    fn limit_told_at(&self) -> Option<u64> {
        let end = self.limit_ends_at()?;
        let moves = if self.quiet && self.limit_until != 0 {
            QUIET_MOVE_S
        } else {
            0
        };
        Some(end.saturating_add(LIMIT_GRACE_S).saturating_add(moves))
    }

    /// When a recorded `limited` wait is known to have ended.
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

    /// A spend or credit wall holds the move with no reset read ([`Self::limit_spend`]).
    #[must_use]
    pub fn spend_held(&self) -> bool {
        self.wait == "limited" && self.limit_spend && self.limit_until == 0
    }

    /// Since when no look has read a record the move is still owed on ([`unwatched_since`]).
    #[must_use]
    pub fn unwatched(&self, now: u64) -> Option<u64> {
        if !self.watch_owed() {
            return None;
        }
        unwatched_since(self.watch_at, self.looked_at, now, self.awake_since)
    }

    /// Whether a watch is for this record at all ([`Self::unwatched`]).
    const fn watch_owed(&self) -> bool {
        matches!(
            self.phase,
            Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
        ) && !self.failed_after_exit()
            && !self.outside
            && self.aside.is_empty()
    }

    /// Whether the due notice is one aterm's own fence refuses (`announce-refused:<why>`).
    #[must_use]
    pub fn fence_fails_under_now(&self) -> bool {
        self.wait.starts_with("announce-refused:") && self.request == Request::Now
    }

    /// Whether the owner's `--now` still quiets an overdue stall at `now`.
    fn hurried(&self, now: u64) -> bool {
        self.request == Request::Now && now < self.request_at.saturating_add(NOW_QUIETS_S)
    }

    /// What the stall means in a person's words ([`Self::stall`]).
    #[must_use]
    pub fn stall_words(&self, now: u64) -> Option<String> {
        let stall = self.stall(now)?;
        let behind = behind_words(self.behind_since, now).map(|b| format!("behind for {b}"));
        let with = |what: &str| match &behind {
            Some(b) => format!("{b}: {what}"),
            None => what.to_string(),
        };
        Some(match stall.as_str() {
            "overdue" if self.limit_passed(now) => {
                if self.limit_until != 0 {
                    let ago = behind_words(self.limit_until, now)
                        .map_or_else(String::new, |span| format!(" {span} ago"));
                    with(&format!(
                        "its usage limit's reset passed{ago}, and the last look still read the \
                         limit"
                    ))
                } else {
                    // time (stall words stay within 64 characters).
                    with("its limit has outlasted any usage limit")
                }
            }
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

    /// Whether the upgrade is still asking on its own, so the window records it and marks nothing.
    #[must_use]
    pub fn asks_on_its_own(&self, now: u64) -> bool {
        !self.past_move_budget(now) && self.will_ask_again(now)
    }

    /// Whether the upgrade will ask again by itself, at any age.
    #[must_use]
    pub fn will_ask_again(&self, now: u64) -> bool {
        if self.limit_passed(now) || self.unwatched(now).is_some() {
            return false;
        }
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
    fn rest_carries_on(&self, now: u64) -> bool {
        match self.wait.as_str() {
            "" | "attended" | upgrade::NEXT_POINT => true,
            _ => self.rearmed_hold_stands_at().is_some_and(|at| now < at),
        }
    }

    /// The instant a re-armed round's person hold ([`person_holds`]) has stood a whole.
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

    /// Whether the session has been behind [`MOVE_BUDGET_S`] or more at `now` and waits.
    fn past_move_budget(&self, now: u64) -> bool {
        self.behind_since > 0
            && now.saturating_sub(self.behind_since) >= MOVE_BUDGET_S
            && (self.wait != "limited" || self.spend_held())
    }

    /// Whether the latest stop repeated.
    #[must_use]
    pub fn repeating_stop(&self) -> bool {
        self.stop_streak >= 2
            && !self.streak_why.is_empty()
            && self.streak_why != upgrade::GAVE_UP
            && !refusal(&self.streak_why)
    }

    /// What a wait stands for, in a person's words.
    fn wait_words(&self, now: u64) -> Option<&'static str> {
        if self.person_gone(now) {
            return Some("it waits for its next turn end");
        }
        self.recorded_wait_words()
    }

    /// Whether this row's wait is a Claude Code tab's `attended` person no look has seen.
    fn person_gone(&self, now: u64) -> bool {
        attended_is_person(self.agent) && self.wait == "attended" && self.attended_unseen(now)
    }

    /// [`Self::wait_words`] as the look that recorded the wait said it, with no clock.
    fn recorded_wait_words(&self) -> Option<&'static str> {
        if self.wait.is_empty() {
            return None;
        }
        if self.spend_held() {
            return Some(SPEND_WORDS);
        }
        let gate_spent = match self.agent {
            upgrade::Agent::Claude => self.wait.starts_with("release:") && self.release.is_empty(),
            upgrade::Agent::Codex => false,
            upgrade::Agent::Muse | upgrade::Agent::Agy | upgrade::Agent::Gemini => return None,
        };
        if gate_spent {
            return None;
        }
        claude_wait_words(&self.wait)
    }

    /// Whether only the ladder's own comfort holds this wait.
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

    /// Whether the owner's `--now` moves this wait once it is overdue.
    fn now_moves(&self) -> bool {
        now_moves_past(&self.wait) || (self.quiet && self.wait.starts_with("status-stale"))
    }

    /// The waiting record's sentence, worded by what holds the move.
    #[must_use]
    pub fn waiting_line(&self, from: &str) -> String {
        if self.waits_for_comfort() {
            return format!(
                "nothing to do: it starts on its own at a quiet moment in the tab, and from \
                 {from} as soon as it is idle and nobody has typed there for {} seconds",
                upgrade::KEYS_GAP_S
            );
        }
        let when = match self.wait.as_str() {
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
            w if w.starts_with("status-stale") && self.quiet => {
                "it moves once Claude Code says it is idle again, or at once on the owner's \
                 word to upgrade now"
            }
            w if w.starts_with("announce-refused:cut-") => {
                "the part it left in its prompt waits to be sent or cleared there"
            }
            "left-typed:mixed" => "it moves once that draft is sent or cleared",
            "left-typed:screen-unreadable" => "it moves once aterm can read the tab again",
            w if w.starts_with("left-typed:") => match self.agent {
                upgrade::Agent::Claude | upgrade::Agent::Codex => {
                    "it moves once they are sent or cleared in its tab"
                }
                upgrade::Agent::Muse | upgrade::Agent::Agy => QUIET_LANE_WORDS,
                upgrade::Agent::Gemini => NO_LANE_WORDS,
            },
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

    /// Whether no look has seen the wait lately ([`ATTENDED_SEEN_S`]).
    fn attended_unseen(&self, now: u64) -> bool {
        now.saturating_sub(self.wait_seen.max(self.wait_since)) > ATTENDED_SEEN_S
    }

    /// Whether the row's words name a person at the tab at `now` ([`Self::wait_words`]).
    #[must_use]
    pub fn person_seen(&self, now: u64) -> bool {
        attended_is_person(self.agent) && self.wait == "attended" && !self.attended_unseen(now)
    }

    /// The unix second this row's words stop naming a person at the tab ([`Self::person_seen`]).
    fn person_unseen_at(&self) -> Option<u64> {
        (attended_is_person(self.agent) && self.wait == "attended").then(|| {
            self.wait_seen
                .max(self.wait_since)
                .saturating_add(ATTENDED_SEEN_S + 1)
        })
    }

    /// The move waits only on the person at its tab.
    #[must_use]
    pub fn pause_note(&self, now: u64) -> Option<crate::harness::footer::UpgradeNote> {
        use crate::harness::footer::{UpgradeNote, UpgradeWords};
        let owed = matches!(self.phase, Phase::Pending | Phase::Announced { .. });
        let footed = match self.agent {
            upgrade::Agent::Claude => true,
            upgrade::Agent::Codex
            | upgrade::Agent::Muse
            | upgrade::Agent::Agy
            | upgrade::Agent::Gemini => false,
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

    /// What the owner can do about this stall, by its kind ([`Remedy`]).
    #[must_use]
    pub fn remedy(&self, now: u64) -> Option<Remedy> {
        let stall = self.stall(now);
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
            "overdue" if self.fence_fails_under_now() => Remedy::Waits,
            "overdue" if self.now_moves() => Remedy::Now,
            "overdue" => Remedy::Waits,
            s if s.starts_with("held-back:") => Remedy::InItsPane,
            s if s.starts_with("stuck:") => Remedy::Waits,
            _ if self.failed_after_exit() => Remedy::ResumeInTab,
            _ => Remedy::ByHand,
        })
    }

    /// A round the quiet flow gave up on its agent's own work.
    #[must_use]
    pub fn rests_on_own_work(&self) -> bool {
        let rearms_at_work_end = match self.agent {
            upgrade::Agent::Claude => true,
            upgrade::Agent::Codex
            | upgrade::Agent::Muse
            | upgrade::Agent::Agy
            | upgrade::Agent::Gemini => false,
        };
        self.quiet
            && rearms_at_work_end
            && matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP)
            && (!self.held_by.is_empty() || (self.work_seen && !self.now_moves()))
    }

    /// Whether this row's phase is one a live process must still hold for it to mean anything.
    const fn held_by_a_process(&self) -> bool {
        matches!(
            self.phase,
            Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
        ) && !self.failed_after_exit()
    }

    /// A move that failed after its agent was seen gone.
    #[must_use]
    pub const fn failed_after_exit(&self) -> bool {
        self.exited_at != 0 && matches!(self.phase, Phase::Failed(_))
    }

    /// A restart in flight whose agent was read gone ([`Self::exited_at`]) and is not yet held.
    const fn down_in_flight(&self) -> bool {
        self.exited_at != 0 && !self.held_back && matches!(self.phase, Phase::Exiting { .. })
    }

    /// Down after its agent's exit.
    const fn down_after_exit(&self) -> bool {
        self.failed_after_exit() || self.down_in_flight()
    }

    /// This row's down answered ([`Self::held_again`], [`View::back`]).
    const fn answer_down(&mut self) {
        if matches!(self.phase, Phase::Exiting { .. }) {
            self.held_back = true;
        } else {
            self.exited_at = 0;
        }
    }

    /// A Claude Code move that failed after its exit, held again.
    fn held_again(&mut self, holders: &[Holder], unproven_tab: bool) {
        let in_front = |h: &Holder| !h.aside && (h.led || unproven_tab);
        let leads_its_tab = |h: &Holder| {
            self.answered_by_its_tabs_agent()
                && h.tab.as_deref() == Some(self.tab.as_str())
                && in_front(h)
        };
        let answers = |h: &Holder| {
            self.shell_gone_afresh()
                && h.answers.as_deref() == Some(self.session.as_str())
                && in_front(h)
        };
        let ended = |h: &Holder| self.pid != 0 && h.pid == self.pid;
        let back = held_again_after_exit(self.agent)
            && self.down_after_exit()
            && holders.iter().any(|h| {
                !ended(h) && (h.session == self.session || leads_its_tab(h) || answers(h))
            });
        if back {
            self.answer_down();
        }
    }

    /// A fresh restart's Claude Code move that failed after its exit because the shell it ran.
    fn shell_gone_afresh(&self) -> bool {
        self.answered_by_its_tabs_agent()
            && matches!(&self.phase, Phase::Failed(why) if why == "shell-gone")
    }

    /// A fresh restart's Claude Code move down after its exit.
    fn answered_by_its_tabs_agent(&self) -> bool {
        self.fresh
            && held_again_after_exit(self.agent)
            && self.down_after_exit()
            && !self.tab.is_empty()
    }

    /// Whether the owner sees or counts this upgrade now.
    #[must_use]
    pub(super) fn standing(&self, holders: &[Holder], unproven_tab: bool) -> bool {
        if !self.held_by_a_process() && !self.held_back {
            return true;
        }
        self.behind_holders(holders)
            .any(|h| self.held_here(h, unproven_tab))
    }

    /// Whether live holder `h` holds this row's conversation in its tab.
    fn held_here(&self, h: &Holder, unproven_tab: bool) -> bool {
        h.tab
            .as_deref()
            .map_or(unproven_tab, |t| self.tab.is_empty() || t == self.tab)
    }

    /// Whether at least one.
    fn held_aside(&self, holders: &[Holder], unproven_tab: bool) -> bool {
        let mut here = self
            .behind_holders(holders)
            .filter(|h| self.held_here(h, unproven_tab))
            .peekable();
        here.peek().is_some() && here.all(|h| h.aside)
    }

    /// Which of one tab's rows speaks FOR the tab ([`tab_rows`], lc-9).
    fn tab_rank(&self) -> (bool, bool, bool, bool, u64, u64, &str) {
        (
            self.aside != NOT_FOREGROUND,
            matches!(self.phase, Phase::Exiting { .. } | Phase::Relaunched { .. }),
            held_again_after_exit(self.agent) && self.failed_after_exit(),
            self.held_by_a_process(),
            self.minted_at,
            self.progress_at,
            self.session.as_str(),
        )
    }

    /// This row set aside by `why`.
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

    /// Sets this row aside if any hand but a host's holds its tab.
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

    /// Keeps the aside of the row `shown` on this one, read at `now`.
    pub fn keep_aside(&mut self, shown: &Row, now: u64) {
        if !shown.aside.is_empty() && shown.session == self.session {
            self.go_aside(&shown.aside, now);
        }
    }

    /// Lets a row no look has reached read Claude's live status.
    fn unreached_status(&mut self, holders: &[Holder], unproven_tab: bool, now: u64) -> bool {
        let has_status = match self.agent {
            upgrade::Agent::Claude => true,
            upgrade::Agent::Codex
            | upgrade::Agent::Muse
            | upgrade::Agent::Agy
            | upgrade::Agent::Gemini => false,
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

    /// The tab a behind live holder of this conversation is proven to run.
    pub(super) fn proven_tab(&self, holders: &[Holder]) -> Option<String> {
        self.behind_holders(holders).find_map(|h| h.tab.clone())
    }

    /// The live holders of this conversation on a build older than its target.
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

    /// The one word a roster carries for this upgrade, `<state>/<to>/<why>/<age>`.
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
            _ if matches!(self.phase, Phase::Failed(_)) && self.asks_on_its_own(now) => {
                ("pending", self.next_round_word(now), behind)
            }
            _ => match self.stall(now) {
                Some(stall) if stall == "overdue" && !self.wait.is_empty() => {
                    ("stalled", format!("overdue:{}", self.wait), behind)
                }
                Some(stall) if stall == "unwatched" => (
                    "stalled",
                    format!(
                        "unwatched:{}",
                        upgrade::span(now.saturating_sub(self.unwatched(now).unwrap_or(now)))
                    ),
                    behind,
                ),
                Some(stall) => ("stalled", stall, behind),
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

    /// `next-round:<span>` until a stopped round's next one.
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

    /// The ladder's rung as `--status`'s `rung=`.
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
        let refused = if self.refused == 0 {
            String::new()
        } else {
            format!(
                " refused={}/{}",
                self.refused,
                upgrade::span(now.saturating_sub(self.refused_at))
            )
        };
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
            ("release", self.release.clone()),
            ("rung", self.rung_word(now)),
            ("looked_by", self.looked_by.clone()),
            ("guard", self.guard.clone()),
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
            ("retry_at", self.retry_at),
            ("next_round_s", self.next_round_in(now).unwrap_or(0)),
            ("progress_at", self.progress_at),
            ("looked_at", self.looked_at),
            ("point_at", self.point_at),
            ("watch_at", self.watch_at),
            ("refused", u64::from(self.refused)),
            ("refused_at", self.refused_at),
            ("limit_until", self.limit_until),
        ] {
            o.insert(k.into(), Value::from(v));
        }
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
        let (class, kind) = match self.attention(now) {
            Attention::Quiet => ("quiet", ""),
            Attention::Fault(n) => ("fault", n.kind),
            Attention::Digest(n) => ("digest", n.kind),
            Attention::Interrupt(n) => ("interrupt", n.kind),
        };
        o.insert("attention".into(), Value::from(class));
        o.insert("attention_kind".into(), Value::from(kind));
        o.insert("cause_words".into(), Value::from(self.overdue_cause(now)));
        Value::Object(o)
    }

    /// What the upgrade moves the session onto, in the owner's words.
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

    /// `--status`'s sentence under [`Self::line`].
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

    /// The tab's attention text (at most [`MARK_MAX`] bytes, one line: the keyed attention cap).
    fn badge(&self, now: u64) -> Option<String> {
        let Attention::Interrupt(need) = self.attention(now) else {
            return None;
        };
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

    /// What holds an overdue move, as a class in the owner's words.
    fn overdue_cause(&self, now: u64) -> String {
        // as what the record read.
        if self.limit_passed(now) {
            return LIMIT_PASSED.to_string();
        }
        if self.spend_held() {
            return SPEND_HELD.to_string();
        }
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

    /// What holds the move, by class ([`Cause`]).
    fn cause(&self) -> Cause {
        Self::cause_of(&self.wait, !self.held_by.is_empty())
    }

    /// [`Self::cause`] for a recorded `wait`, with processes under the agent (`held`) or none.
    fn cause_of(wait: &str, held: bool) -> Cause {
        match Self::class_of(wait, held) {
            WaitClass::OwnWork => Cause::OwnWork,
            WaitClass::Person => Cause::Person,
            WaitClass::Limit => Cause::Limit,
            WaitClass::TabUnread => Cause::TabUnread,
            WaitClass::UnderWay => Cause::UnderWay,
            WaitClass::Notice => Cause::CouldNotType,
            WaitClass::Queued => Cause::Queued,
            WaitClass::Unreached => Cause::Unreached,
            WaitClass::BuildFails => Cause::BuildFails,
            WaitClass::NextPoint => Cause::NextPoint,
            WaitClass::Unrecorded => Cause::NoLook,
            WaitClass::Unclassified => {
                let wait = wait.strip_prefix("release:").unwrap_or(wait);
                Cause::Unclassified(wait.to_string())
            }
        }
    }

    /// The class of a recorded `wait`, with processes under the agent (`held`) or none.
    fn class_of(wait: &str, held: bool) -> WaitClass {
        if held && Self::passes_under_work(wait) {
            WaitClass::OwnWork
        } else if Self::stands(wait) {
            WaitClass::Person
        } else {
            wait_class(wait)
        }
    }

    /// A person's or a peer's state that outlasts a look.
    fn stands(wait: &str) -> bool {
        let wait = wait.strip_prefix("release:").unwrap_or(wait);
        matches!(
            wait,
            "announce-refused:draft" | "announce-refused:held" | "announce-refused:box"
        )
    }

    /// The notice path's passing gates and a resting round's quiet word.
    fn passes_under_work(wait: &str) -> bool {
        let wait = wait.strip_prefix("release:").unwrap_or(wait);
        let head = wait.split_once(':').map_or(wait, |(head, _)| head);
        !Self::stands(wait) && matches!(head, "" | "attended" | "announce-refused" | "failed")
    }

    /// The class [`Self::overdue_cause`] words.
    fn overdue_class(&self, now: u64) -> WaitClass {
        let held = !self.held_by.is_empty();
        if self.person_gone(now) || (held && Self::passes_under_work(&self.wait)) {
            WaitClass::OwnWork
        } else if self.spend_held() {
            // Money no reset ends: the owner's hold ([`Self::spend_held`]).
            WaitClass::Person
        } else {
            Self::class_of(&self.wait, held)
        }
    }

    /// What holds the move, as one word (`--status` `cause=`, `--json` `cause`).
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

    /// What the owner is told, as one word (`--status`'s `told=`, `--json`'s `told`).
    fn told_word(&self, now: u64) -> String {
        match self.attention(now) {
            Attention::Quiet => "-".to_string(),
            Attention::Fault(n) | Attention::Digest(n) | Attention::Interrupt(n) => {
                n.kind.to_string()
            }
        }
    }
}

/// What holds a wait, by class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitClass {
    /// The agent's own work.
    OwnWork,
    /// A person: a box, a draft, a login, a person at the tab, a hold.
    Person,
    /// A usage limit, until its reset.
    Limit,
    /// A tab, screen, shell integration or process aterm cannot read.
    TabUnread,
    /// A move already under way.
    UnderWay,
    /// A notice aterm could not type.
    Notice,
    /// Notices queued unread.
    Queued,
    /// A tab or conversation aterm cannot reach to ask.
    Unreached,
    /// The build it would move to failed to start, and is not proved to start.
    BuildFails,
    /// Nothing holds it now ([`upgrade::NEXT_POINT`]).
    NextPoint,
    /// No look has recorded a wait.
    Unrecorded,
    /// A wait this build does not classify.
    Unclassified,
}

impl WaitClass {
    /// The class as `--status` says it (`cause=`, [`Row::cause_word`]).
    const fn word(self) -> &'static str {
        match self {
            WaitClass::OwnWork => "own-work",
            WaitClass::Person => "person",
            WaitClass::Limit => "limit",
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

/// The class of `wait` ([`WaitClass`]).
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
        (_, "left-typed:screen-unreadable") => WaitClass::TabUnread,
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

/// The waiting record's sentence for a wait with no words of its own ([`Row::waiting_line`]).
fn unworded_waiting_line(wait: &str) -> &'static str {
    match wait {
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
            WaitClass::NextPoint => NEXT_POINT_CAUSE,
            WaitClass::Unrecorded | WaitClass::Unclassified => {
                "aterm is waiting on a step of its own; it moves on its own once that is over \
                 (`aterm harness upgrade --status` names the step)"
            }
        },
    }
}

/// How long the running process.
pub const DIGEST_AFTER_S: u64 = MOVE_BUDGET_S;

const _: () = assert!(
    BACKSTOP_S == MOVE_BUDGET_S,
    "one day: the digest's, the backstop's and the move clock"
);

/// How long an agent suspended (`^Z`) or put in the background may stand before the digest says.
pub const SUSPENDED_AFTER_S: u64 = 3_600;

/// How long a wait that is aterm's own fault.
pub const HARNESS_TELL_S: u64 = TELL_S;

/// How long a SIGTERMed agent may still be alive before the owner is interrupted.
pub const HUNG_EXIT_AFTER_S: u64 = 3 * STALE_S;

/// How long the agent's own work is remembered as what holds a move after the last look that saw.
pub const OWN_WORK_HOLD_S: u64 = 2 * upgrade::REASK_S;

/// The longest a record may go without progress (a notice typed, a round begun, a restart).
pub const BACKSTOP_S: u64 = DIGEST_AFTER_S;

/// The longest a usage limit stands.
pub const LIMIT_MAX_S: u64 = 8 * 86_400;

/// The longest notices may wait unread behind a full queue.
pub const QUEUED_MAX_S: u64 = 3 * 86_400;

/// How long a relaunched agent may go without reaching an idle point before the log says.
pub const RELAUNCH_STUCK_S: u64 = 3 * STALE_S;

/// How long a relaunched agent may go without reaching an idle point before the owner.
pub const RELAUNCH_INTERRUPT_S: u64 = 2 * RELAUNCH_STUCK_S;

/// Whether the agent's own work holds a move for a look that recorded `wait` with (`held`).
pub(super) fn holds_by_own_work(wait: &str, held: bool) -> bool {
    Row::cause_of(wait, held) == Cause::OwnWork
}

/// How loud one upgrade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attention {
    /// Nothing for the glass: the upgrade is working.
    Quiet,
    /// Aterm's own fault, which the owner cannot act.
    Fault(Need),
    /// The owner should know, only the owner can end it, and nothing is lost by waiting a day.
    Digest(Need),
    /// The owner is needed now and the action is theirs alone.
    Interrupt(Need),
}

impl Attention {
    /// This attention as a row carries it ([`Level`]).
    #[must_use]
    pub const fn level(&self) -> Level {
        match self {
            Attention::Quiet => Level::Quiet,
            Attention::Fault(n) => Level::Fault(n.kind),
            Attention::Digest(n) => Level::Digest(n.kind),
            Attention::Interrupt(n) => Level::Interrupt(n.kind),
        }
    }
}

/// [`Attention`] as a row carries it.
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

/// Why the owner is needed and the one action, as data the exceptions surface renders and files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Need {
    /// The kind, a stable slug (`agent-down`, `no-terminal`, `own-work`…).
    pub kind: &'static str,
    /// The once-only key: the kind and the tab (for `build-fails`, the build).
    pub fingerprint: String,
    /// How much it weighs.
    pub severity: NeedSeverity,
    /// A glass title without its place (the host adds `in tab 1`).
    pub title: &'static str,
    /// Why the owner is needed.
    pub why: String,
    /// The one thing to do, in the imperative.
    pub then: String,
    /// The exact command the owner runs for it, when it is one (`claude --resume <conversation>`).
    pub command: Option<String>,
    /// When the condition began (unix seconds).
    pub since: u64,
    /// The one line the log keeps when the item is dismissed or resolved.
    pub log_line: String,
}

/// The longest [`Need::why`], in characters.
pub const NEED_WHY_MAX: usize = 90;
/// The longest [`Need::then`], in characters (the engine's `aterm_messages::need::STEP_CAP`).
pub const NEED_THEN_MAX: usize = 90;

/// The waits whose truth, past a horizon, is news to the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Special {
    /// Text a person typed into the composer and did not send.
    Draft,
    /// The agent is suspended or in the background of its shell.
    Foreground,
}

/// The resume line a person is told for `session`.
fn resume_command(line: &str, session: &str) -> Option<String> {
    let launch = launch_of(line)?;
    let told = crate::harness::upgrade_drive::resume_command_kept(None, &launch, session)?;
    let exports = home_exports(line);
    let env: Vec<(&str, &str)> = exports.iter().map(|(n, v)| (*n, v.as_str())).collect();
    crate::harness::resume::with_env(&env, &told)
}

/// The claude HOME the recorded relaunch line carries.
fn home_exports(line: &str) -> Vec<(&'static str, String)> {
    let last = last_command(line);
    let exports: Vec<(&str, &str)> = last[..program_at(&last)]
        .iter()
        .filter_map(|word| export_of(word))
        .collect();
    [
        crate::harness::accounts::HOME_ENV,
        crate::harness::accounts::HOMEDIR_ENV,
    ]
    .into_iter()
    .filter_map(|name| {
        exports
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, value)| (name, (*value).to_string()))
    })
    .collect()
}

/// The launch a relaunch line planned, as its argv.
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

/// Whether `word` is one of the relaunch line's `NAME=VALUE` exports, and then its name and value.
fn export_of(word: &str) -> Option<(&str, &str)> {
    word.split_once('=').filter(|(name, _)| {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    })
}

/// Where the program stands among `last` ([`last_command`]).
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
    at.min(last.len())
}

/// The login the recorded relaunch line carries.
fn resume_login(line: &str) -> Option<String> {
    let last = last_command(line);
    last[..program_at(&last)]
        .iter()
        .filter_map(|word| export_of(word))
        .find(|(name, _)| *name == crate::harness::accounts::LOGIN_ENV)
        .map(|(_, value)| value.to_string())
}

/// The shell words of the last command of a relaunch line, or of a resume line.
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

/// The most bytes of launch flags a manual resume command carries whole ([`Row::resume_said`]).
const RESUME_FLAGS_MAX: usize = 80;

/// The tab's mark, in bytes.
const MARK_MAX: usize = 200;

/// [`Special`] for a recorded wait word, if it is one.
fn special(wait: &str) -> Option<Special> {
    match wait {
        "draft" | "announce-refused:draft" => Some(Special::Draft),
        "not-foreground" => Some(Special::Foreground),
        _ => None,
    }
}

/// What holds a move, by class ([`Row::cause`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Cause {
    /// The agent's own work: a turn, a shell, a workflow, a status not yet quiet.
    OwnWork,
    /// A person: a box, a draft, a login, someone at the tab, a hold.
    Person,
    /// A usage limit, which ends by itself at its reset.
    Limit,
    /// Aterm's own notice could not be typed.
    CouldNotType,
    /// Notices wait unread behind the conversation's queue.
    Queued,
    /// A tab, screen, shell integration or process aterm cannot read.
    TabUnread,
    /// A move already under way.
    UnderWay,
    /// A tab or conversation aterm cannot reach to ask.
    Unreached,
    /// The build it would move to fails to start.
    BuildFails,
    /// Nothing holds it now: the last look found no gate, and its next step goes at the next point.
    NextPoint,
    /// No look has recorded what it waits.
    NoLook,
    /// A wait this build does not classify, named so it fails loud.
    Unclassified(String),
}

impl Row {
    /// How loud this upgrade is ([`Attention`]).
    #[must_use]
    pub fn attention(&self, now: u64) -> Attention {
        if self.owner_holds(now) {
            return Attention::Quiet;
        }
        if let Some(since) = self.unwatched(now) {
            return Attention::Fault(self.unwatched_need(since));
        }
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
            Phase::Exiting { .. } if self.held_back => Attention::Quiet,
            Phase::Exiting { at_s } if self.exited_at != 0 => {
                let clock =
                    super::super::relaunch::exit_clock(*at_s, self.exited_at, self.hand_seen);
                if now.saturating_sub(clock) > STALE_S {
                    Attention::Interrupt(self.agent_down("exited"))
                } else {
                    Attention::Quiet
                }
            }
            Phase::Exiting { at_s } => {
                if now.saturating_sub(*at_s) > HUNG_EXIT_AFTER_S {
                    Attention::Interrupt(self.hung_exit(*at_s))
                } else {
                    Attention::Quiet
                }
            }
            Phase::Relaunched { at_s } => {
                let waited = now.saturating_sub(*at_s);
                if waited >= RELAUNCH_INTERRUPT_S {
                    // ([`Self::agent_down`]).
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
                    // it.
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
                // one record per build however many agents wait on it.
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
                if self.phase == Phase::Pending
                    && self.wait.is_empty()
                    && self.minted_at != 0
                    && now.saturating_sub(self.minted_at) >= HARNESS_TELL_S
                {
                    return Attention::Fault(self.no_look(now.saturating_sub(self.minted_at)));
                }
                if self.refused_notice(now) && !self.refusal_stands() {
                    return Attention::Fault(
                        self.harness_stuck(now.saturating_sub(self.refused_at)),
                    );
                }
                let waited = now.saturating_sub(self.wait_since);
                if self.wait_since != 0 && special(&self.wait).is_none() {
                    let counted =
                        self.wait.starts_with("announce-refused:") && self.refused_at != 0;
                    if !counted && self.transient_bound().is_some_and(|bound| waited >= bound) {
                        return Attention::Fault(self.harness_stuck(waited));
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
                    _ => self.or_backstop(self.own_work(now), now),
                }
            }
        }
    }

    /// The interrupt this row is, if it is one.
    #[must_use]
    pub fn interrupt(&self, now: u64) -> Option<Need> {
        match self.attention(now) {
            Attention::Interrupt(need) => Some(need),
            _ => None,
        }
    }

    /// `first`, unless it is quiet.
    fn or_backstop(&self, first: Attention, now: u64) -> Attention {
        if first == Attention::Quiet {
            self.backstop(now)
        } else {
            first
        }
    }

    /// The backstop: a record that has made no progress (no notice typed, no round begun).
    fn backstop(&self, now: u64) -> Attention {
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

    /// The last instant this record made progress.
    fn last_progress(&self) -> Option<u64> {
        let past = matches!(self.phase, Phase::Announced { asks, .. } if asks > upgrade::MAX_ASKS);
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

    /// How long a wait that is aterm's own business may stand before it is a fault.
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

    /// Whether the agent's own work is what holds the move now.
    fn works(&self, now: u64) -> bool {
        match self.cause() {
            Cause::OwnWork => true,
            Cause::Limit => false,
            _ => self.own_work_at != 0 && now.saturating_sub(self.own_work_at) < OWN_WORK_HOLD_S,
        }
    }

    /// A record never asked: still pending, and no round has stopped.
    fn never_asked(&self) -> bool {
        self.phase == Phase::Pending
            && self.notices_asked() == 0
            && self.stop_streak == 0
            && self.streak_why.is_empty()
            && self.last_stop.is_empty()
    }

    /// The unix second after `now` at which [`Self::attention`] next reads otherwise by time.
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
            self.hand_seen,
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

    /// The agent's own work holds the move.
    fn own_work(&self, now: u64) -> Attention {
        let behind = now.saturating_sub(self.behind_since);
        let (rounds, never_asked) = (self.unanswered_rounds(), self.never_asked());
        let notices = self.notices_asked();
        let unanswered = notices >= upgrade::MAX_ASKS;
        if behind < DIGEST_AFTER_S || (!unanswered && !never_asked) || !self.works(now) {
            return Attention::Quiet;
        }
        let held = self.held_words(now);
        let holders = self.held_by.len();
        let breaking = matches!(
            self.wait.strip_prefix("release:").unwrap_or(&self.wait),
            "background" | "background-terminal"
        );
        let why = match (holders > 0, self.goal) {
            (true, true) => {
                "aterm never ends running work: its /goal is active and work it started still runs"
            }
            (true, false) => "aterm never ends running work, and work it started is still running",
            (false, true) => "aterm never ends running work, and its /goal is still active",
            (false, false) if breaking => {
                "aterm never ends running work, and its background work is still running"
            }
            (false, false) => "aterm never ends running work, and it is busy with work of its own",
        };
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

    /// The notices this row's session has been asked and left unanswered so far, in every round.
    fn notices_asked(&self) -> u32 {
        self.notices.max(round_notices(
            &self.phase,
            &self.streak_why,
            self.stop_streak,
        ))
    }

    /// The details a surface shows behind a `Need` (Settings), never the digest line itself.
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

    /// The upgrade ended the agent and nothing runs in the tab.
    fn agent_down(&self, why: &str) -> Need {
        let (then, command) = match (self.agent, why) {
            (upgrade::Agent::Codex, "no-rollout") => {
                (cut("Start Codex again in that tab", NEED_THEN_MAX), None)
            }
            (upgrade::Agent::Muse, _) => (
                cut("Resume Muse Code in that tab", NEED_THEN_MAX),
                Some("muse resume".to_string()),
            ),
            (upgrade::Agent::Agy, _) => (
                cut(
                    "Start agy in that tab with this command, then /resume",
                    NEED_THEN_MAX,
                ),
                Some("agy".to_string()),
            ),
            (upgrade::Agent::Gemini, _) => (
                cut(
                    "Start Gemini CLI in that tab with this command, then /resume",
                    NEED_THEN_MAX,
                ),
                Some("gemini".to_string()),
            ),
            (upgrade::Agent::Claude, why) if self.fresh => {
                let then = if why == "shell-gone" {
                    "Open a new tab and start Claude Code there with this command"
                } else {
                    "Start Claude Code again in that tab with this command"
                };
                let said = cut(&self.resume_then(then), NEED_THEN_MAX);
                match self.fresh_command() {
                    Some(command)
                        if format!(" - {said}: {command}").len() + '…'.len_utf8() <= MARK_MAX =>
                    {
                        (said, Some(command))
                    }
                    _ => {
                        let then = then.strip_suffix(" with this command").unwrap_or(then);
                        (cut(then, NEED_THEN_MAX), None)
                    }
                }
            }
            (upgrade::Agent::Claude | upgrade::Agent::Codex, "shell-gone") => {
                self.resume_said("Open a new tab and resume it there with this command")
            }
            (upgrade::Agent::Claude | upgrade::Agent::Codex, _) => {
                self.resume_said("Resume it in that tab with this command")
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

    /// A relaunched agent that has reached no idle point for [`RELAUNCH_INTERRUPT_S`].
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

    /// A SIGTERMed agent that has not exited.
    fn hung_exit(&self, since: u64) -> Need {
        Need {
            kind: "hung-exit",
            fingerprint: self.fp("hung-exit"),
            title: "Agent did not exit",
            severity: NeedSeverity::Warn,
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
            command,
            since: self.wait_since,
            log_line: self.log_line("no-terminal", &format!("terminal owner: {owner}")),
        }
    }

    /// A build that failed to start and has not been proved to start.
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
        let (how, then) = if why == "not-a-shell-job" {
            (
                "was started through a wrapper or as the tab's own program, not by its shell",
                "Start it directly at a shell prompt in a new tab",
            )
        } else if why.starts_with("line:") {
            (
                "has a launch line a resume cannot repeat",
                "Start it directly at a shell prompt in that tab",
            )
        } else {
            (
                "was started with a flag a resume cannot carry",
                "Start it directly at a shell prompt in that tab",
            )
        };
        let (then, command) = self.resume_said(then);
        Need {
            kind: "refused-launch",
            fingerprint: self.fp("refused-launch"),
            title: "Agent cannot be resumed",
            severity: NeedSeverity::Info,
            why: cut(&format!("{who} {how}"), NEED_WHY_MAX),
            then,
            command,
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
            command,
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

    /// A usage limit whose named reset has passed, its notice still the recorded wait.
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

    /// A wait that is aterm's own fault (`Cause::CouldNotType`, an unclassified word).
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

    /// A record no look has read for two watch gaps, the machine awake past its wake grace.
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

    /// A Pending record no look has recorded a wait for (a fault, a record: [`Attention::Fault`]).
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

    /// A record that has made no progress in a day, whatever it waits.
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

    /// A relaunched agent that has reached no idle point (a fault, a record: [`Attention::Fault`]).
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

    /// `then` for a need whose command is the resume line ([`Self::resume_said`]).
    fn resume_then(&self, then: &str) -> String {
        let named = match self.agent {
            upgrade::Agent::Claude => resume_login(&self.line).is_some(),
            upgrade::Agent::Codex
            | upgrade::Agent::Muse
            | upgrade::Agent::Agy
            | upgrade::Agent::Gemini => false,
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

    /// The sentence and the resume command a need hands the owner.
    fn resume_said(&self, then: &str) -> (String, Option<String>) {
        let said = cut(&self.resume_then(then), NEED_THEN_MAX);
        match self.agent {
            upgrade::Agent::Codex => return (said, Some("codex resume".to_string())),
            upgrade::Agent::Claude => {}
            upgrade::Agent::Muse => return (said, Some("muse resume".to_string())),
            upgrade::Agent::Agy => {
                return (
                    cut(&format!("{then}, then /resume"), NEED_THEN_MAX),
                    Some("agy".to_string()),
                );
            }
            upgrade::Agent::Gemini => {
                return (
                    cut(&format!("{then}, then /resume"), NEED_THEN_MAX),
                    Some("gemini".to_string()),
                );
            }
        }
        if self.session.is_empty() {
            return (said, Some("claude --resume".to_string()));
        }
        let Some(plain_bare) = crate::harness::resume::bare(&self.session) else {
            return (
                said,
                Some(format!("claude --resume {}", word(&self.session))),
            );
        };
        let told = resume_command(&self.line, &self.session);
        let homed = !home_exports(&self.line).is_empty();
        let no_command = || {
            let then = then.strip_suffix(" with this command").unwrap_or(then);
            (cut(then, NEED_THEN_MAX), None)
        };
        let bare = match told.as_deref().and_then(crate::harness::resume::bare_of) {
            Some(bare) => bare,
            None if homed => return no_command(),
            None => plain_bare,
        };
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
            (&plain, Some(&bare)),
        ]
        .into_iter()
        .find_map(|(then, command)| {
            command
                .filter(|command| fits(then, command))
                .map(|command| (then.clone(), Some(command.clone())))
        });
        match chosen {
            Some(chosen) => chosen,
            None if homed => no_command(),
            None => (plain, Some(bare)),
        }
    }

    /// The command that starts again by hand an agent this row's restart started afresh.
    #[must_use]
    pub fn fresh_command(&self) -> Option<String> {
        let exports = home_exports(&self.line);
        let env: Vec<(&str, &str)> = exports.iter().map(|(n, v)| (*n, v.as_str())).collect();
        crate::harness::resume::with_env(&env, "claude")
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
    /// The upgrade cannot run at all.
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

    /// The upgrades shown cannot be renewed.
    #[must_use]
    pub fn upgrade_records_unlisted() -> Need {
        Need {
            kind: "upgrade-records-unlisted",
            fingerprint: "upgrade-records-unlisted/-".to_string(),
            title: "Agent upgrade records unreadable",
            severity: NeedSeverity::Warn,
            why: cut(
                "aterm cannot list the folder agent upgrades keep their records in",
                NEED_WHY_MAX,
            ),
            then: cut("Fix that folder's permissions", NEED_THEN_MAX),
            command: None,
            since: 0,
            log_line: "upgrade need=upgrade-records-unlisted".to_string(),
        }
    }

    /// A tab's upgrade row and mark cannot be renewed.
    #[must_use]
    pub fn upgrade_records_unreadable() -> Need {
        Need {
            kind: "upgrade-records-unreadable",
            fingerprint: "upgrade-records-unreadable/-".to_string(),
            title: "Agent upgrade record unreadable",
            severity: NeedSeverity::Warn,
            why: cut(
                "aterm cannot read an agent upgrade's record, so its tab's upgrade is not renewed",
                NEED_WHY_MAX,
            ),
            then: cut("Fix that file's owner and permissions", NEED_THEN_MAX),
            command: None,
            since: 0,
            log_line: "upgrade need=upgrade-records-unreadable".to_string(),
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
/// The class [`Row::overdue_cause`] names a move its agent's own work holds.
const OWN_WORK: &str = "held by its own work";

/// The mark's cause for a round whose last look found nothing holding its next step.
const NEXT_POINT_CAUSE: &str = "nothing holds it now; its next step goes at its next pause";

/// The row's words for the same round ([`claude_wait_words`]).
const NEXT_POINT_WORDS: &str = "its next step goes at its next pause";

/// A spend or credit wall naming no reset ([`Row::spend_held`]).
const SPEND_HELD: &str = "held by its account's credits or spend cap, which only you can lift";

/// The same wall in a stall's own words ([`Row::stall_words`]; within 64 characters).
pub const SPEND_WORDS: &str = "it is out of credits or at its spend cap";

/// The mark's cause for a usage limit whose named reset has passed with its notice.
const LIMIT_PASSED: &str =
    "its usage limit's reset has passed and the last look still read the limit";

/// The wait of a move whose agent is not in front in its tab ([`Row::aside`]).
pub const NOT_FOREGROUND: &str = "not-foreground";

/// The wait of a move whose tab another supervisor's claim holds.
pub const CLAIMED: &str = "claimed";

/// The wait of a move whose tab's Autopilot is Off ([`Row::aside`], [`View::set_off`]).
pub const AUTOPILOT_OFF: &str = "autopilot-off";

/// The wait of a move whose tab's agent its own switch holds.
pub const AGENT_OFF: &str = "agent-off";

/// The tabs where no host worker runs by a hand the host does not hold ([`Row::set_aside`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Aside {
    /// Tabs another supervisor's claim holds.
    pub claimed: BTreeSet<String>,
    /// Tabs whose Autopilot.
    pub off: BTreeSet<String>,
    /// Tabs whose agent its own switch holds.
    pub agent_off: BTreeSet<String>,
}

/// What the owner can do about a stalled upgrade ([`Row::remedy`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    /// `overdue` waiting on what `--now` still moves ([`now_moves_past`]).
    Now,
    /// `overdue` waiting on what `--now` does not waive.
    Waits,
    /// A given-up round ([`upgrade::GAVE_UP`]) resting before it asks again.
    AskAgain,
    /// `held-back:<owner>`: typing into the tab cannot reach it, so no word moves it.
    InItsPane,
    /// `refused:*`, `failed:*`.
    ByHand,
    /// A move that stopped after its agent was seen gone ([`Row::failed_after_exit`]).
    ResumeInTab,
}

/// How long a person at the tab (`attended`) stays the owner's word for a wait with no look.
pub const ATTENDED_SEEN_S: u64 = 3 * upgrade::QUIET_S;

/// Whether `--now` moves an overdue upgrade past the wait a step recorded ([`upgrade::wait_word`]).
fn now_moves_past(wait: &str) -> bool {
    matches!(wait, "left-typed-backoff" | "queued") || wait.starts_with("announce-refused:")
}

/// A recorded wait in a person's words, for `agent`.
pub(in crate::harness) fn wait_words_of(agent: upgrade::Agent, wait: &str) -> Option<&'static str> {
    match agent {
        upgrade::Agent::Claude => claude_wait_words(wait),
        upgrade::Agent::Codex
        | upgrade::Agent::Muse
        | upgrade::Agent::Agy
        | upgrade::Agent::Gemini => relaunch_line_words(wait),
    }
}

/// Whether a row of `agent`'s reads its record's ready clock.
const fn reads_ready_clock(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex
        | upgrade::Agent::Muse
        | upgrade::Agent::Agy
        | upgrade::Agent::Gemini => false,
    }
}

/// Whether `agent`'s lane types a release line (`upgrade_drive::release`, [`Row::release`]).
const fn types_release(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex
        | upgrade::Agent::Muse
        | upgrade::Agent::Agy
        | upgrade::Agent::Gemini => false,
    }
}

/// Whether `agent`'s `attended` wait names the person at its tab on the look's clock.
const fn attended_is_person(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex
        | upgrade::Agent::Muse
        | upgrade::Agent::Agy
        | upgrade::Agent::Gemini => false,
    }
}

/// Whether a move of `agent`'s that failed after its exit is an ordinary stopped upgrade again.
const fn held_again_after_exit(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex
        | upgrade::Agent::Muse
        | upgrade::Agent::Agy
        | upgrade::Agent::Gemini => false,
    }
}

/// What a Claude Code wait stands for, in a person's words ([`Row::wait_words`]).
fn claude_wait_words(wait: &str) -> Option<&'static str> {
    Some(match wait {
        "background" | "not-idle:shell" => "its own work runs",
        "busy" | "not-idle:busy" => "its turn is still running",
        "not-idle:waiting" => "it asked a question and waits",
        "settling" => "waiting for the tab to settle",
        upgrade::NOTICE_SHOWN => "it moves over once it is idle",
        upgrade::NEXT_POINT => NEXT_POINT_WORDS,
        "attended" => "someone is using its tab",
        "awaiting-ready" | "not-ready" => "waiting for it to agree to the move",
        "ready-unrecorded" => "it agreed; its answer is being read",
        "superseded" => "it agreed, then got more work: it is asked again",
        "draft" => "a draft waits in its prompt",
        "box" => "a box on its screen waits for a choice",
        "held" => "its tab is held",
        NOT_FOREGROUND => "it is not in front in its tab",
        CLAIMED => "another supervisor holds its tab",
        AUTOPILOT_OFF => "its autopilot is off",
        AGENT_OFF => "its agent is switched off",
        "limited" => "it is at a usage limit",
        "queued" => "it has not read the upgrade's question yet",
        "login" => "it is not logged in",
        "in-flight" => "a step is under way",
        crate::harness::upgrade_drive::BUILD_FAILS => "its new build fails to start",
        "announce-refused:changed" => {
            "aterm could not type its notice: the screen moved as it typed"
        }
        "announce-refused:yield" => {
            "aterm could not type its notice: someone kept typing in its tab"
        }
        "announce-refused:survey" => {
            "aterm left Claude Code's survey to the person and types its notice once it is gone"
        }
        w if w.starts_with("announce-refused:cut-") => "aterm stopped its notice part-way, unsent",
        "left-typed:mixed" => "aterm's words are in its prompt, with a person's keys",
        "left-typed:screen-unreadable" => "aterm cannot read its screen",
        w if w.starts_with("left-typed:") => "aterm's own words are still in its prompt, unsent",
        w if w.starts_with("announce-refused:") => "aterm could not type its notice",
        w if w.starts_with("release:") => "it is owed the line telling it to carry on",
        w if w.starts_with("status-stale") => "its status lags",
        w if w.starts_with("not-idle") => "it has not gone idle",
        w => return relaunch_line_words(w),
    })
}

/// The relaunch line's own waits.
fn relaunch_line_words(wait: &str) -> Option<&'static str> {
    Some(match wait {
        "exiting" => "it has not exited yet",
        "typing" => "the prompt it returned to does not read as empty",
        "mark-elsewhere" => "its shell and its screen disagree on where its prompt is",
        "unmarked" => "aterm sees no prompt where its shell returned",
        "keyed-while-held" => "its shell may have been used while its tab was held",
        "prompt-moved" => "its prompt changed as aterm read it",
        "yield" | "cut" => "someone typed in its tab as the line that resumes it went in",
        "shell-prompt" => "its shell has not taken the terminal back",
        "screen-unreadable" => "aterm cannot read its screen",
        "tab-ownership-changed" => "another program holds its tab now",
        "conversation-owner-ambiguous" => "another process still holds its conversation",
        _ => return None,
    })
}

/// How long a session has been behind, in a person's words (`40 min`, `8 h`, `3 days`).
fn behind_words(since: u64, now: u64) -> Option<String> {
    const YEAR_S: u64 = 365 * 86_400;
    let secs = now.checked_sub(since).filter(|_| since > 0)?;
    if secs == 0 || secs > YEAR_S {
        return None;
    }
    Some(elapsed_words(secs))
}

/// A span of `secs` in a person's words (`40 min`, `8 h`, `3 days`, at least `1 min`).
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

/// The first unix second after `now` at which [`behind_words`] of `since` reads otherwise.
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

/// Why a stall of `kind` ([`Row::stall`], any kind but `overdue`) will not move on its own.
fn stall_reason(agent: upgrade::Agent, kind: &str) -> String {
    let after_exit = match agent {
        upgrade::Agent::Claude => None,
        upgrade::Agent::Codex => kind.strip_prefix("failed:").and_then(codex_failure),
        upgrade::Agent::Muse | upgrade::Agent::Agy | upgrade::Agent::Gemini => None,
    };
    if let Some(words) = after_exit {
        return words.to_string();
    }
    if let Some(what) = kind.strip_prefix("stuck:") {
        return stuck_words(agent, what).to_string();
    }
    match kind {
        "refused:not-a-shell-job" => {
            "it is not its shell's foreground job, so nothing brings it back on the new build"
                .to_string()
        }
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
        s if s.starts_with("held-back:") => format!(
            "it runs inside {}, where the upgrade cannot type to it",
            runs_under(&s[10..])
        ),
        other => other.to_string(),
    }
}

/// Why a move stopped, in a person's words.
fn stop_words(agent: upgrade::Agent, why: &str) -> String {
    let who = match agent {
        upgrade::Agent::Claude => "Claude",
        upgrade::Agent::Codex => "Codex",
        upgrade::Agent::Muse | upgrade::Agent::Agy | upgrade::Agent::Gemini => agent.product(),
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

/// Why a Codex move stopped, for the kinds that stop it after its exit.
fn codex_failure(why: &str) -> Option<&'static str> {
    Some(match why {
        "no-resume" => "its resume line was typed at the prompt and no Codex came up in the tab",
        "stale-exit" => "it exited minutes ago and was never resumed",
        "shell-gone" => "the shell it ran in is gone: there is no prompt to resume it at",
        "relaunch-refused" => "the line that resumes it was refused at its prompt",
        "no-rollout" => "its conversation's rollout is gone: there is nothing to resume",
        super::codex_alab_drive::LANE_RETIRED => {
            "aterm no longer moves a vendor Codex, so the move it began was not finished; \
             `codex resume` in the tab takes the conversation back"
        }
        _ => return None,
    })
}

/// What a restart under way that has not moved waits.
pub(super) fn stuck_on(phase: &Phase, exited_at: u64, now: u64) -> Option<&'static str> {
    let (at_s, what) = match *phase {
        Phase::Exiting { at_s } if exited_at == 0 => (at_s, "exiting"),
        Phase::Exiting { at_s } => (at_s, "exited"),
        Phase::Relaunched { at_s } => (at_s, "relaunched"),
        _ => return None,
    };
    (now.saturating_sub(at_s) > STALE_S).then_some(what)
}

/// What a stuck restart waits on ([`Row::stuck`]), in words that ask for nothing.
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
            "it ended for the upgrade, and the line that resumes it is not typed while a person \
             or a hold is on its prompt, or at all under Autopilot Off or its agent's switch off; \
             nothing is forced"
        }
        (upgrade::Agent::Muse, "exiting") => {
            "it was ended for the upgrade (SIGTERM, at an idle point) and has not exited yet; \
             nothing is forced"
        }
        (upgrade::Agent::Muse, "exited") => {
            "it ended for the upgrade, and the line that resumes it on the new build is not typed \
             while a person or a hold is on its prompt, or at all under Autopilot Off or its \
             agent's switch off; nothing is forced"
        }
        (upgrade::Agent::Muse, _) => {
            "the line that resumes it on the new build was typed, and the Muse Code it started has \
             not taken its session yet; nothing is forced"
        }
        (upgrade::Agent::Agy, "exiting") => {
            "it was ended for the upgrade (SIGTERM, at an idle point) and has not exited yet; \
             nothing is forced"
        }
        (upgrade::Agent::Agy, "exited") => {
            "it ended for the upgrade, and the line that resumes it on the new build waits until a \
             person or a hold lets go of its prompt; nothing is forced"
        }
        (upgrade::Agent::Agy, _) => {
            "the line that resumes it on the new build was typed, and the agy it started has not \
             taken its conversation yet; nothing is forced"
        }
        (upgrade::Agent::Gemini, _) => NO_LANE_WORDS,
        (upgrade::Agent::Claude | upgrade::Agent::Codex, _) => {
            "the line that resumes it was typed, and the agent it started has not reached an \
             idle point, perhaps behind a box that waits on a person; nothing is forced"
        }
    }
}

/// What a held-back agent runs under, for a line.
pub(in crate::harness) fn runs_under(owner: &str) -> &str {
    match owner {
        "" | "none" | "?" | "-" => "a terminal that is not the tab's",
        name => name,
    }
}

/// Whether a stop's reason is a refusal (the upgrade would not type or signal at all).
fn refusal(why: &str) -> bool {
    why == "not-a-shell-job" || why.starts_with("argv:") || why.starts_with("line:")
}

/// A third party's or a derived word, cut to what a roster token may carry.
fn word(s: &str) -> String {
    let keep =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '(' | ')');
    if !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| keep(char::from(b))) {
        return s.to_owned();
    }
    let w: String = s.chars().filter(|c| keep(*c)).take(64).collect();
    if w.is_empty() { "-".to_string() } else { w }
}

/// Every upgrade record as a [`Row`], sorted by conversation.
#[must_use]
pub fn rows(opts: &Opts) -> Vec<Row> {
    rows_at(opts, now_s())
}

fn rows_at(opts: &Opts, now: u64) -> Vec<Row> {
    read_rows(opts, now).0
}

/// [`rows_at`], plus whether the records were read whole.
fn read_rows(opts: &Opts, now: u64) -> (Vec<Row>, bool) {
    let (rows, listed, unread) = read_records(opts, now);
    (rows, listed && unread.is_empty())
}

/// A record listed but not read ([`read_records`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unread {
    session: String,
    tab: Option<String>,
    path: std::path::PathBuf,
}

/// [`read_rows`] apart: the rows of the records read.
fn read_records(opts: &Opts, now: u64) -> (Vec<Row>, bool, Vec<Unread>) {
    let dir = match std::fs::read_dir(state_dir(opts)) {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Vec::new(), true, Vec::new());
        }
        Err(_) => return (Vec::new(), false, Vec::new()),
    };
    let mut whole = true;
    let mut unread: Vec<Unread> = Vec::new();
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
                unread.push(Unread {
                    session,
                    tab: None,
                    path,
                });
                continue;
            }
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            record_row(opts, &session, &text, now)
        })) {
            Ok(Some(row)) => out.push(row),
            Ok(None) => {}
            Err(_) => {
                let tab = std::panic::catch_unwind(|| St::from_json(&text).map(|st| st.tab))
                    .ok()
                    .flatten()
                    .filter(|tab| !tab.is_empty());
                unread.push(Unread { session, tab, path });
            }
        }
    }
    out.sort_by(|a, b| a.session.cmp(&b.session));
    unread.sort_by(|a, b| a.session.cmp(&b.session));
    (out, whole, unread)
}

/// One upgrade record's row ([`read_rows`]).
fn record_row(opts: &Opts, session: &str, text: &str, now: u64) -> Option<Row> {
    #[cfg(test)]
    if session == RECORD_PANICS {
        panic!("a record whose read panics (a test's)");
    }
    let st = St::from_json(text).filter(St::is_upgrade)?;
    if !opts.only_sid.as_ref().is_none_or(|s| *s == st.tab) {
        return None;
    }
    Some(Row::of(session, &st, now).in_flow(opts, now))
}

/// The session whose record's read panics ([`record_row`], tests only).
#[cfg(test)]
pub(crate) const RECORD_PANICS: &str = "0badf00d-dead-4bad-8bad-panicsonread0";

/// [`rows`] as `aterm harness upgrade --status` shows them.
#[must_use]
pub fn status_rows(opts: &Opts, claims: bool) -> (Vec<Row>, bool) {
    status_rows_with(
        opts,
        now_s(),
        &|sessions| holders(opts, sessions, None),
        &|| aside_tabs(opts, claims),
    )
}

/// The tabs set aside by a hand the host does not hold ([`Aside`]).
fn aside_tabs(opts: &Opts, claims: bool) -> Aside {
    if !claims {
        return Aside::default();
    }
    let one = || {
        super::connect_reads(opts, opts.only_sid.as_deref().unwrap_or(""))
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
                .filter_map(|(_, sock)| super::roster_connection(sock)),
            &opts.agents_off,
        ),
        _ => one(),
    }
}

/// [`status_rows`] at `now`, over `holders` and the tabs `aside` names.
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
                        r.held_again(&held, true);
                        r
                    })
                    .filter(|r| r.standing(&held, true))
                    .map(|mut r| {
                        // host watches it ([`Row::outside`]).
                        if r.held_by_a_process() && !r.standing(&held, false) {
                            r.outside = true;
                            r.read_at(now);
                        }
                        r.unreached_status(&held, true, now);
                        if r.tab.is_empty() {
                            r.tab = r.proven_tab(&held).unwrap_or_default();
                        }
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

/// Tab `tab`'s row (lc-9 of ).
#[must_use]
pub fn tab_row<'a>(rows: &'a [Row], tab: &str) -> Option<&'a Row> {
    rows.iter()
        .filter(|r| r.tab == tab)
        .max_by(|a, b| a.tab_rank().cmp(&b.tab_rank()))
}

/// The conversations whose rows [`Row::standing`] must vet.
fn sessions_to_vet(rows: &[Row]) -> BTreeSet<String> {
    rows.iter()
        .filter(|r| {
            r.held_by_a_process() || (held_again_after_exit(r.agent) && r.down_after_exit())
        })
        .map(|r| r.session.clone())
        .collect()
}

/// One live Claude Code process a recorded upgrade is vetted against ([`Row::standing`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Holder {
    pub(super) pid: u32,
    pub(super) session: String,
    pub(super) version: String,
    pub(super) tab: Option<String>,
    pub(super) status: String,
    pub(super) status_since: u64,
    pub(super) aside: bool,
    pub(super) led: bool,
    pub(super) answers: Option<String>,
}

/// Whether `pid` is proven not the foreground of its terminal, from the kernel now.
fn not_in_front(pid: u32) -> bool {
    super::ids(pid).is_some_and(|(_, group, front)| group > 0 && front > 0 && group != front)
}

/// The live holders of `sessions`.
fn holders(
    opts: &Opts,
    sessions: &BTreeSet<String>,
    tabs: Option<&[LiveTab]>,
) -> Option<Vec<Holder>> {
    let home = opts.home.as_path();
    let lanes_of = |sessions: &BTreeSet<String>| -> Vec<Holder> {
        upgrade::Agent::others()
            .iter()
            .flat_map(|agent| match agent {
                upgrade::Agent::Muse => super::muse::holders(sessions, tabs),
                upgrade::Agent::Agy => super::agy::holders(sessions, tabs),
                upgrade::Agent::Gemini => super::gemini::holders(sessions, tabs),
                upgrade::Agent::Codex | upgrade::Agent::Claude => Vec::new(),
            })
            .map(|h| Holder {
                led: tabs.is_some(),
                ..h
            })
            .collect()
    };
    if sessions.iter().all(|s| upgrade::Agent::keyed(s).is_some()) {
        return Some(lanes_of(sessions));
    }
    let down = agent_down_tabs(opts, sessions, now_s());
    let lanes = if down.is_empty() {
        lanes_of(sessions)
    } else {
        let mut asked = sessions.clone();
        asked.extend(down.iter().flat_map(|tab| {
            upgrade::Agent::others()
                .iter()
                .filter_map(move |agent| agent.record_key(tab))
        }));
        lanes_of(&asked)
    };
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
    let live =
        |sf: &upgrade::SessionFile| conversation_live(std::slice::from_ref(sf), &sf.session_id);
    let gone = shell_gone_afresh(opts, sessions, now_s());
    let mut held: Vec<Holder> = Vec::new();
    let mut elsewhere: Vec<(&upgrade::SessionFile, Holder)> = Vec::new();
    for sf in &files {
        if sessions.contains(&sf.session_id) {
            if live(sf) {
                held.push(holder_of(sf, tabs));
            }
            continue;
        }
        if (down.is_empty() && gone.is_empty())
            || sf.kind != "interactive"
            || sf.entrypoint != "cli"
            || !upgrade::is_session_id(&sf.session_id)
            || !super::alive(sf.pid)
        {
            continue;
        }
        let h = holder_of(sf, tabs);
        match h.tab.as_ref() {
            Some(t) if down.contains(t) => {
                if live(sf) {
                    held.push(h);
                }
            }
            Some(_) if !gone.is_empty() && live(sf) => elsewhere.push((sf, h)),
            _ => {}
        }
    }
    held.extend(answering_shell_gone(home, &gone, elsewhere, started_s));
    Some(held.into_iter().chain(lanes).collect())
}

/// A fresh restart's row whose shell is gone, as [`answering_shell_gone`] matches an agent to it.
pub(super) struct ShellGone {
    /// The row's conversation.
    pub(super) session: String,
    /// The Claude Code directory its agent ran under ([`St::agent_records`]).
    pub(super) dir: Option<std::path::PathBuf>,
    /// The agent's working directory ([`St::cwd`]).
    pub(super) cwd: String,
    /// When its agent exited.
    pub(super) exited_at: u64,
}

/// The fresh restarts' rows in `sessions` whose shell is gone and that are still shown at `now`.
fn shell_gone_afresh(opts: &Opts, sessions: &BTreeSet<String>, now: u64) -> Vec<ShellGone> {
    sessions
        .iter()
        .filter(|s| upgrade::Agent::keyed(s).is_none())
        .filter_map(|s| {
            let st = load(opts, s)?;
            let r = Row::of(s, &st, now);
            (r.shell_gone_afresh() && now.saturating_sub(r.exited_at) <= EXITED_FAILURE_SHOWN_S)
                .then(|| ShellGone {
                    session: s.clone(),
                    dir: st.agent_records(&opts.home).dir().map(Path::to_path_buf),
                    cwd: st.cwd.clone(),
                    exited_at: r.exited_at,
                })
        })
        .collect()
}

/// The agents that answer fresh `shell-gone` rows.
pub(super) fn answering_shell_gone(
    user_home: &Path,
    gone: &[ShellGone],
    mut others: Vec<(&upgrade::SessionFile, Holder)>,
    started: impl Fn(u32) -> Option<u64>,
) -> Vec<Holder> {
    let mut rows: Vec<&ShellGone> = gone
        .iter()
        .filter(|g| !g.cwd.is_empty() && g.dir.is_some())
        .collect();
    rows.sort_by(|a, b| (a.exited_at, &a.session).cmp(&(b.exited_at, &b.session)));
    let mut others: Vec<(u64, &upgrade::SessionFile, Holder)> = others
        .drain(..)
        .filter_map(|(sf, h)| Some((started(sf.pid)?, sf, h)))
        .collect();
    others.sort_by_key(|(at, sf, _)| (*at, sf.pid));
    let mut out = Vec::new();
    for g in rows {
        let Some(i) = others.iter().position(|(at, sf, _)| {
            *at > g.exited_at
                && sf.cwd == g.cwd
                && super::Records::of_file(user_home, sf).dir() == g.dir.as_deref()
        }) else {
            continue;
        };
        let (_, _, mut h) = others.remove(i);
        h.answers = Some(g.session.clone());
        out.push(h);
    }
    out
}

/// The live holder session file `sf`.
fn holder_of(sf: &upgrade::SessionFile, tabs: Option<&[LiveTab]>) -> Holder {
    let by_group =
        tabs.and_then(|tabs| unique_tab_for_group(tabs, process_group(sf.pid)?).map(str::to_owned));
    let by_env = atpkg::caller_shell::process_args(sf.pid)
        .and_then(|a| a.env_var("ATERM_PARENT_SESSION_ID").map(str::to_owned));
    let aside = by_group.is_none() && by_env.is_some() && not_in_front(sf.pid);
    let led = by_group.is_some();
    let tab = match (by_group, by_env) {
        (Some(group), Some(env)) if group != env => None,
        (group, env) => group.or(env),
    };
    Holder {
        pid: sf.pid,
        session: sf.session_id.clone(),
        version: sf.version.clone(),
        led: led && tab.is_some(),
        tab,
        status: sf.status.clone(),
        status_since: sf.status_updated_at_ms / 1_000,
        aside,
        answers: None,
    }
}

/// The tabs a fresh restart left with no agent.
fn agent_down_tabs(opts: &Opts, sessions: &BTreeSet<String>, now: u64) -> BTreeSet<String> {
    sessions
        .iter()
        .filter(|s| upgrade::Agent::keyed(s).is_none())
        .filter_map(|s| Some(Row::of(s, &load(opts, s)?, now)))
        .filter(|r| {
            r.answered_by_its_tabs_agent()
                && now.saturating_sub(r.exited_at) <= EXITED_FAILURE_SHOWN_S
        })
        .map(|r| r.tab)
        .collect()
}

/// What the owner asks of one tab's upgrade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Restart at the next turn end ([`Request::Now`]).
    Now,
    /// Not for this many seconds ([`Request::DeferUntil`]).
    Defer(u64),
    /// Stay on the running build until a newer one than the target comes ([`Request::Skip`]).
    Skip,
}

/// How long [`ask`] waits for a step that holds the lock.
#[cfg(not(test))]
const ASK_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The same bound in this crate's tests, short.
#[cfg(test)]
const ASK_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(300);

/// Prefix of [`ask`]'s refusal of `--now` for a stopped round the owner's word may not re-arm.
pub const REFUSED_STOPPED: &str = "stopped:";

/// A `--now` refused because its round stopped ([`REFUSED_STOPPED`]).
#[must_use]
pub fn refused_stopped(e: &str) -> Option<(u64, &str)> {
    let (at, sentence) = e.strip_prefix(REFUSED_STOPPED)?.split_once(':')?;
    Some((at.parse().ok()?, sentence))
}

/// What a person at a shell is told for [`ask`]'s refusal.
#[must_use]
pub fn refusal_sentence(e: &str) -> &str {
    refused_stopped(e).map_or(e, |(_, sentence)| sentence)
}

/// Writes the owner's word for the upgrade in tab `sid`, under the sweep lock.
pub fn ask(opts: &Opts, sid: &str, what: Ask, claims: bool) -> Result<Row, String> {
    ask_aside(opts, sid, what, &|| aside_tabs(opts, claims))
}

/// [`ask`] over the tabs `aside` names, asked only for a row a live process may hold.
fn ask_aside(opts: &Opts, sid: &str, what: Ask, aside: &dyn Fn() -> Aside) -> Result<Row, String> {
    let (mut row, held) = ask_within(opts, sid, what, None, ASK_LOCK_WAIT, &|s| {
        holders(opts, s, None)
    })?;
    if row.held_by_a_process() {
        row.set_aside(&held, true, &aside(), now_s());
    }
    Ok(row)
}

/// [`ask`] for the row the owner was shown.
pub fn ask_for(
    opts: &Opts,
    sid: &str,
    what: Ask,
    target: &str,
    session: &str,
) -> Result<Row, String> {
    let shown = Shown {
        to: target,
        session,
    };
    ask_within(opts, sid, what, Some(shown), ASK_LOCK_WAIT, &|s| {
        holders(opts, s, None)
    })
    .map(|(row, _)| row)
}

/// The row a window showed the owner, which a pressed word is for ([`ask_for`]).
#[derive(Clone, Copy, Debug)]
struct Shown<'a> {
    /// The build it named.
    to: &'a str,
    /// Its conversation.
    session: &'a str,
}

/// The word written as [`ask`] writes it, waiting at most `wait` for the lock.
fn ask_within(
    opts: &Opts,
    sid: &str,
    what: Ask,
    shown: Option<Shown<'_>>,
    wait: std::time::Duration,
    held_of: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>,
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
                    let st = load(opts, &session).filter(St::is_upgrade)?;
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
            upgrade::Agent::Codex => "once its `/exit` is typed",
            upgrade::Agent::Muse | upgrade::Agent::Agy | upgrade::Agent::Gemini => {
                "by any lane: no lane moves this agent"
            }
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
    let held = held_of(&candidates.iter().map(|(s, _)| s.clone()).collect()).unwrap_or_default();
    candidates.retain(|(session, st)| {
        !st.tab.is_empty() || Row::of(session, st, now).proven_tab(&held).as_deref() == Some(sid)
    });
    let no_hand = Aside::default();
    let ranked: Vec<(bool, Row)> = candidates
        .iter()
        .map(|(session, st)| {
            let mut row = Row::of(session, st, now);
            row.held_again(&held, true);
            row.set_aside(&held, true, &no_hand, now);
            (row.standing(&held, true), row)
        })
        .collect();
    let pick = shown
        .filter(|shown| !shown.session.is_empty())
        .and_then(|shown| candidates.iter().position(|(s, _)| s == shown.session))
        .or_else(|| {
            (0..ranked.len()).max_by(|&a, &b| {
                let rank = |i: usize| (ranked[i].0, ranked[i].1.tab_rank());
                rank(a).cmp(&rank(b))
            })
        });
    let Some((session, mut st)) = pick.map(|i| candidates.swap_remove(i)) else {
        return Err(format!(
            "no upgrade is recorded for tab {sid}; `aterm harness upgrade {sid} --dry-run` \
             shows the plan"
        ));
    };
    if let Some(shown) = shown
        && !same_target(shown.to, &st.to)
    {
        return Err(format!("stale:{}", word(&st.to)));
    }
    match what {
        Ask::Now => {
            match &st.phase {
                Phase::Failed(why) if why == upgrade::GAVE_UP => {
                    st.phase = Phase::Pending;
                    st.forget_markers();
                    st.noted.clear();
                }
                Phase::Failed(why)
                    if st.exited_at != 0
                        && !(held_again_after_exit(st.agent)
                            && held.iter().any(|h| h.session == session)) =>
                {
                    return Err(match st.agent {
                        upgrade::Agent::Codex => format!(
                            "the upgrade in tab {sid} stopped after its `/exit` ({}): `--now` \
                             re-arms only one that gave up — `codex resume` in the tab takes \
                             its conversation back, and `--skip` quiets this record",
                            word(why)
                        ),
                        upgrade::Agent::Claude
                            if crate::harness::relaunch::starts_afresh(&st.cause) =>
                        {
                            let row = Row::of(&session, &st, now);
                            format!(
                                "the upgrade in tab {sid} stopped after its SIGTERM ended the \
                                 agent ({}): `--now` re-arms only one that gave up — {} in the tab \
                                 starts it again (it had no conversation to resume), and `--skip` \
                                 quiets this record",
                                word(why),
                                row.fresh_command().map_or_else(
                                    || "Claude Code started again".to_string(),
                                    |c| format!("`{c}`")
                                )
                            )
                        }
                        upgrade::Agent::Claude => format!(
                            "the upgrade in tab {sid} stopped after its SIGTERM ended the agent \
                             ({}): `--now` re-arms only one that gave up — `claude --resume {}` \
                             in the tab takes its conversation back, and `--skip` quiets this \
                             record",
                            word(why),
                            word(&session)
                        ),
                        upgrade::Agent::Muse | upgrade::Agent::Agy | upgrade::Agent::Gemini => {
                            format!(
                                "the upgrade in tab {sid} stopped ({}): no lane of aterm's moves \
                                 this agent, and `--skip` quiets this record",
                                word(why)
                            )
                        }
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
        }
        Ask::Defer(secs) => st.request = Request::DeferUntil(now.saturating_add(secs)),
        Ask::Skip => st.request = Request::Skip(st.to.clone()),
    }
    st.request_at = now;
    st.new_round(now);
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
    if matches!(&st.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) && what != Ask::Now {
        st.forget_markers();
    }
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

/// Whether the build a word was given for is the one an upgrade moves.
fn same_target(shown: &str, to: &str) -> bool {
    match (upgrade::Version::parse(shown), upgrade::Version::parse(to)) {
        (Some(a), Some(b)) => a == b,
        _ => !shown.is_empty() && shown == to,
    }
}

/// The window's view of its tabs' upgrades, kept by its host thread (`aterm-gui`'s `harness_host`).
#[derive(Debug, Default)]
pub struct View {
    sent: Option<Vec<Row>>,
    /// The tabs the rows last handed over left as they stood ([`Self::refresh_keeping`]).
    sent_kept: Vec<String>,
    badged: BTreeMap<String, String>,
    sock: Option<String>,
    /// Looks in a row whose roster could not be read (socket not bound yet, a busy window).
    unread: u32,
    /// The tabs whose row the last whole look read off its live holder's status.
    follows: Vec<String>,
    /// The tabs' agents back ([`Row::held_again`]).
    back: BTreeMap<String, u64>,
    /// [`Self::back`] was read from [`BACK_FILE`] (at the first whole look).
    back_read: bool,
    /// Tabs with a record the last whole look found past its watch ([`Self::watch_due`]).
    watch: Vec<String>,
    /// Each record's tab as its last READ gave it, by conversation ([`Self::refresh_keeping`]).
    placed: BTreeMap<String, String>,
    /// Since when this view's host has been watching, unix seconds ([`Self::note_look`]).
    watching_since: u64,
    /// The last look's clocks, monotonic and wall.
    looked: Option<(std::time::Instant, u64)>,
    /// Tabs whose worker the host parks behind another supervisor's claim ([`Self::set_claimed`]).
    aside: Aside,
    /// The live tabs whose `owner=upgrade` attention this view has settled.
    settled: BTreeSet<String>,
    /// The records' folder the last look could not list whole ([`Self::unlisted`]).
    unlisted: Option<std::path::PathBuf>,
    /// The records the last whole look could not READ that keep a live tab as it stands.
    unreadable: Vec<std::path::PathBuf>,
}

/// Where a view keeps [`View::back`] beside the records it reads.
pub const BACK_FILE: &str = "agents-back";

/// The most [`BACK_FILE`] is read.
const BACK_MAX: u64 = 64 * 1024;

/// [`View::back`] as [`write_back`] left it.
fn read_back(opts: &Opts) -> BTreeMap<String, u64> {
    let path = state_dir(opts).join(BACK_FILE);
    let Some(text) = std::fs::File::open(&path).ok().and_then(|f| {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::Read::take(f, BACK_MAX), &mut text)
            .ok()
            .map(|_| text)
    }) else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let (session, exited) = line.split_once(' ')?;
            let exited = exited.parse::<u64>().ok().filter(|at| *at != 0)?;
            (!session.is_empty() && !session.contains(char::is_whitespace))
                .then(|| (session.to_string(), exited))
        })
        .collect()
}

/// Writes `back` to [`BACK_FILE`] (renamed into place).
fn write_back(opts: &Opts, back: &BTreeMap<String, u64>) {
    let path = state_dir(opts).join(BACK_FILE);
    if back.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let body: String = back
        .iter()
        .map(|(session, exited)| format!("{session} {exited}\n"))
        .collect();
    let _ = crate::harness::upgrade_models::write_atomic(&path, &body);
}

/// How far the wall clock may run ahead of the monotonic one between two looks before the host.
const SLEPT_S: u64 = 60;

/// Pauses before looking again at an unreadable roster, from two seconds to ten minutes.
const UNREAD_RETRY: [std::time::Duration; 4] = [
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(16),
    std::time::Duration::from_secs(120),
    std::time::Duration::from_secs(600),
];

impl View {
    /// Looks at the upgrades of the instance at `opts.sock`.
    pub fn refresh(&mut self, opts: &Opts, summary: &mut dyn FnMut(&[Row])) -> Option<u64> {
        self.refresh_keeping(opts, &mut |rows, _| summary(rows))
    }

    /// [`Self::refresh`], handing over with the rows the tabs they leave as they stand.
    pub fn refresh_keeping(
        &mut self,
        opts: &Opts,
        summary: &mut dyn FnMut(&[Row], &[String]),
    ) -> Option<u64> {
        self.note_look(std::time::Instant::now(), now_s());
        let Some(tabs) = connect(opts, "")
            .ok()
            .and_then(|mut c| super::host_roster(&mut c))
        else {
            let pause = crate::supervise::ladder::Ladder(&UNREAD_RETRY)
                .step(usize::try_from(self.unread).unwrap_or(usize::MAX));
            self.unread = self.unread.saturating_add(1);
            self.watch.clear();
            return Some(now_s().saturating_add(pause.as_secs()));
        };
        self.unread = 0;
        self.refresh_with_keeping(
            opts,
            &tabs,
            now_s(),
            &|sessions| holders(opts, sessions, Some(&tabs)),
            summary,
        )
    }

    /// [`Self::refresh`] at `now` over the roster `tabs`.
    #[cfg(all(test, unix))]
    fn refresh_with(
        &mut self,
        opts: &Opts,
        tabs: &[LiveTab],
        now: u64,
        holders: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>,
        summary: &mut dyn FnMut(&[Row]),
    ) -> Option<u64> {
        self.refresh_with_keeping(opts, tabs, now, holders, &mut |rows, _| summary(rows))
    }

    /// [`Self::refresh_with`], handing over the tabs left as they stand.
    fn refresh_with_keeping(
        &mut self,
        opts: &Opts,
        tabs: &[LiveTab],
        now: u64,
        holders: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>,
        summary: &mut dyn FnMut(&[Row], &[String]),
    ) -> Option<u64> {
        let (mut all, whole, unread) = read_records(opts, now);
        self.unlisted = (!whole).then(|| state_dir(opts));
        let mut kept: BTreeSet<String> = BTreeSet::new();
        let mut unplaced: BTreeSet<String> = BTreeSet::new();
        let mut unreadable: Vec<std::path::PathBuf> = Vec::new();
        if whole {
            let mut placed: BTreeMap<String, String> = all
                .iter()
                .filter(|r| !r.tab.is_empty())
                .map(|r| (r.session.clone(), r.tab.clone()))
                .collect();
            for u in &unread {
                match u
                    .tab
                    .clone()
                    .or_else(|| self.placed.get(&u.session).cloned())
                {
                    Some(tab) => {
                        if tab_is_live(tabs, &tab) {
                            unreadable.push(u.path.clone());
                        }
                        placed.insert(u.session.clone(), tab.clone());
                        kept.insert(tab);
                    }
                    None => {
                        if !tabs.is_empty() {
                            unreadable.push(u.path.clone());
                        }
                        unplaced.insert(u.session.clone());
                    }
                }
            }
            self.placed = placed;
        }
        self.unreadable = unreadable;
        for r in &mut all {
            r.host_watching_since(self.watching_since, now);
        }
        let (untabbed, all): (Vec<Row>, Vec<Row>) = all
            .into_iter()
            .partition(|r| r.tab.is_empty() && r.watch_at != 0 && r.held_by_a_process());
        let (all, elsewhere): (Vec<Row>, Vec<Row>) =
            all.into_iter().partition(|r| tab_is_live(tabs, &r.tab));
        let mut mine: Vec<Row> = all
            .into_iter()
            .filter(|r| {
                r.phase != Phase::Done
                    || r.restarting(now)
                    || now.saturating_sub(r.finished_at()) <= DONE_SHOWN_S
            })
            .collect();
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
        vet.extend(unplaced.iter().cloned());
        let held = if vet.is_empty() {
            None
        } else {
            let Some(held) = holders(&vet) else {
                return watch_next;
            };
            Some(held)
        };
        for session in &unplaced {
            let at: Vec<String> = held
                .iter()
                .flatten()
                .filter(|h| h.session == *session)
                .filter_map(|h| h.tab.clone())
                .filter(|tab| tab_is_live(tabs, tab))
                .collect();
            if at.is_empty() {
                kept.extend(tabs.iter().map(|t| t.sid.clone()));
            } else {
                kept.extend(at);
            }
        }
        kept.retain(|tab| tab_is_live(tabs, tab));
        let kept: Vec<String> = kept.into_iter().collect();
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
        let down = |r: &Row| held_again_after_exit(r.agent) && r.down_after_exit();
        if !self.back_read {
            self.back_read = true;
            for (session, exited) in read_back(opts) {
                self.back.entry(session).or_insert(exited);
            }
        }
        let before = self.back.clone();
        self.back.retain(|session, _| {
            unread.iter().any(|u| u.session == *session)
                || mine
                    .iter()
                    .chain(&elsewhere)
                    .any(|r| r.session == *session && down(r))
        });
        if let Some(held) = &held {
            for r in &mut mine {
                let was_down = down(r);
                let exited = r.exited_at;
                r.held_again(held, false);
                if was_down && !down(r) {
                    self.back.insert(r.session.clone(), exited);
                } else if was_down && self.back.get(&r.session) == Some(&exited) {
                    r.answer_down();
                }
                r.set_aside(held, false, &self.aside, now);
            }
        }
        if self.back != before {
            write_back(opts, &self.back);
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
            for r in &mut mine {
                if r.unreached_status(held, false, now) || down(r) {
                    follows.push(r.tab.clone());
                }
            }
        }
        follows.sort();
        follows.dedup();
        self.follows = follows;
        for r in &mut mine {
            r.level = r.attention(now).level();
        }
        if self.sent.as_ref() != Some(&mine) || self.sent_kept != kept {
            summary(&mine, &kept);
            self.sent = Some(mine.clone());
            self.sent_kept.clone_from(&kept);
        }
        let mut want = marks(&mine, now);
        for tab in &kept {
            match self.badged.get(tab) {
                Some(badge) => {
                    want.insert(tab.clone(), badge.clone());
                }
                None => {
                    want.remove(tab);
                }
            }
        }
        self.sock.clone_from(&opts.sock);
        self.reconcile(opts, tabs, &want, &kept);
        self.mark(opts, &want);
        next
    }

    /// Lowers `owner=upgrade` marks this view did not raise.
    fn reconcile(
        &mut self,
        opts: &Opts,
        tabs: &[LiveTab],
        want: &BTreeMap<String, String>,
        kept: &[String],
    ) {
        self.settled
            .retain(|tab| tabs.iter().any(|t| t.sid == *tab));
        let due: Vec<String> = tabs
            .iter()
            .map(|t| t.sid.clone())
            .filter(|tab| {
                !self.settled.contains(tab)
                    && !want.contains_key(tab)
                    && !self.badged.contains_key(tab)
                    && !kept.contains(tab)
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

    /// The tabs whose worker the window's host parks behind another supervisor's claim.
    pub fn set_claimed(&mut self, tabs: &[String]) {
        self.aside.claimed = tabs.iter().cloned().collect();
    }

    /// The tabs whose Autopilot is Off (`aterm-gui`'s `harness_host`).
    pub fn set_off(&mut self, tabs: &[String]) {
        self.aside.off = tabs.iter().cloned().collect();
    }

    /// The tabs whose agent its own switch holds.
    pub fn set_agent_off(&mut self, tabs: &[String]) {
        self.aside.agent_off = tabs.iter().cloned().collect();
    }

    /// Tabs whose row follows its agent's live status ([`Row::unreached_status`]).
    #[must_use]
    pub fn follows(&self) -> &[String] {
        &self.follows
    }

    /// The records' folder this view cannot list.
    #[must_use]
    pub fn unlisted(&self) -> Option<&std::path::Path> {
        self.unlisted.as_deref()
    }

    /// The records this view cannot READ that keep a live tab as it stands (host-8).
    #[must_use]
    pub fn unreadable(&self) -> &[std::path::PathBuf] {
        &self.unreadable
    }

    /// Tabs due a watch per the last look.
    #[must_use]
    pub fn watch_due(&self) -> &[String] {
        &self.watch
    }

    /// A look of this view's host at monotonic `mono` and unix second `now`.
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

    /// The host stood down (the switch is off, or it is shutting down).
    pub fn stand_down(&mut self, summary: &mut dyn FnMut(&[Row])) {
        if self.sent.as_ref().is_some_and(|rows| !rows.is_empty()) {
            summary(&[]);
        }
        self.sent = None;
        self.sent_kept.clear();
        self.follows.clear();
        self.watch.clear();
        self.placed.clear();
        self.unlisted = None;
        self.unreadable.clear();
        self.looked = None;
        self.watching_since = 0;
        self.aside = Aside::default();
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
                usage_credits: false,
                usage_tap: None,
                instances: crate::harness::upgrade_drive::Instances::Machine,
                agents_off: Vec::new(),
                land: false,
                timings: crate::harness::Timings::for_build(),
            };
            self.mark(&opts, &BTreeMap::new());
        }
    }

    /// Brings the tabs' `owner=upgrade` attention to `want`.
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

/// Each tab's row (lc-9): `rows` less every row its tab has a higher [`Row::tab_rank`].
#[must_use]
pub fn tab_rows(rows: &[Row]) -> Vec<Row> {
    rows.iter()
        .filter(|r| r.tab.is_empty() || tab_row(rows, &r.tab).is_some_and(|t| std::ptr::eq(t, *r)))
        .cloned()
        .collect()
}

/// Each live tab's `owner=upgrade` mark.
fn marks(rows: &[Row], now: u64) -> BTreeMap<String, String> {
    tab_rows(rows)
        .into_iter()
        .filter_map(|r| {
            let badge = r.badge(now)?;
            Some((r.tab, badge))
        })
        .collect()
}

/// The unix second after `now` at which one of `rows` next reads otherwise by time alone.
fn next_change(rows: &[Row], now: u64) -> Option<u64> {
    rows.iter()
        .flat_map(|r| {
            let waiting = matches!(r.phase, Phase::Pending | Phase::Announced { .. });
            let owed = waiting || matches!(r.phase, Phase::Failed(_));
            let under_way = match r.phase {
                Phase::Exiting { at_s } | Phase::Relaunched { at_s } => Some(at_s),
                _ => None,
            };
            let behind = r.behind_since > 0 && r.asks_on_its_own(now);
            [
                under_way.map(|at_s| at_s.saturating_add(STALE_S + 1)),
                waiting.then(|| r.behind_since.saturating_add(STALLED_AFTER_S)),
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
                r.person_unseen_at().filter(|_| waiting),
                r.next_attention_at(now),
                (r.watch_at != 0).then_some(r.watch_at),
                r.person_hold_stands_at(),
                (owed
                    && r.refused > 0
                    && r.refused_at != 0
                    && r.wait.starts_with("announce-refused:"))
                .then(|| r.refused_at.saturating_add(TELL_S)),
                r.limit_told_at().filter(|_| owed),
                r.rearmed_hold_stands_at(),
                r.behind_said
                    .is_some()
                    .then(|| r.age_from(now))
                    .flatten()
                    .and_then(|since| behind_words_change_at(since, now)),
                r.watch_owed()
                    .then(|| unwatched_turns_at(r.watch_at, r.looked_at, r.awake_since))
                    .flatten(),
            ]
        })
        .flatten()
        .filter(|t| *t > now)
        .min()
}

/// The `HarnessUpgradeProgress` model's variables for one record at `now`.
#[cfg(any(test, debug_assertions))]
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
    let cause = match row.overdue_class(now) {
        WaitClass::OwnWork => 0,
        WaitClass::Person => 1,
        WaitClass::Limit => 2,
        WaitClass::TabUnread
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

/// Where the owner's word is announced to the window's host.
#[must_use]
pub fn word_marker(state: &Path) -> std::path::PathBuf {
    state.join("upgrade").join("owner").join("word")
}

/// Replaces the [`word_marker`] (new file renamed over the old, so its identity changes).
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

#[cfg(all(test, unix))]
#[path = "upgrade_status_tests.rs"]
mod tests;

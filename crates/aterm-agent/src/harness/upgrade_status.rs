// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE OWNER'S VIEW OF THE LIVE UPGRADE, AND THE OWNER'S WORD ON IT (gap audit
//! 2026-09-24). Measured that day in the 0.93.0 window: in 8h22m nothing
//! moved — two sessions, one and two releases behind, answered
//! `step=wait:not-idle` every minute — and the owner could see none of it:
//! `aterm ctl messages` held no harness record, both tabs read `attention=-`,
//! and `aterm harness ledger upgrade` was refused. The only choices were
//! quitting Claude by hand (the harness then never records the outcome) or
//! switching the whole feature off.
//!
//! Everything here is READ FROM or WRITTEN TO the per-conversation state files
//! the upgrade already keeps — never a second record of the same thing:
//!
//! * [`rows`] — one [`Row`] per upgrade record (the relaunch's records, filed
//!   beside them, are not upgrades): the phase, the target, how long the
//!   session has been behind, the wait and since when, the owner's word, and a
//!   finished restart's outcome line. `aterm harness upgrade --status` prints
//!   them; the window's host thread hands the rows of ITS OWN tabs to the
//!   window whenever they change ([`View`]), which shows them as the
//!   `upgrade=` column, a band record, and — only when [`Row::stall`] says the
//!   upgrade will not move on its own — a row.
//! * the tab's mark: the host raises `meta set attention owner=upgrade` on a
//!   STALLED tab and lowers it when that ends ([`View`]). Never merely for
//!   pending, which is the healthy state (a session waits for its turn end).
//! * [`ask`] — the owner's word ([`Request`]), written under the SAME sweep
//!   lock every step holds, so a request can never be lost to a step's
//!   read-modify-write of the same file — and a push to the window's host
//!   ([`word_marker`]), whose worker for that tab takes it at the session's
//!   next idle point.
//!
//! A STATE FILE OUTLIVES WHAT IT DESCRIBES (review of 2026-09-25): a step
//! answers a session already on its target `current` without touching its
//! file, and a conversation that ends leaves its file behind — so a row read
//! off the files alone showed a `Pending` six hours old as a stall, with a
//! Warn row and a tab mark, on a tab whose Claude was current or gone. What
//! the owner is shown or counted is therefore VETTED against the live
//! processes ([`Row::standing`], [`Holder`]): a conversation still held, in
//! its tab, on a build older than its target.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use aterm_json::{Map, Value};

use super::{
    LiveTab, Opts, Phase, Report, Request, STALE_S, St, Version, connect, conversation_live,
    ledger, load, now_s, process_group, save, session_files, state_dir, sweep_lock, tab_is_live,
    unique_tab_for_group, upgrade,
};

/// How long a session may be behind before its upgrade reads STALLED on its
/// own ([`Row::stall`], `overdue`): the ladder's Land rung
/// ([`upgrade::RUNG_LAND_S`], two hours — from it the upgrade moves at the
/// first pause) and a grace past it ([`LAND_GRACE_S`]). Until then the
/// upgrade is WORKING — each rung passes more than the last, and only its
/// floors (a turn running, a draft, a dialog, a keystroke) hold it — so it is
/// a record, never a warning (the owner, 2026-09-28: "What is this alert about
/// 'Codex upgrade waits in tab 1'??? … do I need to do something? It's not
/// clear. I want upgrades to be applied automatically."). Past it, what holds
/// it is a floor that has stood for hours, and the owner is shown what it is
/// and what moves it. Six hours, as before the ladder: Claude Code ships about
/// daily (measured 2026-09-24), so a session six hours behind is a day from
/// being two builds behind.
pub const STALLED_AFTER_S: u64 = upgrade::RUNG_LAND_S + LAND_GRACE_S;

/// The grace past the ladder's Land rung before an upgrade that still has not
/// moved reads `overdue` ([`STALLED_AFTER_S`]): four hours of first pauses
/// none of which came.
pub const LAND_GRACE_S: u64 = 4 * 3_600;

const _: () = assert!(STALLED_AFTER_S == 6 * 3_600);

/// WAITS THE LADDER CANNOT PASS AT ANY RUNG, and that no person's pause ends
/// either ([`Row::stall`], `blocked:<wait>`): the tab's shell integration
/// not reaching aterm where a daemon-mode Codex's `/exit` needs its marks to
/// name the thread it resumes and the kernel cannot name it instead
/// (`no-shell-integration`: the owner's tab read `integration=degraded` with
/// no blocks at 15:35Z on 2026-09-28, after an aterm update; the kernel names
/// it where every thread of the daemon hangs from one root and this Codex is
/// its only client),
/// and a screen the upgrade cannot read at all (`screen-unreadable`). Each is
/// a Warn row once it has blocked the move [`BLOCKED_AFTER_S`] — timed from
/// the first look that met it (`St::blocked_since`), through any other wait
/// a look finds meanwhile, until a look finds it gone — with the one thing
/// that moves it.
pub const BLOCKERS: [&str; 2] = ["no-shell-integration", "screen-unreadable"];

/// How long one of the [`BLOCKERS`] must have blocked the move before it is
/// shown as `blocked` ([`Row::stall`]): a re-ask's interval, so one look that
/// caught a tab mid-handoff is never a warning.
pub const BLOCKED_AFTER_S: u64 = upgrade::REASK_S;

/// How long a finished restart stays in the host's summary: long enough for
/// the window to record it once, and across a handoff.
const DONE_SHOWN_S: u64 = 10 * 60;

/// How long a move that FAILED AFTER ITS EXIT — a Codex `/exit`, a Claude
/// Code SIGTERM — stays in the host's summary — its band row, its `upgrade=`
/// column, its tab's mark — once no process is left to vet it by
/// ([`Row::exited_at`]): a day, so an owner away overnight still finds it,
/// and not for good (the ledger keeps it).
pub const EXITED_FAILURE_SHOWN_S: u64 = 24 * 3_600;

/// The keyed attention owner the host raises on a stalled tab.
pub const ATTENTION_OWNER: &str = "upgrade";

/// How long the owner's `--now` keeps an overdue upgrade from reading
/// STALLED ([`Row::stall`]): the notice's re-ask interval. A `--now` moves the
/// session at its next turn end; one that has not moved it by then — a wait
/// it does not waive, a session that never reaches an idle point — reads
/// stalled again, so an ineffective word cannot hide a stall for good (review
/// of 2026-09-25: `Request::Now` never lapses, and it lowered the band row
/// and the tab's mark for thirty days in a probe).
pub const NOW_QUIETS_S: u64 = upgrade::REASK_S;

/// How long aterm's OWN failure to type a due notice may stand before the
/// owner is told (design record 2026-09-28, §3.2 C6, the `Harness` class):
/// the notice's fence refused it at every look since
/// ([`Row::refused_notice`]). Ninety minutes: three re-ask intervals' worth of
/// looks, long enough that a repaint or a person's typing has had every chance
/// to end, and never the six hours an overdue move waits for (2026-09-28,
/// s-d3346: refused from 14:50, told only "Couldn't upgrade Claude yet", with
/// no word that aterm itself was what could not type).
pub const TELL_S: u64 = 90 * 60;

/// THE MOVE CLOCK (design record 2026-09-28, §3.2 C2 and C6): how long a
/// session may be behind before an upgrade that asks again on its own
/// ([`Row::asks_on_its_own`]) is no longer only a record, whatever it waits
/// on — a usage limit alone excepted, which ends by itself at its named reset
/// (ruling 307). The asking is bounded per round (four notices, a give-up, a
/// rest, a new round), but nothing bounded the ROUNDS: an agent whose own
/// work never ends (a widowed `tail -f`, a poll loop, a server it started)
/// is asked, given up on and asked again for as long as it runs, the tab on
/// its old build for days with nothing on the glass (round six, F7: tab #1,
/// three days). A day is past a whole ask, give-up and stretched rest cycle,
/// so the upgrade working is never marked, and a day is when a session is
/// two builds behind.
///
/// It is also the WATCH's move clock (`upgrade_drive::watch_at`, design
/// record 2026-09-28 §3.2 C2): the one bound that catches a round that asks,
/// gives up, rests and is re-armed for ever, since every one of those steps
/// is progress to `St::progress_at` — only `St::behind_since`, kept across
/// retargets and rounds, sees the session never move. One constant for both,
/// so the row and the watch turn on the same day (the two were written apart,
/// both at a day, and joined at the merge of 2026-09-29).
pub const MOVE_BUDGET_S: u64 = 24 * 3_600;

const _: () = assert!(MOVE_BUDGET_S > STALLED_AFTER_S);

/// Whether a recorded wait is one only a PERSON can end
/// ([`upgrade::person_hold`]'s `box` and `draft`, and what the notice's gate
/// waits for a person on besides: aterm's hold, a login, Claude's own
/// `waiting` for a question's answer), its release's gate too
/// (`release:<gate>`). The upgrade never types over any of them and never
/// ends one, so its re-ask never comes and its asking never gives up while
/// one stands: nothing of the upgrade's own bounds the wait (round six, F6).
fn person_holds(wait: &str) -> bool {
    let wait = wait.strip_prefix("release:").unwrap_or(wait);
    matches!(
        wait,
        "draft" | "box" | "held" | "login" | "not-idle:waiting"
    )
}

impl Row {
    /// The instant an announced round's person hold ([`person_holds`]) has
    /// KEPT THE RE-ASK FROM HAPPENING: the same word stood a whole
    /// [`upgrade::REASK_S`] ([`Self::wait_since`], kept while the word
    /// repeats) and the last ask is that old too, so the re-ask was due under
    /// it. `None` for any other row or word. A hold that passes before then
    /// — a draft sent, a box answered — was the notice's gate working, and
    /// the round still asks on its own ([`Self::asks_on_its_own`]).
    fn person_hold_stands_at(&self) -> Option<u64> {
        let Phase::Announced { at_s, .. } = self.phase else {
            return None;
        };
        person_holds(&self.wait).then(|| self.wait_since.max(at_s).saturating_add(upgrade::REASK_S))
    }
}

/// ONE PROCESS UNDER THE AGENT THAT HELD THE MOVE at the last look that
/// waited, as the owner is shown it ([`Row::held_by`]): its pid, its name and
/// when it started. Its age is read off `since` at every look, so the same
/// process reads the same row and the window is sent it once. Never what it
/// runs: a command is the agent's own words, quoted to the agent alone
/// ([`upgrade::running_clause`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldBy {
    /// Its pid.
    pub pid: u32,
    /// Its executable's basename (`zsh`, `caffeinate`).
    pub name: String,
    /// When it started (unix seconds).
    pub since: u64,
}

/// ONE CONVERSATION'S UPGRADE as the owner sees it, from its state file.
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
    /// One of the [`BLOCKERS`] that blocks the move (`St::blocked`), and
    /// since when — kept through the other waits a look finds between two
    /// that meet it; empty and `0` while none does.
    pub blocked: String,
    pub blocked_since: u64,
    /// For a person at the tab (`attended`), when a look last recorded that
    /// wait (`0` for any other wait, and unknown for a state an older build
    /// wrote). The person is named only while a look has seen them lately
    /// ([`ATTENDED_SEEN_S`]): the wait's word outlives the point it was said
    /// at, and nothing looks while the session works. Only that wait carries
    /// it, so only that wait's rows change at every look.
    pub wait_seen: u64,
    /// Whether the row's words NAME A PERSON AT THE TAB ([`Self::person_seen`])
    /// as of the look that made it — carried, as [`Self::stalled`] is, so the
    /// moment they stop is a CHANGED row, handed to the window again at the
    /// instant [`View::refresh`] names, whose band then restates the row in
    /// place: neither its stall nor its buttons change then, and until
    /// 2026-09-28 the band kept "someone is using its tab" for as long as the
    /// stall stood (the review of that day).
    pub person_named: bool,
    /// The owner's word.
    pub request: Request,
    /// When the owner's word was written (unix seconds; `0`: none recorded).
    pub request_at: u64,
    /// A finished restart's outcome line ([`upgrade::restart_outcome`]).
    pub outcome: String,
    /// When the restart finished (`0`: not done).
    pub done_at: u64,
    /// Until when the model its typed carry-on's answer names is waited for
    /// ([`St::confirming`], `relaunch::MODEL_WAIT` after the carry-on; `0`:
    /// nothing owed). A done row with no `done_at` reads
    /// `restarting/<to>/continued/<age>` UNTIL THEN, and `done` aged from
    /// then after it ([`Self::restarting`], [`Self::finished_at`]) — N2 of
    /// the live re-test of 2026-09-26: such a row was dropped as a move
    /// finished long ago, and the column read blank for the 24 s between the
    /// carry-on and its answer; and the model is read at the next idle point
    /// after the answer, so "until it is read" held `restarting` through a
    /// whole carry-on turn — forty minutes of one read forty minutes of a
    /// restart the new build had finished in its first seconds (the review
    /// of the N2 fix).
    pub confirm_by: u64,
    /// [`Row::stall`] at the moment the row was read — carried so a summary
    /// that crosses [`STALLED_AFTER_S`] differs from the last one and is sent.
    pub stalled: Option<String>,
    /// The model the announcement asked for by the model rule
    /// ([`crate::harness::upgrade_models`]), empty when it asked for none or
    /// has not been typed yet — what [`Row::move_words`] names.
    pub model: String,
    /// Which agent the upgrade moves: Claude Code, or Codex
    /// ([`super::codex`], filed per tab as `codex-<tab>`).
    pub agent: upgrade::Agent,
    /// When the agent the move ended was seen gone — the TUI a Codex `/exit`
    /// was typed into, the Claude Code a SIGTERM was sent to (unix seconds;
    /// `0`: it lives, or nothing ended it). A move that failed after it — no
    /// hint to resume, a relaunch refused or never come up, a stale exit —
    /// has no process left to hold it, so it is not vetted against one
    /// ([`Self::standing`]): until 2026-09-26 every such Codex failure, and
    /// until 2026-09-27 every such Claude Code one (S1 of the in-flight
    /// review), was dropped from `--status`, the band and the `upgrade=`
    /// column, and the owner learned of it from the ledger alone.
    pub exited_at: u64,
    /// What ran under the agent at the last look whose step waited
    /// ([`HeldBy`], [`St::held_by`]): empty when nothing did, and once a
    /// step acted.
    pub held_by: Vec<HeldBy>,
    /// When a STOPPED round starts the next one (unix seconds): the stop's
    /// stamp plus [`upgrade::RETRY_S`] (`St::failed_at`; the owner,
    /// 2026-09-27: "you should NEVER have upgrades stalled"). `0` for a round
    /// that has not stopped. A stop an older build recorded carries no stamp
    /// and is due at once. What `--status` says as `next_round=` instead of a
    /// permanent `gave-up`.
    pub retry_at: u64,
    /// Why the round before this one stopped, while this one — started by a
    /// re-arm — has typed no notice yet (`St::last_stop`). A REFUSAL there
    /// (not a shell job, flags that cannot be carried, a line that cannot be
    /// typed) keeps its stall through the new round's first look, where it is
    /// met again or not: without it the band row resolved at every re-arm and
    /// a new one was posted a minute later, every [`upgrade::RETRY_S`].
    pub last_stop: String,
    /// THE SAME STOP IN A ROW (`St::stop_streak`, `St::streak_why`): how many
    /// rounds in a row stopped for [`Self::streak_why`]. A first stop the
    /// harness asks again after is the upgrade working (a record); a stop
    /// that repeats is the person's to see ([`Self::repeating_stop`], ruling
    /// 283).
    pub stop_streak: u32,
    pub streak_why: String,
    /// A round that GAVE UP holding a late READY its last look heard
    /// (Claude Code's lane, `St::ready_since`): it acts on the answer — the
    /// restart, or its void — and starts no new round while it stands
    /// ([`upgrade::retry_due`]), so no next round is due, however long it has
    /// rested ([`Self::next_round_in`]; the no-stall review of 2026-09-27:
    /// `next_round=due` stood over such a round).
    pub late_ready: bool,
    /// THE RELEASE STILL OWED (`St::release`): why the upgrade abandoned a
    /// notice its agent was given without restarting it (`gave-up`, `void`,
    /// `skipped`, `deferred`, `retargeted`, a stop's reason), while the line
    /// that tells the agent to carry on is not typed yet; empty when nothing
    /// is owed, and for a Codex row (its lane types no release). The one
    /// piece of state that means an agent the upgrade asked to wind down is
    /// stopped NOW (L5 of the upgrade's leftovers, 2026-09-28: it showed only
    /// as an undocumented `wait=release:<gate>` while its own gate held it).
    /// `--status` says it (`release=`, just before the ladder's `rung=`),
    /// `--json` as `release`, and the
    /// words of a round that gave up say the line is owed.
    pub release: String,
    /// A CODEX GOAL THE MOVE PAUSED (the tab's goal record,
    /// `super::super::goal_hold`; the owner's decision of 2026-09-28): when
    /// the pause was made, while the upgrade still owes the goal its resume;
    /// `0` otherwise. The waiting record then says so, and that nothing is
    /// to be done.
    pub goal_held_since: u64,
    /// Since when that goal counts as LEFT PAUSED
    /// (`upgrade_codex::goal_left_since`): its resume could not be made — a
    /// real row, with the one hand step (`/goal resume` in the tab)
    /// ([`Row::stall`] `goal-paused`). `0` otherwise.
    pub goal_left_since: u64,
    /// Until when a hold that ended WITHOUT its move keeps the next pause
    /// off (`upgrade_codex::goal_rest_until`): the goal was resumed, the move
    /// did not come, and it is tried again then. `0` otherwise.
    pub goal_rest_until: u64,
    /// When an announced round's latest notice was typed (unix seconds;
    /// `0`: not announced, or a record older than the stamp). NOT the
    /// phase's `at_s`, which a usage limit's episode and a void's release
    /// move on with nothing asked (review of 2026-09-28): the time the
    /// owner is told the move was "last asked" is this one, and the re-ask's
    /// due time is `at_s`'s.
    pub noticed_at: u64,
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
    /// THE NOTICE REFUSED, IN A ROW (`St::refused`): how many looks since the
    /// upgrade's last act could not type the notice due there, its own fence
    /// refusing it, and when the first was ([`Self::refused_at`]; `0`: none).
    /// Past [`TELL_S`] the move reads overdue whatever its age
    /// ([`Self::refused_notice`]), and the owner is told aterm could not type
    /// it — how often, and since when.
    pub refused: u32,
    pub refused_at: u64,
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
            behind_since: st.behind_since(),
            wait: st.wait.clone(),
            wait_since: st.wait_since,
            blocked: st.blocked.clone(),
            blocked_since: st.blocked_since,
            wait_seen: st.wait_seen,
            request: st.request_for(&st.tab),
            request_at: st.request_at,
            outcome: st.outcome.clone(),
            done_at: st.done_at,
            confirm_by: if st.confirming() { st.confirm_by } else { 0 },
            stalled: None,
            person_named: false,
            model: st.model_list.clone(),
            agent: st.agent,
            exited_at: st.exited_at,
            held_by: st.held_by.clone(),
            retry_at: if matches!(st.phase, Phase::Failed(_)) {
                st.failed_at.saturating_add(upgrade::RETRY_S)
            } else {
                0
            },
            last_stop: st.last_stop.clone(),
            stop_streak: st.stop_streak,
            streak_why: st.streak_why.clone(),
            // A Codex round that gave up hears no late READY (its lane's rule),
            // whatever READY clock its announced round left behind.
            late_ready: st.agent == upgrade::Agent::Claude
                && matches!(&st.phase, Phase::Failed(why) if why == upgrade::GAVE_UP)
                && st.ready_since != 0,
            // Only Claude Code's lane types a release (`upgrade_drive::release`).
            release: if st.agent == upgrade::Agent::Claude {
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
            progress_at: st.progress_at,
            looked_at: st.looked_at,
            looked_by: st.looked_by.clone(),
            point_at: st.point_at,
            guard: st.guard.clone(),
            watch_at: super::watch_at(st).unwrap_or(0),
            refused: st.refused,
            refused_at: st.refused_at,
        };
        row.stalled = row.stall(now);
        row.person_named = row.person_seen(now);
        row
    }

    /// This row with the tab's CODEX GOAL RECORD `hold` read into it
    /// ([`Self::goal_held_since`], [`Self::goal_left_since`],
    /// [`Self::goal_rest_until`]) at `now`, its stall read again. `switch`:
    /// a save-then-wait switch is open on the tab (its loop's ledger).
    ///
    /// A goal is never said LEFT PAUSED while aterm itself still holds it as
    /// it should (the goal-pause review of 2026-09-28: the row told the person
    /// to type `/goal resume` — undoing the move, or running the goal on the
    /// cheaper model near its limit): while a switch is open on the tab (the
    /// session is the switch's until its reset, and the switch resumes the
    /// goal then), and while the look's own wait is the hold working — the
    /// paused goal's last turn still running (`goal-held`), a resume just made
    /// (`goal-resuming`), the switch (`switch`, `goal:switch`).
    #[must_use]
    pub fn with_goal(
        mut self,
        hold: Option<&super::super::goal_hold::Hold>,
        switch: bool,
        now: u64,
    ) -> Row {
        use super::super::goal_hold::Owner;
        use super::super::upgrade_codex as cx;
        if let Some(h) = hold.filter(|_| self.agent == upgrade::Agent::Codex) {
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
                // Left paused ON PURPOSE — its thread fell into a sandbox,
                // and only a person's relaunch brings it back out: a row at
                // once.
                h.last_at().max(1)
            } else {
                cx::goal_left_since(h, now).unwrap_or(0)
            };
            self.goal_rest_until = cx::goal_rest_until(h, now).unwrap_or(0);
        }
        self.stalled = self.stall(now);
        self
    }

    /// WHAT HELD THE MOVE at the last look that waited ([`Self::held_by`]),
    /// in the owner's words at `now`: `pid 63492 (zsh, 5d4h); pid 63493
    /// (zsh, 5d4h)`, at most [`upgrade::HELD_NAMED`] named and the rest
    /// counted ([`upgrade::held_list`]) — no command. `None` when nothing
    /// did.
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
    /// `Some(0)` when it is due, `None` for a round that has not stopped,
    /// that the owner's word holds (a skip is not re-armed; a deferral is,
    /// once it runs out), or that gave up holding a late READY it acts on
    /// ([`Self::late_ready`]: no new round starts while the answer stands).
    #[must_use]
    pub fn next_round_in(&self, now: u64) -> Option<u64> {
        if !matches!(self.phase, Phase::Failed(_)) || self.owner_holds(now) || self.late_ready {
            return None;
        }
        Some(self.retry_at.saturating_sub(now))
    }

    /// When the upgrade reaches the ladder's LAND rung (unix seconds): from
    /// then it moves at the first pause — nothing typed within
    /// [`upgrade::KEYS_GAP_S`], no draft, no dialog, no turn running (the
    /// owner's decision of 2026-09-28). What the waiting record tells the
    /// owner, on their clock. `None` where the start is not known.
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

    /// Whether the OWNER'S WORD IS IN FORCE on this upgrade at `now`: it
    /// holds it ([`Self::owner_holds`]), or a `--now` still quiets its stall
    /// ([`NOW_QUIETS_S`]). What the window reads a stall that ended under the
    /// word by: the word's own record says what it did, and the stall is no
    /// move to call done (the review of 2026-09-28).
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

    /// Whether the upgrade will move this session onto the target at a turn end
    /// with nobody's help: under way or waiting, not held by the owner's word,
    /// and not stalled by anything but its age (an `overdue` session still
    /// moves at its next turn end; a held-back, refused or failed one does
    /// not). A round that stopped and rests until its next
    /// ([`Self::retry_at`]) is NOT moving: it moves only after that rest, a
    /// new notice and a READY — never "at its next turn end".
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

    /// WHY THIS UPGRADE WILL NOT MOVE ON ITS OWN, one word, or `None` while it
    /// is healthy or the owner holds it: `refused:<what>` (a launch that cannot be resumed, or not
    /// its shell's job — while it is stopped, and through the first look of
    /// the round a re-arm starts after it, [`Self::last_stop`]),
    /// `failed:<why>` (a restart that stopped),
    /// `held-back:<owner>` (the agent runs under a multiplexer or another pty
    /// the tab's typing does not reach), `blocked:<wait>` (one of the
    /// [`BLOCKERS`], stood [`BLOCKED_AFTER_S`]), `stuck:<what>` — a restart
    /// under way that has not moved for [`STALE_S`] ([`Self::stuck`]) — and
    /// `overdue` — behind for
    /// [`STALLED_AFTER_S`] or more (the ladder's Land rung and a grace past
    /// it), whatever it waits on, UNLESS the owner's
    /// `--now` was given within [`NOW_QUIETS_S`]: the owner has acted (review
    /// of 2026-09-25: a session already hurried still read `overdue`, the band
    /// told the owner to run the `--now` they had just run, and a gave-up
    /// session re-armed by that very `--now` stalled again at once under a new
    /// row and a new badge). BOUNDED: a `--now` that has not moved the session
    /// by then reads `overdue` again — the word never lapses, and an
    /// ineffective one hid the stall for good. Pending by itself is NOT
    /// stalled: waiting for a turn end is the upgrade working. NOR IS A ROUND
    /// THAT GAVE UP (the owner, 2026-09-27: "you should NEVER have upgrades
    /// stalled"): it rests until its next round ([`Self::retry_at`],
    /// `next_round=`), and the round after it asks again on its own — until
    /// that day `gave-up` was reported here as a stall, for good. It still
    /// reads `overdue` once it is that far behind, as every round does.
    #[must_use]
    pub fn stall(&self, now: u64) -> Option<String> {
        // A CODEX GOAL THE MOVE PAUSED AND COULD NOT RESUME (the owner's
        // decision of 2026-09-28: aterm never leaves the goal paused): a row
        // whatever the move's own state and the owner's word on it — one
        // hand step moves it (`/goal resume` in the tab).
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
        // Behind for STALLED_AFTER_S or more, whatever it waits on — a round
        // resting after it gave up included, so a long-behind session reads the
        // same word through every round and the band row does not flap.
        // So is a notice aterm itself could not type for TELL_S
        // (`refused_notice`): its own failure is told within the hour and a
        // half, not the six a move's age waits for.
        let overdue = ((now.saturating_sub(self.behind_since) >= STALLED_AFTER_S
            && !self.hurried(now))
            || self.refused_notice(now))
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
                    // A wait no rung passes and no pause ends (the owner's
                    // decision of 2026-09-28: a warning only where something
                    // is really broken).
                    Some(format!("blocked:{}", word(blocker)))
                } else if self.repeating_stop() {
                    // A STOP THAT REPEATS (ruling 283) stays the stall through
                    // the re-armed round, until it moves (a restart in flight,
                    // Done) or stops for another reason: the band keeps ONE
                    // entry for it instead of resolving it at every re-arm and
                    // posting a new one at the next stop.
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

    /// A RESTART UNDER WAY THAT HAS NOT MOVED ([`stuck_on`]), from the row's
    /// phase and when its agent was seen gone — the one predicate the sweep's
    /// once-only ledger note reads too (`St::stuck`), so the ledger and
    /// `--status` never disagree.
    fn stuck(&self, now: u64) -> Option<&'static str> {
        stuck_on(&self.phase, self.exited_at, now)
    }

    /// How long this session has been behind at `now`, in a person's words
    /// (`40 min`, `26 h`, `3 days`: the band's own elapsed words) — what the
    /// window's one row for an agent's stalled tabs names each by, most behind
    /// first. `None` where the start is unknown.
    #[must_use]
    pub fn behind_for(&self, now: u64) -> Option<String> {
        behind_words(self.behind_since, now)
    }

    /// ATERM'S OWN FAILURE TO TYPE A DUE NOTICE, standing past [`TELL_S`]
    /// (design record 2026-09-28, C6's `Harness` class): the last look's wait
    /// is a notice its fence refused (`announce-refused:<why>`), and every look
    /// that could type it since [`Self::refused_at`] could not. Told whatever
    /// the move's age and the owner's `--now` — a word that asked for the very
    /// notice aterm cannot type hides nothing.
    #[must_use]
    pub fn refused_notice(&self, now: u64) -> bool {
        self.wait.starts_with("announce-refused:")
            && self.refused > 0
            && self.refused_at != 0
            && now.saturating_sub(self.refused_at) >= TELL_S
    }

    /// Whether the notice due is one ATERM'S OWN FENCE refuses
    /// (`announce-refused:<why>`) while the owner's `--now` is in force: the
    /// word already asked for it ([`Self::remedy`]: `Upgrade now` is no remedy
    /// then).
    #[must_use]
    pub fn fence_fails_under_now(&self) -> bool {
        self.wait.starts_with("announce-refused:") && self.request == Request::Now
    }

    /// Whether the owner's `--now` still quiets an overdue stall at `now`:
    /// in force, and given within [`NOW_QUIETS_S`] ([`Self::stall`]).
    fn hurried(&self, now: u64) -> bool {
        self.request == Request::Now && now < self.request_at.saturating_add(NOW_QUIETS_S)
    }

    /// What the stall means, in a person's words ([`Self::stall`]); `None`
    /// while it is not stalled. An `overdue` stall says how long and what it
    /// waits on — words that move with every step, so they are for a record
    /// read once, never for the tab's mark ([`Self::badge`]). NEVER THE WAIT'S
    /// OWN WORD (round 18, day four, D3: the band read `waiting
    /// (not-idle:busy)` and `waiting (background)`): a wait with no words of
    /// its own is left out here — `--status` and the `upgrade=` column carry
    /// the word for a reader who wants it.
    #[must_use]
    pub fn stall_words(&self, now: u64) -> Option<String> {
        let stall = self.stall(now)?;
        // How long, in a person's words (`behind for 8 h`, never `8h28m`),
        // and only where the start is known (design ruling 308: a start of 0
        // printed `behind for 20721d4h`, the time since 1970).
        let behind = behind_words(self.behind_since, now).map(|b| format!("behind for {b}"));
        let with = |what: &str| match &behind {
            Some(b) => format!("{b}: {what}"),
            None => what.to_string(),
        };
        Some(match stall.as_str() {
            // A round that gave up and then heard a late READY acts on that
            // answer ([`Self::late_ready`]): no rest, and no asking again,
            // while it stands.
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
            "overdue" if matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) => {
                with(if self.release.is_empty() {
                    "it has not agreed to the move yet, so the upgrade rests, then asks again"
                } else {
                    "it has not agreed to the move yet and is owed the line that tells it to \
                     carry on; the upgrade rests, then asks again"
                })
            }
            // ATERM COULD NOT TYPE ITS NOTICE: said as aterm's own, with how
            // often and for how long (design record 2026-09-28, C6).
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
            kind => stall_reason(self.agent, kind),
        })
    }

    /// WHETHER THE UPGRADE IS STILL ASKING ON ITS OWN: an overdue move whose
    /// agent was told and is asked again every half hour
    /// ([`Phase::Announced`]), waiting on what the owner's `--now` does not
    /// waive ([`Remedy::Waits`]). It moves once that ends; a round whose asking
    /// goes unanswered rests and the next round asks again ([`Self::retry_at`])
    /// — so it is the upgrade working, not a thing for the owner to move (round
    /// 18, day four, D5:
    /// it stood as a warn row with a tab mark beside it). The window records
    /// it and marks nothing ([`Self::badge`]).
    ///
    /// A ROUND THAT STOPPED AND WILL ASK AGAIN ON ITS OWN is the same (the
    /// owner, 2026-09-27: "you should NEVER have upgrades stalled"; ruling
    /// 283): one that gave up — the agent gave no READY it could act on —
    /// and the FIRST stop of any other reason that is not a refusal (which a
    /// new round meets again) nor a move whose agent is gone — a Codex's
    /// `/exit`, a Claude Code's SIGTERM ([`Self::failed_after_exit`]: nothing
    /// runs in the tab to ask). Each
    /// rests until [`Self::retry_at`] and the next round asks again. A stop
    /// that repeats ([`Self::repeating_stop`]) is no longer the upgrade
    /// working: a row.
    ///
    /// A ROUND RE-ARMED AFTER A GIVE-UP, ITS AGENT'S OWN WORK STILL HOLDING IT
    /// (2026-09-28, s-692e6): the agent answered every notice of the round
    /// before "not yet, my work still runs", and until its next notice goes
    /// the new round waits on that same work — the upgrade working, a record
    /// as the rounds before and after it are, never a fresh warn row at every
    /// round (ruling 283's last sentence gave one at each re-armed pending
    /// stretch, and it came and went). A notice aterm itself could not type is
    /// never hidden so ([`Self::refused_notice`]). The move itself comes at the
    /// end of that work (`upgrade::Facts::work_ended`).
    ///
    /// NOT WHILE A PERSON HOLDS IT (round six, F6): an announced round whose
    /// notice waits on a draft, a box, aterm's hold, a login or a question
    /// ([`person_holds`]) is never asked again — the re-ask is typed only
    /// under the gate that holds it — so it never gives up either, and
    /// nothing but the person ends the wait. Read as the upgrade working, it
    /// sat on no glass for days, with no mark saying who could move it: a
    /// row, once it is overdue.
    ///
    /// NOT PAST THE MOVE CLOCK (round six, F7; [`MOVE_BUDGET_S`]): a session
    /// a day behind is no longer only a record, whatever its rounds do —
    /// ask, give up, rest and ask again for as long as the agent's own work
    /// runs — except at a usage limit, which ends by itself (ruling 307).
    ///
    /// A HOLD THAT HAS STOOD, never a passing word ([`Self::person_hold_stands_at`],
    /// review two): the notice's gate records `draft`, `held`, a box at
    /// whatever look meets one — a person typing their next prompt at an
    /// idle look — and the next look's `busy` ends it. Read as a person's
    /// hold at once, each such look turned the record into a row with a mark
    /// and back.
    #[must_use]
    pub fn asks_on_its_own(&self, now: u64) -> bool {
        if self.past_move_budget(now) {
            return false;
        }
        match &self.phase {
            Phase::Announced { .. } => {
                self.remedy(now) == Some(Remedy::Waits)
                    && self.person_hold_stands_at().is_none_or(|at| now < at)
            }
            Phase::Pending
                if self.last_stop == upgrade::GAVE_UP
                    && !self.refused_notice(now)
                    && self.overdue_cause(now) == OWN_WORK =>
            {
                true
            }
            // A USAGE LIMIT ends by itself, before a notice as after one
            // (ruling 307): `limited` holds from the transcript's limit row
            // to its named reset — days, for a weekly limit — and nothing the
            // owner presses moves it sooner, so it is a record whatever the
            // phase, never a warn row with a tab mark that the phase alone
            // flipped.
            Phase::Pending => self.wait == "limited" && self.remedy(now) == Some(Remedy::Waits),
            Phase::Failed(why) => {
                !self.owner_holds(now)
                    && (why == upgrade::GAVE_UP
                        || (!refusal(why) && !self.failed_after_exit() && !self.repeating_stop()))
            }
            _ => false,
        }
    }

    /// Whether the session has been behind for [`MOVE_BUDGET_S`] or more at
    /// `now`, and waits on anything but a usage limit (ruling 307): a start
    /// of 0 (a fixture's, a record's default) is no age.
    fn past_move_budget(&self, now: u64) -> bool {
        self.behind_since > 0
            && now.saturating_sub(self.behind_since) >= MOVE_BUDGET_S
            && self.wait != "limited"
    }

    /// Whether the latest stop is one that REPEATED — the same reason two
    /// rounds or more in a row, not a refusal and not a give-up (ruling 283):
    /// the rest now stretches ([`upgrade::rest_extension`]), and the person
    /// is shown it.
    #[must_use]
    pub fn repeating_stop(&self) -> bool {
        self.stop_streak >= 2
            && !self.streak_why.is_empty()
            && self.streak_why != upgrade::GAVE_UP
            && !refusal(&self.streak_why)
    }

    /// What a Codex wait the owner cannot move by `--now` stands for, in a
    /// person's words: what its daemon waits on (`daemon-first:<why>`, the
    /// daemon's own step carried into the client's wait — review of
    /// 2026-09-26: every client read a bare `daemon-first`, and nothing named
    /// a detached thread holding the daemon for hours), a background terminal
    /// left running, the lane's own text in the composer. `None` for every
    /// other wait, whose word says it.
    ///
    /// A PERSON AT A CLAUDE CODE TAB (`attended`) is named only while a look
    /// has seen them lately ([`ATTENDED_SEEN_S`]). Past that the word is the
    /// last idle point's, and the move waits for the next one: said so. The
    /// band said "someone is typing in its tab" for 5 h 50 min of a turn the
    /// harness itself had continued (2026-09-27 20:28 to 2026-09-28 02:18,
    /// s-d3346): no look is taken while the session works.
    fn wait_words(&self, now: u64) -> Option<&'static str> {
        if self.person_gone(now) {
            return Some("it waits for its next turn end");
        }
        self.recorded_wait_words()
    }

    /// Whether this row's wait is a Claude Code tab's person (`attended`)
    /// whom no look has seen lately ([`ATTENDED_SEEN_S`]): the word is the
    /// last idle point's, and the move waits for the next one. The row's
    /// words ([`Self::wait_words`]) and the tab's mark ([`Self::overdue_cause`])
    /// both read it here, so the two never disagree about who holds the move.
    fn person_gone(&self, now: u64) -> bool {
        self.agent != upgrade::Agent::Codex && self.wait == "attended" && self.attended_unseen(now)
    }

    /// [`Self::wait_words`] as the look that recorded the wait said it, with
    /// no clock: what [`Self::waiting_line`] names, which reads a person at
    /// the tab as the ladder's comfort before it gets here.
    fn recorded_wait_words(&self) -> Option<&'static str> {
        if self.wait.is_empty() {
            return None;
        }
        if self.agent != upgrade::Agent::Codex {
            // The release's own gate's words say a line is OWED: only while
            // one is ([`Self::release`]). A gate's word a look recorded can
            // outlive the release it held (the review of the leftovers,
            // 2026-09-28: a dropped release kept `wait=release:attended`),
            // and nothing owes that line any more.
            if self.wait.starts_with("release:") && self.release.is_empty() {
                return None;
            }
            return claude_wait_words(&self.wait);
        }
        Some(match self.wait.as_str() {
            // ITS OWN CONVERSATION's turn — its root's, or a subagent's it
            // spawned — running in its daemon where its screen may not show
            // it (the kernel names the conversation: every thread of its
            // daemon hangs from that root, and this Codex is its only
            // client).
            "daemon-turn" => {
                "a turn is running in this tab's Codex conversation (its own, or a subagent's it \
                 started)"
            }
            // The same, a GOAL being pursued (its footer's `Pursuing goal`):
            // the wait the owner's decision of 2026-09-28 lets aterm lift, by
            // pausing the goal briefly at the ladder's Land rung.
            "goal" => "a goal is running in this tab's Codex",
            // THE GOAL PAUSE (`upgrade_codex::goal_step`): aterm paused the
            // goal, its last turn still running, or is resuming it.
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
            // A turn the lane cannot place (the own conversation not named):
            // never said as this tab's, never a goal to pause here (the
            // review of 2026-09-28) — and never presumed another session's
            // either (the third review: a subagent of this tab's own
            // conversation is never placed, and one is often what runs).
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
            // A click or a scroll stamps the tab as a keystroke does, in a
            // Codex tab as in a Claude Code one (main's 144d9547a).
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
            w if w.starts_with("left-typed") => {
                "the harness's own text is still in its composer, beside a person's typing"
            }
            // The waits every agent's step says alike (a draft, a box, a
            // hold, a limit, the login wall).
            w => return claude_wait_words(w),
        })
    }

    /// Whether only the LADDER'S OWN COMFORT holds this wait — the settle, a
    /// person near the tab, the agent's own turn at a look, a status its idle
    /// screen does not bear out, nothing recorded yet — and no floor: what
    /// [`Self::waiting_line`] words as the ladder's promise. The window's
    /// waiting record keeps a floor's sentence over it while the upgrade
    /// waits (the second review of 2026-09-28: a person near a goal-mode tab
    /// flipped the record between the two at every look).
    ///
    /// So does a Claude Code release's gate word a look recorded after its
    /// release was dropped (`release:<gate>` with no release owed,
    /// [`Self::recorded_wait_words`]): nothing owes that line, the gate held
    /// only it, and the move is back on the ladder's ordinary course.
    #[must_use]
    pub fn waits_for_comfort(&self) -> bool {
        matches!(
            self.wait.as_str(),
            "" | "settling" | "attended" | "busy" | "not-idle" | "not-idle:busy" | "in-flight"
        ) || self.wait.starts_with("status-stale")
            || (self.agent != upgrade::Agent::Codex
                && self.wait.starts_with("release:")
                && self.release.is_empty())
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
        // THE GOAL PAUSE (the owner's decision of 2026-09-28): a goal holds
        // the move only until the Land rung, where aterm pauses it for a
        // moment and resumes it after the move — nothing for the owner to do.
        if self.agent == upgrade::Agent::Codex {
            if self.goal_held_since != 0 {
                return "aterm paused its goal for a moment to install the new Codex, and \
                        resumes it right after: nothing to do"
                    .to_string();
            }
            if self.goal_rest_until != 0 {
                return "the goal aterm paused to install the new Codex is running again: the \
                        move did not come in time, so aterm resumed it, and tries again later \
                        (nothing to do)"
                    .to_string();
            }
            if self.wait == "goal" || self.wait.starts_with("goal:") {
                return format!(
                    "a goal is running in this tab's Codex; from {from} aterm pauses the goal for \
                     a moment to install the new Codex, and resumes it right after: nothing to do"
                );
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
            "limited" => "it moves once the limit resets",
            "login" => "it moves once it is logged in again",
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

    /// Whether the row's words name A PERSON AT THE TAB at `now`
    /// ([`Self::wait_words`]): a Claude Code tab's attended wait a look has
    /// seen lately.
    #[must_use]
    pub fn person_seen(&self, now: u64) -> bool {
        self.agent != upgrade::Agent::Codex && self.wait == "attended" && !self.attended_unseen(now)
    }

    /// The unix second this row's words stop naming a person at the tab
    /// ([`Self::person_seen`]), if they name one.
    fn person_unseen_at(&self) -> Option<u64> {
        (self.agent != upgrade::Agent::Codex && self.wait == "attended").then(|| {
            self.wait_seen
                .max(self.wait_since)
                .saturating_add(ATTENDED_SEEN_S + 1)
        })
    }

    /// WHAT THE OWNER CAN DO about this stall, by its kind ([`Remedy`]);
    /// `None` while it is not stalled. Until 2026-09-25 the band named `--now`
    /// for every kind, which moves only two of them: a held-back agent is
    /// never reached (the terminal check returns before the word is read) and
    /// a refused or failed one stays stopped. And an `overdue` one is moved by
    /// `--now` only while it waits on what `--now` waives or on the turn end
    /// it moves at ([`now_moves_past`]); one waiting on the agent's READY
    /// answer, a draft, a box, a hold or work under it is told what it waits
    /// on instead ([`Remedy::Waits`]).
    ///
    /// A ROUND THAT GAVE UP is no stall — it rests until its next round — but
    /// the owner's `--now` still re-arms it AT ONCE rather than at
    /// [`Self::retry_at`]: [`Remedy::AskAgain`] is its remedy all the same, so
    /// the words the window offers for it keep `Upgrade now`.
    #[must_use]
    ///
    /// Before its rest ends it reads `overdue` once that far behind, as every
    /// round does, while its step waits `failed` — which `--now` does move:
    /// until ruling 283 that read [`Remedy::Waits`], and the row said
    /// `Upgrade now` could not move it and dropped the capsule that does.
    pub fn remedy(&self, now: u64) -> Option<Remedy> {
        if matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) {
            return (!self.owner_holds(now)).then_some(Remedy::AskAgain);
        }
        let stall = self.stall(now)?;
        Some(match stall.as_str() {
            // The goal left paused: one hand step, `/goal resume` in the tab
            // — or, its thread fallen into a sandbox, the relaunch that
            // brings it out.
            "goal-paused" | "goal-sandboxed" => Remedy::ByHand,
            // ATERM'S OWN FENCE FAILS UNDER THE OWNER'S `--now` (the review of
            // 2026-09-28): the word asked for the very notice aterm cannot
            // type, so pressing it again moves nothing — it would only arm
            // another round the same fence refuses. What the owner can do is
            // hold the move; aterm keeps trying at the agent's turn ends.
            "overdue" if self.fence_fails_under_now() => Remedy::Waits,
            "overdue" if now_moves_past(&self.wait) => Remedy::Now,
            "overdue" => Remedy::Waits,
            s if s.starts_with("held-back:") => Remedy::InItsPane,
            // A restart under way: nothing the owner says moves it
            // ([`ask`] refuses a word then), and nothing is forced.
            s if s.starts_with("stuck:") => Remedy::Waits,
            _ if self.failed_after_exit() => Remedy::ResumeInTab,
            _ => Remedy::ByHand,
        })
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

    /// A move that FAILED after the agent it ended was seen gone — a Codex
    /// `/exit`, a Claude Code SIGTERM ([`Self::exited_at`]): the tab's
    /// record, shown for [`EXITED_FAILURE_SHOWN_S`] by the host. Until
    /// 2026-09-27 only a Codex move's (S1 of the in-flight review).
    #[must_use]
    pub fn failed_after_exit(&self) -> bool {
        self.exited_at != 0 && matches!(self.phase, Phase::Failed(_))
    }

    /// A CLAUDE CODE MOVE THAT FAILED AFTER ITS EXIT, HELD AGAIN: once a live
    /// process holds its conversation — resumed by hand, in its tab or
    /// another, on whichever build — there is a process to vet it by, and it
    /// is an ordinary stopped upgrade again: shown only for a holder on a
    /// build older than its target in its tab ([`Self::standing`]), with the
    /// remedy by hand, and no longer the tab's record of an agent nothing
    /// runs. (A Codex move's holder is the tab's Codex TUI, whose own lane
    /// retires the record once it is current.)
    fn held_again(&mut self, holders: &[Holder]) {
        if self.agent == upgrade::Agent::Claude
            && self.failed_after_exit()
            && holders.iter().any(|h| h.session == self.session)
        {
            self.exited_at = 0;
        }
    }

    /// WHETHER THE OWNER SEES OR COUNTS THIS UPGRADE NOW: a finished move or a
    /// restart in flight always; any other only while a live process holds
    /// its conversation IN ITS TAB on a build OLDER than its target
    /// ([`Holder`]). A conversation that ended, one moved onto the target by
    /// hand, and one resumed in another tab all read `false` — their files
    /// stay (a stopped upgrade is not retried for the same session and
    /// target), but they are no longer anything to show. `unproven_tab`:
    /// whether a holder whose tab no proof read still counts — the CLI's view,
    /// which has no roster, never the host's. A row with NO recorded tab (a
    /// pending state an older build wrote: it recorded the tab only at the
    /// announcement — measured 2026-09-25, both live states on the owner's
    /// machine) stands in whichever tab its holder is proven in
    /// ([`Self::proven_tab`]).
    #[must_use]
    pub(super) fn standing(&self, holders: &[Holder], unproven_tab: bool) -> bool {
        if !self.held_by_a_process() {
            return true;
        }
        self.behind_holders(holders)
            .any(|h| self.held_here(h, unproven_tab))
    }

    /// Whether the live holder `h` holds this row's conversation IN ITS TAB:
    /// proven there (or anywhere, for a row with no recorded tab), or — where
    /// `unproven_tab` — in no tab a proof reads ([`Self::standing`]).
    fn held_here(&self, h: &Holder, unproven_tab: bool) -> bool {
        h.tab
            .as_deref()
            .map_or(unproven_tab, |t| self.tab.is_empty() || t == self.tab)
    }

    /// A ROW NO LOOK HAS REACHED READS CLAUDE'S OWN LIVE STATUS (L2 of the
    /// upgrade's leftovers, 2026-09-28). Only a step records a wait, and the
    /// window steps only at an idle screen or a break of the agent's own
    /// work: a session minted behind at its attach (`note_behind`), or whose
    /// last step acted (its notice typed, which clears the wait), keeps an
    /// EMPTY wait for as long as a question box or a turn of hours stands —
    /// read as waiting on nothing, and offered `Upgrade now` at six hours,
    /// which cannot move a session whose status is not idle (the gate asks
    /// Claude idle, and `--now` waives only the settling window and the
    /// attended tab). So a Claude Code row, pending or announced, whose
    /// recorded wait is EMPTY and whose live holder in its tab (`holders`,
    /// `unproven_tab` as [`Self::standing`]) says a status other than `idle`
    /// reads the word its first look would record (`not-idle:<status>`,
    /// [`upgrade::wait_word`]), since that status began (never before the
    /// session was behind): a box `not-idle:waiting` — [`Remedy::Waits`], `it
    /// asked a question and waits` — a background shell `not-idle:shell`, a
    /// turn `not-idle:busy`, which `--now` still moves at its next turn end.
    /// NEVER OVER A RECORDED WORD: a look's own word stands until the next
    /// look. The owner's view only — `--status` and the window's rows — and
    /// no gate reads it.
    ///
    /// Whether the row FOLLOWS that status — a live holder in its tab, `idle`
    /// or not, and no recorded word: it reads otherwise whenever the status
    /// moves, which no instant names, so the window's host looks at its tab
    /// again whenever that tab's screen moves ([`View::follows`]; the review
    /// of the leftovers, 2026-09-28: a look taken while a box was up kept
    /// "it asked a question and waits" through the whole turn that followed).
    fn unreached_status(&mut self, holders: &[Holder], unproven_tab: bool, now: u64) -> bool {
        if self.agent != upgrade::Agent::Claude
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
            self.stalled = self.stall(now);
        }
        true
    }

    /// The tab a live holder of this conversation, on a build older than its
    /// target, is PROVEN to run in — what a row with no recorded tab is shown
    /// under and what the owner's word may name.
    pub(super) fn proven_tab(&self, holders: &[Holder]) -> Option<String> {
        self.behind_holders(holders).find_map(|h| h.tab.clone())
    }

    /// The live holders of this conversation on a build older than its target
    /// — or, for a SAME-BUILD restart (`from == to`: the build current, a model
    /// move due, [`crate::harness::upgrade_models`]),
    /// on that build: a relaunch with `--model` moves the conversation without
    /// moving its build, so a holder still on it is exactly what it waits for.
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
            // before it reads `restarting` (S2 of the in-flight review).
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
                self.request.word(),
                behind,
            ),
            // A ROUND THAT STOPPED AND ASKS AGAIN ON ITS OWN — resting after
            // it gave up, or after a first stop (ruling 283) — is pending on
            // its next round, however far behind (day five, D20: a 7-hour
            // give-up read `stalled/…/overdue:failed` and a first stop
            // `stalled/…/failed:signal-refused`, beside a band that said it
            // retries later; the owner: "you should NEVER have upgrades
            // stalled"). `--status` keeps the stall word (`stalled=`) and the
            // stop (`phase=`).
            _ if matches!(self.phase, Phase::Failed(_)) && self.asks_on_its_own(now) => {
                ("pending", self.next_round_word(now), behind)
            }
            // An OVERDUE stall names what it waits on (round 18, day four,
            // D17: two tabs, one mid-turn and one on its own work, both read
            // a bare `overdue`).
            _ => match self.stall(now) {
                Some(stall) if stall == "overdue" && !self.wait.is_empty() => {
                    ("stalled", format!("overdue:{}", self.wait), behind)
                }
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

    /// The ladder's rung as `--status` says it (`rung=`): the rung's word
    /// ([`upgrade::Rung::word`]) while the move is owed — `land` under the
    /// owner's `--now`, the last rung at once — and `-` for a restart under
    /// way, a finished one, or one that stopped.
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
        let wait_for = if self.wait.is_empty() {
            "-".to_string()
        } else {
            upgrade::span(now.saturating_sub(self.wait_since))
        };
        let next_round = match self.next_round_in(now) {
            Some(0) => "due".to_string(),
            Some(secs) => upgrade::span(secs),
            None => "-".to_string(),
        };
        // The watch's fields (design record 2026-09-28, §3.2 C8): how long
        // ago a look off a point read the record (`looked=`) and which build
        // (`by=`), how long ago the loop last offered a point (`point=`) and
        // the guard that withheld the latest (`guard=`), and when the record
        // is watched next (`watch_at=`: `due` once past, `-` for a record
        // nothing is owed on).
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
        // New fields go LAST (`held_by=`, then `release=`, then `rung=`,
        // then the watch's, then `refused=` while there is one): a reader
        // that splits the line keeps its positions, and the one field said
        // only sometimes is the line's tail.
        format!(
            "upgrade tab={} session={} from={} to={}({}) phase={} pending_for={} wait={} \
             wait_for={wait_for} request={} next_round={next_round} stalled={} held_by={} \
             release={} rung={} looked={} by={} point={} guard={} watch_at={watch_at}{refused}",
            dash(&self.tab),
            dash(&self.session),
            dash(&self.from),
            dash(&self.to),
            dash(&self.source),
            word(&self.phase.word()),
            upgrade::span(now.saturating_sub(self.behind_since)),
            dash(&self.wait),
            self.request.word(),
            self.stall(now)
                .map_or_else(|| "-".to_string(), |s| word(&s)),
            self.held_token(now),
            dash(&self.release),
            self.rung_word(now),
            ago(self.looked_at),
            dash(&self.looked_by),
            ago(self.point_at),
            dash(&self.guard),
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
        ] {
            o.insert(k.into(), Value::from(v));
        }
        for (k, v) in [
            ("pending_for_s", now.saturating_sub(self.behind_since)),
            (
                "wait_for_s",
                if self.wait.is_empty() {
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
        Value::Object(o)
    }

    /// What the upgrade moves the session onto, in the owner's words:
    /// `Claude Code <from> → <to>` (`Codex <from> → <to>` for a Codex row), `… with <model>` once the announcement
    /// named a model, and — for a SAME-BUILD restart (`from == to`: the build
    /// current, a model move due) — `Claude Code <to> → <model>`, or `→ another
    /// model` before one is named. A same-build move
    /// spelled `2.1.282 → 2.1.282` named no change at all.
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

    /// The tab's attention text for a stalled upgrade (at most 200 bytes, one
    /// line: the keyed attention cap). STABLE FOR AS LONG AS THE STALL'S KIND
    /// IS (review of 2026-09-25): it carried the running age (`behind for
    /// 7h1m`), so every minutely look re-sent `meta set attention` — a
    /// meta-change event and a wake a minute, and, the newest stamp being the
    /// one shown, the upgrade took the tab's attention back from any owner
    /// that raised it later. Neither the age nor the wait is in it now.
    ///
    /// AN OVERDUE MARK NAMES ITS CAUSE, BY CLASS (design record 2026-09-28,
    /// §1.4 and §3.2 C8): it read "behind for more than 6h" alone — true of
    /// tab #1 for three days, and no help: it said neither what held the move
    /// nor who could end it. The class ([`Self::overdue_cause`]) is meant to
    /// move only when what holds the move changes hands (its own work, a
    /// person, a limit, aterm itself), not with every wait word under it:
    /// the notice path's passing words read as the processes under the agent
    /// while any run, so a stall on its own work is sent once, whatever its
    /// gates said at each look.
    fn badge(&self, now: u64) -> Option<String> {
        if self.asks_on_its_own(now) {
            return None;
        }
        let words = match self.stall(now)?.as_str() {
            // Told for aterm's own refusal before six hours behind: the mark
            // says only what holds it, never an age it has not reached.
            "overdue" if self.refused_notice(now) => {
                let cause = "aterm could not type its notice";
                if now.saturating_sub(self.behind_since) >= STALLED_AFTER_S {
                    format!(
                        "behind for more than {}, {cause}",
                        upgrade::span(STALLED_AFTER_S)
                    )
                } else {
                    cause.to_string()
                }
            }
            "overdue" => format!(
                "behind for more than {}, {}",
                upgrade::span(STALLED_AFTER_S),
                self.overdue_cause(now)
            ),
            kind => stall_reason(self.agent, kind),
        };
        let text = format!("{} upgrade stalled: {words}", self.move_words());
        Some(super::super::one_line(&text, 200))
    }

    /// WHAT HOLDS AN OVERDUE MOVE, as a class in the owner's words — the
    /// tab's mark's cause ([`Self::badge`]). Read from the recorded wait (a
    /// release's own gate, `release:<gate>`, by the gate it waits on) and
    /// what ran under the agent at that look ([`Self::held_by`]):
    ///
    /// * the agent's own work — a turn running, a background shell or task,
    ///   processes under it, its status not yet quiet, its READY not given,
    ///   a round resting after its notices went unanswered (`failed`, the
    ///   `gave-up` round's rest: its own work outlasted four notices);
    /// * a person — a box, a draft, a login, a person at the tab, a hold;
    /// * a usage limit, which ends by itself at its reset — and a Codex
    ///   save-then-wait switch, which holds the session until that reset;
    /// * a Codex goal aterm paused and could not resume, waiting on the hand
    ///   step its own row names (ruling 381);
    /// * its Codex daemon — shared with other tabs, restarting, or its
    ///   version not yet read;
    /// * aterm itself — a notice it could not type, notices queued unread,
    ///   a tab or conversation it cannot reach to ask in, a screen or shell
    ///   integration it cannot read, a move already under way;
    /// * a wait this build does not classify, NAMED (§3.1 principle 5: an
    ///   unclassified cause fails loud), and a record no look has given a
    ///   wait at all, said as such.
    ///
    /// THE PROCESSES UNDER IT OUTRANK A PASSING WORD (review of 2026-09-28):
    /// the class came from the last wait word alone, so a long stall on
    /// background work — `background` at a break, `announce-refused:changed`
    /// when the notice's fence refused at an idle point, `attended` when a
    /// person touched the tab — changed its mark's text at each, and every
    /// change re-sent `meta set attention`, taking the tab's attention back
    /// from any owner that raised it since (the churn the 2026-09-25 review
    /// removed). Those words are the notice path's passing gates, not what
    /// holds the move: while processes run under the agent, the move is held
    /// by its own work, and the mark says so through all of them. A standing
    /// box, login or limit still names its own class — each is a hand that
    /// must act. What remains word-read (a turn with nothing under it, then a
    /// draft) is the design record's step 8, which reads the class from
    /// facts, sticky.
    ///
    /// A PERSON NO LOOK HAS SEEN LATELY is no person (the review of
    /// 2026-09-28, the s-d3346 shape: 13 h under an `attended` word): the
    /// row's words say the move waits for the agent's next turn end
    /// ([`Self::wait_words`]), and the mark says its own work with them
    /// ([`Self::person_gone`]) — it read "waiting on a person (a question, a
    /// draft, a login or a hold)", pointing the owner at a question, a draft,
    /// a login or a hold that did not exist. The row changes once at that
    /// instant ([`Self::person_unseen_at`]), so the mark changes once with
    /// it, and never churns.
    fn overdue_cause(&self, now: u64) -> String {
        let wait = self.wait.strip_prefix("release:").unwrap_or(&self.wait);
        let head = wait.split_once(':').map_or(wait, |(head, _)| head);
        // The notice path's passing gates, and a resting round's quiet word.
        let passing = matches!(
            head,
            "" | "attended" | "draft" | "held" | "announce-refused" | "failed"
        );
        if (passing && !self.held_by.is_empty()) || self.person_gone(now) {
            return OWN_WORK.to_string();
        }
        match wait_class(&self.wait) {
            WaitClass::OwnWork => OWN_WORK.to_string(),
            WaitClass::Person => {
                "waiting on a person (a question, a draft, a login or a hold)".to_string()
            }
            WaitClass::Limit => "paused by a usage limit until its reset".to_string(),
            WaitClass::GoalLeftPaused => {
                "its paused goal waits for a person to resume it".to_string()
            }
            WaitClass::SharedDaemon => {
                "waiting on its Codex daemon, shared with other tabs".to_string()
            }
            WaitClass::Daemon => "waiting on its Codex daemon".to_string(),
            WaitClass::TabUnread => "aterm cannot read its tab well enough to move it".to_string(),
            WaitClass::UnderWay => "its move is already under way".to_string(),
            WaitClass::Notice => "aterm could not type its notice".to_string(),
            WaitClass::Queued => "its notices wait unread behind its queue".to_string(),
            WaitClass::Unreached => "aterm cannot reach its tab to ask".to_string(),
            WaitClass::Unrecorded => "no look has recorded what it waits on".to_string(),
            WaitClass::Unclassified => format!("a wait aterm does not classify ({})", word(wait)),
        }
    }
}

/// WHAT HOLDS A WAIT, BY CLASS — one table for the two places the owner reads
/// a wait's class: the overdue mark's cause ([`Row::overdue_cause`]) and the
/// waiting record's sentence for a wait with no words of its own
/// ([`unworded_waiting_line`]), so the record and the mark never class one
/// word two ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitClass {
    /// The agent's own work.
    OwnWork,
    /// A person: a box, a draft, a login, a person at the tab, a hold.
    Person,
    /// A usage limit, or a Codex save-then-wait switch, until its reset.
    Limit,
    /// A Codex goal aterm paused and could not resume (ruling 381).
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
    /// No look has recorded a wait.
    Unrecorded,
    /// A wait this build does not classify.
    Unclassified,
}

/// The class of `wait` ([`WaitClass`]) — a release's own gate
/// (`release:<gate>`) by the gate it waits on. Read from the word alone: the
/// row's state that outranks it (the processes under the agent, a person no
/// look has seen) is [`Row::overdue_cause`]'s.
fn wait_class(wait: &str) -> WaitClass {
    let wait = wait.strip_prefix("release:").unwrap_or(wait);
    let head = wait.split_once(':').map_or(wait, |(head, _)| head);
    match (head, wait) {
        ("limited", _) => WaitClass::Limit,
        (_, "not-idle:waiting") | ("box" | "draft" | "login" | "attended" | "held", _) => {
            WaitClass::Person
        }
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
            | "settling"
            | "busy-thread"
            | "failed",
            _,
        ) => WaitClass::OwnWork,
        // THE CODEX LANE'S LADDER AND GOAL PAUSE (rulings 380 and 381):
        // a save-then-wait switch holds the session until the limit's
        // reset; a goal it pursues, the pause's own steps and its own
        // conversation's turn in the daemon are its own work; a goal
        // left paused waits on the one hand step its row names.
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
        // conversation before it may type there (the review of 2026-09-28:
        // the waiting record showed these words raw).
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
            | "version-unreadable",
            _,
        ) => WaitClass::TabUnread,
        ("in-flight", _) => WaitClass::UnderWay,
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
        ("", "") => WaitClass::Unrecorded,
        _ => WaitClass::Unclassified,
    }
}

/// THE WAITING RECORD'S SENTENCE FOR A WAIT WITH NO WORDS OF ITS OWN
/// ([`Row::waiting_line`]): worded by its class ([`wait_class`]), NEVER the
/// wait's own word. Until the review of 2026-09-28 it fell back to the raw
/// word — `announce-refused:changed; it moves on its own once that is over`,
/// of exactly tab #1's shape, a notice the typing fence refused — on the
/// owner's only view of a healthy upgrade. A word no class knows is said as
/// aterm's own step, and `--status` names it.
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
            WaitClass::Unrecorded | WaitClass::Unclassified => {
                "aterm is waiting on a step of its own; it moves on its own once that is over \
                 (`aterm harness upgrade --status` names the step)"
            }
        },
    }
}

/// The class [`Row::overdue_cause`] names a move its agent's own work holds
/// by — what [`Row::asks_on_its_own`] keeps a record.
const OWN_WORK: &str = "held by its own work";

/// What the owner can do about a stalled upgrade ([`Row::remedy`]) — the
/// words differ because what moves it differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    /// `overdue` waiting on what `--now` still moves ([`now_moves_past`]): a
    /// Codex act that left its text typed, tried again at once, and a notice
    /// behind a full queue of notices (`queued`), typed once more; `--skip`
    /// keeps it where it is.
    Now,
    /// `overdue` waiting on what `--now` does NOT waive — the agent's READY
    /// answer, a draft in its composer, a box, a hold, work still running
    /// under it, a background shell or a question waiting on a person:
    /// [`Row::wait`] is named, and `--skip` keeps it where it is.
    Waits,
    /// A round that gave up ([`upgrade::GAVE_UP`]), resting before it asks
    /// again: `--now` asks it again at once — the one stopped kind it re-arms
    /// (the upgrade giving up, never a refusal).
    AskAgain,
    /// `held-back:<owner>`: typing into the tab cannot reach it, so no word
    /// moves it; quit it in its pane and resume it there, or `--skip`.
    InItsPane,
    /// `refused:*`, `failed:*`: the harness asks it again only after a rest
    /// ([`Row::retry_at`]); to move it sooner, quit it and resume it by hand,
    /// or `--skip`. And `blocked:*` ([`BLOCKERS`]): no rung passes it, so to
    /// move it, quit it and resume it by hand in its tab, or `--skip`.
    ByHand,
    /// A move that stopped AFTER the agent it ended was seen gone
    /// ([`Row::failed_after_exit`]): nothing runs in the tab to quit, and the
    /// conversation is kept — a Codex's in the daemon or its rollout (`codex
    /// resume` in the tab takes it back), a Claude Code's in its transcript
    /// (`claude --resume <conversation>` in the tab).
    ResumeInTab,
}

/// How long a PERSON AT THE TAB (`attended`) stays the owner's word for a wait
/// with no look to confirm it ([`Row::wait_words`]): three of the looks the
/// window's host takes of an attended wait while it owns the point, one
/// [`upgrade::QUIET_S`] apart (`upgrade_drive::after_attended`). Past it the
/// wait is the last point's, and the move waits for the next one. (Past the
/// backstop on those looks the host's looks climb the ladder and the point is
/// the loop's, so between them the words say the next turn end.)
pub const ATTENDED_SEEN_S: u64 = 3 * upgrade::QUIET_S;

/// Whether `--now` moves an OVERDUE upgrade past the wait a step recorded
/// ([`upgrade::wait_word`]). An overdue upgrade has stood at the ladder's
/// Land rung for hours ([`STALLED_AFTER_S`]) — where the settle is waived and
/// a person holds it only by a keystroke — and `--now` is that same rung
/// (the owner's decision of 2026-09-28), so the settle, a person near the
/// tab and a turn still running are no longer anything `--now` moves: until
/// that day this row said `Upgrade now moves it at its next turn end` of a
/// goal-mode Codex whose turns never ended. What stands whatever the owner
/// says: the READY answer, a draft, a box, a hold, work under the agent or
/// in its daemon, a process proof, Claude's status off `idle`, a keystroke.
/// Nor is it ever the line an abandoned notice owes, which its own gate
/// holds (`release:<gate>`): `--now` asks the agent again, and that line
/// goes when its gate lets it or the next notice supersedes it.
/// A Codex act that left its text typed and backs off (`left-typed-backoff`)
/// is tried again at once on `--now`, which lifts the back-off. A Claude
/// notice waiting behind a FULL QUEUE of notices the model has not taken
/// (`queued`, [`upgrade::Facts::queued`]; review of 2026-09-27: the band said
/// `Upgrade now` could not move it and that it "moves once that ends", of a
/// limit over by every word) is typed once more on `--now`: the person
/// asking — never while a limit still stands, which waits `limited`. A
/// notice aterm's OWN FENCE refused (`announce-refused:<why>`) is asked for
/// again on `--now`, through the same fence, which still holds it for a
/// keystroke; once that word is in force the remedy is the wait
/// ([`Row::fence_fails_under_now`]).
fn now_moves_past(wait: &str) -> bool {
    matches!(wait, "left-typed-backoff" | "queued") || wait.starts_with("announce-refused:")
}

/// What a Claude Code wait stands for, in a person's words
/// ([`Row::wait_words`]): work of the agent's own under it (a break of its
/// background work, or Claude's own `shell` status at an idle point), which
/// `--now` cannot move, and a notice waiting unread behind a full queue
/// (`queued`), which it can ([`now_moves_past`]). Until 2026-09-26 a Claude
/// row had no words at all. The owner read
/// "waiting (background)" beside a promise that the move comes "once that
/// ends", while two poll loops that could never end held a tab for four days.
/// The words are SHORT: the band's first line holds 64 characters, so what
/// the upgrade does about the wait is on its remedy line
/// (`message_reporters::agent_upgrade_stalled`). They say only what is true
/// before a notice too (`not-idle:shell` is also a pending wait). Claude's own
/// status that its idle screen does not bear out (`status-stale:<status>`,
/// 2026-09-27) is said so: the owner cannot move it. A line moves once the
/// status has stood; the restart only once Claude says `idle`, a READY it
/// holds asked again meanwhile ([`upgrade::next_step`]). `None` for every
/// other wait, whose word says it.
///
/// Round 18, day four (D3): EVERY wait a Claude step records has words now —
/// the band read `waiting (not-idle:busy)` for a turn still running.
fn claude_wait_words(wait: &str) -> Option<&'static str> {
    Some(match wait {
        "background" | "not-idle:shell" => "its own work runs",
        "busy" | "not-idle:busy" => "its turn is still running",
        "not-idle:waiting" => "it asked a question and waits",
        "settling" => "waiting for the tab to settle",
        // A click or a scroll stamps the tab as a keystroke does.
        "attended" => "someone is using its tab",
        "awaiting-ready" | "not-ready" => "waiting for it to agree to the move",
        "draft" => "a draft waits in its prompt",
        "box" => "a box on its screen waits for a choice",
        "held" => "its tab is held",
        "limited" => "it is at a usage limit",
        // Never `notice` (ruling 307): the upgrade's own word for the text it
        // types, which read as one of the band's rows.
        "queued" => "it has not read the upgrade's question yet",
        "login" => "it is not logged in",
        "in-flight" => "a step is under way",
        // ATERM'S OWN NOTICE, which its fence would not type (2026-09-28): the
        // owner reads what could not be done, and by whom — never "its turn".
        "announce-refused:changed" => {
            "aterm could not type its notice: the screen moved as it typed"
        }
        "announce-refused:yield" => {
            "aterm could not type its notice: someone kept typing in its tab"
        }
        w if w.starts_with("announce-refused:") => "aterm could not type its notice",
        // THE RELEASE'S OWN GATE holds the line an abandoned notice owes
        // (`wait:release:<gate>`, `upgrade_drive::owed_word`; L5 of the
        // upgrade's leftovers): the agent asked to wind down has not been
        // told to carry on. Never the gate's word, and never "waits" — the
        // agent may be at work again (`release:not-idle`). Said only while a
        // release is owed ([`Row::wait_words`]).
        w if w.starts_with("release:") => "it is owed the line telling it to carry on",
        w if w.starts_with("status-stale") => "its status lags",
        w if w.starts_with("not-idle") => "it has not gone idle",
        _ => return None,
    })
}

/// How long a session has been behind, in a person's words for a record read
/// once (design ruling 308): `40 min`, `8 h`, `3 days` — the band's own
/// elapsed words' units, never the roster's `8h28m`. `None` where the start
/// is unknown (0, a fixture's or a record's default) or not in the past, or
/// more than a year back, which no running session has been.
fn behind_words(since: u64, now: u64) -> Option<String> {
    const YEAR_S: u64 = 365 * 86_400;
    let secs = now.checked_sub(since).filter(|_| since > 0)?;
    if secs == 0 || secs > YEAR_S {
        return None;
    }
    Some(if secs < 3_600 {
        format!("{} min", (secs / 60).max(1))
    } else if secs < 48 * 3_600 {
        format!("{} h", secs / 3_600)
    } else {
        format!("{} days", secs / 86_400)
    })
}

/// Why a stall of `kind` ([`Row::stall`], every kind but `overdue`) will not
/// move on its own — words that depend on the kind alone, so the tab's mark
/// built from them is sent once per stall. A Codex move that stopped after
/// its `/exit` says where its conversation is ([`codex_failure`]); a Claude
/// Code move that stopped after its SIGTERM ended it says so in
/// [`stop_words`], its remedy naming `claude --resume`. A restart under way
/// that has not moved says what it waits on ([`stuck_words`]).
fn stall_reason(agent: upgrade::Agent, kind: &str) -> String {
    if agent == upgrade::Agent::Codex
        && let Some(words) = kind.strip_prefix("failed:").and_then(codex_failure)
    {
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
        // A `line:` refusal is the relaunch LINE's (`upgrade::relaunch_line`),
        // never the flags' (review of 2026-09-25: it read as "launch flags",
        // and a fish tab in another directory was told the wrong thing).
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
        // Once, plainly (day five, D23: two lines said it twice, and `no
        // word moves it` read as jargon); the remedy names the pane.
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

/// Why a move stopped, in a person's words (round 19, ruling 283: the band
/// read `the move stopped (signal-refused)`). Never the raw word — that stays
/// on `--status` and the `upgrade=` column — and never "restart": the words
/// say what happened to the agent.
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

/// A RESTART UNDER WAY THAT HAS NOT MOVED (S2 of the in-flight review,
/// 2026-09-27): what it waits on, one word, once its `phase` has stood
/// longer than [`STALE_S`] from the phase's own start — `exiting` (the agent
/// was asked to end and has not: a SIGTERMed Claude Code hung in its
/// shutdown, a Codex that has not taken its `/exit`), `exited` (it ended —
/// seen gone at `exited_at` — and the line that resumes it is not typed: a
/// person at the prompt, a hold) or `relaunched` (the line was typed, and the
/// agent it started has not held the conversation or reached an idle point:
/// a box on its screen, most often). Until then neither phase ever read
/// stalled, and the column read `restarting` for as long as the file lived.
/// NOTHING IS FORCED: no harder signal, no kill — the stall only names the
/// wait. `None` for any other phase, and within the bound. The owner's view
/// ([`Row::stuck`]) and the sweep's once-only ledger note (`St::stuck`, L4 of
/// the upgrade's leftovers, 2026-09-28) both read it here.
pub(super) fn stuck_on(phase: &Phase, exited_at: u64, now: u64) -> Option<&'static str> {
    let (at_s, what) = match *phase {
        Phase::Exiting { at_s } if exited_at == 0 => (at_s, "exiting"),
        Phase::Exiting { at_s } => (at_s, "exited"),
        Phase::Relaunched { at_s } => (at_s, "relaunched"),
        _ => return None,
    };
    (now.saturating_sub(at_s) > STALE_S).then_some(what)
}

/// What a restart under way that has not moved waits on ([`Row::stuck`]), in
/// a person's words that ask for nothing: nothing is forced, and no word of
/// the owner's moves a move under way. Stable per kind, for the tab's mark,
/// and the words of the sweep's ledger note of it (`note_stuck`).
pub(super) fn stuck_words(agent: upgrade::Agent, what: &str) -> &'static str {
    match (agent, what) {
        (upgrade::Agent::Codex, "exiting") => {
            "the /exit typed into it for the upgrade has not ended it yet; nothing is forced"
        }
        (_, "exiting") => {
            "it was asked to end for the upgrade and has not exited, its own shutdown still \
             running; nothing is forced"
        }
        (_, "exited") => {
            "it ended for the upgrade, and the line that resumes it waits until a person or a \
             hold lets go of its prompt; nothing is forced"
        }
        _ => {
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

/// Whether a stop's reason is a REFUSAL — the upgrade would not type or
/// signal at all: not the shell's job, launch flags that cannot be carried
/// into a resume (`argv:`), a relaunch line that cannot be typed (`line:`).
/// A re-armed round meets it again unless something changed, so it stays a
/// stall the owner is shown ([`Row::stall`]).
fn refusal(why: &str) -> bool {
    why == "not-a-shell-job" || why.starts_with("argv:") || why.starts_with("line:")
}

/// A third party's or a derived word, cut to what a roster token may carry.
fn word(s: &str) -> String {
    let w: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '(' | ')'))
        .take(64)
        .collect();
    if w.is_empty() { "-".to_string() } else { w }
}

/// EVERY UPGRADE RECORD as a [`Row`], sorted by conversation — the tab
/// `opts` names only, when it names one. Files only: no socket, no lock (each file
/// is replaced whole by a rename, so a read sees one version or the other).
/// What cannot be read is left out ([`read_rows`] says whether anything was).
#[must_use]
pub fn rows(opts: &Opts) -> Vec<Row> {
    rows_at(opts, now_s())
}

fn rows_at(opts: &Opts, now: u64) -> Vec<Row> {
    read_rows(opts, now).0
}

/// [`rows_at`], and whether the records were READ WHOLE — `false` for a
/// state directory there but not listable, a listing that broke off, or a
/// record listed but not readable, each left out of the rows (review of
/// 2026-09-27: read as no records, a successor's first look handed the
/// window no rows, and it withdrew every carried stall row whose tab still
/// stalled — back as new unread rows at the next look). No directory at all
/// is whole: nothing was ever recorded. A record removed between the listing
/// and its read is no record, nor is a directory, and a file that reads but
/// is no upgrade record (`models.json`, a relaunch's record) is none either.
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
        // The relaunch files its records beside the upgrade's
        // (`St::cause`): an agent relaunched after an exit, a memory or a
        // model restart is no upgrade of the tab — the upgrade's own
        // restart of a conversation with no task is.
        let Some(st) = St::from_json(&text).filter(St::is_upgrade) else {
            continue;
        };
        if opts.only_sid.as_ref().is_none_or(|s| *s == st.tab) {
            // A Codex tab's goal record (`goal_hold`), beside its loop's
            // ledger under the aterm state root.
            let root = opts
                .aterm_state
                .as_deref()
                .filter(|_| st.agent == upgrade::Agent::Codex && !st.tab.is_empty());
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
            out.push(Row::of(&session, &st, now).with_goal(hold.as_ref(), switch, now));
        }
    }
    out.sort_by(|a, b| a.session.cmp(&b.session));
    (out, whole)
}

/// [`rows`] as `aterm harness upgrade --status` shows them: each one
/// [`Row::standing`], where Claude's session files read whole (`true`); where
/// they do not, every recorded row (`false` — the caller says they are
/// unchecked). No roster here, so a tab is proven by the agent's environment
/// alone, and a holder whose environment cannot be read is not disproven. A
/// row with no recorded tab is shown under the tab its holder is proven in,
/// and only then matched against the tab `opts` names.
#[must_use]
pub fn status_rows(opts: &Opts) -> (Vec<Row>, bool) {
    let now = now_s();
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
        match holders(&opts.home, &vet, None) {
            Some(held) => (
                all.into_iter()
                    .map(|mut r| {
                        r.held_again(&held);
                        r
                    })
                    .filter(|r| r.standing(&held, true))
                    .map(|mut r| {
                        // An unreached row reads its holder's live status
                        // before its tab is filled in from the proof.
                        r.unreached_status(&held, true, now);
                        if r.tab.is_empty() {
                            r.tab = r.proven_tab(&held).unwrap_or_default();
                        }
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
            r.held_by_a_process() || (r.agent == upgrade::Agent::Claude && r.failed_after_exit())
        })
        .map(|r| r.session.clone())
        .collect()
}

// ---------------------------------------------------------------- who holds it now

/// ONE LIVE CLAUDE CODE PROCESS a recorded upgrade is vetted against
/// ([`Row::standing`]): the conversation it holds, the build it runs (its
/// session file's `version`, as the running build reports itself), and the
/// tab it is PROVEN to run in — `None` when no proof reads — and Claude's own
/// status in that file (`busy`, `shell`, `idle`, `waiting`) and since when
/// (unix seconds), which a row no look has reached reads as its wait
/// ([`Row::unreached_status`]). A Codex holder has no status (empty, `0`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Holder {
    pub(super) session: String,
    pub(super) version: String,
    pub(super) tab: Option<String>,
    pub(super) status: String,
    pub(super) status_since: u64,
}

/// The live holders of `sessions`, from Claude's session files: a file whose
/// pid is alive and started when the file says ([`conversation_live`]'s rule:
/// a recycled pid holds nothing). Its tab is proven by its process group
/// being ONE tab's foreground group in `tabs` (the host's `who` roster), or
/// by the `ATERM_PARENT_SESSION_ID` its environment carries — a suspended
/// agent is not the foreground, and one in a pane is not in the tab's group
/// at all. Two proofs that disagree prove nothing. `None` when the session
/// files cannot be read whole: no verdict either way — but NO sessions
/// directory at all is a verdict: Claude Code writes one file there for every
/// interactive session, so nothing holds anything. Past the directory scan,
/// only the files of `sessions` cost a read (a `ps` and a `KERN_PROCARGS2`
/// each).
fn holders(
    home: &Path,
    sessions: &BTreeSet<String>,
    tabs: Option<&[LiveTab]>,
) -> Option<Vec<Holder>> {
    // A Codex upgrade (`codex-<tab>`) is held by the Codex TUI that leads its
    // tab now ([`super::codex::holders`]); Claude's files say nothing of it.
    let codex = super::codex::holders(home, sessions, tabs);
    if sessions.iter().all(|s| s.starts_with("codex-")) {
        return Some(codex);
    }
    let files = match session_files(home) {
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
    /// Restart at the next turn end ([`Request::Now`]): the settling window
    /// and the attended-tab guard waived. An upgrade that GAVE UP (no READY
    /// answer after its last notice) is re-armed AT ONCE — the one
    /// place the owner's word adds an act: the notice is typed again, and
    /// after its READY answer the agent is restarted — rather than at its
    /// next round ([`upgrade::RETRY_S`]). A refused or failed upgrade is NOT
    /// re-armed by the word ([`ask`] refuses: what stopped it may still hold);
    /// it starts its next round on its own once it has rested.
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

/// The prefix of [`ask`]'s refusal of `--now` for a round that STOPPED and
/// is not the owner's word to re-arm: `stopped:<next round, unix s; 0 at its
/// next look>:<the CLI's sentence>` — machine words the window reads
/// ([`refused_stopped`]) and the CLI never prints ([`refusal_sentence`]).
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

/// WRITE THE OWNER'S WORD for the upgrade in tab `sid`, under the sweep lock,
/// with one ledger line (`requested:<word>`). The word is on the TAB: it
/// applies to the conversation there, not to the same conversation resumed
/// elsewhere (`St::request_for`). The upgrade is the tab's newest one that
/// has not begun restarting — pending, announced, or stopped — one a live
/// process still holds in that tab first ([`Row::standing`]). `Err` names why
/// nothing was written: another sweep held the lock past its bound, 10 s
/// (`busy:another-sweep`, so a caller can retry by code), a restart already
/// under way, `--now` for an upgrade that was refused or failed (only one
/// that gave up is re-armed, [`Ask::Now`]), or no upgrade recorded for the
/// tab.
///
/// # Errors
///
/// As above; the state directory is left as it was.
pub fn ask(opts: &Opts, sid: &str, what: Ask) -> Result<Row, String> {
    ask_within(opts, sid, what, None, ASK_LOCK_WAIT)
}

/// [`ask`] FOR THE BUILD THE OWNER WAS SHOWN: the word is written only when
/// the tab's upgrade — the one [`ask`] would write it on — still moves to
/// `target` (equal as versions, else as text). The window's band row and tab
/// menu name the build they were drawn for, and either can be pressed after
/// the upgrade moved on to a newer one: a skip pressed on a row for 2.1.282
/// must never skip 2.1.283, nor a `Now` hurry a move to a build the owner
/// was never shown. Checked under the lock the word is written under, so no
/// step moves the target between the check and the write.
///
/// # Errors
///
/// [`ask`]'s, and `stale:<the target now>` when the upgrade moved on;
/// nothing is written.
pub fn ask_for(opts: &Opts, sid: &str, what: Ask, target: &str) -> Result<Row, String> {
    ask_within(opts, sid, what, Some(target), ASK_LOCK_WAIT)
}

/// [`ask`], waiting at most `wait` for the lock; with `target`, [`ask_for`].
fn ask_within(
    opts: &Opts,
    sid: &str,
    what: Ask,
    target: Option<&str>,
    wait: std::time::Duration,
) -> Result<Row, String> {
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
        &opts.home,
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
                // GAVE UP — no READY answer it could act on after the last notice
                // (`Step::GiveUp`): re-armed, every gate asked again from the
                // notice. Nothing is ever killed to pass them, so asking again
                // can only ask.
                Phase::Failed(why) if why == upgrade::GAVE_UP => {
                    st.phase = Phase::Pending;
                    st.forget_markers();
                    st.noted.clear();
                }
                // Refused or failed: what stopped it still holds (a launch
                // that cannot be resumed, no shell to relaunch at, a
                // conversation resumed by hand), and re-arming it would type
                // a notice and, after READY, signal an agent the harness had
                // stopped acting on for good (review of 2026-09-25).
                // A Codex move that stopped after its `/exit` has nothing left
                // in the tab to quit: its conversation is `codex resume`'s.
                Phase::Failed(why) if st.agent == upgrade::Agent::Codex && st.exited_at != 0 => {
                    return Err(format!(
                        "the upgrade in tab {sid} stopped after its `/exit` ({}): `--now` \
                         re-arms only one that gave up — `codex resume` in the tab takes its \
                         conversation back, and `--skip` quiets this record",
                        word(why)
                    ));
                }
                // A Claude Code move that stopped after its SIGTERM ended the
                // agent, nothing holding the conversation again: nothing runs
                // in the tab to quit (S1 of the in-flight review).
                Phase::Failed(why)
                    if st.exited_at != 0 && !held.iter().any(|h| h.session == session) =>
                {
                    return Err(format!(
                        "the upgrade in tab {sid} stopped after its SIGTERM ended the agent \
                         ({}): `--now` re-arms only one that gave up — `claude --resume {}` in \
                         the tab takes its conversation back, and `--skip` quiets this record",
                        word(why),
                        word(&session)
                    ));
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
                    // TYPED for the window (ruling 283): the band words it in
                    // a person's terms ([`refused_stopped`]); the CLI prints
                    // the sentence ([`refusal_sentence`]).
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
    // EVERY WORD ARMS A NEW ROUND (`St::new_round`, review of 2026-09-25): a
    // fresh salt, so the READY marker of a notice typed after the word is one
    // no answer given before the word can carry.
    st.new_round(now);
    // A HOLD ENDS AN ANNOUNCEMENT (review of 2026-09-25). A held upgrade
    // waits `skipped`/`deferred` and owns none of the session's turn ends, so
    // its supervisor continues the worker past the wind-down the notice asked
    // for — the agent goes back to work after any READY it gave. Kept, that
    // notice and its answer were read as consent once the hold ended (the
    // deferral ran out, or `--now` over the skip), and an agent at work was
    // SIGTERMed with no fresh notice. The upgrade is pending again, its marker
    // gone: the next turn end after the hold gets a new notice, and a new
    // READY is needed.
    //
    // AND THE AGENT IS RELEASED (2026-09-26): it was asked to wind down for a
    // restart the owner has now held, so it is owed ONE line telling it to go
    // on ([`St::owe_release`], typed by the next step that may type it).
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
    // An upgrade that GAVE UP still honours a READY to its notices
    // (`upgrade::next_step`), and a word holds that too: no answer given
    // before the word may act after it.
    if matches!(&st.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) && what != Ask::Now {
        st.forget_markers();
    }
    // The tab the word is on — and, for a state with none recorded, the tab
    // its holder was just proven in (a step records it for a pending upgrade
    // at every visit anyway).
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
    Ok(Row::of(&session, &st, now))
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

/// THE WINDOW'S VIEW of its tabs' upgrades, kept by its host thread
/// (`aterm-gui`'s `harness_host`), which refreshes it after its workers act,
/// at an activation notice or the owner's word ([`word_marker`]), when its
/// roster changes, at the instant [`Self::refresh`] names, and when the
/// screen of a tab it follows moves ([`Self::follows`]) — never on a timer of
/// its own. What it last told the window and the tabs: the rows it
/// handed over (sent again only when they change — a [`Row`] carries
/// instants, not ages, so an unchanged upgrade is an unchanged row) and the
/// stalled tabs it marked.
#[derive(Debug, Default)]
pub struct View {
    sent: Option<Vec<Row>>,
    badged: BTreeMap<String, String>,
    sock: Option<String>,
    /// Looks in a row whose roster could not be read (the control socket not
    /// bound yet at start-up, a window busy past its bound): the next look is
    /// asked for on a growing pause ([`UNREAD_RETRY`]).
    unread: u32,
    /// The tabs whose row the last whole look read off its live holder's
    /// status ([`Self::follows`]).
    follows: Vec<String>,
    /// The tabs a record of which the last whole look found PAST ITS WATCH
    /// ([`Self::watch_due`]).
    watch: Vec<String>,
}

/// The pauses before looking again at a roster that could not be read: two
/// seconds, growing to ten minutes. Only a failed read asks for one — a view
/// that reads is looked at again only when something moves.
const UNREAD_RETRY: [std::time::Duration; 4] = [
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(16),
    std::time::Duration::from_secs(120),
    std::time::Duration::from_secs(600),
];

impl View {
    /// Look at the upgrades of the instance at `opts.sock`: the rows of its
    /// LIVE tabs (a finished restart for [`DONE_SHOWN_S`]) to `summary` when
    /// they changed, and each stalled tab's attention raised, each recovered
    /// one's lowered. An unreadable roster changes nothing: a tab that cannot
    /// be seen is not a tab that recovered. The answer is the unix second at
    /// which a row next changes by time alone ([`next_change`]: an upgrade
    /// turning overdue, an owner's `--now` or `--defer` running out, a
    /// finished move leaving the summary), or `None` when none will — the
    /// host looks again then. A roster that could not be read asks for a look
    /// again on a growing pause ([`UNREAD_RETRY`]): the control socket is
    /// bound after the host starts.
    pub fn refresh(&mut self, opts: &Opts, summary: &mut dyn FnMut(&[Row])) -> Option<u64> {
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
            &|sessions| holders(&opts.home, sessions, Some(&tabs)),
            summary,
        )
    }

    /// [`Self::refresh`] at `now` over the roster `tabs`, with the live
    /// holders of a set of conversations read by `holders` ([`holders`] in
    /// the host; a script in a test). Only a row [`Row::standing`] is handed
    /// over or marked; session files that cannot be read whole change
    /// nothing, like a roster that cannot be read — and so do upgrade
    /// records that cannot be read whole ([`read_rows`]): what is handed
    /// over is a WHOLE LOOK, which the window takes as every tab seen.
    fn refresh_with(
        &mut self,
        opts: &Opts,
        tabs: &[LiveTab],
        now: u64,
        holders: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>,
        summary: &mut dyn FnMut(&[Row]),
    ) -> Option<u64> {
        let (all, whole) = read_rows(opts, now);
        // A RECORD NO BUILD GAVE A TAB (an older build recorded it only at
        // the announcement; measured 2026-09-25 on the owner's machine): its
        // tab is the one its live holder is PROVEN in, as `--status` shows
        // it ([`status_rows`]) — else it was never watched, whatever its
        // deadline said (the watch's review, 2026-09-28). Read for the watch
        // alone: its row is shown as before.
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
        // THE WATCH (design record 2026-09-28, §3.2 C3): every live tab with
        // a record past its watch, whether or not its row stands, whether or
        // not its holders read, and whether or not every OTHER record read
        // whole — each record alone decides its own watch, and the host hands
        // each tab to the watch, which reads it off any point. The instant
        // the next one comes is named the same way: a look that stops short
        // (records not read whole, holders not read) still names it, or the
        // host would keep no timer for a session offering no point, which is
        // the stall the watch exists to end (the watch's review, 2026-09-28:
        // one session file `session_files` refuses made every look stop at
        // the holders, for as long as that file stayed).
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
        // an ordinary stopped upgrade, whatever the day it is shown for says.
        if let Some(held) = &held {
            for r in &mut mine {
                r.held_again(held);
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
            // A row no look has reached reads its holder's live status (L2):
            // a changed status is a changed row, sent again at the look the
            // host takes when that tab's screen moves ([`Self::follows`]).
            for r in &mut mine {
                if r.unreached_status(held, false, now) {
                    follows.push(r.tab.clone());
                }
            }
        }
        follows.sort();
        follows.dedup();
        self.follows = follows;
        if self.sent.as_ref() != Some(&mine) {
            summary(&mine);
            self.sent = Some(mine.clone());
        }
        let want: BTreeMap<String, String> = mine
            .iter()
            .filter_map(|r| Some((r.tab.clone(), r.badge(now)?)))
            .collect();
        self.sock.clone_from(&opts.sock);
        self.mark(opts, &want);
        next
    }

    /// THE TABS WHOSE ROW FOLLOWS ITS AGENT'S LIVE STATUS, as the last whole
    /// look read them ([`Row::unreached_status`]): a row that reads otherwise
    /// whenever Claude's status in that tab moves — a box answered, a turn
    /// begun or ended — which no instant names ([`next_change`] is time
    /// alone). The window's host looks again whenever one of these tabs'
    /// screens moves, the push a status move comes with, and never on a
    /// timer (the review of the upgrade's leftovers, 2026-09-28). A look that
    /// could not read whole leaves them as they were.
    #[must_use]
    pub fn follows(&self) -> &[String] {
        &self.follows
    }

    /// THE TABS DUE A WATCH, as the last look read them (design record
    /// 2026-09-28, "No upgrade stuck forever", §3.2 C3): a live tab one of
    /// whose records is past its [`Row::watch_at`] — its step's deadline or
    /// its move clock — which the window's host hands to the watch
    /// (`upgrade_drive::watch`), whatever point the session does or does not
    /// offer. Every step lived inside a visit, and a visit came only at a
    /// point, so tab #1 sat three days with no clock running: the instant is
    /// named by [`next_change`], and this says, once it has come, whose it
    /// was. Each record decides its own: a look that could not read every
    /// record whole, or the holders, still names the tabs of those it read
    /// (a record with no recorded tab only through its proven holder); a
    /// roster that could not be read names none.
    #[must_use]
    pub fn watch_due(&self) -> &[String] {
        &self.watch
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
            };
            self.mark(&opts, &BTreeMap::new());
        }
    }

    /// Bring the tabs' `owner=upgrade` attention to `want`: set a changed or
    /// new text, unset a tab no longer in it. A write that fails is tried
    /// again at the next refresh (it is not remembered as done).
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
                .is_ok_and(|l| l.starts_with("OK"))
                || c.request_line(&format!("@{tab} status"))
                    .is_ok_and(|l| !l.starts_with("OK"))
            {
                self.badged.remove(&tab);
            }
        }
        for (tab, text) in want {
            if self.badged.get(tab) == Some(text) {
                continue;
            }
            if c.request_line(&format!("@{tab} meta set attention {owner} {text}"))
                .is_ok_and(|l| l.starts_with("OK"))
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
/// wait began), an upgrade still asking on its own turning a row
/// ([`MOVE_BUDGET_S`]), the owner's `--now` no longer quieting it
/// ([`NOW_QUIETS_S`]), a `--defer`
/// running out, a restart's carry-on no longer waited on for its model
/// ([`Row::confirm_by`]: `restarting` turns `done`), a finished move leaving
/// the summary ([`DONE_SHOWN_S`]),
/// a move that failed after its exit leaving it
/// ([`EXITED_FAILURE_SHOWN_S`]), a restart under way turning `stuck`
/// ([`STALE_S`] from its phase's start: S2 of the in-flight review), and —
/// for every record still owed a step — the instant the host WATCHES it
/// ([`Row::watch_at`], design record 2026-09-28, §3.2 C3): the step's
/// deadline or the move clock, reached whether or not the session offers a
/// point. One wake per deadline, never a poll: past it the row is listed
/// due ([`View::watch_due`]) and the watch stamps it, which moves the
/// instant a whole [`super::WATCH_GAP`] on.
fn next_change(rows: &[Row], now: u64) -> Option<u64> {
    rows.iter()
        .flat_map(|r| {
            let waiting = matches!(r.phase, Phase::Pending | Phase::Announced { .. });
            let under_way = match r.phase {
                Phase::Exiting { at_s } | Phase::Relaunched { at_s } => Some(at_s),
                _ => None,
            };
            // The move clock turns a record into a row ([`MOVE_BUDGET_S`]):
            // a round resting after a stop included — named only for a row
            // it changes, one still asking on its own.
            let behind = r.behind_since > 0 && r.asks_on_its_own(now);
            [
                under_way.map(|at_s| at_s.saturating_add(STALE_S + 1)),
                waiting.then(|| r.behind_since.saturating_add(STALLED_AFTER_S)),
                // A wait no rung passes turning `blocked` (ruling 380).
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
                // A goal the move paused turning LEFT PAUSED, and a goal's
                // rest running out.
                (r.goal_held_since != 0 && r.goal_left_since == 0).then(|| {
                    r.goal_held_since
                        .saturating_add(super::super::upgrade_codex::GOAL_LEFT_AFTER_S)
                }),
                (r.goal_rest_until != 0).then_some(r.goal_rest_until),
                // A person no look has seen lately stops being named
                // ([`Row::person_named`]).
                r.person_unseen_at().filter(|_| waiting),
                // THE WATCH (§3.2 C3): `0` for a record owed nothing more.
                (r.watch_at != 0).then_some(r.watch_at),
                // A person's hold that has stood turns a record into a row
                // ([`Row::person_hold_stands_at`]).
                r.person_hold_stands_at(),
            ]
        })
        .flatten()
        .filter(|t| *t > now)
        .min()
}

/// Where the owner's word is announced to the window's host: a file under
/// `<state>/upgrade/owner/` that [`ask`] replaces after every word, which the
/// host's activation wake watches (`super::upgrade_wake`) — a push, so the
/// tab's worker takes the word at the session's next idle point, and the
/// view shows it, without a timer.
#[must_use]
pub fn word_marker(state: &Path) -> std::path::PathBuf {
    state.join("upgrade").join("owner").join("word")
}

/// Replace the [`word_marker`] (a new file, renamed over the old one, so its
/// identity changes): best effort — a word nobody is told of is still
/// written, and taken at the next activation notice.
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

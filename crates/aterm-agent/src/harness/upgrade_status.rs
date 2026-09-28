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
    LiveTab, Opts, Phase, Report, Request, St, Version, connect, conversation_live, ledger, load,
    now_s, process_group, save, session_files, state_dir, sweep_lock, tab_is_live,
    unique_tab_for_group, upgrade,
};

/// How long a session may be behind before its upgrade reads STALLED on its
/// own ([`Row::stall`]): the default the busy-session work proposes for its
/// deadline step (`upgrade_max_defer`, "6h"). Claude Code ships about daily
/// (measured 2026-09-24: native builds on Sep 22, 23 and 24), so a session
/// six hours behind is a day from being two builds behind.
pub const STALLED_AFTER_S: u64 = 6 * 3_600;

/// How long a finished restart stays in the host's summary: long enough for
/// the window to record it once, and across a handoff.
const DONE_SHOWN_S: u64 = 10 * 60;

/// How long a Codex move that FAILED AFTER ITS EXIT stays in the host's
/// summary — its band row, its `upgrade=` column, its tab's mark — once no
/// process is left to vet it by ([`Row::exited_at`]): a day, so an owner away
/// overnight still finds it, and not for good (the ledger keeps it).
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
    /// The last wait a step recorded ([`upgrade::wait_word`]), empty after an act.
    pub wait: String,
    /// When that wait began.
    pub wait_since: u64,
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
    /// A Codex move: when the TUI its `/exit` was typed into was seen gone
    /// (unix seconds; `0`: it lives, or no exit was typed). A move that
    /// failed after it — no hint to resume, a relaunch refused or never come
    /// up, a stale exit — has no process left to hold it, so it is not
    /// vetted against one ([`Self::standing`]): until 2026-09-26 every such
    /// failure was dropped from `--status`, the band and the `upgrade=`
    /// column, and the owner learned of it from the ledger alone.
    pub exited_at: u64,
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
            request: st.request_for(&st.tab),
            request_at: st.request_at,
            outcome: st.outcome.clone(),
            done_at: st.done_at,
            confirm_by: if st.confirming() { st.confirm_by } else { 0 },
            stalled: None,
            model: st.model_list.clone(),
            agent: st.agent,
            exited_at: st.exited_at,
            retry_at: if matches!(st.phase, Phase::Failed(_)) {
                st.failed_at.saturating_add(upgrade::RETRY_S)
            } else {
                0
            },
            last_stop: st.last_stop.clone(),
        };
        row.stalled = row.stall(now);
        row
    }

    /// Seconds until this stopped round's next one ([`Self::retry_at`]):
    /// `Some(0)` when it is due, `None` for a round that has not stopped or
    /// that the owner's word holds (a skip is not re-armed; a deferral is,
    /// once it runs out).
    #[must_use]
    pub fn next_round_in(&self, now: u64) -> Option<u64> {
        if !matches!(self.phase, Phase::Failed(_)) || self.owner_holds(now) {
            return None;
        }
        Some(self.retry_at.saturating_sub(now))
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
    /// the tab's typing does not reach), and `overdue` — behind for
    /// [`STALLED_AFTER_S`] or more, whatever it waits on, UNLESS the owner's
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
        if self.owner_holds(now) {
            return None;
        }
        // Behind for STALLED_AFTER_S or more, whatever it waits on — a round
        // resting after it gave up included, so a long-behind session reads the
        // same word through every round and the band row does not flap.
        let overdue = (now.saturating_sub(self.behind_since) >= STALLED_AFTER_S
            && !self.hurried(now))
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
                } else {
                    overdue
                }
            }
            Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => None,
        }
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
        let behind = upgrade::span(now.saturating_sub(self.behind_since));
        Some(match stall.as_str() {
            "overdue" => match self.wait_words() {
                Some(what) => format!("behind for {behind}: {what}"),
                None => format!("behind for {behind}"),
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
    #[must_use]
    pub fn asks_on_its_own(&self, now: u64) -> bool {
        matches!(self.phase, Phase::Announced { .. }) && self.remedy(now) == Some(Remedy::Waits)
    }

    /// What a Codex wait the owner cannot move by `--now` stands for, in a
    /// person's words: what its daemon waits on (`daemon-first:<why>`, the
    /// daemon's own step carried into the client's wait — review of
    /// 2026-09-26: every client read a bare `daemon-first`, and nothing named
    /// a detached thread holding the daemon for hours), a background terminal
    /// left running, the lane's own text in the composer. `None` for every
    /// other wait, whose word says it.
    fn wait_words(&self) -> Option<&'static str> {
        if self.wait.is_empty() {
            return None;
        }
        if self.agent != upgrade::Agent::Codex {
            return claude_wait_words(&self.wait);
        }
        Some(match self.wait.as_str() {
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
            _ => return None,
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
    pub fn remedy(&self, now: u64) -> Option<Remedy> {
        let Some(stall) = self.stall(now) else {
            return (matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP)
                && !self.owner_holds(now))
            .then_some(Remedy::AskAgain);
        };
        Some(match stall.as_str() {
            "overdue" if now_moves_past(&self.wait) => Remedy::Now,
            "overdue" => Remedy::Waits,
            "gave-up" => Remedy::AskAgain,
            s if s.starts_with("held-back:") => Remedy::InItsPane,
            _ if self.failed_after_exit() => Remedy::ResumeInTab,
            _ => Remedy::ByHand,
        })
    }

    /// Whether this row's phase is one a live process must still hold for it
    /// to mean anything ([`Self::standing`]). A finished move and a restart
    /// in flight are not — a record, and a move bounded by
    /// [`super::STALE_S`] whose old process is meant to be gone — and nor is
    /// a Codex move that failed after its TUI was seen gone
    /// ([`Self::failed_after_exit`]): the TUI a holder would be is gone by
    /// construction.
    fn held_by_a_process(&self) -> bool {
        matches!(
            self.phase,
            Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
        ) && !self.failed_after_exit()
    }

    /// A Codex move that FAILED after its `/exit` ended the TUI
    /// ([`Self::exited_at`]): the tab's record, shown for
    /// [`EXITED_FAILURE_SHOWN_S`] by the host.
    #[must_use]
    pub fn failed_after_exit(&self) -> bool {
        self.agent == upgrade::Agent::Codex
            && self.exited_at != 0
            && matches!(self.phase, Phase::Failed(_))
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
        self.behind_holders(holders).any(|h| {
            h.tab
                .as_deref()
                .map_or(unproven_tab, |t| self.tab.is_empty() || t == self.tab)
        })
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
            Phase::Exiting { .. } | Phase::Relaunched { .. } => {
                ("restarting", self.phase.word(), behind)
            }
            _ if self.owner_holds(now) => (
                if matches!(self.request, Request::Skip(_)) {
                    "skipped"
                } else {
                    "deferred"
                },
                self.request.word(),
                behind,
            ),
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
    /// once it is due (the next look re-arms it); `-` for any other row.
    fn next_round_word(&self, now: u64) -> String {
        match self.next_round_in(now) {
            Some(0) => "next-round:due".to_string(),
            Some(secs) => format!("next-round:{}", upgrade::span(secs)),
            None => "-".to_string(),
        }
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
        format!(
            "upgrade tab={} session={} from={} to={}({}) phase={} pending_for={} wait={} \
             wait_for={wait_for} request={} next_round={next_round} stalled={}",
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
            // the seconds until then (0: due, or not stopped).
            ("retry_at", self.retry_at),
            ("next_round_s", self.next_round_in(now).unwrap_or(0)),
        ] {
            o.insert(k.into(), Value::from(v));
        }
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
    fn badge(&self, now: u64) -> Option<String> {
        if self.asks_on_its_own(now) {
            return None;
        }
        let words = match self.stall(now)?.as_str() {
            "overdue" => format!("behind for more than {}", upgrade::span(STALLED_AFTER_S)),
            kind => stall_reason(self.agent, kind),
        };
        let text = format!("{} upgrade stalled: {words}", self.move_words());
        Some(super::super::one_line(&text, 200))
    }
}

/// What the owner can do about a stalled upgrade ([`Row::remedy`]) — the
/// words differ because what moves it differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    /// `overdue` waiting on nothing, the settling window, the attended-tab
    /// guard, or a turn still running ([`now_moves_past`]): `--now` moves it
    /// at its next turn end (the settling window waived); `--skip` keeps it
    /// where it is.
    Now,
    /// `overdue` waiting on what `--now` does NOT waive — the agent's READY
    /// answer, a draft in its composer, a box, a hold, work still running
    /// under it, a background shell or a question waiting on a person:
    /// [`Row::wait`] is named, and `--skip` keeps it where it is.
    Waits,
    /// `gave-up`: `--now` asks it again — the one stopped kind it re-arms (the
    /// upgrade giving up, never a refusal).
    AskAgain,
    /// `held-back:<owner>`: typing into the tab cannot reach it, so no word
    /// moves it; quit it in its pane and resume it there, or `--skip`.
    InItsPane,
    /// `refused:*`, `failed:*`: the harness will not restart it; quit it and
    /// resume it by hand, or `--skip`.
    ByHand,
    /// A Codex move that stopped AFTER its `/exit` ended the TUI
    /// ([`Row::failed_after_exit`]): nothing runs in the tab to quit, and the
    /// conversation is kept (in the daemon, or its rollout) — `codex resume`
    /// in the tab takes it back.
    ResumeInTab,
}

/// Whether `--now` moves an upgrade past the wait a step recorded
/// ([`upgrade::wait_word`]): none, the settling window and the attended-tab
/// guard it waives (`upgrade::requested_step`), or a TURN still running
/// (`busy`, `not-idle:busy`) — `--now` moves it at the turn end that follows,
/// the next idle point the session's worker takes its step at. Any other
/// wait stands whatever the
/// owner says: the READY answer, a draft, a box, a hold, work under the
/// agent, a process proof — and Claude's status off `idle` for anything but a
/// turn (`not-idle:shell`, a background shell; `not-idle:waiting`, a question
/// waiting on a person; a bare `not-idle`, a status nobody read): `--now`
/// still asks Claude idle, so nothing ends those but the session itself.
/// A Codex act that left its text typed and backs off (`left-typed-backoff`)
/// is tried again at once on `--now`, which lifts the back-off.
fn now_moves_past(wait: &str) -> bool {
    matches!(
        wait,
        "" | "settling" | "attended" | "busy" | "not-idle:busy" | "left-typed-backoff"
    )
}

/// What a Claude Code wait the owner cannot move by `--now` stands for, in a
/// person's words ([`Row::wait_words`]): work of the agent's own under it (a
/// break of its background work, or Claude's own `shell` status at an idle
/// point). Until 2026-09-26 a Claude row had no words at all. The owner read
/// "waiting (background)" beside a promise that the move comes "once that
/// ends", while two poll loops that could never end held a tab for four days.
/// The words are SHORT: the band's first line holds 64 characters, so what
/// the upgrade does about the wait is on its remedy line
/// (`message_reporters::agent_upgrade_stalled`). They say only what is true
/// before a notice too (`not-idle:shell` is also a pending wait). `None` for
/// every other wait, whose word says it.
///
/// Round 18, day four (D3): EVERY wait a Claude step records has words now —
/// the band read `waiting (not-idle:busy)` for a turn still running.
fn claude_wait_words(wait: &str) -> Option<&'static str> {
    Some(match wait {
        "background" | "not-idle:shell" => "its own work runs",
        "busy" | "not-idle:busy" => "its turn is still running",
        "not-idle:waiting" => "it asked a question and waits",
        "settling" => "waiting for the tab to settle",
        "attended" => "someone is typing in its tab",
        "awaiting-ready" | "not-ready" => "waiting for it to answer READY",
        "draft" => "a draft waits in its prompt",
        "box" => "a box on its screen waits for a choice",
        "held" => "its tab is held",
        "limited" => "it is at a usage limit",
        "login" => "it is not logged in",
        "in-flight" => "a step is under way",
        w if w.starts_with("not-idle") => "it has not gone idle",
        _ => return None,
    })
}

/// Why a stall of `kind` ([`Row::stall`], every kind but `overdue`) will not
/// move on its own — words that depend on the kind alone, so the tab's mark
/// built from them is sent once per stall. A Codex move that stopped after
/// its `/exit` says where its conversation is ([`codex_failure`]).
fn stall_reason(agent: upgrade::Agent, kind: &str) -> String {
    if agent == upgrade::Agent::Codex
        && let Some(words) = kind.strip_prefix("failed:").and_then(codex_failure)
    {
        return words.to_string();
    }
    match kind {
        "gave-up" => format!(
            "no READY answer it could act on after {} notices",
            upgrade::MAX_ASKS
        ),
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
        s if s.starts_with("failed:") => format!("the move stopped ({})", &s[7..]),
        s if s.starts_with("held-back:") => format!(
            "it runs under {}, which typing into the tab does not reach",
            runs_under(&s[10..])
        ),
        other => other.to_string(),
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
#[must_use]
pub fn rows(opts: &Opts) -> Vec<Row> {
    rows_at(opts, now_s())
}

fn rows_at(opts: &Opts, now: u64) -> Vec<Row> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let mut out: Vec<Row> = dir
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                return None;
            }
            let session = path.file_stem()?.to_string_lossy().into_owned();
            // The relaunch files its records beside the upgrade's
            // (`St::cause`): an agent relaunched after an exit, a memory or a
            // model restart is no upgrade of the tab — the upgrade's own
            // restart of a conversation with no task is.
            let st = load(opts, &session).filter(|st| {
                st.cause.is_empty() || st.cause == super::super::relaunch::CAUSE_UPGRADE_FRESH
            })?;
            let ours = opts.only_sid.as_ref().is_none_or(|s| *s == st.tab);
            ours.then(|| Row::of(&session, &st, now))
        })
        .collect();
    out.sort_by(|a, b| a.session.cmp(&b.session));
    out
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
    let all = rows(&Opts {
        only_sid: None,
        ..opts.clone()
    });
    let vet = sessions_to_vet(&all);
    let (mut shown, vetted) = if vet.is_empty() {
        (all, true)
    } else {
        match holders(&opts.home, &vet, None) {
            Some(held) => (
                all.into_iter()
                    .filter(|r| r.standing(&held, true))
                    .map(|mut r| {
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

/// The conversations whose rows [`Row::standing`] must vet.
fn sessions_to_vet(rows: &[Row]) -> BTreeSet<String> {
    rows.iter()
        .filter(|r| r.held_by_a_process())
        .map(|r| r.session.clone())
        .collect()
}

// ---------------------------------------------------------------- who holds it now

/// ONE LIVE CLAUDE CODE PROCESS a recorded upgrade is vetted against
/// ([`Row::standing`]): the conversation it holds, the build it runs (its
/// session file's `version`, as the running build reports itself), and the
/// tab it is PROVEN to run in — `None` when no proof reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Holder {
    pub(super) session: String,
    pub(super) version: String,
    pub(super) tab: Option<String>,
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
        (
            Row::of(session, st, now).standing(&held, true),
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
                Phase::Failed(why) => {
                    let row = Row::of(&session, &st, now);
                    let next = match row.next_round_in(now) {
                        Some(0) | None => "at its next look".to_string(),
                        Some(secs) => format!("in {}", upgrade::span(secs)),
                    };
                    return Err(format!(
                        "the upgrade in tab {sid} stopped ({}): `--now` re-arms only one that gave \
                         up (no READY answer it could act on after its last notice) — this one \
                         starts a new round on its own {next}; `--skip` keeps it on {}, or quit \
                         it and resume it by hand",
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
/// roster changes, and at the instant [`Self::refresh`] names — never on a
/// timer of its own. What it last told the window and the tabs: the rows it
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
    /// nothing, like a roster that cannot be read.
    fn refresh_with(
        &mut self,
        opts: &Opts,
        tabs: &[LiveTab],
        now: u64,
        holders: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>,
        summary: &mut dyn FnMut(&[Row]),
    ) -> Option<u64> {
        let mut mine: Vec<Row> = rows_at(opts, now)
            .into_iter()
            .filter(|r| tab_is_live(tabs, &r.tab))
            .filter(|r| {
                r.phase != Phase::Done
                    || r.restarting(now)
                    || now.saturating_sub(r.finished_at()) <= DONE_SHOWN_S
            })
            .filter(|r| {
                !r.failed_after_exit() || now.saturating_sub(r.exited_at) <= EXITED_FAILURE_SHOWN_S
            })
            .collect();
        let next = next_change(&mine, now);
        let vet = sessions_to_vet(&mine);
        if !vet.is_empty() {
            let held = holders(&vet)?;
            mine.retain(|r| r.standing(&held, false));
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
        self.mark(opts, &want);
        next
    }

    /// The host stood down (the switch is off, or it is shutting down): an
    /// empty summary once, and every mark it raised lowered.
    pub fn stand_down(&mut self, summary: &mut dyn FnMut(&[Row])) {
        if self.sent.as_ref().is_some_and(|rows| !rows.is_empty()) {
            summary(&[]);
        }
        self.sent = None;
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
/// the owner's `--now` no longer quieting it ([`NOW_QUIETS_S`]), a `--defer`
/// running out, a restart's carry-on no longer waited on for its model
/// ([`Row::confirm_by`]: `restarting` turns `done`), a finished move leaving
/// the summary ([`DONE_SHOWN_S`]), and
/// a Codex move that failed after its `/exit` leaving it
/// ([`EXITED_FAILURE_SHOWN_S`]).
fn next_change(rows: &[Row], now: u64) -> Option<u64> {
    rows.iter()
        .flat_map(|r| {
            let waiting = matches!(r.phase, Phase::Pending | Phase::Announced { .. });
            [
                waiting.then(|| r.behind_since.saturating_add(STALLED_AFTER_S)),
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

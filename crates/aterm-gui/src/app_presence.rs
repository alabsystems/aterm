// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The App's side of presence ([`crate::presence`]).

use std::time::{Duration, Instant};

use aterm_messages::presence::drive::{self, Desk, Shown};
use aterm_session::SessionId;

use crate::app_render::OverlayGlow;
use crate::presence::{self, Facts, Hand, HoldFact, Link, StoryVerb, TurnFact, WindowView};
use crate::{App, WindowId};

/// The chip level the indicator bits spell (`aterm_messages::presence::chip_of_attention`).
pub(crate) use aterm_messages::presence::chip_of_attention;

/// The per-session presence state.
#[derive(Default, Debug)]
pub(crate) struct PresenceTable {
    table: drive::Table<presence::Native>,
    pub(crate) herald: crate::status_item::Herald,
}

impl PresenceTable {
    pub(crate) fn slot(&self, session: u64) -> Option<&presence::Slot> {
        self.table.slot(session)
    }

    pub(crate) fn retire(&mut self, session: u64) {
        self.table.retire(session);
        self.herald.retire(session);
    }
}

/// WHAT ONE LOOK MOVED ([`App::see_front`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) task: bool,
    pub(crate) end: bool,
}

/// The chrome-row re-grid a [`App::with_chrome_regrid_batch`] recorded, to apply at its end.
#[derive(Debug, Default)]
pub(crate) enum ChromeRegridBatch {
    /// None yet.
    #[default]
    Idle,
    /// Every window (`regrid_for_chrome_rows`).
    All,
}

impl ChromeRegridBatch {
    pub(crate) const fn all(&mut self) {
        *self = Self::All;
    }

    const fn covers(&self, _wid: WindowId) -> bool {
        matches!(self, Self::All)
    }
}

#[cfg(test)]
thread_local! {
    static CHROME_REGRIDS_APPLIED: std::cell::RefCell<Vec<(WindowId, u16)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Log an applied chrome re-grid of `wid` with its chrome rows.
#[cfg(test)]
pub(crate) fn note_chrome_regrid_applied(app: &App, wid: WindowId) {
    let rows = app.chrome_rows(wid);
    CHROME_REGRIDS_APPLIED.with(|log| log.borrow_mut().push((wid, rows)));
}

/// Take this thread's applied chrome re-grids, in order: `(window, chrome rows)`.
#[cfg(test)]
pub(crate) fn take_chrome_regrids_applied() -> Vec<(WindowId, u16)> {
    CHROME_REGRIDS_APPLIED.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

#[cfg(test)]
thread_local! {
    static PRESENCE_CLOCK_FOR_TEST: std::cell::Cell<Option<Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Set (or, with `None`, clear) this thread's presence clock for a test.
#[cfg(test)]
pub(crate) fn set_presence_clock_for_test(now: Option<Instant>) {
    PRESENCE_CLOCK_FOR_TEST.with(|c| c.set(now));
}

#[cfg(test)]
thread_local! {
    static NEED_UNIX_CLOCK_FOR_TEST: std::cell::Cell<Option<i64>> =
        const { std::cell::Cell::new(None) };
}

/// Set (or, with `None`, clear) this thread's need wall clock for a test.
#[cfg(test)]
pub(crate) fn set_need_unix_clock_for_test(now: Option<i64>) {
    NEED_UNIX_CLOCK_FOR_TEST.with(|c| c.set(now));
}

/// Whether `program` reports its own boxes (OSC 7501 `blocked`), which a program-claim need
/// reads ([`crate::needs::ProgramClaimFact`]): codex alone. An exhaustive match, so a new agent
/// is placed here before it compiles.
const fn reports_its_boxes(program: aterm_phase::Program) -> bool {
    use aterm_phase::Program as P;
    match program {
        P::Codex => true,
        P::Claude | P::Agy | P::Muse | P::Grok | P::Kimi | P::Generic => false,
    }
}

/// Whether `program`'s sign-in wall folds into the machine's one sign-in
/// (`aterm harness signin --cli <id>`, [`crate::needs::GrokSignin`]): Grok Build's alone.
const fn signs_in_once_per_machine(program: aterm_phase::Program) -> bool {
    use aterm_phase::Program as P;
    match program {
        P::Grok => true,
        P::Claude | P::Codex | P::Agy | P::Muse | P::Kimi | P::Generic => false,
    }
}

fn need_unix_now() -> i64 {
    #[cfg(test)]
    if let Some(now) = NEED_UNIX_CLOCK_FOR_TEST.with(std::cell::Cell::get) {
        return now;
    }
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0))
}

/// Whether window `ws`'s grid re-grids when its chrome rows move.
pub(crate) const fn chrome_regrids_grid(ws: &crate::WindowState) -> bool {
    #[cfg(test)]
    if ws.chrome_regrids_for_test {
        return true;
    }
    ws.os_window.is_some()
}

/// What the macOS `NeedsPill` view (and the `chrome` reader) is handed with the strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NeedsPillModel {
    pub(crate) kind: presence::PillKind,
    pub(crate) words: presence::PillWords,
    pub(crate) session: u64,
}

impl App {
    /// The registry's local id for a fabric `sid`, if the session is live here.
    pub(crate) fn local_session_of(&self, sid: &SessionId) -> Option<u64> {
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(sid)
            .map(|h| h.local_id)
    }

    /// Gather one session's facts: leaf locks only, each held for a copy.
    pub(crate) fn presence_facts(&self, session: u64) -> Option<Facts> {
        let s = self.pool.get(session)?;
        let ctx = &s.ctx;
        let now_us = crate::metrics::now_us();
        let now = Instant::now();
        let (hand, lease_until) = {
            let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
            match lease.as_ref() {
                Some(crate::Lease::Turn { id, driver, .. }) => (
                    Hand::DrivenTurn {
                        id: *id,
                        holder: driver.as_ref().map(|d| self.name_of_sid(d)),
                    },
                    None,
                ),
                Some(crate::Lease::Drive {
                    holder,
                    expires_us,
                    holder_pid,
                    ..
                }) if *expires_us > now_us => (
                    Hand::DrivenLease {
                        holder: holder.clone(),
                        pid: *holder_pid,
                    },
                    Some(now + Duration::from_micros(expires_us - now_us)),
                ),
                _ => (Hand::None, None),
            }
        };
        let hand = if matches!(hand, Hand::None) {
            self.driving_target(&ctx.self_id)
                .map_or(Hand::None, |dst| Hand::Driving {
                    sid: presence::short_sid(dst.as_str()),
                })
        } else {
            hand
        };
        let hold = ctx.fabric.hold().map(|h| HoldFact {
            reason: h.reason,
            fleet: h.origin == "fleet",
        });
        let mail = ctx.fabric.mail_facts();
        let attention = ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .attention
            .clone();
        let link = match crate::fabric::fabric_state_seen() {
            "connected" => Link::Connected {
                rtt_ms: crate::fabric::fabric_link_facts().1,
            },
            "stalled" => Link::Stalled {
                age_ms: crate::fabric::fabric_stalled_ms(),
            },
            "disconnected" => Link::Disconnected,
            _ => Link::Absent,
        };
        let turn = {
            let turns = ctx.turns.lock().unwrap_or_else(|p| p.into_inner());
            turns.high_id().and_then(|id| {
                turns
                    .since(Some(id.saturating_sub(1)))
                    .last()
                    .map(|r| TurnFact {
                        id: r.id,
                        settled: r.status == "settled",
                        dur_ms: r.dur_ms,
                        carried: r.carried,
                        aterm: !r.carried && r.by == crate::turn_ledger::TurnBy::Harness,
                        opened: (!r.carried).then(|| opened_at(r.started_ms)).flatten(),
                    })
            })
        };
        let shell = self
            .session_status
            .status(session)
            .map(|st| (st.phase.as_str(), st.since));
        let program = self
            .session_status
            .program_state(session)
            .map(|(state, since)| (crate::program_state::row_word(state), since));
        let (agent_seq, agent) = self.session_status.agent_reading(session);
        let now_unix = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
        let agent = match self.harness_waits.get(ctx.self_id.as_str()) {
            None => agent,
            Some(plan) => presence::with_retry_plan(agent, Some(plan), now_unix, now),
        };
        let wall_answer =
            self.wall_answer_of(session, ctx, agent.as_ref().map(|a| &a.phase), now_us);
        let input_stall = self.session_status.input_stall(session).cloned();
        let (need, held_by_person) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let verdict = tl.need();
            (
                verdict.need.as_ref().map(presence::need_fact),
                verdict.held == Some(aterm_types::needs::HoldWord::Person),
            )
        };
        Some(Facts {
            attention,
            shell,
            program,
            agent_seq: agent_seq + self.harness_wait_epoch,
            agent,
            hand,
            lease_until,
            hold,
            mail,
            link,
            turn,
            input_stall,
            wall_answer,
            need,
            held_by_person,
            program_turn_ends: self.session_status.program_turn_ends(session),
            program_turn_starts: self.session_status.program_turn_starts(session),
            program_turn_began: self.session_status.program_turn_began(session),
            program_turn_watched: self
                .windows
                .iter()
                .any(|(wid, ws)| ws.focused && self.focused_session_id(*wid) == Some(session)),
        })
    }

    fn wall_answer_of(
        &self,
        session: u64,
        ctx: &crate::SessionCtx,
        phase: Option<&presence::AgentPhase>,
        now_us: u64,
    ) -> presence::WallAnswer {
        let (claimant_named, front_group) = {
            let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let agent = timeline.agent();
            (
                crate::self_claim::fronts_claimant(agent.program.as_deref()),
                agent.program_pgid,
            )
        };
        let claimant_in_front = claimant_named
            && crate::self_claim::fronts_claimant(
                self.session_status.agent_reading_program(session),
            );
        match (phase, self.harness.as_ref()) {
            (Some(presence::AgentPhase::Wall { kind, reset, .. }), Some(host))
                if ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .live_supervisor(now_us)
                    .is_some_and(|holder| holder == crate::harness_host::holder()) =>
            {
                let program = {
                    let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                    crate::harness_host::agent_of(timeline.agent())
                };
                program.map_or(presence::WallAnswer::Nobody, |agent| {
                    host.wall_answer(agent, *kind, reset.is_some())
                })
            }
            (Some(presence::AgentPhase::Wall { kind, reset, .. }), _)
                if claimant_in_front
                    && ctx
                        .meta
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .live_claim(now_us)
                        .and_then(|claim| claim.proven.as_ref())
                        .is_some_and(|proven| i64::from(proven.pid) == i64::from(front_group)) =>
            {
                self.harness_policy_facts(session)
                    .map_or(presence::WallAnswer::Nobody, |facts| {
                        crate::self_claim::wall_answer(
                            *kind,
                            reset.is_some(),
                            &facts.policy,
                            facts.effective.0,
                        )
                    })
            }
            _ => presence::WallAnswer::Nobody,
        }
    }

    /// ONE SESSION'S NEED FACTS ([`crate::needs::NeedFacts`], `docs/DESIGN-owner-surfaces-2026-10-16.md` §2.3), under the leaf locks the status glance already takes, each released before the next.
    pub(crate) fn need_facts(
        &self,
        h: &crate::session_store::SessionHandle,
        hosting: &crate::harness_reach::HarnessHosting,
        now_us: u64,
        now_unix: i64,
    ) -> crate::needs::NeedFacts {
        let word = crate::autopilot::word_of(&h.ctx);
        self.need_facts_with(
            h,
            |agent| hosting.hosts(h.sid.as_str(), word, agent),
            &hosting.relaunching,
            now_us,
            now_unix,
        )
    }

    /// [`Self::need_facts`] with the host's answer for the session's agent (`hosted`, asked after the timeline's leaf lock is released) and the sessions the host would relaunch passed in: the seam a test stands a host up by.
    pub(crate) fn need_facts_with(
        &self,
        h: &crate::session_store::SessionHandle,
        hosted: impl FnOnce(aterm_phase::Program) -> bool,
        relaunching: &std::collections::HashSet<String>,
        now_us: u64,
        now_unix: i64,
    ) -> crate::needs::NeedFacts {
        let ctx = &h.ctx;
        let (attention, supervisor, self_claimed) = {
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let mut attention = meta.attention_owners.kinded();
            if attention.is_empty()
                && let Some(text) = meta.attention.clone()
            {
                attention.push(crate::needs::AttentionFact {
                    owner: crate::session_timeline::BARE_ATTENTION_OWNER.to_string(),
                    text,
                    kind: Some(aterm_types::needs::AttentionKind::Ask),
                });
            }
            (
                attention,
                meta.live_claim(now_us)
                    .map(|claim| (claim.holder.clone(), claim.speaks_for_boxes_and_walls())),
                meta.supervisor_proven(now_us),
            )
        };
        let restarts = relaunching.contains(h.ctx.self_id.as_str())
            && crate::input_stall::host_hand_refusal(ctx, now_us).is_none();
        let (agent, program_blocked, admit, stall, task, resume, age_ms, told) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let a = tl.agent();
            let agent = (a.word != "-").then(|| crate::needs::AgentFact {
                word: a.word,
                detail: a.detail.clone(),
                rev: a.rev,
                subject: a.subject.clone(),
            });
            // Codex's own claim, in its own field: only where its reader names no state.
            let program_blocked = tl
                .program_shown()
                .filter(|_| {
                    a.word == "unknown"
                        && crate::harness_host::identified_agent(a).is_some_and(reports_its_boxes)
                })
                .filter(|shown| {
                    shown.state.state
                        == aterm_core::terminal::program_state::ProgramStateWord::Blocked
                })
                .map(|shown| crate::needs::ProgramClaimFact {
                    kind: shown.state.kind,
                    key: shown.seq,
                    report: shown.report,
                });
            let stall = a.input.clone().map(|f| {
                let after = crate::input_stall::after_restart(&tl, f.act.as_ref());
                (f, a.program.clone(), after)
            });
            let task = tl.task().map(|t| crate::needs::TaskFact {
                stalled: t.state == crate::task_record::TaskState::Stalled,
                why: t.why,
                entered: t.entered,
            });
            let resume = tl.claude_footer().and_then(|facts| facts.resume.clone());
            let admit = (crate::harness_host::agent_of(a), a.program_pgid);
            let told = tl.harness_needs();
            // The walled program's group leader, whose start says whether a login predates it.
            let grok_walled = (a.word == "wall:auth"
                && crate::harness_host::identified_agent(a).is_some_and(signs_in_once_per_machine))
            .then_some(a.program_pgid);
            (
                agent,
                program_blocked,
                admit,
                stall,
                task,
                resume,
                a.age_ms(),
                (told, grok_walled),
            )
        };
        let (told, grok_walled) = told;
        // Read only for a Grok session on a sign-in wall: a record read and a file stat.
        let grok_signin = grok_walled.map(grok_signin_now);
        let host_supervises =
            crate::harness_host::admitted_as(ctx, admit.0, admit.1).is_some_and(hosted);
        let holds = ctx.fabric.holds();
        let hold = holds.word();
        let stall = stall.map(|(fact, program, after)| {
            let row = crate::input_stall::menu_row(
                &fact,
                program.as_deref(),
                &after,
                h.ctx.self_id.as_str(),
                Instant::now(),
            );
            let leader = fact.restart.leader;
            let act = !fact.stopped && crate::restart_act::offered(ctx, leader, now_us).is_some();
            let hand_back_frees = !fact.stopped
                && !act
                && holds.hand_back_origin().is_some()
                && holds.standing().count() == 1
                && crate::restart_act::offered_but_for_hold(ctx, leader, now_us);
            crate::needs::StallFact {
                text: row.text,
                key: row.key,
                stopped: row.stopped,
                act,
                hand_back_frees,
            }
        });
        let (_, reading) = self.session_status.agent_reading(h.local_id);
        let wall_answer =
            self.wall_answer_of(h.local_id, ctx, reading.as_ref().map(|r| &r.phase), now_us);
        let printed = now_unix.saturating_sub(i64::try_from(age_ms / 1000).unwrap_or(0));
        let reset_unix = agent
            .as_ref()
            .filter(|a| a.word.starts_with("wall:"))
            .and_then(|a| a.detail.as_deref())
            .and_then(|reset| presence::place_reset(reset, printed));
        // A scoped claim stands aterm down for exactly the duties it names (§8.1 item 4): one that
        // does not speak for boxes and walls (grok-alab's `relaunch,upgrade`) leaves its boxes,
        // walls and codex's own reports raised for the person.
        let other_supervisor = supervisor
            .as_ref()
            .is_some_and(|(holder, speaks)| *speaks && *holder != crate::harness_host::holder());
        crate::needs::NeedFacts {
            stall,
            task,
            attention,
            agent,
            program_blocked,
            self_claimed,
            supervised: host_supervises || other_supervisor,
            other_supervisor,
            claim_live: supervisor.is_some(),
            wall_answer,
            restarts: restarts && host_supervises,
            resume,
            hold,
            reset_unix,
            now_unix,
            told,
            grok_signin,
        }
    }

    /// THE NEED PUBLICATION (§2.3).
    pub(crate) fn publish_need(&mut self, session: u64) -> bool {
        let started = Instant::now();
        let moved = self.publish_need_timed(session);
        crate::metrics::note_need_publish(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        moved
    }

    fn publish_need_timed(&mut self, session: u64) -> bool {
        let hosting = self.harness_hosting();
        let now_us = crate::metrics::now_us();
        let now_unix = need_unix_now();
        let handle = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .cloned();
        let Some(h) = handle else {
            self.forget_need(session, now_unix);
            return false;
        };
        let facts = (!matches!(h.state, crate::session_store::SessionState::Exited))
            .then(|| self.need_facts(&h, &hosting, now_us, now_unix));
        let ctx = h.ctx;
        let verdict = facts.as_ref().map(crate::needs::of).unwrap_or_default();
        self.note_api_outage(
            session,
            verdict
                .waiting
                .as_ref()
                .is_some_and(|w| w.word == aterm_types::needs::WaitWord::Api),
            now_unix,
        );
        match verdict.recheck_unix {
            Some(at) => {
                self.need_rechecks.insert(session, at);
            }
            None => {
                self.need_rechecks.remove(&session);
            }
        }
        let held = verdict.held;
        let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        if tl.need().held != held {
            self.agents.long_holds_moved = true;
        }
        let title_moved = {
            let (was, now) = (tl.need().harness_ask(), verdict.harness_ask());
            was.map(|a| (a.first.as_str(), a.more)) != now.map(|a| (a.first.as_str(), a.more))
        };
        let moved = tl.set_need(verdict);
        let _ = tl.slog_take_wake();
        drop(tl);
        if title_moved {
            crate::harness_glass::on_need_words(self, session);
            self.redraw_autopilot_title_readers(session);
        }
        moved
    }

    fn forget_need(&mut self, session: u64, now_unix: i64) {
        self.need_rechecks.remove(&session);
        self.note_api_outage(session, false, now_unix);
    }

    fn note_api_outage(&mut self, session: u64, waits: bool, now_unix: i64) {
        let moved = if waits {
            self.api_outage.insert(session, now_unix).is_none()
        } else {
            self.api_outage.remove(&session).is_some()
        };
        if moved {
            self.hand_api_outage();
        }
    }

    /// THE API OUTAGE'S WHOLE ANSWER (`Family::ApiOutage`, a level family).
    pub(crate) fn hand_api_outage(&mut self) {
        let entries: Vec<aterm_messages::exceptions::Entry> = match self.api_outage.values().min() {
            None => Vec::new(),
            Some(since) => {
                let hhmm = crate::presence::local_offset_s().map(|offset| {
                    let local = (since + offset).rem_euclid(86_400);
                    format!("{:02}:{:02}", local / 3600, (local % 3600) / 60)
                });
                vec![crate::message_reporters::api_outage_entry(
                    self.api_outage.len(),
                    hhmm.as_deref(),
                )]
            }
        };
        self.except_replace(aterm_messages::exceptions::Family::ApiOutage, &entries);
    }

    /// Every session's need published again (and its herald and chrome refreshed).
    pub(crate) fn publish_needs_all(&mut self) {
        let sessions: Vec<u64> = self.pool.iter().map(|session| session.id).collect();
        for session in sessions {
            self.refresh_presence_session(session);
        }
    }

    /// The earliest need recheck (`need_rechecks`), for the presence timer, placed on `now`'s clock from the wall clock each time it is asked.
    pub(crate) fn need_recheck_deadline(&self, now: Instant) -> Option<Instant> {
        let at = *self.need_rechecks.values().min()?;
        let wait = u64::try_from(at.saturating_sub(need_unix_now())).unwrap_or(0);
        Some(now + Duration::from_secs(wait))
    }

    fn need_rechecks_due(&mut self) {
        let now_unix = need_unix_now();
        let due: Vec<u64> = self
            .need_rechecks
            .iter()
            .filter(|(_, at)| **at <= now_unix)
            .map(|(session, _)| *session)
            .collect();
        for session in due {
            self.need_rechecks.remove(&session);
            self.refresh_presence_session(session);
        }
    }

    fn name_of_sid(&self, sid: &SessionId) -> String {
        let role = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(sid)
            .and_then(|h| {
                h.ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .role
                    .clone()
            });
        role.unwrap_or_else(|| presence::short_sid(sid.as_str()))
    }

    fn driving_target(&self, src: &SessionId) -> Option<SessionId> {
        let mut peers: Vec<_> = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .live_handles()
            .filter(|h| &h.sid != src)
            .map(|h| (h.local_id, std::sync::Arc::clone(&h.ctx)))
            .collect();
        peers.sort_by_key(|&(id, _)| id);
        peers.iter().find_map(|(_, ctx)| {
            let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
            matches!(
                lease.as_ref(),
                Some(crate::Lease::Turn { driver: Some(d), .. }) if d == src
            )
            .then(|| ctx.self_id.clone())
        })
    }

    /// A fact about `session` changed.
    pub(crate) fn refresh_presence_session(&mut self, session: u64) {
        drive::refresh_session(self, session);
    }

    /// The harness loop's next try for `sid` ([`crate::Wake::HarnessWait`]).
    pub(crate) fn apply_harness_wait(&mut self, sid: &str, plan: Option<presence::RetryPlan>) {
        let changed = match plan {
            Some(plan) => {
                let was = self.harness_waits.insert(sid.to_string(), plan.clone());
                was.as_ref() != Some(&plan)
            }
            None => self.harness_waits.remove(sid).is_some(),
        };
        if !changed {
            return;
        }
        self.harness_wait_epoch += 1;
        if let Some(session) = self.local_session_of(&SessionId::new(sid)) {
            self.refresh_presence_session(session);
        }
    }

    /// What the harness loop of `sid` told the window.
    pub(crate) fn apply_harness_fact(&mut self, sid: &str, fact: crate::HarnessFact) {
        let Some(session) = self.local_session_of(&SessionId::new(sid)) else {
            return;
        };
        match fact {
            crate::HarnessFact::Act(act) => crate::harness_glass::on_act(self, session, act),
            crate::HarnessFact::Task(face) => {
                // Nor is its face in the gap of the self-claim's own re-exec
                // (`upgrade@v1`, then the claim given back and taken again):
                // the self-claim's face stands across it.
                if self.beside_proven_self_claim(session) || self.in_self_claim_reexec(session) {
                    self.clear_land_held(session);
                    return;
                }
                self.apply_task_face(session, face, None, None);
                if face == Some(aterm_agent::harness::acts::TaskFace::Theirs) {
                    self.clear_land_held(session);
                }
            }
            crate::HarnessFact::TaskNoted(face, noted_ms) => {
                if self.beside_proven_self_claim(session) || self.in_self_claim_reexec(session) {
                    return;
                }
                self.apply_task_face(session, Some(face), None, Some(noted_ms));
            }
            crate::HarnessFact::SelfTask(_) => {
                let faces = {
                    let Some(entry) = self.pool.get(session) else {
                        return;
                    };
                    let meta = entry.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                    let faces = timeline.take_self_tasks();
                    if !meta.supervisor_proven(crate::metrics::now_us()) {
                        return;
                    }
                    faces
                };
                let mut holds = false;
                for face in faces {
                    // Nothing ends a Cruise or Off task. Open acts still
                    // apply, including one after a skipped landing.
                    if face == aterm_agent::harness::acts::TaskFace::Landed
                        && !self.self_claim_may_land(session)
                    {
                        continue;
                    }
                    self.apply_task_face(
                        session,
                        Some(face),
                        Some(aterm_agent::harness::acts::TaskFace::Open),
                        None,
                    );
                    holds = crate::harness_task::holds_until_another_turn(face);
                }
                if holds {
                    self.reopen_self_claim_landing(session);
                }
                self.clear_land_held(session);
            }
            crate::HarnessFact::Pause(plan) => {
                let Some(entry) = self.pool.get(session) else {
                    return;
                };
                let moved = entry
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .set_pause(plan);
                if crate::harness_glass::on_pause(self, session, plan) {
                    self.redraw_autopilot_title_readers(session);
                    self.note_harness_arm(session);
                } else if !moved {
                    return;
                }
                self.harness_screen_moved();
            }
            crate::HarnessFact::Needs => {
                let was = self
                    .presence
                    .slot(session)
                    .and_then(|slot| slot.need.clone());
                self.refresh_presence_session(session);
                let now = self
                    .presence
                    .slot(session)
                    .and_then(|slot| slot.need.as_ref());
                if now == was.as_ref() {
                    self.restamp_for_hover(session);
                }
            }
            crate::HarnessFact::LandHeld => {
                if self.beside_proven_self_claim(session) {
                    self.clear_land_held(session);
                    return;
                }
                let Some(entry) = self.pool.get(session) else {
                    return;
                };
                let held = entry
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .land_held();
                self.show_land_held(session, held);
            }
        }
    }

    /// LAND'S HOLD OF SESSION `session` MOVED TO `held` on its glass.
    pub(crate) fn show_land_held(
        &mut self,
        session: u64,
        held: Option<aterm_agent::harness::acts::LandHeld>,
    ) {
        let hover_was = self.driven_by_aterm(session);
        if !crate::harness_glass::on_land_held(self, session, held) {
            return;
        }
        self.redraw_autopilot_title_readers(session);
        self.harness_screen_moved();
        if self.driven_by_aterm(session) == hover_was {
            self.mark_status_dirty();
        } else {
            self.restamp_for_hover(session);
        }
    }

    /// ATERM STOPPED TYPING INTO SESSION `session`, or no longer does (§14.3): `stopped` on its
    /// glass ([`crate::harness_glass::on_stopped`]), and every surface that says it read again —
    /// the chip's title where it is hovered, Settings ▸ Harness, the hover and the status item.
    pub(crate) fn show_stopped(
        &mut self,
        session: u64,
        stopped: Option<aterm_agent::harness::acts::HeldBy>,
    ) {
        let hover_was = self.driven_by_aterm(session);
        if !crate::harness_glass::on_stopped(self, session, stopped) {
            return;
        }
        self.redraw_autopilot_title_readers(session);
        self.harness_screen_moved();
        if self.driven_by_aterm(session) == hover_was {
            self.mark_status_dirty();
        } else {
            self.restamp_for_hover(session);
        }
    }

    /// A STALE HOLD GOES (§12.3.3 item 3).
    pub(crate) fn clear_land_held(&mut self, session: u64) {
        if let Some(entry) = self.pool.get(session) {
            entry
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .set_land_held(None);
        }
        self.show_land_held(session, None);
    }

    fn self_claim_may_land(&self, session: u64) -> bool {
        let (word, view, _) = self.session_autopilot(session);
        view.mode()
            .or(word)
            .is_none_or(|mode| mode == aterm_agent::supervise::Autopilot::Land)
    }

    /// The wake arms: resolve the fabric sid to the pool's local id.
    pub(crate) fn on_presence_wake(&mut self, sid: &SessionId) {
        if let Some(session) = self.local_session_of(sid) {
            self.refresh_presence_session(session);
        }
    }

    /// The `Wake::FabricChanged` arm: session `sid`'s fabric endpoint moved.
    pub(crate) fn on_fabric_changed(&mut self, sid: &SessionId) {
        let Some(session) = self.local_session_of(sid) else {
            return;
        };
        self.refresh_presence_session(session);
        self.let_go_lifted_held(session);
        self.journal_owner_words(session);
    }

    /// Project the window's most pressing session onto its view (`drive::refresh_window`).
    pub(crate) fn refresh_presence_window(&mut self, wid: WindowId) {
        drive::refresh_window(self, wid);
    }

    /// Every window: a restore, a successor's commit, a handoff's release and a config adopt.
    pub(crate) fn refresh_presence_all_windows(&mut self) {
        self.rebuild_teams();
        drive::refresh_all(self);
    }

    /// The story read (`drive::human_acted`): the human acted in `wid`.
    #[cfg(test)]
    pub(crate) fn note_human_acted(&mut self, wid: WindowId) {
        drive::human_acted(self, wid);
    }

    /// One key from the winit `KeyboardInput` arm.
    pub(crate) fn dispatch_hw_key(&mut self, wid: WindowId, event: aterm_window::event::KeyEvent) {
        let pressed = event.state == aterm_window::event::ElementState::Pressed;
        let front = self.focused_session_id(wid);
        let seen = front.filter(|_| pressed);
        let echo = self.echo_probe_open(wid, front);
        self.on_key(wid, event);
        self.echo_probe_close(echo);
        if let Some(seen) = seen
            && self.focused_session_id(wid) == Some(seen)
        {
            self.note_human_acted_on(wid, seen);
        }
    }

    /// The story read for a press (`drive::human_acted_on`).
    pub(crate) fn note_human_acted_on(&mut self, wid: WindowId, session: u64) {
        drive::human_acted_on(self, wid, session);
    }

    /// Re-read every pooled session's facts at a successor's Commit.
    #[cfg(unix)]
    pub(crate) fn arm_supervisor_expiries(&mut self) {
        let sessions: Vec<u64> = self.pool.iter().map(|session| session.id).collect();
        for session in sessions {
            self.refresh_presence_session(session);
        }
    }

    /// The one instant the presence timer arms (`drive::deadline`): `None` on a quiet desktop.
    pub(crate) fn presence_deadline(&self, now: Instant) -> Option<Instant> {
        drive::deadline(self, now)
    }

    /// The timer's tick (`drive::tick`): the windows that need a repaint.
    pub(crate) fn presence_tick(&mut self, now: Instant) -> Vec<WindowId> {
        drive::tick(self, now)
    }

    /// The border as an [`OverlayGlow`] for the frame path.
    pub(crate) fn presence_overlay(&self, wid: WindowId) -> Option<OverlayGlow> {
        let ws = self.windows.get(&wid)?;
        let glow = ws.presence.rim_glow(|| {
            crate::chrome_band::presence_tones(
                self.chrome_palette(crate::app_render::ChromeSurface::Header),
            )
        })?;
        Some(OverlayGlow {
            accent: pack(glow.accent),
            wash_a: glow.wash_a,
            border_a: glow.border_a,
            border_scale_q4: glow.border_scale_q4,
        })
    }

    /// The repaint-key term for `wid`: 0 on a quiet window.
    pub(crate) fn presence_fp(&self, wid: WindowId) -> u64 {
        self.windows.get(&wid).map_or(0, |ws| ws.presence.fp())
    }

    /// The projected pill window `wid` shows now, as the strip carries it ([`NeedsPillModel`]).
    pub(crate) fn presence_pill(&self, wid: WindowId) -> Option<NeedsPillModel> {
        let pill = self.windows.get(&wid)?.presence.pill.as_ref()?;
        Some(NeedsPillModel {
            kind: pill.kind,
            words: presence::pill_words(pill, Desk::clock(self)),
            session: pill.session,
        })
    }

    /// Whether window `wid`'s IN-GRID strip carries its needs pill.
    pub(crate) fn strip_carries_pill(&self, wid: WindowId) -> bool {
        !(cfg!(target_os = "macos")
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.os_window.is_some()))
    }

    /// The in-grid strip's cache term for window `wid`'s pill.
    pub(crate) fn presence_pill_key(&self, wid: WindowId) -> Option<(u64, Option<Instant>)> {
        let view = &self.windows.get(&wid)?.presence;
        let pill = view.pill.as_ref()?;
        let now = Desk::clock(self);
        Some((
            view.fp(),
            pill.since
                .map(|since| aterm_messages::presence::pill_due(since, now)),
        ))
    }

    /// Stamp each tab's mark onto the strip metadata.
    pub(crate) fn stamp_presence_chips(
        &self,
        wid: WindowId,
        metadata: &mut [crate::tab_bar::TabStripMetadata],
    ) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let front = ws.tab_set.active_index();
        let tab_count = ws.tab_set.len();
        let denial_marks =
            self.effective_observer_mode() == crate::consent_observer::ObserverMode::Full;
        for (index, (tab, item)) in ws
            .tab_set
            .tabs()
            .iter()
            .zip(metadata.iter_mut())
            .enumerate()
        {
            item.load = self.chip_load(tab_count, tab);
            let mut level = item.attention;
            tab.root.any_leaf(&mut |view| {
                if let Some(session) = self
                    .view_store
                    .get(*view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                {
                    if self.consent_observer.marks_session(session)
                        || (denial_marks && self.consent_attention.marks_session(session))
                    {
                        level = level.max(presence::ChipLevel::Note);
                    }
                    if let Some(slot) = self.presence.slot(session) {
                        level = level.max(slot.chip());
                    }
                    if Some(index) != front && self.done_unseen(session) {
                        level = level.max(presence::ChipLevel::Done);
                    }
                }
                false
            });
            item.attention = level;
            item.team = self
                .view_store
                .get(tab.focus)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                .and_then(|session| self.teams.get(&session))
                .map_or(crate::tab_bar::TabTeam::NONE, crate::team::Team::strip);
        }
    }

    /// The pill's a11y message: the projected pill's words, a polite status with no bar row.
    #[cfg(a11y_tree)]
    pub(crate) fn presence_a11y_message(
        &self,
        wid: WindowId,
    ) -> Option<crate::a11y_tree::GridMessage> {
        let pill = self.windows.get(&wid)?.presence.pill.as_ref()?;
        let now = Desk::clock(self);
        let spoken = presence::pill_words(
            &aterm_messages::presence::Pill {
                since: None,
                ..pill.clone()
            },
            now,
        )
        .text();
        let aged = presence::pill_words(pill, now).text();
        Some(crate::a11y_tree::GridMessage {
            message: crate::a11y_tree::ChromeMessage::PresenceStatus,
            context: None,
            detail: (aged != spoken).then_some(aged),
            text: spoken,
            progress: None,
            busy: false,
            alarm: matches!(
                pill.kind,
                presence::PillKind::Need(presence::NeedSeverity::Stuck)
            ),
            activates: false,
            bar_row: None,
            capsules: Vec::new(),
        })
    }

    fn done_unseen(&self, session: u64) -> bool {
        self.task_done
            .get(&session)
            .is_some_and(|rev| self.done_seen.get(&session) != Some(rev))
            || self.turn_ends_unseen(session).is_some()
            || self.end_unseen(session)
    }

    /// Whether `session`'s task end stands on its glass unseen (§11.2.3, item 5): a map lookup.
    pub(crate) fn end_unseen(&self, session: u64) -> bool {
        self.harness_glass
            .get(&session)
            .and_then(crate::harness_glass::SessionGlass::end)
            .is_some_and(crate::harness_glass::EndGlass::unseen)
    }

    fn turn_ends_unseen(&self, session: u64) -> Option<u64> {
        let news = self.presence.slot(session)?.turn_end_news;
        (news > self.turn_end_seen.get(&session).copied().unwrap_or(0)).then_some(news)
    }

    /// THE LOOK (§14.11 of `docs/DESIGN-harness-autopilot-2026-10-15.md`): window `wid` is the frontmost window, the OS made it its key window (aterm is the active app and this its key window: `WindowState::os_key`, which a controller's `focus in|out` never writes), and it is on screen — or, in a development build, the `person look` stand-in looks at it in place of a headless window. A window left frontmost while he works in another app looks at nothing: what lands in its front tab then stands until he comes back to it.
    fn window_looked_at(&self, wid: WindowId) -> bool {
        #[cfg(any(test, debug_assertions))]
        let stood_in = self.person_look == Some(wid)
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.os_window.is_none());
        #[cfg(not(any(test, debug_assertions)))]
        let stood_in = false;
        self.frontmost_window == Some(wid)
            && (stood_in
                || (self.windows.get(&wid).is_some_and(|ws| ws.os_key)
                    && Desk::pill_visible(self, wid)))
    }

    /// `aterm ctl person …` on the main thread.
    pub(crate) fn person_stand_in(
        &mut self,
        asked: crate::control::control_media::PersonAsked,
    ) -> Result<String, String> {
        use crate::control::control_media::PersonAsked;
        #[cfg(any(test, debug_assertions))]
        {
            match asked {
                PersonAsked::Look(window) => self.person_look(window.map(WindowId)),
                PersonAsked::Press(at) => {
                    let proxy = self.proxy.clone();
                    self.person_menubar_press(&at, &mut |wake| {
                        proxy.as_ref().is_some_and(|p| p.send_event(wake).is_ok())
                    })
                }
            }
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            match asked {
                PersonAsked::Look(_) | PersonAsked::Press(_) => {
                    Err(crate::control::control_media::PERSON_NEEDS_DEBUG.to_string())
                }
            }
        }
    }

    /// A DEVELOPMENT BUILD'S STAND-IN FOR THE PERSON LOOKS AT `window`.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn person_look(&mut self, window: Option<WindowId>) -> Result<String, String> {
        let Some(wid) = window else {
            self.person_look = None;
            return Ok("person look off".to_string());
        };
        let Some(ws) = self.windows.get(&wid) else {
            return Err(format!("no window {}", wid.0));
        };
        if ws.os_window.is_some() {
            return Err("a window looks for itself".to_string());
        }
        self.person_look = Some(wid);
        self.note_done_seen(wid);
        Ok(format!("person look window={}", wid.0))
    }

    /// `wid`'s front tab is looked at — where someone can (§3: the owner focuses or looks at the tab), never in a window behind, minimized, occluded or headless.
    pub(crate) fn note_done_seen(&mut self, wid: WindowId) {
        let seen = self.see_front(wid);
        if seen.end {
            self.restamp_for_end();
        } else if seen.task {
            self.mark_status_dirty();
        }
    }

    /// THE LOOK ITSELF ([`Self::note_done_seen`]).
    pub(crate) fn see_front(&mut self, wid: WindowId) -> Seen {
        let mut seen = Seen::default();
        if !self.window_looked_at(wid) {
            return seen;
        }
        let Some(tab) = self.windows.get(&wid).and_then(|ws| ws.tab_set.active()) else {
            return seen;
        };
        let shown = if tab.zoomed {
            vec![tab.focus]
        } else {
            tab.root.leaves()
        };
        for view in shown {
            let Some(session) = self
                .view_store
                .get(view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                continue;
            };
            if let Some(rev) = self.task_done.get(&session).copied() {
                seen.task |= self.done_seen.insert(session, rev) != Some(rev);
            }
            if let Some(news) = self.turn_ends_unseen(session) {
                self.turn_end_seen.insert(session, news);
            }
            if let Some(glass) = self.harness_glass.get_mut(&session) {
                seen.end |= glass.see_end();
            }
        }
        seen
    }

    /// THE ONE WRITE OF A DONE SEEN BY AN ACT (owner surfaces §3).
    pub(crate) fn mark_done_seen(&mut self, session: u64, entered: u64) {
        if self.done_seen.insert(session, entered) == Some(entered) {
            return;
        }
        self.mark_status_dirty();
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in &windows {
            Desk::request_redraw(self, *wid);
        }
        self.refresh_tab_chrome_windows(windows);
    }

    /// A TASK'S END MOVED ON THE TAB'S `•` (§11.2.3, items 6 and 7).
    pub(crate) fn restamp_for_end(&mut self) {
        #[cfg(test)]
        crate::work_counts::end_restamped();
        self.mark_status_dirty();
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in &windows {
            Desk::request_redraw(self, *wid);
        }
        self.refresh_tab_chrome_windows(windows);
    }

    /// THE TAB'S HOVER NAMES WHO DRIVES IN WORDS THAT MOVED for session `session`.
    pub(crate) fn restamp_for_hover(&mut self, session: u64) {
        for wid in self.windows_with_focused_session(session) {
            let (titles, metadata) = self.refresh_window_tabs(wid);
            let fp = self.tab_strip_fingerprint_from_parts(wid, &titles, &metadata);
            let Some(ws) = self.windows.get(&wid) else {
                continue;
            };
            if ws.last_present.as_ref().is_none_or(|k| k.tab_strip != fp) {
                #[cfg(test)]
                crate::work_counts::hover_redraw_asked();
                if let Some(window) = ws.os_window.as_ref() {
                    window.request_redraw();
                }
            }
        }
        self.mark_status_dirty();
    }

    /// What the tab's hover says after `driven by aterm` for session `session` ([`driven_words`]).
    pub(crate) fn driven_by_aterm(&self, session: u64) -> Option<String> {
        let (_, view, supervised) = self.session_autopilot(session);
        if !supervised {
            return None;
        }
        let glass = self.chip_glass(session);
        let own = self.harness_glass.get(&session);
        let hold = own.and_then(crate::harness_glass::SessionGlass::hold);
        driven_words(
            crate::claude_lights::ChipFace::of(view, &glass),
            &glass,
            hold.as_ref(),
            false,
            own.and_then(crate::harness_glass::SessionGlass::stopped),
        )
    }

    /// WHO DRIVES (owner surfaces §5, S9).
    pub(crate) fn rebuild_teams(&mut self) {
        let handles: Vec<(u64, String, std::sync::Arc<crate::SessionCtx>)> = self
            .pool
            .iter()
            .map(|s| {
                (
                    s.id,
                    s.ctx.self_id.as_str().to_string(),
                    std::sync::Arc::clone(&s.ctx),
                )
            })
            .collect();
        let mut rows = Vec::with_capacity(handles.len());
        let mut done = std::collections::HashMap::new();
        for (local, sid, ctx) in handles {
            let name = self.tab_label_of_session(local);
            let task = ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task()
                .map(|t| (t.from.clone(), t.state, t.entered, t.since));
            if let Some((_, crate::task_record::TaskState::Done, entered, _)) = &task {
                done.insert(local, *entered);
            }
            rows.push(crate::team::Row {
                local,
                name,
                sid,
                task: task
                    .as_ref()
                    .map(|(_, state, _, since)| (state.as_str(), *since)),
                from: task
                    .map(|(from, ..)| from)
                    .filter(|from| from != crate::task_record::FROM_CTL),
            });
        }
        let teams = crate::team::table(&rows);
        if teams == self.teams && done == self.task_done {
            return;
        }
        self.teams = teams;
        self.task_done = done;
        if let Some(front) = self.frontmost_window {
            self.note_done_seen(front);
        }
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in &windows {
            Desk::request_redraw(self, *wid);
        }
        self.refresh_tab_chrome_windows(windows);
    }

    /// What `session`'s Team chip says ([`crate::claude_lights::TeamFace`]).
    pub(crate) fn team_face(&self, session: u64) -> Option<crate::claude_lights::TeamFace> {
        use crate::claude_lights::TeamFace;
        if self
            .presence
            .slot(session)
            .is_some_and(|slot| slot.held_by_person)
        {
            return Some(TeamFace::Yours);
        }
        let team = self.teams.get(&session)?;
        if let Some(manager) = &team.manager {
            return Some(TeamFace::Worker {
                manager: manager.local,
                label: std::sync::Arc::clone(&manager.label),
                task: team.task,
            });
        }
        (team.workers > 0).then(|| TeamFace::Manager {
            mark: std::sync::Arc::clone(&team.workers_mark),
            workers: team.workers,
        })
    }

    /// The footer's repaint term for `session`'s Team chip.
    pub(crate) fn team_term(&self, session: u64) -> u64 {
        let held = self
            .presence
            .slot(session)
            .is_some_and(|slot| slot.held_by_person);
        self.teams.get(&session).map_or(0, crate::team::Team::term)
            ^ u64::from(held).rotate_left(61)
    }

    /// A tab's HOVER, with its presence lines (§5).
    pub(crate) fn presence_hover(&self, session: u64, tooltip: Option<String>) -> Option<String> {
        let prefix = crate::session_chrome::NEEDS_ATTENTION_PREFIX;
        let mut lines: Vec<String> = tooltip
            .as_deref()
            .map(|t| {
                t.lines()
                    .filter(|line| !line.starts_with(prefix))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let slot = self.presence.slot(session);
        let (program, held, ask) = self.pool.get(session).map_or((None, None, None), |s| {
            let tl = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let verdict = tl.need();
            (
                tl.agent().program.clone(),
                verdict.held,
                verdict.harness_ask().cloned(),
            )
        });
        if let Some(need) = slot.and_then(|slot| slot.need.as_ref()) {
            match &ask {
                Some(ask) => lines.extend(harness_need_hover(ask)),
                None => lines.push(format!("{prefix}{}", need.what)),
            }
        }
        if let Some(line) = self.who_drives_line(session, held) {
            lines.push(line);
        }
        if let Some(line) = slot
            .filter(|slot| slot.agent.is_some())
            .and_then(|slot| presence::state_line(slot, program.as_deref()))
        {
            lines.push(line);
        }
        if let Some(line) = self
            .pool
            .get(session)
            .and_then(|s| self.typed_hover_line(&s.ctx))
        {
            lines.push(line);
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    fn who_drives_line(
        &self,
        session: u64,
        held: Option<aterm_types::needs::HoldWord>,
    ) -> Option<String> {
        use aterm_types::needs::HoldWord;
        let mut parts: Vec<String> = Vec::new();
        if let Some(held) = held {
            parts.push(match held {
                HoldWord::Person => "you have it".to_string(),
                HoldWord::Remote => {
                    let host = self
                        .pool
                        .get(session)
                        .and_then(|s| s.ctx.fabric.holds().remote().map(|r| r.host.clone()));
                    format!(
                        "held by {}",
                        crate::typed_by::fleet_credit(host.as_deref().unwrap_or("another Mac"))
                    )
                }
                HoldWord::Local => "held (aterm ctl hold)".to_string(),
                HoldWord::Bridge => "held over the message bus".to_string(),
            });
        }
        if let Some(team) = self.teams.get(&session) {
            if let Some(manager) = &team.manager {
                parts.push(format!("worker of {}", manager.label));
            }
            if team.workers > 0 {
                parts.push(format!("manages {}", team.workers));
            }
        }
        if let Some(words) = self.driven_by_aterm(session) {
            parts.push(format!("driven by aterm ({words})"));
        }
        (!parts.is_empty()).then(|| parts.join(crate::native_ui::FACT_SEPARATOR))
    }

    /// The native pill's click as its wake delivers it (`Wake::PillAct`).
    pub(crate) fn on_pill_wake(&mut self, window: WindowId, session: u64, hit: presence::PillHit) {
        self.clear_tab_surface_move_license(Some(window));
        self.settle_rename_edit(window);
        self.on_pill_act(window, session, hit);
    }

    /// A click on window `window`'s pill landed on `hit`, the pill naming `session` when it was drawn.
    pub(crate) fn on_pill_act(&mut self, window: WindowId, session: u64, hit: presence::PillHit) {
        let Some(pill) = self
            .windows
            .get(&window)
            .and_then(|ws| ws.presence.pill.as_ref())
            .filter(|pill| pill.session == session)
        else {
            return;
        };
        match hit {
            presence::PillHit::Go => {
                let _ = self.focus_session_preferring(Some(window), session);
            }
            presence::PillHit::More if pill.more > 0 => {
                let at = Desk::next_need_after(self, window, session).map_or(
                    crate::app_overview::OverviewAt::Top,
                    crate::app_overview::OverviewAt::Session,
                );
                if let Err(e) = self.open_overview_raised(window, at) {
                    aterm_log::info!("pill +N: {e}");
                }
            }
            presence::PillHit::Act(act) if pill.act == Some(act) => match act {
                presence::PillAct::Restart => self.pill_restart(session),
                presence::PillAct::Resume => self.pill_resume(session),
                presence::PillAct::HandBack => self.hand_back_person(session),
            },
            presence::PillHit::More | presence::PillHit::Act(_) => {}
        }
    }

    fn pill_restart(&mut self, session: u64) {
        #[cfg(unix)]
        {
            let handle = self
                .store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .by_local(session)
                .map(|h| (h.master, std::sync::Arc::clone(&h.ctx)));
            if let Some((master, ctx)) = handle {
                let spawned = std::thread::Builder::new()
                    .name("aterm-pill-restart".into())
                    .spawn(move || {
                        let reply = crate::restart_act::restart(master, "if-stalled", &ctx);
                        aterm_log::info!("pill restart {session}: {}", reply.trim_end());
                    });
                if let Err(e) = spawned {
                    aterm_log::warn!("pill restart {session}: no thread: {e}");
                }
            }
        }
        #[cfg(not(unix))]
        let _ = session;
    }

    fn pill_resume(&mut self, session: u64) {
        #[cfg(unix)]
        {
            let handle = self
                .store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .by_local(session)
                .map(|h| (h.master, std::sync::Arc::clone(&h.ctx)));
            if let Some((master, ctx)) = handle {
                let spawned = std::thread::Builder::new()
                    .name("aterm-pill-resume".into())
                    .spawn(move || {
                        let reply = resume_if_stopped(master, &ctx);
                        aterm_log::info!("pill resume {session}: {}", reply.trim_end());
                    });
                if let Err(e) = spawned {
                    aterm_log::warn!("pill resume {session}: no thread: {e}");
                }
            }
        }
        #[cfg(not(unix))]
        let _ = session;
    }

    /// Re-project every window whose pill names `session`.
    pub(crate) fn reproject_pill_of(&mut self, session: u64) {
        let windows: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| {
                ws.presence
                    .pill
                    .as_ref()
                    .is_some_and(|pill| pill.session == session)
            })
            .map(|(wid, _)| *wid)
            .collect();
        for wid in windows {
            drive::refresh_window(self, wid);
        }
    }

    /// Hand Back (the pill's, the Team chip's `✋`).
    pub(crate) fn hand_back_person(&mut self, session: u64) {
        if let Err(why) = self.hand_back_session(session) {
            aterm_log::info!("hand back {session}: {why}");
        }
    }

    /// The window's view, for the tests.
    #[cfg(test)]
    pub(crate) fn presence_view(&self, wid: WindowId) -> Option<&WindowView> {
        self.windows.get(&wid).map(|ws| &ws.presence)
    }

    /// `chrome`'s presence line for the front window (the frontmost, else the first).
    pub(crate) fn presence_chrome_line(&self) -> String {
        let front = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied());
        drive::chrome_line(self, front)
    }

    /// A session's level as `status level=` reports it, for the tests ([`drive::session_level`]).
    #[cfg(test)]
    pub(crate) fn presence_session_level(&self, session: u64) -> presence::Level {
        drive::session_level(self, session)
    }

    /// The additive presence `status` fields (`hand= hand_pid= level= story= why=`).
    pub(crate) fn presence_status_tail(&self, session: u64) -> String {
        drive::status_tail(self, session)
    }

    /// `aterm ctl story <verb> [<text>]` for `session` (`drive::tell`).
    #[cfg(test)]
    pub(crate) fn tell_story(
        &mut self,
        session: u64,
        verb: StoryVerb,
    ) -> Result<u64, &'static str> {
        self.tell_story_by(session, verb, aterm_slog::Actor::Ctl)
    }

    /// `Self::tell_story` told by `by` (the wire's `log_actor`, carried in `Wake::Story`).
    pub(crate) fn tell_story_by(
        &mut self,
        session: u64,
        verb: StoryVerb,
        by: aterm_slog::Actor,
    ) -> Result<u64, &'static str> {
        let seq = drive::tell(self, session).ok_or("no such session")?;
        let word = StoryVerb::TOLD_WORDS
            .into_iter()
            .find(|w| StoryVerb::parse_told(w) == Some(verb))
            .unwrap_or("unknown");
        if let Some(s) = self.pool.get(session) {
            let mut timeline = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let at = timeline.slog().now();
            timeline.slog_mut().observe_story(word, by, at);
            let _ = timeline.slog_take_wake();
        }
        Ok(seq)
    }

    /// Run `f` with every chrome-row re-grid it causes (`regrid_for_chrome_rows`) recorded, then apply one re-grid from the rows the commits left.
    #[cfg(any(unix, test))]
    pub(crate) fn with_chrome_regrid_batch<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        if self.chrome_regrid_batch.is_some() {
            return f(self);
        }
        self.chrome_regrid_batch = Some(ChromeRegridBatch::default());
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match self.chrome_regrid_batch.take().unwrap_or_default() {
            ChromeRegridBatch::Idle => {}
            ChromeRegridBatch::All => self.regrid_for_chrome_rows(),
        }
        out.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }

    /// The terminal rows window `wid` measures a chrome commit against, and whether it has glass.
    pub(crate) fn chrome_commit_grid_rows(&self, wid: WindowId) -> Option<(u16, bool)> {
        let ws = self.windows.get(&wid)?;
        let size = self
            .chrome_regrid_batch
            .as_ref()
            .is_some_and(|batch| batch.covers(wid))
            .then(|| ws.os_window.as_ref().map(|w| w.inner_size()))
            .flatten();
        Some((
            self.chrome_commit_grid_rows_at(wid, ws.rows, size),
            chrome_regrids_grid(ws),
        ))
    }

    fn chrome_commit_grid_rows_at(
        &self,
        wid: WindowId,
        rows: u16,
        size: Option<aterm_window::dpi::PhysicalSize<u32>>,
    ) -> u16 {
        match (&self.chrome_regrid_batch, size) {
            (Some(batch), Some(size)) if batch.covers(wid) => self.grid_dims_for(wid, size).0,
            _ => rows,
        }
    }

    /// Lift an update handoff's park (`Wake::UpdateHandoffFinished`).
    #[cfg(unix)]
    pub(crate) fn release_update_handoff_park(&mut self, finish: impl FnOnce(&mut Self)) {
        self.with_chrome_regrid_batch(|app| {
            finish(app);
            app.refresh_presence_all_windows();
            app.sync_messages();
        });
    }

    /// Lift a successor's freeze (the Commit wake, once `incoming_handoff_pending` is false).
    #[cfg(unix)]
    pub(crate) fn release_successor_commit_freeze(&mut self) {
        self.with_chrome_regrid_batch(|app| {
            app.refresh_presence_all_windows();
            app.arm_supervisor_expiries();
            if let Some(build) = app.landing_at_commit.take() {
                app.retire_charging_surge();
                app.post_update_landed(
                    crate::build_info::version_display(),
                    build,
                    app.seamless_repainted_tabs(),
                );
            }
            app.say_carried_history_loss();
            app.say_carried_settings_drafts_loss();
            app.sync_messages();
        });
    }

    /// Note a rise of the chrome rows every window shares (a band row reached the glass, the tab strip grew) on every window's regrow floor.
    pub(crate) fn note_chrome_rise_everywhere(&mut self, now: Instant) {
        for ws in self.windows.values_mut() {
            ws.regrow_floor.rose(now);
        }
    }

    /// Window `wid`'s chrome regrow floor (`RegrowFloor::floor`).
    #[cfg(test)]
    pub(crate) fn chrome_regrow_floor(&self, wid: WindowId) -> Option<Instant> {
        self.windows
            .get(&wid)
            .filter(|ws| chrome_regrids_grid(ws))
            .and_then(|ws| ws.regrow_floor.floor())
    }

    /// The floor the band's quiet shrink and the tab strip's fall wait.
    pub(crate) fn band_regrow_floor(&self) -> Option<Instant> {
        aterm_messages::chrome::latest(
            self.windows
                .values()
                .filter(|ws| chrome_regrids_grid(ws))
                .map(|ws| ws.regrow_floor.floor()),
        )
    }

    /// Forget a retired session's slot, every window's watermark for it, its done-seen rev, and its need's recheck and part in the API outage.
    pub(crate) fn retire_presence(&mut self, session: u64) {
        self.presence.retire(session);
        self.consent_observer.retire_session(session);
        self.consent_attention.retire_session(session);
        for ws in self.windows.values_mut() {
            ws.presence.watermarks.remove(&session);
        }
        self.done_seen.remove(&session);
        self.turn_end_seen.remove(&session);
        self.forget_need(session, need_unix_now());
        self.rebuild_teams();
    }
}

impl Desk for App {
    type V = presence::Native;
    type Win = WindowId;

    fn clock(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = PRESENCE_CLOCK_FOR_TEST.with(std::cell::Cell::get) {
            return now;
        }
        Instant::now()
    }

    fn table(&self) -> &drive::Table<presence::Native> {
        &self.presence.table
    }

    fn table_mut(&mut self) -> &mut drive::Table<presence::Native> {
        &mut self.presence.table
    }

    fn each_view(&self, f: &mut dyn FnMut(WindowId, &WindowView)) {
        for (wid, ws) in &self.windows {
            f(*wid, &ws.presence);
        }
    }

    fn view_of(&self, w: WindowId) -> Option<&WindowView> {
        self.windows.get(&w).map(|ws| &ws.presence)
    }

    fn view_of_mut(&mut self, w: WindowId) -> Option<&mut WindowView> {
        self.windows.get_mut(&w).map(|ws| &mut ws.presence)
    }

    fn front_session(&self, w: WindowId) -> Option<u64> {
        self.focused_session_id(w)
    }

    fn shown(&self, w: WindowId) -> Vec<Shown> {
        let Some(ws) = self.windows.get(&w) else {
            return Vec::new();
        };
        let front = ws.tab_set.active_index();
        let mut out = Vec::new();
        for (tab, t) in ws.tab_set.tabs().iter().enumerate() {
            t.root.any_leaf(&mut |view| {
                if let Some(session) = self
                    .view_store
                    .get(*view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                {
                    out.push(Shown {
                        session,
                        tab,
                        front_tab: Some(tab) == front,
                    });
                }
                false
            });
        }
        out
    }

    fn windows_showing(&self, session: u64) -> Vec<WindowId> {
        let mut windows = self.windows_with_focused_session(session);
        for (wid, _) in self.tabs_viewing_session(session) {
            if !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        windows
    }

    fn holds_session(&self, session: u64) -> bool {
        self.pool.get(session).is_some()
    }

    fn sense(&self, session: u64) -> Option<Facts> {
        self.presence_facts(session)
    }

    fn supervisor_lapse(&self, session: u64, now: Instant) -> Option<Instant> {
        let at_us = self.pool.get(session).and_then(|s| {
            s.ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .supervisor_expiry()
        })?;
        let now_us = crate::metrics::us_at(now);
        Some(now + Duration::from_micros(at_us.saturating_sub(now_us)) + Duration::from_millis(1))
    }

    fn pill_visible(&self, w: WindowId) -> bool {
        self.windows
            .get(&w)
            .map(Box::as_ref)
            .is_some_and(crate::messages_host::band_on_screen)
    }

    fn needs_shown(&self) -> bool {
        self.presence_rim_on()
    }

    fn label_of(&self, session: u64) -> String {
        self.tab_label_of_session(session)
    }

    fn restart_offered(&self, session: u64) -> bool {
        let sid = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .map(|h| h.sid.clone());
        sid.is_some_and(|sid| crate::harness_host::offered_now(&self.store, sid.as_str()).is_some())
    }

    fn sid_of(&self, session: u64) -> Option<String> {
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .map(|h| h.sid.as_str().to_string())
    }

    fn herald_deadline(&self, now: Instant) -> Option<Instant> {
        let herald = match self.presence.herald.due(now) {
            (ready, _) if !ready.is_empty() => Some(now),
            (_, next) => next,
        };
        match (herald, self.need_recheck_deadline(now)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn projecting(&mut self, w: WindowId) {
        if self.frontmost_window == Some(w) {
            self.publish_front_hold(w);
        }
        self.note_done_seen(w);
    }

    fn request_redraw(&self, w: WindowId) {
        if let Some(win) = self.windows.get(&w).and_then(|ws| ws.os_window.as_ref()) {
            win.request_redraw();
        }
    }

    fn restamp_chips(&mut self, windows: Vec<WindowId>) {
        self.refresh_tab_chrome_windows(windows);
    }

    fn push_pill(&mut self, windows: Vec<WindowId>) -> Vec<WindowId> {
        use crate::platform::AppRt as _;
        let mut framed = Vec::new();
        for wid in windows {
            if self.strip_carries_pill(wid) {
                framed.push(wid);
                continue;
            }
            let held = self._toolbars.get(&wid).is_none_or(|handle| {
                let pill = self.presence_pill(wid);
                self.apprt.set_toolbar_pill(handle, pill.as_ref())
            });
            if !held {
                self.refresh_window_tabs(wid);
            }
        }
        framed
    }

    fn herald(&mut self, session: u64) {
        if self.publish_need(session) {
            self.harness_screen_moved();
        }
        self.herald_session(session);
    }

    fn herald_owed(&mut self, now: Instant) {
        self.need_rechecks_due();
        let (ready, _) = self.presence.herald.due(now);
        for session in ready {
            self.herald_session(session);
        }
    }

    fn lapse_supervisor(&mut self, session: u64) {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return;
        };
        let now_us = crate::metrics::us_at(Desk::clock(self));
        if crate::session_timeline::lapse_supervisor(&ctx, now_us) {
            self.redraw_claude_footer(session);
            self.note_harness_arm(session);
            self.harness_screen_moved();
        }
    }

    fn forget(&mut self, session: u64) {
        self.presence.retire(session);
        self.turn_end_seen.remove(&session);
    }
}

/// A HARNESS'S NEED ON THE TAB'S HOVER (§12.2.1).
pub(crate) fn harness_need_hover(ask: &crate::needs::HarnessAsk) -> Vec<String> {
    let prefix = crate::session_chrome::NEEDS_YOU_PREFIX;
    let mut lines: Vec<String> = std::iter::once(&ask.first)
        .chain(&ask.others)
        .map(|need| format!("{prefix}{need}"))
        .collect();
    if let Some(last) = lines.last_mut() {
        last.push_str(&ask.more_past(ask.others.len()));
    }
    lines.push(aterm_agent::supervise::mode_words::report_words(
        ask.program.product(),
        false,
    ));
    lines
}

/// WHO DRIVES, IN THE CHIP'S OWN WORDS (§12.4.1; §12.4.3, item 1). `landed_said`: the row these
/// words end already says `landed` (the Overview's `task landed 17m`), so a landed chip says only
/// `land` there; the tab's hover, which has no task words, passes `false` and keeps `land, landed`
/// (§14.6). `stopped`: aterm stopped typing into it, said first under a face that carries a mode
/// (§14.3: `land: stopped typing; not landed yet, 1 untracked file`, `cruise: stopped typing`).
#[must_use]
pub(crate) fn driven_words(
    face: crate::claude_lights::ChipFace,
    glass: &crate::claude_lights::ChipGlass,
    hold: Option<&crate::harness_glass::HeldGlass>,
    landed_said: bool,
    stopped: Option<aterm_agent::harness::acts::HeldBy>,
) -> Option<String> {
    use crate::claude_lights::ChipFace;
    let stop = stopped
        .filter(|_| face.carries())
        .map(|arm| crate::harness_glass::stop_words(arm, crate::harness_glass::HoldForm::Hover));
    let mode = |mode: &str, why: Option<String>| match (&stop, why) {
        (Some(stop), Some(why)) => format!("{mode}: {stop}; {why}"),
        (Some(stop), None) => format!("{mode}: {stop}"),
        (None, Some(why)) => format!("{mode}: {why}"),
        (None, None) => mode.to_owned(),
    };
    let words = match face {
        ChipFace::Cruise => mode("cruise", None),
        ChipFace::CruiseUntil => match (glass.land_at, &stop) {
            (Some(at), Some(_)) => mode(
                "cruise",
                Some(match at.other_day {
                    Some(day) => format!("lands {} {}", day.short(), at.clock.as_str()),
                    None => format!("lands at {}", at.clock.as_str()),
                }),
            ),
            (Some(at), None) => match at.other_day {
                Some(day) => format!("cruise, lands {} {}", day.short(), at.clock.as_str()),
                None => format!("cruise, lands at {}", at.clock.as_str()),
            },
            (None, _) => mode("cruise", None),
        },
        ChipFace::Land => mode(
            "land",
            hold.and_then(|hold| {
                crate::harness_glass::hold_words(
                    hold,
                    None,
                    crate::harness_glass::HoldForm::Hover,
                    None,
                )
            }),
        ),
        ChipFace::Landed if landed_said => "land".to_owned(),
        ChipFace::Landed | ChipFace::Theirs => format!("land, {}", face.word()?),
        ChipFace::Off | ChipFace::MasterOff | ChipFace::AgentOff(_) | ChipFace::Unhosted => {
            return None;
        }
    };
    Some(words)
}

fn pack(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

/// THE PILL'S RESUME, fenced ([`App::pill_resume`]).
#[cfg(unix)]
pub(crate) fn resume_if_stopped(master: i32, ctx: &crate::SessionCtx) -> String {
    if !crate::input_stall::fg_stopped(master) {
        return "ERR not-stopped\n".to_string();
    }
    crate::control::signal_verb(master, "cont", ctx)
}

fn opened_at(started_ms: u64) -> Option<std::time::Instant> {
    let now = std::time::Instant::now();
    let ago = crate::turn_ledger::now_ms()
        .saturating_sub(started_ms)
        .saturating_add(1);
    now.checked_sub(std::time::Duration::from_millis(ago))
}

/// Where this machine's one Grok Build sign-in stands for a Grok session on its sign-in wall
/// whose program group leader is `pgid` ([`crate::needs::GrokSignin`]): a sign-in runs (its
/// leader's record under the harness state), else a login the walled grok may not have read
/// (written after it started) says restart it, else signed out. A login written before that grok
/// started was read and refused by it: it is marked refused
/// ([`aterm_agent::harness::grok_signin::mark_refused`]), so the next launch is signed in through
/// the one sandboxed sign-in instead of a grok that opens its own page.
fn grok_signin_now(pgid: i32) -> crate::needs::GrokSignin {
    use aterm_agent::harness::grok_signin::{self, WallLogin};
    if crate::harness_host::harness_state().is_some_and(|state| grok_signin::running(&state)) {
        return crate::needs::GrokSignin::Running;
    }
    let home = aterm_types::dirs::home_dir();
    let started = (pgid > 0)
        .then(|| aterm_uds::process::read_process_birth(pgid))
        .flatten()
        .map(|b| {
            std::time::UNIX_EPOCH
                + std::time::Duration::from_secs(b.seconds)
                + std::time::Duration::from_micros(b.microseconds)
        });
    match grok_signin::wall_login(&|name| std::env::var_os(name), home.as_deref(), started) {
        WallLogin::SignedIn(_) => crate::needs::GrokSignin::SignedIn,
        WallLogin::Refused(file) => {
            let _ = grok_signin::mark_refused(&file);
            crate::needs::GrokSignin::Idle
        }
        WallLogin::SignedOut => crate::needs::GrokSignin::Idle,
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::presence::{ChipLevel, PillHit, PillKind, Rim};
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    use crate::{App, WindowId};
    use std::sync::Arc;

    fn app_with_stub() -> (App, WindowId, u64, Arc<crate::SessionCtx>) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sid = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid));
        let ctx = app.pool.get(sid).expect("stub pooled").ctx.clone();
        assert_eq!(
            app.focused_session_id(wid),
            Some(sid),
            "the stub is the front session"
        );
        (app, wid, sid, ctx)
    }

    fn second_tab(app: &mut App, wid: WindowId) -> (u64, Arc<crate::SessionCtx>) {
        let sid = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid));
        let ctx = app.pool.get(sid).expect("stub pooled").ctx.clone();
        (sid, ctx)
    }

    fn ask(ctx: &crate::SessionCtx, text: Option<&str>) {
        ctx.meta.lock().unwrap().attention = text.map(str::to_string);
    }

    fn task(ctx: &crate::SessionCtx, local: u64, state: TaskState) {
        let mark = TaskMark {
            why: (state == TaskState::Stalled).then_some("startup"),
            ..TaskMark::new(state)
        };
        mark_session_task(ctx, local, &mark).expect("unfenced");
    }

    fn view(app: &App, wid: WindowId) -> &WindowView {
        app.presence_view(wid).expect("a window")
    }

    #[test]
    fn the_published_need_lights_the_border_and_its_pill() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        app.refresh_presence_session(sid);
        assert_eq!(view(&app, wid).rim, Rim::None);
        ask(&ctx, Some("deploy to prod?"));
        app.refresh_presence_session(sid);
        assert_eq!(view(&app, wid).rim, Rim::Decide);
        let pill = app.presence_pill(wid).expect("lit: the pill");
        assert_eq!(
            pill.kind,
            PillKind::Need(crate::presence::NeedSeverity::Decide)
        );
        assert_eq!(pill.session, sid);
        assert!(
            pill.words.label.is_empty(),
            "the front tab is named already"
        );
        let line = app.presence_chrome_line();
        assert!(line.contains("rim=decide"), "{line}");
        assert!(
            line.contains(&format!("pressing=@{}", ctx.self_id.as_str())),
            "{line}"
        );
        task(&ctx, sid, TaskState::Stalled);
        app.refresh_presence_session(sid);
        assert_eq!(view(&app, wid).rim, Rim::Stuck, "stuck outranks decide");
        ask(&ctx, None);
        task(&ctx, sid, TaskState::Working);
        app.refresh_presence_session(sid);
        assert_eq!(view(&app, wid).rim, Rim::None);
        assert!(app.presence_pill(wid).is_none());
        assert!(
            app.presence_chrome_line()
                .contains("rim=none pressing=- pill=\"\"")
        );
    }

    #[test]
    fn a_kind_wait_entry_lights_no_border_and_a_kind_ask_does() {
        use aterm_types::needs::AttentionKind;
        let (mut app, wid, sid, ctx) = app_with_stub();
        let entry = |app: &mut App, kind: AttentionKind| {
            ctx.meta
                .lock()
                .unwrap()
                .set_attention_kind(
                    aterm_agent::supervise::ATTENTION_OWNER,
                    Some("limited: resets 7pm".to_string()),
                    Some(kind),
                )
                .expect("room for the entry");
            app.on_meta_changed(sid);
            (
                view(app, wid).rim,
                app.presence_pill(wid).is_some(),
                app.presence.slot(sid).map(|s| s.chip()),
                app.presence_chrome_line(),
            )
        };
        let (rim, pill, chip, line) = entry(&mut app, AttentionKind::Wait);
        assert!(ctx.meta.lock().unwrap().attention.is_some(), "it stands");
        assert!(ctx.timeline.lock().unwrap().need().need.is_none());
        assert_eq!((rim, pill, chip), (Rim::None, false, Some(ChipLevel::Off)));
        assert!(line.contains("rim=none"), "{line}");
        let (rim, pill, chip, line) = entry(&mut app, AttentionKind::Ask);
        assert_eq!(
            (rim, pill, chip),
            (Rim::Decide, true, Some(ChipLevel::Wait))
        );
        assert!(line.contains("rim=decide"), "{line}");
    }

    #[test]
    fn a_background_tabs_need_lights_its_window_and_names_the_tab() {
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let (second, second_ctx) = second_tab(&mut app, wid);
        assert_eq!(app.focused_session_id(wid), Some(second), "tab 2 in front");
        first_ctx.meta.lock().unwrap().role = Some("fmt".into());
        ask(&first_ctx, Some("which branch?"));
        app.refresh_presence_session(first);
        assert_eq!(view(&app, wid).rim, Rim::Decide);
        assert_eq!(view(&app, wid).pressing, Some(first));
        let pill = app.presence_pill(wid).expect("lit");
        assert!(pill.words.label.contains("fmt"), "{:?}", pill.words);
        let tab = app.windows[&wid].tab_set.tabs().len() - 1;
        assert_eq!(view(&app, wid).pill.as_ref().and_then(|p| p.tab), Some(tab));
        ask(&second_ctx, Some("run the tests?"));
        app.refresh_presence_session(second);
        assert_eq!(view(&app, wid).pressing, Some(second));
        assert_eq!(view(&app, wid).pill.as_ref().map(|p| p.more), Some(1));
        let _ = first;
    }

    #[test]
    fn a_hundred_verdict_flips_regrid_no_pane() {
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let _ = second_tab(&mut app, wid);
        let rows = app.chrome_rows(wid);
        let _ = take_chrome_regrids_applied();
        let mut lit = 0;
        for i in 0..100 {
            ask(&first_ctx, (i % 2 == 0).then_some("approve?"));
            app.refresh_presence_session(first);
            lit += usize::from(view(&app, wid).rim == Rim::Decide);
        }
        assert_eq!(lit, 50, "every flip reached the border");
        assert!(
            take_chrome_regrids_applied().is_empty(),
            "no flip re-gridded a pane"
        );
        assert_eq!(app.chrome_rows(wid), rows);
        app.set_tab_strip_rows(app.tab_strip_rows + 1, Instant::now());
        assert!(
            take_chrome_regrids_applied()
                .iter()
                .any(|(w, r)| *w == wid && *r == rows + 1),
            "the strip's new row is a re-grid the probe records"
        );
    }

    #[test]
    fn a_headless_pill_arms_no_timer() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        ask(&ctx, Some("approve?"));
        app.refresh_presence_session(sid);
        assert!(app.presence_pill(wid).is_some());
        let now = Instant::now();
        assert_eq!(app.presence_deadline(now), None);
    }

    #[test]
    fn presence_fp_stays_zero_while_status_revisions_replay() {
        use crate::session_status::{ActivitySample, Evidence};
        crate::fabric::with_link_reset(|| {
            let (mut app, wid, sid, _ctx) = app_with_stub();
            app.refresh_presence_session(sid);
            app.refresh_presence_all_windows();
            let t0 = Instant::now();
            let mut ev = Evidence {
                shell: None,
                completed_block: None,
                lifecycle: None,
                foreground_job: Some(true),
                activity: ActivitySample {
                    alt_screen: false,
                    content_seq: 1,
                    last_output: Some(t0),
                },
            };
            let quiet = |app: &mut App, t: Instant, frame: u64| {
                assert_eq!(
                    app.presence_fp(wid),
                    0,
                    "frame {frame}: {}",
                    app.presence_chrome_line()
                );
                assert!(app.presence_overlay(wid).is_none(), "frame {frame}");
                assert!(app.presence_tick(t).is_empty(), "frame {frame}");
                assert!(
                    app.presence_deadline(t).is_none(),
                    "frame {frame}: no timer"
                );
            };
            let mut revisions = 0u64;
            let mut last = app.session_status.revision(sid);
            for i in 0..1000u64 {
                let t = t0 + Duration::from_millis(i * 700);
                ev.activity.content_seq += 1;
                ev.activity.last_output = if i % 6 < 3 { Some(t) } else { None };
                ev.foreground_job = Some(i % 6 < 3);
                app.session_status.observe(sid, &ev, t);
                let r = app.session_status.revision(sid);
                if r != last {
                    revisions += 1;
                    last = r;
                    app.refresh_presence_session(sid);
                }
                quiet(&mut app, t, i);
            }
            assert!(revisions > 0, "the replay moved no revision ({revisions})");
            let other = app
                .open_active_session_in_new_window_logical()
                .expect("a second window");
            let (needy, needy_ctx) = second_tab(&mut app, other);
            ask(&needy_ctx, Some("deploy to prod?"));
            app.refresh_presence_session(needy);
            assert_ne!(app.presence_fp(other), 0, "the needy window is lit");
            assert!(app.presence_overlay(other).is_some());
            quiet(&mut app, Instant::now(), 1000);
        });
    }

    #[cfg(a11y_tree)]
    #[test]
    fn the_spoken_pill_never_ticks_with_its_age() {
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let _ = second_tab(&mut app, wid);
        let t0 = Instant::now();
        set_presence_clock_for_test(Some(t0));
        ask(&first_ctx, Some("which branch?"));
        app.refresh_presence_session(first);
        let at = |app: &App, t: Instant| {
            set_presence_clock_for_test(Some(t));
            app.presence_a11y_message(wid).expect("lit")
        };
        let early = at(&app, t0 + Duration::from_secs(5));
        let later = at(&app, t0 + Duration::from_secs(65));
        set_presence_clock_for_test(None);
        assert_eq!(early.text, later.text, "the spoken sentence held still");
        assert!(
            early.detail.is_some(),
            "a background pill's age is described"
        );
        assert_ne!(early.detail, later.detail, "CONTROL: the age moved");
        ask(&first_ctx, Some("run the tests?"));
        app.refresh_presence_session(first);
        let moved = app.presence_a11y_message(wid).expect("lit");
        assert_ne!(moved.text, early.text, "CONTROL: a new need is spoken");
    }

    #[test]
    fn a_presence_refresh_never_classifies() {
        let (mut app, _wid, sid, ctx) = app_with_stub();
        let calls = presence::classifier_calls();
        for i in 0..1000 {
            ask(&ctx, (i % 3 == 0).then_some("approve?"));
            app.refresh_presence_session(sid);
        }
        assert_eq!(
            presence::classifier_calls() - calls,
            0,
            "presence folds the published verdict; it never reads the screen"
        );
    }

    #[test]
    fn each_chip_carries_its_tabs_load_until_the_bars_are_off() {
        let (mut app, wid, _first, _ctx) = app_with_stub();
        let (second, _second_ctx) = second_tab(&mut app, wid);
        let mut snap = crate::stats_deck::tests::snapshot();
        snap.tabs[0].key = second;
        snap.tabs[0].scope.cores.level = 5.0;
        app.vitals.snapshot = Some(std::sync::Arc::new(snap));
        let loads = |app: &App| {
            let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = app.windows[&wid]
                .tab_set
                .tabs()
                .iter()
                .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
                .collect();
            app.stamp_presence_chips(wid, &mut metadata);
            metadata.iter().map(|m| m.load).collect::<Vec<_>>()
        };
        let lit = loads(&app);
        assert_eq!(lit.last(), Some(&128), "five of ten cores: half the meter");
        assert!(
            lit[..lit.len() - 1].iter().all(|&l| l == 0),
            "an unmeasured tab draws no meter: {lit:?}"
        );
        assert_eq!(
            app.tab_strip_metadata(wid).last().map(|m| m.load),
            Some(128)
        );
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("off".into());
        assert!(
            loads(&app).iter().all(|&l| l == 0),
            "the bars off, no meters"
        );
    }

    #[test]
    fn the_tab_marks_read_the_verdict_and_the_done_dot_clears_once_seen() {
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let (second, second_ctx) = second_tab(&mut app, wid);
        let marks = |app: &App| {
            let ws = &app.windows[&wid];
            let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
                .tab_set
                .tabs()
                .iter()
                .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
                .collect();
            app.stamp_presence_chips(wid, &mut metadata);
            metadata[metadata.len() - 2..]
                .iter()
                .map(|m| m.attention)
                .collect::<Vec<_>>()
        };
        app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
        assert_eq!(app.frontmost_window, Some(wid));
        ask(&first_ctx, Some("approve?"));
        app.refresh_presence_session(first);
        assert_eq!(marks(&app), [ChipLevel::Wait, ChipLevel::Off]);
        ask(&first_ctx, None);
        task(&first_ctx, first, TaskState::Done);
        app.on_task_changed(first);
        assert_eq!(
            marks(&app),
            [ChipLevel::Done, ChipLevel::Off],
            "a finished task behind is the dot"
        );
        task(&second_ctx, second, TaskState::Done);
        app.on_task_changed(second);
        assert_eq!(marks(&app)[1], ChipLevel::Off);
        let tabs = app.windows[&wid].tab_set.tabs().len();
        app.switch_tab_in(wid, tabs - 2);
        assert_eq!(marks(&app), [ChipLevel::Off, ChipLevel::Off]);
        app.switch_tab_in(wid, tabs - 1);
        assert_eq!(marks(&app), [ChipLevel::Off, ChipLevel::Off]);
        task(&first_ctx, first, TaskState::Stalled);
        app.on_task_changed(first);
        assert!(matches!(marks(&app)[0], ChipLevel::Stop(_)));
    }

    #[cfg(unix)]
    #[test]
    fn a_done_the_owner_saw_stays_seen_across_a_seamless_update() {
        let behind = |app: &App, wid: WindowId| {
            let ws = &app.windows[&wid];
            let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
                .tab_set
                .tabs()
                .iter()
                .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
                .collect();
            app.stamp_presence_chips(wid, &mut metadata);
            [metadata[1].attention, metadata[2].attention]
        };
        let (mut parent, wid, seen, seen_ctx) = app_with_stub();
        let (unseen, unseen_ctx) = second_tab(&mut parent, wid);
        let _ = second_tab(&mut parent, wid);
        parent
            .windows
            .get_mut(&wid)
            .unwrap()
            .band_on_screen_for_test = true;
        parent.frontmost_window = Some(wid);
        for (ctx, local) in [(&seen_ctx, seen), (&unseen_ctx, unseen)] {
            task(ctx, local, TaskState::Done);
            parent.on_task_changed(local);
        }
        parent.switch_tab_in(wid, 1);
        parent.switch_tab_in(wid, 3);
        assert_eq!(
            behind(&parent, wid),
            [ChipLevel::Off, ChipLevel::Done],
            "PRECONDITION: one seen, one not"
        );
        let rows = parent.status_carry_rows();
        let carried = |local: u64, ctx: &crate::SessionCtx| {
            let task = ctx
                .timeline
                .lock()
                .unwrap()
                .task()
                .expect("a task")
                .carry_row();
            let status = rows
                .iter()
                .find_map(|(id, row)| (*id == local).then(|| row.clone()));
            (task, status)
        };
        let (seen_task, seen_row) = carried(seen, &seen_ctx);
        let (unseen_task, unseen_row) = carried(unseen, &unseen_ctx);
        assert!(
            unseen_row.is_none(),
            "nothing the status item knows of the unseen one"
        );
        let (mut successor, wid, first, first_ctx) = app_with_stub();
        let (second, second_ctx) = second_tab(&mut successor, wid);
        let _ = second_tab(&mut successor, wid);
        for (local, ctx, task_row, status_row) in [
            (first, &first_ctx, seen_task, seen_row),
            (second, &second_ctx, unseen_task, unseen_row),
        ] {
            crate::task_record::seed_carried(ctx, Some(&task_row));
            App::register_session(
                &successor.store,
                successor.pool.get(local).expect("pooled"),
                None,
            );
            if let Some(row) = status_row {
                successor.status_carry.insert(
                    ctx.self_id.as_str().to_string(),
                    crate::status_item::StatusCarry::parse(&row),
                );
            }
        }
        successor.refresh_presence_all_windows();
        successor.herald_session(first);
        successor.herald_session(second);
        assert_eq!(
            behind(&successor, wid),
            [ChipLevel::Off, ChipLevel::Done],
            "the look crossed; the unlooked dot stands"
        );
    }

    #[test]
    fn a_task_done_where_nobody_looks_keeps_its_dot() {
        let (mut app, wid, _first, _) = app_with_stub();
        let (second, second_ctx) = second_tab(&mut app, wid);
        let tabs = app.windows[&wid].tab_set.tabs().len();
        let dot = |app: &App| {
            let ws = &app.windows[&wid];
            let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
                .tab_set
                .tabs()
                .iter()
                .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
                .collect();
            app.stamp_presence_chips(wid, &mut metadata);
            metadata[tabs - 1].attention
        };
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.band_on_screen_for_test = true;
            ws.occluded = true;
        }
        task(&second_ctx, second, TaskState::Done);
        app.on_task_changed(second);
        app.refresh_presence_all_windows();
        app.switch_tab_in(wid, tabs - 2);
        assert_eq!(dot(&app), ChipLevel::Done, "nobody looked");
        app.switch_tab_in(wid, tabs - 1);
        app.on_occlusion_changed(wid, false);
        app.switch_tab_in(wid, tabs - 2);
        assert_eq!(dot(&app), ChipLevel::Off, "seen once revealed");
    }

    #[test]
    fn a_split_panes_task_done_in_front_is_seen() {
        let behind = |occluded: bool| {
            let (mut app, wid, worker, worker_ctx) = app_with_stub();
            let other = app.split_active_stub_tab(wid);
            assert_eq!(app.focused_session_id(wid), Some(other), "the new pane");
            {
                let ws = app.windows.get_mut(&wid).unwrap();
                ws.band_on_screen_for_test = true;
                ws.occluded = occluded;
            }
            app.frontmost_window = Some(wid);
            task(&worker_ctx, worker, TaskState::Done);
            app.on_task_changed(worker);
            let split = app.windows[&wid].tab_set.tabs().len() - 1;
            let _ = second_tab(&mut app, wid);
            let ws = &app.windows[&wid];
            let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
                .tab_set
                .tabs()
                .iter()
                .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
                .collect();
            app.stamp_presence_chips(wid, &mut metadata);
            metadata[split].attention
        };
        assert_eq!(behind(false), ChipLevel::Off, "looked at in front");
        assert_eq!(behind(true), ChipLevel::Done, "CONTROL: nobody looked");
    }

    #[test]
    fn an_in_window_role_renames_the_pill_and_the_team() {
        use crate::claude_lights::TeamFace;
        let (mut app, wid, manager, manager_ctx) = app_with_stub();
        let (worker, worker_ctx) = second_tab(&mut app, wid);
        let mark = TaskMark {
            from: Some(manager_ctx.self_id.as_str().to_string()),
            ..TaskMark::new(TaskState::Working)
        };
        mark_session_task(&worker_ctx, worker, &mark).expect("unfenced");
        app.on_task_changed(worker);
        ask(&manager_ctx, Some("approve?"));
        app.refresh_presence_session(manager);
        let named = |app: &App| {
            let pill = app.presence_pill(wid).expect("lit").words.label;
            let Some(TeamFace::Worker { label, .. }) = app.team_face(worker) else {
                panic!("a worker's face");
            };
            (pill.contains("lab-sweep"), label.contains("lab-sweep"))
        };
        assert_eq!(named(&app), (false, false), "CONTROL: the old name");
        app.write_session_meta_field(
            manager,
            crate::session_timeline::MetaField::Role,
            "lab-sweep",
        )
        .expect("stored");
        assert_eq!(named(&app), (true, true), "renamed on both");
    }

    #[test]
    fn a_team_chip_lands_in_the_window_clicked() {
        let shared = |via_chip: bool| {
            let (mut app, w0, manager, manager_ctx) = app_with_stub();
            app.frontmost_window = Some(w0);
            let w1 = app
                .open_active_session_in_new_window_logical()
                .expect("the share");
            let (worker, worker_ctx) = second_tab(&mut app, w1);
            assert!(w0 < w1, "PRECONDITION: the first window showing it is w0");
            let mark = TaskMark {
                from: Some(manager_ctx.self_id.as_str().to_string()),
                ..TaskMark::new(TaskState::Working)
            };
            mark_session_task(&worker_ctx, worker, &mark).expect("unfenced");
            app.on_task_changed(worker);
            assert_eq!(app.focused_session_id(w1), Some(worker));
            if via_chip {
                app.note_claude_team(w1, worker);
                app.press_claude_chip(
                    w1,
                    worker,
                    crate::claude_lights::Chip::Team,
                    std::time::Instant::now(),
                );
            } else {
                let _ = app.focus_session_window(manager);
            }
            (manager, app.focused_session_id(w1))
        };
        let (manager, focused) = shared(true);
        assert_eq!(
            focused,
            Some(manager),
            "the window clicked shows the manager"
        );
        let (manager, focused) = shared(false);
        assert_ne!(focused, Some(manager), "control: w1 stays where it was");
    }

    #[test]
    fn a_hold_is_said_on_the_hover_and_nowhere_else() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        let _ = second_tab(&mut app, wid);
        app.refresh_presence_session(sid);
        assert_eq!(app.presence_hover(sid, None), None, "nothing to say");
        let held = |app: &mut App, origin: &str| {
            assert!(crate::fabric::apply_hold_for_test(
                &ctx,
                Some(crate::fabric::Hold {
                    reason: "-".into(),
                    origin: origin.into(),
                })
            ));
            app.refresh_presence_session(sid);
            assert_eq!(view(app, wid).rim, Rim::None, "{origin}: no border");
            assert!(app.presence_pill(wid).is_none(), "{origin}: no pill");
            assert_eq!(
                app.presence.slot(sid).map(|s| s.chip()),
                Some(ChipLevel::Off),
                "{origin}: no mark"
            );
            let hover = app.presence_hover(sid, None).expect("the hold is said");
            assert_eq!(hover.lines().count(), 1, "{hover}");
            assert!(crate::fabric::apply_hold_for_test(&ctx, None));
            hover
        };
        let local = held(&mut app, "local");
        let bridge = held(&mut app, "fleet");
        assert_ne!(local, bridge, "each origin says its own");
    }

    #[test]
    fn a_harness_need_stays_in_the_hover_without_a_tab_bar_pill() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        ctx.meta
            .lock()
            .unwrap()
            .set_attention_kind(
                crate::harness_host::NEEDS_ATTENTION_OWNER,
                Some("claude needs you: sign 0.48.0 (+1 more) \u{2014} carrying on".into()),
                Some(aterm_types::needs::AttentionKind::Ask),
            )
            .expect("a slot");
        app.refresh_presence_session(sid);
        assert!(app.presence_pill(wid).is_none());
        assert!(app.presence_pill_key(wid).is_none());
        #[cfg(a11y_tree)]
        assert!(app.presence_a11y_message(wid).is_none());
        let hover = app.presence_hover(sid, None).expect("the need is said");
        let lines: Vec<&str> = hover.lines().collect();
        assert_eq!(
            lines[..2],
            [
                "Needs you: sign 0.48.0 (+1 more)",
                "Claude Code carries on meanwhile"
            ],
            "{hover}"
        );
        assert!(!hover.contains("Needs attention"), "{hover}");
        ctx.meta
            .lock()
            .unwrap()
            .set_attention_kind(crate::harness_host::NEEDS_ATTENTION_OWNER, None, None)
            .expect("cleared");
        ask(&ctx, Some("please look"));
        app.refresh_presence_session(sid);
        assert!(
            app.presence_pill(wid).is_some(),
            "an interactive ask keeps its pill"
        );
        assert!(app.presence_pill_key(wid).is_some());
        let hover = app.presence_hover(sid, None).expect("the ask is said");
        assert_eq!(
            hover.lines().next(),
            Some("Needs attention: please look"),
            "{hover}"
        );
        assert!(!hover.contains("carries on"), "CONTROL: {hover}");
    }

    #[test]
    fn a_failed_command_is_a_note_and_never_a_need_mark() {
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let _ = second_tab(&mut app, wid);
        let tab = app.windows[&wid].tab_set.tabs().len() - 2;
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.tab_set.tab_at_mut(tab))
            .expect("the stub's tab")
            .presentation
            .indicators
            .status_attention = true;
        app.refresh_presence_session(first);
        let stamp = |app: &App| {
            let ws = &app.windows[&wid];
            let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
                .tab_set
                .tabs()
                .iter()
                .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
                .collect();
            app.stamp_presence_chips(wid, &mut metadata);
            metadata[tab]
        };
        let item = stamp(&app);
        assert_eq!(item.attention, ChipLevel::Note);
        assert_eq!(view(&app, wid).rim, Rim::None, "no border");
        assert!(app.presence_pill(wid).is_none(), "no pill");
        let ink = [1, 2, 3];
        let drawn = crate::tab_bar::status_mark_ink_for_test(item, ink);
        assert_eq!(drawn, ink, "the label's ink, no presence tone");
        ask(&first_ctx, Some("approve?"));
        app.refresh_presence_session(first);
        let item = stamp(&app);
        assert_eq!(item.attention, ChipLevel::Wait);
        assert_ne!(crate::tab_bar::status_mark_ink_for_test(item, ink), ink);
    }

    #[test]
    fn a_stale_pill_click_acts_on_nothing() {
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let (second, _) = second_tab(&mut app, wid);
        ask(&first_ctx, Some("approve?"));
        app.refresh_presence_session(first);
        assert_eq!(app.focused_session_id(wid), Some(second));
        app.on_pill_act(wid, second, PillHit::Go);
        assert_eq!(app.focused_session_id(wid), Some(second), "stale: nothing");
        app.on_pill_act(wid, first, PillHit::Act(crate::presence::PillAct::Restart));
        assert_eq!(app.focused_session_id(wid), Some(second), "not offered");
        app.on_pill_act(wid, first, PillHit::Go);
        assert_eq!(
            app.focused_session_id(wid),
            Some(first),
            "the body goes there"
        );
    }

    #[test]
    fn the_pills_hand_back_lifts_the_persons_hold() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        app.take_over_session(sid).expect("the person takes it");
        app.refresh_presence_session(sid);
        let pill = view(&app, wid).pill.as_ref().expect("yours is a pill");
        assert_eq!(
            (pill.session, pill.act),
            (sid, Some(crate::presence::PillAct::HandBack))
        );
        let hand_back = PillHit::Act(crate::presence::PillAct::HandBack);
        app.with_gesture_source(true, |app| app.on_pill_act(wid, sid, hand_back));
        assert!(
            ctx.fabric.holds().person().is_some(),
            "a control client's click lifts nothing"
        );
        app.on_pill_act(wid, sid, hand_back);
        assert!(ctx.fabric.holds().person().is_none(), "handed back");
    }

    #[test]
    fn take_over_lights_every_yours_and_the_chips_hand_back_lifts_it() {
        use crate::claude_lights::{Chip, TeamFace};
        use aterm_types::needs::HoldWord;
        let (mut app, wid, sid, ctx) = app_with_stub();
        let yours = |app: &App| {
            (
                view(app, wid).rim,
                app.presence.slot(sid).map(|s| s.chip()),
                app.team_face(sid),
                app.status_glance()
                    .held
                    .iter()
                    .map(|h| h.hold)
                    .collect::<Vec<_>>(),
            )
        };
        app.refresh_presence_session(sid);
        assert_eq!(
            yours(&app),
            (Rim::None, Some(ChipLevel::Off), None, Vec::new()),
            "control: nothing is yours"
        );
        app.take_over_session(sid).expect("the person takes it");
        assert_eq!(
            yours(&app),
            (
                Rim::Yours,
                Some(ChipLevel::Yours),
                Some(TeamFace::Yours),
                vec![HoldWord::Person]
            )
        );
        assert!(
            app.presence_chrome_line().contains("rim=yours"),
            "{}",
            app.presence_chrome_line()
        );
        app.note_claude_team(wid, sid);
        app.with_gesture_source(true, |app| {
            app.press_claude_chip(wid, sid, Chip::Team, std::time::Instant::now());
        });
        assert!(
            ctx.fabric.holds().person().is_some(),
            "a control client's press lifts nothing"
        );
        app.press_claude_chip(wid, sid, Chip::Team, std::time::Instant::now());
        assert!(ctx.fabric.holds().person().is_none(), "handed back");
        assert_eq!(
            yours(&app),
            (Rim::None, Some(ChipLevel::Off), None, Vec::new())
        );
    }

    #[test]
    fn a_managers_team_chip_opens_the_overview_at_it() {
        use crate::claude_lights::Chip;
        use crate::overview::{OverviewAct, OverviewItem};
        let (mut app, wid, manager, manager_ctx) = app_with_stub();
        let (worker, worker_ctx) = second_tab(&mut app, wid);
        let (stray, stray_ctx) = second_tab(&mut app, wid);
        let from = |sid: &str| TaskMark {
            from: Some(sid.to_string()),
            ..TaskMark::new(TaskState::Working)
        };
        let manager_sid = manager_ctx.self_id.as_str().to_string();
        mark_session_task(&worker_ctx, worker, &from(&manager_sid)).expect("unfenced");
        app.on_task_changed(worker);
        mark_session_task(&stray_ctx, stray, &from("s-not-here")).expect("unfenced");
        app.on_task_changed(stray);
        let _ = crate::app_tabs::take_raised_windows();

        app.note_claude_team(wid, stray);
        app.press_claude_chip(wid, stray, Chip::Team, std::time::Instant::now());
        assert!(app.windows[&wid].overview().is_none(), "control: no card");
        assert_eq!(crate::app_tabs::take_raised_windows(), Vec::new());

        app.note_claude_team(wid, manager);
        app.press_claude_chip(wid, manager, Chip::Team, std::time::Instant::now());
        assert_eq!(crate::app_tabs::take_raised_windows(), vec![wid]);
        assert_eq!(app.focused_session_id(wid), Some(stray), "no tab moved");
        let card = app
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.overview_mut())
            .expect("the card is open");
        assert!(matches!(
            card.selected_item(),
            Some(OverviewItem::Manages { .. })
        ));
        assert_eq!(card.activate(), OverviewAct::Raise(manager));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn the_pills_resume_continues_only_a_job_still_stopped() {
        use aterm_uds::spawnfd::InheritNothing as _;
        use std::os::fd::{FromRawFd as _, OwnedFd};
        use std::os::unix::process::CommandExt as _;
        struct Leader {
            child: std::process::Child,
            master: i32,
        }
        impl Drop for Leader {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                // SAFETY: the master fd this test opened and alone owns.
                unsafe { libc::close(self.master) };
            }
        }
        let (_app, _wid, _sid, ctx) = app_with_stub();
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: `slave` is a fresh fd this test alone owns.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let stdio = || std::process::Stdio::from(slave.try_clone().expect("dup slave"));
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.inherit_nothing()
            .arg("60")
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio());
        // SAFETY: in the forked child before exec: `setsid` and `ioctl(TIOCSCTTY)` are async-signal-safe and touch nothing of the parent's.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let led = Leader {
            child: cmd.spawn().expect("spawn sleep"),
            master,
        };
        drop(slave);
        let pid = libc::pid_t::try_from(led.child.id()).expect("pid");
        let until = |what: &str, stopped: bool| {
            aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
                (crate::input_stall::pid_stopped(pid) == Some(stopped)).then_some(())
            });
        };
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the child leads the pty's foreground",
            // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
            || (unsafe { libc::tcgetpgrp(led.master) } == pid).then_some(()),
        );
        until("the job runs", false);
        assert_eq!(
            resume_if_stopped(led.master, &ctx),
            "ERR not-stopped\n",
            "CONTROL: a running job is sent nothing"
        );
        // SAFETY: `pid` is this test's own live child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        until("the job stops", true);
        let reply = resume_if_stopped(led.master, &ctx);
        assert!(reply.starts_with("OK signalled"), "{reply}");
        until("the job is continued", false);
    }

    #[test]
    fn a_shared_sessions_pill_lands_in_the_window_clicked() {
        let shared = |via_pill: bool| {
            let (mut app, w0, first, first_ctx) = app_with_stub();
            app.frontmost_window = Some(w0);
            let w1 = app
                .open_active_session_in_new_window_logical()
                .expect("the share");
            let (other, _) = second_tab(&mut app, w1);
            assert!(w0 < w1, "PRECONDITION: the first window showing it is w0");
            assert_eq!(app.focused_session_id(w1), Some(other));
            ask(&first_ctx, Some("approve?"));
            app.refresh_presence_session(first);
            let pill = view(&app, w1).pill.clone().expect("w1 is lit");
            assert_eq!((pill.session, pill.tab), (first, Some(1)));
            if via_pill {
                app.on_pill_act(w1, first, PillHit::Go);
            } else {
                let _ = app.focus_session_window(first);
            }
            (first, app.focused_session_id(w1))
        };
        let (first, focused) = shared(true);
        assert_eq!(focused, Some(first), "the tab clicked comes to the front");
        let (first, focused) = shared(false);
        assert_ne!(focused, Some(first), "control: w1 stays where it was");
    }

    #[test]
    fn the_pills_body_goes_to_the_pane_that_needs_you() {
        let (mut app, wid, first, first_ctx) = app_with_stub();
        first_ctx.meta.lock().unwrap().role = Some("fmt".into());
        let other = app.split_active_stub_tab(wid);
        assert_eq!(app.focused_session_id(wid), Some(other), "the new pane");
        ask(&first_ctx, Some("approve?"));
        app.refresh_presence_session(first);
        let pill = app.presence_pill(wid).expect("lit");
        assert_eq!(pill.session, first);
        assert!(pill.words.label.contains("fmt"), "{:?}", pill.words);
        app.on_pill_act(wid, first, PillHit::Go);
        assert_eq!(app.focused_session_id(wid), Some(first), "the pane");
        let pill = app.presence_pill(wid).expect("still lit");
        assert!(pill.words.label.is_empty(), "{:?}", pill.words);
    }

    #[test]
    fn a_pill_naming_another_pane_names_it_with_nothing_else_to_go_on() {
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let other = app.split_active_stub_tab(wid);
        assert_eq!(app.focused_session_id(wid), Some(other), "the new pane");
        ask(&first_ctx, Some("approve?"));
        app.refresh_presence_session(first);
        let pill = app.presence_pill(wid).expect("lit");
        assert_eq!(pill.session, first);
        assert!(!pill.words.label.is_empty(), "{:?}", pill.words);
        assert_eq!(pill.words.label, app.tab_label_of_session(first));
        app.on_pill_act(wid, first, PillHit::Go);
        let pill = app.presence_pill(wid).expect("still lit");
        assert!(pill.words.label.is_empty(), "{:?}", pill.words);
    }

    #[test]
    fn a_background_worker_has_one_name_on_every_surface() {
        let (mut app, wid, _front, _) = app_with_stub();
        let (worker, worker_ctx) = second_tab(&mut app, wid);
        app.switch_tab_in(wid, 0);
        crate::term_lock(&app.pool.get(worker).expect("pooled").term)
            .process("\x1b]2;\u{2733} bench\x07".as_bytes());
        ask(&worker_ctx, Some("approve?"));
        app.refresh_presence_session(worker);

        let glance = app.status_glance();
        assert_eq!(glance.local_need_sessions(), vec![worker]);
        assert_eq!(glance.needs[0].label, "bench", "❯'s need row");
        let handle = app
            .store
            .read()
            .unwrap()
            .by_local(worker)
            .cloned()
            .expect("registered");
        let notice = crate::status_item::escalation(&app.status_session_row(&handle))
            .expect("a need's notice");
        assert!(notice.body.starts_with("bench"), "{}", notice.body);
        let pill = app
            .presence_view(wid)
            .and_then(|v| v.pill.as_ref())
            .map(|p| p.label.clone())
            .expect("lit");
        assert_eq!(pill, "bench", "the pill");
        app.open_overview().expect("the card opens");
        let card = app
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.overview_mut())
            .expect("open");
        assert!(card.select_session(worker, false), "the worker's chip");
        assert_eq!(
            card.selected_chip().map(|chip| chip.label.as_str()),
            Some("bench"),
            "the Overview"
        );
        let registry = crate::status_item::tab_name(None, None, &handle.title)
            .unwrap_or_else(|| presence::short_sid(handle.sid.as_str()));
        assert_eq!(
            registry,
            presence::short_sid(handle.sid.as_str()),
            "{:?}",
            handle.title
        );
    }

    #[test]
    fn the_pill_names_a_background_tab_by_the_title_its_strip_shows() {
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let _ = second_tab(&mut app, wid);
        let tab = app.windows[&wid].tab_set.tabs().len() - 2;
        let retitle = |app: &App, title: &str| {
            crate::term_lock(&app.pool.get(first).expect("pooled").term)
                .process(format!("\x1b]0;{title}\x07").as_bytes());
        };
        retitle(&app, "\u{2733} review");
        ask(&first_ctx, Some("approve?"));
        app.refresh_presence_session(first);
        let label = |app: &App| {
            app.presence_view(wid)
                .and_then(|v| v.pill.as_ref())
                .map(|p| p.label.clone())
                .expect("lit")
        };
        let strip = app.tab_titles(wid)[tab].clone();
        assert_eq!(
            label(&app),
            crate::app_take_over::title_without_agent_glyph(&strip),
            "{strip:?}"
        );
        assert!(!label(&app).contains('\u{2733}'));
        retitle(&app, "deploy");
        assert!(app.flush_title_drift(first, Instant::now()));
        assert_eq!(label(&app), app.tab_titles(wid)[tab]);
        first_ctx.meta.lock().unwrap().user_title = Some("mytab".into());
        app.on_meta_changed(first);
        assert_eq!(label(&app), "mytab");
    }

    #[test]
    fn more_opens_the_overview_at_the_windows_next_need() {
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let (second, second_ctx) = second_tab(&mut app, wid);
        let (front, _) = second_tab(&mut app, wid);
        let t0 = Instant::now();
        set_presence_clock_for_test(Some(t0));
        ask(&first_ctx, Some("one?"));
        app.refresh_presence_session(first);
        let _ = crate::app_tabs::take_raised_windows();
        assert_eq!(view(&app, wid).pill.as_ref().map(|p| p.more), Some(0));
        app.on_pill_act(wid, first, PillHit::More);
        assert!(app.windows[&wid].overview().is_none(), "control: no card");
        assert_eq!(crate::app_tabs::take_raised_windows(), Vec::new());

        set_presence_clock_for_test(Some(t0 + Duration::from_secs(1)));
        ask(&second_ctx, Some("two?"));
        app.refresh_presence_session(second);
        let named = view(&app, wid).pill.as_ref().map(|p| (p.session, p.more));
        assert_eq!(named, Some((first, 1)), "the older need, and one more");
        app.on_pill_act(wid, second, PillHit::More);
        assert!(app.windows[&wid].overview().is_none(), "stale: no card");
        app.on_pill_act(wid, first, PillHit::More);
        let card = app.windows[&wid].overview().expect("the card is open");
        assert_eq!(
            card.selected_chip().map(|c| c.local_id),
            Some(second),
            "at the next need"
        );
        assert_eq!(crate::app_tabs::take_raised_windows(), vec![wid]);
        assert_eq!(app.focused_session_id(wid), Some(front), "no tab moved");
        set_presence_clock_for_test(None);
    }

    #[test]
    fn the_in_grid_strip_carries_the_pill_and_its_press_goes_to_the_tab() {
        use crate::tab_bar::TabHit;
        let (mut app, wid, first, first_ctx) = app_with_stub();
        let (second, _) = second_tab(&mut app, wid);
        app.tab_strip_rows = 1;
        assert!(
            app.strip_carries_pill(wid),
            "a headless window has no titlebar strip"
        );
        app.splice_tab_strip(wid);
        let body = |app: &App| {
            app.windows[&wid]
                .tab_segments
                .iter()
                .find(|s| s.kind == TabHit::Pill(PillHit::Go))
                .map(|s| s.start_col + 1)
        };
        assert_eq!(body(&app), None, "no need, no pill");
        ask(&first_ctx, Some("approve?"));
        app.refresh_presence_session(first);
        app.splice_tab_strip(wid);
        let col = body(&app).expect("the pill's body is laid out");
        assert_eq!(app.windows[&wid].strip_pill_session, Some(first));
        app.windows.get_mut(&wid).unwrap().strip_pill_session = Some(second);
        app.handle_tab_strip_click(wid, col, std::time::Instant::now());
        assert_eq!(app.focused_session_id(wid), Some(second), "stale: nothing");
        app.windows.get_mut(&wid).unwrap().strip_pill_session = Some(first);
        app.handle_tab_strip_click(wid, col, std::time::Instant::now());
        assert_eq!(
            app.focused_session_id(wid),
            Some(first),
            "the body goes there"
        );
        ask(&first_ctx, None);
        app.refresh_presence_session(first);
        app.splice_tab_strip(wid);
        assert_eq!(body(&app), None, "the need cleared, the pill went");
        assert_eq!(app.windows[&wid].strip_pill_session, None);
    }

    #[test]
    fn workers_name_their_manager_on_the_chip_the_strip_and_the_press() {
        use crate::claude_lights::TeamFace;
        let (mut app, wid, manager, manager_ctx) = app_with_stub();
        manager_ctx.meta.lock().unwrap().role = Some("lab-sweep".into());
        let (bench, bench_ctx) = second_tab(&mut app, wid);
        let (fmt, fmt_ctx) = second_tab(&mut app, wid);
        let (solo, solo_ctx) = second_tab(&mut app, wid);
        let from = manager_ctx.self_id.as_str().to_string();
        for (local, ctx) in [(bench, &bench_ctx), (fmt, &fmt_ctx)] {
            let mark = TaskMark {
                from: Some(from.clone()),
                ..TaskMark::new(TaskState::Working)
            };
            mark_session_task(ctx, local, &mark).expect("unfenced");
            app.on_task_changed(local);
        }
        task(&solo_ctx, solo, TaskState::Working);
        app.on_task_changed(solo);
        assert_eq!(
            app.team_face(manager),
            Some(TeamFace::Manager {
                mark: std::sync::Arc::from("\u{21c9}2"),
                workers: 2
            })
        );
        assert!(matches!(
            app.team_face(bench),
            Some(TeamFace::Worker { manager: Some(m), .. }) if m == manager
        ));
        assert_eq!(app.team_face(solo), None, "control: from=ctl is no team");
        app.refresh_window_tabs(wid);
        let shown = |app: &App, session: u64| {
            let ws = &app.windows[&wid];
            let tab = ws
                .tab_set
                .tabs()
                .iter()
                .find(|t| {
                    app.view_store
                        .get(t.focus)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                })
                .expect("the session's tab")
                .id;
            ws.tab_chrome_titles_by_tab[&tab].1.clone()
        };
        assert!(
            shown(&app, bench).starts_with('\u{21b3}'),
            "{}",
            shown(&app, bench)
        );
        assert!(
            shown(&app, manager).ends_with("\u{21c9}2"),
            "{}",
            shown(&app, manager)
        );
        assert!(!shown(&app, solo).contains('\u{21b3}'));
        assert!(
            !app.tab_label_of_session(manager).contains('\u{21c9}'),
            "the tab's own name keeps no mark"
        );
        app.note_claude_team(wid, bench);
        app.press_claude_chip(
            wid,
            bench,
            crate::claude_lights::Chip::Team,
            std::time::Instant::now(),
        );
        assert_eq!(app.focused_session_id(wid), Some(manager));
        assert!(app.detach_session_view(fmt), "the last view of it");
        assert!(matches!(
            app.team_face(manager),
            Some(TeamFace::Manager { workers: 1, .. })
        ));
    }

    #[test]
    fn a_panic_inside_a_chrome_regrid_batch_ends_the_batch() {
        let mut app = App::headless_for_test();
        let _ = take_chrome_regrids_applied();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.with_chrome_regrid_batch(|app| {
                app.regrid_for_chrome_rows();
                assert!(take_chrome_regrids_applied().is_empty(), "recorded");
                panic!("inside the batch");
            })
        }));
        assert!(caught.is_err(), "the panic resumed");
        assert!(app.chrome_regrid_batch.is_none(), "the batch ended");
        assert!(
            !take_chrome_regrids_applied().is_empty(),
            "what it recorded was applied"
        );
        app.regrid_for_chrome_rows();
        assert!(
            !take_chrome_regrids_applied().is_empty(),
            "the next re-grid applies at once"
        );
    }

    #[test]
    fn a_chrome_commit_inside_a_batch_measures_the_pending_regrid() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let size = aterm_window::dpi::PhysicalSize::new(1600, 1200);
        let gridded = app.grid_dims_for(wid, size).0;
        assert!(gridded > 2, "a window with rows to spare");
        app.message_band_rows += 1;
        assert_eq!(
            app.chrome_commit_grid_rows_at(wid, gridded, Some(size)),
            gridded,
            "outside a batch the grid re-grids at once, so it is current"
        );
        app.chrome_regrid_batch = Some(ChromeRegridBatch::default());
        app.regrid_for_chrome_rows();
        assert_eq!(
            app.chrome_commit_grid_rows_at(wid, gridded, Some(size)),
            gridded - 1,
            "the pending re-grid gives a row fewer"
        );
        assert_eq!(
            app.chrome_commit_grid_rows_at(wid, gridded, None),
            gridded,
            "a headless window never re-grids for chrome"
        );
        app.chrome_regrid_batch = None;
        let _ = take_chrome_regrids_applied();
    }

    #[test]
    fn a_calm_key_reads_the_story_on_the_wire() {
        let (mut app, wid, sid, _ctx) = app_with_stub();
        app.refresh_presence_session(sid);
        app.tell_story(sid, presence::StoryVerb::Chose)
            .expect("the stub is held");
        assert_eq!(app.presence_session_level(sid), presence::Level::Story);
        app.note_human_acted(wid);
        assert_eq!(app.presence_session_level(sid), presence::Level::Quiet);
    }

    #[test]
    fn a_proven_self_claim_with_resume_limits_reads_a_usage_wall_as_note() {
        use crate::presence::Level;
        crate::fabric::with_link_reset(|| {
            let (mut app, _wid, sid, ctx) = app_with_stub();
            {
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(42);
                timeline.set_program(42, Some("claude".into()));
            }
            let rows = aterm_phase::prompt::fixtures::screen(
                aterm_phase::prompt::fixtures::SESSION_LIMIT_THEN_LOCAL_COMMANDS_MEASURED,
            );
            let step = app.session_status.agent_observe(
                sid,
                crate::control::ScreenGen { epoch: 0, seq: 1 },
                Some(rows),
                crate::presence::Cursor::Unknown,
                Some("claude".into()),
                42,
                false,
                Instant::now(),
            );
            let (word, detail, subject, reader) = step.publish.expect("the wall was read");
            ctx.timeline.lock().unwrap().publish_agent(
                word,
                detail,
                subject,
                reader,
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 0, seq: 1 },
                    fp: 1,
                },
            );
            let facts = app.presence_facts(sid).unwrap();
            assert!(
                matches!(
                    facts.agent.as_ref().map(|a| &a.phase),
                    Some(presence::AgentPhase::Wall {
                        kind: aterm_phase::WallKind::UsageSession,
                        ..
                    })
                ),
                "precondition: a usage wall"
            );
            assert_eq!(facts.wall_answer, presence::WallAnswer::Nobody, "no claim");
            app.harness = Some(crate::harness_host::HostHandle::for_supervised_test(
                Vec::new(),
                Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
            ));
            let level = |app: &mut App| {
                app.refresh_presence_session(sid);
                let slot = app.presence.slot(sid).expect("a refreshed slot");
                (
                    app.presence_facts(sid).unwrap().wall_answer,
                    slot.level(slot.story_seq),
                )
            };
            crate::session_timeline::claim_supervisor(
                &ctx,
                "codex-alab@4242",
                Some(7),
                None,
                crate::metrics::now_us(),
            )
            .unwrap();
            assert_eq!(
                level(&mut app),
                (presence::WallAnswer::Nobody, Level::Limited),
                "an unproven self-claim"
            );
            crate::session_timeline::release_supervisor(
                &ctx,
                None,
                crate::metrics::now_us(),
                aterm_slog::Actor::Ctl,
            );
            crate::session_timeline::claim_supervisor_as(
                &ctx,
                "codex-alab@4242",
                Some(7),
                None,
                Some(crate::self_claim::Proven {
                    pid: 4242,
                    start: "test".into(),
                }),
                crate::metrics::now_us(),
                aterm_slog::Actor::Ctl,
            )
            .unwrap();
            assert!(
                ctx.meta
                    .lock()
                    .unwrap()
                    .supervisor_proven(crate::metrics::now_us())
            );
            assert_eq!(
                level(&mut app),
                (presence::WallAnswer::Nobody, Level::Limited),
                "Claude Code's wall under a codex-alab claim is nobody's"
            );
            let in_front = |pgid: i32, program: &str| {
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(pgid);
                timeline.set_program(pgid, Some(program.into()));
            };
            in_front(4242, "codex");
            assert_eq!(
                level(&mut app),
                (presence::WallAnswer::Nobody, Level::Limited),
                "a reading made under Claude Code is not the claimant's"
            );
            app.session_status.stand_in_reading_program(sid, "codex");
            for claimant in ["codex-alab", "codex"] {
                in_front(4242, claimant);
                assert_eq!(
                    level(&mut app),
                    (presence::WallAnswer::Harness, Level::Note),
                    "{claimant}: resumes by itself"
                );
            }
            in_front(77, "codex");
            assert_eq!(
                level(&mut app),
                (presence::WallAnswer::Nobody, Level::Limited),
                "a codex the proven pid does not lead"
            );
            in_front(4242, "codex-alab");
            for toml in [
                "[harness]\nresume_limits = false\n",
                "[harness]\nenabled = false\n",
            ] {
                app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                    crate::app_config::HarnessPolicy::read(toml),
                );
                assert_eq!(
                    level(&mut app),
                    (presence::WallAnswer::Nobody, Level::Limited),
                    "{toml:?}"
                );
            }
            if let Some(host) = app.harness.take() {
                host.shutdown_and_join();
            }
        });
    }

    /// A WALL THE LOOP TYPES NOTHING INTO IS NOBODY'S: under this host's own claim, Claude Code's
    /// usage wall is the harness's to resume, and a wall of a program no act of the loop reaches
    /// ([`aterm_phase::Program::supervisable`]) is nobody's, so its need is a person's rather
    /// than a `waiting:` nothing ends. Codex is one: the host types nothing into it (its worker
    /// watches beside codex-alab and never takes the claim), so the same standing wall, once a
    /// Codex leads the tab under the host's claim, reads nobody's where it once read the
    /// harness's; that screen-level step is the one the host's per-agent gate decides. Grok
    /// Build's weekly limit reads nobody's on the screen too, but there two filters ahead of the
    /// host decide it whatever the host says (its reading is no evidence, so the presence shows
    /// no wall, and [`crate::harness_host::agent_of`] drops a program with no
    /// `[harness.<agent>]` table), so for it the host's own answer is asked directly: it is what
    /// stays honest once either filter moves.
    #[test]
    fn under_the_host_a_grok_limit_wall_is_nobodys_and_claudes_resumes() {
        crate::fabric::with_link_reset(|| {
            let (mut app, _wid, sid, ctx) = app_with_stub();
            app.harness = Some(crate::harness_host::HostHandle::for_supervised_test(
                Vec::new(),
                Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
            ));
            crate::session_timeline::claim_supervisor(
                &ctx,
                &crate::harness_host::holder(),
                None,
                None,
                crate::metrics::now_us(),
            )
            .unwrap();
            let mut seq = 0;
            let mut wall = |app: &mut App, program: &str, screen: &str| {
                seq += 1;
                {
                    let mut timeline = ctx.timeline.lock().unwrap();
                    timeline.note_foreground_group(42);
                    timeline.set_program(42, Some(program.into()));
                }
                let step = app.session_status.agent_observe(
                    sid,
                    crate::control::ScreenGen { epoch: 0, seq },
                    Some(aterm_phase::prompt::fixtures::screen(screen)),
                    crate::presence::Cursor::Unknown,
                    Some(program.into()),
                    42,
                    false,
                    Instant::now(),
                );
                let (word, detail, subject, reader) = step.publish.expect("the wall was read");
                ctx.timeline.lock().unwrap().publish_agent(
                    word,
                    detail,
                    subject,
                    reader,
                    crate::session_timeline::AgentStamp {
                        generation: crate::control::ScreenGen { epoch: 0, seq },
                        fp: seq,
                    },
                );
                let facts = app.presence_facts(sid).unwrap();
                let kind = match facts.agent.as_ref().map(|a| &a.phase) {
                    Some(presence::AgentPhase::Wall { kind, .. }) => Some(*kind),
                    _ => None,
                };
                (kind, facts.wall_answer)
            };
            assert_eq!(
                wall(
                    &mut app,
                    "claude",
                    aterm_phase::prompt::fixtures::SESSION_LIMIT_THEN_LOCAL_COMMANDS_MEASURED,
                ),
                (
                    Some(aterm_phase::WallKind::UsageSession),
                    presence::WallAnswer::Harness
                ),
                "Claude Code's usage wall under the host's claim resumes"
            );
            {
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(43);
                timeline.set_program(43, Some("codex".into()));
            }
            let facts = app.presence_facts(sid).unwrap();
            assert!(
                matches!(
                    facts.agent.as_ref().map(|a| &a.phase),
                    Some(presence::AgentPhase::Wall {
                        kind: aterm_phase::WallKind::UsageSession,
                        ..
                    })
                ),
                "precondition: the wall still stands"
            );
            assert_eq!(
                facts.wall_answer,
                presence::WallAnswer::Nobody,
                "a wall while a Codex leads the tab under the host's claim is nobody's"
            );
            let (_, grok) = wall(
                &mut app,
                "grok",
                aterm_phase::grok::fixtures::WALL_WEEKLY_LIMIT,
            );
            assert_eq!(
                grok,
                presence::WallAnswer::Nobody,
                "Grok Build's weekly limit under the host's claim is nobody's"
            );
            let host = app.harness.take().expect("the host");
            for kind in [
                aterm_phase::WallKind::UsageSession,
                aterm_phase::WallKind::UsageWeekly,
                aterm_phase::WallKind::Spend,
                aterm_phase::WallKind::Overloaded,
                aterm_phase::WallKind::Auth,
            ] {
                for agent in [
                    aterm_phase::Program::Grok,
                    aterm_phase::Program::Kimi,
                    aterm_phase::Program::Codex,
                ] {
                    assert_eq!(
                        host.wall_answer(agent, kind, true),
                        presence::WallAnswer::Nobody,
                        "{agent:?} {kind:?}"
                    );
                }
            }
            assert_eq!(
                host.wall_answer(
                    aterm_phase::Program::Claude,
                    aterm_phase::WallKind::UsageWeekly,
                    true
                ),
                presence::WallAnswer::Harness
            );
            assert_eq!(
                host.wall_answer(
                    aterm_phase::Program::Claude,
                    aterm_phase::WallKind::Auth,
                    false
                ),
                presence::WallAnswer::Harness,
                "Claude Code's lost login is carried"
            );
            assert_eq!(
                host.wall_answer(
                    aterm_phase::Program::Agy,
                    aterm_phase::WallKind::Auth,
                    false
                ),
                presence::WallAnswer::Nobody,
                "agy's lost login is not"
            );
            host.shutdown_and_join();
        });
    }

    #[test]
    fn status_hand_pid_names_the_leases_kernel_attested_taker() {
        crate::fabric::with_link_reset(|| {
            let (mut app, _wid, sid, ctx) = app_with_stub();
            let own = crate::harness_host::holder();
            let me = std::process::id();
            let now_us = crate::metrics::now_us();
            let drive = |holder_pid: Option<u32>, expires_us: u64| crate::Lease::Drive {
                holder: own.clone(),
                expires_us,
                conn: None,
                hard: true,
                here: false,
                holder_pid,
            };
            let mut read = |lease: Option<crate::Lease>| {
                *ctx.turn_lease.lock().unwrap() = lease;
                app.on_presence_wake(&ctx.self_id);
                app.presence_status_tail(sid)
            };
            let hand = format!("hand=lease:{}", crate::control::pct_encode(&own));
            let tail = read(Some(drive(Some(me), now_us + 60_000_000)));
            assert!(
                tail.starts_with(&format!("{hand} hand_pid={me} ")),
                "{tail}"
            );
            let forger = me + 1;
            let tail = read(Some(drive(Some(forger), now_us + 60_000_000)));
            assert!(
                tail.starts_with(&format!("{hand} hand_pid={forger} ")),
                "{tail}"
            );
            let tail = read(Some(drive(None, now_us + 60_000_000)));
            assert!(tail.starts_with(&format!("{hand} hand_pid=- ")), "{tail}");
            let tail = read(Some(drive(Some(me), now_us.saturating_sub(1))));
            assert!(tail.starts_with("hand=- hand_pid=- "), "lapsed: {tail}");
            let tail = read(Some(crate::Lease::Turn {
                id: 9,
                driver: None,
                typing: false,
            }));
            assert!(tail.starts_with("hand=turn:9 hand_pid=- "), "{tail}");
            let tail = read(None);
            assert!(tail.starts_with("hand=- hand_pid=- "), "{tail}");
        });
    }

    #[test]
    fn a_background_codex_turn_end_puts_the_dot_on_its_tab() {
        crate::fabric::with_link_reset(|| {
            let (mut app, wid, sid_a, _ctx_a) = app_with_stub();
            let ia = app.windows[&wid].tab_set.tabs().len() - 1;
            let sid_b = app.next_session_id;
            app.push_stub_tab(wid, crate::stub_session(sid_b));
            let ib = ia + 1;
            assert_eq!(
                app.focused_session_id(wid),
                Some(sid_b),
                "A is in the background"
            );
            let report = |bytes: &[u8]| {
                let mut term = aterm_core::terminal::Terminal::new(24, 80);
                term.process(bytes);
                term.program_state()
            };
            let working = report(b"\x1b]7501;state=working:app=codex\x1b\\");
            let mut done = report(b"\x1b]7501;state=done:app=codex\x1b\\");
            done.seq = working.seq + 1;
            let chips = |app: &App| {
                let mut m: Vec<_> = app.windows[&wid]
                    .tab_set
                    .tabs()
                    .iter()
                    .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
                    .collect();
                app.stamp_presence_chips(wid, &mut m);
                (m[ia].attention, m[ib].attention)
            };
            let now = Instant::now();
            for sid in [sid_a, sid_b] {
                app.session_status
                    .observe_program_state(sid, working, true, false, now);
                app.refresh_presence_session(sid);
            }
            assert_eq!(
                chips(&app),
                (ChipLevel::Off, ChipLevel::Off),
                "working is no news"
            );
            app.windows.get_mut(&wid).expect("window").focused = true;
            for sid in [sid_a, sid_b] {
                app.session_status
                    .observe_program_state(sid, done, true, false, now);
                app.refresh_presence_session(sid);
            }
            assert_eq!(
                chips(&app),
                (ChipLevel::Done, ChipLevel::Off),
                "the background tab's dot, and none on the tab in front: A `{}` B `{}`",
                app.presence_status_tail(sid_a),
                app.presence_status_tail(sid_b)
            );
            assert!(
                app.presence_status_tail(sid_a).contains("level=story"),
                "the background tab's turn end is a story: `{}`",
                app.presence_status_tail(sid_a)
            );
            assert!(
                app.presence_status_tail(sid_b).contains("level=quiet"),
                "the tab in front is not news: `{}`",
                app.presence_status_tail(sid_b)
            );
            app.windows.get_mut(&wid).expect("window").focused = false;
            let mut working2 = working;
            working2.seq = done.seq + 1;
            let mut done2 = done;
            done2.seq = working2.seq + 1;
            for state in [working2, done2] {
                app.session_status
                    .observe_program_state(sid_b, state, true, false, now);
                app.refresh_presence_session(sid_b);
            }
            assert!(
                app.presence_status_tail(sid_b).contains("level=story"),
                "`{}`",
                app.presence_status_tail(sid_b)
            );
            app.windows
                .get_mut(&wid)
                .expect("window")
                .band_on_screen_for_test = true;
            assert_eq!(app.frontmost_window, Some(wid));
            app.switch_tab_in(wid, ia);
            assert_eq!(app.focused_session_id(wid), Some(sid_a));
            assert_eq!(chips(&app), (ChipLevel::Off, ChipLevel::Done));
            app.switch_tab_in(wid, ib);
            assert_eq!(chips(&app), (ChipLevel::Off, ChipLevel::Off));
        });
    }
}

#[cfg(test)]
#[path = "person_stand_in_tests.rs"]
mod person_stand_in_tests;

#[cfg(test)]
#[path = "look_needs_focus_tests.rs"]
mod look_needs_focus_tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The App's side of presence ([`crate::presence`]).

use std::time::{Duration, Instant};

use aterm_messages::presence::drive::{self, Desk};
use aterm_session::SessionId;

use crate::presence::{self, Facts, Hand, HoldFact, Link, StoryVerb, TurnFact, WindowView};
use crate::{App, WindowId};

/// The chip level the indicator bits spell (`aterm_messages::presence::chip_of_attention`).
pub(crate) use aterm_messages::presence::chip_of_attention;

/// The per-session presence state.
#[derive(Default, Debug)]
pub(crate) struct PresenceTable {
    table: drive::Table<presence::Native>,
    pub(crate) need_ages: crate::status_item::NeedAges,
}

impl PresenceTable {
    pub(crate) fn slot(&self, session: u64) -> Option<&presence::Slot> {
        self.table.slot(session)
    }

    pub(crate) fn retire(&mut self, session: u64) {
        self.table.retire(session);
        self.need_ages.retire(session);
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

/// Whether `program`'s sign-in wall (its sign-in screen's, [`crate::presence::SIGN_IN_SCREEN`])
/// folds into the machine's one sign-in (`aterm harness signin --cli <id>`,
/// [`crate::needs::GrokSignin`]): Grok Build's alone.
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
        let (need, held_by_person, codex_ctx_left) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let verdict = tl.need();
            (
                verdict.need.as_ref().map(presence::need_fact),
                verdict.held == Some(aterm_types::needs::HoldWord::Person),
                tl.codex_survey().and_then(|codex| codex.facts.ctx_left_pct),
            )
        };
        Some(Facts {
            attention,
            shell,
            program_ctx_left: program.and(codex_ctx_left),
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
                    .is_some_and(|holder| holder == crate::harness_host::holder())
                    && !crate::agent_act::observed(ctx.self_id.as_str(), front_group) =>
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
        let (agent, program_blocked, admit, stall, task, resume, age_ms, grok_walled) = {
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
            // The walled program's group leader, whose start says whether a login predates it:
            // only its sign-in screen's wall, never a 401's row or a login-ended text, which
            // run no sign-in on the screen and say nothing of the login file it was drawn over.
            let grok_walled = (a.word == "wall:auth"
                && a.subject.as_deref() == Some(crate::presence::SIGN_IN_SCREEN)
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
                grok_walled,
            )
        };
        let told = crate::needs::told_of(ctx, now_us);
        // A Grok session on a sign-in wall shows the last look's answer: nothing is read here.
        let grok_signin = match grok_walled {
            Some(pgid) => {
                let (proxy, session) = (self.proxy.clone(), h.local_id);
                Some(self.grok_looks.ask(session, pgid, move || {
                    if let Some(proxy) = &proxy {
                        let _ = proxy.send_event(crate::Wake::GrokSigninLooked { session });
                    }
                }))
            }
            None => {
                self.grok_looks.forget(h.local_id);
                None
            }
        };
        let host_supervises =
            crate::harness_host::admitted_as(ctx, admit.0, admit.1).is_some_and(hosted);
        // An agent the host observes only (its image unconfirmed, `supervisor_act=observe`) gets
        // nothing typed, so the host answers none of its boxes or walls: they stay the person's.
        let host_answers =
            host_supervises && !crate::agent_act::observed(h.ctx.self_id.as_str(), admit.1);
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
            supervised: host_answers || other_supervisor,
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
        if facts.is_none() {
            self.grok_looks.forget(session);
        }
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
        self.grok_looks.forget(session);
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
                let hhmm = crate::status_item::clock_words_local(*since, &|unix| {
                    self.messages_wall.offset_at(unix)
                });
                vec![crate::message_reporters::api_outage_entry(
                    self.api_outage.len(),
                    hhmm.as_deref(),
                )]
            }
        };
        self.except_replace(aterm_messages::exceptions::Family::ApiOutage, &entries);
    }

    /// Every session's need published again (and its age and chrome refreshed).
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
            crate::HarnessFact::Act(taken) => crate::harness_glass::on_taken(self, session, taken),
            crate::HarnessFact::Task(face) => {
                // Nor is its face in the gap of the self-claim's own re-exec
                // (`upgrade@v1`, then the claim given back and taken again):
                // the self-claim's face stands across it.
                if self.beside_proven_self_claim(session) || self.in_self_claim_reexec(session) {
                    self.clear_land_held(session);
                    return;
                }
                self.apply_task_face(session, face, None, None);
                if matches!(
                    face,
                    Some(
                        aterm_agent::harness::acts::TaskFace::Theirs
                            | aterm_agent::harness::acts::TaskFace::Failed
                    )
                ) {
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
            // A self-claim's acts happened under the proof that admitted them: each goes on the
            // glass as its holder's, whatever the claim stands at now.
            crate::HarnessFact::SelfAct => {
                let acts = {
                    let Some(entry) = self.pool.get(session) else {
                        return;
                    };
                    entry
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .take_self_acts()
                };
                for crate::session_timeline::SelfAct { act, holder } in acts {
                    match act {
                        Some(act) => crate::harness_glass::on_self_act(self, session, act, &holder),
                        None => self.note_harness_act_for_page(std::time::Instant::now(), false),
                    }
                }
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
            // Only Settings ▸ Harness's state line and `status waiting=` say it, so no title,
            // hover or status item is read again.
            crate::HarnessFact::Waiting(wait) => {
                if crate::harness_glass::on_waiting(self, session, wait) {
                    self.harness_screen_moved();
                }
            }
            // Only Settings ▸ Harness's state line and `status land_asked=` say it.
            crate::HarnessFact::SwitchAsked(asked) => {
                if crate::harness_glass::on_switch_asked(self, session, asked) {
                    self.harness_screen_moved();
                }
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
                self.mail_worker_news(session);
            }
            crate::HarnessFact::LandHeld => {
                if self.beside_proven_self_claim(session) {
                    self.clear_land_held(session);
                    return;
                }
                let Some(entry) = self.pool.get(session) else {
                    return;
                };
                let (held, said) = {
                    let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                    (timeline.land_held(), timeline.land_said().cloned())
                };
                self.show_land_held(session, held, said);
            }
        }
    }

    /// LAND'S HOLD OF SESSION `session` MOVED TO `held` on its glass, or its own words to
    /// `said` (§14.17 item 1, slice 4b: a reworded item moves the words under an equal hold).
    pub(crate) fn show_land_held(
        &mut self,
        session: u64,
        held: Option<aterm_agent::harness::acts::LandHeld>,
        said: Option<std::sync::Arc<aterm_agent::harness::hold_said::HoldSaid>>,
    ) {
        let hover_was = self.driven_by_aterm(session);
        let said = said.filter(|_| held.is_some());
        let words = crate::harness_glass::on_land_said(self, session, said);
        if !(crate::harness_glass::on_land_held(self, session, held) | words) {
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
    /// glass ([`crate::harness_glass::on_stopped`]), with whether the person's switch to Land
    /// would ask once more over it (`switch`, v6 §14.17 item 3), and every surface that says it
    /// read again — the chip's title where it is hovered, Settings ▸ Harness, the hover and the
    /// status item.
    pub(crate) fn show_stopped(
        &mut self,
        session: u64,
        stopped: Option<aterm_agent::harness::acts::HeldBy>,
        switch: bool,
    ) {
        let hover_was = self.driven_by_aterm(session);
        if !crate::harness_glass::on_stopped(self, session, stopped, switch) {
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
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.set_land_said(None);
            timeline.set_land_held(None);
        }
        self.show_land_held(session, None, None);
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

    /// Publish the window's front hold and record its look (`drive::refresh_window`).
    pub(crate) fn refresh_presence_window(&mut self, wid: WindowId) {
        drive::refresh_window(self, wid);
    }

    /// Every window: a restore, a successor's commit, a handoff's release and a config adopt.
    #[cfg(any(unix, test))]
    pub(crate) fn refresh_presence_all_windows(&mut self) {
        self.rebuild_teams();
        drive::refresh_all(self);
    }

    /// Keep activity pacing for every key or input-method callback, but known controller input
    /// establishes no person's presence. `origin` is the provenance captured at the native source:
    /// a key another process posted is a person's presence here, as their on-screen keyboard's or
    /// Screen Sharing's is.
    pub(crate) fn note_window_input_arrival(
        &mut self,
        event: &aterm_window::event::WindowEvent,
        origin: aterm_window::event::InputOrigin,
    ) {
        use aterm_window::event::WindowEvent;
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) {
            self.last_keystroke_at = Some(Instant::now());
        }
        if crate::is_person_input(event) && origin != aterm_window::event::InputOrigin::Posted {
            self.last_person_input_at = Some(Instant::now());
        }
    }

    /// One key from the winit `KeyboardInput` arm, from `origin` (as it arrived:
    /// `App::input_origin`). A key an `hwkey` burst posted takes this, the person's own key path,
    /// with no mark of its own, but it is a control client's gesture: its dispatch carries the
    /// controller mark, so the person-only leaves it reaches (⌘V, ⌘C, a Copy or Paste binding, the
    /// stats lens's copies, an Editor's kill ring and macro) refuse it as they refuse `key`. A key
    /// another process posted carries the relayed mark, so its edit of the aterm.toml in force is
    /// no person's ([`crate::owner_keys_save::Unvouched::Relayed`]).
    pub(crate) fn dispatch_hw_key(
        &mut self,
        wid: WindowId,
        event: aterm_window::event::KeyEvent,
        origin: aterm_window::event::InputOrigin,
    ) {
        self.with_origin_mark(origin, |app| app.dispatch_key_event(wid, event));
    }

    /// An input method's callback through the same mark as its causal native key. Composed text
    /// still follows the existing recipient and delivery paths. A commit also carries the mark of
    /// the keys that composed it ([`Self::ime_composed_from`]), so text a posted key composed
    /// stays that key's when the person's key commits it.
    pub(crate) fn dispatch_ime_event(
        &mut self,
        wid: WindowId,
        ime: aterm_window::event::Ime,
        origin: aterm_window::event::InputOrigin,
    ) {
        use aterm_window::event::{Ime, InputOrigin};
        let composed = if matches!(ime, Ime::Commit(_)) {
            self.ime_composed_from(wid)
        } else {
            InputOrigin::Person
        };
        self.with_origin_mark(origin.max(composed), |app| match ime {
            Ime::Preedit(text, cursor) => app.on_ime_preedit(wid, text, cursor),
            Ime::Commit(text) => app.on_ime_commit(wid, text),
            Ime::Enabled | Ime::Disabled => app.on_ime_enablement_changed(wid),
        });
    }

    /// [`Self::dispatch_hw_key`] under whatever mark it set.
    fn dispatch_key_event(&mut self, wid: WindowId, event: aterm_window::event::KeyEvent) {
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

    /// The story read for a press (`drive::human_acted_on`). The watermark records what the
    /// person saw, so a control client's press reads none: a key an `hwkey` burst posted
    /// ([`Self::dispatch_hw_key`] carries the controller mark) or a `pointer` click (run under
    /// [`Self::with_gesture_source`]).
    pub(crate) fn note_human_acted_on(&mut self, wid: WindowId, session: u64) {
        if self.person_gesture_refused() {
            return;
        }
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

    /// The timer's tick (`drive::tick`).
    pub(crate) fn presence_tick(&mut self, now: Instant) {
        drive::tick(self, now);
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
        let tab_count = ws.tab_set.shown_len();
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
            let mut read = |canonical: usize, tab: &crate::tab_model::Tab| {
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
                        if Some(canonical) != front && self.done_unseen(session) {
                            level = level.max(presence::ChipLevel::Done);
                        }
                    }
                    false
                });
            };
            read(index, tab);
            // The Workers chip lights for the workers it stands for: they have no chip of their own.
            if ws.tab_set.home() == Some(tab.id) {
                let first = ws.tab_set.shown_len();
                for (at, parked) in ws.tab_set.parked().iter().enumerate() {
                    read(first + at, parked);
                }
            }
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

    /// THE LOOK (§14.11 of `docs/DESIGN-harness-autopilot-2026-10-15.md`): window `wid` is the frontmost window, the OS made it its key window (aterm is the active app and this its key window: `WindowState::os_key`, which a controller's `focus in|out` never writes), and it is on screen — or, in a development build, the `person look` stand-in looks at it in place of a headless window. A window left frontmost while the person works in another app looks at nothing: what lands in its front tab then stands until they come back to it.
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
                || self
                    .windows
                    .get(&wid)
                    .is_some_and(|ws| ws.os_key && crate::messages_host::window_on_screen(ws)))
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
                PersonAsked::Click => {
                    let proxy = self.proxy.clone();
                    self.person_menubar_click(&mut |wake| {
                        proxy.as_ref().is_some_and(|p| p.send_event(wake).is_ok())
                    })
                }
            }
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            match asked {
                PersonAsked::Look(_) | PersonAsked::Press(_) | PersonAsked::Click => {
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
        let Some(ws) = self.windows.get(&wid) else {
            return seen;
        };
        let Some(tab) = ws.tab_set.active() else {
            return seen;
        };
        // THE WORKERS LIST ON SCREEN IS A LOOK AT THE ROWS IT SHOWS: each concluded row says how
        // its task ended, a landing too, as its tab would. A program's own turn end is no row's.
        let list = ws.tab_set.home() == Some(tab.id);
        let mut sessions = Vec::new();
        let mut push_view = |view: crate::tab_model::ViewId| {
            if let Some(session) = self
                .view_store
                .get(view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            {
                sessions.push(session);
            }
        };
        if list {
            for parked in ws.tab_set.parked() {
                parked.root.visit(&mut |&view| push_view(view));
            }
        } else if tab.zoomed {
            push_view(tab.focus);
        } else {
            tab.root.visit(&mut |&view| push_view(view));
        }
        for session in sessions {
            let ended = self
                .task_done
                .get(&session)
                .or(self.task_exited.get(&session));
            if let Some(rev) = ended.copied() {
                seen.task |= self.done_seen.insert(session, rev) != Some(rev);
            }
            if list {
                if ended.is_some()
                    && let Some(glass) = self.harness_glass.get_mut(&session)
                {
                    seen.end |= glass.see_end();
                }
                continue;
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
        self.request_redraw_all_windows();
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        self.refresh_tab_chrome_windows(windows);
    }

    /// A TASK'S END MOVED ON THE TAB'S `•` (§11.2.3, items 6 and 7).
    pub(crate) fn restamp_for_end(&mut self) {
        #[cfg(test)]
        crate::work_counts::end_restamped();
        self.mark_status_dirty();
        self.request_redraw_all_windows();
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
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
        let agent = self.session_agent(session);
        let (_, view, supervised, self_proven) =
            self.session_autopilot_for(session, agent, &crate::autopilot::ClaimClock::default());
        if !(supervised
            || agent.is_some_and(|agent| crate::autopilot::carried(agent, supervised, self_proven)))
        {
            return None;
        }
        let glass = self.chip_glass(session);
        driven_words(
            crate::claude_lights::ChipFace::of(view, &glass),
            &glass,
            self.shown_hold(session),
            false,
            self.shown_stop(session),
        )
    }

    pub(crate) fn autopilot_driver(&self, session: u64, words: String) -> crate::overview::Driver {
        if self.beside_proven_self_claim(session) || self.in_self_claim_reexec(session) {
            crate::overview::Driver::CodexAlab(words)
        } else {
            crate::overview::Driver::Aterm(words)
        }
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
        let mut exited = std::collections::HashMap::new();
        for (local, sid, ctx) in handles {
            let name = self.tab_label_of_session(local);
            let task = ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task()
                .map(|t| (t.from.clone(), t.state, t.entered, t.since));
            if let Some((_, state, entered, _)) = &task
                && state.marks_its_tab()
            {
                done.insert(local, *entered);
            } else if let Some((_, state, entered, _)) = &task
                && state.has_an_end_to_read()
            {
                exited.insert(local, *entered);
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
        if teams == self.teams && done == self.task_done && exited == self.task_exited {
            return;
        }
        self.teams = teams;
        self.task_done = done;
        self.task_exited = exited;
        if let Some(front) = self.frontmost_window {
            self.note_done_seen(front);
        }
        self.request_redraw_all_windows();
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
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
            parts.push(self.autopilot_driver(session, words).words());
        }
        (!parts.is_empty()).then(|| parts.join(crate::native_ui::FACT_SEPARATOR))
    }

    /// Hand Back (the Team chip's `✋`).
    pub(crate) fn hand_back_person(&mut self, session: u64) {
        if let Err(why) = self.hand_back_session(session) {
            aterm_log::info!("hand back {session}: {why}");
        }
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

    /// Run `f` with every chrome-row re-grid it causes (`regrid_for_chrome_rows`) recorded, then apply one re-grid from the rows it left.
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

    /// Lift an update handoff's park (`Wake::UpdateHandoffFinished`), and ask every window for a
    /// frame: the lift re-projects every window's presence ([`Self::refresh_presence_all_windows`]),
    /// which asks for no frame itself.
    #[cfg(unix)]
    pub(crate) fn release_update_handoff_park(&mut self, finish: impl FnOnce(&mut Self)) {
        self.with_chrome_regrid_batch(|app| {
            finish(app);
            app.refresh_presence_all_windows();
            app.sync_messages();
            app.request_redraw_all_windows();
        });
    }

    /// Lift a successor's freeze (the Commit wake, once `incoming_handoff_pending` is false), and
    /// ask every window for a frame as [`Self::release_update_handoff_park`] does.
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
            app.request_redraw_all_windows();
        });
    }

    /// Note a rise of the chrome rows every window shares (the tab strip grew) on every window's regrow floor.
    pub(crate) fn note_chrome_rise_everywhere(&mut self, now: Instant) {
        for ws in self.windows.values_mut() {
            ws.regrow_floor.rose(now);
        }
    }

    /// The floor the tab strip's fall waits: the latest regrow floor of the windows whose grid
    /// re-grids with their chrome rows.
    pub(crate) fn chrome_regrow_floor(&self) -> Option<Instant> {
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
        self.task_exited.remove(&session);
        self.worker_end_told.remove(&session);
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

    fn view_of_mut(&mut self, w: WindowId) -> Option<&mut WindowView> {
        self.windows.get_mut(&w).map(|ws| &mut ws.presence)
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

    fn next_need_recheck(&self, now: Instant) -> Option<Instant> {
        self.need_recheck_deadline(now)
    }

    fn projecting(&mut self, w: WindowId) {
        if self.frontmost_window == Some(w) {
            self.publish_front_hold(w);
        }
        self.note_done_seen(w);
    }

    fn restamp_chips(&mut self, windows: Vec<WindowId>) {
        self.refresh_tab_chrome_windows(windows);
    }

    fn publish_and_age_need(&mut self, session: u64) {
        if self.publish_need(session) {
            self.harness_screen_moved();
        }
        self.age_session_need(session);
    }

    fn run_need_rechecks(&mut self, _now: Instant) {
        self.need_rechecks_due();
    }

    fn lapse_supervisor(&mut self, session: u64) {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return;
        };
        let now_us = crate::metrics::us_at(Desk::clock(self));
        if crate::session_timeline::publish_lapse_supervisor(&ctx, now_us).published() {
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
        ask.tail,
        false,
    ));
    lines
}

/// WHO DRIVES, IN THE CHIP'S OWN WORDS (§12.4.1; §12.4.3, item 1). `landed_said`: the row these
/// words end already says `landed` (the Overview's `task landed 17m`), so a landed chip says only
/// `land` there; the tab's hover, which has no task words, passes `false` and keeps `land, landed`
/// (§14.6). `stopped`: the stop of whoever drives it ([`App::shown_stop`]), said first under a face
/// that carries a mode (§14.3: `land: stopped typing; not landed yet, 1 untracked file`, `cruise:
/// stopped typing`), led by its holder's name where a self-claim stopped its own typing
/// ([`crate::harness_glass::ShownStop::words`]), since these words name aterm as the driver
/// (§14.3.2: `land: codex-alab stopped typing`). `hold`: Land's hold of whoever drives it
/// ([`App::shown_hold`]), said under Land's `⤓` once its yield was read, led by its holder's name
/// where a self-claim told it and no stop of that holder's named it first
/// ([`crate::harness_glass::ShownHold::words`], §14.3.3: `land: codex-alab: not landed yet, 1
/// untracked file`).
#[must_use]
pub(crate) fn driven_words(
    face: crate::claude_lights::ChipFace,
    glass: &crate::claude_lights::ChipGlass,
    hold: Option<crate::harness_glass::ShownHold>,
    landed_said: bool,
    stopped: Option<crate::harness_glass::ShownStop>,
) -> Option<String> {
    use crate::claude_lights::ChipFace;
    let stopped = stopped.filter(|_| face.carries());
    let stop = stopped.map(|stop| stop.words(crate::harness_glass::HoldForm::Hover));
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
                hold.words(None, crate::harness_glass::HoldForm::Hover, None, stopped)
            }),
        ),
        ChipFace::Landed if landed_said => "land".to_owned(),
        ChipFace::Landed | ChipFace::Theirs | ChipFace::Failed => {
            format!("land, {}", face.word()?)
        }
        ChipFace::Off | ChipFace::MasterOff | ChipFace::AgentOff(_) | ChipFace::Unhosted => {
            return None;
        }
    };
    Some(words)
}

fn opened_at(started_ms: u64) -> Option<std::time::Instant> {
    let now = std::time::Instant::now();
    let ago = crate::turn_ledger::now_ms()
        .saturating_sub(started_ms)
        .saturating_add(1);
    now.checked_sub(std::time::Duration::from_millis(ago))
}

/// THE GROK SIGN-IN LOOKS, off the event loop: a look ([`grok_signin_now`]) reads a record and a
/// login file and may write the refused mark durably, which a network home that stopped answering
/// holds for as long as it is silent (`docs/DESIGN-hung-network-volumes-2026-10-20.md`). For each
/// session on a Grok sign-in wall it keeps the last answer for its walled program, one look at a
/// time on a thread of its own: a need publication shows the kept answer and reads nothing
/// ([`Self::ask`]), a look whose answer moves what is shown wakes the session's presence (its
/// waker: `Wake::GrokSigninLooked`), and an answer for a program the session no longer runs, or
/// for a session forgotten, lands nowhere.
pub(crate) struct GrokLooks {
    kept: std::sync::Arc<std::sync::Mutex<KeptLooks>>,
    look: std::sync::Arc<GrokLook>,
}

/// One look at the sign-in: [`grok_signin_now`]'s inputs and answer.
type GrokLook = dyn Fn(Option<&std::path::Path>, Option<&std::path::Path>, i32) -> crate::needs::GrokSignin
    + Send
    + Sync;

impl Default for GrokLooks {
    fn default() -> Self {
        Self {
            kept: std::sync::Arc::default(),
            look: std::sync::Arc::new(grok_signin_now),
        }
    }
}

#[derive(Default)]
struct KeptLooks {
    /// The last entry's epoch: a look lands only in the entry it was started for.
    next: u64,
    by_session: std::collections::HashMap<u64, KeptLook>,
    /// How many looks have ended and been taken in or dropped, each counted after its wake.
    #[cfg(test)]
    settled: u64,
}

struct KeptLook {
    epoch: u64,
    /// The walled program's group leader the answer is for.
    pgid: i32,
    /// The last answer for it; none until its first look ends.
    answer: Option<crate::needs::GrokSignin>,
    /// A look is out.
    out: bool,
    /// Asked while a look was out: it looks once more as it ends.
    again: bool,
}

impl GrokLooks {
    /// Looks made by `look` instead of [`grok_signin_now`].
    #[cfg(all(test, unix))]
    pub(crate) fn with_look(
        look: impl Fn(
            Option<&std::path::Path>,
            Option<&std::path::Path>,
            i32,
        ) -> crate::needs::GrokSignin
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            kept: std::sync::Arc::default(),
            look: std::sync::Arc::new(look),
        }
    }

    /// How many looks have ended and been taken in or dropped.
    #[cfg(all(test, unix))]
    pub(crate) fn settled(&self) -> u64 {
        self.kept.lock().unwrap_or_else(|p| p.into_inner()).settled
    }

    /// What session `session`'s wall shows for its walled program `pgid`: the kept answer, or
    /// [`crate::needs::GrokSignin::Reading`] until the first look for that program ends. A look
    /// starts (or the one out owes another), and `wake` runs where its answer moves what is shown.
    pub(crate) fn ask(
        &self,
        session: u64,
        pgid: i32,
        wake: impl Fn() + Send + 'static,
    ) -> crate::needs::GrokSignin {
        use crate::needs::GrokSignin;
        let (epoch, shown) = {
            let mut kept = self.kept.lock().unwrap_or_else(|p| p.into_inner());
            let KeptLooks {
                next, by_session, ..
            } = &mut *kept;
            let entry = by_session.entry(session).or_insert_with(|| {
                *next += 1;
                KeptLook {
                    epoch: *next,
                    pgid,
                    answer: None,
                    out: false,
                    again: false,
                }
            });
            if entry.pgid != pgid {
                entry.pgid = pgid;
                entry.answer = None;
            }
            let shown = entry.answer.unwrap_or(GrokSignin::Reading);
            if entry.out {
                entry.again = true;
                return shown;
            }
            entry.out = true;
            (entry.epoch, shown)
        };
        let (kept, look) = (
            std::sync::Arc::clone(&self.kept),
            std::sync::Arc::clone(&self.look),
        );
        // Named on the asking thread: a test's harness state is that thread's.
        let state = crate::harness_host::harness_state();
        let home = aterm_types::dirs::home_dir();
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-grok-signin-look",
            crate::qos::Role::Background,
            move || {
                let mut pgid = pgid;
                loop {
                    let answer = look(state.as_deref(), home.as_deref(), pgid);
                    let (moved, again) = {
                        let mut map = kept.lock().unwrap_or_else(|p| p.into_inner());
                        match map
                            .by_session
                            .get_mut(&session)
                            .filter(|entry| entry.epoch == epoch)
                        {
                            None => (false, false),
                            Some(entry) => {
                                let current = entry.pgid == pgid;
                                let moved = current && entry.answer != Some(answer);
                                if current {
                                    entry.answer = Some(answer);
                                }
                                let again = std::mem::take(&mut entry.again) || !current;
                                pgid = entry.pgid;
                                entry.out = again;
                                (moved, again)
                            }
                        }
                    };
                    if moved {
                        wake();
                    }
                    #[cfg(test)]
                    {
                        kept.lock().unwrap_or_else(|p| p.into_inner()).settled += 1;
                    }
                    if !again {
                        return;
                    }
                }
            },
        );
        if let Err(error) = spawned {
            aterm_log::warn!("grok sign-in look for session {session}: no thread ({error})");
            let mut kept = self.kept.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = kept.by_session.get_mut(&session) {
                entry.out = false;
                entry.again = false;
            }
        }
        shown
    }

    /// Session `session` is on no Grok sign-in wall (or gone): its answer goes, and a look still
    /// out for it lands nowhere.
    pub(crate) fn forget(&self, session: u64) {
        self.kept
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .by_session
            .remove(&session);
    }
}

/// Where this machine's one Grok Build sign-in stands for a Grok session on its sign-in wall
/// whose program group leader is `pgid` ([`crate::needs::GrokSignin`]), read from the harness
/// state `state` and the home `home` (blocking file I/O: a look's thread only, [`GrokLooks`]): a
/// sign-in runs (its leader's record under the harness state), else a login the walled grok may
/// not have read (written after it started) says restart it, else signed out. A login written
/// before that grok started was read and refused by it: it is marked refused
/// ([`aterm_agent::harness::grok_signin::mark_refused`]), so the next launch is signed in through
/// the one sandboxed sign-in instead of a grok that opens its own page.
fn grok_signin_now(
    state: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
    pgid: i32,
) -> crate::needs::GrokSignin {
    use aterm_agent::harness::grok_signin::{self, WallLogin};
    if state.is_some_and(grok_signin::running) {
        return crate::needs::GrokSignin::Running;
    }
    let started = (pgid > 0)
        .then(|| aterm_uds::process::read_process_birth(pgid))
        .flatten()
        .map(|b| {
            std::time::UNIX_EPOCH
                + std::time::Duration::from_secs(b.seconds)
                + std::time::Duration::from_micros(b.microseconds)
        });
    match grok_signin::wall_login(&|name| std::env::var_os(name), home, started) {
        WallLogin::SignedIn(_) => crate::needs::GrokSignin::SignedIn,
        WallLogin::Refused(file) => {
            let _ = grok_signin::mark_refused(&file);
            crate::needs::GrokSignin::Idle
        }
        WallLogin::SignedOut => crate::needs::GrokSignin::Idle,
    }
}

#[cfg(test)]
#[path = "app_presence_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "person_stand_in_tests.rs"]
mod person_stand_in_tests;

#[cfg(test)]
#[path = "look_needs_focus_tests.rs"]
mod look_needs_focus_tests;

#[cfg(all(test, unix))]
#[path = "grok_signin_look_tests.rs"]
mod grok_signin_look_tests;

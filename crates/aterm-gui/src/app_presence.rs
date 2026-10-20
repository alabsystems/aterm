// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The App's side of presence ([`crate::presence`]): publishing each
//! session's NEED VERDICT (`docs/DESIGN-owner-surfaces-2026-10-16.md` §2.3)
//! and hosting the engine driver that draws it — the window's NEEDS BORDER,
//! its titlebar PILL and the TAB MARKS (§5). The sequencing (fold the facts
//! into the per-session slot, project each window's most pressing session
//! onto its view, the story read, told stories, deadline and tick) is
//! `aterm_messages::presence::drive`; this file implements its [`Desk`] on
//! the App: the window walk, the facts (the published verdict among them),
//! the on-screen test, the tab label, the restart's offer, repaint, chips,
//! herald.
//!
//! PRESENCE NEVER RE-GRIDS A PANE: it owns no chrome row (the presence band
//! and its fold law are gone, §7). The border is an overlay the frame path
//! reads with no lock and no allocation ([`App::presence_overlay`],
//! [`App::presence_fp`]); the pill is pushed with the strip
//! ([`App::presence_pill`]); a click on it is [`App::on_pill_act`].
//!
//! Every entry point runs at change rate: lease, fabric, meta and task wakes,
//! a status-revision or agent-verdict move from the sweep, a tab switch, the
//! human's keystroke (the story read), and the pill's age while one is seen.

use std::time::{Duration, Instant};

use aterm_messages::presence::drive::{self, Desk, Shown};
use aterm_session::SessionId;

use crate::app_render::OverlayGlow;
use crate::presence::{self, Facts, Hand, HoldFact, Link, StoryVerb, TurnFact, WindowView};
use crate::{App, WindowId};

/// The chip level the indicator bits spell
/// (`aterm_messages::presence::chip_of_attention`).
pub(crate) use aterm_messages::presence::chip_of_attention;

/// The per-session presence state: the engine's table (the slots and each
/// `ttl=` supervisor claim's lapse, `drive::Table`) and this host's herald.
#[derive(Default, Debug)]
pub(crate) struct PresenceTable {
    table: drive::Table<presence::Native>,
    /// The menu-bar rows' and notifications' transition memory
    /// (`App::herald_session`), per session like the slots.
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

/// The chrome-row re-grid a [`App::with_chrome_regrid_batch`] recorded, to
/// apply at its end.
#[derive(Debug, Default)]
pub(crate) enum ChromeRegridBatch {
    /// None yet.
    #[default]
    Idle,
    /// Every window (`regrid_for_chrome_rows`).
    All,
}

impl ChromeRegridBatch {
    pub(crate) fn all(&mut self) {
        *self = Self::All;
    }

    /// Whether a re-grid of `wid` is recorded (every window's, or none).
    fn covers(&self, _wid: WindowId) -> bool {
        matches!(self, Self::All)
    }
}

#[cfg(test)]
thread_local! {
    static CHROME_REGRIDS_APPLIED: std::cell::RefCell<Vec<(WindowId, u16)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Log an applied chrome re-grid of `wid` with its chrome rows (test-only; a
/// headless window has no OS size to re-grid from).
#[cfg(test)]
pub(crate) fn note_chrome_regrid_applied(app: &App, wid: WindowId) {
    let rows = app.chrome_rows(wid);
    CHROME_REGRIDS_APPLIED.with(|log| log.borrow_mut().push((wid, rows)));
}

/// Take this thread's applied chrome re-grids, in order: `(window, chrome
/// rows)`.
#[cfg(test)]
pub(crate) fn take_chrome_regrids_applied() -> Vec<(WindowId, u16)> {
    CHROME_REGRIDS_APPLIED.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

#[cfg(test)]
thread_local! {
    /// A test's presence clock: [`Desk::clock`] answers it while set, so a rig
    /// steps the driver's clock. `None`: the wall clock.
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
    /// A test's wall clock for the need publication (unix seconds): a rig
    /// moves it without moving the presence clock, as a Mac's sleep moves
    /// the wall clock and not `Instant`. `None`: the wall clock.
    static NEED_UNIX_CLOCK_FOR_TEST: std::cell::Cell<Option<i64>> =
        const { std::cell::Cell::new(None) };
}

/// Set (or, with `None`, clear) this thread's need wall clock for a test.
#[cfg(test)]
pub(crate) fn set_need_unix_clock_for_test(now: Option<i64>) {
    NEED_UNIX_CLOCK_FOR_TEST.with(|c| c.set(now));
}

/// Now on the wall clock, unix seconds: what a need recheck is kept in
/// (`App::need_rechecks`). A Mac's sleep does not advance `Instant`, and a
/// limit's reset is a time of day.
fn need_unix_now() -> i64 {
    #[cfg(test)]
    if let Some(now) = NEED_UNIX_CLOCK_FOR_TEST.with(std::cell::Cell::get) {
        return now;
    }
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0))
}

/// Whether window `ws`'s grid re-grids when its chrome rows move: a real OS
/// window, or a test's stand-in for one (a headless window keeps its
/// inserted grid; its chrome caches are the whole re-grid). Only such a
/// window has a chrome regrow floor, since a grid that never moves for its
/// chrome cannot flap. It is also what "glass" means to the band's afford law
/// (`App::band_commit`).
pub(crate) fn chrome_regrids_grid(ws: &crate::WindowState) -> bool {
    #[cfg(test)]
    if ws.chrome_regrids_for_test {
        return true;
    }
    ws.os_window.is_some()
}

/// What the macOS `NeedsPill` view (and the `chrome` reader) is handed with
/// the strip: the pill's words as of the push, what lit it, and the session
/// it names — which a click carries back, so a click on a pill that has
/// since moved acts on nothing ([`App::on_pill_act`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NeedsPillModel {
    /// What lit it (the tone the view paints).
    pub(crate) kind: presence::PillKind,
    /// The words, in the pieces the view lays out and clips.
    pub(crate) words: presence::PillWords,
    /// The session it names.
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

    /// Gather one session's facts: leaf locks only, each held for a copy; the
    /// agent reading is the status sweep's published one, and the NEED is the
    /// verdict [`Self::publish_need`] has just published on the session's
    /// timeline (`Desk::herald` runs before `Desk::sense`): the border, the
    /// pill and the mark draw it, never a classification of their own.
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
                    // The lapse posts no wake: it is a deadline instead.
                    Some(now + Duration::from_micros(expires_us - now_us)),
                ),
                _ => (Hand::None, None),
            }
        };
        // Driving another session: an open turn on a peer whose lease names
        // this session as its driver.
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
        // The link as this App sees it (`fabric_state_seen`: the process link, keyed away
        // from a foreign test section in a test binary).
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
        // What a `codex` in front reports about itself (OSC 7501): the row's
        // word where no agent reading names a phase. Display only.
        let program = self
            .session_status
            .program_state(session)
            .map(|(state, since)| (crate::program_state::row_word(state), since));
        // The sweep's published verdict (only for an identified agent, only
        // when its live zone changed); presence never re-reads the screen.
        let (agent_seq, agent) = self.session_status.agent_reading(session);
        // The loop's next try on an API wall (`with_retry_plan`): the hover's
        // state line reads the plan beside the wall's words.
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
            // The plan's and the placement's changes count as news to the engine (see
            // `App::harness_wait_epoch`).
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
            // In front of a window the person has focused: a turn end there happens in
            // plain sight, and is no news for the story.
            program_turn_watched: self
                .windows
                .iter()
                .any(|(wid, ws)| ws.focused && self.focused_session_id(*wid) == Some(session)),
        })
    }

    /// Who answers the agent's wall ([`presence::WallAnswer`]): one this
    /// aterm's harness resumes by itself (it holds the session's claim and
    /// admits its agent) is a record, not a stop; so is a usage wall of a
    /// PROVEN self-claim's own program whose policy resumes it
    /// (`crate::self_claim`); one nobody answers (no claim, another holder's,
    /// the owner's switch off, a login, the memory banner) stays a stop. Read
    /// on every refresh, so a claim arriving or lapsing moves it without a new
    /// reading. Leaf locks, one at a time.
    fn wall_answer_of(
        &self,
        session: u64,
        ctx: &crate::SessionCtx,
        phase: Option<&presence::AgentPhase>,
        now_us: u64,
    ) -> presence::WallAnswer {
        // Whether the wall is a proven self-claim's own (the second arm below): the
        // program in front is a Codex, the reading was made under one, and the group
        // in front is the claimant's own (its leader is the proven pid). Read before
        // the match, under the timeline's leaf lock alone: the arms take the meta lock
        // in their guards, and the two are never held together.
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
                // Only Claude's lost login is carried by this harness. Other
                // agents' Auth walls still need their own sign-in or key.
                if program.is_some_and(|agent| {
                    host.admits(agent)
                        && (*kind != aterm_phase::WallKind::Auth
                            || aterm_agent::supervise::policy::login::carried(agent))
                }) {
                    host.wall_answer(*kind, reset.is_some())
                } else {
                    presence::WallAnswer::Nobody
                }
            }
            // A PROVEN self-claim (codex-alab supervising itself, `crate::self_claim`)
            // resumes a usage wall by itself when the policy it reads (`harness policy`)
            // is in effect with `resume_limits` on: the same record, not a stop. Only a
            // wall OF the claimant: a Codex in front (`self_claim::fronts_claimant`),
            // read under one, in the claimant's own group (its proven pid leads it), as
            // the arm above asks `host.admits` of the agent in front. Another agent's
            // wall under the claim (a reading made under Claude Code that the sweep has
            // not replaced, a stock codex started beside the claimant) is nobody's.
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

    /// ONE SESSION'S NEED FACTS ([`crate::needs::NeedFacts`],
    /// `docs/DESIGN-owner-surfaces-2026-10-16.md` §2.3), under the leaf locks
    /// the status glance already takes, each released before the next: the
    /// meta's (the attention entries with their kinds, the live claim), the
    /// timeline's (the agent publication, the stall's words, the task record,
    /// the resume line), the fabric's (the holds); the sweep's reading, the
    /// host's word and `hosting` outside any lock. Called with NO store lock
    /// held ([`Self::publish_need`] copies the handle out first): the host's
    /// wall answer takes the host's state lock, which is never nested under
    /// the registry's (`harness_reach`'s order). No `Terminal` lock.
    ///
    /// SUPERVISED is the host's answer for the session (`hosting`, read once
    /// per refresh before the store's lock) or another holder's live claim:
    /// this host's old claim can still be winding down after its agent was
    /// switched off, and promises no answer then. The memory row's restart
    /// is the host's own word, never supervision alone (resume-hint review,
    /// 2026-09-26).
    pub(crate) fn need_facts(
        &self,
        h: &crate::session_store::SessionHandle,
        hosting: &crate::harness_reach::HarnessHosting,
        now_us: u64,
        now_unix: i64,
    ) -> crate::needs::NeedFacts {
        // Read before the timeline's leaf lock.
        let word = crate::autopilot::word_of(&h.ctx);
        self.need_facts_with(
            h,
            |agent| hosting.hosts(h.sid.as_str(), word, agent),
            &hosting.relaunching,
            now_us,
            now_unix,
        )
    }

    /// [`Self::need_facts`] with the host's answer for the session's agent
    /// (`hosted`, asked after the timeline's leaf lock is released) and the
    /// sessions the host would relaunch passed in: the seam a test stands a
    /// host up by.
    pub(crate) fn need_facts_with(
        &self,
        h: &crate::session_store::SessionHandle,
        hosted: impl FnOnce(aterm_phase::Program) -> bool,
        relaunching: &std::collections::HashSet<String>,
        now_us: u64,
        now_unix: i64,
    ) -> crate::needs::NeedFacts {
        let ctx = &h.ctx;
        let (attention, supervisor) = {
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let mut attention = meta.attention_owners.kinded();
            // A meta seeded as a value (a restore) holds the bare owner's ask.
            if attention.is_empty()
                && let Some(text) = meta.attention.clone()
            {
                attention.push(crate::needs::AttentionFact {
                    owner: crate::session_timeline::BARE_ATTENTION_OWNER.to_string(),
                    text,
                    kind: Some(aterm_types::needs::AttentionKind::Ask),
                });
            }
            (attention, meta.live_supervisor(now_us).map(str::to_string))
        };
        let restarts = relaunching.contains(h.ctx.self_id.as_str())
            && crate::input_stall::host_hand_refusal(ctx, now_us).is_none();
        let (agent, admit, stall, task, resume, age_ms) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let a = tl.agent();
            let agent = (a.word != "-").then(|| crate::needs::AgentFact {
                word: a.word,
                detail: a.detail.clone(),
                rev: a.rev,
                subject: a.subject.clone(),
            });
            // The stall's words name the publication's explicit restart
            // (`InputStallFact::act`), as its attention entry does.
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
            (agent, admit, stall, task, resume, a.age_ms())
        };
        // The host's hand as it admits the agent ([`crate::harness_host::admitted`]:
        // never a vendor `codex`), read after the timeline's leaf lock.
        let host_supervises =
            crate::harness_host::admitted_as(ctx, admit.0, admit.1).is_some_and(hosted);
        // Whose hold stands, the person's Take Over first (`Holds::word`).
        let holds = ctx.fabric.holds();
        let hold = holds.word();
        // The stall in the server attention's own words — composed after the
        // timeline's leaf lock is released (it reads the clock and the zone) —
        // and whether the pill carries its restart now, by the one predicate
        // the pill's Restart is offered by (`restart_act::offered`), or would
        // once Hand Back lifts the one hold that refuses it.
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
        // A wall's reset placed as of when its notice was read: a span counts
        // from then, never from each re-read — and through any sleep since
        // (`age_ms`), as the wall clock it is placed on does.
        let printed = now_unix.saturating_sub(i64::try_from(age_ms / 1000).unwrap_or(0));
        let reset_unix = agent
            .as_ref()
            .filter(|a| a.word.starts_with("wall:"))
            .and_then(|a| a.detail.as_deref())
            .and_then(|reset| presence::place_reset(reset, printed));
        let other_supervisor = supervisor
            .as_deref()
            .is_some_and(|holder| holder != crate::harness_host::holder());
        crate::needs::NeedFacts {
            stall,
            task,
            attention,
            agent,
            supervised: host_supervises || other_supervisor,
            other_supervisor,
            claim_live: supervisor.is_some(),
            wall_answer,
            restarts: restarts && host_supervises,
            resume,
            hold,
            reset_unix,
            now_unix,
        }
    }

    /// THE NEED PUBLICATION (§2.3): classify session `session`
    /// ([`crate::needs::of`] over [`Self::need_facts`]) and publish the
    /// verdict on its timeline, where every reader reads it. An exited
    /// session needs nothing. Arms the presence timer at an unsupervised
    /// limit's reset (`need_rechecks`), past which the verdict moves with no
    /// fact moving. Returns whether the verdict moved.
    pub(crate) fn publish_need(&mut self, session: u64) -> bool {
        // What one publication costs (`metrics percentiles`' `need_publish_*`).
        let started = Instant::now();
        let moved = self.publish_need_timed(session);
        crate::metrics::note_need_publish(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        moved
    }

    /// [`Self::publish_need`]'s body.
    fn publish_need_timed(&mut self, session: u64) -> bool {
        let hosting = self.harness_hosting();
        let now_us = crate::metrics::now_us();
        let now_unix = need_unix_now();
        // The handle copied out under the store's guard, which is dropped
        // before the facts are read: they take the host's state lock.
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
        // A hold set or lifted over the wire (a client's, which the long
        // hold's entry names too) moves no person's act here: its walk is
        // owed where the published hold moved.
        if tl.need().held != held {
            self.agents.long_holds_moved = true;
        }
        tl.set_need(verdict)
    }

    /// A session that is gone (closed, or no longer in the registry) needs
    /// nothing and waits on nothing: its recheck is dropped and it leaves the
    /// API outage's count, which is stated again where that moved.
    fn forget_need(&mut self, session: u64, now_unix: i64) {
        self.need_rechecks.remove(&session);
        self.note_api_outage(session, false, now_unix);
    }

    /// Session `session`'s verdict waits on an unreachable API (`waits`) or
    /// no longer does: where that moved, the digest's one `ApiOutage` entry
    /// is stated again ([`Self::hand_api_outage`]).
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

    /// THE API OUTAGE'S WHOLE ANSWER (`Family::ApiOutage`, a level family):
    /// one entry counting the tabs that cannot reach their API, since the
    /// first of them; none when no tab waits on it, which sweeps the entry.
    /// Stated as tabs enter and leave the wait (a closed tab leaves it:
    /// [`Self::retire_presence`]), and once per process however quiet it is
    /// (`App::ripen_exceptions`), so an entry an earlier process left open
    /// ends when this one sees no outage.
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

    /// Every session's need published again (and its herald and chrome
    /// refreshed): where an input every verdict reads moved at once — the
    /// harness policy in force, which decides whose boxes the host answers.
    pub(crate) fn publish_needs_all(&mut self) {
        let sessions: Vec<u64> = self.pool.iter().map(|session| session.id).collect();
        for session in sessions {
            self.refresh_presence_session(session);
        }
    }

    /// The earliest need recheck (`need_rechecks`), for the presence timer,
    /// placed on `now`'s clock from the wall clock each time it is asked: a
    /// Mac that slept through a reset finds it due at the first turn after
    /// it wakes, never a sleep's length late. The event loop folds it again
    /// after its status sweep, whose publications can store one.
    pub(crate) fn need_recheck_deadline(&self, now: Instant) -> Option<Instant> {
        let at = *self.need_rechecks.values().min()?;
        let wait = u64::try_from(at.saturating_sub(need_unix_now())).unwrap_or(0);
        Some(now + Duration::from_secs(wait))
    }

    /// Re-publish every session whose need recheck has come (on the wall
    /// clock).
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

    /// The name the band gives a driving session: its `meta role` when local
    /// and set, else its short sid. Who drives is the lease's record
    /// ([`crate::Lease::Turn::driver`]), never inferred from the edge table: a
    /// standing write edge is authority, not a hand, and an Owner-token turn
    /// has no driver.
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

    /// The session `src` is driving now: one whose open turn lease names `src`
    /// as its driver. Only a lease counts, not a standing edge.
    ///
    /// Each peer's `(local id, ctx)` is taken under the store guard, dropped
    /// before any lease lock; peers are walked in local-id order.
    /// `ctx.self_id` is the handle's sid (`register_session`).
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

    /// A fact about `session` changed: publish its need, re-read it, fold it
    /// in, and project every window that shows the session
    /// (`drive::refresh_session`).
    pub(crate) fn refresh_presence_session(&mut self, session: u64) {
        drive::refresh_session(self, session);
    }

    /// The harness loop's next try for `sid` ([`crate::Wake::HarnessWait`]):
    /// stored, and the tab's facts re-read where it changed.
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

    /// What the harness loop of `sid` told the window
    /// ([`crate::Wake::HarnessFact`], `docs/DESIGN-harness-autopilot-2026-10-15.md`
    /// §9.2.3, §9.4.4): an ACT goes to the harness's glass
    /// ([`crate::harness_glass::on_act`]); the TASK'S FACE goes on the
    /// session's timeline, where `status task=` reads it under the lock it
    /// already takes, and into the glass the frame reads with none — and,
    /// where it moved, the session's footer is posted and the move goes past
    /// the glass (`App::harness_task_moved`: the page, and a record of a
    /// landing's end). A fact for a session this window does not hold is
    /// dropped. A face told again is no move: no frame.
    pub(crate) fn apply_harness_fact(&mut self, sid: &str, fact: crate::HarnessFact) {
        let Some(session) = self.local_session_of(&SessionId::new(sid)) else {
            return;
        };
        match fact {
            crate::HarnessFact::Act(act) => crate::harness_glass::on_act(self, session, act),
            crate::HarnessFact::Task(face) => self.apply_task_face(session, face, None),
            crate::HarnessFact::TaskNoted(face, noted_ms) => {
                self.apply_task_face(session, Some(face), Some(noted_ms));
            }
            // THE LOOP'S PAUSE AFTER SHORT TURNS (§10.5.3, item 4): on the
            // timeline (`status pause=`, under the lock it already takes)
            // and the glass (the title's and the page's words). No face
            // moved, so no footer is posted: a frame at rest keys nothing
            // new. A title someone is reading is built again — the glass's
            // trail generation moved — so the windows reading this chip's
            // title are redrawn, the A/B book's cause noted, and the page
            // republished where shown.
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
                    // The A/B book's cause moved: the prompt's time from
                    // here is the harness's pause, or no longer.
                    self.note_harness_arm(session);
                } else if !moved {
                    return;
                }
                self.harness_screen_moved();
            }
        }
    }

    /// THE TASK'S FACE of session `session` told by its loop
    /// ([`Self::apply_harness_fact`]): `face` (`None`: unknown), and
    /// `noted_ms` where the loop's start restated it from the journal
    /// (`HarnessFact::TaskNoted`: the wall time, unix ms, its end was
    /// noted). On the timeline and in the glass; where it moved, the footer
    /// posted and the move taken past the glass.
    fn apply_task_face(
        &mut self,
        session: u64,
        face: Option<aterm_agent::harness::acts::TaskFace>,
        noted_ms: Option<i64>,
    ) {
        let Some(entry) = self.pool.get(session) else {
            return;
        };
        // The wall clock, unix ms, as the timeline keeps it and the glass
        // formats the face's clock time from it (`landed 17:42`) — never the
        // turn ledger's process-relative ms. A face restated from the
        // journal takes its note's time instead (never one past now): a
        // window that held no face of the session — a seamless update's new
        // one — shows when the work ended, not when the loop came back
        // (§10.2.7).
        let now_ms = crate::harness_screen::unix_ms_now();
        let stamp_ms = noted_ms.map_or(now_ms, |noted| noted.min(now_ms));
        // The face it moved from, and the time the face now held moved
        // there: a restated face (the same face told after "unknown") keeps
        // the first time this window held.
        let (was, at_ms) = {
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let was = timeline.task_face().map(|(f, _)| f);
            if !timeline.set_task_face(face, stamp_ms) {
                return;
            }
            (was, timeline.task_face().map_or(stamp_ms, |(_, at)| at))
        };
        if crate::harness_glass::on_task(self, session, face, at_ms) {
            crate::claude_footer::post_changed(session);
        }
        // The A/B book's cause may have moved with the face (done or landed
        // is "finished" at the prompt, §10.5.3 B.3).
        self.note_harness_arm(session);
        // Past the glass: the page republished, and a landing that landed
        // or was spent under Land recorded (§9.4.4).
        self.harness_task_moved(session, was, face);
    }

    /// The wake arms: resolve the fabric sid to the pool's local id.
    pub(crate) fn on_presence_wake(&mut self, sid: &SessionId) {
        if let Some(session) = self.local_session_of(sid) {
            self.refresh_presence_session(session);
        }
    }

    /// The `Wake::FabricChanged` arm: session `sid`'s fabric endpoint moved —
    /// a delivery, a post, `inbox seen`, a link report, or a HOLD on or off
    /// (`hold <sid> on|off`, Take Over, the fleet's). The border, pill and
    /// marks move ([`Self::refresh_presence_session`]), and a hold is a
    /// standing word about typing, so the crash journal carries it at once
    /// ([`Self::journal_owner_words`]; anything else owes it nothing). A
    /// held tab a cold restore's drain still holds the agent of, lifted, lets
    /// its agent go and says it ([`Self::let_go_lifted_held`]).
    pub(crate) fn on_fabric_changed(&mut self, sid: &SessionId) {
        let Some(session) = self.local_session_of(sid) else {
            return;
        };
        self.refresh_presence_session(session);
        self.let_go_lifted_held(session);
        self.journal_owner_words(session);
    }

    /// Project the window's most pressing session onto its view
    /// (`drive::refresh_window`).
    pub(crate) fn refresh_presence_window(&mut self, wid: WindowId) {
        drive::refresh_window(self, wid);
    }

    /// Every window: a restore, a successor's commit, a handoff's release and
    /// a config adopt (`drive::refresh_all`).
    pub(crate) fn refresh_presence_all_windows(&mut self) {
        // The funnel a restore and a successor's commit pass through: the
        // team table and the done revs are read again with every window.
        self.rebuild_teams();
        drive::refresh_all(self);
    }

    /// The story read (`drive::human_acted`): the human acted in `wid`; a calm
    /// session's story is read. The shipping key arm goes through
    /// [`Self::dispatch_hw_key`]; tests act through this.
    #[cfg(test)]
    pub(crate) fn note_human_acted(&mut self, wid: WindowId) {
        drive::human_acted(self, wid);
    }

    /// One key from the winit `KeyboardInput` arm: the echo-probe bracket
    /// around [`Self::on_key`], and the story read. The read follows
    /// `on_key`, since only it decides whether a key moves the front: the
    /// session front before the key is read only if the key left it front.
    pub(crate) fn dispatch_hw_key(&mut self, wid: WindowId, event: aterm_window::event::KeyEvent) {
        let pressed = event.state == aterm_window::event::ElementState::Pressed;
        let front = self.focused_session_id(wid);
        // Press-only, and never on focus alone.
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

    /// The story read for a press (`drive::human_acted_on`): the human acted
    /// in `wid` while `session` was front.
    pub(crate) fn note_human_acted_on(&mut self, wid: WindowId, session: u64) {
        drive::human_acted_on(self, wid, session);
    }

    /// Re-read every pooled session's facts at a successor's Commit: a lease
    /// a seamless update carried was seeded before its session was
    /// registered, so no `meta set` wake armed its lapse; without this a dead
    /// holder's claim would never lapse and this instance's supervisor host
    /// would never take the session.
    #[cfg(unix)]
    pub(crate) fn arm_supervisor_expiries(&mut self) {
        let sessions: Vec<u64> = self.pool.iter().map(|session| session.id).collect();
        for session in sessions {
            self.refresh_presence_session(session);
        }
    }

    /// The one instant the presence timer arms (`drive::deadline`): `None`
    /// on a quiet desktop.
    pub(crate) fn presence_deadline(&self, now: Instant) -> Option<Instant> {
        drive::deadline(self, now)
    }

    /// The timer's tick (`drive::tick`): the windows that need a repaint.
    pub(crate) fn presence_tick(&mut self, now: Instant) -> Vec<WindowId> {
        drive::tick(self, now)
    }

    /// The border as an [`OverlayGlow`] for the frame path — plain field reads
    /// and arithmetic, no lock, no allocation; `None` on a quiet window.
    pub(crate) fn presence_overlay(&self, wid: WindowId) -> Option<OverlayGlow> {
        let ws = self.windows.get(&wid)?;
        // `View::rim_glow`: the quiet frame answers before the tones are
        // derived, so a border-less window pays only the match.
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

    /// The needs pill window `wid` shows now, as the strip carries it
    /// ([`NeedsPillModel`]): `None` while its border is unlit or the View
    /// switch is off.
    pub(crate) fn presence_pill(&self, wid: WindowId) -> Option<NeedsPillModel> {
        let pill = self.windows.get(&wid)?.presence.pill.as_ref()?;
        Some(NeedsPillModel {
            kind: pill.kind,
            words: presence::pill_words(pill, Desk::clock(self)),
            session: pill.session,
        })
    }

    /// Whether window `wid`'s IN-GRID strip carries its needs pill: every
    /// window but a macOS one on glass, whose titlebar strip draws it
    /// (`toolbar.rs`'s `NeedsPill`) — so a pill is drawn once, by whichever
    /// strip the window has.
    pub(crate) fn strip_carries_pill(&self, wid: WindowId) -> bool {
        !(cfg!(target_os = "macos")
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.os_window.is_some()))
    }

    /// The in-grid strip's cache term for window `wid`'s pill: the view's
    /// seed (bumped on every change the painter can see) and, for a pill
    /// that prints an age (a background tab's), when that figure next moves
    /// (`aterm_messages::presence::pill_due`) — plain reads and arithmetic,
    /// no words composed. `None` with no pill.
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

    /// Stamp each tab's mark onto the strip metadata: the most pressing of
    /// its terminal leaves' marks — the published need's (`◆` stuck, `◇`
    /// decide), the person's hold (`✋`), a finished task whose done rev, or
    /// a program's own turn end, was not yet looked at (`•`, never on the
    /// front tab: [`Self::done_unseen`]) — folded over the tab's own
    /// indicator bit. A supervised box makes no mark: it is no need.
    pub(crate) fn stamp_presence_chips(
        &self,
        wid: WindowId,
        metadata: &mut [crate::tab_bar::TabStripMetadata],
    ) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let front = ws.tab_set.active_index();
        for (index, (tab, item)) in ws
            .tab_set
            .tabs()
            .iter()
            .zip(metadata.iter_mut())
            .enumerate()
        {
            let mut level = item.attention;
            tab.root.any_leaf(&mut |view| {
                if let Some(session) = self
                    .view_store
                    .get(*view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                {
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
            // The tab's team marks: its focused session's (`crate::team`).
            item.team = self
                .view_store
                .get(tab.focus)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                .and_then(|session| self.teams.get(&session))
                .map_or(crate::tab_bar::TabTeam::NONE, crate::team::Team::strip);
        }
    }

    /// The pill's a11y message: the window's most pressing need as its pill
    /// says it, a polite status with no bar row (the pill is titlebar
    /// chrome); `None` with no pill. The spoken sentence carries no age, so
    /// it changes only with the need, its label or its act and a live region
    /// never floods; the aged sentence rides the description.
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

    /// Whether something in `session` finished that was not yet looked at:
    /// its task, whose done rev is not in [`Self::done_seen`], or its
    /// program, which reported its own turn end where nobody watched (a
    /// background codex: codex-alab keeps OSC 9 inert and desktop notices
    /// are off by default, so the tab is where the end shows), beyond the
    /// count [`Self::turn_end_seen`] holds. The tab's `•`. Reads the done
    /// revs the last team build took ([`Self::rebuild_teams`]) and the
    /// presence slot, so a strip stamp on the frame path takes no lock.
    fn done_unseen(&self, session: u64) -> bool {
        self.task_done
            .get(&session)
            .is_some_and(|rev| self.done_seen.get(&session) != Some(rev))
            || self.turn_ends_unseen(session).is_some()
    }

    /// `session`'s program turn ends that were news (the slot's
    /// `turn_end_news`), where more stand than were looked at
    /// ([`Self::turn_end_seen`]): the count to mark seen.
    fn turn_ends_unseen(&self, session: u64) -> Option<u64> {
        let news = self.presence.slot(session)?.turn_end_news;
        (news > self.turn_end_seen.get(&session).copied().unwrap_or(0)).then_some(news)
    }

    /// Whether someone can look at window `wid` now: the frontmost window,
    /// on screen (not occluded or minimized; a headless window never is).
    fn window_looked_at(&self, wid: WindowId) -> bool {
        self.frontmost_window == Some(wid) && Desk::pill_visible(self, wid)
    }

    /// `wid`'s front tab is looked at — where someone can (§3: the owner
    /// focuses or looks at the tab), never in a window behind, minimized,
    /// occluded or headless: a finished task's done rev in any pane it shows
    /// is seen ([`Self::done_unseen`]), the focused one's and a split's
    /// others alike (a zoomed tab shows its focused pane alone). Run where
    /// that can begin: a projection, a task that finished in front
    /// ([`Self::rebuild_teams`]), the window's focus gain and its reveal.
    pub(crate) fn note_done_seen(&mut self, wid: WindowId) {
        if !self.window_looked_at(wid) {
            return;
        }
        let Some(tab) = self.windows.get(&wid).and_then(|ws| ws.tab_set.active()) else {
            return;
        };
        let shown = if tab.zoomed {
            vec![tab.focus]
        } else {
            tab.root.leaves()
        };
        let mut moved = false;
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
                moved |= self.done_seen.insert(session, rev) != Some(rev);
            }
            // A program's turn end is a tab mark alone: no menu bar row.
            if let Some(news) = self.turn_ends_unseen(session) {
                self.turn_end_seen.insert(session, news);
            }
        }
        // The front tab wears no `•`, so only the menu bar's Done for you
        // can have moved.
        if moved {
            self.mark_status_dirty();
        }
    }

    /// THE ONE WRITE OF A DONE SEEN BY AN ACT (owner surfaces §3): the
    /// owner went to `session` from the menu bar, read its result, or a
    /// seamless update carried that they had (`SessionRecord::status_seen`,
    /// `App::adopt_status_carry`) — its done at entry rev `entered` is seen.
    /// Both readers of [`Self::done_seen`] follow at once: the menu bar's
    /// Done for you (dirtied) and the tab's `•`, wherever it is drawn (every
    /// strip re-stamped). A done already seen moves nothing.
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

    /// WHO DRIVES (owner surfaces §5, S9): build the team table
    /// ([`crate::team::table`]) and the done revs the tab's `•` reads, from
    /// one row per live session — its sid, its name (as every Agents surface
    /// names its tab, [`Self::tab_label_of_session`]) and its task's `from=`
    /// and done rev — each session's leaf locks taken one at a time, the
    /// terminal's by a try-lock, and the registry's only for a session no
    /// live rung names. At change rate only: a task moved
    /// (`Wake::TaskChanged`), a role or a title pin moved
    /// (`Wake::MetaChanged`), a manager's program retitled itself
    /// (`App::flush_title_drift`), a tab retired
    /// ([`Self::retire_presence`]), and [`Self::refresh_presence_all_windows`]
    /// (a restore, a successor's commit, a config adopt) — never a focus
    /// change, which moves nothing the table holds. Where
    /// either moved, every window's strip re-stamps and repaints (the footers'
    /// key reads the table).
    pub(crate) fn rebuild_teams(&mut self) {
        // The pool, not the registry: a closing tab leaves the pool first
        // (`App::detach_session_view`, whose retire rebuilds this table).
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
        // A task that finished in the tab someone is looking at is seen as
        // it lands: no projection need run for a done mark.
        if let Some(front) = self.frontmost_window {
            self.note_done_seen(front);
        }
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in &windows {
            Desk::request_redraw(self, *wid);
        }
        self.refresh_tab_chrome_windows(windows);
    }

    /// What `session`'s Team chip says ([`crate::claude_lights::TeamFace`]):
    /// the person's hold (the published `hold_by=person`, as the presence
    /// slot folded it) outranks its manager, which outranks its workers;
    /// `None` for none of them. Map reads and refcounts only: the footer asks
    /// it every frame it draws.
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

    /// The footer's repaint term for `session`'s Team chip: moves with the
    /// person's hold and with anything the team table says of it.
    pub(crate) fn team_term(&self, session: u64) -> u64 {
        let held = self
            .presence
            .slot(session)
            .is_some_and(|slot| slot.held_by_person);
        self.teams.get(&session).map_or(0, crate::team::Team::term)
            ^ u64::from(held).rotate_left(61)
    }

    /// A tab's HOVER, with its presence lines (§5): the composed `tooltip`
    /// (identity, description, connections) keeps every line but its
    /// `Needs attention:` one, which says the session's PUBLISHED need — and
    /// is gone where none stands, so a wait the harness settles is no reason
    /// on the hover either — then who drives it, then, for an identified
    /// agent, the state line (`claude · busy · ctx 61% left`, the engine's
    /// `state_line`: no age, since the hover is composed with the strip's
    /// push and shown long after), then who last typed into it and at what
    /// clock minute (`last typed into by lab-sweep at 14:02`). `None` when
    /// nothing is left to say.
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
        if let Some(need) = slot.and_then(|slot| slot.need.as_ref()) {
            lines.push(format!("{prefix}{}", need.what));
        }
        // The program's name and the published hold, under one leaf lock.
        let (program, held) = self.pool.get(session).map_or((None, None), |s| {
            let tl = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (tl.agent().program.clone(), tl.need().held)
        });
        if let Some(line) = self.who_drives_line(session, held) {
            lines.push(line);
        }
        if let Some(line) = slot
            .filter(|slot| slot.agent.is_some())
            .and_then(|slot| presence::state_line(slot, program.as_deref()))
        {
            lines.push(line);
        }
        // Who last typed into it over the socket, at the clock minute (S9):
        // the hover is pushed again when that moves (`Wake::Typed`).
        if let Some(line) = self
            .pool
            .get(session)
            .and_then(|s| self.typed_hover_line(&s.ctx))
        {
            lines.push(line);
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// The hover's WHO DRIVES line (§5, S9): the published hold (`held`,
    /// `status hold_by=`) — `you have it` for the person's, `held (aterm ctl
    /// hold)` for an Owner client's, `held by mac-03 over the fleet` for a
    /// remote node's, `held over the message bus` for the bridge's (§7: a
    /// hold is a record, hover only, no mark tone) — then
    /// `worker of lab-sweep` and `manages 3` from the team table, and
    /// `driven by aterm (cruise)` where this host's harness holds the
    /// session's claim and its Autopilot moves the work — joined ` · `;
    /// `None` with none of them. A record, composed with the hover.
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
                // The Mac driving it, by the name this Mac's book gives it.
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
        let (_, view, supervised) = self.session_autopilot(session);
        if supervised
            && matches!(
                view,
                crate::autopilot::AutopilotView::Cruise | crate::autopilot::AutopilotView::Land
            )
        {
            parts.push(format!("driven by aterm ({})", view.wire()));
        }
        (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
    }

    /// The native pill's click as its wake delivers it (`Wake::PillAct`): it
    /// crosses the move-licence fence and settles an open rename first, as a
    /// native tab select and the in-grid pill's press do, then acts
    /// ([`Self::on_pill_act`]).
    pub(crate) fn on_pill_wake(&mut self, window: WindowId, session: u64, hit: presence::PillHit) {
        self.clear_tab_surface_move_license(Some(window));
        self.settle_rename_edit(window);
        self.on_pill_act(window, session, hit);
    }

    /// A click on window `window`'s pill landed on `hit`, the pill naming
    /// `session` when it was drawn (`Wake::PillAct`). STALE CLICKS FAIL
    /// CLOSED: unless the window's pill still names that session and still
    /// offers the act, nothing happens. The body goes to the session's tab
    /// and pane; Restart runs the restart act the pill was offered for; `+N`
    /// opens the Overview at the window's next need, the window raised
    /// ([`Self::open_overview_raised`]); Hand Back lifts the person's hold.
    pub(crate) fn on_pill_act(&mut self, window: WindowId, session: u64, hit: presence::PillHit) {
        let Some(pill) = self
            .windows
            .get(&window)
            .and_then(|ws| ws.presence.pill.as_ref())
            .filter(|pill| pill.session == session)
        else {
            return;
        };
        // Each lands in THIS window: the pill names a tab of it, and a
        // session shown in two windows is focused where it was clicked.
        match hit {
            presence::PillHit::Go => {
                let _ = self.focus_session_preferring(Some(window), session);
            }
            // The card that lists them all (§5: "+2 opens Overview"), on this
            // window, raised, its cursor on the window's next need after the
            // one in front, else after the one the pill names
            // (`drive::next_need`): the first of the N the count stands for.
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

    /// The pill's Restart: the host's restart act on `session`'s stalled
    /// program (`aterm ctl @<sid> restart if-stalled`, [`crate::restart_act`]),
    /// as a person's: run once, its reply logged. The act re-checks its own
    /// fences, so a pill drawn before the stall moved refuses with
    /// `ERR not-stalled` and changes nothing. It runs on a short-lived thread
    /// of its own, as it does on a control thread: it takes the instance's
    /// one act lock (which another restart holds across its fences and its
    /// stamp's write) and writes the stamp itself, neither of which the main
    /// thread may wait on.
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

    /// The pill's Resume: `signal cont` to `session`'s stopped job, as a
    /// person's act, its reply logged ([`resume_if_stopped`]). Fenced: it
    /// sends nothing unless the terminal's foreground job is still stopped
    /// when the signal goes, so a pill drawn before the job was continued (or
    /// another job took the terminal) changes nothing. Off the main thread,
    /// as the restart is: the signal reads the foreground group and the
    /// stall's watch looks again after it.
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

    /// Re-project every window whose pill names `session`: its label moved
    /// (a `meta set role|title`, its program's title), which no fact the
    /// engine folds carries, so a quiet or frozen session would otherwise
    /// keep its old name on the pill for as long as nothing else moved. A
    /// window whose pill names another session costs one compare.
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

    /// Hand Back (the pill's, the Team chip's `✋`): lift the person's hold on
    /// `session`, and only it — Take Over's own act
    /// (`App::hand_back_session`, S5), so a control client's pointer into the
    /// in-grid pill is refused under its gesture mark as the menu row's
    /// would be. A refusal is logged: the click was offered for a hold that
    /// stood when the pill was drawn, and nothing moved.
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

    /// `chrome`'s presence line for the front window (the frontmost, else
    /// the first): the engine's bytes ([`drive::chrome_line`]).
    pub(crate) fn presence_chrome_line(&self) -> String {
        let front = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied());
        drive::chrome_line(self, front)
    }

    /// A session's level as `status level=` reports it, for the tests
    /// ([`drive::session_level`]).
    #[cfg(test)]
    pub(crate) fn presence_session_level(&self, session: u64) -> presence::Level {
        drive::session_level(self, session)
    }

    /// The additive presence `status` fields (`hand= hand_pid= level= story= why=`):
    /// the engine's bytes ([`drive::status_tail`]).
    pub(crate) fn presence_status_tail(&self, session: u64) -> String {
        drive::status_tail(self, session)
    }

    /// `aterm ctl story <verb> [<text>]` for `session` (`drive::tell`): the
    /// slot is brought current and the point noted (`status story=`), a
    /// record. Returns the point's seq; `Err` if the pool no longer holds
    /// the session.
    #[cfg(test)]
    pub(crate) fn tell_story(
        &mut self,
        session: u64,
        verb: StoryVerb,
    ) -> Result<u64, &'static str> {
        self.tell_story_by(session, verb, aterm_slog::Actor::Ctl)
    }

    /// [`Self::tell_story`] told by `by` (the wire's `log_actor`, carried in
    /// `Wake::Story`): once the point is noted, the session log's `mark -
    /// story word=<verb>` (never a text) under the timeline alone, every
    /// other lock let go, and then the log's watchers (session log phase 3B,
    /// addendum A15).
    pub(crate) fn tell_story_by(
        &mut self,
        session: u64,
        verb: StoryVerb,
        by: aterm_slog::Actor,
    ) -> Result<u64, &'static str> {
        let seq = drive::tell(self, session, verb).ok_or("no such session")?;
        let word = StoryVerb::TOLD_WORDS
            .into_iter()
            .find(|w| StoryVerb::parse_told(w) == Some(verb))
            .unwrap_or("unknown");
        let owed = self.pool.get(session).is_some_and(|s| {
            let mut timeline = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let at = timeline.slog().now();
            timeline.slog_mut().observe_story(word, by, at);
            timeline.slog_take_wake()
        });
        self.wake_log_watchers(session, owed);
        Ok(seq)
    }

    /// Run `f` with every chrome-row re-grid it causes
    /// (`regrid_for_chrome_rows`) recorded, then apply one re-grid from the
    /// rows the commits left.
    ///
    /// A freeze lift commits the band's growth and the strip's change back to
    /// back; unbatched that can be two PTY resizes, and a program that
    /// repaints only on a changed size never repaints. A re-grid to the rows
    /// the grid already has resizes nothing (`apply_term_resize`). Nested
    /// batches join the outer one.
    #[cfg(any(unix, test))]
    pub(crate) fn with_chrome_regrid_batch<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        if self.chrome_regrid_batch.is_some() {
            return f(self);
        }
        self.chrome_regrid_batch = Some(ChromeRegridBatch::default());
        // A panic in `f` must not leave the batch open: the batch ends and
        // applies what it recorded before the panic resumes.
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match self.chrome_regrid_batch.take().unwrap_or_default() {
            ChromeRegridBatch::Idle => {}
            ChromeRegridBatch::All => self.regrid_for_chrome_rows(),
        }
        out.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }

    /// The terminal rows window `wid` measures a chrome commit against, and
    /// whether it has glass ([`chrome_regrids_grid`]). Normally `ws.rows`;
    /// inside a [`Self::with_chrome_regrid_batch`] that recorded `wid`, the rows
    /// the pending re-grid will give ([`Self::grid_dims_for`] over the OS size),
    /// so a batch's second commit is measured against the first's result.
    pub(crate) fn chrome_commit_grid_rows(&self, wid: WindowId) -> Option<(u16, bool)> {
        let ws = self.windows.get(&wid)?;
        // The OS size is read only inside a batch that recorded `wid`.
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

    /// The pure half of [`Self::chrome_commit_grid_rows`]: `rows`, or inside a
    /// batch that recorded `wid` with an OS `size`, the rows that re-grid gives.
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

    /// Lift an update handoff's park (`Wake::UpdateHandoffFinished`): `finish`
    /// settles the attempt (clearing the park on failure or abort), then every
    /// window's border and pill are projected again and the band commits what
    /// it owes in one re-grid ([`Self::with_chrome_regrid_batch`]). `finish`
    /// may close windows or quit.
    #[cfg(unix)]
    pub(crate) fn release_update_handoff_park(&mut self, finish: impl FnOnce(&mut Self)) {
        self.with_chrome_regrid_batch(|app| {
            finish(app);
            app.refresh_presence_all_windows();
            // Band row changes are deferred while a handoff is pending
            // (`sync_message_band_rows`); now it has settled, commit them.
            app.sync_messages();
        });
    }

    /// Lift a successor's freeze (the Commit wake, once
    /// `incoming_handoff_pending` is false): every window's border and pill
    /// are projected again and the band commits what it owes (the landing's record,
    /// carried scrollback loss via [`Self::say_carried_history_loss`], the
    /// carried Settings drafts the restore could not put back via
    /// [`Self::say_carried_settings_drafts_loss`]) in one re-grid
    /// ([`Self::with_chrome_regrid_batch`]).
    #[cfg(unix)]
    pub(crate) fn release_successor_commit_freeze(&mut self) {
        self.with_chrome_regrid_batch(|app| {
            app.refresh_presence_all_windows();
            // Carried supervisor leases lapse on time if their holder does not
            // renew them here, so a supervisor host parked behind one takes the
            // session then.
            app.arm_supervisor_expiries();
            if let Some(build) = app.landing_at_commit.take() {
                app.retire_charging_surge();
                // …naming the tabs this successor adopted onto a blank
                // screen, if any.
                app.post_update_landed(
                    crate::build_info::version_display(),
                    build,
                    app.seamless_repainted_tabs(),
                );
            }
            // The scrollback the parent left behind: its row grows the band in
            // this one re-grid, not when the import settles later.
            app.say_carried_history_loss();
            // …and the carried Settings text the restore could not put back,
            // which the parent held until now.
            app.say_carried_settings_drafts_loss();
            app.sync_messages();
        });
    }

    /// Note a rise of the chrome rows every window shares (a band row reached the glass, the
    /// tab strip grew) on every window's regrow floor (`aterm_messages::chrome::RegrowFloor`);
    /// read only where the grid re-grids for its chrome ([`chrome_regrids_grid`]).
    pub(crate) fn note_chrome_rise_everywhere(&mut self, now: Instant) {
        for ws in self.windows.values_mut() {
            ws.regrow_floor.rose(now);
        }
    }

    /// Window `wid`'s chrome regrow floor (`RegrowFloor::floor`): `None` for a
    /// window whose grid never re-grids for its chrome, or with no rise noted.
    #[cfg(test)]
    pub(crate) fn chrome_regrow_floor(&self, wid: WindowId) -> Option<Instant> {
        self.windows
            .get(&wid)
            .filter(|ws| chrome_regrids_grid(ws))
            .and_then(|ws| ws.regrow_floor.floor())
    }

    /// The floor the band's quiet shrink and the tab strip's fall wait: the latest
    /// of every window their re-grid resizes (`aterm_messages::chrome::latest`).
    pub(crate) fn band_regrow_floor(&self) -> Option<Instant> {
        aterm_messages::chrome::latest(
            self.windows
                .values()
                .filter(|ws| chrome_regrids_grid(ws))
                .map(|ws| ws.regrow_floor.floor()),
        )
    }

    /// Forget a retired session's slot, every window's watermark for it, its
    /// done-seen rev, and its need's recheck and part in the API outage (no
    /// publication runs for it again, so nothing else would).
    pub(crate) fn retire_presence(&mut self, session: u64) {
        self.presence.retire(session);
        for ws in self.windows.values_mut() {
            ws.presence.watermarks.remove(&session);
        }
        self.done_seen.remove(&session);
        self.turn_end_seen.remove(&session);
        self.forget_need(session, need_unix_now());
        // A worker that closed leaves its manager's count; a manager that
        // closed leaves its workers naming a sid not here.
        self.rebuild_teams();
    }
}

/// The presence driver's host (`aterm_messages::presence::drive::Desk`):
/// every method a read of the App or one effect on it.
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

    /// Every terminal leaf of every tab of `w`, in tab order, the front tab
    /// marked: what the window's most pressing session is chosen from.
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

    /// The claim's lapse on the microsecond clock, mapped onto `now`'s: one
    /// millisecond past it, so the wake's lapse check sees it lapsed.
    fn supervisor_lapse(&self, session: u64, now: Instant) -> Option<Instant> {
        let at_us = self.pool.get(session).and_then(|s| {
            s.ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .supervisor_expiry()
        })?;
        let now_us = crate::metrics::now_us();
        Some(now + Duration::from_micros(at_us.saturating_sub(now_us)) + Duration::from_millis(1))
    }

    /// A window someone can see: on screen, not occluded or minimized, with
    /// an OS window (a headless one's pill is read by `chrome`, which
    /// composes its age at the read).
    fn pill_visible(&self, w: WindowId) -> bool {
        self.windows
            .get(&w)
            .map(Box::as_ref)
            .is_some_and(crate::messages_host::band_on_screen)
    }

    /// View ▸ Show Needs in Windows, persisted as `[presence] rim`.
    fn needs_shown(&self) -> bool {
        self.presence_rim_on()
    }

    /// The pill names a session as every Agents surface names its tab
    /// ([`App::tab_label_of_session`]: role, title pin, the strip's live
    /// rungs without an agent's status glyph, the registry's title, the
    /// tab's position, the short sid), so ❯, the Overview, the tab menu and
    /// the pill agree, and a pill naming another pane always names it. Read
    /// where a pill is composed, at change rate.
    fn label_of(&self, session: u64) -> String {
        self.tab_label_of_session(session)
    }

    /// The one predicate every stall word chooses by
    /// (`harness_host::offered_now` over `restart_act::offered`).
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
        // A need recheck rides the herald's wake: the verdict it moves is
        // what the herald tells.
        match (herald, self.need_recheck_deadline(now)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The front window's hold, for the native menu's synchronous validate,
    /// and the front session's finished task looked at where someone can
    /// ([`App::note_done_seen`]).
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

    /// The pill's age moved and nothing else: the titlebar pill is pushed
    /// alone (`toolbar::set_window_pill`), no tab chip re-stamped and no
    /// strip re-planned, while its width holds; where it moved (its age's
    /// unit did) the strip is pushed whole. Returns the windows whose
    /// in-grid strip carries the pill ([`App::strip_carries_pill`]): only
    /// their frame draws it, so only they repaint (their strip's cache keys
    /// on the pill's age step); a pill on glass re-presents no frame.
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

    /// Every refresh of the session (`drive::refresh_session`, the wakes and
    /// the timer's lapses alike) publishes its need verdict first
    /// ([`App::publish_need`]), so the facts it senses next read the verdict
    /// this refresh published, and the herald it rides reads it too. The
    /// human channel dedups by transition itself, so a refresh that moved
    /// nothing costs a few leaf-lock reads and no AppKit call.
    fn herald(&mut self, session: u64) {
        self.publish_need(session);
        self.herald_session(session);
    }

    /// Every owed notification the rate limit now allows
    /// ([`crate::status_item::Herald::due`]).
    fn herald_owed(&mut self, now: Instant) {
        self.need_rechecks_due();
        let (ready, _) = self.presence.herald.due(now);
        for session in ready {
            self.herald_session(session);
        }
    }

    /// Remove and record the lapsed claim (`meta-change field=supervisor
    /// value=-`, pushed as `EVENT meta`); the driver's re-read re-heralds the
    /// session, so a box the dead supervisor held reaches the menu row and the
    /// notification.
    fn lapse_supervisor(&mut self, session: u64) {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return;
        };
        if crate::session_timeline::lapse_supervisor(&ctx, crate::metrics::now_us()) {
            // The footer's Autopilot chip and the harness card read the live
            // claim: a lapsed one repaints them, as a released one does
            // (`Wake::MetaChanged`, `App::on_meta_changed`). So does the
            // session's A/B arm, and its Settings ▸ Harness row.
            self.redraw_claude_footer(session);
            self.note_harness_arm(session);
            self.harness_screen_moved();
            if self.subscribers.any() {
                self.subscribers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .notify(session);
            }
        }
    }

    fn forget(&mut self, session: u64) {
        self.presence.retire(session);
        // The count the looking rule saw was the retired slot's.
        self.turn_end_seen.remove(&session);
    }
}

fn pack(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

/// THE PILL'S RESUME, fenced ([`App::pill_resume`]): the `signal cont` verb
/// ([`crate::control::signal_verb`]) to `master`'s foreground job only
/// while that job is stopped (`input_stall::fg_stopped`); otherwise
/// `ERR not-stopped`, and nothing is sent.
#[cfg(unix)]
pub(crate) fn resume_if_stopped(master: i32, ctx: &crate::SessionCtx) -> String {
    if !crate::input_stall::fg_stopped(master) {
        return "ERR not-stopped\n".to_string();
    }
    crate::control::signal_verb(master, "cont", ctx)
}

/// The instant a ledger turn opened, from its `started_ms` (the ledger clock,
/// `turn_ledger::now_ms`), on this process's monotonic clock: never later than
/// the turn really opened (the clock reads whole milliseconds, so one more is
/// taken off). `None` where the clock cannot reach back that far.
fn opened_at(started_ms: u64) -> Option<std::time::Instant> {
    let now = std::time::Instant::now();
    let ago = crate::turn_ledger::now_ms()
        .saturating_sub(started_ms)
        .saturating_add(1);
    now.checked_sub(std::time::Duration::from_millis(ago))
}

#[cfg(test)]
mod tests {
    //! The host's half: the published verdict lights the window's border,
    //! its pill and the tab marks; presence re-grids no pane; the idle repaint
    //! invariant; the classifier gate; a stale pill click acts on nothing.
    //! The border's images are `app_introspect`'s capture tests.

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

    /// A second tab in window `wid`, now front: its session id and context.
    fn second_tab(app: &mut App, wid: WindowId) -> (u64, Arc<crate::SessionCtx>) {
        let sid = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid));
        let ctx = app.pool.get(sid).expect("stub pooled").ctx.clone();
        (sid, ctx)
    }

    /// An ask on `ctx` (a bare `meta attention`): a decide need.
    fn ask(ctx: &crate::SessionCtx, text: Option<&str>) {
        ctx.meta.lock().unwrap().attention = text.map(str::to_string);
    }

    /// `ctx`'s task marked `state` (a `stalled` one is a stuck need).
    fn task(app: &App, ctx: &crate::SessionCtx, local: u64, state: TaskState) {
        let mark = TaskMark {
            why: (state == TaskState::Stalled).then_some("startup"),
            ..TaskMark::new(state)
        };
        mark_session_task(ctx, local, &mark, &app.subscribers).expect("unfenced");
    }

    fn view(app: &App, wid: WindowId) -> &WindowView {
        app.presence_view(wid).expect("a window")
    }

    /// THE PUBLISHED VERDICT LIGHTS THE BORDER: an ask is the decide border
    /// and its pill, a stalled task the stuck one, and with neither the
    /// window is quiet again. The pill names the session the border is lit
    /// for, and `chrome presence` says the same.
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
        task(&app, &ctx, sid, TaskState::Stalled);
        app.refresh_presence_session(sid);
        assert_eq!(view(&app, wid).rim, Rim::Stuck, "stuck outranks decide");
        ask(&ctx, None);
        task(&app, &ctx, sid, TaskState::Working);
        app.refresh_presence_session(sid);
        assert_eq!(view(&app, wid).rim, Rim::None);
        assert!(app.presence_pill(wid).is_none());
        assert!(
            app.presence_chrome_line()
                .contains("rim=none pressing=- pill=\"\"")
        );
    }

    /// A WAIT IS NO NEED, SO IT LIGHTS NOTHING (§2.4, scenario 5): an
    /// attention entry typed `kind=wait` (the harness waiting out a limit)
    /// publishes no need, and presence reads only the published need — no
    /// border (`chrome presence rim=none`), no pill, no mark — although the
    /// raw entry stands on the session's meta. NEGATIVE CONTROL: the same
    /// entry typed `kind=ask` is a decide need and lights all three.
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

    /// THE MOST PRESSING SESSION, FROM A BACKGROUND TAB: a need in the tab
    /// behind lights the window and its pill names that tab by its label and
    /// number; the front tab's own need wins a tie and the other counts as
    /// `+1`.
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
        // Its 1-based tab number: the headless window's own first tab, then
        // the stub's.
        let tab = app.windows[&wid].tab_set.tabs().len() - 1;
        assert_eq!(view(&app, wid).pill.as_ref().and_then(|p| p.tab), Some(tab));
        // The front tab's own need wins the tie; the other is `+1`.
        ask(&second_ctx, Some("run the tests?"));
        app.refresh_presence_session(second);
        assert_eq!(view(&app, wid).pressing, Some(second));
        assert_eq!(view(&app, wid).pill.as_ref().map(|p| p.more), Some(1));
        let _ = first;
    }

    /// SCENARIO 1 AS A REGRESSION TEST: a hundred verdict flips on a
    /// background tab ask for no chrome re-grid at all and leave the window's
    /// chrome rows where they were — presence owns no row. NEGATIVE CONTROL:
    /// a real chrome change (the tab strip growing a row) is seen by the same
    /// probe, so its silence above is no blind spot.
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
        // CONTROL: the probe sees a chrome re-grid when one happens.
        app.set_tab_strip_rows(app.tab_strip_rows + 1, Instant::now());
        assert!(
            take_chrome_regrids_applied()
                .iter()
                .any(|(w, r)| *w == wid && *r == rows + 1),
            "the strip's new row is a re-grid the probe records"
        );
    }

    /// A lit pill on a headless window arms no age timer: nobody sees it, and
    /// `chrome` composes its age at the read.
    #[test]
    fn a_headless_pill_arms_no_timer() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        ask(&ctx, Some("approve?"));
        app.refresh_presence_session(sid);
        assert!(app.presence_pill(wid).is_some());
        let now = Instant::now();
        assert_eq!(app.presence_deadline(now), None);
    }

    /// THE IDLE LAW WHILE THE STATUS REVISIONS REPLAY (§10): a quiet session
    /// whose worker oscillates Running/Quiet a thousand frames over, each new
    /// revision refreshed as the sweep refreshes it, leaves its window's
    /// `presence_fp` at 0, its overlay `None`, the timer's tick empty and no
    /// deadline armed, every frame. NEGATIVE CONTROL: another window's
    /// session that needs the person lights that window, and its refresh
    /// leaves this one quiet.
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
            // CONTROL: a need in another window's session lights that window,
            // and its refresh leaves this one quiet.
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

    /// THE SPOKEN PILL HOLDS STILL WHILE ITS AGE TICKS: a background tab's
    /// pill prints an age, but the live region's sentence is the same a
    /// minute on, and the age rides the description. CONTROL: the aged
    /// description does move with the clock, and a new need moves the
    /// sentence.
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

    /// Presence's half of the classifier gate: a refresh folds the sweep's
    /// published reading and the published verdict and never classifies a
    /// screen, so refreshes cost zero `aterm_phase` calls.
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

    /// THE TAB MARKS: a decide need is the hollow diamond, a stuck one the
    /// filled, a finished task not yet looked at the dot — never on the front
    /// tab, and gone once the tab has been front.
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
            // The stub's tab and the second, after the window's own first.
            metadata[metadata.len() - 2..]
                .iter()
                .map(|m| m.attention)
                .collect::<Vec<_>>()
        };
        // Someone looks at this window: the frontmost, on screen.
        app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
        assert_eq!(app.frontmost_window, Some(wid));
        ask(&first_ctx, Some("approve?"));
        app.refresh_presence_session(first);
        assert_eq!(marks(&app), [ChipLevel::Wait, ChipLevel::Off]);
        ask(&first_ctx, None);
        task(&app, &first_ctx, first, TaskState::Done);
        app.on_task_changed(first);
        assert_eq!(
            marks(&app),
            [ChipLevel::Done, ChipLevel::Off],
            "a finished task behind is the dot"
        );
        // The front tab's own task finishes in front, moving no need (no
        // projection runs): it is seen as it lands, so the tab left behind
        // wears no dot.
        task(&app, &second_ctx, second, TaskState::Done);
        app.on_task_changed(second);
        assert_eq!(marks(&app)[1], ChipLevel::Off);
        // The stub's tab comes front, and back: its dot is gone, the rev seen;
        // the second, left behind, wears none.
        let tabs = app.windows[&wid].tab_set.tabs().len();
        app.switch_tab_in(wid, tabs - 2);
        assert_eq!(marks(&app), [ChipLevel::Off, ChipLevel::Off]);
        app.switch_tab_in(wid, tabs - 1);
        assert_eq!(marks(&app), [ChipLevel::Off, ChipLevel::Off]);
        // NEGATIVE CONTROL: a stalled task is a stuck need's mark.
        task(&app, &first_ctx, first, TaskState::Stalled);
        app.on_task_changed(first);
        assert!(matches!(marks(&app)[0], ChipLevel::Stop(_)));
    }

    /// WHICH DONE THE OWNER SAW CROSSES A SEAMLESS UPDATE (§5): of two
    /// finished workers behind the front tab, the one the owner looked at
    /// wears no `•` in the successor — the park stamps what the status item
    /// knew of each session (`App::status_carry_rows`, the one carry of what
    /// the owner saw and was told), the successor seeds the task from its
    /// carried row and takes the carry at the session's first publication
    /// (`App::adopt_status_carry`), and the look lands in the one
    /// `done_seen`. NEGATIVE CONTROL: the one nobody looked at keeps its dot
    /// across the same update.
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
            // The two workers' tabs, between the window's own first and the
            // front tab.
            [metadata[1].attention, metadata[2].attention]
        };
        // THE PARENT: `seen` and `unseen` finish behind the front tab of a
        // window someone looks at; the owner brings `seen` to the front and
        // goes back.
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
            task(&parent, ctx, local, TaskState::Done);
            parent.on_task_changed(local);
        }
        parent.switch_tab_in(wid, 1);
        parent.switch_tab_in(wid, 3);
        assert_eq!(
            behind(&parent, wid),
            [ChipLevel::Off, ChipLevel::Done],
            "PRECONDITION: one seen, one not"
        );
        // THE PARK's two carries: the task rows and the status item's rows.
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
        // THE SUCCESSOR: its own sessions, in the same tabs, seeded from the
        // carried rows, its status carry taken at each first publication.
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
        // Each session's first publication takes its carry.
        successor.herald_session(first);
        successor.herald_session(second);
        assert_eq!(
            behind(&successor, wid),
            [ChipLevel::Off, ChipLevel::Done],
            "the look crossed; the unlooked dot stands"
        );
    }

    /// NOBODY LOOKED, SO NOTHING IS SEEN (§3): a task that finishes in the
    /// front tab of a window nobody can see (occluded) is not looked at by
    /// any projection there, and wears its dot once the tab is behind.
    /// CONTROL: revealed, the window's front tab is seen at once.
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
        task(&app, &second_ctx, second, TaskState::Done);
        app.on_task_changed(second);
        app.refresh_presence_all_windows();
        app.switch_tab_in(wid, tabs - 2);
        assert_eq!(dot(&app), ChipLevel::Done, "nobody looked");
        // CONTROL: back in front and revealed, it is seen.
        app.switch_tab_in(wid, tabs - 1);
        app.on_occlusion_changed(wid, false);
        app.switch_tab_in(wid, tabs - 2);
        assert_eq!(dot(&app), ChipLevel::Off, "seen once revealed");
    }

    /// EVERY PANE ON SHOW IS LOOKED AT (§5): a worker in the split pane that
    /// does not have the focus finishes while its tab is in front of a window
    /// someone looks at, and once the tab is behind it wears no dot.
    /// NEGATIVE CONTROL: the same in an occluded window keeps the dot.
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
            task(&app, &worker_ctx, worker, TaskState::Done);
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

    /// AN IN-WINDOW RENAME RENAMES THE SESSION WHERE IT IS NAMED (§5): Set
    /// Role… on a manager behind the front tab renames it on the pill that
    /// names it and on its worker's Team chip, as a wire `meta set role`
    /// does. CONTROL: before the write both carry the old name.
    #[test]
    fn an_in_window_role_renames_the_pill_and_the_team() {
        use crate::claude_lights::TeamFace;
        let (mut app, wid, manager, manager_ctx) = app_with_stub();
        let (worker, worker_ctx) = second_tab(&mut app, wid);
        let mark = TaskMark {
            from: Some(manager_ctx.self_id.as_str().to_string()),
            ..TaskMark::new(TaskState::Working)
        };
        mark_session_task(&worker_ctx, worker, &mark, &app.subscribers).expect("unfenced");
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

    /// A WORKER'S TEAM CHIP LANDS WHERE IT WAS CLICKED (§5): its manager is
    /// shared into a second window, behind the worker there; a press on the
    /// worker's chip in that window brings the manager's tab to its front.
    /// NEGATIVE CONTROL: the plain session-focusing body finds the first
    /// window showing the manager and leaves the clicked one where it was.
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
            mark_session_task(&worker_ctx, worker, &mark, &app.subscribers).expect("unfenced");
            app.on_task_changed(worker);
            assert_eq!(app.focused_session_id(w1), Some(worker));
            if via_chip {
                app.note_claude_team(w1, worker);
                app.press_claude_chip(w1, worker, crate::claude_lights::Chip::Team);
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

    /// A HOLD IS A RECORD (§2.2, §7): an Owner client's hold and the bridge's
    /// each add one who-drives line to the tab's hover, a different one, and
    /// light no border, no pill and no mark. CONTROL: with no hold the hover
    /// has nothing to say.
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

    /// THE TAB'S OWN NOTE IS NO NEED (§5): a failed last command (the status
    /// indicator's bit) with no need published stamps the quiet note — never
    /// `◇` or `◆` — and its in-grid ink is the label's, no presence tone, as
    /// the border and `status needs=-` say nothing. CONTROL: a decide need on
    /// the same tab stamps its `◇` over the note.
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
        // CONTROL: the published need's mark outranks the note.
        ask(&first_ctx, Some("approve?"));
        app.refresh_presence_session(first);
        let item = stamp(&app);
        assert_eq!(item.attention, ChipLevel::Wait);
        assert_ne!(crate::tab_bar::status_mark_ink_for_test(item, ink), ink);
    }

    /// STALE CLICKS FAIL CLOSED: a pill click naming a session the window's
    /// pill no longer names does nothing; the body of the current pill goes
    /// to its tab; an act the pill does not offer does nothing.
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

    /// THE PILL'S HAND BACK IS TAKE OVER'S OWN: the person's hold on the
    /// front tab offers it, and its click lifts the person's slot.
    /// NEGATIVE CONTROL: the same click under a control client's gesture
    /// mark (its pointer into the in-grid pill) is refused and the hold
    /// stands.
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

    /// THE PERSON'S TAKE OVER LIGHTS EVERY "YOURS" (§4, §5): the person slot
    /// reaches the need facts as `HoldWord::Person`, so the one published
    /// verdict draws the yours outline (`chrome presence rim=yours`), the
    /// tab's hand mark, the `✋ you` Team chip and the menu bar's You hold
    /// row; the chip's `✋` hands it back, and every one of them goes out.
    /// NEGATIVE CONTROLS: before the Take Over none is lit, and the `✋`
    /// pressed under a control client's gesture mark lifts nothing.
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
        app.with_gesture_source(true, |app| app.press_claude_chip(wid, sid, Chip::Team));
        assert!(
            ctx.fabric.holds().person().is_some(),
            "a control client's press lifts nothing"
        );
        app.press_claude_chip(wid, sid, Chip::Team);
        assert!(ctx.fabric.holds().person().is_none(), "handed back");
        assert_eq!(
            yours(&app),
            (Rim::None, Some(ChipLevel::Off), None, Vec::new())
        );
    }

    /// A MANAGER'S TEAM CHIP OPENS THE OVERVIEW AT IT (§5): on the window
    /// clicked, raised, the cursor on the manager's manages line, whose ⏎
    /// goes to it; no tab moves. NEGATIVE CONTROL: a worker whose manager is
    /// not on this aterm has a chip whose press opens nothing and raises
    /// nothing.
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
        mark_session_task(&worker_ctx, worker, &from(&manager_sid), &app.subscribers)
            .expect("unfenced");
        app.on_task_changed(worker);
        mark_session_task(&stray_ctx, stray, &from("s-not-here"), &app.subscribers)
            .expect("unfenced");
        app.on_task_changed(stray);
        let _ = crate::app_tabs::take_raised_windows();

        app.note_claude_team(wid, stray);
        app.press_claude_chip(wid, stray, Chip::Team);
        assert!(app.windows[&wid].overview().is_none(), "control: no card");
        assert_eq!(crate::app_tabs::take_raised_windows(), Vec::new());

        app.note_claude_team(wid, manager);
        app.press_claude_chip(wid, manager, Chip::Team);
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

    /// THE PILL'S RESUME CONTINUES ONLY A JOB STILL STOPPED: a real `sleep`
    /// leading a pty's foreground, stopped, is continued by the fenced act.
    /// NEGATIVE CONTROL: while it runs, the same act answers `not-stopped`
    /// and sends nothing.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn the_pills_resume_continues_only_a_job_still_stopped() {
        use aterm_uds::spawnfd::InheritNothing as _;
        use std::os::fd::{FromRawFd as _, OwnedFd};
        use std::os::unix::process::CommandExt as _;
        /// The child and its master, killed and closed however the test ends.
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
        // std's Command on purpose: the PTY child needs `pre_exec` (setsid +
        // TIOCSCTTY), which spawnfd lacks; the strip first, so the `sleep`
        // holds its terminal and nothing else of this process.
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.inherit_nothing()
            .arg("60")
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio());
        // SAFETY: in the forked child before exec: `setsid` and
        // `ioctl(TIOCSCTTY)` are async-signal-safe and touch nothing of the
        // parent's.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY.into(), 0) < 0 {
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

    /// A SHARED SESSION'S PILL LANDS WHERE IT WAS CLICKED (§5): a session
    /// shown in two windows, in front of the first and behind in the second,
    /// is named by the second's pill as its tab there, and a click on that
    /// pill brings that tab to the front of the window clicked. NEGATIVE
    /// CONTROL: the plain session-focusing body finds the first window that
    /// shows it and leaves the clicked one where it was.
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

    /// THE PILL GOES TO THE PANE (§5): a need in the pane of the front tab
    /// that does not have the focus is named on the pill by its label, and
    /// the pill's body moves the keyboard to that pane, not only to its tab
    /// — so an answer typed to the ask lands there. CONTROL: once that pane
    /// has the focus the pill names nothing; it is in front.
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

    /// A NEED IN ANOTHER PANE IS ALWAYS NAMED (§5): the pane that needs you
    /// has no role, no title pin and no program title, and the pill still
    /// names it — as every Agents row does, by the tab's position here — so
    /// it never reads as the focused pane's own need and an answer typed
    /// after reading it reaches the session it names. CONTROL: focused, it is
    /// in front, and the pill names nothing.
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

    /// ONE NAME FOR A BACKGROUND WORKER ON EVERY SURFACE (§1: "rows do not
    /// name their tab"): a worker started behind, with no role and only its
    /// program's OSC title (`✳ bench`), is `bench` in ❯'s need row and its
    /// notice, on the pill and in the Overview. NEGATIVE CONTROL: the
    /// registry's ladder ❯ read before (role, user title, the registry's
    /// title, else the short sid) names it by its sid, since only a window's
    /// active tab publishes a registry title.
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
        // NEGATIVE CONTROL: the registry's ladder alone.
        let registry = crate::status_item::tab_name(None, None, &handle.title)
            .unwrap_or_else(|| presence::short_sid(handle.sid.as_str()));
        assert_eq!(
            registry,
            presence::short_sid(handle.sid.as_str()),
            "{:?}",
            handle.title
        );
    }

    /// THE PILL NAMES A TAB AS ITS STRIP DOES (§5): a background tab with no
    /// role and no user title is named by the title its strip shows — its
    /// program's live title — without Claude Code's idle `✳`; a new title
    /// and a `meta set title` each re-project the pill with no other fact
    /// moving, so a frozen session's pill never keeps an old name.
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
        // The program retitles itself: the drift flush re-projects the pill.
        retitle(&app, "deploy");
        assert!(app.flush_title_drift(first, Instant::now()));
        assert_eq!(label(&app), app.tab_titles(wid)[tab]);
        // A `meta set title` (the MetaChanged arm) does too.
        first_ctx.meta.lock().unwrap().user_title = Some("mytab".into());
        app.on_meta_changed(first);
        assert_eq!(label(&app), "mytab");
    }

    /// `+N` OPENS THE OVERVIEW (§5: "+2 opens Overview"): on the pill's own
    /// window, raised, its cursor on the window's next need after the one
    /// the pill names; the front tab stays where it was. NEGATIVE CONTROLS:
    /// with one need the pill carries no `+N`, and a press on it opens
    /// nothing and raises nothing; nor does a stale press, naming a session
    /// the pill no longer names.
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
        // One need: no `+N`, and its press does nothing.
        assert_eq!(view(&app, wid).pill.as_ref().map(|p| p.more), Some(0));
        app.on_pill_act(wid, first, PillHit::More);
        assert!(app.windows[&wid].overview().is_none(), "control: no card");
        assert_eq!(crate::app_tabs::take_raised_windows(), Vec::new());

        set_presence_clock_for_test(Some(t0 + Duration::from_secs(1)));
        ask(&second_ctx, Some("two?"));
        app.refresh_presence_session(second);
        let named = view(&app, wid).pill.as_ref().map(|p| (p.session, p.more));
        assert_eq!(named, Some((first, 1)), "the older need, and one more");
        // Stale: the pill names `first`, not `second`.
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

    /// THE IN-GRID STRIP CARRIES THE PILL where the window has no titlebar
    /// strip (off macOS, or a headless window): no need lays out no pill; a
    /// need behind lays its regions out and a press on its body goes to that
    /// tab; a press carrying a session the pill no longer names acts on
    /// nothing. Its re-layout follows the view, so a need that clears takes
    /// the pill out of the strip.
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
        // Stale: the drawn pill named another session.
        app.windows.get_mut(&wid).unwrap().strip_pill_session = Some(second);
        app.handle_tab_strip_click(wid, col);
        assert_eq!(app.focused_session_id(wid), Some(second), "stale: nothing");
        app.windows.get_mut(&wid).unwrap().strip_pill_session = Some(first);
        app.handle_tab_strip_click(wid, col);
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

    /// WHO DRIVES (S9): two workers whose tasks name a manager make it a
    /// manager of two and each a worker of it — the Team chip's faces, the
    /// strip marks on the titles the strip shows (and inspection reads), and
    /// a press on a worker's chip goes to the manager. A worker that closes
    /// leaves the count. CONTROL: a task the instance's own client started
    /// (`from=ctl`) makes no team, and the tab's own name keeps no mark.
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
            mark_session_task(ctx, local, &mark, &app.subscribers).expect("unfenced");
            app.on_task_changed(local);
        }
        task(&app, &solo_ctx, solo, TaskState::Working);
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
        // The strip marks ride the titles the strip shows.
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
        // A press on the worker's chip goes to the manager.
        app.note_claude_team(wid, bench);
        app.press_claude_chip(wid, bench, crate::claude_lights::Chip::Team);
        assert_eq!(app.focused_session_id(wid), Some(manager));
        // A worker that closes leaves the count (the production funnel).
        assert!(app.detach_session_view(fmt), "the last view of it");
        assert!(matches!(
            app.team_face(manager),
            Some(TeamFace::Manager { workers: 1, .. })
        ));
    }

    /// A panic inside a chrome re-grid batch ends the batch: the re-grid it
    /// had recorded is applied, and the next one is applied at once rather
    /// than recorded into a batch nobody ends.
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

    /// Inside a batch that recorded a re-grid, a chrome commit measures
    /// against the rows that re-grid gives, not the stale grid; outside one,
    /// against the grid.
    #[test]
    fn a_chrome_commit_inside_a_batch_measures_the_pending_regrid() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let size = aterm_window::dpi::PhysicalSize::new(1600, 1200);
        let gridded = app.grid_dims_for(wid, size).0;
        assert!(gridded > 2, "a window with rows to spare");
        // The band commits a row: `ws.rows` still reads the old grid.
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

    /// The story read: a person's key in a calm session moves the watermark
    /// and `status level=story` quiets.
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

    /// A PROVEN self-claim (codex-alab supervising itself) whose policy has
    /// `resume_limits` on reads a usage wall OF ITS OWN PROGRAM as aterm's own
    /// harness's does: `note`, resumes by itself, not `limited`. CONTROLS: the
    /// same wall while Claude Code is in front stays a stop under the proven
    /// claim (it speaks for codex-alab, not for whatever agent's wall is
    /// read); the claim unproven, the claimant in front over a reading made
    /// under Claude Code, a codex in a group the proven pid does not lead, and
    /// under the claimant `resume_limits = false` and the policy off (`enabled =
    /// false`), all leave the stop for a person.
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
            // The level with the wall's own story point read (a person who has
            // seen it): what the wall itself stands at.
            let level = |app: &mut App| {
                app.refresh_presence_session(sid);
                let slot = app.presence.slot(sid).expect("a refreshed slot");
                (
                    app.presence_facts(sid).unwrap().wall_answer,
                    slot.level(slot.story_seq),
                )
            };
            // Unproven: the holder's name alone proves nothing.
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
            // The claimant takes the front before the sweep reads its screen: the
            // reading standing is still Claude Code's.
            in_front(4242, "codex");
            assert_eq!(
                level(&mut app),
                (presence::WallAnswer::Nobody, Level::Limited),
                "a reading made under Claude Code is not the claimant's"
            );
            // A reading made under the claimant (no Codex reader reads walls today:
            // the stand-in names the reading's program), in its own group: its wall.
            app.session_status.stand_in_reading_program(sid, "codex");
            for claimant in ["codex-alab", "codex"] {
                in_front(4242, claimant);
                assert_eq!(
                    level(&mut app),
                    (presence::WallAnswer::Harness, Level::Note),
                    "{claimant}: resumes by itself"
                );
            }
            // A stock codex in another group (the claimant suspended) is not it.
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

    /// `status hand_pid=` names the kernel-attested taker of a live drive
    /// lease, beside an unchanged `hand=`: aterm's own pid for the harness's
    /// hand, another pid for a client that spelled aterm's holder name, and
    /// `-` for a lapsed lease, a `turn`, an unattested lease and no lease at
    /// all. (The hand lights nothing: the border and the marks read the
    /// published need alone.)
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

    /// A codex in a BACKGROUND tab reporting its own turn ended (OSC 7501
    /// `working` → `done`, folded by the sweep's gate) puts the done dot on
    /// that tab (`•`, what finished: codex-alab keeps OSC 9 inert and desktop
    /// notices are off by default) and is a story point (`status
    /// level=story`); the same end in the tab the person is looking at is
    /// not news and marks nothing. Display only: `agent=` and the need never
    /// read it. NEGATIVE CONTROLS: with the window not focused, the front
    /// tab's next turn end is news too; once someone looks at the background
    /// tab its dot goes, and the end nobody saw in the tab left behind keeps
    /// its own.
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
            // Someone looks at the window and brings A front: its dot goes.
            // B's second end, which nobody saw, is behind now and wears one.
            app.windows
                .get_mut(&wid)
                .expect("window")
                .band_on_screen_for_test = true;
            assert_eq!(app.frontmost_window, Some(wid));
            app.switch_tab_in(wid, ia);
            assert_eq!(app.focused_session_id(wid), Some(sid_a));
            assert_eq!(chips(&app), (ChipLevel::Off, ChipLevel::Done));
            // And B front again: seen, so neither tab wears one.
            app.switch_tab_in(wid, ib);
            assert_eq!(chips(&app), (ChipLevel::Off, ChipLevel::Off));
        });
    }
}

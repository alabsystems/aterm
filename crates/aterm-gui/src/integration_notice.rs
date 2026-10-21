// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tabs whose shell lost its shell-integration key say so.
//!
//! An adopted shell whose key did not survive a seamless update reads
//! `status integration=degraded`: no command blocks, exit codes or running command for that
//! tab, and it does not heal on its own. The shell fixed its key when it sourced its script and
//! scrubbed it from its environment, and macOS hides a platform binary's exec environment, so
//! only a line run by the shell, while it holds the terminal, can hand it a new key:
//!
//! * a shell spawned with the re-key channel takes the key the adoption wrote at its next
//!   prompt (`shell_rekey`): [`Heal::NextPrompt`], a per-tab record;
//! * under an agent the harness supervises and moves
//!   ([`crate::harness_host::HostHandle::rekeys_agents`]), the harness's next move types the
//!   relaunch line, which takes a one-use key (`shell_rekey::typed`): [`Heal::AgentMoved`];
//! * otherwise nothing will: [`Heal::NewTab`].
//!
//! One row per condition, never one per tab, since an update can degrade every adopted tab at
//! once. The row ([`message_reporters::integration_lost`]) names its tabs by place
//! (`App::upgrade_place`) and is:
//!
//! * restated in place as tabs join, heal, close or change remedy; a remedy change is never
//!   a `✓`;
//! * resolved `Ok` ([`message_reporters::INTEGRATION_BACK`]) only when its last tab healed,
//!   by key (`resolve_key_prefix`, which also closes as fixed a row the person put down);
//! * withdrawn when its last tab closed with the key still lost;
//! * not posted again after the person put it down while every tab it would name is one it
//!   named.
//!
//! The harness's own move is not a remedy change: it ends the agent, and the shell is in front
//! until the relaunch line heals it, so a flip from the move to a new tab is held for
//! [`AGENT_MOVE_GRACE`].
//!
//! A row carried across a seamless update is adopted, not withdrawn: a lost key is the tab's
//! fact, not the old process's (`messages_host::describes_the_old_process`). On the first word
//! its session ids ([`message_reporters::integration_lost_sessions`]) are read against this
//! process's readings: a tab still degraded keeps the row (restated, never posted twice), one
//! healed meanwhile resolves it fixed, one gone withdraws it, and one not yet read defers.
//!
//! Nothing is said before the handoff's Commit: the successor's harness host is suspended until
//! then, so its reading would offer a new tab for an agent about to be moved.
//!
//! The readings come from the status sweep (`App::observe_session_statuses`), under the same
//! terminal guard as `status integration=`, so the notice and the column cannot disagree.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use aterm_messages::Outcome;

use crate::App;
use crate::message_reporters;

/// How long a tab's remedy may read `a new tab` after it read the harness's
/// move before the row says so: the harness ends the agent to move it, and the
/// relaunch line it types at the shell's next prompt (a look a second,
/// `harness::Timings::shell_look`) heals the tab within seconds; a handoff's
/// suspended host reads as no move for its park. A minute covers both; an
/// agent the person really quit is said a minute late.
pub(crate) const AGENT_MOVE_GRACE: Duration = Duration::from_secs(60);

/// What gives a degraded tab its shell integration back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Heal {
    /// A fresh key already waits in the tab's re-key channel file: the shell takes
    /// it at its next prompt, with nothing typed.
    NextPrompt,
    /// An agent the harness supervises and moves holds the terminal: the next
    /// time the harness starts it anew — its move onto a new build, or its
    /// relaunch after a crash — types the one line that re-keys the shell.
    /// `agent` is its product name.
    AgentMoved {
        /// Its [`aterm_phase::Program::product`]: `Claude Code`, `Codex`.
        agent: &'static str,
    },
    /// Nothing will: a new tab has the integration.
    NewTab,
}

impl Heal {
    /// Whether the tab stands in the row: a remedy the person reads.
    fn in_row(self) -> bool {
        !matches!(self, Self::NextPrompt)
    }
}

/// What heals a degraded tab: its re-key channel when it has one, else the
/// harness's move of the agent in front of it (`agent`, the session's agent
/// as the host knows it, [`crate::harness_host::agent_of`]: by its name, or a
/// Claude Code started as `node` by its screen) when the harness will type
/// that move's line (`harness_moves`:
/// [`crate::harness_host::HostHandle::rekeys_agents`]), else a new tab — and
/// a new tab for agy, Muse Code and Gemini CLI, whose move no lane types. A
/// Codex is moved by no lane (the Codex screen lane's retirement,
/// 2026-10-16): only a codex-alab whose crash relaunch has a line aterm's
/// copy of its own record validated (`codex_line`) is relaunched by a line
/// aterm types; a vendor `codex`, or a codex-alab with no such line, heals in
/// a new tab — never told of a move that will not come.
#[must_use]
pub(crate) fn heal_for(
    rekey_channel: bool,
    agent: Option<aterm_phase::Program>,
    harness_moves: bool,
    codex_line: bool,
) -> Heal {
    if rekey_channel {
        return Heal::NextPrompt;
    }
    if !harness_moves {
        return Heal::NewTab;
    }
    match agent {
        Some(agent @ aterm_phase::Program::Claude) => Heal::AgentMoved {
            agent: agent.product(),
        },
        Some(agent @ aterm_phase::Program::Codex) if codex_line => Heal::AgentMoved {
            agent: agent.product(),
        },
        Some(aterm_phase::Program::Codex) => Heal::NewTab,
        Some(
            aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Gemini
            | aterm_phase::Program::Generic,
        )
        | None => Heal::NewTab,
    }
}

/// Every live session's last reading, and what the one row last said.
#[derive(Debug, Default)]
pub(crate) struct IntegrationNotices {
    /// `None`: healthy when last read. `Some(heal)`: degraded, healed by `heal`.
    seen: BTreeMap<u64, Option<Heal>>,
    /// A tab read [`Heal::AgentMoved`] now reading [`Heal::NewTab`], since
    /// when: held for [`AGENT_MOVE_GRACE`].
    flip_since: BTreeMap<u64, Instant>,
    /// The row's words moved since they were last said.
    owed: bool,
    /// A tab of the row healed since the row was last said.
    healed: bool,
    /// Tabs whose new key now waits at their next prompt: a record each.
    records: Vec<u64>,
    /// The tabs the row named when it was last posted or restated; a row gone
    /// from the glass while this holds was put down by the person.
    said: Option<BTreeSet<u64>>,
    /// The tabs a row the person put down named: not posted again while every
    /// tab it would name is among them.
    down: BTreeSet<u64>,
    /// A row the previous process carried was read against this process's
    /// readings (or there was none).
    carry_reckoned: bool,
}

impl IntegrationNotices {
    /// Take one reading of session `id` at `now` (`heal`: `None` for a healthy
    /// posture). Returns whether it moved: a session first read healthy moves
    /// nothing, first read degraded moves, and after that only a changed
    /// reading does, except a flip from the harness's move to a new tab, held
    /// for [`AGENT_MOVE_GRACE`] while the move may be in flight.
    pub(crate) fn observe(&mut self, id: u64, heal: Option<Heal>, now: Instant) -> bool {
        let was = self.seen.get(&id).copied();
        if matches!(was, Some(Some(Heal::AgentMoved { .. }))) && heal == Some(Heal::NewTab) {
            let since = *self.flip_since.entry(id).or_insert(now);
            if now.saturating_duration_since(since) < AGENT_MOVE_GRACE {
                return false;
            }
        }
        self.flip_since.remove(&id);
        let before = match was {
            Some(before) if before == heal => return false,
            Some(before) => before,
            None if heal.is_none() => {
                self.seen.insert(id, None);
                return false;
            }
            None => None,
        };
        self.seen.insert(id, heal);
        let was_in_row = before.is_some_and(Heal::in_row);
        if was_in_row && heal.is_none() {
            self.healed = true;
        }
        if heal == Some(Heal::NextPrompt) {
            self.records.push(id);
        }
        if was_in_row || heal.is_some_and(Heal::in_row) {
            self.owed = true;
        }
        true
    }

    /// Session `id` closed: its reading is forgotten. Returns whether the row's
    /// words moved (the tab stood in it).
    pub(crate) fn forget(&mut self, id: u64) -> bool {
        self.flip_since.remove(&id);
        let stood = matches!(self.seen.remove(&id), Some(Some(heal)) if heal.in_row());
        if stood {
            self.owed = true;
        }
        stood
    }

    /// Whether anything is owed a word: a moved reading, a record, or a
    /// carried row not yet reckoned.
    pub(crate) fn owes(&self) -> bool {
        self.owed || !self.records.is_empty() || !self.carry_reckoned
    }

    /// The tabs the row names, by session: every degraded tab whose remedy
    /// the person reads.
    fn in_row(&self) -> Vec<(u64, Heal)> {
        self.seen
            .iter()
            .filter_map(|(id, heal)| heal.filter(|h| h.in_row()).map(|h| (*id, h)))
            .collect()
    }
}

/// How a carried row reads against this process's readings.
enum Carried {
    /// A tab it names has a live session this process has not read yet.
    Unread,
    /// Every tab it names is read or gone; `healed`: one of them healed.
    Read { healed: bool },
}

impl App {
    /// Say what the readings now are ([`IntegrationNotices::owes`]): each
    /// tab whose key waits at its next prompt is recorded, and the one row
    /// for the others is posted, restated in place, resolved fixed or
    /// withdrawn (the module's rules).
    pub(crate) fn say_integration(&mut self) {
        if !self.integration_notices.owes() {
            return;
        }
        // Nothing is said before Commit (module docs): nothing owed is
        // cleared, so the first word after Commit says the readings.
        if self.incoming_handoff_pending {
            return;
        }
        let key = message_reporters::KEY_INTEGRATION_LOST;
        let live = self.messages.live_by_key(key).map(|l| {
            (
                l.id,
                message_reporters::integration_lost_sessions(&l.msg.detail),
            )
        });
        let mut healed_carry = false;
        if !self.integration_notices.carry_reckoned {
            match &live {
                None => self.integration_notices.carry_reckoned = true,
                Some((_, sids)) => match self.read_carried(sids) {
                    Carried::Unread => return,
                    Carried::Read { healed } => {
                        healed_carry = healed;
                        self.integration_notices.carry_reckoned = true;
                    }
                },
            }
        }
        let notices = &mut self.integration_notices;
        let records = std::mem::take(&mut notices.records);
        let healed = std::mem::take(&mut notices.healed) || healed_carry;
        notices.owed = false;
        for id in records {
            let Some(sid) = self.sid_of(id) else {
                continue;
            };
            let place = self.upgrade_place(&sid);
            let _ = self.record_message(message_reporters::integration_rekey_waits(&place, &sid));
        }
        let tabs: Vec<(u64, message_reporters::LostTab)> = self
            .integration_notices
            .in_row()
            .into_iter()
            .filter_map(|(id, heal)| {
                let sid = self.sid_of(id)?;
                let place = self.upgrade_place(&sid);
                Some((id, message_reporters::LostTab { place, sid, heal }))
            })
            .collect();
        let named: BTreeSet<u64> = tabs.iter().map(|(id, _)| *id).collect();
        let lost: Vec<message_reporters::LostTab> = tabs.into_iter().map(|(_, t)| t).collect();
        let Some(msg) = message_reporters::integration_lost(&lost) else {
            // No tab stands in it: healed resolves it fixed by key (closing a
            // put-down row too); closed with the key still lost withdraws it.
            let notices = &mut self.integration_notices;
            notices.said = None;
            notices.down.clear();
            if healed {
                if self
                    .messages
                    .resolve_key_prefix(key, Outcome::Ok, Instant::now())
                    > 0
                {
                    self.sync_messages();
                }
            } else if let Some((id, _)) = live {
                let _ = self.withdraw_message(id);
            }
            return;
        };
        if let Some((id, _)) = live {
            // The same row, restated in place (a carried one adopted in this
            // build's words); a changed remedy is not a fix.
            let _ = self.restate_message(id, crate::messages_host::restatement_of(&msg));
            let notices = &mut self.integration_notices;
            notices.said = Some(named);
            notices.down.clear();
            return;
        }
        let notices = &mut self.integration_notices;
        if let Some(said) = notices.said.take() {
            // The row this process said is gone from the glass unresolved:
            // the person put it down.
            notices.down.extend(said);
        }
        if named.is_subset(&notices.down) {
            return;
        }
        notices.down.clear();
        notices.said = Some(named);
        let _ = self.post_message(msg);
    }

    /// Session `id` closed: its reading is forgotten, and the row restated or
    /// ended without it.
    pub(crate) fn forget_integration_notice(&mut self, id: u64) {
        if self.integration_notices.forget(id) {
            self.say_integration();
        }
    }

    /// The session id of local session `id`, while it is registered.
    fn sid_of(&self, id: u64) -> Option<String> {
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        store.by_local(id).map(|h| h.sid.as_str().to_string())
    }

    /// A carried row's tabs (`sids`) against this process's readings.
    fn read_carried(&self, sids: &[String]) -> Carried {
        let locals: BTreeMap<String, u64> = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            store
                .live_handles()
                .map(|h| (h.sid.as_str().to_string(), h.local_id))
                .collect()
        };
        let mut healed = false;
        for sid in sids {
            let Some(local) = locals.get(sid) else {
                continue;
            };
            match self.integration_notices.seen.get(local) {
                None => return Carried::Unread,
                Some(None) => healed = true,
                Some(Some(_)) => {}
            }
        }
        Carried::Read { healed }
    }
}

#[cfg(test)]
mod tests {
    use super::{AGENT_MOVE_GRACE, Heal, IntegrationNotices, heal_for};
    use crate::{App, WindowId};
    use std::time::{Duration, Instant};

    /// A headless App with one stub tab whose engine is degraded as an adoption
    /// that carried no nonce leaves it (`spawn::authorize_adopted_shell_nonce`:
    /// the requirement standing, no key authorized), running `program`.
    fn degraded_tab(program: &str) -> (App, u64) {
        let mut app = App::headless_for_test();
        let sid = app.next_session_id;
        app.push_stub_tab(WindowId(0), crate::stub_session(sid));
        let pooled = app.pool.get(sid).expect("pooled");
        crate::term_lock(&pooled.term).set_require_shell_integration_nonce(true);
        front(&app, sid, program);
        (app, sid)
    }

    fn front(app: &App, sid: u64, program: &str) {
        let pooled = app.pool.get(sid).expect("pooled");
        let mut tl = pooled.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(4242);
        tl.set_program(4242, Some(program.to_string()));
    }

    /// The rows on the glass under the lost-integration key, as `(title, detail)`.
    fn rows(app: &App) -> Vec<(String, Vec<String>)> {
        app.messages
            .live_rows()
            .filter(|l| {
                l.msg
                    .key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("session.integration"))
            })
            .map(|l| (l.msg.title.clone(), l.msg.detail.clone()))
            .collect()
    }

    /// A shell spawned WITH the re-key channel heals at its next prompt by
    /// itself: a record, never a row. And a closed tab takes its row with it.
    #[test]
    fn a_channel_tab_is_a_record_and_a_closed_tab_takes_its_row() {
        let (mut app, sid) = degraded_tab("claude");
        app.store.write().unwrap().mark_rekey_channel(sid);
        let _ = app.observe_session_statuses(Instant::now());
        assert!(rows(&app).is_empty(), "a record, never a row");
        let record = app
            .messages
            .log()
            .records()
            .find(|r| {
                r.key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("session.integration."))
            })
            .expect("the record");
        assert!(record.detail[0].starts_with("in tab 2:"), "{record:?}");

        let (mut app, sid) = degraded_tab("vim");
        let _ = app.observe_session_statuses(Instant::now());
        assert_eq!(rows(&app).len(), 1);
        app.retire_session_status(sid);
        assert!(rows(&app).is_empty(), "the closed tab's row went with it");
    }

    /// A row the person put down stays down, a newly degraded tab included, and the heal that
    /// follows still closes its record as fixed.
    #[test]
    fn a_dismissed_row_stays_down_until_news_and_its_heal_still_reads_fixed() {
        let (mut app, a) = degraded_tab("vim");
        let mut t = Instant::now();
        let _ = app.observe_session_statuses(t);
        let first = app
            .messages
            .live_by_key(crate::message_reporters::KEY_INTEGRATION_LOST)
            .expect("the row")
            .id;
        assert!(app.messages.dismiss(first, t));
        t += Duration::from_millis(300);
        let _ = app.observe_session_statuses(t);
        assert!(rows(&app).is_empty(), "put down: stays down");

        let b = app.next_session_id;
        app.push_stub_tab(WindowId(0), crate::stub_session(b));
        crate::term_lock(&app.pool.get(b).expect("pooled").term)
            .set_require_shell_integration_nonce(true);
        front(&app, b, "zsh");
        t += Duration::from_millis(300);
        let _ = app.observe_session_statuses(t);
        // One row, once: a further tab that lost its key is the same loss.
        assert!(
            rows(&app).is_empty(),
            "put down: a further tab is the same loss: {:?}",
            rows(&app)
        );

        // One of its tabs heals: a tab leaving it is no news, so it stays down,
        // and the put-down row's record closes fixed.
        crate::term_lock(&app.pool.get(a).expect("pooled").term)
            .authorize_shell_integration([0x3C; 32]);
        t += Duration::from_millis(300);
        let _ = app.observe_session_statuses(t);
        assert!(rows(&app).is_empty(), "a heal is no news: {:?}", rows(&app));

        crate::term_lock(&app.pool.get(b).expect("pooled").term)
            .authorize_shell_integration([0x3C; 32]);
        t += Duration::from_millis(300);
        let _ = app.observe_session_statuses(t);
        assert!(rows(&app).is_empty());
        let record = app.messages.log().get(first).expect("the record");
        assert_eq!(
            record.title,
            crate::message_reporters::INTEGRATION_BACK,
            "{record:?}"
        );
    }

    #[test]
    fn a_channel_heals_at_the_prompt_a_moved_agent_by_its_move_anything_else_in_a_new_tab() {
        use aterm_phase::Program;
        assert_eq!(
            heal_for(true, Some(Program::Claude), true, false),
            Heal::NextPrompt
        );
        assert_eq!(
            heal_for(true, Some(Program::Claude), false, false),
            Heal::NextPrompt
        );
        assert_eq!(
            heal_for(false, Some(Program::Claude), true, false),
            Heal::AgentMoved {
                agent: "Claude Code"
            }
        );
        // A Codex: moved only as a codex-alab with a validated line; a vendor
        // `codex` (no line) heals in a new tab, never told of a move.
        assert_eq!(
            heal_for(false, Some(Program::Codex), true, true),
            Heal::AgentMoved { agent: "Codex" }
        );
        assert_eq!(
            heal_for(false, Some(Program::Codex), true, false),
            Heal::NewTab
        );
        // The harness will not type the line: a new tab.
        assert_eq!(
            heal_for(false, Some(Program::Claude), false, false),
            Heal::NewTab
        );
        assert_eq!(
            heal_for(false, Some(Program::Codex), false, true),
            Heal::NewTab
        );
        assert_eq!(
            heal_for(false, Some(Program::Generic), true, false),
            Heal::NewTab
        );
        assert_eq!(heal_for(false, None, true, false), Heal::NewTab);
        // No lane types agy's or Muse Code's move: a new tab.
        assert_eq!(
            heal_for(false, Some(Program::Agy), true, false),
            Heal::NewTab
        );
        assert_eq!(
            heal_for(false, Some(Program::Muse), true, false),
            Heal::NewTab
        );
    }

    /// A CLAUDE CODE STARTED AS `node` HEALS WITH ITS MOVE: the heal asked the
    /// published program's NAME, so a `node` the host supervises by its screen
    /// (`harness_host::agent_of`) was told "a new tab" though the live upgrade
    /// moves it in the same tab. It now asks the agent as the host knows it.
    /// NEGATIVE CONTROL: a `node` whose screen named no agent is a new tab.
    #[test]
    fn a_node_hosted_agent_the_host_supervises_heals_with_its_move() {
        use crate::session_timeline::AgentPublication;
        use aterm_phase::Program;
        let node = |reader: Option<Program>| {
            crate::harness_host::agent_of(&AgentPublication {
                program: Some("node".to_string()),
                reader,
                ..AgentPublication::default()
            })
        };
        assert_eq!(
            heal_for(false, node(Some(Program::Claude)), true, false),
            Heal::AgentMoved {
                agent: "Claude Code"
            }
        );
        assert_eq!(heal_for(false, node(None), true, false), Heal::NewTab);
    }

    #[test]
    fn only_a_changed_reading_moves_and_the_moves_own_flip_is_held() {
        let t = Instant::now();
        let mut n = IntegrationNotices::default();
        assert!(!n.observe(1, None, t), "first read healthy: nothing to say");
        assert!(!n.observe(1, None, t));
        assert!(n.observe(2, Some(Heal::NewTab), t), "first read degraded");
        assert!(
            !n.observe(2, Some(Heal::NewTab), t),
            "the same reading again"
        );
        let agent = Some(Heal::AgentMoved {
            agent: "Claude Code",
        });
        assert!(n.observe(2, agent, t), "its program was only now resolved");
        // The agent left: held while the harness's move may be in flight…
        assert!(!n.observe(2, Some(Heal::NewTab), t));
        assert!(!n.observe(2, Some(Heal::NewTab), t + AGENT_MOVE_GRACE / 2));
        // …and said once the grace is spent.
        assert!(n.observe(2, Some(Heal::NewTab), t + AGENT_MOVE_GRACE));
        assert!(n.observe(2, None, t + AGENT_MOVE_GRACE), "healed");
        assert!(n.healed);
        assert!(!n.observe(2, None, t + AGENT_MOVE_GRACE));
        assert!(
            n.observe(1, Some(Heal::NextPrompt), t),
            "a later loss moves too"
        );
        assert_eq!(n.records, vec![1]);
    }
}

/// App-level tests, through the sweep and the message center only.
#[cfg(test)]
#[path = "integration_notice_review_tests.rs"]
mod review_tests;

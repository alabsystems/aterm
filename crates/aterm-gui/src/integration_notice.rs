// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TABS THAT LOST THEIR SHELL INTEGRATION SAY SO (2026-09-26; reworked to
//! main's messages rulings 2026-09-29).
//!
//! The owner, on 0.93.0, a Claude Code tab that read `status integration=degraded
//! path=frozen` — an adopted shell whose seamless update could not carry its
//! shell-integration key: *"I don't see the aterm claude harness reacting"*. The
//! loss itself was invisible: `status` and `aterm.log` named it, and nothing a
//! person reads did. Its cost is real — no command blocks, no exit codes, no
//! running command for that tab — and it does not end on its own: the shell
//! fixed its key when it sourced its script, unset it from its environment
//! before any child ran, and macOS hides a platform binary's exec environment
//! (`session_program`, measured 2026-09-24), so nothing outside the shell can
//! learn the key it still signs with. Only a line run BY the shell can hand it a
//! new one, and only while the shell holds the terminal:
//!
//! * a shell spawned with the re-key channel takes the key the adoption wrote at
//!   its next prompt (`shell_rekey`) — [`Heal::NextPrompt`], a per-tab RECORD;
//! * under an agent the harness supervises and MOVES (`[harness] upgrade` or
//!   `relaunch`, [`crate::harness_host::HostHandle::rekeys_agents`]), its next
//!   move of that agent types the relaunch line at the shell's prompt, and that
//!   line takes a one-use key (`shell_rekey::typed`) — [`Heal::AgentMoved`];
//! * otherwise nothing will — [`Heal::NewTab`].
//!
//! ONE ROW PER CONDITION, never one per tab (the owner of 2026-09-28: "also fix
//! this stacking failures of claude upgrades!"; ruling 374): an update from a
//! build without the re-key channel degrades every adopted tab at once, and N
//! identical rows would stack. The row (`message_reporters::integration_lost`,
//! [`message_reporters::KEY_INTEGRATION_LOST`]) counts and names the tabs by
//! their PLACE (`App::upgrade_place`, rulings 270 and 300 — the session id only
//! on its technical line), and it is:
//!
//! * RESTATED in place as tabs join, heal, close or change remedy — a problem
//!   that changed was not fixed (ruling 266), so a remedy change is never a `✓`;
//! * RESOLVED `Ok` under its declared fix words
//!   ([`message_reporters::INTEGRATION_BACK`]) only when its last tab HEALED,
//!   by key (`resolve_key_prefix`, which also closes as fixed a row the person
//!   put down before the fix, ruling 367);
//! * WITHDRAWN when its last tab closed with the key still lost;
//! * never posted again after the person put it down while every tab it would
//!   name is one it named (a tab that newly lost its key is news).
//!
//! THE HARNESS'S OWN MOVE IS NOT A REMEDY CHANGE: to move an agent the harness
//! ends it, the shell is in front for the seconds before the relaunch line
//! heals it, and a handoff suspends the host meanwhile. A tab whose remedy goes
//! from the agent's move to a new tab holds its reading for
//! [`AGENT_MOVE_GRACE`]; the heal that follows ends the row with no `a new tab`
//! words said and no intermediate `✓`.
//!
//! A ROW CARRIED ACROSS A SEAMLESS UPDATE IS ADOPTED, not withdrawn
//! (`messages_host::describes_the_old_process` is for facts the successor makes
//! untrue by existing — a session wait, the old process's accessibility
//! publisher; a lost shell key is the TAB's, and the tab crosses with the row).
//! On this process's first word the row's own session ids
//! ([`message_reporters::integration_lost_sessions`]) are read against this
//! process's readings: a tab still degraded keeps the row (restated in this
//! build's words, never posted twice — no `×2`), one that healed meanwhile
//! resolves it fixed, and one whose session is gone withdraws it. A tab this
//! process has not read yet defers the reckoning.
//!
//! NOTHING IS SAID BEFORE COMMIT: the parent still owns the tabs, and the
//! successor's harness host is suspended until the Commit resumes it, so a
//! successor's reading of an agent tab would offer a new tab for the agent
//! the harness is about to move. The readings are taken and kept; the first
//! word after Commit says them.
//!
//! The reading is the status sweep's (`App::observe_session_statuses`): the
//! posture from the SAME terminal guard `status integration=` is read under, so
//! the notice and the column cannot disagree, and a session whose posture is
//! healthy never touches the store. Only a MOVED reading (or an unreckoned
//! carried row) costs a word.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use aterm_messages::Outcome;

use crate::App;
use crate::message_reporters;

/// How long a tab's remedy may read `a new tab` after it read the harness's
/// move before the row says so: the harness ends the agent to move it, and the
/// relaunch line it types at the shell's next prompt (a look a second,
/// `relaunch::SHELL_LOOK`) heals the tab within seconds; a handoff's suspended
/// host reads as no move for its park. A minute covers both; an agent the
/// person really quit is said a minute late.
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
/// a new tab for agy and Muse Code, whose move no lane types.
#[must_use]
pub(crate) fn heal_for(
    rekey_channel: bool,
    agent: Option<aterm_phase::Program>,
    harness_moves: bool,
) -> Heal {
    if rekey_channel {
        return Heal::NextPrompt;
    }
    if !harness_moves {
        return Heal::NewTab;
    }
    match agent {
        Some(agent @ (aterm_phase::Program::Claude | aterm_phase::Program::Codex)) => {
            Heal::AgentMoved {
                agent: agent.product(),
            }
        }
        Some(
            aterm_phase::Program::Agy | aterm_phase::Program::Muse | aterm_phase::Program::Generic,
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
    /// posture). Returns whether it MOVED: a session first read healthy moves
    /// nothing, first read degraded moves, and after that only a changed
    /// reading does — except a flip from the harness's move to a new tab, held
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
    /// tab whose key waits at its next prompt is recorded, and the ONE row
    /// for the others is posted, restated in place, resolved fixed or
    /// withdrawn (the module's rules).
    pub(crate) fn say_integration(&mut self) {
        if !self.integration_notices.owes() {
            return;
        }
        // BEFORE COMMIT NOTHING IS SAID (verifier, 2026-09-29): the parent
        // still owns the tabs, and this process's harness host starts
        // SUSPENDED until the Commit resumes it (`HostHandle::resume_after_handoff`),
        // so a reading taken now offers `a new tab` for the very agent the
        // harness is about to supervise and move — the update that degraded
        // the tabs is exactly when the successor first reads them. The
        // readings keep (nothing owed is cleared); the first word after
        // Commit says them as they then stand.
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
            // No tab stands in it: healed — fixed, by key, which also closes
            // as fixed a row the person put down before the fix (ruling 367)
            // — or closed with the key still lost: withdrawn, never a `✓`.
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
            // build's words): a tab that joined, healed, closed or changed its
            // remedy is not a fix (ruling 266).
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

    /// A headless App with one stub tab whose engine is DEGRADED exactly as an
    /// adoption that carried no nonce leaves it (`spawn::authorize_adopted_shell_nonce`:
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

    /// THE OWNER'S TAB, with no harness to move its agent (a headless test App
    /// has none): ONE row that names the tab by its place, the loss and the
    /// remedy that applies — a new tab — and nothing more while nothing
    /// changes; when the shell signs with a key the engine knows, the row
    /// leaves. NEGATIVE CONTROL: the same tab before its engine lost the key
    /// says nothing. (Updated 2026-09-29: the branch's first cut offered the
    /// harness's move here with no harness at all, and named the tab by its
    /// session id; the move's remedy is pinned in `review_tests`.)
    #[test]
    fn a_degraded_tab_says_what_gives_it_back_and_the_row_leaves_when_it_heals() {
        let mut healthy = App::headless_for_test();
        let hid = healthy.next_session_id;
        healthy.push_stub_tab(WindowId(0), crate::stub_session(hid));
        front(&healthy, hid, "claude");
        let _ = healthy.observe_session_statuses(Instant::now());
        assert!(rows(&healthy).is_empty(), "a healthy tab says nothing");

        let (mut app, sid) = degraded_tab("claude");
        let mut t = Instant::now();
        let _ = app.observe_session_statuses(t);
        let said = rows(&app);
        assert_eq!(said.len(), 1, "one row for the tab: {said:?}");
        assert_eq!(said[0].0, "Tab lost its shell integration");
        // The headless App opens with a tab of its own: the stub is tab 2.
        assert!(
            said[0].1[0].starts_with("in tab 2:")
                && said[0].1[0].contains("no command blocks or exit codes"),
            "{said:?}"
        );
        assert!(
            said[0].1[1].starts_with("open a new tab"),
            "no harness moves the agent: {said:?}"
        );

        t += Duration::from_millis(300);
        let _ = app.observe_session_statuses(t);
        assert_eq!(rows(&app), said, "an unchanged reading says nothing new");

        // The shell signs with a key the engine knows: healed, the row leaves.
        {
            let pooled = app.pool.get(sid).expect("pooled");
            crate::term_lock(&pooled.term).authorize_shell_integration([0x3C; 32]);
        }
        t += Duration::from_millis(300);
        let _ = app.observe_session_statuses(t);
        assert!(rows(&app).is_empty(), "healed: {:?}", rows(&app));
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

    /// A ROW THE PERSON PUT DOWN stays down, a tab that newly lost its key
    /// included (the once-only rule: one row for the loss, not one per tab);
    /// and the heal that follows closes the put-down row's record as FIXED
    /// (ruling 367: a fix reaches a problem that already left the glass).
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
        // ONE ROW, ONCE (the exceptions design, section 2): the loss is put
        // down, and a further tab that lost its key is the same news for the
        // day the once-only rule keeps it quiet, never a second row.
        assert!(
            rows(&app).is_empty(),
            "put down: a further tab is the same loss: {:?}",
            rows(&app)
        );

        // One of its tabs heals: a tab leaving it is no news — it stays down,
        // and the put-down row's record closes FIXED.
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
            heal_for(true, Some(Program::Claude), true),
            Heal::NextPrompt
        );
        assert_eq!(
            heal_for(true, Some(Program::Claude), false),
            Heal::NextPrompt
        );
        assert_eq!(
            heal_for(false, Some(Program::Claude), true),
            Heal::AgentMoved {
                agent: "Claude Code"
            }
        );
        assert_eq!(
            heal_for(false, Some(Program::Codex), true),
            Heal::AgentMoved { agent: "Codex" }
        );
        // The harness will not type the line: a new tab.
        assert_eq!(heal_for(false, Some(Program::Claude), false), Heal::NewTab);
        assert_eq!(heal_for(false, Some(Program::Generic), true), Heal::NewTab);
        assert_eq!(heal_for(false, None, true), Heal::NewTab);
        // No lane types agy's or Muse Code's move: a new tab.
        assert_eq!(heal_for(false, Some(Program::Agy), true), Heal::NewTab);
        assert_eq!(heal_for(false, Some(Program::Muse), true), Heal::NewTab);
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
            heal_for(false, node(Some(Program::Claude)), true),
            Heal::AgentMoved {
                agent: "Claude Code"
            }
        );
        assert_eq!(
            heal_for(false, node(Some(Program::Codex)), true),
            Heal::AgentMoved { agent: "Codex" }
        );
        assert_eq!(heal_for(false, node(None), true), Heal::NewTab);
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

/// The review of 2026-09-29, one test per finding — App-level, through the
/// sweep and the message center only, so each ran against the branch's first
/// cut too (where each failed).
#[cfg(test)]
#[path = "integration_notice_review_tests.rs"]
mod review_tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shell-integration notices ([`Heal::NextPrompt`], [`message_reporters::integration_lost`],
//! [`message_reporters::integration_lost_sessions`], [`message_reporters::INTEGRATION_BACK`],
//! [`crate::harness_host::HostHandle::rekeys_agents`]).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use aterm_messages::Outcome;

use crate::App;
use crate::message_reporters;

/// Grace period before a remedy transitions from [`Heal::AgentMoved`] to [`Heal::NewTab`].
pub(crate) const AGENT_MOVE_GRACE: Duration = Duration::from_secs(60);

/// What gives a degraded tab its shell integration back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Heal {
    /// A fresh key already waits in the tab's re-key channel file.
    NextPrompt,
    /// An agent the harness supervises and moves holds the terminal.
    AgentMoved {
        /// Its [`aterm_phase::Program::product`]: `Claude Code`, `Codex`.
        agent: &'static str,
    },
    /// Nothing will: a new tab has the integration.
    NewTab,
}

impl Heal {
    /// Whether the tab stands in the row: a remedy the person reads.
    const fn in_row(self) -> bool {
        !matches!(self, Self::NextPrompt)
    }
}

/// Remedy for a degraded tab ([`crate::harness_host::agent_of`]).
#[must_use]
pub(crate) const fn heal_for(
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
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Generic,
        )
        | None => Heal::NewTab,
    }
}

/// Every live session's last reading, and what the one row last said.
#[derive(Debug, Default)]
pub(crate) struct IntegrationNotices {
    /// `None`: healthy when last read.
    seen: BTreeMap<u64, Option<Heal>>,
    /// Since when a [`Heal::AgentMoved`] tab read [`Heal::NewTab`] ([`AGENT_MOVE_GRACE`]).
    flip_since: BTreeMap<u64, Instant>,
    /// The row's words moved since they were last said.
    owed: bool,
    /// A tab of the row healed since the row was last said.
    healed: bool,
    /// Tabs whose new key now waits at their next prompt: a record each.
    records: Vec<u64>,
    /// The tabs the row named when it was last posted or restated.
    said: Option<BTreeSet<u64>>,
    /// The tabs a row the person put down named.
    down: BTreeSet<u64>,
    /// A row the previous process carried was read against this process's readings.
    carry_reckoned: bool,
}

impl IntegrationNotices {
    /// Record a posture reading for `id` at `now` ([`AGENT_MOVE_GRACE`]).
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

    /// Session `id` closed: its reading is forgotten.
    pub(crate) fn forget(&mut self, id: u64) -> bool {
        self.flip_since.remove(&id);
        let stood = matches!(self.seen.remove(&id), Some(Some(heal)) if heal.in_row());
        if stood {
            self.owed = true;
        }
        stood
    }

    /// Whether anything is owed a word.
    pub(crate) const fn owes(&self) -> bool {
        self.owed || !self.records.is_empty() || !self.carry_reckoned
    }

    /// The tabs the row names, by session: every degraded tab whose remedy the person reads.
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
    /// Say what the readings now are ([`IntegrationNotices::owes`]).
    pub(crate) fn say_integration(&mut self) {
        if !self.integration_notices.owes() {
            return;
        }
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

    /// Session `id` closed: its reading is forgotten, and the row restated or ended without it.
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
#[path = "integration_notice_tests.rs"]
mod tests;

/// App-level tests, through the sweep and the message center only.
#[cfg(test)]
#[path = "integration_notice_review_tests.rs"]
mod review_tests;

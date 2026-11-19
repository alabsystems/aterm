// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Addressable terminal sessions hosted by this process and their persistent shortcut numbers.

use std::collections::BTreeMap;

use aterm_session::SessionId;

use crate::session_store::SessionState;
use crate::tab_model::View;
use crate::{App, WindowId};

#[derive(Default)]
pub(crate) struct LocalPanelRoster {
    /// The snapshot includes the latest changes to the process's windows and sessions.
    current: bool,
    layout_pending: bool,
    /// Only the registry's current local address of each SID can be activated.
    sessions: BTreeMap<u64, SessionId>,
    numbers: [Option<u64>; 9],
}

impl LocalPanelRoster {
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.sessions.len()
    }

    pub(crate) fn request_layout(&mut self) {
        self.layout_pending = true;
    }

    pub(crate) fn take_layout_request(&mut self) -> bool {
        std::mem::take(&mut self.layout_pending)
    }

    pub(crate) fn number(&self, session: u64) -> Option<u8> {
        self.numbers
            .iter()
            .position(|slot| *slot == Some(session))
            .map(|index| index as u8 + 1)
    }

    pub(super) fn invalidate(&mut self) {
        self.current = false;
    }

    /// Keep each shortcut with its SID. Newcomers and formerly unnumbered survivors take
    /// vacated positions in process creation order, independently of the order of tabs.
    fn reconcile(&mut self, now: &BTreeMap<u64, &SessionId>) -> bool {
        self.current = true;
        if self.sessions.len() == now.len()
            && now
                .iter()
                .all(|(id, sid)| self.sessions.get(id) == Some(*sid))
        {
            return false;
        }
        for slot in &mut self.numbers {
            if let Some(id) = *slot
                && let Some(sid) = self.sessions.get(&id)
                && now.get(&id).copied() != Some(sid)
            {
                *slot = now
                    .iter()
                    .find_map(|(&id, &held)| (held == sid).then_some(id));
            }
        }
        for &id in now.keys() {
            if self.numbers.contains(&Some(id)) {
                continue;
            }
            let Some(free) = self.numbers.iter_mut().find(|slot| slot.is_none()) else {
                break;
            };
            *free = Some(id);
        }
        self.request_layout();
        self.sessions = now.iter().map(|(&id, &sid)| (id, sid.clone())).collect();
        true
    }
}

impl App {
    /// Read terminal leaves (shown and parked) against this process's live registry. Mirrors
    /// share the canonical local address. A prewarmed shell has no leaf, a utility has no SID.
    pub(crate) fn refresh_local_panel_roster(&mut self) -> bool {
        if self.local_panel_roster.current {
            return false;
        }
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        let mut now = BTreeMap::new();
        for window in self.windows.values() {
            for tab in window.tab_set.tabs() {
                tab.root.visit(&mut |&view| {
                    if let Some(session) = self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(View::terminal_session)
                        && let Some(handle) = store.by_local(session)
                        && handle.local_id == session
                        && handle.state != SessionState::Exited
                    {
                        now.insert(session, &handle.sid);
                    }
                });
            }
        }
        let before = self.local_panel_roster.numbers;
        let changed = self.local_panel_roster.reconcile(&now);
        if changed && let Some(nav) = self.nav.as_mut() {
            for (old, new) in before.into_iter().zip(self.local_panel_roster.numbers) {
                if old != new {
                    nav.rows.extend(old);
                    nav.rows.extend(new);
                }
            }
        }
        changed
    }

    /// Resolve a displayed number against the current roster, also between a tab change and
    /// the next publication. A hidden session opens read-only in the receiving window.
    pub(crate) fn activate_local_panel_number(&mut self, wid: WindowId, number: u8) -> bool {
        self.refresh_local_panel_roster();
        let session = number
            .checked_sub(1)
            .and_then(|index| self.local_panel_roster.numbers.get(usize::from(index)))
            .copied()
            .flatten();
        session.is_some_and(|session| self.activate_local_panel_session(wid, session))
    }

    pub(crate) fn activate_local_panel_session(&mut self, wid: WindowId, session: u64) -> bool {
        self.refresh_local_panel_roster();
        let Some(sid) = self.local_panel_roster.sessions.get(&session).cloned() else {
            return false;
        };
        self.activate_local_panel_sid(wid, &sid)
    }

    /// A SID from a foreign instance or a retired record can never focus a coincident local ID.
    pub(crate) fn activate_local_panel_sid(&mut self, wid: WindowId, sid: &SessionId) -> bool {
        self.refresh_local_panel_roster();
        if !self.windows.contains_key(&wid) {
            return false;
        }
        let session = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(sid)
            .filter(|handle| handle.state != SessionState::Exited)
            .map(|handle| handle.local_id);
        let Some(session) = session else {
            return false;
        };
        if self.local_panel_roster.sessions.get(&session) != Some(sid) {
            return false;
        }
        self.select_dock_session(wid, session);
        self.focused_session_id(wid) == Some(session)
    }
}

#[cfg(test)]
#[path = "app_local_panel_roster_tests.rs"]
mod tests;

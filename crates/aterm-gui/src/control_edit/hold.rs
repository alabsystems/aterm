// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which sessions an open `edit` ticket holds (amacs design §6.7).
//!
//! While a session's program waits on an edit, what is typed into that session
//! would queue in its TTY and reach the program only after the editor closes.
//! The map names the ticket and view holding each such session: `app_edit` sets
//! it when a ticket's view opens, starts its expiry when the last waiter leaves
//! (an orphan clears itself five seconds later with no main-thread timer) and
//! clears it when the ticket resolves. Slice N4 refuses typed input against it.
//!
//! The lock is a leaf, named `edit_hold` at every acquisition: nothing else
//! is acquired while it is held, and every function here takes and drops it
//! within its own body.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

use amacs::protocol::edit::Ticket;
use aterm_types::MutexExt as _;

/// The ticket and view holding one session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Hold {
    pub(crate) ticket: Ticket,
    /// The ticket's view (`inspect app/v1 view <view>`).
    pub(crate) view: u64,
}

#[derive(Debug)]
struct Entry {
    hold: Hold,
    /// When an orphaned ticket's hold lapses; `None` while a waiter is parked
    /// or none has had the chance to arrive.
    expires: Option<Instant>,
}

static EDIT_HOLD: Mutex<BTreeMap<u64, Entry>> = Mutex::new(BTreeMap::new());

/// The map's lock, a leaf (see the module doc). Callers lock it themselves
/// (`edit_hold().lock_or_recover()`), so every acquisition carries the name.
fn edit_hold() -> &'static Mutex<BTreeMap<u64, Entry>> {
    &EDIT_HOLD
}

/// Session `session` is held by `hold`, until cleared; the latest ticket wins.
pub(crate) fn set(session: u64, hold: Hold) {
    edit_hold().lock_or_recover().insert(
        session,
        Entry {
            hold,
            expires: None,
        },
    );
}

/// Ticket `ticket` no longer holds `session`. Another ticket's hold stays.
pub(crate) fn clear(session: u64, ticket: Ticket) {
    let mut map = edit_hold().lock_or_recover();
    if map
        .get(&session)
        .is_some_and(|entry| entry.hold.ticket == ticket)
    {
        map.remove(&session);
    }
}

/// `ticket`'s hold on `session` lapses at `expires` (`None`: it stands).
pub(crate) fn set_expiry(session: u64, ticket: Ticket, expires: Option<Instant>) {
    if let Some(entry) = edit_hold()
        .lock_or_recover()
        .get_mut(&session)
        .filter(|entry| entry.hold.ticket == ticket)
    {
        entry.expires = expires;
    }
}

/// What holds `session` now, if anything: a lapsed hold is dropped here.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "slice N4 refuses typed input against the hold")
)]
pub(crate) fn get(session: u64) -> Option<Hold> {
    get_at(session, Instant::now())
}

/// [`get`] as of `now`.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "slice N4 refuses typed input against the hold")
)]
pub(crate) fn get_at(session: u64, now: Instant) -> Option<Hold> {
    let mut map = edit_hold().lock_or_recover();
    let lapsed = map
        .get(&session)
        .is_some_and(|entry| entry.expires.is_some_and(|at| at <= now));
    if lapsed {
        map.remove(&session);
        return None;
    }
    map.get(&session).map(|entry| entry.hold)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    // Sessions far above any a test App mints, so this test shares the
    // process-wide map with the App tests without meeting their entries.
    const SESSION: u64 = u64::MAX - 7;

    #[test]
    fn edit_hold_is_set_cleared_by_its_own_ticket_and_lapses_at_expiry() {
        let first = Hold {
            ticket: Ticket(1),
            view: 10,
        };
        let second = Hold {
            ticket: Ticket(2),
            view: 20,
        };
        set(SESSION, first);
        assert_eq!(get(SESSION), Some(first));
        set(SESSION, second);
        clear(SESSION, first.ticket);
        assert_eq!(get(SESSION), Some(second), "only its own ticket clears it");

        let now = Instant::now();
        set_expiry(SESSION, first.ticket, Some(now));
        assert_eq!(
            get_at(SESSION, now),
            Some(second),
            "another ticket's expiry"
        );
        set_expiry(SESSION, second.ticket, Some(now + Duration::from_secs(5)));
        assert_eq!(get_at(SESSION, now), Some(second), "not lapsed yet");
        set_expiry(SESSION, second.ticket, None);
        assert_eq!(
            get_at(SESSION, now + Duration::from_secs(60)),
            Some(second),
            "a returned waiter stops the clock"
        );
        set_expiry(SESSION, second.ticket, Some(now + Duration::from_secs(5)));
        assert_eq!(get_at(SESSION, now + Duration::from_secs(5)), None);
        assert_eq!(get(SESSION), None, "a lapsed hold is gone");
        clear(SESSION, second.ticket);
    }
}

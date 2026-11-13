// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which sessions an open `edit` ticket holds, and the refusal that enforces it (amacs design §6.7; slices N1 and N4).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

use amacs::protocol::edit::Ticket;
use aterm_types::MutexExt as _;
use aterm_types::control_verbs::input_of;

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

/// Each held session's holds, in the order their tickets first held it.
type Holds = BTreeMap<u64, Vec<Entry>>;

static EDIT_HOLD: Mutex<Holds> = Mutex::new(BTreeMap::new());

/// The map's lock, a leaf: nothing else is acquired while it is held. Callers lock it themselves
/// (`edit_hold().lock_or_recover()`), so every acquisition carries the name.
fn edit_hold() -> &'static Mutex<Holds> {
    &EDIT_HOLD
}

/// The session verbs a held session refuses.
pub(crate) fn holds_verb(verb: &str) -> bool {
    matches!(
        verb,
        "send"
            | "key"
            | "ctrl"
            | "feed"
            | "feed-bin"
            | "mouse"
            | "paste"
            | "paste-bin"
            | "turn"
            | "focus"
    )
}

/// Ticket `hold.ticket` holds `session` until cleared.
pub(crate) fn set(session: u64, hold: Hold) {
    let mut holds = edit_hold().lock_or_recover();
    let entries = holds.entry(session).or_default();
    match entries
        .iter_mut()
        .find(|entry| entry.hold.ticket == hold.ticket)
    {
        Some(entry) => {
            entry.hold = hold;
            entry.expires = None;
        }
        None => entries.push(Entry {
            hold,
            expires: None,
        }),
    }
}

/// Ticket `ticket` no longer holds `session`.
pub(crate) fn clear(session: u64, ticket: Ticket) {
    let mut holds = edit_hold().lock_or_recover();
    if let Some(entries) = holds.get_mut(&session) {
        entries.retain(|entry| entry.hold.ticket != ticket);
        if entries.is_empty() {
            holds.remove(&session);
        }
    }
}

/// `ticket`'s hold on `session` lapses at `expires` (`None`: it stands).
pub(crate) fn set_expiry(session: u64, ticket: Ticket, expires: Option<Instant>) {
    if let Some(entry) = edit_hold()
        .lock_or_recover()
        .get_mut(&session)
        .and_then(|entries| entries.iter_mut().find(|entry| entry.hold.ticket == ticket))
    {
        entry.expires = expires;
    }
}

/// What holds `session` now, if anything.
pub(crate) fn get(session: u64) -> Option<Hold> {
    get_at(session, Instant::now())
}

/// `get` as of `now`.
pub(crate) fn get_at(session: u64, now: Instant) -> Option<Hold> {
    let mut holds = edit_hold().lock_or_recover();
    live(&mut holds, session, now)
        .last()
        .map(|entry| entry.hold)
}

/// How many tickets hold `session` now (`status editing=<n>`).
pub(crate) fn count(session: u64) -> usize {
    count_at(session, Instant::now())
}

/// `count` as of `now`.
pub(crate) fn count_at(session: u64, now: Instant) -> usize {
    let mut holds = edit_hold().lock_or_recover();
    live(&mut holds, session, now).len()
}

/// The holds standing at `now` through one of `views`, as `(session, ticket)` pairs ascending.
pub(crate) fn holds_through(views: &[u64], now: Instant) -> Vec<(u64, Ticket)> {
    let mut holds = edit_hold().lock_or_recover();
    let sessions: Vec<u64> = holds.keys().copied().collect();
    let mut through: Vec<(u64, Ticket)> = Vec::new();
    for session in sessions {
        through.extend(
            live(&mut holds, session, now)
                .iter()
                .filter(|entry| views.contains(&entry.hold.view))
                .map(|entry| (session, entry.hold.ticket)),
        );
    }
    drop(holds);
    through.sort_unstable();
    through
}

/// `session`'s holds standing at `now`, the lapsed ones dropped from the map.
fn live(sessions: &mut BTreeMap<u64, Vec<Entry>>, session: u64, now: Instant) -> &[Entry] {
    let Some(entries) = sessions.get_mut(&session) else {
        return &[];
    };
    entries.retain(|entry| entry.expires.is_none_or(|at| at > now));
    if entries.is_empty() {
        sessions.remove(&session);
        return &[];
    }
    sessions.get(&session).map_or(&[], Vec::as_slice)
}

/// The answer to `verb` (argument tail `rest`) while an edit holds the session it types into.
pub(crate) fn refusal(
    verb: &str,
    rest: &str,
    session: Option<u64>,
    front: Option<u64>,
) -> Option<String> {
    let typed_into = if holds_verb(verb) {
        session
    } else if front.is_some()
        && crate::input_stall::writes_input(input_of(verb), verb, rest.as_bytes())
        && !reads_the_pointer(verb, rest)
    {
        front
    } else {
        None
    };
    let hold = get(typed_into?)?;
    Some(format!(
        "ERR busy editing ticket={} view={}\n",
        hold.ticket, hold.view
    ))
}

/// `pointer` or `pointer status` (`control_input::parse_pointer`'s
/// `PointerAction::Status`): a read of where the pointer is, which stays
/// answerable as every read does.
fn reads_the_pointer(verb: &str, rest: &str) -> bool {
    verb == "pointer" && matches!(rest.split_whitespace().next(), None | Some("status"))
}

#[cfg(test)]
#[path = "hold_tests.rs"]
mod tests;

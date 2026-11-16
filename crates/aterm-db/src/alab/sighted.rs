// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where and when each account was sighted (`docs/DESIGN-account-discovery-2026-11-09.md`): the
//! one fold of every Mac's `account-obs` registry rows and the journal's `seen` events, for
//! every reader that says where an account was seen: `aterm db ai-accounts --registry`
//! (`super::command::views`) and the accounts relation of the fleet tables (`crate::fleet`).
//! Both read one answer from the same records, each naming a Mac its own way.
//!
//! Pure: it reads no file and no clock. A Mac is the Commons machine uuid (32 hex) that wrote
//! the row or logged the event. A sighting is a login file that names the account on that Mac:
//! a tab running on it (a `seen` that names its tab, an `account-obs` row that says `running`)
//! or a login place the machine sweep found (a `seen` that names no tab, a row that says
//! `idle`). It is no sign-in word: a login file names its account long after the login ended
//! (`journal::Event::signin`). A row that says `signed-out` is the opposite of a sighting: its
//! Mac's login files stopped naming the account, and it dates none.

use crate::typed::{keys, owned::OwnedRead};
use aterm_json::Value;
use std::collections::BTreeMap;

/// How long a sighting keeps an account running on a Mac: two of a running tab's hourly `seen`
/// refreshes (`docs/DESIGN-account-discovery-2026-11-09.md`, "In use on <mac>").
pub const SEEN_LIVE_MS: u64 = 2 * 3_600_000;

/// The clock-skew bound, in milliseconds: the claims ring reads a Mac's clock as split past 15
/// minutes. A record dated further than this ahead of its read is evidence of nothing at that
/// read, a sighting as much as a refill or a plan word (`journal::seen`).
pub const SKEW_MS: u64 = 900_000;

/// What an `account-obs` row says of the account on its Mac (`observe::STATES`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObsState {
    /// A tab runs it (`running`).
    Running,
    /// A login place names it and no tab runs it (`idle`).
    Idle,
    /// Its login files stopped naming it (`signed-out`): no sighting, and the end of that Mac's
    /// earlier ones.
    SignedOut,
}

/// One Mac's `account-obs` row, as far as a sighting reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Obs {
    pub account: String,
    /// The Commons uuid (32 hex) of the Mac that wrote it.
    pub uuid: String,
    /// When it was last written, Unix milliseconds.
    pub at_ms: u64,
    pub state: ObsState,
    /// How many tabs it says run it; 0 unless it says `running`.
    pub tabs: u64,
}

/// The `account-obs` rows of `read` that are a Mac's word of an account. A row whose key does
/// not end in a uuid, or whose prefix is not the account it names, is no Mac's word and is
/// skipped, as the collection's writers rule admits only its signer's own suffix. So is a row
/// that does not say what a sighting reads of it (the owned-row reader holds a row to its size,
/// not its columns): a state this build knows, when it was written and, where it says a tab
/// runs the account, how many. No default stands in for a cell the row lacks.
pub fn obs_rows(read: &OwnedRead) -> Vec<Obs> {
    read.rows
        .iter()
        .filter_map(|(key, row)| {
            let row = &row.value;
            let account = row.get("account").and_then(Value::as_str)?;
            let (prefix, uuid) = key.rsplit_once('.')?;
            if prefix != account || keys::parse_uuid32(uuid).is_none() {
                return None;
            }
            let state = match row.get("state").and_then(Value::as_str)? {
                "running" => ObsState::Running,
                "idle" => ObsState::Idle,
                "signed-out" => ObsState::SignedOut,
                _ => return None,
            };
            Some(Obs {
                account: account.to_owned(),
                uuid: uuid.to_owned(),
                at_ms: row.get("at").and_then(Value::as_u64)?,
                state,
                tabs: match state {
                    ObsState::Running => row.get("tabs").and_then(Value::as_u64)?,
                    ObsState::Idle | ObsState::SignedOut => 0,
                },
            })
        })
        .collect()
}

/// One journal event a sighting reads: a `seen`, or the `session-end` that closes a tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heard {
    /// The writer's Commons uuid, 32 hex.
    pub uuid: String,
    pub account: String,
    /// The tab it is about; `None` for the machine sweep's `seen`, which saw a login place and
    /// no tab's run.
    pub tab: Option<String>,
    /// When, Unix milliseconds.
    pub t_ms: u64,
    /// A `session-end` of that tab, rather than a sighting.
    pub ended: bool,
}

/// The `seen` events of the journal window `seen`, a tab's and the machine sweep's, and each
/// tabbed `session-end`.
#[cfg(unix)]
pub fn heard(seen: &super::journal::seen::Seen) -> Vec<Heard> {
    use super::journal::Kind;
    seen.events
        .iter()
        .filter_map(|logged| {
            let event = &logged.event;
            let ended = match event.kind {
                Kind::Seen => false,
                // Only a tab's session ends a tab's sighting.
                Kind::SessionEnd if event.tab.is_some() => true,
                _ => return None,
            };
            Some(Heard {
                uuid: logged.uuid.clone(),
                account: event.account.as_ref()?.as_str().to_owned(),
                tab: event.tab.as_ref().map(|tab| tab.as_str().to_owned()),
                t_ms: event.t.saturating_mul(1_000),
                ended,
            })
        })
        .collect()
}

/// One Mac's sightings of one account.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OnMac {
    /// Its newest sighting, Unix milliseconds: its row's last write or a `seen` it logged,
    /// however old, by its own clock and never after the read.
    pub at_ms: u64,
    /// The tabs that run the account there now: the more of its row's count, where the row says
    /// `running` and was written within [`SEEN_LIVE_MS`], and its distinct tabs seen within
    /// that span and not ended since, none of them older than a `signed-out` row of that Mac.
    /// 0 where none does.
    pub tabs: u64,
    /// The first instant after the fold's at which `tabs` is another number with nothing new
    /// read (a row or a tab's sighting lapses); `None` where it stays.
    pub lapses_ms: Option<u64>,
}

/// Where one account was sighted: each Mac (Commons uuid, 32 hex) that sighted it, and each
/// whose row says its login files stopped naming it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sighted {
    pub macs: BTreeMap<String, OnMac>,
    /// Each Mac whose `account-obs` row says `signed-out` and that sighted the account nowhere
    /// since, with when the row was written (Unix milliseconds, never after the read). A Mac
    /// that logged a `seen` after its row said so sighted a login that is back, and is not
    /// here; one here may still be in `macs`, for the sighting its row ended.
    pub out: BTreeMap<String, u64>,
}

impl Sighted {
    /// The newest sighting on any Mac: when (Unix milliseconds), and that Mac's uuid.
    pub fn last(&self) -> Option<(u64, &str)> {
        self.macs
            .iter()
            .map(|(uuid, on)| (on.at_ms, uuid.as_str()))
            .max()
    }
}

/// What one Mac's records say of one account.
#[derive(Default)]
struct Told<'a> {
    /// Its newest sighting, by its own clock: a `running` or `idle` row's write, or a `seen`.
    sighted: Option<u64>,
    /// The row's last write and its tab count, where it says `running` with a tab.
    row: Option<(u64, u64)>,
    /// Each tab's newest word: when, and whether it is its session's end.
    tabs: BTreeMap<&'a str, (u64, bool)>,
    /// When its row said `signed-out`: nothing sighted up to then runs the account.
    out: Option<u64>,
}

impl Told<'_> {
    /// Whether a sighting at `at` still stands: the Mac's row has not said `signed-out` since.
    fn stands(&self, at: u64) -> bool {
        self.out.is_none_or(|out| at > out)
    }

    /// The tabs that run the account at `now_ms`.
    fn tabs_at(&self, now_ms: u64) -> u64 {
        let live = |at: u64| self.stands(at) && now_ms.saturating_sub(at) <= SEEN_LIVE_MS;
        let row = self
            .row
            .filter(|(at, _)| live(*at))
            .map_or(0, |(_, tabs)| tabs);
        let heard = self
            .tabs
            .values()
            .filter(|(at, ended)| !ended && live(*at))
            .count();
        row.max(u64::try_from(heard).unwrap_or(u64::MAX))
    }

    /// The first instant after `now_ms` at which [`Self::tabs_at`] is another number.
    fn lapses(&self, now_ms: u64) -> Option<u64> {
        let now = self.tabs_at(now_ms);
        let mut instants: Vec<u64> = self
            .row
            .iter()
            .map(|(at, _)| *at)
            .chain(
                self.tabs
                    .values()
                    .filter(|(_, ended)| !ended)
                    .map(|(at, _)| *at),
            )
            .filter(|at| self.stands(*at))
            .map(|at| at.saturating_add(SEEN_LIVE_MS).saturating_add(1))
            .filter(|lapse| *lapse > now_ms)
            .collect();
        instants.sort_unstable();
        instants.into_iter().find(|at| self.tabs_at(*at) != now)
    }
}

/// Every account's [`Sighted`] at `now_ms` from the `account-obs` rows `obs` and the journal
/// events `heard`, both read at `read_ms`. A Mac runs an account now where its row says
/// `running` with a tab and was written within [`SEEN_LIVE_MS`], or where one of its tabs was
/// seen on it within that span and has not ended since; its tabs are the more of the row's
/// count and the distinct tabs heard. Every `running` or `idle` row and every `seen` dates the
/// Mac's newest sighting, however old; a `session-end` dates none, and neither does a row that
/// says `signed-out`, which ends what that Mac sighted up to its write ([`Sighted::out`]).
///
/// A row or an event dated more than [`SKEW_MS`] ahead of `read_ms` is read as nothing: its
/// writer's clock is split from the reader's, and a time at which nothing was observed dates
/// no sighting. One dated inside the bound is a sighting at the read.
pub fn fold(obs: &[Obs], heard: &[Heard], read_ms: u64, now_ms: u64) -> BTreeMap<String, Sighted> {
    let believed = |at_ms: u64| at_ms <= read_ms.saturating_add(SKEW_MS);
    let mut told: BTreeMap<(&str, &str), Told<'_>> = BTreeMap::new();
    for row in obs.iter().filter(|row| believed(row.at_ms)) {
        let on = told.entry((&row.account, &row.uuid)).or_default();
        if row.state == ObsState::SignedOut {
            on.out = on.out.max(Some(row.at_ms));
            continue;
        }
        on.sighted = on.sighted.max(Some(row.at_ms));
        if row.state == ObsState::Running
            && row.tabs > 0
            && on.row.is_none_or(|(at, _)| at <= row.at_ms)
        {
            on.row = Some((row.at_ms, row.tabs));
        }
    }
    for event in heard.iter().filter(|event| believed(event.t_ms)) {
        let on = told.entry((&event.account, &event.uuid)).or_default();
        if !event.ended {
            on.sighted = on.sighted.max(Some(event.t_ms));
        }
        let Some(tab) = &event.tab else {
            continue;
        };
        // Each tab's newest word: a sighting, or its session's end.
        let newest = on.tabs.entry(tab).or_insert((event.t_ms, event.ended));
        if event.t_ms >= newest.0 {
            *newest = (event.t_ms, event.ended);
        }
    }
    let mut out: BTreeMap<String, Sighted> = BTreeMap::new();
    for ((account, uuid), on) in &told {
        // The order of a Mac's own records is its own clock's, before any is held to the read.
        let signed_out = on.out.filter(|out| on.sighted.is_none_or(|at| at <= *out));
        // A Mac whose tabs only ended neither sighted the account nor signed out of it: it
        // holds no entry.
        if on.sighted.is_none() && signed_out.is_none() {
            continue;
        }
        // Two lookups, so an account already held costs no allocation.
        if !out.contains_key(*account) {
            out.insert((*account).to_owned(), Sighted::default());
        }
        let Some(sighted) = out.get_mut(*account) else {
            continue;
        };
        if let Some(at) = on.sighted {
            sighted.macs.insert(
                (*uuid).to_owned(),
                OnMac {
                    at_ms: at.min(read_ms),
                    tabs: on.tabs_at(now_ms),
                    lapses_ms: on.lapses(now_ms),
                },
            );
        }
        if let Some(at) = signed_out {
            sighted.out.insert((*uuid).to_owned(), at.min(read_ms));
        }
    }
    out
}

#[cfg(test)]
#[path = "sighted_tests.rs"]
mod tests;

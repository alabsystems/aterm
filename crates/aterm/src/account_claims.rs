// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Commons table the harness's account claims ride (docs/DESIGN-fluid-swarm-2026-10-28.md
//! F19): `aterm_db::accounts::claims` behind `aterm_agent::harness::claims::Table`, and what this
//! process's Commons runtime hears of its peers. A read carries what this Mac's peers say of its
//! clock and the peers it reads as skewed, named by the swarm roster (F22).

use aterm_agent::harness::claims::{self, Book, Presence, Row, Table, Tabled};
use aterm_codec::hex;
use aterm_secrets::client::Paths;
use aterm_types::network_state::{NetworkObservation, Visibility};
use std::path::Path;

/// The local runtime's publication as the account presence adapter reads it.
pub(super) fn current_observation(
    publication: aterm_db::network_state::Publication,
) -> NetworkObservation {
    let wall = aterm_time::unix_millis();
    observation_at(publication, std::time::Instant::now(), wall)
}

fn observation_at(
    publication: aterm_db::network_state::Publication,
    now: std::time::Instant,
    wall: Option<u64>,
) -> NetworkObservation {
    publication.current_at(now, wall)
}

// A row lapses within one claims-table day, so it lands in the day's table it is written on and
// has lapsed on every clock that deletes that table (`aterm_db::accounts::claims`); it is renewed
// before it lapses.
const _: () = assert!(
    claims::HORIZON_S > 0
        && claims::HORIZON_S.unsigned_abs() <= aterm_db::accounts::claims::EPOCH_S
        && claims::RENEW_WITHIN_S < claims::HORIZON_S
);

/// Install the claims table, `observe` (this process's Commons runtime's latest observation) as
/// what it hears, and `pusher` (that runtime's push, where one runs here) as how a quiet or off
/// row reaches its peers at once.
pub(super) fn install(
    observe: fn() -> Option<NetworkObservation>,
    pusher: fn() -> Option<aterm_db::runtime::Pusher>,
) {
    claims::install(Box::new(Commons { pusher }));
    claims::install_presence(Box::new(move || presence(&observe()?)));
}

struct Commons {
    pusher: fn() -> Option<aterm_db::runtime::Pusher>,
}

fn denied_swarm_labels_from_roster(
    roster: &aterm_secrets::swarm_roster::SwarmRoster,
) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for d in &roster.deny_labels {
        let trimmed = d.trim_ascii();
        if !trimmed.is_empty() {
            out.insert(trimmed.to_ascii_lowercase());
            if let Some((_, canon)) = aterm_secrets::swarm_roster::leading_m_label(trimmed) {
                out.insert(canon);
            }
        }
    }
    if !roster.members.iter().any(|m| roster.is_member_denied(m)) {
        return out;
    }
    let surviving_canon: std::collections::BTreeSet<String> = roster
        .members
        .iter()
        .filter(|m| !roster.is_member_denied(m))
        .flat_map(|m| std::iter::once(m.label.as_str()).chain(m.aliases.iter().map(String::as_str)))
        .filter_map(|s| aterm_secrets::swarm_roster::leading_m_label(s).map(|(_, c)| c))
        .filter(|c| !out.contains(c))
        .collect();
    let mut insert_member_label = |raw: &str| {
        let trimmed = raw.trim_ascii();
        if trimmed.is_empty() {
            return;
        }
        let lower = trimmed.to_ascii_lowercase();
        if !surviving_canon.contains(&lower) {
            out.insert(lower);
        }
        if let Some((_, canon)) = aterm_secrets::swarm_roster::leading_m_label(trimmed)
            && !surviving_canon.contains(&canon)
        {
            out.insert(canon);
        }
    };
    for member in &roster.members {
        if roster.is_member_denied(member) {
            insert_member_label(&member.label);
            insert_member_label(&member.vault_name);
            if let Some(claimed) = &member.claimed_label {
                insert_member_label(claimed);
            }
            for alias in &member.aliases {
                insert_member_label(alias);
            }
        }
    }
    out
}

fn label_is_denied(label: &str, denied: &std::collections::BTreeSet<String>) -> bool {
    if denied.is_empty() {
        return false;
    }
    let trimmed = label.trim_ascii();
    if trimmed.is_empty() {
        return false;
    }
    let is_denied = if trimmed.bytes().any(|b| b.is_ascii_uppercase()) {
        denied.contains(&trimmed.to_ascii_lowercase())
    } else {
        denied.contains(trimmed)
    };
    if is_denied {
        return true;
    }
    aterm_secrets::swarm_roster::leading_m_label(trimmed)
        .is_some_and(|(_, canon)| denied.contains(&canon))
}

impl Table for Commons {
    fn read(&self, home: &Path) -> Result<Tabled, String> {
        let paths = Paths::for_home(home);
        let read = aterm_db::accounts::claims::read(&paths).map_err(|why| why.to_string())?;
        let me = hex::encode_lower(&read.me);
        // One read of the swarm roster names the denied labels and the skewed peers alike.
        let roster = aterm_secrets::client::swarm_roster_until(
            &paths,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
        )
        .map_err(|why| why.to_string())?;
        let denied = roster
            .as_ref()
            .map(denied_swarm_labels_from_roster)
            .unwrap_or_default();
        let mut machine_str = String::with_capacity(32);
        let book = Book::of(
            read.rows
                .iter()
                .filter_map(|(machine, body)| {
                    machine_str.clear();
                    hex::push_lower(&mut machine_str, machine);
                    Row::from_value(&machine_str, body).ok()
                })
                .filter(|row| row.machine == me || !label_is_denied(&row.label, &denied)),
        );
        Ok(Tabled {
            me,
            book,
            writable: read.writable,
            clock: clock_read(&read.clock),
            skewed: skewed_peers(&read.clock.skewed, roster.as_ref()),
            held: skewed_peers(&read.clock.held, roster.as_ref()),
        })
    }

    fn write(&self, home: &Path, row: &Row) -> Result<(), String> {
        aterm_db::accounts::claims::write(&Paths::for_home(home), &row.to_value())
            .map_err(|why| why.to_string())
    }

    fn push(&self, home: &Path, deadline: std::time::Instant) -> Result<(), String> {
        let pusher = (self.pusher)()
            .ok_or_else(|| "this process runs no Commons repair to hand the row on".to_owned())?;
        match aterm_db::accounts::claims::push(&Paths::for_home(home), &pusher, deadline) {
            Ok(aterm_db::runtime::Pushed::Reached) => Ok(()),
            Ok(other) => Err(other.words().to_owned()),
            Err(why) => Err(why.to_string()),
        }
    }
}

/// What `observation` says this Mac's Commons hears, where its discovery is active: a Mac that
/// cannot look says nothing about who is gone.
fn presence(observation: &NetworkObservation) -> Option<Presence> {
    (observation.visibility == Visibility::Active).then(|| Presence {
        at: secs(observation.observed_at_ms),
        heard: observation
            .peers
            .iter()
            .map(|peer| {
                (
                    hex::encode_lower(&peer.identity.machine_id),
                    secs(peer.last_seen_ms),
                )
            })
            .collect(),
    })
}

fn secs(ms: u64) -> i64 {
    i64::try_from(ms / 1000).unwrap_or(i64::MAX)
}

/// What the table's read says of this Mac's clock, as the harness takes it (F22): Split or Off;
/// none while its peers agree with it or it has no fresh reading.
fn clock_read(judged: &aterm_db::clock::Judgement) -> Option<claims::ClockRead> {
    use aterm_agent::harness::accounts::ClockStanding;
    use aterm_db::clock::Standing;
    let standing = match judged.standing {
        Standing::Split => ClockStanding::Split,
        Standing::Off { ahead } => ClockStanding::Off { ahead },
        Standing::Unknown | Standing::Agrees => return None,
    };
    Some(claims::ClockRead {
        standing,
        by_s: i64::try_from(judged.by_ms / 1000).unwrap_or(i64::MAX),
        against: u32::try_from(judged.against).unwrap_or(u32::MAX),
        of: u32::try_from(judged.of).unwrap_or(u32::MAX),
    })
}

/// The peers the read names skewed or holds for (F22), each with the swarm labels `roster` gives
/// its recipient (the roster's best entry for it, `member_by_recipient`: un-denied, bound, newest),
/// primary first, then its aliases: none where this Mac has no roster, the roster does not list
/// it, or the roster denies it, so no hold is kept for a Mac nobody can name.
fn skewed_peers(
    skewed: &[aterm_db::clock::Skewed],
    roster: Option<&aterm_secrets::swarm_roster::SwarmRoster>,
) -> Vec<claims::SkewedPeer> {
    skewed
        .iter()
        .map(|peer| claims::SkewedPeer {
            machine: hex::encode_lower(&peer.machine),
            labels: roster
                .and_then(|roster| {
                    roster
                        .member_by_recipient(&peer.recipient)
                        .filter(|m| !roster.is_member_denied(m))
                })
                .map(|member| {
                    std::iter::once(member.label.clone())
                        .chain(member.aliases.iter().cloned())
                        .collect()
                })
                .unwrap_or_default(),
            until: i64::try_from(peer.until_ms / 1000).unwrap_or(i64::MAX),
            ahead_s: i64::try_from(peer.ahead_ms.max(0).unsigned_abs().div_ceil(1000))
                .unwrap_or(i64::MAX),
        })
        .collect()
}

#[cfg(test)]
#[path = "account_claims_tests.rs"]
mod tests;

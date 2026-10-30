// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Commons table the harness's account claims ride (docs/DESIGN-fluid-swarm-2026-10-28.md
//! F19): `aterm_db::accounts::claims` behind `aterm_agent::harness::claims::Table`, and what this
//! process's Commons runtime hears of its peers.

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

/// Install the claims table, and `observe` (this process's Commons runtime's latest observation)
/// as what it hears.
pub(super) fn install(observe: fn() -> Option<NetworkObservation>) {
    claims::install(Box::new(Commons));
    claims::install_presence(Box::new(move || presence(&observe()?)));
}

struct Commons;

fn denied_swarm_labels(paths: &Paths) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let Ok(Some(roster)) = aterm_secrets::client::swarm_roster(paths) else {
        return out;
    };
    for d in &roster.deny_labels {
        out.insert(d.to_ascii_lowercase());
    }
    for member in &roster.members {
        let member_denied = roster.is_label_denied(&member.label)
            || member.aliases.iter().any(|a| roster.is_label_denied(a))
            || member
                .node_id
                .as_deref()
                .is_some_and(|n| roster.is_node_denied(n))
            || member
                .node_key
                .as_deref()
                .is_some_and(|k| roster.is_node_denied(k));
        if member_denied {
            out.insert(member.label.to_ascii_lowercase());
            if !member.vault_name.trim().is_empty() {
                out.insert(member.vault_name.trim().to_ascii_lowercase());
            }
            if let Some(claimed) = &member.claimed_label {
                out.insert(claimed.to_ascii_lowercase());
            }
            for alias in &member.aliases {
                out.insert(alias.to_ascii_lowercase());
            }
        }
    }
    out
}

fn label_is_denied(label: &str, denied: &std::collections::BTreeSet<String>) -> bool {
    if denied.is_empty() {
        return false;
    }
    let canon = aterm_secrets::swarm_roster::leading_m_label(label)
        .map(|(_, c)| c)
        .unwrap_or_else(|| label.trim_ascii().to_ascii_lowercase());
    !canon.is_empty() && denied.contains(&canon)
}

impl Table for Commons {
    fn read(&self, home: &Path) -> Result<Tabled, String> {
        let paths = Paths::for_home(home);
        let read = aterm_db::accounts::claims::read(&paths).map_err(|why| why.to_string())?;
        let me = hex::encode_lower(&read.me);
        let denied = denied_swarm_labels(&paths);
        let book = Book::of(
            read.rows
                .iter()
                .filter_map(|(machine, body)| {
                    Row::from_value(&hex::encode_lower(machine), body).ok()
                })
                .filter(|row| row.machine == me || !label_is_denied(&row.label, &denied)),
        );
        Ok(Tabled {
            me,
            book,
            writable: read.writable,
        })
    }

    fn write(&self, home: &Path, row: &Row) -> Result<(), String> {
        aterm_db::accounts::claims::write(&Paths::for_home(home), &row.to_value())
            .map_err(|why| why.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_db::network_state::Publication;
    use aterm_types::network_state::{
        HistoryState, MachineIdentity, PeerObservation, Reachability,
    };
    use std::time::{Duration, Instant};

    fn publication(sampled: Instant, wall_ms: u64) -> Publication {
        let heard = wall_ms - (claims::PRESENCE_TTL_S.unsigned_abs() + 60) * 1000;
        Publication::new(
            NetworkObservation {
                observed_at_ms: wall_ms,
                visibility: Visibility::Active,
                history: HistoryState::Ready,
                peers: vec![PeerObservation {
                    slot: 1,
                    identity: MachineIdentity {
                        machine_id: [2; 16],
                        incarnation: [1; 16],
                        label: None,
                    },
                    first_seen_ms: heard,
                    last_seen_ms: heard,
                    last_checked_ms: None,
                    reachability: Reachability::LastSeen,
                    instances: None,
                    samples: Vec::new(),
                }],
                ..NetworkObservation::default()
            },
            sampled,
        )
    }

    fn unheard(observation: &NetworkObservation, wall_ms: u64) -> bool {
        claims::View {
            me: hex::encode_lower(&[1; 16]),
            presence: presence(observation),
        }
        .unheard(&hex::encode_lower(&[2; 16]), secs(wall_ms))
    }

    #[test]
    fn expired_local_observation_cannot_lapse_claims_after_wall_clock_rollback() {
        let wall = 2_000_000;
        let now = Instant::now();
        // The wall timestamp looks fresh after a rollback; monotonic time says
        // this owner has not supplied a current sample for twenty seconds.
        let publication = publication(now - Duration::from_secs(20), wall);
        assert_eq!(publication.observation().visibility, Visibility::Active);
        let observation = observation_at(publication, now, Some(wall));
        assert!(
            !unheard(&observation, wall),
            "an expired observer supplied evidence to lapse another Mac's lease"
        );
        assert_eq!(observation.visibility, Visibility::Unavailable);
    }

    #[test]
    fn current_local_observation_still_lapses_genuinely_absent_peer_claims() {
        let wall = 2_000_000;
        let now = Instant::now();
        let observation = observation_at(publication(now, wall), now, Some(wall));
        assert_eq!(observation.visibility, Visibility::Active);
        assert!(unheard(&observation, wall));
    }
}

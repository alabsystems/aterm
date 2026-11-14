// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::network_state::Publication;
use aterm_types::network_state::{HistoryState, MachineIdentity, PeerObservation, Reachability};
use std::time::{Duration, Instant};

fn publication(sampled: Instant, wall_ms: u64) -> Publication {
    publication_of(sampled, wall_ms, Some(observer()), HistoryState::Ready)
}

/// This Mac's own identity as its observer confirmed it.
fn observer() -> MachineIdentity {
    MachineIdentity {
        machine_id: [1; 16],
        incarnation: [1; 16],
        label: None,
    }
}

/// A current Active publication whose one peer was last heard past the presence window, naming
/// `observer` as its observer, with its history in `history`.
fn publication_of(
    sampled: Instant,
    wall_ms: u64,
    observer: Option<MachineIdentity>,
    history: HistoryState,
) -> Publication {
    let heard = wall_ms - (claims::PRESENCE_TTL_S.unsigned_abs() + 60) * 1000;
    Publication::new(
        NetworkObservation {
            observed_at_ms: wall_ms,
            visibility: Visibility::Active,
            history,
            observer,
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
                last_repair: None,
                last_authenticated_endpoint: None,
                reachability: Reachability::LastSeen,
                instances: None,
                samples: Vec::new(),
                lease_ms: None,
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
        ..claims::View::default()
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

/// An observer that takes no contacts publishes Active over a peer list it does not keep
/// current, and names no observer: one that refused its retained history (another observer's or
/// trust context's, as a writer successor's did before it took up its own), could not open it,
/// or has not confirmed this Mac yet contacts none of those peers into it, so their last contacts
/// stand still. Its peers' leases do not lapse by it: it gives no presence, as a Mac that cannot
/// look gives none. An observer whose checkpoint failed still names itself and takes every
/// contact (`aterm_db`'s `a_failed_checkpoint_keeps_hearing_peers_and_naming_its_observer`), so
/// its peers' last contacts are current: an absent peer's lease still lapses by it, as by a
/// ready one (`current_local_observation_still_lapses_genuinely_absent_peer_claims`).
///
/// Negative controls: with presence read off visibility alone, an observer that named none lapsed
/// the peer's lease (`an observer that takes no contacts lapsed a peer's lease (Unavailable)`);
/// with it gated on a ready history (2026-11-13), the failed checkpoint's observer gave no
/// presence and the absent peer's lease stood until its row's `until` (`an observer whose
/// checkpoint failed kept an absent peer's lease`).
#[test]
fn an_observer_that_takes_no_contacts_lapses_no_claim() {
    let wall = 2_000_000;
    let now = Instant::now();
    let failed = observation_at(
        publication_of(now, wall, Some(observer()), HistoryState::Unavailable),
        now,
        Some(wall),
    );
    assert!(
        unheard(&failed, wall),
        "an observer whose checkpoint failed kept an absent peer's lease"
    );
    for history in [
        HistoryState::Unavailable,
        HistoryState::Restoring,
        HistoryState::Ready,
    ] {
        let observation = observation_at(publication_of(now, wall, None, history), now, Some(wall));
        assert_eq!(
            observation.visibility,
            Visibility::Active,
            "PRECONDITION: a current publication"
        );
        assert!(
            !unheard(&observation, wall),
            "an observer that takes no contacts lapsed a peer's lease ({history:?})"
        );
        assert_eq!(presence(&observation), None, "{history:?}");
    }
}

/// The newest row this Mac's earlier writer seat signed (the db read's `former`) goes into the
/// book as the current seat's own row at counter 0, while that seat has none: its claims keep
/// their `since`, so a peer's claim made after them is junior here (the harness's
/// `a_writer_successor_keeps_the_seniority_of_the_claims_it_still_uses`). It never replaces a row
/// the current seat wrote, and a lapsed row or one that claims nothing carries nothing.
///
/// Negative control: with `carry` putting nothing in, the book held no row of the current seat's
/// (`carried as the current seat's own row`), so the peer's junior claim held the account.
#[test]
fn the_earlier_seats_row_carries_its_seniority_into_the_current_seats_book() {
    use aterm_json::json;
    let now = 1_790_000_000;
    let body = |counter: u64, since: i64, until: i64| {
        json!({"v": 1, "label": "m1", "counter": counter, "until": until, "claims": [
            {"pool": "claude", "email": "a@example.test", "kind": "lease", "since": since}
        ]})
    };
    let key = claims::LeaseKey::new("claude", "a@example.test").unwrap();
    let peer = Row::from_value("dd04", &body(1, now - 600, now + 3600)).unwrap();
    let former = body(7, now - 1200, now + 3600);
    let mut book = Book::of([peer.clone()]);
    carry(&mut book, "ee01", Some(&former), now);
    let carried = book
        .row("ee01")
        .expect("carried as the current seat's own row");
    assert_eq!(carried.counter, 0);
    assert_eq!(
        carried.claim(&key).map(|claim| claim.since),
        Some(now - 1200)
    );
    let view = claims::View {
        me: "ee01".to_owned(),
        ..claims::View::default()
    };
    assert_eq!(
        book.senior(&key, &view, now).map(|holder| holder.machine),
        Some("ee01".to_owned()),
        "the peer's later claim is junior"
    );
    // A row the current seat wrote stands.
    let written = Row::from_value("ee01", &body(1, now, now + 3600)).unwrap();
    let mut book = Book::of([peer.clone(), written.clone()]);
    carry(&mut book, "ee01", Some(&former), now);
    assert_eq!(book.row("ee01"), Some(&written));
    // A lapsed row, one that claims nothing, and none at all carry nothing.
    let empty = json!({"v": 1, "label": "m1", "counter": 7, "until": now + 3600, "claims": []});
    for former in [Some(body(7, now - 1200, now)), Some(empty), None] {
        let mut book = Book::of([peer.clone()]);
        carry(&mut book, "ee01", former.as_ref(), now);
        assert_eq!(book.row("ee01"), None, "{former:?}");
    }
}

#[test]
fn denied_swarm_labels_canonicalizes_suffixed_names_without_denying_surviving_member() {
    use aterm_secrets::swarm_roster::{SwarmMember, SwarmRoster};

    let roster = SwarmRoster {
        deny_labels: vec!["M15.local".to_owned()],
        deny_nodes: vec!["node-bad".to_owned()],
        members: vec![
            SwarmMember {
                recipient: "age1good".to_owned(),
                vault_name: "m12".to_owned(),
                label: "m12".to_owned(),
                aliases: vec!["m44".to_owned()],
                hint: None,
                hw_hint: None,
                admitted_ms: 100,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: Some("node-good".to_owned()),
                node_key: None,
            },
            SwarmMember {
                recipient: "age1bad".to_owned(),
                vault_name: "m12-dup.local".to_owned(),
                label: "m101".to_owned(),
                aliases: vec!["m33".to_owned()],
                hint: None,
                hw_hint: None,
                admitted_ms: 200,
                claimed_label: Some("m12".to_owned()),
                claimed_at_ms: Some(200),
                node_id: Some("node-bad".to_owned()),
                node_key: None,
            },
            SwarmMember {
                recipient: "age1bad2".to_owned(),
                vault_name: "m22.local".to_owned(),
                label: "m102".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 300,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: Some("node-bad".to_owned()),
                node_key: None,
            },
        ],
    };

    let denied = denied_swarm_labels_from_roster(&roster);
    assert!(label_is_denied("m15", &denied));
    assert!(label_is_denied("m15-studio", &denied));
    assert!(label_is_denied("m101", &denied));
    assert!(label_is_denied("m33", &denied));
    assert!(label_is_denied("m12-dup.local", &denied));
    assert!(label_is_denied("m22", &denied));
    assert!(label_is_denied("m22.local", &denied));

    // Negative control: surviving member m12 (and its alias m44) must not be denied even
    // though the node-denied member had vault_name `m12-dup.local` and claimed_label `m12`.
    assert!(!label_is_denied("m12", &denied));
    assert!(!label_is_denied("m44", &denied));
}

/// What the db judges of this Mac's clock reaches the harness (F22): Split and Off carry
/// their counts and how far, in whole seconds rounded down (at least that far); agreement and
/// no fresh reading give none, so they refuse nothing.
///
/// Negative control: with Agrees taken as Split, a clock its peers agree with refused
/// leases (`left: Some(ClockRead { standing: Split, .. })`, `right: None`).
#[test]
fn the_clock_read_carries_split_and_off_and_drops_agreement() {
    use aterm_agent::harness::accounts::ClockStanding;
    use aterm_db::clock::{Judgement, Standing};
    let judged = |standing, by_ms| Judgement {
        standing,
        by_ms,
        against: 3,
        of: 4,
        silent: false,
        skewed: Vec::new(),
        held: Vec::new(),
    };
    let read = |standing, by_s| claims::ClockRead {
        standing,
        by_s,
        against: 3,
        of: 4,
    };
    assert_eq!(clock_read(&judged(Standing::Agrees, 0)), None);
    assert_eq!(clock_read(&judged(Standing::Unknown, 0)), None);
    assert_eq!(
        clock_read(&judged(Standing::Split, 0)),
        Some(read(ClockStanding::Split, 0))
    );
    assert_eq!(
        clock_read(&judged(Standing::Off { ahead: false }, 90_999)),
        Some(read(ClockStanding::Off { ahead: false }, 90))
    );
}

/// The peers the db reads as skewed (F22) carry the swarm labels the roster gives their
/// recipients, primary first, so their home accounts are held for them; a recipient the
/// roster does not list or denies, or any recipient with no roster, gets none, and so no
/// hold.
///
/// A recipient the roster lists twice (a stale entry left by a re-admission before the bound
/// one) takes the best entry's labels, the one `member_by_recipient` picks.
///
/// Negative controls: with the roster's denial ignored (the `is_member_denied` filter left
/// out), the denied Mac got its label (`left: [["m1", "m7"], ["m9"], []]`); with `ahead_s`
/// rounded down, the offset fell short (`rounded up: at most that far ahead`, `left: 90`,
/// `right: 91`); with the first listed entry of a recipient taken, the re-admitted Mac got its
/// stale label (`left: ["m40"]`, `right: ["m4", "m44"]`).
#[test]
fn skewed_peers_are_named_by_the_roster_and_never_when_denied() {
    use aterm_secrets::swarm_roster::{SwarmMember, SwarmRoster};
    let member = |recipient: &str, label: &str, aliases: &[&str], node: &str| SwarmMember {
        recipient: recipient.to_owned(),
        vault_name: label.to_owned(),
        label: label.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        hint: None,
        admitted_ms: 100,
        claimed_label: None,
        claimed_at_ms: None,
        node_id: Some(node.to_owned()),
        node_key: None,
        hw_hint: None,
    };
    let roster = SwarmRoster {
        deny_labels: Vec::new(),
        deny_nodes: vec!["node-bad".to_owned()],
        members: vec![
            member("age1good", "m1", &["m7"], "node-good"),
            member("age1bad", "m9", &[], "node-bad"),
        ],
    };
    let skewed = |recipient: &str, byte: u8| aterm_db::clock::Skewed {
        machine: [byte; 16],
        recipient: recipient.to_owned(),
        until_ms: 1_790_000_000_999,
        ahead_ms: 90_001,
    };
    let peers = skewed_peers(
        &[
            skewed("age1good", 1),
            skewed("age1bad", 2),
            skewed("age1none", 3),
        ],
        Some(&roster),
    );
    assert_eq!(
        peers
            .iter()
            .map(|peer| peer.labels.clone())
            .collect::<Vec<_>>(),
        [
            vec!["m1".to_owned(), "m7".to_owned()],
            Vec::new(),
            Vec::new()
        ]
    );
    assert_eq!(peers[0].machine, hex::encode_lower(&[1; 16]));
    assert_eq!(peers[0].until, 1_790_000_000);
    assert_eq!(peers[0].ahead_s, 91, "rounded up: at most that far ahead");
    assert!(
        skewed_peers(&[skewed("age1good", 1)], None)[0]
            .labels
            .is_empty()
    );
    let stale = SwarmMember {
        node_id: None,
        admitted_ms: 50,
        ..member("age1again", "m40", &[], "")
    };
    let readmitted = SwarmRoster {
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
        members: vec![stale, member("age1again", "m4", &["m44"], "node-again")],
    };
    assert_eq!(
        skewed_peers(&[skewed("age1again", 4)], Some(&readmitted))[0].labels,
        ["m4", "m44"]
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::log_entries_tests::{member_key, newcomer, two_macs};
use super::*;
use crate::age;
use crate::membership_log::{Founder, Root, node_proof_message};
use ring::signature::{Ed25519KeyPair, KeyPair as _};

const DAY: u64 = 24 * 60 * 60 * 1000;
const T0: u64 = 1_000 * DAY;

/// A fleet book with nothing against the proving Mac, in which every member runs 0.148 or later.
const CURRENT: Book = Book {
    node_id_held_out: false,
    node_id_seated: false,
    node_elsewhere: false,
    moved_seats_read: true,
};

/// A synthetic Mac: every key is derived from its tag, nothing touches a real vault.
#[derive(Clone, Copy, Debug)]
struct Mac(u8);

/// The member that decides.
const ME: Mac = Mac(1);
/// A founder bound by the genesis whose own `Node` record binds `node(2)`.
const SEAT: Mac = Mac(2);
/// A Mac `ME` admitted with `node(3)`, which no `Node` record proves.
const ADMITTED: Mac = Mac(3);
/// A founder bound by the genesis, which lists `node(4)` for it.
const LISTED: Mac = Mac(4);
/// A founder the genesis lists with no binding, under `m5`, with `node(5)`.
const UNPROVED: Mac = Mac(5);

impl Mac {
    fn seed(self) -> [u8; 32] {
        [self.0; 32]
    }
    fn incarnation(self) -> [u8; 16] {
        [self.0; 16]
    }
    fn identity(self) -> age::Identity {
        age::Identity::from_secret_bytes([self.0 ^ 0x55; 32])
    }
    fn request(self) -> Request {
        Request::create(
            [2; 16],
            [3; 32],
            [self.0; 16],
            self.incarnation(),
            &self.identity(),
            &self.seed(),
        )
        .unwrap()
    }
    fn id(self) -> MemberId {
        self.request().binding().actor()
    }
}

fn node_pair(tag: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[tag ^ 0xa5; 32]).unwrap()
}
fn node(tag: u8) -> [u8; 32] {
    node_pair(tag).public_key().as_ref().try_into().unwrap()
}

fn founder(mac: Mac, label: &str, node_key: Option<[u8; 32]>) -> Founder {
    let binding = mac.request().binding().clone();
    Founder {
        recipient: binding.recipient(),
        binding: Some(binding),
        node_key,
        name: label.to_owned(),
        label: label.to_owned(),
    }
}

fn keep(log: &mut Log, by: Mac, at: u64, body: Body) -> Record {
    let record = log
        .draft(&by.id(), at, body)
        .sign(&by.seed(), by.incarnation())
        .unwrap();
    log.insert(record.clone()).unwrap();
    record
}

fn node_record(log: &mut Log, by: Mac, tag: u8, at: u64) {
    let proof: [u8; 64] = node_pair(tag)
        .sign(&node_proof_message(log.network(), &by.id()))
        .as_ref()
        .try_into()
        .unwrap();
    keep(
        log,
        by,
        at,
        Body::Node {
            node_key: node(tag),
            proof,
        },
    );
}

fn admit(log: &mut Log, by: Mac, mac: Mac, tag: u8, label: &str, evidence: Evidence, at: u64) {
    keep(
        log,
        by,
        at,
        Body::Admit {
            entrant: Entrant {
                request: mac.request(),
                node_key: node(tag),
                name: label.to_owned(),
                label: label.to_owned(),
            },
            evidence,
        },
    );
}

/// ALab in small: `ME` (`m1`), `SEAT` (`m2`, its `Node` record binds `node(2)`), `ADMITTED`
/// (`m3`, `ME`'s `Admit` names `node(3)`), `LISTED` (`m4`, the genesis lists `node(4)`) and
/// `UNPROVED` (`m5`, listed with no binding and `node(5)`).
fn alab() -> Log {
    let root = Root::of(ME.request().binding());
    let founders = vec![
        founder(ME, "m1", Some(node(1))),
        founder(SEAT, "m2", None),
        founder(LISTED, "m4", Some(node(4))),
        Founder {
            recipient: UNPROVED.identity().recipient(),
            binding: None,
            node_key: Some(node(5)),
            name: "m5".to_owned(),
            label: "m5".to_owned(),
        },
    ];
    let mut log = Log::new(Record::genesis(&[99; 32], root, founders, 1).unwrap()).unwrap();
    node_record(&mut log, SEAT, 2, 2);
    admit(&mut log, ME, ADMITTED, 3, "m3", Evidence::Phrase, 3);
    log
}

fn decided(log: &Log, mac: Mac, tag: u8, fleet: bool, at: u64) -> Decision {
    let request = mac.request();
    decide(
        log,
        &ME.id(),
        &Proven {
            request: &request,
            node: node(tag),
            name: "machine",
            fleet,
            lan: false,
            fresh: false,
            book: CURRENT,
        },
        at,
    )
}

/// [`decided`] for a proof from another host on the member's home ALab LAN, with no fleet key.
fn decided_on_lan(log: &Log, mac: Mac, tag: u8, at: u64) -> Decision {
    let request = mac.request();
    decide(
        log,
        &ME.id(),
        &Proven {
            request: &request,
            node: node(tag),
            name: "machine",
            fleet: false,
            lan: true,
            fresh: false,
            book: CURRENT,
        },
        at,
    )
}

/// Sign what `decision` says, as [`admit_by_proof`] does with no epoch to wrap.
fn apply(log: &mut Log, mac: Mac, tag: u8, decision: &Decision, at: u64) {
    let label = match decision {
        Decision::Replace {
            seat,
            cutoff,
            label,
        } => {
            keep(
                log,
                ME,
                at,
                Body::Remove {
                    member: *seat,
                    cutoff: *cutoff,
                },
            );
            label.clone()
        }
        Decision::Founder { label } => label.clone(),
        Decision::Fleet | Decision::Lan => "m60".to_owned(),
        other => panic!("nothing to sign for {other:?}"),
    };
    admit(log, ME, mac, tag, &label, Evidence::Manual, at);
}

fn refused(decision: &Decision) -> bool {
    matches!(decision, Decision::Refuse(_))
}

/// A new vault key that proves the node key the log binds to a seat by authority takes that
/// seat back under its label, with nobody: the seat's own `Node` record (`SEAT`) and the
/// genesis's listing (`LISTED`) each bind it. The evaluator then seats the new key bare under the
/// old label with the node key, and the old seat is out.
///
/// NEGATIVE CONTROL: `authority_within` reading only the genesis listing (its `Node` record and
/// chain arms cut): `SEAT`'s new key is refused ("only that seat's Admit binds this node key")
/// and the first `Replace` match fails.
#[test]
fn a_new_key_that_proves_a_seats_node_key_takes_that_seat_under_its_label() {
    let mut log = alab();
    for (old, new, tag, label) in [(SEAT, Mac(12), 2, "m2"), (LISTED, Mac(14), 4, "m4")] {
        let decision = decided(&log, new, tag, false, T0);
        let Decision::Replace {
            seat, label: taken, ..
        } = &decision
        else {
            panic!("{label}'s new key is refused: {decision:?}");
        };
        assert_eq!((*seat, taken.as_str()), (old.id(), label));
        apply(&mut log, new, tag, &decision, T0);
        let membership = log.membership();
        assert_eq!(
            membership.label(&new.id()),
            Some(label),
            "the label carries over"
        );
        assert_eq!(membership.node(&node(tag)), NodeStanding::Member(new.id()));
        assert!(!membership.contains(&old.id()), "the old seat is out");
        assert!(!membership.removed_labels().contains(label));
        assert_eq!(
            decided(&log, new, tag, false, T0 + 1),
            Decision::Already,
            "a second proof by the same Mac signs nothing new"
        );
    }
}

/// A seat moves by no proof while a member the fleet book heard in the last 14 days runs a build
/// older than 0.148 (`Book::moved_seats_read` false): 0.147's evaluator reads the moved seat's node
/// key as removed (it supersedes a removed seat only by its writer key or recipient), and so the
/// returned Mac's recipient as a removed Mac's, and its vault pass would mint `[everyone]` keys
/// that leave the Mac out for good. The 0.147 view is checked here on the very records a move
/// signs: v0.147.0's `recipient_is_removed` (a member whose node key reads `Removed`) holds for the
/// new key once its seat's node key is not superseded.
///
/// NEGATIVE CONTROL: the `moved_seats_read` gate in `decide` cut (`if false && …`): the proof is
/// let in while a 0.147 member is heard and the first assert fails.
#[test]
fn a_seat_moves_by_no_proof_while_a_member_still_runs_0147() {
    let mut log = alab();
    let request = Mac(12).request();
    let old_member_heard = Proven {
        request: &request,
        node: node(2),
        name: "m2",
        fleet: true,
        lan: false,
        fresh: false,
        book: Book {
            moved_seats_read: false,
            ..CURRENT
        },
    };
    assert!(
        refused(&decide(&log, &ME.id(), &old_member_heard, T0)),
        "a 0.147 member heard: the seat stays"
    );
    // What 0.147 would make of the move: its supersede rule (writer key or recipient alone)
    // leaves the old seat removed, so the shared node key reads removed to it.
    let decision = decided(&log, Mac(12), 2, false, T0);
    apply(&mut log, Mac(12), 2, &decision, T0);
    let new_seat = log
        .membership()
        .get(&Mac(12).id())
        .cloned()
        .expect("seated");
    assert_ne!(
        new_seat.binding.recipient(),
        SEAT.request().binding().recipient()
    );
    assert_ne!(new_seat.binding.writer(), SEAT.request().binding().writer());
    assert_ne!(Mac(12).id()[..32], SEAT.id()[..32]);
    assert_eq!(
        new_seat.node_key,
        Some(node(2)),
        "only the node key ties the seats"
    );
}

/// A seat whose Mac may still run keeps it, and a person decides: one that signed a record in the
/// last [`DARK_FOR_MS`] (a copy of its fleet key on a Mac that made a new vault key, while the
/// original still writes), and one whose node key this member's fleet book hears from another
/// address than the proof's. A day after its last record, with the key heard nowhere else, the
/// seat moves. A Mac a move left out is offered no Undo of it (`seat_moved_by`).
///
/// NEGATIVE CONTROLS: the `signed_since` check in `decide` cut (`if false && …`): the live seat
/// moves and "the seat signed an hour ago" fails. The `node_elsewhere` check cut: "heard from
/// another address" fails. `seat_moved_by` answering `false`: "no Undo of a move" fails.
#[test]
fn a_seat_whose_mac_may_still_run_goes_to_a_person() {
    let mut log = alab();
    node_record(&mut log, SEAT, 2, T0 - DAY / 24);
    assert!(
        refused(&decided(&log, Mac(12), 2, false, T0)),
        "the seat signed an hour ago"
    );
    let request = Mac(12).request();
    let elsewhere = Proven {
        request: &request,
        node: node(2),
        name: "m2",
        fleet: true,
        lan: false,
        fresh: false,
        book: Book {
            node_elsewhere: true,
            ..CURRENT
        },
    };
    assert!(
        refused(&decide(&log, &ME.id(), &elsewhere, T0 + 2 * DAY)),
        "heard from another address"
    );
    let decision = decided(&log, Mac(12), 2, false, T0 + 2 * DAY);
    assert!(
        matches!(&decision, Decision::Replace { seat, .. } if *seat == SEAT.id()),
        "dark a day and heard nowhere else: {decision:?}"
    );
    assert!(!seat_moved_by(&log, &ME.id(), &SEAT.id()));
    apply(&mut log, Mac(12), 2, &decision, T0 + 2 * DAY);
    assert!(
        seat_moved_by(&log, &ME.id(), &SEAT.id()),
        "no Undo of a move"
    );
    let cutoff = *log
        .records()
        .find(|r| matches!(r.body(), Body::Admit { entrant, .. } if entrant.member() == ADMITTED.id()))
        .expect("ADMITTED's Admit")
        .id();
    keep(
        &mut log,
        ME,
        T0 + 3 * DAY,
        Body::Remove {
            member: ADMITTED.id(),
            cutoff,
        },
    );
    assert!(
        !seat_moved_by(&log, &ME.id(), &ADMITTED.id()),
        "a plain removal keeps its Undo"
    );
}

/// A seat that only its own `Admit` binds to a node key (the admitting member's word, which no
/// proof by the key backs) moves only where the Mac also proved the fleet's current key.
///
/// NEGATIVE CONTROL: the Admit-only gate in `decide` cut (`if false && !bound_by_authority(…)`):
/// the proof of `node(3)` alone moves `ADMITTED`'s seat and the "an Admit-only binding is not
/// proof enough" assert fails.
#[test]
fn a_seat_only_its_admit_binds_moves_only_with_the_fleets_key_too() {
    let mut log = alab();
    assert!(
        refused(&decided(&log, Mac(13), 3, false, T0)),
        "an Admit-only binding is not proof enough"
    );
    let decision = decided(&log, Mac(13), 3, true, T0);
    assert!(
        matches!(&decision, Decision::Replace { seat, label, .. }
            if *seat == ADMITTED.id() && label == "m3"),
        "{decision:?}"
    );
    apply(&mut log, Mac(13), 3, &decision, T0);
    assert_eq!(log.membership().label(&Mac(13).id()), Some("m3"));
}

/// The founder case (m100 and m110: founder seats whose vault keys changed). A founder the
/// genesis lists with no binding and a node key, which never proved its old device key, comes
/// back under its label when its Mac proves that node key; a bound founder comes back by its seat
/// (`a_new_key_that_proves_a_seats_node_key_takes_that_seat_under_its_label`). Its own `Node`
/// record then binds the key to it.
///
/// NEGATIVE CONTROL: deleting the `membership.unproved()` branch from `decide`: the proof of
/// `node(5)` without the fleet's key is refused and the `Founder` assert fails.
#[test]
fn a_founder_listed_with_no_binding_comes_back_under_its_label() {
    let mut log = alab();
    let decision = decided(&log, Mac(15), 5, false, T0);
    assert_eq!(
        decision,
        Decision::Founder {
            label: "m5".to_owned()
        }
    );
    apply(&mut log, Mac(15), 5, &decision, T0);
    assert_eq!(log.membership().label(&Mac(15).id()), Some("m5"));
    node_record(&mut log, Mac(15), 5, T0 + 1);
    assert_eq!(
        log.membership().node(&node(5)),
        NodeStanding::Member(Mac(15).id())
    );
}

/// A node key no seat binds lets a Mac in only where it also proved the fleet's current key.
///
/// NEGATIVE CONTROL: dropping the `!proven.fleet` refusal from the `Unknown` arm of `decide`: the
/// proof of `node(9)` alone lets the Mac in and the first assert fails.
#[test]
fn a_mac_no_seat_binds_comes_in_only_by_the_fleets_key() {
    let log = alab();
    assert!(refused(&decided(&log, Mac(19), 9, false, T0)));
    assert_eq!(decided(&log, Mac(19), 9, true, T0), Decision::Fleet);
}

/// Two Macs holding one node key (Migration Assistant, a restore, a cloned disk) never trade its
/// seat back and forth: the seat moves at most once a day, a key it moved off never takes it back,
/// and after [`MAX_READMISSIONS`] moves a person decides.
///
/// NEGATIVE CONTROL: the day check in `decide` cut: `Mac(22)`'s proof an hour after `Mac(12)`
/// took the seat moves it again and the "less than a day" assert fails. With `>=` read as `>` in
/// the `MAX_READMISSIONS` check, the fifth move goes through and the last assert fails.
#[test]
fn two_macs_holding_one_node_key_never_trade_its_seat_back_and_forth() {
    let mut log = alab();
    let first = decided(&log, Mac(12), 2, false, T0);
    apply(&mut log, Mac(12), 2, &first, T0);
    assert!(
        refused(&decided(&log, Mac(22), 2, true, T0 + DAY / 24)),
        "a second Mac proving the key less than a day after the move goes to a person"
    );
    let second = decided(&log, Mac(22), 2, false, T0 + DAY + 1);
    assert!(
        matches!(&second, Decision::Replace { seat, label, .. }
            if *seat == Mac(12).id() && label == "m2"),
        "a day on, the key moved by proof moves again: {second:?}"
    );
    apply(&mut log, Mac(22), 2, &second, T0 + DAY + 1);
    assert!(
        refused(&decided(&log, Mac(12), 2, true, T0 + 3 * DAY)),
        "the key the seat moved off never takes it back"
    );
    for (n, mac) in [Mac(23), Mac(24)].into_iter().enumerate() {
        let at = T0 + (3 + n as u64) * DAY;
        let decision = decided(&log, mac, 2, false, at);
        assert!(matches!(decision, Decision::Replace { .. }), "{decision:?}");
        apply(&mut log, mac, 2, &decision, at);
    }
    assert_eq!(readmissions(&log, &node(2)).len(), MAX_READMISSIONS);
    assert_eq!(log.membership().label(&Mac(24).id()), Some("m2"));
    assert!(
        refused(&decided(&log, Mac(25), 2, true, T0 + 10 * DAY)),
        "past the moves allowed a person decides"
    );
}

/// A new Mac on the home ALab LAN is let in for being there (owner, 2026-11-02: "we assume that
/// physical access to the LAN is sufficient proof"): no seat binds its node key and it proved no
/// fleet key, yet it takes a new seat, past this member's hourly share and the vault's daily one,
/// which hold only off the LAN. The same proof from off the LAN gets nothing.
///
/// NEGATIVE CONTROLS: the `proven.lan` arm of `decide`'s unknown-node branch cut (`else if false`):
/// the first assert fails with `NO_SEAT`. `!proven.lan &&` cut from the `PROVEN_PER_DAY` check: the
/// LAN Mac past the day's share is refused and "the LAN takes no daily share" fails.
#[test]
fn a_new_mac_on_the_home_lan_takes_a_seat_with_nobody() {
    let mut log = alab();
    assert_eq!(decided_on_lan(&log, Mac(70), 70, T0), Decision::Lan);
    assert!(
        refused(&decided(&log, Mac(70), 70, false, T0)),
        "off the LAN, with no seat and no fleet key, nothing"
    );
    for (n, tag) in (80..80 + PROVEN_PER_DAY as u8).enumerate() {
        let at = T0 + n as u64;
        assert_eq!(decided_on_lan(&log, Mac(tag), tag, at), Decision::Lan);
        admit(
            &mut log,
            ME,
            Mac(tag),
            tag,
            &format!("m{tag}"),
            Evidence::Manual,
            at,
        );
    }
    let late = T0 + PROVEN_PER_DAY as u64;
    assert!(
        refused(&decided(&log, Mac(71), 71, true, late)),
        "off the LAN the shares hold"
    );
    assert_eq!(
        decided_on_lan(&log, Mac(71), 71, late),
        Decision::Lan,
        "the LAN takes no daily share"
    );
}

/// A seat that only its own `Admit` binds to a node key (`ADMITTED`, which no `Node` record or
/// genesis listing proves) moves to the Mac that proves that node key from the home ALab LAN, as it
/// does for one that proves the fleet's key; from off the LAN with neither, it stays. A Mac the
/// vault removed stays out on the LAN too.
///
/// NEGATIVE CONTROL: `&& !proven.lan` cut from the authority check in `decide`: the LAN Mac is
/// refused and the first assert fails.
#[test]
fn on_the_home_lan_a_node_key_takes_back_the_seat_its_admit_binds() {
    let mut log = alab();
    assert!(
        matches!(
            decided_on_lan(&log, Mac(13), 3, T0),
            Decision::Replace { seat, .. } if seat == ADMITTED.id()
        ),
        "the LAN moves the seat its Admit binds"
    );
    assert!(
        refused(&decided(&log, Mac(13), 3, false, T0)),
        "off the LAN, with no fleet key, the seat stays"
    );
    let cutoff = *log
        .records()
        .filter(|r| r.actor() == &SEAT.id())
        .last()
        .unwrap()
        .id();
    keep(
        &mut log,
        ME,
        T0,
        Body::Remove {
            member: SEAT.id(),
            cutoff,
        },
    );
    assert!(
        refused(&decided_on_lan(&log, Mac(32), 2, T0 + DAY)),
        "a removed node key stays out on the LAN"
    );
}

/// A Mac the vault removed (a person's plain removal, no `--stolen`) stays out whatever it
/// proves: its own key, its node key under a new vault key, the fleet's key, or a fresh node key
/// under a node id the fleet book bound to a key the log holds out.
///
/// NEGATIVE CONTROL: the `NodeStanding::Removed(_)` arm of `decide` read as `Decision::Fleet`:
/// `Mac(32)` proving the removed `node(2)` and the fleet's key is let in and the second assert
/// fails.
#[test]
fn a_removed_mac_stays_out_whatever_it_proves() {
    let mut log = alab();
    let cutoff = *log
        .records()
        .filter(|r| r.actor() == &SEAT.id())
        .last()
        .unwrap()
        .id();
    keep(
        &mut log,
        ME,
        T0,
        Body::Remove {
            member: SEAT.id(),
            cutoff,
        },
    );
    assert!(refused(&decided(&log, SEAT, 9, true, T0 + DAY)));
    assert!(refused(&decided(&log, Mac(32), 2, true, T0 + DAY)));
    let request = Mac(33).request();
    let held_out = Proven {
        request: &request,
        node: node(9),
        name: "m2",
        fleet: true,
        lan: false,
        fresh: false,
        book: Book {
            node_id_held_out: true,
            ..CURRENT
        },
    };
    assert!(refused(&decide(&log, &ME.id(), &held_out, T0 + DAY)));
}

/// What else goes to a person: a key two seats hold, this member's own node key, a proof of no
/// key, and a proof past this member's hourly share of admissions.
///
/// NEGATIVE CONTROL: the `AUTOMATIC_PER_HOUR` check in `decide` cut: the fifth proof within the
/// hour is let in and the last assert fails.
#[test]
fn contested_own_and_past_the_hourly_share_go_to_a_person() {
    let mut log = alab();
    node_record(&mut log, LISTED, 2, 4);
    assert_eq!(log.membership().node(&node(2)), NodeStanding::Contested);
    assert!(refused(&decided(&log, Mac(12), 2, true, T0)));
    assert!(refused(&decided(&log, Mac(11), 1, true, T0)));
    let request = Mac(10).request();
    let no_key = Proven {
        request: &request,
        node: [0; 32],
        name: "machine",
        fleet: true,
        lan: false,
        fresh: false,
        book: CURRENT,
    };
    assert!(refused(&decide(&log, &ME.id(), &no_key, T0)));
    for (n, tag) in (40..40 + AUTOMATIC_PER_HOUR as u8).enumerate() {
        let at = T0 + n as u64;
        let decision = decided(&log, Mac(tag), tag, true, at);
        assert_eq!(decision, Decision::Fleet);
        admit(
            &mut log,
            ME,
            Mac(tag),
            tag,
            &format!("m{tag}"),
            Evidence::Manual,
            at,
        );
    }
    assert!(refused(&decided(&log, Mac(50), 50, true, T0 + 10)));
}

/// Limits every member reads alike from the log, so the fleet's key cannot fill it: once the log
/// holds [`PROVEN_PER_DAY`] `Admit`s of `Manual` from the last day (by any signers) no member
/// lets another Mac in by proof until they age past a day; a log [`PROVEN_FILL`] full takes none;
/// and a node id the fleet book binds to another seat's key (a VM that re-mints its node key at
/// each boot) takes no new seat.
///
/// NEGATIVE CONTROLS: the `PROVEN_PER_DAY` check in `decide` cut: the Mac past the day's limit is
/// let in and "the day's limit" fails. `room_for_proven` comparing `<=` against the whole of
/// `MAX_RECORDS`: "three quarters full" fails. The `node_id_seated` check cut: "one seat a node
/// id" fails.
#[test]
fn the_log_holds_a_daily_limit_and_room_for_people() {
    let mut log = alab();
    let signers = [ME, SEAT, LISTED];
    for (n, tag) in (60..60 + PROVEN_PER_DAY as u8).enumerate() {
        let by = signers[n % signers.len()];
        let at = T0 + n as u64 * (DAY / 24);
        assert_eq!(decided(&log, Mac(tag), tag, true, at), Decision::Fleet);
        admit(
            &mut log,
            by,
            Mac(tag),
            tag,
            &format!("m{tag}"),
            Evidence::Manual,
            at,
        );
    }
    let late = T0 + PROVEN_PER_DAY as u64 * (DAY / 24);
    assert!(
        refused(&decided(&log, Mac(80), 80, true, late)),
        "the day's limit"
    );
    assert_eq!(
        decided(&log, Mac(80), 80, true, T0 + 2 * DAY),
        Decision::Fleet,
        "a day on, the limit has room again"
    );
    assert!(room_for_proven(MAX_RECORDS * 3 / 4 - 1));
    assert!(!room_for_proven(MAX_RECORDS * 3 / 4), "three quarters full");
    let request = Mac(81).request();
    let reborn = Proven {
        request: &request,
        node: node(81),
        name: "m81",
        fleet: true,
        lan: false,
        fresh: false,
        book: Book {
            node_id_seated: true,
            ..CURRENT
        },
    };
    assert!(
        refused(&decide(&log, &ME.id(), &reborn, T0 + 2 * DAY)),
        "one seat a node id"
    );
}

#[test]
fn proof_capacity_observation_matches_the_decision_at_its_boundary() {
    let mut log = alab();
    let limit = MAX_RECORDS * PROVEN_FILL.0 / PROVEN_FILL.1;
    while log.held_count() < limit - 1 {
        keep(
            &mut log,
            ME,
            T0,
            Body::Name {
                name: "capacity-observation".into(),
            },
        );
    }
    let before = ProofAdmissionCapacity::observe(&log);
    assert_eq!(before.complete_records, limit - 1);
    assert_eq!(before.exclusive_record_limit, limit);
    assert!(before.within_limit);
    assert_eq!(decided(&log, Mac(99), 99, true, T0), Decision::Fleet);

    keep(
        &mut log,
        ME,
        T0,
        Body::Name {
            name: "capacity-boundary".into(),
        },
    );
    let at = ProofAdmissionCapacity::observe(&log);
    assert_eq!(at.complete_records, limit);
    assert!(!at.within_limit);
    assert!(refused(&decided(&log, Mac(99), 99, true, T0)));
}

/// 0.147's reader in small: the record framing and the kind and evidence codes v0.147.0's
/// `membership_log.rs` decodes (`Kind::from_code` 1..=9, `Evidence::from_code` 1..=3; the framing
/// has not changed since). Each record's kind code and, for an `Admit`, its evidence code; or why
/// a 0.147 Mac refuses the whole stream.
fn reader_0147(mut wire: &[u8]) -> Result<Vec<(u8, Option<u8>)>, String> {
    fn take<'a>(wire: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
        if wire.len() < n {
            return Err("truncated".to_owned());
        }
        let (head, rest) = wire.split_at(n);
        *wire = rest;
        Ok(head)
    }
    let mut out = Vec::new();
    while !wire.is_empty() {
        if take(&mut wire, 4)? != b"ALM1" || take(&mut wire, 1)? != [1] {
            return Err("not a membership record".to_owned());
        }
        take(&mut wire, 32)?;
        let kind = take(&mut wire, 1)?[0];
        if !(1..=9).contains(&kind) {
            return Err(format!("membership record of an unknown kind {kind}"));
        }
        take(&mut wire, 48 + 8 + 32)?;
        let parents = usize::from(take(&mut wire, 1)?[0]);
        take(&mut wire, parents * 32 + 8)?;
        let len = take(&mut wire, 4)?;
        let body = take(
            &mut wire,
            u32::from_be_bytes(len.try_into().unwrap()) as usize,
        )?;
        take(&mut wire, 64)?;
        let evidence = (kind == 2).then(|| body.last().copied().unwrap_or(0));
        if let Some(code) = evidence
            && !(1..=3).contains(&code)
        {
            return Err(format!(
                "membership admission of an unknown evidence {code}"
            ));
        }
        out.push((kind, evidence));
    }
    Ok(out)
}

/// End to end on two real vault folders (synthetic keys, a loopback server): m7 publishes its
/// fleet node key in its own `Node` record, then its vault key changes (a fresh vault folder,
/// as after a reinstall). m22, shown the proof of that node key alone, lets it back in with
/// nobody: under the label m7, with the node key, reading `[everyone]` at once. Every record m22
/// signs and every record the Mac takes is one a 0.147 Mac reads (`Remove`, an `Admit` of
/// `Manual`, `Wrap`s).
///
/// NEGATIVE CONTROLS: `admit_by_proof` signing the `Admit` without the `Remove` of the old seat
/// (`let mut signed = Vec::new();` in place of the insert and `vec![remove]`): the evaluator
/// labels the new key `m7-<hex>` beside the old seat, and the `Let` match fails with
/// `Refused("the membership log would seat this Mac as m7-…")`. `Evidence::Manual` coded 4 on the
/// wire (both `code` and `from_code`): this build reads it, the 0.147 reader refuses the stream
/// with "membership admission of an unknown evidence 4".
#[test]
fn a_mac_whose_vault_key_changed_takes_its_seat_back_by_proof_with_nobody() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_, old, _) = member_key(m7);
    let pair = node_pair(7);
    let n7 = node(7);
    publish_local_node_key(m7, n7, |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .expect("m7 publishes its node key")
    .expect("a Node record");
    post_records_to_servers(m7).expect("m7 posts its records");
    sync_records(m22, None).expect("m22 pulls m7's Node record");

    let (fresh, request, _) = newcomer(macs.root.path(), m22, "m7-fresh");
    let proven = Proven {
        request: &request,
        node: n7,
        name: "m7",
        fleet: false,
        lan: false,
        fresh: false,
        book: CURRENT,
    };
    // A day past m7's last record (its `Node`), so the old seat is dark.
    let now = aterm_time::unix_millis().unwrap() + DARK_FOR_MS + 1;
    let let_in = admit_by_proof(m22, &proven, now).expect("admit by proof");
    let Readmit::Let {
        bundle,
        how,
        signed,
    } = let_in
    else {
        panic!("m7's new key is not let in: {let_in:?}");
    };
    assert_eq!(
        how,
        How::Seat {
            label: "m7".to_owned()
        }
    );
    assert!(signed >= 3, "a Remove, an Admit and the Wraps: {signed}");
    let bundle = crate::members::Bundle::decode(bundle.as_slice()).expect("bundle");
    write_from_log_bundle(&fresh, &bundle).expect("the Mac writes its vault from the log");
    assert_eq!(load_state(&fresh).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(fresh.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );
    let store = load_records(&fresh).unwrap();
    let membership = store.log().unwrap().membership();
    let me = request.binding().actor();
    assert_eq!(membership.label(&me), Some("m7"), "the label carries over");
    assert_eq!(membership.node(&n7), NodeStanding::Member(me));
    assert!(!membership.contains(&old), "the old seat is out");

    let read = reader_0147(&store.encode()).expect("a 0.147 Mac reads every record it takes");
    let tail = &read[read.len() - signed..];
    assert_eq!(tail[0], (3, None), "a Remove of the old seat first");
    assert_eq!(tail[1], (2, Some(2)), "then an Admit of Manual");
    assert!(
        tail[2..].iter().all(|r| *r == (6, None)),
        "then Wraps: {tail:?}"
    );

    // The Mac the move left out (m7's old vault folder) is offered no Undo of it.
    sync_records(m7, None).expect("m7 pulls the move");
    assert!(
        undo_offers(m7).expect("undo offers").is_empty(),
        "no Undo of a seat move"
    );

    let again = admit_by_proof(m22, &proven, now + 1).expect("again");
    assert!(
        matches!(
            again,
            Readmit::Let {
                how: How::Already,
                signed: 0,
                ..
            }
        ),
        "a second run signs nothing: {again:?}"
    );
    macs.server.stop();
    drop(macs.root);
}

/// A member that reads the vault's newest records from no server or peer when a proof comes may
/// not know of a removal (an asleep laptop, or one away with only the removed Mac), so it lets no
/// new key in by proof; with its server back it does.
///
/// NEGATIVE CONTROL: `admit_by_proof` taking every read as fresh (`let fresh = true;`): m22 with
/// no server reachable moves the seat and the `Refused` match fails.
#[test]
fn a_member_that_reads_no_newer_records_lets_no_new_key_in() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let pair = node_pair(9);
    let n7 = node(9);
    publish_local_node_key(m7, n7, |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .expect("m7 publishes its node key")
    .expect("a Node record");
    post_records_to_servers(m7).expect("m7 posts its records");
    sync_records(m22, None).expect("m22 pulls m7's Node record");
    let (_, request, _) = newcomer(macs.root.path(), m22, "m7-fresh");
    let proven = Proven {
        request: &request,
        node: n7,
        name: "m7",
        fleet: false,
        lan: false,
        fresh: false,
        book: CURRENT,
    };
    let later = aterm_time::unix_millis().unwrap() + DARK_FOR_MS + 1;
    macs.server.stop();
    let refused = admit_by_proof(m22, &proven, later).expect("decided");
    let Readmit::Refused(why) = &refused else {
        panic!("a member that reads no server let the new key in: {refused:?}");
    };
    assert!(why.contains("newest records"), "{why}");
    drop(macs.root);
}

/// A brand-new Mac proves itself first and only then asks plainly, an ask that waits for a person's
/// Allow. A member that refuses a Mac whatever it proves next says so
/// before its possession challenge, so nothing was asked of `decide`, the Mac sent nothing a member
/// could sign on (`Missed::signed_maybe` false) and no server is read: a stranger, and a seat whose
/// Mac signed in the last day, which a person decides.
///
/// A refusal once possession has gone stays ambiguous, whatever its line says: nothing authenticates
/// a `NO`, and a forged one after a real signature must not send the Mac to a second member, where
/// two removals of one seat make a quorum `Remove`. So does the book's refusal of a seat members
/// older than 0.148 would read as removed: it moves by itself, and no person is asked.
///
/// NEGATIVE CONTROLS: the `refuses` check removed from `desk::prove`: `decide` runs and
/// `signed_maybe` is true for each refusal this expects early. `moved_seats_read` left as the book
/// says in `refuses_before_possession_at_ms`: the dark seat is refused early. `joiner::prove`
/// clearing `sent` on any `NO`: the seated Mac's `signed_maybe` is false. The `unclaimed` answer
/// removed: the Mac with no claim reads the book's sentence. No `read_newest` in the dry run: the
/// `unread` Mac goes on to possession. A missing signer read as `None`: `said` is `None`.
#[test]
fn a_stranger_is_refused_before_possession_and_a_refusal_after_it_stays_ambiguous() {
    use crate::members::desk::{self, Desk, Local};
    use crate::members::join::{self, Stream};
    use crate::members::joiner::Joining;
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (macs.m22.clone(), macs.m7.clone());
    let pair = node_pair(8);
    publish_local_node_key(&m7, node(8), |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .expect("m7 publishes its node key")
    .expect("a Node record");
    post_records_to_servers(&m7).expect("m7 posts its records");
    sync_records(&m22, None).expect("m22 pulls m7's Node record");
    let fresh = Paths::for_home(&macs.root.path().join("new-mac"));
    machine_identity(&fresh).expect("identity");
    let device = crate::commons::bootstrap::Device::open(&fresh)
        .expect("device")
        .expect("an identity");
    let prefix = format!("aterm-fleet/v3 record {}\0", desk::PROVE_RECORD);
    let asked = std::sync::atomic::AtomicUsize::new(0);
    let decide = |_: &desk::Proving<'_>| -> crate::Result<crate::SecretBytes> {
        asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(crate::Error::new("this member refused after possession"))
    };
    let tried =
        |node_key: [u8; 32], signer: &Ed25519KeyPair, fleet: Option<desk::Fleet>, at: u64| {
            let joining = Joining {
                device: &device,
                node: node_key,
                name: "new-mac".to_owned(),
                model: "Mac17,6".to_owned(),
            };
            let sign_node = |message: &[u8]| -> [u8; 64] {
                signer
                    .sign(&[prefix.as_bytes(), message].concat())
                    .as_ref()
                    .try_into()
                    .unwrap()
            };
            let hearing = std::sync::Mutex::new(Desk::new(
                Local::of(&m22)
                    .expect("m22's desk")
                    .expect("m22 is a member"),
            ));
            std::thread::scope(|scope| {
                let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
                    let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
                    for end in [&ours, &theirs] {
                        end.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
                    }
                    let (hearing, decide, member) = (&hearing, &decide, &m22);
                    scope.spawn(move || {
                        let refuses = |proving: &desk::Proving<'_>| {
                            refuses_before_possession_at_ms(member, proving, Book::default(), at)
                        };
                        let _ = desk::serve_with(
                            hearing,
                            &mut ours,
                            std::net::IpAddr::from([192, 0, 2, 22]),
                            &|_| Err(crate::Error::new("no phrase runs here")),
                            Some(desk::Proof {
                                decide,
                                refuses: &refuses,
                                fleet,
                            }),
                        );
                    });
                    Ok(Box::new(theirs))
                };
                match join::prove_first(&fresh, &dial, &joining, &sign_node, &["m22:7".to_owned()])
                {
                    Ok(_) => panic!("this Mac was let in by its proof"),
                    Err(missed) => missed,
                }
            })
        };
    let count = || asked.load(std::sync::atomic::Ordering::SeqCst);

    let now = aterm_time::unix_millis().unwrap();
    let later = now + 2 * DARK_FOR_MS;
    let stranger_pair = node_pair(9);

    // The fleet's channel proved this one: its log may yet seat it, so the decision runs.
    let over_fleet = tried(
        node(9),
        &stranger_pair,
        Some(desk::Fleet {
            node: node(9),
            current: true,
        }),
        now,
    );
    assert_eq!(count(), 1, "{over_fleet:?}");
    assert!(over_fleet.signed_maybe, "{over_fleet:?}");

    // The seat's Mac signed just now: whatever this one proves a person decides, so it may ask.
    let running = tried(node(8), &pair, None, now);
    assert_eq!(count(), 1, "{running:?}");
    assert!(!running.signed_maybe, "{running:?}");
    assert!(
        running.refused[0].1.contains("in the last day"),
        "{running:?}"
    );

    // A dark seat the book would not let move, and any refusal past possession, stay ambiguous.
    let seated = tried(node(8), &pair, None, later);
    assert_eq!(count(), 2, "{seated:?}");
    assert!(seated.signed_maybe, "{seated:?}");

    // No server answers: a stranger's refusal does not wait on one, and a member that may not know
    // of a removal says so before anything could be signed.
    macs.server.stop();
    let stranger = tried(node(9), &stranger_pair, None, now);
    assert_eq!(count(), 2, "a stranger reached the decision: {stranger:?}");
    assert!(!stranger.signed_maybe, "{stranger:?}");
    assert!(
        stranger.refused[0]
            .1
            .contains("no seat binds this node key"),
        "{stranger:?}"
    );
    let unread = tried(node(8), &pair, None, later);
    assert_eq!(count(), 2, "{unread:?}");
    assert!(!unread.signed_maybe, "{unread:?}");
    assert!(unread.refused[0].1.contains("newest records"), "{unread:?}");

    // A Mac with no claim learns nothing else of the log, whatever `decide` would have said of it.
    let request = Mac(33).request();
    let unread = std::cell::Cell::new(false);
    let proving = |node_key: [u8; 32]| desk::Proving {
        request: &request,
        node: node_key,
        name: "new-mac",
        fleet: false,
        lan: false,
        fresh: &unread,
    };
    let seated_elsewhere = Book {
        node_id_seated: true,
        ..Book::default()
    };
    let said = refuses_before_possession_at_ms(&m22, &proving(node(9)), seated_elsewhere, now);
    assert!(
        said.as_deref()
            .is_some_and(|why| why.contains("no seat binds")),
        "{said:?}"
    );

    // A member with no signer cannot sign, so it says so while the Mac may still go elsewhere.
    let device = m22.dir.join("commons").join("device.age");
    std::fs::rename(&device, device.with_extension("gone")).unwrap();
    let said = refuses_before_possession_at_ms(&m22, &proving(node(8)), Book::default(), later);
    assert!(
        said.as_deref()
            .is_some_and(|why| why.contains("admits nobody")),
        "{said:?}"
    );
    drop(macs.root);
}

/// A Commons member with no membership log can still hear a plain ask and hand over files.
/// Its proof refusal must precede possession, so that the new Mac reaches that plain ask.
///
/// NEGATIVE CONTROL: let `refuses_before_possession_at_ms` return `None` for an absent log:
/// the real proof exchange ends with `signed_maybe`, and no plain ask can follow it.
#[test]
fn a_member_without_a_log_refuses_before_possession_so_the_mac_can_ask() {
    use crate::commons::bootstrap::Device;
    use crate::members::desk::{self, Desk, Local};
    use crate::members::join::{self, Stream};
    use crate::members::joiner::{self, Joining};

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let vault = root.path().join("alab.vault");
    let member = Paths::for_home(&root.path().join("member"));
    test_create_unbound(&member, vault.to_str().unwrap(), None, Some("member"))
        .expect("create vault");
    admin_keys::legacy::publish_legacy(&member, &admin_open(&member).unwrap());
    admin_commons_init(&member).expect("Commons init");
    assert!(load_records(&member).unwrap().log().is_none(), "no genesis");
    let local = Local::of(&member)
        .unwrap()
        .expect("a Commons member's desk");
    assert!(local.can_allow(), "the pre-log Commons member can allow");
    let desk = std::sync::Mutex::new(Desk::new(local));
    let fresh = Paths::for_home(&root.path().join("fresh"));
    machine_identity(&fresh).unwrap();
    let device = Device::open(&fresh).unwrap().unwrap();
    let joining = Joining {
        device: &device,
        node: node(9),
        name: "fresh".to_owned(),
        model: "Mac17,6".to_owned(),
    };
    let pair = node_pair(9);
    let prefix = format!("aterm-fleet/v3 record {}\0", desk::PROVE_RECORD);
    let sign_node = |message: &[u8]| -> [u8; 64] {
        pair.sign(&[prefix.as_bytes(), message].concat())
            .as_ref()
            .try_into()
            .unwrap()
    };
    std::thread::scope(|scope| {
        let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
            let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(Duration::from_secs(60)))?;
            }
            let (desk, member) = (&desk, &member);
            scope.spawn(move || {
                let decide =
                    |proving: &desk::Proving<'_>| prove_at_ms(member, proving, CURRENT, T0);
                let refuses = |proving: &desk::Proving<'_>| {
                    refuses_before_possession_at_ms(member, proving, CURRENT, T0)
                };
                let _ = desk::serve_with(
                    desk,
                    &mut ours,
                    "192.168.1.19".parse().unwrap(),
                    &|_| Err(crate::Error::new("no phrase runs here")),
                    Some(desk::Proof {
                        decide: &decide,
                        refuses: &refuses,
                        fleet: None,
                    }),
                );
            });
            Ok(Box::new(theirs))
        };
        let tried = join::prove_first(
            &fresh,
            &dial,
            &joining,
            &sign_node,
            &["member:7".to_owned()],
        );
        let missed = match tried {
            Ok(_) => panic!("a member without a log cannot admit by proof"),
            Err(missed) => missed,
        };
        assert!(
            !missed.signed_maybe,
            "a plain ask must remain possible: {missed:?}"
        );

        let mut stream = dial("member:7").unwrap();
        let greeting = joiner::greeting(&mut stream).unwrap();
        joiner::ask(&mut stream, &greeting, &joining).expect("plain ask after proof refusal");
    });
    let rows = desk.lock().unwrap().rows(aterm_time::Instant::now());
    assert_eq!(rows.len(), 1, "the member hears the new Mac's plain ask");
    assert_eq!(rows[0].name, "fresh");
}

/// The whole path a Mac whose vault key changed takes, on two real vault folders: m22's desk
/// serves `prove` with [`prove_at`] on its connection thread, and the fresh Mac's
/// `members::join::prove_first` proves m7's node key over the desk's own wire (a Unix socket
/// pair here), takes the sealed log bundle, writes its vault and is in as m7, with nobody. A
/// second Mac that only copied the node key's public half gets nothing.
///
/// NEGATIVE CONTROL: `prove_first` taking the bundle without writing it
/// (`let report = Report::default();` for `client::write_from_log_bundle(paths, &bundle)?`): the
/// fresh Mac's state is not `In` and the `Stored::In` assert fails.
#[test]
fn a_fresh_vault_key_proves_its_node_key_to_a_members_desk_and_is_in() {
    use crate::members::desk::{self, Desk, Local};
    use crate::members::join::{self, Stream};
    use crate::members::joiner::Joining;
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let pair = node_pair(8);
    let n7 = node(8);
    publish_local_node_key(m7, n7, |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .expect("m7 publishes its node key")
    .expect("a Node record");
    post_records_to_servers(m7).expect("m7 posts its records");
    sync_records(m22, None).expect("m22 pulls m7's Node record");
    let desk = std::sync::Mutex::new(Desk::new(
        Local::of(m22)
            .expect("m22's desk")
            .expect("m22 is a member"),
    ));

    let fresh = Paths::for_home(&macs.root.path().join("m7-fresh"));
    machine_identity(&fresh).expect("identity");
    let device = crate::commons::bootstrap::Device::open(&fresh)
        .expect("device")
        .expect("an identity");
    let joining = Joining {
        device: &device,
        node: n7,
        name: "m7".to_owned(),
        model: "Mac15,3".to_owned(),
    };
    let prefix = format!("aterm-fleet/v3 record {}\0", desk::PROVE_RECORD);
    let sign_node = |message: &[u8]| -> [u8; 64] {
        pair.sign(&[prefix.as_bytes(), message].concat())
            .as_ref()
            .try_into()
            .unwrap()
    };
    // A day past m7's last record (its `Node`), so the old seat is dark.
    let later = aterm_time::unix_millis().unwrap() + DARK_FOR_MS + 1;
    let decide = |proving: &desk::Proving<'_>| prove_at_ms(m22, proving, CURRENT, later);
    let served = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
            let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
            }
            let (desk, decide, served) = (&desk, &decide, &served);
            scope.spawn(move || {
                let ended = desk::serve_with(
                    desk,
                    &mut ours,
                    std::net::IpAddr::from([192, 0, 2, 22]),
                    &|_| Err(crate::Error::new("no phrase runs here")),
                    Some(desk::Proof {
                        decide,
                        refuses: &|_| None,
                        fleet: None,
                    }),
                );
                served.lock().unwrap().push(ended.is_ok());
            });
            Ok(Box::new(theirs))
        };
        let joined = join::prove_first(&fresh, &dial, &joining, &sign_node, &["m22:7".to_owned()]);
        let joined = match joined {
            Ok(joined) => joined,
            Err(missed) => panic!("no member let the fresh key in: {missed:?}"),
        };
        assert_eq!(joined.name, "m7");
        assert!(matches!(joined.admission, join::Admission::Logged));

        // A Mac holding only the node key's public half signs nothing that verifies.
        let other = Paths::for_home(&macs.root.path().join("copier"));
        machine_identity(&other).expect("identity");
        let other_device = crate::commons::bootstrap::Device::open(&other)
            .expect("device")
            .expect("an identity");
        let copier = Joining {
            device: &other_device,
            node: n7,
            name: "m7".to_owned(),
            model: "Mac15,3".to_owned(),
        };
        let zero = |_: &[u8]| [0u8; 64];
        let refused = join::prove_first(&other, &dial, &copier, &zero, &["m22:7".to_owned()]);
        assert!(refused.is_err(), "a copied public node key proves nothing");
    });
    assert_eq!(load_state(&fresh).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(fresh.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );
    let membership = load_records(&fresh).unwrap().log().unwrap().membership();
    let me = device
        .request(
            crate::commons::bootstrap::Bootstrap::required(m22)
                .unwrap()
                .trust()
                .network,
            crate::commons::bootstrap::Bootstrap::required(m22)
                .unwrap()
                .trust()
                .authority,
        )
        .unwrap()
        .binding()
        .actor();
    assert_eq!(membership.label(&me), Some("m7"));
    macs.server.stop();
    drop(macs.root);
}

/// When `read_newest` pulls records that either deny the proving Mac in `aterm-swarm-roster` or
/// remove the answering member itself, [`refuses_before_possession_at_ms`] checks the roster and
/// local signer again before challenging possession, so a newcomer reaches the next healthy member
/// instead of halting on an ambiguous post-possession `NO`.
///
/// NEGATIVE CONTROLS: re-running only `decide` after `read_newest`: the roster-denied Mac goes on
/// to possession (`missed.signed_maybe` is true), and the removed `m7` challenges possession first
/// so `prove_first` stops at `a-m7:7` and never reaches `b-m22:7`.
#[test]
fn a_member_whose_newest_records_keep_it_or_the_mac_out_refuses_before_possession() {
    use crate::members::desk::{self, Desk, Local};
    use crate::members::join::{self, Stream};
    use crate::members::joiner::Joining;

    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (macs.m22.clone(), macs.m7.clone());
    let now = aterm_time::unix_millis().unwrap();
    let prefix = format!("aterm-fleet/v3 record {}\0", desk::PROVE_RECORD);

    let desk_m7 = std::sync::Mutex::new(Desk::new(
        Local::of(&m7).expect("m7's desk").expect("m7 is a member"),
    ));
    let desk_m22 = std::sync::Mutex::new(Desk::new(
        Local::of(&m22)
            .expect("m22's desk")
            .expect("m22 is a member"),
    ));

    // 1. m22 publishes a roster deny for node(11) in the log before m7 syncs.
    let denied_roster = format!(
        "deny_nodes = [\"{}\"]\n",
        aterm_codec::hex::encode_lower(&node(11))
    );
    test_put_everyone(&m22, crate::SWARM_ROSTER_ENTRY, denied_roster.as_bytes())
        .expect("m22 writes roster deny");
    post_records_to_servers(&m22).expect("m22 posts roster deny");

    let denied_mac = Paths::for_home(&macs.root.path().join("denied-mac"));
    machine_identity(&denied_mac).expect("identity");
    let denied_device = crate::commons::bootstrap::Device::open(&denied_mac)
        .expect("device")
        .expect("an identity");
    let denied_joining = Joining {
        device: &denied_device,
        node: node(11),
        name: "denied-mac".to_owned(),
        model: "Mac17,6".to_owned(),
    };
    let denied_pair = node_pair(11);
    let sign_denied = |message: &[u8]| -> [u8; 64] {
        denied_pair
            .sign(&[prefix.as_bytes(), message].concat())
            .as_ref()
            .try_into()
            .unwrap()
    };

    std::thread::scope(|scope| {
        let dial = |target: &str| -> crate::Result<Box<dyn Stream>> {
            let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
            }
            let (desk, member) = match target {
                "a-m7:7" => (&desk_m7, &m7),
                "b-m22:7" => (&desk_m22, &m22),
                other => panic!("unexpected target: {other}"),
            };
            scope.spawn(move || {
                let decide =
                    |proving: &desk::Proving<'_>| prove_at_ms(member, proving, CURRENT, now);
                let refuses = |proving: &desk::Proving<'_>| {
                    refuses_before_possession_at_ms(member, proving, CURRENT, now)
                };
                let _ = desk::serve_with(
                    desk,
                    &mut ours,
                    std::net::IpAddr::from([192, 0, 2, 22]),
                    &|_| Err(crate::Error::new("no phrase runs here")),
                    Some(desk::Proof {
                        decide: &decide,
                        refuses: &refuses,
                        fleet: Some(desk::Fleet {
                            node: node(11),
                            current: true,
                        }),
                    }),
                );
            });
            Ok(Box::new(theirs))
        };
        let missed = join::prove_first(
            &denied_mac,
            &dial,
            &denied_joining,
            &sign_denied,
            &["a-m7:7".to_owned()],
        )
        .expect_err("roster-denied node key must be refused");
        assert!(
            !missed.signed_maybe,
            "roster deny synced via read_newest must refuse before possession: {missed:?}"
        );
        assert!(
            missed.refused[0].1.contains("the vault removed this Mac"),
            "{missed:?}"
        );
    });

    // 2. m22 removes m7 on the server before m7 syncs; a newcomer dialling m7 first moves on to m22.
    remove_member(&m22, "m7", false).expect("m22 removes m7");
    post_records_to_servers(&m22).expect("m22 posts m7 removal");

    let fresh = Paths::for_home(&macs.root.path().join("fresh-fleet-mac"));
    machine_identity(&fresh).expect("identity");
    let fresh_device = crate::commons::bootstrap::Device::open(&fresh)
        .expect("device")
        .expect("an identity");
    let fresh_joining = Joining {
        device: &fresh_device,
        node: node(12),
        name: "fresh-mac".to_owned(),
        model: "Mac17,6".to_owned(),
    };
    let fresh_pair = node_pair(12);
    let sign_fresh = |message: &[u8]| -> [u8; 64] {
        fresh_pair
            .sign(&[prefix.as_bytes(), message].concat())
            .as_ref()
            .try_into()
            .unwrap()
    };

    let dialled = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        let dial = |target: &str| -> crate::Result<Box<dyn Stream>> {
            dialled.lock().unwrap().push(target.to_owned());
            let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
            }
            let (desk, member) = match target {
                "a-m7:7" => (&desk_m7, &m7),
                "b-m22:7" => (&desk_m22, &m22),
                other => panic!("unexpected target: {other}"),
            };
            scope.spawn(move || {
                let decide =
                    |proving: &desk::Proving<'_>| prove_at_ms(member, proving, CURRENT, now);
                let refuses = |proving: &desk::Proving<'_>| {
                    refuses_before_possession_at_ms(member, proving, CURRENT, now)
                };
                let _ = desk::serve_with(
                    desk,
                    &mut ours,
                    std::net::IpAddr::from([192, 0, 2, 22]),
                    &|_| Err(crate::Error::new("no phrase runs here")),
                    Some(desk::Proof {
                        decide: &decide,
                        refuses: &refuses,
                        fleet: Some(desk::Fleet {
                            node: node(12),
                            current: true,
                        }),
                    }),
                );
            });
            Ok(Box::new(theirs))
        };
        let joined = join::prove_first(
            &fresh,
            &dial,
            &fresh_joining,
            &sign_fresh,
            &["a-m7:7".to_owned(), "b-m22:7".to_owned()],
        )
        .expect("removed m7 refuses before possession and m22 admits the newcomer");
        assert!(matches!(joined.admission, join::Admission::Logged));
    });
    assert_eq!(
        *dialled.lock().unwrap(),
        vec!["a-m7:7".to_owned(), "b-m22:7".to_owned()]
    );
    assert_eq!(load_state(&fresh).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(fresh.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );
    macs.server.stop();
    drop(macs.root);
}

/// An unset clock (`now_ms == 0`) refuses every proof decision instead of signing permanent
/// membership-log records stamped at epoch zero.
///
/// NEGATIVE CONTROL: `decide` without `if now_ms == 0` admits `Mac(9)` over the fleet or wraps
/// `ME` with `authored_ms = 0`, failing these assertions.
#[test]
fn unset_clock_refuses_proof_decisions() {
    let log = alab();
    assert!(
        matches!(decided(&log, Mac(9), 9, true, 0), Decision::Refuse(why) if why.contains("clock"))
    );
    assert!(
        matches!(decided(&log, ME, 9, false, 0), Decision::Refuse(why) if why.contains("clock"))
    );
}

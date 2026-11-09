// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::log_entries_tests::{TwoMacs, join_host, member_key, newcomer, two_macs};
use super::*;
use crate::age;
use crate::membership_log::{
    Draft, Founder, Inserted, LEGACY_RECORDS, MAX_HELD_RECORDS, RecordBudgets, Root,
    node_proof_message,
};
use ring::signature::{Ed25519KeyPair, KeyPair as _};

const DAY: u64 = 24 * 60 * 60 * 1000;
const T0: u64 = 1_000 * DAY;

#[path = "proof_admission_tests.rs"]
mod proof_admission;

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
    decided_with_wraps(log, mac, tag, fleet, 0, at)
}

/// [`decided`] where `ME`'s admission signs `wraps` `Wrap`s beside its `Admit`.
fn decided_with_wraps(
    log: &Log,
    mac: Mac,
    tag: u8,
    fleet: bool,
    wraps: usize,
    at: u64,
) -> Decision {
    let request = mac.request();
    decide(
        log,
        &ME.id(),
        &Proven {
            reader: crate::members::bundle::Reader::BoundedLog(
                crate::crossing::RecordsBound::uncrossed(),
            ),
            request: &request,
            node: node(tag),
            name: "machine",
            fleet,
            lan: false,
            fresh: false,
            book: CURRENT,
        },
        wraps,
        at,
    )
}

/// [`decided`] for a proof from another host on the member's home ALab LAN, with no fleet key.
fn decided_on_lan(log: &Log, mac: Mac, tag: u8, at: u64) -> Decision {
    on_lan_with_wraps(log, mac, tag, 0, at)
}

/// [`decided_on_lan`] where `ME`'s admission signs `wraps` `Wrap`s beside its `Admit`.
fn on_lan_with_wraps(log: &Log, mac: Mac, tag: u8, wraps: usize, at: u64) -> Decision {
    let request = mac.request();
    decide(
        log,
        &ME.id(),
        &Proven {
            reader: crate::members::bundle::Reader::BoundedLog(
                crate::crossing::RecordsBound::uncrossed(),
            ),
            request: &request,
            node: node(tag),
            name: "machine",
            fleet: false,
            lan: true,
            fresh: false,
            book: CURRENT,
        },
        wraps,
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
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
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
        refused(&decide(&log, &ME.id(), &old_member_heard, 0, T0)),
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
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
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
        refused(&decide(&log, &ME.id(), &elsewhere, 0, T0 + 2 * DAY)),
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

/// A founder the genesis lists with its recipient and neither a binding nor a node key, as it
/// lists m9 (one-mesh S1): the Mac that holds that recipient (the possession challenge proves it)
/// proves itself from the home ALab LAN and takes the founder's own label, `m9`, not the next free
/// one. Off the LAN with no fleet key it is told no seat, as any Mac is; a Mac with another
/// recipient on the LAN takes a new label.
///
/// NEGATIVE CONTROL: `decide` matching an unproved founder by node key alone (its `by_recipient`
/// arm cut): the LAN proof is `Decision::Lan` and the first assert fails.
#[test]
fn a_founder_listed_with_no_node_key_comes_back_by_its_recipient_under_its_label() {
    const M9: Mac = Mac(9);
    let root = Root::of(ME.request().binding());
    let founders = vec![
        founder(ME, "m1", Some(node(1))),
        Founder {
            recipient: M9.identity().recipient(),
            binding: None,
            node_key: None,
            name: "m9".to_owned(),
            label: "m9".to_owned(),
        },
    ];
    let mut log = Log::new(Record::genesis(&[99; 32], root, founders, 1).unwrap()).unwrap();
    let decision = decided_on_lan(&log, M9, 0x39, T0);
    assert_eq!(
        decision,
        Decision::Founder {
            label: "m9".to_owned()
        }
    );
    assert!(
        refused(&decided(&log, M9, 0x39, false, T0)),
        "off the LAN with no fleet key"
    );
    assert_eq!(decided_on_lan(&log, Mac(19), 0x39, T0), Decision::Lan);
    apply(&mut log, M9, 0x39, &decision, T0);
    let membership = log.membership();
    assert_eq!(membership.label(&M9.id()), Some("m9"));
    assert_eq!(
        membership.get(&M9.id()).map(|m| m.binding.recipient()),
        Some(M9.identity().recipient()),
        "the seat binds the founder's own recipient"
    );
    assert!(membership.unproved().is_empty(), "the founder is proved");
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
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
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
    assert!(refused(&decide(&log, &ME.id(), &held_out, 0, T0 + DAY)));
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
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
        request: &request,
        node: [0; 32],
        name: "machine",
        fleet: true,
        lan: false,
        fresh: false,
        book: CURRENT,
    };
    assert!(refused(&decide(&log, &ME.id(), &no_key, 0, T0)));
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
/// lets another Mac in by proof until they age past a day; and a node id the fleet book binds to
/// another seat's key (a VM that re-mints its node key at each boot) takes no new seat. The
/// record budgets' share is `proof_waits_below_three_quarters_of_each_budget`'s.
///
/// NEGATIVE CONTROLS: the `PROVEN_PER_DAY` check in `decide` cut: the Mac past the day's limit is
/// let in and "the day's limit" fails. The `node_id_seated` check cut: "one seat a node id"
/// fails.
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
    let request = Mac(81).request();
    let reborn = Proven {
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
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
        refused(&decide(&log, &ME.id(), &reborn, 0, T0 + 2 * DAY)),
        "one seat a node id"
    );
}

/// `ME`'s `Name`s, each after the last, until `log` holds `to` records: keyed records, as many
/// as a test needs, drafted after the previous one rather than from every head.
fn fill_names(log: &mut Log, to: usize) {
    if log.held_count() >= to {
        return;
    }
    let name = || Body::Name {
        name: "filler".into(),
    };
    let mut last = keep(log, ME, T0, name());
    while log.held_count() < to {
        let record = Draft {
            network: *log.network(),
            sequence: last.sequence() + 1,
            previous: Some(*last.id()),
            parents: vec![*last.id()],
            authored_ms: T0,
            body: name(),
        }
        .sign(&ME.seed(), ME.incarnation())
        .unwrap();
        assert_eq!(log.insert(record.clone()).unwrap(), Inserted::Held);
        last = record;
    }
}

/// `ME` admits `mac` by a person's phrase, which no proof's share counts.
fn admit_by_phrase(log: &mut Log, mac: Mac, tag: u8) -> Record {
    keep(
        log,
        ME,
        T0,
        Body::Admit {
            entrant: Entrant {
                request: mac.request(),
                node_key: node(tag),
                name: format!("m{tag}"),
                label: format!("m{tag}"),
            },
            evidence: Evidence::Phrase,
        },
    )
}

/// ALab's log as it stood on 2026-11-03, when every member refused a Mac on the home LAN: 36
/// weighed records (the genesis, 27 `Admit`s, 8 `Remove`s) and 4,035 keyed ones, 4,071 in all.
/// The keyed records here are `ME`'s `Name`s, which the budgets count as they count `Wrap`s.
fn live_shape() -> Log {
    let mut log = alab();
    for tag in 100..126 {
        let admit = admit_by_phrase(&mut log, Mac(tag), tag);
        if tag < 108 {
            keep(
                &mut log,
                ME,
                T0,
                Body::Remove {
                    member: Mac(tag).id(),
                    cutoff: *admit.id(),
                },
            );
        }
    }
    fill_names(&mut log, 4_071);
    log
}

/// A Mac on the home LAN comes into a log shaped like ALab's on 2026-11-03 under the production
/// budgets: its 36 weighed records are far below three quarters of theirs, and its 4,035 keyed
/// ones below three quarters of theirs, and the Admit with three Wraps fits the held total.
///
/// NEGATIVE CONTROL (in the test): the reserve as it was, counted over every kind against one
/// bound of 4,096 records, refuses this log; and the capacity read with every keyed record
/// counted as weighed reports the weighed budget three quarters full.
#[test]
fn a_lan_mac_comes_into_a_log_shaped_like_alab_on_2026_11_03() {
    let log = live_shape();
    assert_eq!(log.budgets(), RecordBudgets::PRODUCTION);
    assert_eq!(
        log.counts(),
        RecordCounts {
            weighed: 36,
            keyed: 4_035
        }
    );
    let capacity = ProofAdmissionCapacity::observe(&log);
    assert_eq!(
        (
            capacity.weighed_limit,
            capacity.keyed_limit,
            capacity.held_cap
        ),
        (3_072, 9_216, MAX_HELD_RECORDS)
    );
    assert!(capacity.within_limit);
    assert_eq!(capacity.reserved(), None);
    assert_eq!(
        on_lan_with_wraps(&log, Mac(200), 200, 3, T0),
        Decision::Lan,
        "the LAN Mac is let in"
    );

    let over_all_kinds = log.held_count() * PROVEN_FILL.1 < LEGACY_RECORDS * PROVEN_FILL.0;
    assert!(!over_all_kinds, "today's reserve refuses this log");
    let weighed_as_all = ProofAdmissionCapacity {
        weighed_records: log.held_count(),
        ..capacity
    };
    assert_eq!(weighed_as_all.reserved(), Some(Budget::Weighed));
}

/// Under small budgets, a proof is considered while each of the weighed and keyed budgets is
/// below three quarters full, and refused once either reaches it, with the budget named; the
/// two count apart, so a full share of one leaves the other's count where it was.
///
/// NEGATIVE CONTROL (in the test): one record under each boundary is let in, so the refusals
/// are the boundary's, not the log's.
#[test]
fn proof_waits_below_three_quarters_of_each_budget() {
    let small = RecordBudgets {
        weighed: 8,
        keyed: 8,
        held: 64,
    };
    small.within(|| {
        // The weighed budget: alab()'s genesis and Admit, then Admits to one short of 6 of 8.
        let mut log = alab();
        assert_eq!(
            log.counts(),
            RecordCounts {
                weighed: 2,
                keyed: 1
            }
        );
        for tag in 100..103 {
            admit_by_phrase(&mut log, Mac(tag), tag);
        }
        let capacity = ProofAdmissionCapacity::observe(&log);
        assert_eq!((capacity.weighed_records, capacity.weighed_limit), (5, 6));
        assert_eq!(decided_on_lan(&log, Mac(90), 90, T0), Decision::Lan);
        admit_by_phrase(&mut log, Mac(103), 103);
        assert_eq!(
            ProofAdmissionCapacity::observe(&log).reserved(),
            Some(Budget::Weighed)
        );
        let Decision::Refuse(why) = decided_on_lan(&log, Mac(90), 90, T0) else {
            panic!("a log whose weighed budget is three quarters full let a Mac in");
        };
        assert!(
            why.contains("weighed records") && why.contains("three quarters"),
            "{why}"
        );
        assert_eq!(log.counts().keyed, 1, "the keyed count is apart");

        // The keyed budget: keyed records to one short of 6 of 8.
        let mut log = alab();
        fill_names(&mut log, 3 + 4);
        let capacity = ProofAdmissionCapacity::observe(&log);
        assert_eq!((capacity.keyed_records, capacity.keyed_limit), (5, 6));
        assert_eq!(decided_on_lan(&log, Mac(90), 90, T0), Decision::Lan);
        fill_names(&mut log, 3 + 5);
        let Decision::Refuse(why) = decided_on_lan(&log, Mac(90), 90, T0) else {
            panic!("a log whose keyed budget is three quarters full let a Mac in");
        };
        assert!(
            why.contains("keyed records") && why.contains("three quarters"),
            "{why}"
        );
        assert_eq!(log.counts().weighed, 2, "the weighed count is apart");
    });
}

/// An admission whose whole batch (its `Admit` and the `Wrap`s) would pass the held total is
/// refused by `decide` before anything is signed, with a reason that says the log is full: a Mac
/// on the LAN, and a founder the genesis lists with no binding whose Mac proved its node key.
///
/// NEGATIVE CONTROLS: in the test, the same log takes each batch one record smaller, so the
/// refusal is the whole batch's: a check of the `Admit` alone would let signing start and stop at
/// a Wrap. The founder's arm of `decide` returning its decision before the batch is weighed (as
/// it did): the founder's batch of 3 is let in and its refusal fails.
#[test]
fn an_admission_whose_batch_would_pass_the_held_total_is_refused_before_signing() {
    let small = RecordBudgets {
        held: 8,
        ..RecordBudgets::PRODUCTION
    };
    small.within(|| {
        let mut log = alab();
        fill_names(&mut log, 6);
        assert!(ProofAdmissionCapacity::observe(&log).within_limit);
        let founder = Decision::Founder {
            label: "m5".to_owned(),
        };
        for (fits, too_many, admitted) in [
            (
                on_lan_with_wraps(&log, Mac(90), 90, 1, T0),
                on_lan_with_wraps(&log, Mac(90), 90, 2, T0),
                Decision::Lan,
            ),
            (
                decided_with_wraps(&log, Mac(15), 5, false, 1, T0),
                decided_with_wraps(&log, Mac(15), 5, false, 2, T0),
                founder,
            ),
        ] {
            assert_eq!(fits, admitted);
            let Decision::Refuse(why) = too_many else {
                panic!("an admission of 3 records into 2 records of room was let in: {admitted:?}");
            };
            assert!(
                why.contains("is full") && why.contains("3 records"),
                "{admitted:?}: {why}"
            );
        }
    });
}

/// A Mac on the LAN, in a log whose record bytes leave room for its batch at its longest (the
/// `Admit` and three `Wrap`s, [`Kind::most_bytes`] each) and no more, is let in; one more `Wrap`
/// is refused before anything is signed, with a reason that names the record bytes and does not
/// call the log full.
///
/// NEGATIVE CONTROL (in the test): the estimate the batch had before, [`MAX_RECORD`] a record,
/// passes the byte bound on the very log that lets the Mac in.
#[test]
fn an_admission_whose_batch_could_pass_the_record_bytes_is_refused_before_signing() {
    use crate::membership_log::{MAX_RECORD, MAX_RECORDS_STREAM, Payload, entries};

    let batch = |wraps: usize| Kind::Admit.most_bytes() + wraps * Kind::Wrap.most_bytes();
    let to = MAX_RECORDS_STREAM - batch(3);
    let mut log = alab();
    let (draft, _) = log
        .draft_epoch(&ME.id(), T0, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = draft.sign(&ME.seed(), ME.incarnation()).unwrap();
    assert_eq!(log.insert(epoch.clone()).unwrap(), Inserted::Held);
    // `ME`'s `Put`s, each after the last, with ciphertexts sized to bring the log to `to` bytes.
    let put = |log: &Log, last: &Record, ciphertext: usize| {
        Draft {
            network: *log.network(),
            sequence: last.sequence() + 1,
            previous: Some(*last.id()),
            parents: vec![*last.id()],
            authored_ms: T0,
            body: Body::Put {
                audience: EpochAudience::Everyone,
                epoch: *epoch.id(),
                salt: [7; 32],
                ciphertext: Payload::from(vec![1; ciphertext]),
            },
        }
        .sign(&ME.seed(), ME.incarnation())
        .unwrap()
    };
    let overhead = put(&log, &epoch, 17).bytes().len() - 17;
    let mut last = epoch.clone();
    while log.held_bytes() < to {
        let room = to - log.held_bytes() - overhead;
        let ciphertext = if room <= entries::MAX_PUT_CIPHERTEXT {
            room
        } else {
            (room - overhead - 17).min(entries::MAX_PUT_CIPHERTEXT)
        };
        let record = put(&log, &last, ciphertext);
        assert_eq!(log.insert(record.clone()).unwrap(), Inserted::Held);
        last = record;
    }
    assert_eq!(log.held_bytes(), to);
    assert!(log.held_count() < 100, "{}", log.held_count());

    assert_eq!(on_lan_with_wraps(&log, Mac(90), 90, 3, T0), Decision::Lan);
    let Decision::Refuse(why) = on_lan_with_wraps(&log, Mac(90), 90, 4, T0) else {
        panic!("an admission past the record bytes at its longest was let in");
    };
    assert!(
        why.contains("record bytes") && why.contains("5 records") && !why.contains("full"),
        "{why}"
    );

    let before = RecordCounts {
        weighed: 1,
        keyed: 3,
    };
    assert_eq!(
        log.passed_by(before, before.held() * MAX_RECORD),
        Some(Budget::Bytes),
        "the old estimate refuses the batch that fits"
    );
}

/// End to end on real vault folders: m22, whose log has room for one record, refuses a Mac on the
/// home LAN whose admission signs an `Admit` and its `Wrap`s, and its records file is unchanged;
/// under the production budgets the same proof lets the Mac in with more than one record.
///
/// NEGATIVE CONTROL (in the test): the run with room shows the batch is more than its `Admit`,
/// which alone would have fit the one record of room, so only refusing the whole batch up front
/// keeps signing from starting.
#[test]
fn a_proof_whose_batch_cannot_fit_is_refused_with_nothing_signed() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m22 = &macs.m22;
    let (_, request, _) = newcomer(macs.root.path(), m22, "stranger");
    let proven = Proven {
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
        request: &request,
        node: node(9),
        name: "stranger",
        fleet: false,
        lan: true,
        fresh: true,
        book: CURRENT,
    };
    let now = aterm_time::unix_millis().unwrap();
    let held = load_records(m22).unwrap().len();
    let before = std::fs::read(m22.records_file()).unwrap();
    let one_more = RecordBudgets {
        held: held + 1,
        ..RecordBudgets::PRODUCTION
    };
    let refused = one_more
        .within(|| {
            let limited = Proven {
                reader: crate::members::bundle::Reader::BoundedLog(
                    crate::crossing::RecordsBound::here(false),
                ),
                ..proven
            };
            admit_by_proof(m22, &limited, now)
        })
        .expect("decided");
    let Readmit::Refused(_) = &refused else {
        panic!("a batch past the held total was signed: {refused:?}");
    };
    assert_eq!(std::fs::read(m22.records_file()).unwrap(), before);

    let let_in = admit_by_proof(m22, &proven, now).expect("let in");
    let Readmit::Let { how, signed, .. } = let_in else {
        panic!("the stranger is not let in with room: {let_in:?}");
    };
    assert!(matches!(how, How::Lan { .. }), "{how:?}");
    assert!(signed >= 2, "an Admit and its Wraps: {signed}");
    macs.server.stop();
    drop(macs.root);
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
/// nobody: under the label m7, with the node key, reading `[everyone]` at once, while the old
/// binding is in no epoch newer than the `Remove` (the batch rotates `[everyone]` and Commons
/// without it, [`sign_seat_move`]). Every record m22 signs and every record the Mac takes is one
/// a 0.147 Mac reads (`Remove`, an `Admit` of `Manual`, `Wrap`s, `Epoch`s and `Put`s).
///
/// NEGATIVE CONTROLS: `admit_by_proof` signing the `Admit` without the `Remove` of the old seat
/// (`let mut signed = Vec::new();` in place of the insert and `vec![remove]`): the evaluator
/// labels the new key `m7-<hex>` beside the old seat, and the `Let` match fails with
/// `Refused("the membership log would seat this Mac as m7-…")`. `Evidence::Manual` coded 4 on the
/// wire (both `code` and `from_code`): this build reads it, the 0.147 reader refuses the stream
/// with "membership admission of an unknown evidence 4".
#[test]
fn a_mac_whose_vault_key_changed_takes_its_seat_back_by_proof_with_nobody() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_, old, _) = member_key(m7);
    let old_recipient = read_identity(m7).unwrap().unwrap().recipient();
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
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
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
        tail[2..].contains(&(5, None)),
        "then its Wraps and the rotation that leaves the old key out: {tail:?}"
    );
    let (_, wrapping) = epochs_after_remove_wrapping(m22, &old, &old_recipient);
    assert!(
        wrapping.is_empty(),
        "the old binding reads an epoch newer than its Remove: {wrapping:?}"
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
    // The server answers until it stops below.
    let answering = ServersAnswering::set();
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
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
        request: &request,
        node: n7,
        name: "m7",
        fleet: false,
        lan: false,
        fresh: false,
        book: CURRENT,
    };
    let later = aterm_time::unix_millis().unwrap() + DARK_FOR_MS + 1;
    drop(answering);
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
    // The server answers the setup's reads; the desk's below keep their own bounds.
    let answering = ServersAnswering::set();
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
    drop(answering);
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
                records_bound: crate::crossing::RecordsBound::uncrossed(),
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
                                authority: None,
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
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
        request: &request,
        node: node_key,
        name: "new-mac",
        fleet: false,
        lan: false,
        lab: false,
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
        records_bound: crate::crossing::RecordsBound::uncrossed(),
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
                        authority: None,
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
/// Calling `sync` or `ask_allowed` before returning the admission contacts the
/// separate HTTP source, failing the zero-request assertion. The explicit
/// deferred post then reaches it, so an inert request counter cannot pass.
#[test]
fn a_fresh_vault_key_proves_its_node_key_to_a_members_desk_and_is_in() {
    use std::io::{BufRead as _, Write as _};
    use std::os::fd::AsFd as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::members::desk::{self, Desk, Local};
    use crate::members::join::{self, Stream};
    use crate::members::joiner::Joining;
    // Every server this thread reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
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
        records_bound: crate::crossing::RecordsBound::uncrossed(),
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
    // The member already hands every record and key needed for admission. Its
    // unrelated HTTP source may be unavailable: returning the admission must
    // neither fetch it nor post the legacy approval queue synchronously.
    let source = aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0"))
        .expect("separate vault source");
    let source_url = format!("http://{}/v1/alab", source.local_addr().unwrap());
    let requests = AtomicUsize::new(0);
    let decide = |proving: &desk::Proving<'_>| {
        let bytes = prove_at_ms(m22, proving, CURRENT, later)?;
        let mut bundle = crate::members::Bundle::decode(bytes.as_slice())?;
        bundle.servers = vec![source_url.clone()];
        bundle.encode()
    };
    let served = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        let (cancelled, cancel) = std::io::pipe().unwrap();
        let requests = &requests;
        let source = &source;
        scope.spawn(move || {
            loop {
                let ready = aterm_uds::ready::either_until(
                    source.as_fd(),
                    cancelled.as_fd(),
                    std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                )
                .unwrap()
                .expect("the proof completes or reaches its source");
                if ready.second {
                    break;
                }
                let (mut stream, _) = aterm_uds::accept(
                    source,
                    aterm_uds::AcceptWait::Ready,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap();
                stream
                    .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                    .unwrap();
                let mut line = String::new();
                std::io::BufReader::new(&mut stream)
                    .read_line(&mut line)
                    .unwrap();
                requests.fetch_add(1, Ordering::SeqCst);
                stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
            }
        });
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
                        authority: None,
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
            records_bound: crate::crossing::RecordsBound::uncrossed(),
            device: &other_device,
            node: n7,
            name: "m7".to_owned(),
            model: "Mac15,3".to_owned(),
        };
        let zero = |_: &[u8]| [0u8; 64];
        let refused = join::prove_first(&other, &dial, &copier, &zero, &["m22:7".to_owned()]);
        assert!(refused.is_err(), "a copied public node key proves nothing");
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "a locally verified admission waits for no HTTP vault or approval queue; the subscriber follows it"
        );
        assert_eq!(load_state(&fresh).unwrap().requested, 0);
        assert!(crate::client::holds_allowed(&fresh));
        assert!(crate::client::ask_allowed(&fresh).is_err());
        drop(cancel);
    });
    assert!(
        requests.load(Ordering::SeqCst) > 0,
        "the saved Allowed still supplies the subscriber's deferred post"
    );
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

/// m22 narrows Commons control to itself and m7 syncs: m7 stays in the vault and in its log seat,
/// while the current signed Commons epochs leave it out of control (`control-excluded`), as they
/// left 55 of ALab's 58 members on 2026-11-04. m7 opened Commons while it held control (the
/// fixture enrolled it), so it pinned the network's anchor then.
fn exclude_m7_from_commons(macs: &TwoMacs) {
    use crate::commons::{Observation, Unavailable};
    let (m22, m7) = (&macs.m22, &macs.m7);
    narrow_commons_to_m22(m22);
    sync(m7, None).expect("m7 syncs");
    assert!(matches!(
        crate::commons::observe(m7).unwrap(),
        Observation::Unavailable(Unavailable::ControlExcluded)
    ));
    assert!(matches!(
        status(m7).unwrap().membership,
        Membership::In { .. }
    ));
    assert!(admits_by_log(m7), "the log still seats m7");
}

/// The admin `m22` addresses the Commons control entry to itself alone and publishes.
fn narrow_commons_to_m22(m22: &Paths) {
    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    let control = doc
        .entries()
        .into_iter()
        .find(|entry| entry.name == crate::commons::ENTRY)
        .expect("the Commons control entry");
    doc.unset_entry(&control.audience, crate::commons::ENTRY)
        .unwrap();
    doc.set_entry(
        &Audience::Hosts(vec!["m22".to_owned()]),
        crate::commons::ENTRY,
        control.value.as_slice(),
    )
    .unwrap();
    // As a 0.154.0 Mac minted it: the current carry keeps every seat (one-mesh M1).
    crate::client::log_entries::as_0154(|| admin_publish(m22, &admin, &doc))
        .expect("publish the narrowed Commons control");
}

/// Every byte of the Commons custody at `paths` (its claim, device, pin and control journal), so a
/// test can show that nothing granted, wrote or claimed Commons control there.
fn commons_custody(paths: &Paths) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&paths.dir.join("commons"), &mut out);
    let claim = paths.dir.join("commons.claim");
    out.insert(claim.clone(), std::fs::read(&claim).unwrap());
    out
}

/// A member the current Commons epochs leave out of control is a member still: its log seats it
/// (owner, 2026-11-02: membership is the log, Commons control governs application data), so it
/// has a desk, under the anchor it pinned, and allows by its seat. A Mac on its home ALab LAN that
/// proves itself to that desk is let in through the log, with nobody: the member's `Admit` and
/// `[everyone]` `Wrap`s, the log bundle the new Mac writes, and the records the member posts, which
/// the other member reads it seated from. None of it writes, grants or claims Commons control: the
/// member's Commons custody stays byte for byte, it stays `control-excluded`, and the new Mac holds
/// no Commons control either, which the epochs' holders give it.
///
/// NEGATIVE CONTROL (in-test oracle): the old `Local::of` was `None` wherever
/// `Bootstrap::cached_until` was, which it is for this member (asserted below), so the old rule
/// gave it no desk and "the excluded member has a desk" fails. By hand: the seated arm of
/// `Local::of` answering `Ok(None)` fails there.
#[test]
fn a_member_commons_control_leaves_out_lets_a_home_lan_mac_in_by_its_proof() {
    use crate::commons::bootstrap::{Bootstrap, Device};
    use crate::members::desk::{self, Desk, Local};
    use crate::members::join::{self, Stream};
    use crate::members::joiner::Joining;
    // Every server this thread reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let trust = Bootstrap::required(m22).unwrap().trust();
    exclude_m7_from_commons(&macs);
    let custody = commons_custody(m7);

    // The old rule's only gate: no Commons bootstrap, so no desk.
    assert!(
        Bootstrap::cached(m7).unwrap().is_none(),
        "the oracle: the old desk had nothing to stand on here"
    );
    let local = Local::of(m7)
        .expect("m7's state reads")
        .expect("the excluded member has a desk");
    assert!(local.can_allow(), "its log seat lets it allow");
    assert_eq!(
        local.trust(),
        trust,
        "the anchor m7 pinned, the log's network"
    );
    let log7 = load_records(m7).unwrap().log().unwrap().clone();
    let m7_actor = *log7
        .membership()
        .members()
        .iter()
        .find(|(_, member)| member.binding.writer() == local.key())
        .expect("the desk's key is m7's seat")
        .0;
    let desk = std::sync::Mutex::new(Desk::new(local.with_automatic_lan([])));

    let fresh = Paths::for_home(&macs.root.path().join("fresh"));
    machine_identity(&fresh).expect("identity");
    let device = Device::open(&fresh).expect("device").expect("an identity");
    let pair = node_pair(31);
    let joining = Joining {
        records_bound: crate::crossing::RecordsBound::uncrossed(),
        device: &device,
        node: node(31),
        name: "fresh".to_owned(),
        model: "Mac16,10".to_owned(),
    };
    let prefix = format!("aterm-fleet/v3 record {}\0", desk::PROVE_RECORD);
    let sign_node = |message: &[u8]| -> [u8; 64] {
        pair.sign(&[prefix.as_bytes(), message].concat())
            .as_ref()
            .try_into()
            .unwrap()
    };
    let now = aterm_time::unix_millis().unwrap();
    std::thread::scope(|scope| {
        let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
            let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(Duration::from_secs(60)))?;
            }
            let desk = &desk;
            scope.spawn(move || {
                let decide = |proving: &desk::Proving<'_>| {
                    assert!(proving.lan, "a proof from the home ALab LAN");
                    prove_at_ms(m7, proving, CURRENT, now)
                };
                let refuses = |proving: &desk::Proving<'_>| {
                    refuses_before_possession_at_ms(m7, proving, CURRENT, now)
                };
                let _ = desk::serve_with(
                    desk,
                    &mut ours,
                    "192.168.1.19".parse().unwrap(),
                    &|_| Err(crate::Error::new("no phrase runs here")),
                    Some(desk::Proof {
                        decide: &decide,
                        refuses: &refuses,
                        authority: None,
                        fleet: None,
                    }),
                );
            });
            Ok(Box::new(theirs))
        };
        let joined = join::prove_first(&fresh, &dial, &joining, &sign_node, &["m7:7".to_owned()])
            .unwrap_or_else(|missed| panic!("the excluded member let nobody in: {missed:?}"));
        assert!(matches!(joined.admission, join::Admission::Logged));
    });

    assert_eq!(load_state(&fresh).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(fresh.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );
    let fresh_actor = device
        .request(trust.network, trust.authority)
        .unwrap()
        .binding()
        .actor();
    let seated = load_records(&fresh).unwrap().log().unwrap().membership();
    assert_eq!(
        seated.get(&fresh_actor).and_then(|m| m.admitted_by),
        Some(m7_actor),
        "m7's Admit lets the new Mac in"
    );
    assert!(matches!(
        crate::commons::observe(&fresh).unwrap(),
        crate::commons::Observation::Unavailable(_)
    ));

    // The records reach the vault's server, and the other member reads the new Mac seated.
    assert!(post_records_to_servers(m7).expect("m7 posts its records") >= 1);
    sync_records(m22, None).expect("m22 pulls the records");
    assert!(
        load_records(m22)
            .unwrap()
            .log()
            .unwrap()
            .membership()
            .contains(&fresh_actor)
    );

    // Nothing touched Commons control on the excluded member.
    assert!(matches!(
        crate::commons::observe(m7).unwrap(),
        crate::commons::Observation::Unavailable(crate::commons::Unavailable::ControlExcluded)
    ));
    assert!(
        commons_custody(m7) == custody,
        "the excluded member's Commons custody changed"
    );
    macs.server.stop();
    drop(macs.root);
}

/// One connection to `desk`, served as the window's desk serves it (`desk::serve_with`, whose run
/// hands the bundle [`hand_over`] builds on `member`) while `joiner` speaks, each end with a
/// one-minute hang detector.
fn served_by<T>(
    desk: &std::sync::Mutex<crate::members::desk::Desk>,
    member: &Paths,
    joiner: impl FnOnce(&mut std::os::unix::net::UnixStream) -> T,
) -> (crate::Result<Option<crate::members::desk::Let>>, T) {
    let (mut ours, mut theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    for end in [&ours, &theirs] {
        end.set_read_timeout(Some(Duration::from_secs(60)))
            .expect("timeout");
    }
    std::thread::scope(|scope| {
        let served = scope.spawn(move || {
            crate::members::desk::serve_with(
                desk,
                &mut ours,
                "192.0.2.9".parse().unwrap(),
                &|handing| hand_over(member, handing),
                None,
            )
        });
        let said = joiner(&mut theirs);
        drop(theirs);
        (served.join().expect("the member's end"), said)
    })
}

/// The excluded member's desk lets in a Mac a person allowed, as the window's desk runs it: the
/// Mac asks, the desk greets it as a member that can allow under the anchor it pinned, a person
/// allows the row, and the Mac runs the phrase (CPace, the possession challenge) over
/// `desk::serve_with`, whose run hands it the membership log ([`hand_over`]): m7's `Admit` and
/// `[everyone]` `Wrap`s, which the Mac writes and is in by. No Commons control is asked of either.
///
/// NEGATIVE CONTROL (in-test oracle): as in
/// [`a_member_commons_control_leaves_out_lets_a_home_lan_mac_in_by_its_proof`], the old rule gave
/// this member no desk, so "the excluded member has a desk" fails under it.
#[test]
fn a_member_commons_control_leaves_out_lets_in_a_mac_a_person_allowed() {
    use crate::commons::bootstrap::{Bootstrap, Device};
    use crate::members::bundle::Handed;
    use crate::members::desk::{Answer, Desk, Local};
    use crate::members::joiner::{self, Joining};
    // Every server this thread reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let trust = Bootstrap::required(m22).unwrap().trust();
    exclude_m7_from_commons(&macs);
    let custody = commons_custody(m7);
    assert!(
        Bootstrap::cached(m7).unwrap().is_none(),
        "the oracle: the old desk had nothing to stand on here"
    );
    let local = Local::of(m7)
        .expect("m7's state reads")
        .expect("the excluded member has a desk");
    assert_eq!(local.trust(), trust, "the anchor m7 pinned");
    let m7_actor = *load_records(m7)
        .unwrap()
        .log()
        .unwrap()
        .membership()
        .members()
        .iter()
        .find(|(_, member)| member.binding.writer() == local.key())
        .expect("the desk's key is m7's seat")
        .0;
    let desk = std::sync::Mutex::new(Desk::new(local));

    let fresh = Paths::for_home(&macs.root.path().join("allowed"));
    machine_identity(&fresh).expect("identity");
    let device = Device::open(&fresh).expect("device").expect("an identity");
    let joining = Joining {
        records_bound: crate::crossing::RecordsBound::uncrossed(),
        device: &device,
        node: node(32),
        name: "allowed".to_owned(),
        model: "Mac16,10".to_owned(),
    };
    let (served, asked) = served_by(&desk, m7, |stream| {
        let greeting = joiner::greeting(stream)?;
        assert!(
            greeting.can_allow,
            "the desk greets as a member that can allow"
        );
        joiner::ask(stream, &greeting, &joining)
    });
    served.expect("the ask is served");
    assert_eq!(asked.expect("asked"), Answer::Asking);
    let words = {
        let mut desk = desk.lock().unwrap();
        let now = aterm_time::Instant::now();
        let row = desk.rows(now)[0].id;
        let phrase = desk.allow(row, now).expect("a person allows the row");
        desk.words(phrase, now).expect("live").join(" ")
    };
    let phrase = crate::phrase::Phrase::parse(&words).expect("three words");
    let (served, ran) = served_by(&desk, m7, |stream| {
        let greeting = joiner::greeting(stream)?;
        joiner::run(stream, &greeting, &joining, &phrase)
    });
    let bundle = ran.expect("the run hands the bundle");
    let let_in = served
        .expect("the run is served")
        .expect("a run that sent its bundle");
    assert!(let_in.logged, "the log, not F10's files");
    assert!(matches!(bundle.handed, Handed::Log { .. }));

    write_from_log_bundle(&fresh, &bundle).expect("the Mac writes the log bundle");
    assert_eq!(load_state(&fresh).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(fresh.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );
    let fresh_actor = device
        .request(trust.network, trust.authority)
        .unwrap()
        .binding()
        .actor();
    assert_eq!(
        load_records(&fresh)
            .unwrap()
            .log()
            .unwrap()
            .membership()
            .get(&fresh_actor)
            .and_then(|m| m.admitted_by),
        Some(m7_actor),
        "m7's Admit lets the new Mac in"
    );
    assert!(matches!(
        crate::commons::observe(m7).unwrap(),
        crate::commons::Observation::Unavailable(crate::commons::Unavailable::ControlExcluded)
    ));
    assert!(
        commons_custody(m7) == custody,
        "the excluded member's Commons custody changed"
    );
    macs.server.stop();
    drop(macs.root);
}

/// A vault pass that holds an excluded member's state while its desk reads its seat delays the
/// read, as the Commons observation before it waits for a busy vault: the seat is read once the
/// pass ends, and a pass that still holds the state at the read's deadline is a refusal that
/// says so, no sooner than that deadline. The pass ends only once the second read is waiting for
/// it (its first blocked try, through `OBSERVE_BLOCKED`), so that read starts while the pass still
/// holds the state.
///
/// NEGATIVE CONTROLS: `log_seat` reading with `open_local_member_signer_until` (the read that
/// does not wait, as before): the held read fails at once with "busy or unavailable", and "a pass
/// still holding the state at the deadline" fails. `log_entries::observe_signer_until`
/// observing the state once (`StateLock::observe(paths)?`, the same error text): "no sooner than
/// its deadline" fails, and with that assert cut, "the read waits for the pass that holds the
/// state" fails.
#[test]
fn an_excluded_members_seat_read_waits_for_a_vault_pass() {
    use crate::commons::bootstrap::Bootstrap;
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let trust = Bootstrap::required(m22).unwrap().trust();
    exclude_m7_from_commons(&macs);
    let held = StateLock::take(m7).expect("a pass holds m7's state");
    let deadline = std::time::Instant::now() + Duration::from_millis(200);
    let error = seated_member(m7, Some(deadline))
        .err()
        .expect("a pass still holding the state at the deadline");
    assert!(
        std::time::Instant::now() >= deadline,
        "the held read returns no sooner than its deadline: {error}"
    );
    assert!(error.to_string().contains("at the deadline"), "{error}");
    let (waiting, waits) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let read = scope.spawn(move || {
            let mut waiting = Some(waiting);
            super::super::OBSERVE_BLOCKED.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    if let Some(waiting) = waiting.take() {
                        let _ = waiting.send(());
                    }
                }));
            });
            let read = seated_member(
                m7,
                Some(std::time::Instant::now() + Duration::from_secs(60)),
            );
            super::super::OBSERVE_BLOCKED.with(|hook| hook.borrow_mut().take());
            read
        });
        waits
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the read waits for the pass that holds the state");
        drop(held);
        let (_, pinned) = read
            .join()
            .expect("the reader")
            .expect("the seat reads once the pass ends")
            .expect("m7 is seated");
        assert_eq!(pinned, trust);
    });
    macs.server.stop();
    drop(macs.root);
}

/// A seated member whose own vault pass holds its state when its desk asks whether it can allow
/// ([`admits_by_log`], which `members::desk::Local::of` asks right after a Commons read that waits
/// for such a pass) is told it can once the pass ends, though the pass brought a record in: the
/// pass pulls a record m22 posted and ends while the read waits on it
/// ([`crate::client::OBSERVE_BLOCKED`]), and the seat is read from the log the pass left.
///
/// NEGATIVE CONTROLS: `log_seat` reading with `open_local_member_signer_until` (a read that does
/// not wait): it finds the state held, never reaches the hook, and "the read waits for the pass"
/// fails. `log_seat` loading its log before the wait (as 04dc05e579 did): that log is behind the
/// vault snapshot the pass wrote, and "m7's log seats it once the pass ends" fails.
#[test]
fn a_seated_members_allow_read_waits_for_a_vault_pass() {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    assert!(admits_by_log(m7), "m7's log seats it");
    let pair = node_pair(22);
    publish_local_node_key(m22, node(22), |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .expect("m22 publishes its node key")
    .expect("a Node record");
    post_records_to_servers(m22).expect("m22 posts its records");
    let held = StateLock::take(m7).expect("a pass holds m7's state");
    let (blocks, blocked) = std::sync::mpsc::channel::<()>();
    let (ends, ended) = std::sync::mpsc::channel::<()>();
    let (seated, waits, pulled) = std::thread::scope(|scope| {
        let reader = scope.spawn(move || {
            let waits = std::rc::Rc::new(std::cell::Cell::new(0));
            let count = std::rc::Rc::clone(&waits);
            crate::client::OBSERVE_BLOCKED.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    count.set(count.get() + 1);
                    let _ = blocks.send(());
                    ended
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .expect("the pass ends");
                }));
            });
            // The hang detector, not the desk's own wait: a real pass runs while the read waits.
            let seated = super::super::admit::admits_by_log_until(
                m7,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            );
            crate::client::OBSERVE_BLOCKED.with(|hook| hook.borrow_mut().take());
            (seated, waits.get())
        });
        blocked
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the read waits for the pass that holds the state");
        // The pass: it lets the state go and m7's own pass pulls m22's record in, while the read
        // still waits in its hook.
        drop(held);
        let pulled = sync_records(m7, None).expect("m7's pass pulls m22's record");
        ends.send(()).expect("the reader waits");
        let (seated, waits) = reader.join().expect("the reader");
        (seated, waits, pulled)
    });
    assert!(pulled >= 1, "the pass brought m22's record in");
    assert_eq!(waits, 1, "the read found m7's state held once");
    assert!(seated, "m7's log seats it once the pass ends");
    macs.server.stop();
    drop(macs.root);
}

/// A member's write while its own vault pass holds its state waits for the pass, though the pass
/// brings a record in: m7's state is held, the write waits on it
/// ([`crate::client::OBSERVE_BLOCKED`]), the pass pulls a record m22 posted and ends, and the
/// write puts its value under the newest `[everyone]` epoch, read from the log the pass left.
///
/// NEGATIVE CONTROL: `test_put_everyone` drafting under `author_and_commit_records` (one
/// observation that does not wait, as before): it finds the state held, never reaches the hook,
/// and "the write waits for the pass" fails.
#[test]
fn a_members_write_waits_for_its_own_vault_pass() {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let pair = node_pair(22);
    publish_local_node_key(m22, node(22), |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .expect("m22 publishes its node key")
    .expect("a Node record");
    post_records_to_servers(m22).expect("m22 posts its records");
    let held = StateLock::take(m7).expect("a pass holds m7's state");
    let (blocks, blocked) = std::sync::mpsc::channel::<()>();
    let (ends, ended) = std::sync::mpsc::channel::<()>();
    let (put, pulled) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            let mut blocks = Some(blocks);
            crate::client::OBSERVE_BLOCKED.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    if let Some(blocks) = blocks.take() {
                        let _ = blocks.send(());
                        ended
                            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                            .expect("the pass ends");
                    }
                }));
            });
            let put = test_put_everyone(m7, "written-while-a-pass-ran", b"v");
            crate::client::OBSERVE_BLOCKED.with(|hook| hook.borrow_mut().take());
            put
        });
        blocked
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the write waits for the pass that holds the state");
        drop(held);
        let pulled = sync_records(m7, None).expect("m7's pass pulls m22's record");
        ends.send(()).expect("the writer waits");
        (writer.join().expect("the writer"), pulled)
    });
    assert!(pulled >= 1, "the pass brought m22's record in");
    put.expect("the write lands once the pass ends");
    macs.server.stop();
    drop(macs.root);
}

/// A member's removal while its own vault pass holds its state waits for the pass, though the
/// pass brings a record in. The churn lab found this: a `members remove` that met its Mac's
/// subscriber applying refused at once, "the vault state is busy or unavailable while reading
/// its signer". m22's state is held, and its removal of m7, an admin of the envelope 0.145 Macs
/// read, waits on it ([`crate::client::OBSERVE_BLOCKED`]). The pass pulls a record m7 posted
/// and ends. The removal then signs from the log the pass left: m7 is out of m22's log, and the
/// removal still says m7 keeps the envelope's signing key.
///
/// NEGATIVE CONTROLS:
/// - `author_and_commit_records` drafting with no observation, as before (`draft_observation`
///   returning `Ok(None)`). The draft's signer read finds the state held, the removal never
///   reaches the hook, and "the removal waits for the pass" fails.
/// - `vault_admin_named` read before the draft, as before: its read finds the state held and
///   names no admin, and "the removal says m7 keeps the signing key" fails.
#[test]
fn a_members_removal_waits_for_its_own_vault_pass() {
    let _answering = ServersAnswering::set();
    let macs = super::super::log_entries_tests::two_macs_with(
        &[(Audience::Everyone, "shared", b"everyone-v1".as_slice())],
        true,
    );
    let (m22, m7) = (&macs.m22, &macs.m7);
    let pair = node_pair(7);
    publish_local_node_key(m7, node(7), |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .expect("m7 publishes its node key")
    .expect("a Node record");
    post_records_to_servers(m7).expect("m7 posts its records");
    let seat = |paths: &Paths| {
        let store = load_records(paths).unwrap();
        crate::client::log_entries::open_local_member_signer(paths, store.log().unwrap())
            .unwrap()
            .expect("a seat")
            .0
    };
    let m7_seat = seat(m7);
    let held = StateLock::take(m22).expect("a pass holds m22's state");
    let (blocks, blocked) = std::sync::mpsc::channel::<()>();
    let (ends, ended) = std::sync::mpsc::channel::<()>();
    let (removal, pulled) = std::thread::scope(|scope| {
        let remover = scope.spawn(move || {
            let mut blocks = Some(blocks);
            crate::client::OBSERVE_BLOCKED.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    if let Some(blocks) = blocks.take() {
                        let _ = blocks.send(());
                        ended
                            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                            .expect("the pass ends");
                    }
                }));
            });
            let removal = remove_member(m22, "m7", false);
            crate::client::OBSERVE_BLOCKED.with(|hook| hook.borrow_mut().take());
            removal
        });
        blocked
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the removal waits for the pass that holds the state");
        drop(held);
        let pulled = sync_records(m22, None).expect("m22's pass pulls m7's record");
        ends.send(()).expect("the remover waits");
        (remover.join().expect("the remover"), pulled)
    });
    assert!(pulled >= 1, "the pass brought m7's record in");
    let removal = removal.expect("the removal lands once the pass ends");
    assert!(removal.signed > 0, "the removal signed its batch");
    assert!(
        removal
            .envelope_notes
            .iter()
            .any(|note| note.contains("m7 was an admin") && note.contains("signing key")),
        "the removal says m7 keeps the signing key: {:?}",
        removal.envelope_notes
    );
    let after = load_records(m22).unwrap();
    assert!(
        !after.log().unwrap().membership().contains(&m7_seat),
        "m7 is out of m22's log"
    );
    macs.server.stop();
    drop(macs.root);
}

/// Copy the file or directory `from` to `to`, keeping each file's mode and making each directory
/// this user's alone, as Commons custody is held.
fn copy_private(from: &Path, to: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let kind = std::fs::symlink_metadata(from).unwrap().file_type();
    if kind.is_dir() {
        std::fs::create_dir(to).unwrap();
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o700)).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            copy_private(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        assert!(kind.is_file(), "{} is no file", from.display());
        std::fs::copy(from, to).unwrap();
    }
}

/// The parts of the pinned Commons anchor at `paths`: the pin, its claim, and the control journal
/// that carries the anchor too.
fn anchor_parts(paths: &Paths) -> [std::path::PathBuf; 3] {
    [
        paths.dir.join("commons/default.pin"),
        paths.dir.join("commons.claim"),
        paths.dir.join("commons/control"),
    ]
}

/// NEGATIVE CONTROLS for the log-seat rule: no desk for a Mac with no vault; for a Mac its vault
/// lets in, holding Commons custody and a pinned anchor, that its log seats nowhere (its Commons
/// control narrowed away, so only the seat is missing); for a Commons-excluded member that lost
/// its pinned anchor; an error for one whose pin its claim does not name, and for one whose pin,
/// claim and control journal agree on another network than its membership log's (no other anchor
/// stands in for the log's); and no desk for a member its log removed.
///
/// Each was also run as a mutant. `log_seat`, for the desk's read (with a deadline), taking the
/// device it opens whether or not the log seats it (`Some(_) => return
/// Ok(Device::open_existing(paths).ok().map(|device| (device, *root)))`): `m9` gets a desk and "no
/// seat" fails. `seated_member`'s root check cut (`if false && root != …`): the agreeing foreign
/// anchor gets a desk and "an anchor naming another network than the log" fails.
#[test]
fn no_vault_no_seat_no_pinned_anchor_or_a_removal_gives_no_desk() {
    use crate::commons::bootstrap::{Bootstrap, Device, established_trust};
    use crate::commons::{Observation, Unavailable};
    use crate::members::desk::Local;
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    exclude_m7_from_commons(&macs);

    let bare = Paths::for_home(&macs.root.path().join("bare"));
    machine_identity(&bare).expect("identity");
    Device::open(&bare).expect("device").expect("an identity");
    assert!(Local::of(&bare).expect("readable").is_none(), "no vault");

    // m9 is approved after Genesis v2: no record admits it, so the log seats it nowhere.
    let m9 = Paths::for_home(&macs.root.path().join("m9"));
    let m9_rec = machine_identity(&m9).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m9", &m9_rec).unwrap();
        admin_publish_approved(m22, &admin, &doc, std::slice::from_ref(&m9_rec)).unwrap();
    }
    join_host(&m9, &format!("{}#{}", macs.url, macs.signer), "m9");
    // Its approval addressed Commons control to it: it opens Commons while it holds control, so
    // it holds a device and pins the network's anchor, as m7 did. The admin then narrows control
    // to itself again.
    Bootstrap::required(&m9).expect("m9 opens Commons while control reaches it");
    narrow_commons_to_m22(m22);
    sync(&m9, None).expect("m9 syncs");
    assert!(matches!(
        status(&m9).unwrap().membership,
        Membership::In { .. }
    ));
    let observed = crate::commons::observe(&m9).unwrap();
    assert!(
        matches!(
            observed,
            Observation::Unavailable(Unavailable::ControlExcluded | Unavailable::ControlNotVisible)
        ),
        "{:?}",
        match &observed {
            Observation::Unavailable(reason) => reason.code(),
            Observation::Visible(_) => "visible",
        }
    );
    assert!(m9.dir.join("commons/device.age").is_file(), "m9's device");
    assert!(m9.dir.join("commons/default.pin").is_file(), "m9's pin");
    assert_eq!(
        established_trust(&m9).expect("m9's anchor reads"),
        Some(Bootstrap::required(m22).unwrap().trust()),
        "m9 pinned the network's anchor"
    );
    assert!(!admits_by_log(&m9));
    assert!(Local::of(&m9).expect("readable").is_none(), "no seat");
    assert!(!member_by_log(&m9).expect("readable"));

    let pin = m7.dir.join("commons/default.pin");
    let pinned = std::fs::read(&pin).unwrap();
    std::fs::rename(&pin, pin.with_extension("gone")).unwrap();
    assert!(
        Local::of(m7).expect("readable").is_none(),
        "no pinned anchor"
    );
    assert!(!member_by_log(m7).expect("readable"));
    std::fs::rename(pin.with_extension("gone"), &pin).unwrap();
    let mut other = pinned.clone();
    other[4] ^= 1;
    fsx::write_atomic(&pin, &other, 0o600).unwrap();
    let error = Local::of(m7).err().expect("a pin its claim does not name");
    assert!(
        error
            .to_string()
            .contains("differs from its retained history claim"),
        "{error}"
    );
    fsx::write_atomic(&pin, &pinned, 0o600).unwrap();

    // Another network's anchor, whole: the pin, claim and control journal of a member of another
    // vault's network, which agree with each other, beside m7's own device, which m7's log seats.
    let elsewhere = macs.root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let elsewhere = test_commons_excluded(&elsewhere).expect("a member of another network");
    let foreign = established_trust(&elsewhere.member)
        .expect("its anchor reads")
        .expect("it pinned one");
    assert_ne!(
        foreign.network,
        Bootstrap::required(m22).unwrap().trust().network
    );
    let kept = |path: &Path| path.with_extension("kept");
    for (own, theirs) in anchor_parts(m7)
        .iter()
        .zip(anchor_parts(&elsewhere.member).iter())
    {
        std::fs::rename(own, kept(own)).unwrap();
        copy_private(theirs, own);
    }
    assert_eq!(
        established_trust(m7).expect("the foreign anchor agrees with itself"),
        Some(foreign)
    );
    let error = Local::of(m7)
        .err()
        .expect("an anchor naming another network than the log");
    assert!(
        error
            .to_string()
            .contains("names another network than its membership log"),
        "{error}"
    );
    for own in anchor_parts(m7) {
        if own.is_dir() {
            std::fs::remove_dir_all(&own).unwrap();
        } else {
            std::fs::remove_file(&own).unwrap();
        }
        std::fs::rename(kept(&own), &own).unwrap();
    }
    elsewhere.server.stop();
    assert!(Local::of(m7).unwrap().is_some(), "the pinned anchor again");
    assert!(member_by_log(m7).expect("readable"));

    remove_member(m22, "m7", false).expect("m22 removes m7");
    sync(m7, None).expect("m7 syncs its removal");
    assert!(
        Local::of(m7).expect("readable").is_none(),
        "removed by its log"
    );
    assert!(!member_by_log(m7).expect("readable"));
    macs.server.stop();
    drop(macs.root);
}

/// The seam other crates' tests build a Commons-excluded member with ([`test_commons_excluded`])
/// builds one this crate's desk reads as a member that can allow.
#[test]
fn the_commons_excluded_seam_builds_a_seated_member_with_a_desk() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let fixture = test_commons_excluded(root.path()).expect("the seam's member");
    let local = crate::members::desk::Local::of(&fixture.member)
        .expect("its state reads")
        .expect("the excluded member has a desk");
    assert!(local.can_allow());
    fixture.server.stop();
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
        records_bound: crate::crossing::RecordsBound::uncrossed(),
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
                        authority: None,
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
        records_bound: crate::crossing::RecordsBound::uncrossed(),
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
                        authority: None,
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

/// `SwarmRecordsCapacity` in each configuration it declares: the committed one, then the
/// activation's.
fn records_capacity_configurations() -> Vec<aterm_spec::derive::Model> {
    let name = aterm_models_swarm::records_capacity_model().name;
    aterm_spec::xref::model_configurations()
        .into_iter()
        .find(|configurations| configurations[0].name == name)
        .expect("SwarmRecordsCapacity is registered")
}

/// `model`'s constant `name`, as a count.
fn capacity_constant(model: &aterm_spec::derive::Model, name: &str) -> usize {
    model
        .consts
        .iter()
        .find(|(constant, _)| *constant == name)
        .map(|&(_, value)| usize::try_from(value).expect("a count"))
        .expect("the model's constant")
}

/// The record budgets `model`'s `Budgets` log holds: `W` weighed, `K` keyed, `H` in all.
fn capacity_budgets(model: &aterm_spec::derive::Model) -> RecordBudgets {
    RecordBudgets {
        weighed: capacity_constant(model, "W"),
        keyed: capacity_constant(model, "K"),
        held: capacity_constant(model, "H"),
    }
}

/// A log of `SwarmRecordsCapacity`'s `Budgets` holding `w` weighed and `k` keyed records:
/// [`alab`] (`w` 2, `k` 1), then `ME`'s phrase `Admit`s and `Name`s.
fn capacity_log(w: i64, k: i64) -> Log {
    let mut log = alab();
    for n in 0..w - 2 {
        let tag = 100 + u8::try_from(n).expect("a tag");
        admit_by_phrase(&mut log, Mac(tag), tag);
    }
    fill_names(&mut log, usize::try_from(w + k).expect("a count"));
    log
}

/// `log`'s held records by the budget each counts against ([`Kind::weighed`]): weighed, keyed.
fn by_kind(log: &Log) -> (i64, i64) {
    let weighed = log.records().filter(|r| r.kind().weighed()).count();
    let keyed = log.held_count() - weighed;
    (
        i64::try_from(weighed).expect("a count"),
        i64::try_from(keyed).expect("a count"),
    )
}

/// One of `SwarmRecordsCapacity`'s budgets actions on `log`, and what it did (the model's
/// `last`): `AddWeighed` a person's `Admit` of a new Mac, `AddKeyed` a `Name`, each held or
/// refused by [`Log::insert`] for a budget, and `Prove` a Mac on the home LAN that proves its
/// node key, judged by [`decide`] with an admission of `wraps` `Wrap`s beside its `Admit` (the
/// model's `PW`). Where [`decide`] lets it in, its batch is signed: the `Admit`, then a keyed
/// record for each `Wrap` (a `Name`, which counts as a `Wrap` does), each held where
/// [`Log::insert`] takes it.
fn capacity_act(log: &mut Log, action: &str, wraps: usize) -> i64 {
    let inserted = |log: &mut Log, body: Body, (held, refused): (i64, i64)| {
        let record = log
            .draft(&ME.id(), T0, body)
            .sign(&ME.seed(), ME.incarnation())
            .unwrap();
        match log.insert(record) {
            Ok(Inserted::Held) => held,
            Err(e) if e.kind() == crate::ErrorKind::MembershipCapacity => refused,
            other => panic!("{action}: {other:?}"),
        }
    };
    let name = || Body::Name {
        name: "added".into(),
    };
    match action {
        "AddWeighed" => inserted(
            log,
            Body::Admit {
                entrant: Entrant {
                    request: Mac(150).request(),
                    node_key: node(150),
                    name: "m150".to_owned(),
                    label: "m150".to_owned(),
                },
                evidence: Evidence::Phrase,
            },
            (1, 2),
        ),
        "AddKeyed" => inserted(log, name(), (3, 4)),
        "Prove" => match on_lan_with_wraps(log, Mac(90), 90, wraps, T0) {
            Decision::Lan => {
                admit(log, ME, Mac(90), 90, "m90", Evidence::Manual, T0);
                for _ in 0..wraps {
                    inserted(log, name(), (0, 0));
                }
                5
            }
            Decision::Refuse(_) => 6,
            other => panic!("Prove: {other:?}"),
        },
        other => panic!("not a budgets action: {other}"),
    }
}

/// `SwarmRecordsCapacity`'s variables after a budgets action on a real log: `w` and `k` the
/// log's held records by kind (not its own counts, which a mutant may count wrong), `last` what
/// the action did, and `lw` and `lk` the records by kind it judged, `before` it.
fn records_capacity_transition(
    before: (i64, i64),
    last: i64,
    after: &Log,
) -> aterm_spec::interp::State {
    let (w, k) = by_kind(after);
    aterm_spec::interp::State::from([
        ("sc", 1),
        ("w", w),
        ("k", k),
        ("last", last),
        ("lw", before.0),
        ("lk", before.1),
        ("nr", 0),
        ("nx", 0),
        ("mark", 0),
        ("gs", 0),
        ("fenced", 0),
    ])
}

/// `action` on a log of `model`'s `Budgets` holding `w` weighed and `k` keyed records, made and
/// acted on within the model's budgets, a proof's batch of its `PW` `Wrap`s: the state it leaves.
fn capacity_observed(
    model: &aterm_spec::derive::Model,
    (w, k): (i64, i64),
    action: &str,
) -> aterm_spec::interp::State {
    capacity_budgets(model).within(|| {
        let mut log = capacity_log(w, k);
        assert_eq!(by_kind(&log), (w, k), "the log realizes ({w}, {k})");
        let last = capacity_act(&mut log, action, capacity_constant(model, "PW"));
        records_capacity_transition((w, k), last, &log)
    })
}

/// The state `model` reaches by `path` from its first (each of these actions has one
/// successor).
fn capacity_state(model: &aterm_spec::derive::Model, path: &[&str]) -> aterm_spec::interp::State {
    path.iter().fold(model.init_state(), |state, action| {
        let next = model.successors(action, &state);
        assert_eq!(next.len(), 1, "{action} from {state:?}");
        next[0].clone()
    })
}

/// S8: `SwarmRecordsCapacity` holds its laws, each of its mutants breaks the law it stands for
/// (1 `LegacyNeverPartial`, 2 `CrossedBeforeHeld`, 3 `ProofReserve`, 4 `KeyedBound` and
/// `ProofReserve`, 5 `ProofReserve`, 6 `LegacyNeverPartial`), and a real log refines its
/// `Budgets` in each configuration the model declares (the committed held total, and the
/// activation's, the two budgets together): from every count of weighed and keyed records the
/// model reaches, each of `AddWeighed`, `AddKeyed` and `Prove` on a real log of those counts
/// (made within the model's budgets, `RecordBudgets::within`) gives exactly the successor the
/// model allows there. `Prove` is [`decide`] judging an admission of the model's `PW` `Wrap`s,
/// and its whole batch signed where it lets the Mac in. A successor reads only the counts, so
/// one log per count and action judges every state with those counts.
///
/// NEGATIVE CONTROLS, each the real log down its mutant's path:
/// - `readmit::RESERVE_OVER_ALL` (the reserve over every kind against three quarters of the held
///   total, as before the split): the activation's log of three weighed records and one keyed
///   lets a proof in past the weighed reserve, which is no successor of `Prove` and breaks
///   `ProofReserve`.
/// - `budgets::KEYED_AS_WEIGHED` (keyed kinds counted as weighed): the activation's log of two
///   weighed and two keyed records refuses a `Name`, which is no successor of `AddKeyed` and
///   breaks `KeyedBound`; and its first log refuses the first proof by the weighed reserve, as
///   the model's mutant 4 does, which is no successor and breaks `ProofReserve`.
/// - `readmit::BATCH_WITHOUT_WRAPS` (a proof's fit judged by its `Admit` alone): the committed
///   first log lets a proof in whose `Wrap` the held total then refuses, which is no successor
///   of `Prove` and breaks `ProofReserve`.
#[test]
fn record_budgets_and_the_proof_reserve_conform() {
    use crate::membership_log::budgets::KEYED_AS_WEIGHED;
    use aterm_spec::interp::{fired_actions, violated_invariants, with_buggy};

    let configurations = records_capacity_configurations();
    assert_eq!(configurations.len(), 2, "the committed and the activation");
    let faults = aterm_spec::verify::sweep_faults(&configurations);
    assert!(faults.is_empty(), "{faults:?}");
    let model = &configurations[0];
    aterm_spec::interp::prove_and_catch(model);
    let fired: BTreeSet<&str> = configurations.iter().flat_map(fired_actions).collect();
    assert_eq!(fired.len(), model.actions.len(), "fired actions: {fired:?}");
    for (buggy, law) in [
        (1, "LegacyNeverPartial"),
        (2, "CrossedBeforeHeld"),
        (3, "ProofReserve"),
        (4, "KeyedBound"),
        (4, "ProofReserve"),
        (5, "ProofReserve"),
        (6, "LegacyNeverPartial"),
    ] {
        assert!(
            configurations
                .iter()
                .any(|m| violated_invariants(&with_buggy(m, buggy)).contains(law)),
            "mutant {buggy} breaks {law}"
        );
    }

    let actions = ["AddWeighed", "AddKeyed", "Prove"];
    let mut admitted = 0;
    for model in &configurations {
        let start = model.successors("Budgets", &model.init_state());
        assert_eq!(start.len(), 1);
        let start = start[0].clone();
        let mut reached = BTreeSet::from([start.clone()]);
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(state) = queue.pop_front() {
            for action in &model.actions {
                let enabled = model.action_enabled(action.name, &state);
                assert_eq!(enabled, actions.contains(&action.name), "{}", action.name);
                for next in model.successors(action.name, &state) {
                    if reached.insert(next.clone()) {
                        queue.push_back(next);
                    }
                }
            }
        }
        let counts: BTreeSet<(i64, i64)> = reached.iter().map(|s| (s["w"], s["k"])).collect();
        for &(w, k) in &counts {
            for action in actions {
                let observed = capacity_observed(model, (w, k), action);
                admitted += usize::from(observed["last"] == 5);
                for state in reached.iter().filter(|s| (s["w"], s["k"]) == (w, k)) {
                    assert_eq!(
                        model.successors(action, state),
                        vec![observed.clone()],
                        "{} {action} from {state:?}",
                        capacity_constant(model, "H")
                    );
                }
            }
        }
        eprintln!(
            "SwarmRecordsCapacity (held {}): {} states, {} counts judged",
            capacity_constant(model, "H"),
            reached.len(),
            counts.len()
        );
    }
    assert!(admitted > 0, "a proof's whole batch is signed somewhere");

    let committed = &configurations[0];
    let activation = &configurations[1];
    let control = |switch: &'static std::thread::LocalKey<std::cell::Cell<bool>>,
                   model: &aterm_spec::derive::Model,
                   path: &[&str],
                   action: &str,
                   law: &str| {
        let from = capacity_state(model, path);
        switch.with(|cell| cell.set(true));
        let observed = capacity_observed(model, (from["w"], from["k"]), action);
        switch.with(|cell| cell.set(false));
        assert!(
            !model.successors(action, &from).contains(&observed),
            "{action} from {from:?} gave {observed:?}, a successor"
        );
        assert!(!model.check_invariant(law, &observed), "{observed:?}");
        observed
    };

    // The reserve over every kind, past the weighed reserve.
    let over_all = control(
        &RESERVE_OVER_ALL,
        activation,
        &["Budgets", "AddWeighed"],
        "Prove",
        "ProofReserve",
    );
    assert_eq!(over_all["last"], 5, "the mutant lets the proof in");
    // Keyed kinds counted as weighed: a `Name`, then the first proof.
    let name = control(
        &KEYED_AS_WEIGHED,
        activation,
        &["Budgets", "AddKeyed"],
        "AddKeyed",
        "KeyedBound",
    );
    assert_eq!(name["last"], 4, "the mutant refuses the Name");
    let proof = control(
        &KEYED_AS_WEIGHED,
        activation,
        &["Budgets"],
        "Prove",
        "ProofReserve",
    );
    assert_eq!(proof["last"], 6, "the mutant refuses the first proof");
    assert_eq!(
        with_buggy(activation, 4).successors("Prove", &capacity_state(activation, &["Budgets"])),
        vec![proof],
        "the model's mutant 4 refuses it alike"
    );
    // A proof's fit judged by its `Admit` alone.
    let cut = control(
        &BATCH_WITHOUT_WRAPS,
        committed,
        &["Budgets"],
        "Prove",
        "ProofReserve",
    );
    assert_eq!(
        (cut["last"], cut["w"], cut["k"]),
        (5, 3, 1),
        "the mutant signs the Admit, and the held total refuses its Wrap"
    );
}

/// A12: a lab identity's proof is refused on the live ALab genesis: before the possession
/// challenge where the desk checks first ([`refuses_before_possession_at_ms`]), and at the
/// decision where it does not ([`prove_at_ms`]), with nothing signed either way. The same proof on
/// another genesis, a lab's, lets the Mac in. The pair shows the refusal bites.
#[test]
fn a_lab_identitys_proof_is_refused_on_the_live_genesis_and_let_in_on_another() {
    use crate::client::with_live_genesis;
    use crate::members::desk::{self, Desk, Local};
    use crate::members::join::{self, Stream};
    use crate::members::joiner::Joining;
    let _answering = ServersAnswering::set();
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

    let lab = Paths::for_home(&macs.root.path().join("m7-lab"));
    machine_identity(&lab).expect("identity");
    let device = crate::commons::bootstrap::Device::open(&lab)
        .expect("device")
        .expect("an identity");
    assert_eq!(device.lab(), Some(crate::LabMark::TestBinary));
    let joining = Joining {
        records_bound: crate::crossing::RecordsBound::uncrossed(),
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
    // One proof from the lab identity to m22's desk, m22 on the live genesis (`live`) or not, its
    // desk asking the check before possession first (`checks_first`) or not.
    let prove = |live: bool, checks_first: bool| {
        std::thread::scope(|scope| {
            let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
                let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
                for end in [&ours, &theirs] {
                    end.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
                }
                let desk = &desk;
                scope.spawn(move || {
                    let decide = |proving: &desk::Proving<'_>| {
                        with_live_genesis(live, || prove_at_ms(m22, proving, CURRENT, later))
                    };
                    let refuses = |proving: &desk::Proving<'_>| {
                        checks_first
                            .then(|| {
                                with_live_genesis(live, || {
                                    refuses_before_possession_at_ms(m22, proving, CURRENT, later)
                                })
                            })
                            .flatten()
                    };
                    let _ = desk::serve_with(
                        desk,
                        &mut ours,
                        std::net::IpAddr::from([192, 0, 2, 22]),
                        &|_| Err(crate::Error::new("no phrase runs here")),
                        Some(desk::Proof {
                            decide: &decide,
                            refuses: &refuses,
                            authority: None,
                            fleet: None,
                        }),
                    );
                });
                Ok(Box::new(theirs))
            };
            join::prove_first(&lab, &dial, &joining, &sign_node, &["m22:7".to_owned()])
        })
    };
    let records = || load_records(m22).unwrap().log().unwrap().records().count();
    let before = records();

    let missed = prove(true, true).expect_err("refused before possession");
    assert!(!missed.signed_maybe, "{missed:?}");
    assert_eq!(missed.refused[0].1, desk::LAB_REFUSED, "{missed:?}");
    let missed = prove(true, false).expect_err("refused at the decision");
    assert_eq!(missed.refused[0].1, desk::LAB_REFUSED, "{missed:?}");
    assert_eq!(records(), before, "m22 signed nothing for a lab identity");
    assert_ne!(load_state(&lab).unwrap().membership, Stored::In);

    let joined = prove(false, true).expect("let in on another genesis");
    assert_eq!(joined.name, "m7");
    assert_eq!(load_state(&lab).unwrap().membership, Stored::In);
    macs.server.stop();
    drop(macs.root);
}

/// The last `Remove` in `paths`'s log naming `seat`, and every epoch after it in the log's causal
/// order that wraps `recipient`: a moved or removed seat's binding must be in none of them.
fn epochs_after_remove_wrapping(
    paths: &Paths,
    seat: &MemberId,
    recipient: &age::Recipient,
) -> (RecordId, Vec<RecordId>) {
    let store = load_records(paths).unwrap();
    let log = store.log().unwrap();
    let identity = read_identity(paths).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let order: Vec<&Record> = log.records().collect();
    let at = order
        .iter()
        .rposition(|r| matches!(r.body(), Body::Remove { member, .. } if member == seat))
        .expect("a Remove of the seat");
    let wrapping = order[at + 1..]
        .iter()
        .filter(|r| r.kind() == Kind::Epoch)
        .filter(|r| {
            opened
                .wrapped_recipients
                .get(r.id())
                .is_some_and(|set| set.contains(recipient))
        })
        .map(|r| *r.id())
        .collect();
    (*order[at].id(), wrapping)
}

/// Whether `paths`'s newest `[everyone]` epochs each wrap `recipient`.
fn newest_everyone_wraps(paths: &Paths, recipient: &age::Recipient) -> bool {
    let store = load_records(paths).unwrap();
    let log = store.log().unwrap();
    let identity = read_identity(paths).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let newest = log.newest_epochs(&EpochAudience::Everyone);
    !newest.is_empty()
        && newest.iter().all(|epoch| {
            opened
                .wrapped_recipients
                .get(epoch)
                .is_some_and(|set| set.contains(recipient))
        })
}

/// m7 is rebuilt with its fleet node key kept (a fresh vault folder) a minute after its old seat
/// signed its `Node` record: no proof moves the seat within [`DARK_FOR_MS`], and today's person's
/// Allow refused it ("already bound to a different seat"). A person's Allow on m22 now signs the
/// batch a proof's `Replace` signs ([`sign_seat_move`]): the `Remove` of the old seat cut at its
/// last record, the `Admit` under `m7` with the node key, and the `[everyone]` and Commons
/// rotation. The Mac keeps its label and reads `[everyone]`, while the old binding is in no epoch
/// newer than the `Remove`. With a 0.147 member in the fleet book, the Allow refuses first.
///
/// NEGATIVE CONTROL: `sign_seat_move` without its `rotate_after_removal` (an early `return
/// Ok(signed)` before it): the batch mints no epoch and the `Kind::Epoch` assert fails.
#[test]
fn a_persons_allow_moves_a_kept_key_seat_that_signed_a_minute_ago() {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_, old, _) = member_key(m7);
    let old_recipient = read_identity(m7).unwrap().unwrap().recipient();
    let pair = node_pair(17);
    let n7 = node(17);
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
    let new_recipient = request.binding().recipient();
    let minute_later = aterm_time::unix_millis().unwrap() + 60_000;

    // No proof moves it: the old seat signed within the day.
    let proven = Proven {
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
        request: &request,
        node: n7,
        name: "m7",
        fleet: false,
        lan: false,
        fresh: true,
        book: CURRENT,
    };
    let by_proof = admit_by_proof(m22, &proven, minute_later).expect("proof");
    assert!(
        matches!(&by_proof, Readmit::Refused(why) if why.contains("last day")),
        "{by_proof:?}"
    );

    let allow = |book: Book| {
        super::super::admit::log_path(
            m22,
            &request,
            n7,
            "m7",
            minute_later,
            (
                Some(crate::members::bundle::Reader::BoundedLog(
                    crate::crossing::RecordsBound::uncrossed(),
                )),
                false,
                (book, true),
            ),
            None,
        )
        .expect("the Allow runs")
    };
    // A member heard on 0.147 would read the moved seat as removed: the Allow waits.
    let held = allow(Book::default());
    assert!(
        matches!(&held, super::super::admit::ByLog::KeptOut(why) if why.contains("0.148")),
        "{held:?}"
    );

    let moved = allow(CURRENT);
    let super::super::admit::ByLog::Admitted(admission) = moved else {
        panic!("the person's Allow does not move m7's seat: {moved:?}");
    };
    assert_eq!(admission.label, "m7", "the label carries over");
    let kinds: Vec<Kind> = admission.signed.iter().map(Record::kind).collect();
    assert_eq!(&kinds[..2], &[Kind::Remove, Kind::Admit], "{kinds:?}");
    assert!(
        kinds.contains(&Kind::Epoch),
        "the batch rotates [everyone]: {kinds:?}"
    );
    let store = load_records(m22).unwrap();
    let membership = store.log().unwrap().membership();
    let me = request.binding().actor();
    assert_eq!(membership.label(&me), Some("m7"));
    assert_eq!(membership.node(&n7), NodeStanding::Member(me));
    assert!(!membership.contains(&old), "the old seat is out");

    let (_, wrapping) = epochs_after_remove_wrapping(m22, &old, &old_recipient);
    assert!(
        wrapping.is_empty(),
        "the old binding reads an epoch newer than its Remove: {wrapping:?}"
    );
    assert!(newest_everyone_wraps(m22, &new_recipient));
    assert!(!newest_everyone_wraps(m22, &old_recipient));
    assert!(
        store.log().unwrap().rekey_dues().is_empty(),
        "nothing is left due"
    );
    // The rebuilt Mac reads [everyone] from the log alone.
    let identity = read_identity(&fresh).unwrap().unwrap();
    let entries = store
        .log()
        .unwrap()
        .open_entries(&identity)
        .unwrap()
        .unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.name == "shared" && e.value.as_slice() == b"everyone-v1"),
        "the moved Mac reads [everyone]"
    );
    // The Mac left out by the move is offered no Undo of it.
    sync_records(m7, None).expect("m7 pulls the move");
    assert!(undo_offers(m7).expect("undo offers").is_empty());

    // A second Allow signs nothing new.
    let again = allow(CURRENT);
    assert!(
        matches!(&again, super::super::admit::ByLog::Admitted(a) if a.signed.is_empty()),
        "{again:?}"
    );
    macs.server.stop();
    drop(macs.root);
}

/// The negative control A15 names, run on every test pass: [`sign_seat_move`] without its
/// `Remove` of the old seat. The evaluator, over what was signed, does not seat the Mac under the
/// old label with its node key (the old seat holds both still), so the person's Allow keeps
/// nothing, and m22's records are what they were.
#[test]
fn a_seat_move_without_its_remove_is_refused_by_the_evaluator() {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let pair = node_pair(18);
    let n7 = node(18);
    publish_local_node_key(m7, n7, |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .unwrap()
    .unwrap();
    post_records_to_servers(m7).unwrap();
    sync_records(m22, None).unwrap();
    let (_fresh, request, _) = newcomer(macs.root.path(), m22, "m7-fresh");
    let before = load_records(m22).unwrap().inventory();
    let allow = || {
        super::super::admit::log_path(
            m22,
            &request,
            n7,
            "m7",
            aterm_time::unix_millis().unwrap() + 60_000,
            (
                Some(crate::members::bundle::Reader::BoundedLog(
                    crate::crossing::RecordsBound::uncrossed(),
                )),
                false,
                (CURRENT, true),
            ),
            None,
        )
        .expect("the Allow runs")
    };
    MOVE_WITHOUT_REMOVE.with(|on| on.set(true));
    let dropped = allow();
    MOVE_WITHOUT_REMOVE.with(|on| on.set(false));
    assert!(
        matches!(&dropped, super::super::admit::ByLog::KeptOut(why) if why.contains("would not seat")),
        "{dropped:?}"
    );
    assert_eq!(
        load_records(m22).unwrap().inventory(),
        before,
        "nothing was kept"
    );
    // CONTROL: the same Allow with the Remove moves the seat.
    assert!(matches!(allow(), super::super::admit::ByLog::Admitted(_)));
    macs.server.stop();
    drop(macs.root);
}

/// What a person's Allow of a node key a live seat holds still refuses, read from the log alone
/// ([`super::super::admit::allowed`]): the seat moves to the new key under its label where the
/// Mac proved the node key and the fleet book reads moved seats, and not where a member runs
/// 0.147 or the Mac only names the key; a key two seats hold is not moved; and a seat a quorum
/// removed stays out, however the person presses.
///
/// NEGATIVE CONTROL: `allowed_seat_move` returning `None` for `NodeStanding::Contested` (folded
/// into the `Unknown` arm): the contested key reads as no move, the Allow is refused only as a
/// key bound to another seat, and the `contested` assert fails.
#[test]
fn a_persons_allow_still_refuses_a_contested_key_a_quorums_removal_and_0147() {
    let allowed = |log: &Log, mac: Mac, tag: u8, restore: bool, book: Book| {
        super::super::admit::allowed(
            log,
            mac.request().binding(),
            node(tag),
            (restore, true),
            book,
        )
    };
    let log = alab();
    let moved = allowed(&log, Mac(12), 2, false, CURRENT).expect("the Allow moves SEAT");
    let (seat, _, label) = moved.expect("a move");
    assert_eq!((seat, label.as_str()), (SEAT.id(), "m2"));
    // A Mac that only names the seat's node key (an ask proves none) moves nothing.
    let named = super::super::admit::allowed(
        &log,
        Mac(12).request().binding(),
        node(2),
        (false, false),
        CURRENT,
    );
    assert!(
        named
            .as_ref()
            .is_err_and(|why| why.contains("did not prove")),
        "{named:?}"
    );
    let old = allowed(&log, Mac(12), 2, false, Book::default());
    assert!(
        old.as_ref().is_err_and(|why| why.contains("0.148")),
        "{old:?}"
    );

    let mut contested = alab();
    node_record(&mut contested, LISTED, 2, 4);
    assert_eq!(
        contested.membership().node(&node(2)),
        NodeStanding::Contested
    );
    let refused = allowed(&contested, Mac(12), 2, false, CURRENT);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("two seats hold")),
        "contested: {refused:?}"
    );

    let mut quorum = alab();
    let cutoff = *quorum
        .records()
        .filter(|r| r.actor() == &SEAT.id())
        .last()
        .unwrap()
        .id();
    for by in [ME, LISTED] {
        keep(
            &mut quorum,
            by,
            T0,
            Body::Remove {
                member: SEAT.id(),
                cutoff,
            },
        );
    }
    assert!(
        quorum
            .membership()
            .removal(&SEAT.id())
            .is_some_and(|r| r.quorum)
    );
    for restore in [false, true] {
        let refused = allowed(&quorum, Mac(12), 2, restore, CURRENT);
        assert!(
            refused.as_ref().is_err_and(|why| why.contains("quorum")),
            "restore={restore}: {refused:?}"
        );
    }
}

/// A8's churn finding, on two real vault folders: m22's removal of m7 reached the log as a bare
/// `Remove` (its remover killed before the rotation of its batch), and m7 then came back with a
/// fresh vault key and its node key kept by a person's "Allow again". The restoring `Admit`
/// supersedes the removed seat, and before this fix the evaluator then dropped its recipient from
/// the removed ones: no rotation was ever due, and every newest `[everyone]` epoch still wrapped
/// the removed binding. A `Remove` now keeps counting its recipient as removed until an epoch that
/// excludes it is minted: the rotation is due at once, the ranked minter's pass mints it, and the
/// old binding is in no epoch newer than the `Remove`.
///
/// NEGATIVE CONTROL: the superseded-seat loop at the end of `Log::evaluate`'s membership
/// (`membership_log/eval.rs`) cut: `rekey_dues` is empty after the restore and the `due` assert
/// fails.
#[test]
fn allow_again_of_a_kept_node_key_rotates_out_the_removed_binding() {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_, old, _) = member_key(m7);
    let old_recipient = read_identity(m7).unwrap().unwrap().recipient();
    let pair = node_pair(19);
    let n7 = node(19);
    publish_local_node_key(m7, n7, |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .unwrap()
    .unwrap();
    post_records_to_servers(m7).unwrap();
    sync_records(m22, None).unwrap();
    let now = aterm_time::unix_millis().unwrap();
    // The bare Remove a killed remover leaves: no rotation after it.
    log_entries::author_and_commit_records(m22, |store| {
        let mut log = store.log().unwrap().clone();
        let (me, device, incarnation) =
            log_entries::open_local_member_signer(m22, &log)?.expect("m22 signs");
        let cutoff = *log
            .records()
            .filter(|r| r.actor() == &old)
            .last()
            .unwrap()
            .id();
        let remove = device.with_seed(|seed| {
            log.draft(
                &me,
                now,
                Body::Remove {
                    member: old,
                    cutoff,
                },
            )
            .sign(seed, incarnation)
        })?;
        log.insert(remove.clone())?;
        Ok((vec![remove], ()))
    })
    .expect("m22 keeps a bare Remove");
    assert!(
        newest_everyone_wraps(m22, &old_recipient),
        "CONTROL: a partial removal"
    );

    let (_fresh, request, _) = newcomer(macs.root.path(), m22, "m7-fresh");
    let restored = super::super::admit::log_path(
        m22,
        &request,
        n7,
        "m7",
        now + 1,
        (
            Some(crate::members::bundle::Reader::BoundedLog(
                crate::crossing::RecordsBound::uncrossed(),
            )),
            true,
            (CURRENT, true),
        ),
        None,
    )
    .expect("Allow again runs");
    assert!(
        matches!(&restored, super::super::admit::ByLog::Admitted(a) if a.label == "m7"),
        "{restored:?}"
    );
    let store = load_records(m22).unwrap();
    let log = store.log().unwrap();
    let me = request.binding().actor();
    assert_eq!(log.membership().node(&n7), NodeStanding::Member(me));
    assert!(
        log.removed_recipients().contains(&old_recipient),
        "the removed binding still counts as removed"
    );
    let due = log.rekey_dues();
    assert!(
        due.iter()
            .any(|d| d.audience == EpochAudience::Everyone && d.rotation),
        "due: {:?}",
        due.iter().map(|d| &d.audience).collect::<Vec<_>>()
    );
    // The ranked minter's passes, an injected clock past the settle grace.
    for pass in 0..3u64 {
        tend_epochs_at(m22, now + 2 + pass * 10 * 60 * 1000).expect("a vault pass");
    }
    let (_, wrapping) = epochs_after_remove_wrapping(m22, &old, &old_recipient);
    assert!(wrapping.is_empty(), "{wrapping:?}");
    assert!(!newest_everyone_wraps(m22, &old_recipient));
    assert!(newest_everyone_wraps(m22, &request.binding().recipient()));
    macs.server.stop();
    drop(macs.root);
}

/// The swarm roster a member derives from the log after a seat move: the old seat's row (its
/// recipient now counts as removed) moves to the deny lists, but its node key and label are the
/// moved seat's now, so the Mac that took the seat back is denied nowhere. Found by review of
/// A15: the old row's node key went to `deny_nodes`, and every reader of the overlaid roster held
/// the returned Mac out.
///
/// NEGATIVE CONTROL: `overlay_log_membership`'s removed-row loop without its `seated_nodes`
/// check: the moved seat's node key is denied and the first assert fails.
#[test]
fn a_moved_seat_is_denied_by_no_roster_row_of_its_old_seat() {
    let mut log = alab();
    let decision = decided(&log, Mac(12), 2, false, T0);
    apply(&mut log, Mac(12), 2, &decision, T0);
    assert!(
        log.recipient_is_removed(&SEAT.identity().recipient()),
        "CONTROL: the old seat's recipient counts as removed"
    );
    let mut roster = crate::swarm_roster::SwarmRoster {
        members: vec![crate::swarm_roster::SwarmMember {
            recipient: SEAT.identity().recipient().to_bech32(),
            vault_name: "m2".to_owned(),
            label: "m2".to_owned(),
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            admitted_ms: 0,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key: Some(aterm_codec::hex::encode_lower(&node(2))),
        }],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    super::super::roster::overlay_log_membership(&log, None, &mut roster);
    assert!(
        !roster.is_node_denied(&aterm_codec::hex::encode_lower(&node(2))),
        "the moved seat's node key is denied: {roster:?}"
    );
    let moved = log.membership().get(&Mac(12).id()).cloned().unwrap();
    assert!(
        !log_entries::member_denied_by_roster(&roster, &moved),
        "{roster:?}"
    );
    // A live seat the signed roster itself denies (by an alias of its row), which the log never
    // removed, stays denied: only a log-removed row's node key is lifted for the seat that holds
    // it now.
    let admitted = log.membership().get(&ADMITTED.id()).cloned().unwrap();
    let mut denying = crate::swarm_roster::SwarmRoster {
        members: vec![crate::swarm_roster::SwarmMember {
            recipient: ADMITTED.identity().recipient().to_bech32(),
            vault_name: "m3".to_owned(),
            label: "m3".to_owned(),
            aliases: vec!["m30".to_owned()],
            hint: None,
            hw_hint: None,
            admitted_ms: 0,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key: Some(aterm_codec::hex::encode_lower(&node(3))),
        }],
        deny_labels: vec!["m30".to_owned()],
        deny_nodes: Vec::new(),
    };
    super::super::roster::overlay_log_membership(&log, None, &mut denying);
    assert!(
        denying.is_node_denied(&aterm_codec::hex::encode_lower(&node(3))),
        "{denying:?}"
    );
    assert!(log_entries::member_denied_by_roster(&denying, &admitted));
}

/// A proof with nobody's Allow readmits a Mac to the vault it is configured for and never moves it
/// to another: a Mac vault A let go proves its node key to a member of vault B, whose log binds
/// that key to a seat (the same Mac's, from when it was in B), and B's member lets it in by the
/// proof. The bundle is refused under the state lock before anything is written, so the Mac stays
/// configured for A, with A's version, digests and cached envelope. Only a person's phrase, or
/// `aterm secrets join`, moves a Mac to another vault.
///
/// NEGATIVE CONTROL: write the proof's bundle as a phrase's is written (`prove_first` calling
/// `client::write_from_log_bundle` for `client::readmit_from_log_bundle`): the Mac is let into
/// B ("a proof never moves the Mac").
#[test]
fn a_proof_never_moves_a_mac_to_another_vault() {
    use crate::members::desk::{self, Desk, Local};
    use crate::members::join::{self, Stream};
    use crate::members::joiner::Joining;
    let _answering = ServersAnswering::set();
    let a = two_macs(&[(Audience::Everyone, "shared", b"a".as_slice())]);
    let b = two_macs(&[(Audience::Everyone, "shared", b"b".as_slice())]);
    let mac = &a.m7;
    let admin = admin_open(&a.m22).unwrap();
    admin_revoke(&a.m22, &admin, "m7", false).unwrap();
    let _ = sync(mac, None);
    let state = load_state(mac).unwrap();
    assert_eq!(
        state.membership,
        Stored::Revoked,
        "PRECONDITION: A let m7 go"
    );
    let floor = (state.seq, state.digest, state.records_digest);
    let cache = std::fs::read(mac.cache_file()).unwrap();
    let configured_before = config(mac).unwrap();
    let pair = node_pair(8);
    let n7 = node(8);
    publish_local_node_key(&b.m7, n7, |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .expect("B's m7 publishes the node key")
    .expect("a Node record");
    post_records_to_servers(&b.m7).expect("B's m7 posts its records");
    sync_records(&b.m22, None).expect("B's m22 pulls the Node record");
    let desk = std::sync::Mutex::new(Desk::new(
        Local::of(&b.m22)
            .expect("B's m22's desk")
            .expect("B's m22 is a member"),
    ));
    let device = crate::commons::bootstrap::Device::open(mac)
        .expect("device")
        .expect("an identity");
    let joining = Joining {
        records_bound: crate::crossing::RecordsBound::uncrossed(),
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
    // A day past B's m7's last record (its `Node`), so its seat is dark.
    let later = aterm_time::unix_millis().unwrap() + DARK_FOR_MS + 1;
    let decided = std::sync::Mutex::new(Vec::new());
    let decide = |proving: &desk::Proving<'_>| {
        let decision = prove_at_ms(&b.m22, proving, CURRENT, later);
        decided.lock().unwrap().push(decision.is_ok());
        decision
    };
    let tried = std::thread::scope(|scope| {
        let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
            let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
            }
            let (desk, decide) = (&desk, &decide);
            scope.spawn(move || {
                drop(desk::serve_with(
                    desk,
                    &mut ours,
                    std::net::IpAddr::from([192, 0, 2, 22]),
                    &|_| Err(crate::Error::new("no phrase runs here")),
                    Some(desk::Proof {
                        decide,
                        refuses: &|_| None,
                        authority: None,
                        fleet: None,
                    }),
                ));
            });
            Ok(Box::new(theirs))
        };
        join::prove_first(mac, &dial, &joining, &sign_node, &["m22:7".to_owned()])
    });
    assert_eq!(
        *decided.lock().unwrap(),
        [true],
        "PRECONDITION: B's member lets the Mac in by its proof"
    );
    let missed = match tried {
        Ok(joined) => panic!(
            "a proof never moves the Mac: it joined B as {}",
            joined.name
        ),
        Err(missed) => missed,
    };
    assert!(
        missed.refused[0].1.contains("another vault"),
        "the bundle is refused as another vault's: {missed:?}"
    );
    let after = load_state(mac).unwrap();
    assert_eq!(after.membership, Stored::Revoked, "the Mac stays let go");
    assert_eq!(
        (after.seq, after.digest, after.records_digest),
        floor,
        "with A's floor"
    );
    assert_eq!(std::fs::read(mac.cache_file()).ok(), Some(cache));
    assert!(
        config(mac).unwrap() == configured_before,
        "and configured for A"
    );
    a.server.stop();
    b.server.stop();
}

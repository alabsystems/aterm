// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::log_entries_tests::{TwoMacs, newcomer, two_macs};
use super::super::readmit::{Decision, decide};
use super::*;
use crate::age;
use crate::commons::binding::Request as DeviceRequest;
use crate::membership_log::{
    Body, Entrant, Evidence, Founder, Log, NodeStanding, Record, Root, node_proof_message,
};
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

/// The member that decides and admits (`m22`).
const ME: Mac = Mac(22);
/// The founder `m110`, whose own `Node` record binds `node(110)`.
const M110: Mac = Mac(110);
/// The founder `m14`, which never published a node key (ALab's Genesis v2 lists none).
const M14: Mac = Mac(14);

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
    fn request(self) -> DeviceRequest {
        DeviceRequest::create(
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

fn founder(mac: Mac, label: &str) -> Founder {
    let binding = mac.request().binding().clone();
    Founder {
        recipient: binding.recipient(),
        binding: Some(binding),
        node_key: None,
        name: label.to_owned(),
        label: label.to_owned(),
    }
}

fn keep(log: &mut Log, by: Mac, at: u64, body: Body) {
    let record = log
        .draft(&by.id(), at, body)
        .sign(&by.seed(), by.incarnation())
        .unwrap();
    log.insert(record).unwrap();
}

/// `by` binds `node(tag)` to its own seat, the key's proof beside it.
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

/// `ME` lets `mac` in under `label`, asking as `name`, with `node_key` (the admitting member's
/// word, which proves nothing).
fn admit(log: &mut Log, mac: Mac, name: &str, label: &str, node_key: [u8; 32], at: u64) {
    admit_by(log, ME, mac, name, label, node_key, at);
}

/// [`admit`], signed by `by`.
fn admit_by(
    log: &mut Log,
    by: Mac,
    mac: Mac,
    name: &str,
    label: &str,
    node_key: [u8; 32],
    at: u64,
) {
    keep(
        log,
        by,
        at,
        Body::Admit {
            entrant: Entrant {
                request: mac.request(),
                node_key,
                name: name.to_owned(),
                label: label.to_owned(),
            },
            evidence: Evidence::Manual,
        },
    );
}

/// ALab in small: `ME` (`m22`), `M110` (`m110`, its own `Node` record binds `node(110)`) and `M14`
/// (`m14`, no node key), founders all.
fn alab() -> Log {
    let root = Root::of(ME.request().binding());
    let founders = vec![
        founder(ME, "m22"),
        founder(M110, "m110"),
        founder(M14, "m14"),
    ];
    let mut log = Log::new(Record::genesis(&[99; 32], root, founders, 1).unwrap()).unwrap();
    node_record(&mut log, ME, 22, 2);
    node_record(&mut log, M110, 110, 3);
    log
}

/// What `ME` decides of `mac` proving `node(tag)` from another host on its home ALab LAN while
/// asking as `name`.
fn on_lan(log: &Log, mac: Mac, tag: u8, name: &str, at: u64) -> Decision {
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
            name,
            fleet: false,
            lan: true,
            fresh: true,
            book: CURRENT,
        },
        0,
        at,
    )
}

/// The founder m110 comes back with a new vault key on the home ALab LAN and keeps its seat and
/// label: the name `m110` names its seat, a proof can move it (its `Node` record binds its node
/// key), and the proof does, under `m110`. A stranger asking as `m110` with a node key of its own
/// takes a new seat under the next free label, never `m110`'s, and every member reads the pair
/// from the log, naming both labels. `m14`, which holds no node key, is named too; a name that is
/// no seat's `m<N>` names nothing.
///
/// NEGATIVE CONTROL: `splits`' name arm cut (`if true || asked == member.label`): the stranger's
/// pair is not recorded and the `splits` assert fails (`left: []`).
#[test]
fn a_returning_founder_keeps_m110_and_a_stranger_named_m110_does_not_take_it() {
    let mut log = alab();
    assert_eq!(seat_named(&log, "m110").as_deref(), Some("m110"));
    assert_eq!(
        seat_named(&log, "M110"),
        seat_named(&log, "m110"),
        "a name's case changes nothing"
    );
    assert_eq!(
        seat_named(&log, "m14").as_deref(),
        Some("m14"),
        "a seat with no node key is named too"
    );
    assert_eq!(seat_named(&log, "m999"), None, "no seat holds m999");
    assert_eq!(seat_named(&log, "andrews"), None, "no m<N> in the name");

    // The stranger first: its own node key, its name m110.
    let stranger = Mac(66);
    let decision = on_lan(&log, stranger, 66, "m110", T0);
    assert_eq!(decision, Decision::Lan, "the name moves no seat");
    admit(&mut log, stranger, "m110", "m160", node(66), T0);

    // m110's Mac, a day after its seat's last record, proves the seat's node key.
    let returned = Mac(111);
    let later = T0 + DAY;
    let Decision::Replace {
        seat,
        label,
        cutoff,
    } = on_lan(&log, returned, 110, "m110", later)
    else {
        panic!("m110's new key does not take its seat back");
    };
    assert_eq!((seat, label.as_str()), (M110.id(), "m110"));
    keep(
        &mut log,
        ME,
        later,
        Body::Remove {
            member: seat,
            cutoff,
        },
    );
    admit(&mut log, returned, "m110", &label, node(110), later);
    let membership = log.membership();
    assert_eq!(
        membership.label(&returned.id()),
        Some("m110"),
        "the label carries over"
    );
    assert_eq!(membership.label(&stranger.id()), Some("m160"));
    assert!(!membership.contains(&M110.id()), "the old seat is out");
    assert_eq!(
        seat_named(&log, "m110").as_deref(),
        Some("m110"),
        "the returned key's seat is m110's now"
    );
    assert_eq!(
        splits(&log),
        [Split {
            label: "m160".to_owned(),
            of: "m110".to_owned(),
            by: SplitBy::Name,
        }]
    );
}

/// Seats split already. m110's Mac came back as m160 ahead of its proof, then bound its node key
/// to the new seat too: the log binds one node key to both by authority, so it is one Mac (or a
/// copy of its disk), and `splits` says so by the node key. A plain `Remove` of the old seat is no
/// fold: signed after the new seat's `Admit`, it supersedes nothing, so the node key reads as
/// removed and m160 goes out with m110. That is why no member signs one, and the record is a
/// person's. m14's Mac, back as m161 under `m14-2` with no node key the log ties to m14, is a pair
/// by its name alone.
///
/// NEGATIVE CONTROL: `splits`' node arm cut (`by_node.filter(|_| false)`): m160's pair is told
/// by its name instead and the first `splits` assert fails (`by: Name` against `by: Node`).
#[test]
fn a_split_the_node_key_proves_is_recorded_for_a_person_since_a_remove_cannot_fold_it() {
    let mut log = alab();
    let m160 = Mac(160);
    let m161 = Mac(161);
    admit(&mut log, m160, "m110", "m160", [0; 32], T0);
    node_record(&mut log, m160, 110, T0 + 1);
    admit(&mut log, m161, "m14-2", "m161", node(61), T0 + 2);
    assert_eq!(log.membership().node(&node(110)), NodeStanding::Contested);
    assert_eq!(
        splits(&log),
        [
            Split {
                label: "m160".to_owned(),
                of: "m110".to_owned(),
                by: SplitBy::Node,
            },
            Split {
                label: "m161".to_owned(),
                of: "m14".to_owned(),
                by: SplitBy::Name,
            },
        ]
    );
    let request = Mac(162).request();
    assert!(
        matches!(
            decide(
                &log,
                &ME.id(),
                &Proven {
                    reader: crate::members::bundle::Reader::BoundedLog(crate::crossing::RecordsBound::uncrossed()),
                    request: &request,
                    node: node(110),
                    name: "m110",
                    fleet: false,
                    lan: true,
                    fresh: true,
                    book: CURRENT,
                },
                0,
                T0 + 2 * DAY,
            ),
            Decision::Refuse(why) if why.starts_with("two seats hold this node key")
        ),
        "a node key two seats hold moves neither"
    );

    // What a fold by a plain Remove of the stale seat would do.
    let cutoff = log
        .records()
        .filter(|record| *record.actor() == M110.id())
        .last()
        .map(|record| *record.id())
        .unwrap();
    keep(
        &mut log,
        ME,
        T0 + DAY,
        Body::Remove {
            member: M110.id(),
            cutoff,
        },
    );
    let membership = log.membership();
    let standing = |mac: Mac| {
        membership
            .get(&mac.id())
            .is_some_and(|member| stands(&log, &mac.id(), member))
    };
    assert!(!standing(M110), "the stale seat is out");
    assert!(
        !standing(m160),
        "and the seat its Mac runs as goes out with it: no member folds a split by itself"
    );
}

/// ALab in small with a fourth founder, `m112`, that the genesis names `andrews-3`.
fn alab_with_a_named_founder() -> (Log, Mac) {
    let andrews = Mac(112);
    let root = Root::of(ME.request().binding());
    let founders = vec![
        founder(ME, "m22"),
        founder(M110, "m110"),
        founder(M14, "m14"),
        Founder {
            name: "andrews-3".to_owned(),
            ..founder(andrews, "m112")
        },
    ];
    let log = Log::new(Record::genesis(&[99; 32], root, founders, 1).unwrap()).unwrap();
    (log, andrews)
}

/// A seat its own Mac admitted on a writer-succession statement (an `Admit` that Mac's seat
/// signed, by a person's word as the log reads it, with no fleet node key, under the name the log
/// gave that seat) is told as its successor: the log ties the two, where a name ties nothing. So
/// m22's successor read `by=name ... nothing proves one Mac holds both` on 2026-11-12, and a
/// successor whose seat goes by no `m<N>` name (`andrews-3` under `m112`) was not listed at all.
///
/// NEGATIVE CONTROL: `splits`' successor arm cut: m170's pair reads `by: Former` and m171's is
/// missing, so the `assert_eq!` fails on the first pair.
#[test]
fn a_seat_its_own_mac_admitted_is_a_split_by_successor() {
    let (mut log, andrews) = alab_with_a_named_founder();
    admit_by(&mut log, M14, Mac(170), "m14", "m170", [0; 32], T0);
    admit_by(
        &mut log,
        andrews,
        Mac(171),
        "andrews-3",
        "m171",
        [0; 32],
        T0 + 1,
    );
    assert_eq!(
        splits(&log),
        [
            Split {
                label: "m170".to_owned(),
                of: "m14".to_owned(),
                by: SplitBy::Successor,
            },
            Split {
                label: "m171".to_owned(),
                of: "m112".to_owned(),
                by: SplitBy::Successor,
            },
        ]
    );
    assert_eq!(
        splits(&log)[1].line(),
        "split m171 of=m112 by=successor: m171 came in as a Commons writer's successor in m112's \
         line (signed in under that seat's own name, with no fleet node key), so the log reads \
         them as one home's"
    );
}

/// The same two seats let in by another member are no successors: a seat that asked as `m14` is a
/// pair by its name alone, as before, one that asked as `andrews-3` is tied to nothing, and a
/// seat its own Mac admitted with a fleet node key (a Mac of the fleet under the same name) is
/// no successor either.
///
/// NEGATIVE CONTROL: `splits` reading every same-name `Admit` as a succession (`Lineages::of`
/// not requiring `by` to hold the name, or the arm not asking `of_member`): m172 reads
/// `by: Successor`.
#[test]
fn a_same_name_seat_another_member_admitted_is_no_split_by_successor() {
    let (mut log, andrews) = alab_with_a_named_founder();
    admit(&mut log, Mac(172), "m14", "m172", [0; 32], T0);
    admit(&mut log, Mac(173), "andrews-3", "m173", [0; 32], T0 + 1);
    admit_by(
        &mut log,
        andrews,
        Mac(174),
        "andrews-3",
        "m174",
        node(74),
        T0 + 2,
    );
    assert_eq!(
        splits(&log),
        [Split {
            label: "m172".to_owned(),
            of: "m14".to_owned(),
            by: SplitBy::Name,
        }]
    );
}

/// `by` takes `seat` out, its records counting through its last one.
fn remove_by(log: &mut Log, by: Mac, seat: Mac, at: u64) {
    let cutoff = log
        .records()
        .filter(|record| *record.actor() == seat.id())
        .last()
        .map(|record| *record.id())
        .unwrap();
    keep(
        log,
        by,
        at,
        Body::Remove {
            member: seat.id(),
            cutoff,
        },
    );
}

/// A successor whose admitting seat no longer stands is still told by its line, not by its name:
/// paired with the nearest seat above it that stands, through every one that is out (m14 admits
/// m170, m170 admits m175, m170 is removed: m175 is m14's), and where none stands but its name's
/// label is held again (m110 itself out, its Mac back with a new key that takes the seat's label
/// by its node key), as a former successor of a seat that holds that label no more
/// (`by=former`), never `by=name ... nothing proves`.
///
/// NEGATIVE CONTROL: the arm reading only the seats that admitted the successor themselves (no
/// walk up the line): m175 reads `by: Name`, and the first `assert_eq!` fails. The `Former` arm
/// printing `Name`: the second fails.
#[test]
fn a_successor_whose_admitter_is_out_is_told_by_its_line() {
    let mut log = alab();
    admit_by(&mut log, M14, Mac(170), "m14", "m170", [0; 32], T0);
    admit_by(&mut log, Mac(170), Mac(175), "m14", "m175", [0; 32], T0 + 1);
    remove_by(&mut log, ME, Mac(170), T0 + DAY);
    assert_eq!(
        splits(&log),
        [Split {
            label: "m175".to_owned(),
            of: "m14".to_owned(),
            by: SplitBy::Successor,
        }]
    );

    let mut log = alab();
    admit_by(&mut log, M110, Mac(176), "m110", "m176", [0; 32], T0);
    remove_by(&mut log, ME, M110, T0 + DAY);
    admit(&mut log, Mac(177), "m110", "m110", node(110), T0 + DAY);
    assert_eq!(log.membership().label(&Mac(177).id()), Some("m110"));
    let pairs = splits(&log);
    assert_eq!(
        pairs,
        [Split {
            label: "m176".to_owned(),
            of: "m110".to_owned(),
            by: SplitBy::Former,
        }]
    );
    assert!(
        pairs[0].line().contains("by=former") && !pairs[0].line().contains("nothing proves"),
        "{}",
        pairs[0].line()
    );
}

/// What `aterm members list` prints of each member is read from the log: who let it in, the
/// stamp of its entry, how (a person's Allow, the member by itself, the genesis) and whether a
/// fleet node key is bound to its seat, the key an `Admit` names counting whoever let it in.
///
/// NEGATIVE CONTROL: `listed` reading `node` as `false` for every seat: the founder m110's row
/// fails; reading `how` from anything but the entry's `Admit`: m181's fails.
#[test]
fn a_members_listed_facts_are_the_logs() {
    let mut log = alab();
    admit(&mut log, Mac(180), "m180", "m180", node(80), T0);
    keep(
        &mut log,
        ME,
        T0 + 1,
        Body::Admit {
            entrant: Entrant {
                request: Mac(181).request(),
                node_key: node(81),
                name: "m181".to_owned(),
                label: "m181".to_owned(),
            },
            evidence: Evidence::Phrase,
        },
    );
    admit_by(&mut log, M14, Mac(170), "m14", "m170", [0; 32], T0 + 2);
    let membership = log.membership();
    let row = |mac: Mac| {
        let listed =
            super::super::remove::listed(&log, &membership, membership.get(&mac.id()).unwrap());
        (
            listed.label,
            listed.admitted_by,
            listed.admitted_ms,
            listed.how,
            listed.node,
        )
    };
    assert_eq!(
        row(M110),
        ("m110".to_owned(), None, 1, None, true),
        "a founder whose own Node record binds a key"
    );
    assert_eq!(row(M14), ("m14".to_owned(), None, 1, None, false));
    assert_eq!(
        row(Mac(180)),
        (
            "m180".to_owned(),
            Some("m22".to_owned()),
            T0,
            Some(Evidence::Manual),
            true
        )
    );
    assert_eq!(
        row(Mac(181)),
        (
            "m181".to_owned(),
            Some("m22".to_owned()),
            T0 + 1,
            Some(Evidence::Phrase),
            true
        ),
        "a person's Allow binds the key the ask named"
    );
    assert_eq!(
        row(Mac(170)),
        (
            "m170".to_owned(),
            Some("m14".to_owned()),
            T0 + 2,
            Some(Evidence::Manual),
            false
        ),
        "a writer successor holds no node key"
    );
}

/// A Mac that asks as `m7` and proves `node_key` from another host on the member's home ALab LAN.
fn asked_on_lan(request: &DeviceRequest, node_key: [u8; 32]) -> Proven<'_> {
    Proven {
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
        request,
        node: node_key,
        name: "m7",
        fleet: false,
        lan: true,
        fresh: true,
        book: CURRENT,
    }
}

/// A vault with m22 (the admin) and m7, whose own `Node` record binds `node(7)`, on real vault
/// folders (synthetic keys, a loopback server), and a clock a day past that record, by which a
/// proof of `node(7)` would move seat m7.
fn m7_published() -> (TwoMacs, u64) {
    // The server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let pair = node_pair(7);
    publish_local_node_key(&macs.m7, node(7), |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .expect("m7 publishes its node key")
    .expect("a Node record");
    post_records_to_servers(&macs.m7).expect("m7 posts its records");
    sync_records(&macs.m22, None).expect("m22 pulls m7's Node record");
    let dark = aterm_time::unix_millis().unwrap() + super::super::readmit::DARK_FOR_MS + 1;
    (macs, dark)
}

/// The request the Mac at `paths` queues under `host`, with the join password `password` of the
/// vault `vault` proved.
fn ask_with_password(paths: &Paths, host: &str, vault: &str, password: &[u8]) -> Request {
    let recipient = machine_identity(paths).unwrap().recipient().to_bech32();
    let proof = join::Key::derive(vault, password).prove(vault, &recipient, host);
    Request {
        code: join_code(&machine_identity(paths).unwrap().recipient()),
        recipient,
        host: host.to_owned(),
        at: Some(1),
        allowed: None,
        proof: Some(proof),
    }
}

/// On real vault folders, m7 comes back from a fresh vault folder under the name `m7` and proves
/// its seat's node key on the home ALab LAN: its seat moves to it under `m7`. A stranger asking
/// as `m7` with a node key of its own takes a new seat beside it, which the log records as a pair
/// (`seat_splits`). An admin's approver that lets a Mac in by the join password under a standing
/// seat's label (`m22`) seats it beside that seat, on the record, and one under a name that is no
/// seat's goes in as before.
///
/// NEGATIVE CONTROL: `auto_approve_listed` without its `seat_named` look (`named` always `None`):
/// the approver's admission beside m22 is not on the record and the `beside` assert fails
/// (`left: []`).
#[test]
fn a_proof_keeps_a_returning_macs_seat_and_a_new_seat_beside_one_is_on_the_record() {
    let (macs, dark) = m7_published();
    let m22 = &macs.m22;
    let (_, fresh_request, _) = newcomer(macs.root.path(), m22, "m7-fresh");
    let (_, stranger_request, _) = newcomer(macs.root.path(), m22, "stranger");
    let back = admit_by_proof(m22, &asked_on_lan(&fresh_request, node(7)), dark).unwrap();
    let Readmit::Let { how, .. } = back else {
        panic!("m7's new key is not let in: {back:?}");
    };
    assert_eq!(
        how,
        ReadmitHow::Seat {
            label: "m7".to_owned()
        }
    );
    let beside = admit_by_proof(m22, &asked_on_lan(&stranger_request, node(9)), dark).unwrap();
    let Readmit::Let {
        how: ReadmitHow::Lan { label },
        ..
    } = beside
    else {
        panic!("the stranger is not let in beside m7: {beside:?}");
    };
    assert_ne!(label, "m7");
    assert_eq!(
        seat_splits(m22).unwrap(),
        [Split {
            label: label.clone(),
            of: "m7".to_owned(),
            by: SplitBy::Name,
        }]
    );

    // An admin's approver, the vault on a join password: a name that is no seat's goes in as
    // before; one under a standing seat's label goes in beside it.
    const PASSWORD: &[u8] = b"fixture-password-for-seat-continuity";
    let mut admin = admin_open(m22).unwrap();
    admin.published_doc.set_open(false).unwrap();
    admin
        .published_doc
        .set_join_password(Some(PASSWORD))
        .unwrap();
    let vault = admin.config.name.clone();
    let (other, _, _) = newcomer(macs.root.path(), m22, "other");
    let (namesake, _, _) = newcomer(macs.root.path(), m22, "namesake");
    let done = auto_approve_listed(
        m22,
        &admin,
        &[
            ask_with_password(&other, "m9", &vault, PASSWORD),
            ask_with_password(&namesake, "m22", &vault, PASSWORD),
        ],
        &mut false,
        &mut BTreeSet::new(),
        (&mut JoinKeys::default(), None),
    )
    .unwrap();
    assert_eq!(done.approved, ["m9", "m22-2"]);
    assert_eq!(done.beside, [("m22-2".to_owned(), "m22".to_owned())]);
    assert_eq!(
        seat_lines(&done).len(),
        1,
        "one line for the one new seat beside another"
    );
    macs.server.stop();
    drop(macs.root);
}

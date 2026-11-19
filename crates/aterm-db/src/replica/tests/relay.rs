// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    control::{
        Phase,
        tail::{self, Fault},
    },
    runtime::CredentialOwner,
};
use aterm_secrets::commons::{
    binding::Binding,
    hearing::{
        Floor, Hold, LIFTED, Ledger, Mark, RELAYS, REPORT2_LEN, Reports, Said, Version, in_place,
        relayed,
    },
};

/// The floor a member that reports [`RELAYS`] clears: these tests judge the relay, not where a
/// deployed fence such as `hearing::CHECKPOINTS` stands.
const RELAYED: Floor = in_place(Some(RELAYS));
use std::sync::{Arc, Mutex};

/// The first flight with `entries` offered entries, as its frame goes on the wire.
const fn offer(entries: usize) -> usize {
    4 + 4 + 2 + 24 * entries
}
/// The second flight answering `offered` entries and offering `entries`.
const fn answer(offered: usize, entries: usize) -> usize {
    4 + 4 + offered.div_ceil(8) + 2 + 24 * entries
}
/// The third flight answering `offered` entries and giving `reports`.
const fn give(offered: usize, reports: usize) -> usize {
    4 + 4 + offered.div_ceil(8) + 2 + REPORT2_LEN * reports
}
/// The fourth flight giving `reports`.
const fn last(reports: usize) -> usize {
    4 + 4 + 2 + REPORT2_LEN * reports
}

/// A lab of writers, each Mac with its Commons tables, its credentials custody and a store of
/// kept version reports whose roster is the whole lab.
struct Relays {
    lab: Lab,
    members: Vec<Binding>,
    stores: Vec<Arc<Mutex<Reports>>>,
    owners: Vec<CredentialOwner>,
    now_ms: u64,
}

impl Relays {
    fn new(macs: usize) -> Self {
        let lab = Lab::new(&vec![Role::Writer; macs]);
        let now_ms = aterm_time::unix_millis().unwrap();
        let members: Vec<_> = (0..macs)
            .map(|n| lab.bootstrap(n).request().unwrap().binding().clone())
            .collect();
        let stores = members
            .iter()
            .map(|local| {
                let mut reports = Reports::new(lab.bootstrap(0).trust(), local);
                reports.set_roster(&members, now_ms);
                Arc::new(Mutex::new(reports))
            })
            .collect();
        let owners = lab.paths.iter().map(CredentialOwner::for_test).collect();
        Self {
            lab,
            members,
            stores,
            owners,
            now_ms,
        }
    }

    /// Mac `n`'s endpoint as the node service builds one, without a store of kept reports.
    fn storeless(&self, n: usize) -> Endpoint {
        let config = self.lab.paths[n].home.join("config.toml");
        Endpoint::from_bootstrap(&self.lab.bootstrap(n))
            .unwrap()
            .unwrap()
            .with_commons(self.lab.paths[n].clone(), config.clone())
            .unwrap()
            .with_credential_owner(self.owners[n].clone(), config)
    }

    /// Mac `n`'s endpoint with its store, trading with a peer that reports [`RELAYS`] or later:
    /// the product's floor, which no test binary's report reaches.
    fn released(&self, n: usize) -> Endpoint {
        self.storeless(n).with_reports(self.stores[n].clone())
    }

    /// Mac `n`'s endpoint with its store, trading with a peer that reports what a test binary
    /// does.
    fn endpoint(&self, n: usize) -> Endpoint {
        self.released(n).relaying_from(LIFTED)
    }

    /// Mac `n` signs `RELAYS` with the time into its own store: the report's bytes.
    fn sign(&self, n: usize) -> [u8; REPORT2_LEN] {
        let signed = self.lab.bootstrap(n).device().with_seed(|seed| {
            self.stores[n]
                .lock()
                .unwrap()
                .sign_own(seed, RELAYS, self.now_ms)
        });
        assert!(signed.unwrap());
        *self.kept(n).last().unwrap()
    }

    /// Mac `to`'s store takes `wire`, as from a round with a peer no test here runs.
    fn hand(&self, to: usize, wire: &[u8; REPORT2_LEN]) {
        let mut store = self.stores[to].lock().unwrap();
        let member = store.screen(wire, self.now_ms).unwrap();
        let report = relayed(wire, &member, self.now_ms).unwrap();
        assert!(store.accept(report, self.now_ms));
    }

    /// The reports Mac `n`'s store keeps, in the order it took them.
    fn kept(&self, n: usize) -> Vec<[u8; REPORT2_LEN]> {
        let (kept, _) = self.stores[n].lock().unwrap().since(0);
        kept.iter().map(|report| *report.wire()).collect()
    }
}

/// One side of a round: its repair state after it, and the bytes it wrote and read after its
/// credential handoff.
struct Sided {
    repair: PeerRepair,
    moved: [usize; 2],
}

impl Sided {
    /// Whether the round completed on this side: its application exchange, the peer's version
    /// report and the credential handoff, with no error of any.
    fn complete(&self) -> bool {
        self.repair.application().is_some()
            && self.repair.application_error().is_none()
            && self.repair.membership_error().is_none()
            && self.repair.phase() == Phase::Complete
            && self.repair.said() == Said::Version(Version::own())
            && self.repair.handoff() == Some(&Ok(0))
    }
}

/// One repair round over loopback TLS, `dialler` to `listener`, each side's session after its
/// handoff under its own `faults` entry. Both exchanges returned without an error.
fn round(dialler: &Endpoint, listener: &Endpoint, faults: [Fault; 2]) -> [Sided; 2] {
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    thread::scope(|threads| {
        let served = threads.spawn(|| {
            let mut repair = PeerRepair::default();
            let (result, moved) = tail::observed(faults[1], || {
                listener.exchange(
                    Connection::admit(
                        aterm_uds::accept(
                            &socket,
                            aterm_uds::AcceptWait::Forever,
                            aterm_uds::AcceptedMode::Blocking,
                        )
                        .unwrap()
                        .0,
                        LinkScope::Loopback,
                    )
                    .unwrap(),
                    Side::Responder,
                    &mut repair,
                )
            });
            result.unwrap();
            Sided { repair, moved }
        });
        let mut repair = PeerRepair::default();
        let (result, moved) = tail::observed(faults[0], || {
            dialler.exchange(
                fixture_connect(address, std::time::Instant::now()).unwrap(),
                Side::Initiator,
                &mut repair,
            )
        });
        result.unwrap();
        [Sided { repair, moved }, served.join().unwrap()]
    })
}

/// [`round`] with no fault on either side.
fn clean(dialler: &Endpoint, listener: &Endpoint) -> [Sided; 2] {
    round(dialler, listener, [Fault::None; 2])
}

/// LT-04D's exit test. Mac A signs its version with the time. A round between A and B leaves
/// the report with B; a round between B and C, which never has a round with A, leaves it with
/// C, byte for byte, and C's ledger then counts A for the checkpoint gate, which held A as
/// unheard before. That is no contact with A: C's watch does not start. Each round stays
/// complete, and each side's bytes after the handoff are the flights it sent and read. Once
/// the report was offered back to the Mac that gave it, the pair trades two empty flights.
///
/// NEGATIVE CONTROL: with `frontier::connected` not calling `Endpoint::trade_relayed`, nothing
/// follows the first round's handoff: `` left: [0, 0] right: [180, 11] ``.
#[test]
fn a_report_crosses_two_rounds_to_a_member_that_never_met_its_signer() {
    let lab = Relays::new(3);
    let (a, b, c) = (lab.endpoint(0), lab.endpoint(1), lab.endpoint(2));
    let wire = lab.sign(0);
    let signer = &lab.members[0];
    let mut ledger = Ledger::read(&lab.lab.bootstrap(2));
    assert_eq!(
        ledger.holding_below([signer], lab.now_ms, RELAYED),
        vec![(signer.actor(), Hold::Unheard)]
    );

    // A dials B: one entry offered, wanted, and given in the third flight.
    let [dialled, served] = clean(&a, &b);
    assert!(dialled.complete() && served.complete());
    assert_eq!(dialled.moved, [offer(1) + give(0, 1), answer(1, 0)]);
    assert_eq!(served.moved, [answer(1, 0), offer(1) + give(0, 1)]);
    assert_eq!(lab.kept(1), [wire]);
    assert_ne!(dialled.repair.relay(), Mark::default());
    assert!(lab.kept(2).is_empty());

    // C dials B: C offers nothing, B offers A's report, C wants it, B gives it last.
    let [dialled, served] = clean(&c, &b);
    assert!(dialled.complete() && served.complete());
    assert_eq!(
        dialled.moved,
        [offer(0) + give(1, 0), answer(0, 1) + last(1)]
    );
    assert_eq!(
        served.moved,
        [answer(0, 1) + last(1), offer(0) + give(1, 0)]
    );
    assert_eq!(lab.kept(2), [wire]);

    let (kept, _) = lab.stores[2].lock().unwrap().since(0);
    assert!(
        kept.iter()
            .fold(false, |changed, report| ledger.relay(report, lab.now_ms)
                | changed)
    );
    assert!(
        ledger
            .holding_below([signer], lab.now_ms, RELAYED)
            .is_empty()
    );
    assert!(!ledger.watch(lab.now_ms));

    // C names the report to B once, B does not want it, and then neither has anything to say.
    let [dialled, _] = clean(&c, &b);
    assert_eq!(dialled.moved, [offer(1), answer(1, 0)]);
    let [dialled, served] = clean(&b, &c);
    assert!(dialled.complete() && served.complete());
    assert_eq!(
        (dialled.moved, served.moved),
        ([offer(0); 2], [offer(0); 2])
    );
    assert_eq!((lab.kept(1), lab.kept(2)), (vec![wire], vec![wire]));
}

/// A peer whose verified version report names a release before the floor is sent the bytes it
/// was sent before reports were traded: nothing follows the credential handoff in either
/// direction, on `atdb/8` and on `atdb/7` in either seat, though both sides keep a report to
/// offer and the round completes with both versions read and the handoff made. So for the
/// product's floor ([`RELAYS`]), which a test binary's report (0.146.0) is below, and for a
/// floor one patch above that report. At a floor the peer's report reaches, the same Macs
/// trade their reports after the handoff on both wires, and on `atdb/3`, whose rounds are refused
/// table repair before the handoff, they still trade nothing.
///
/// NEGATIVE CONTROL: with `Endpoint::trade_relayed` not comparing the peer's version with the
/// floor, the first round's dialler sends its offer and the two trade:
/// `` left: [[181, 181], [181, 181]] right: [[0, 0], [0, 0]] ``.
#[test]
fn a_peer_that_reports_a_release_before_relaying_is_sent_nothing_after_the_handoff() {
    let lab = Relays::new(2);
    let wires = [lab.sign(0), lab.sign(1)];
    let atdb7 = |mut endpoint: Endpoint| {
        endpoint.reconcile_tables_only = true;
        endpoint
    };
    let silent = |dialler: &Endpoint, listener: &Endpoint| {
        let [dialled, served] = clean(dialler, listener);
        assert!(dialled.complete() && served.complete());
        assert_eq!([dialled.moved, served.moved], [[0, 0], [0, 0]]);
        for side in [&dialled, &served] {
            assert_eq!(side.repair.relay(), Mark::default());
        }
        assert_eq!((lab.kept(0), lab.kept(1)), (vec![wires[0]], vec![wires[1]]));
    };
    assert!(Version::own() < RELAYS);
    silent(&lab.released(0), &lab.released(1));
    silent(&lab.released(0), &atdb7(lab.released(1)));
    silent(&atdb7(lab.released(1)), &lab.released(0));
    let above = Version(LIFTED.0, LIFTED.1, LIFTED.2 + 1);
    let held = |n| lab.released(n).relaying_from(above);
    silent(&held(0), &held(1));
    silent(&held(0), &atdb7(held(1)));

    // On `atdb/3`, before the frontier protocols, a round is refused table repair whatever the
    // peer reports: at a floor its report reaches, nothing is kept and no mark moves.
    let atdb3 = |mut endpoint: Endpoint| {
        endpoint.legacy_credentials = true;
        endpoint
    };
    for (dialler, listener) in [
        (atdb3(lab.endpoint(0)), lab.endpoint(1)),
        (lab.endpoint(1), atdb3(lab.endpoint(0))),
    ] {
        for side in clean(&dialler, &listener) {
            let refused = side
                .repair
                .application_error()
                .expect("table repair is refused");
            assert_eq!(refused.text, refused_below_atdb_7("atdb/3"));
            assert_eq!(side.repair.relay(), Mark::default());
        }
        assert_eq!((lab.kept(0), lab.kept(1)), (vec![wires[0]], vec![wires[1]]));
    }

    // At a floor the peer's report reaches, each offers its own report and takes the other's;
    // on `atdb/7` the two, as new endpoints, then name their reports again and want neither.
    let [dialled, served] = clean(&lab.endpoint(0), &lab.endpoint(1));
    assert!(dialled.complete() && served.complete());
    assert_eq!(
        dialled.moved,
        [offer(1) + give(1, 1), answer(1, 1) + last(1)]
    );
    assert_eq!(lab.kept(0), [wires[0], wires[1]]);
    assert_eq!(lab.kept(1), [wires[1], wires[0]]);
    let [dialled, served] = clean(&lab.endpoint(0), &atdb7(lab.endpoint(1)));
    assert!(dialled.complete() && served.complete());
    assert_eq!(dialled.moved, [offer(1) + give(1, 0), answer(1, 1)]);
}

/// A peer that ends its session after the credential handoff, as one that keeps no reports
/// does, costs the round nothing: both exchanges return, both sides hold a completed round
/// with the peer's version and the handoff, and the Mac that would have traded keeps its mark
/// for that peer where it was. As the dialler it wrote its offer and read nothing; as the
/// listener it read nothing and wrote nothing. Once the peer answers, first an offer of
/// nothing, its offer is made again in the round after, and taken.
///
/// NEGATIVE CONTROL: with `Endpoint::trade_relayed` recording the trade's error as the round's
/// application error (`repair.application_failed(error)`), the dialler's round no longer reads
/// as complete: `assertion failed: dialled.complete() && served.complete()`.
#[test]
fn a_peer_that_ends_its_session_after_the_handoff_still_completes_the_round() {
    let lab = Relays::new(2);
    let wire = lab.sign(0);
    let (trading, ending) = (lab.endpoint(0), lab.storeless(1));

    let [dialled, served] = clean(&trading, &ending);
    assert!(dialled.complete() && served.complete());
    assert_eq!([dialled.moved, served.moved], [[offer(1), 0], [0, 0]]);
    assert_eq!(dialled.repair.relay(), Mark::default());

    let [dialled, served] = clean(&ending, &trading);
    assert!(dialled.complete() && served.complete());
    assert_eq!([dialled.moved, served.moved], [[0, 0], [0, 0]]);
    assert_eq!(served.repair.relay(), Mark::default());
    assert!(lab.kept(1).is_empty());

    let answering = lab.endpoint(1);
    let [dialled, served] = clean(&trading, &answering);
    assert!(dialled.complete() && served.complete());
    assert_eq!(dialled.moved, [offer(0), answer(0, 0)]);
    assert_eq!(dialled.repair.relay(), Mark::default());
    assert!(lab.kept(1).is_empty());
    let [dialled, served] = clean(&trading, &answering);
    assert!(dialled.complete() && served.complete());
    assert_eq!(dialled.moved, [offer(1) + give(0, 1), answer(1, 0)]);
    assert_ne!(dialled.repair.relay(), Mark::default());
    assert_eq!(lab.kept(1), [wire]);
}

/// A peer that leaves an offer unanswered is not sent the kept reports' names again in every
/// round. The round that found it so wrote the whole offer and read nothing; each round dialled
/// to it after that writes an offer of nothing, 10 bytes, and the mark for it stays where it
/// was. When the peer answers that offer, the same round takes what the peer offers, and the
/// round after names this Mac's reports again, which the peer takes. A whole offer of the
/// peer's own, in a round the peer dials, ends it as well: this Mac's next round to it names
/// what it took since.
///
/// NEGATIVE CONTROL: with `Kept::offer_first` offering from the store whatever the last round
/// left (`Relay::unanswered` not read), the second round to the silent peer names the report
/// again: `` left: [[34, 0], [0, 0]] right: [[10, 0], [0, 0]] ``. And with `Kept::answer_first`
/// leaving `Relay::unanswered` set once the peer's offer arrived, the last round offers nothing:
/// `` left: [21, 34] right: [181, 35] ``.
#[test]
fn a_peer_that_left_an_offer_unanswered_is_offered_nothing_until_it_answers() {
    let lab = Relays::new(4);
    let wires = [lab.sign(0), lab.sign(1), lab.sign(2), lab.sign(3)];
    let trading = lab.endpoint(0);

    let (silent, answering) = (lab.storeless(1), lab.endpoint(1));
    let [dialled, _] = clean(&trading, &silent);
    assert_eq!(dialled.moved, [offer(1), 0]);
    for _ in 0..2 {
        let [dialled, served] = clean(&trading, &silent);
        assert!(dialled.complete() && served.complete());
        assert_eq!([dialled.moved, served.moved], [[offer(0), 0], [0, 0]]);
        assert_eq!(dialled.repair.relay(), Mark::default());
    }
    // The peer answers the offer of nothing and offers its own report, which is taken.
    let [dialled, served] = clean(&trading, &answering);
    assert!(dialled.complete() && served.complete());
    assert_eq!(
        dialled.moved,
        [offer(0) + give(1, 0), answer(0, 1) + last(1)]
    );
    assert_eq!(dialled.repair.relay(), Mark::default());
    assert_eq!(lab.kept(0), [wires[0], wires[1]]);
    assert_eq!(lab.kept(1), [wires[1]]);
    // The round after names this Mac's report again.
    let [dialled, _] = clean(&trading, &answering);
    assert_eq!(dialled.moved, [offer(1) + give(0, 1), answer(1, 0)]);
    assert_ne!(dialled.repair.relay(), Mark::default());
    assert_eq!(lab.kept(1), [wires[1], wires[0]]);

    // Mac 2 leaves an offer unanswered, then dials with an offer of its own: both trade all
    // they keep in that round.
    let (silent, answering) = (lab.storeless(2), lab.endpoint(2));
    let [dialled, _] = clean(&trading, &silent);
    assert_eq!(dialled.moved, [offer(2), 0]);
    let [dialled, served] = clean(&answering, &trading);
    assert!(dialled.complete() && served.complete());
    assert_eq!(
        served.moved,
        [answer(1, 2) + last(2), offer(1) + give(2, 1)]
    );
    assert_eq!(lab.kept(2), [wires[2], wires[0], wires[1]]);
    // So what Mac 0 takes afterwards is named to Mac 2 in its next round, not after a round
    // that offers nothing. Mac 2 names back, once, the report of Mac 1 that this round gave it.
    lab.hand(0, &wires[3]);
    let [dialled, _] = clean(&trading, &answering);
    assert_eq!(dialled.moved, [offer(1) + give(1, 1), answer(1, 1)]);
    assert_eq!(lab.kept(2), [wires[2], wires[0], wires[1], wires[3]]);
}

/// A Mac whose store has no roster yet, as between the start of its serving loop and its first
/// hearing tick, trades nothing: as the listener it leaves a peer's offer unanswered, and as the
/// dialler it sends nothing. It does not answer that it wants none of the reports, so the
/// peer's mark for it stays where it was, and once the store has its roster the peer names the
/// report again and the Mac takes it.
///
/// NEGATIVE CONTROL: with `Endpoint::trade_relayed` not asking whether the store has a roster,
/// the Mac answers the offer, wanting nothing, and the peer's mark moves past the report:
/// `` left: [[34, 11], [11, 34]] right: [[34, 0], [0, 0]] ``.
#[test]
fn a_mac_whose_store_has_no_roster_yet_leaves_an_offer_unanswered_and_is_offered_it_again() {
    let lab = Relays::new(2);
    let wire = lab.sign(0);
    *lab.stores[1].lock().unwrap() = Reports::new(lab.lab.bootstrap(0).trust(), &lab.members[1]);
    let (keeping, starting) = (lab.endpoint(0), lab.endpoint(1));

    let [dialled, served] = clean(&keeping, &starting);
    assert!(dialled.complete() && served.complete());
    assert_eq!([dialled.moved, served.moved], [[offer(1), 0], [0, 0]]);
    assert_eq!(dialled.repair.relay(), Mark::default());
    let [dialled, served] = clean(&starting, &keeping);
    assert!(dialled.complete() && served.complete());
    assert_eq!([dialled.moved, served.moved], [[0, 0], [0, 0]]);
    assert_eq!(served.repair.relay(), Mark::default());

    // The first hearing tick gives the store its roster.
    lab.stores[1]
        .lock()
        .unwrap()
        .set_roster(&lab.members, lab.now_ms);
    let [dialled, _] = clean(&keeping, &starting);
    assert_eq!(dialled.moved, [offer(0), answer(0, 0)]);
    assert_eq!(dialled.repair.relay(), Mark::default());
    let [dialled, _] = clean(&keeping, &starting);
    assert_eq!(dialled.moved, [offer(1) + give(0, 1), answer(1, 0)]);
    assert_eq!(lab.kept(1), [wire]);
}

/// A trade that a peer cuts short or sends malformed ends there and nowhere else. Each Mac
/// keeps its own report and would take the other's. Whichever flight of the peer's is cut or
/// has its magic changed (its offer, its answer, or the flight that gives what was wanted),
/// both exchanges return with the round complete, this Mac keeps nothing of the peer's, and
/// its mark for the peer stays where it was. So clean rounds afterwards, between the same two
/// endpoints, trade both reports: the last case left this Mac's offer unanswered, so its next
/// round offers nothing and takes the peer's report, and the round after offers the same report
/// again, which the peer takes.
///
/// NEGATIVE CONTROL: with the dialler's mark moved once its offer was sent, before the answer
/// to it was read, the first case fails: `` assertion `left == right` failed: Responder
/// Garble(4) `` with `left: Mark { epoch: 1, stamp: 1 }`, `right: Mark { epoch: 0, stamp: 0 }`.
#[test]
fn a_relay_cut_short_or_malformed_leaves_the_round_complete_and_the_mark_unmoved() {
    let lab = Relays::new(2);
    let wires = [lab.sign(0), lab.sign(1)];
    // Mac 1 is the faulty peer in the seat named, Mac 0 the one judged.
    let third = offer(1) + 4;
    let cases = [
        // Mac 1 answers Mac 0's offer with another magic, or not at all.
        (Side::Responder, Fault::Garble(4)),
        (Side::Responder, Fault::Cut(answer(1, 1) - 1)),
        // Mac 1's own offer has another magic, or stops inside its first entry.
        (Side::Initiator, Fault::Garble(4)),
        (Side::Initiator, Fault::Cut(offer(1) - 3)),
        // Mac 1's third flight, which gives the report Mac 0 wanted, the same.
        (Side::Initiator, Fault::Garble(third)),
        (Side::Initiator, Fault::Cut(third + 60)),
        // Mac 1 stops before it answers anything: neither side's offer was answered.
        (Side::Responder, Fault::Cut(0)),
    ];
    let mut kept = None;
    for (seat, fault) in cases {
        let (judged, faulty) = (lab.endpoint(0), lab.endpoint(1));
        let [dialled, served] = match seat {
            Side::Responder => round(&judged, &faulty, [Fault::None, fault]),
            Side::Initiator => round(&faulty, &judged, [fault, Fault::None]),
        };
        assert!(
            dialled.complete() && served.complete(),
            "{seat:?} {fault:?}"
        );
        let own = match seat {
            Side::Responder => &dialled,
            Side::Initiator => &served,
        };
        assert_eq!(own.repair.relay(), Mark::default(), "{seat:?} {fault:?}");
        assert_eq!(lab.kept(0), [wires[0]], "{seat:?} {fault:?}");
        kept = Some((judged, faulty));
    }
    assert_eq!(lab.kept(1), [wires[1]]);
    let (judged, faulty) = kept.unwrap();
    let [dialled, served] = clean(&judged, &faulty);
    assert!(dialled.complete() && served.complete());
    assert_eq!(
        dialled.moved,
        [offer(0) + give(1, 0), answer(0, 1) + last(1)]
    );
    assert_eq!(lab.kept(0), [wires[0], wires[1]]);
    assert_eq!(lab.kept(1), [wires[1]]);
    let [dialled, served] = clean(&judged, &faulty);
    assert!(dialled.complete() && served.complete());
    assert_eq!(dialled.moved, [offer(1) + give(0, 1), answer(1, 0)]);
    assert_eq!(lab.kept(1), [wires[1], wires[0]]);
}
